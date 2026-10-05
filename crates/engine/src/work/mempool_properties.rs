//! Reservations use real scan and database boundaries; cancellation must leave
//! retryable work, while parallel live callers cannot duplicate custody scans.
use super::*;
use crate::property_support::{config, hold_worker, runtime, wait_queued, GateCustody, TempFile};
use crate::scanner::tests::{fixture_tenant, fixture_tx};
use crate::store::{Db, Store, TenantId};
use proptest::prelude::*;
use std::collections::BTreeMap;

proptest! {
    #![proptest_config(config())]
    #[test]
    fn batch_reservations_and_cache_histories(data in prop::collection::vec(any::<u8>(), 0..4097)) {
        crate::exploration::mempool(&data);
    }

    #[test]
    fn reservation_histories_match_independent_ownership(
        events in prop::collection::vec((0u8..5,0usize..8,0usize..4,0usize..4,0u32..4),1..256),
    ) {
        let state = MempoolState::default();
        let mut owners: Vec<Option<(ScanClaims<'_>,String,TenantId,u64)>> = std::iter::repeat_with(|| None).take(8).collect();
        let mut busy = BTreeMap::new();
        let mut done = BTreeMap::new();
        for (action,owner,tx,tenant,window) in events {
            let txid = format!("tx-{tx}");
            let id = TenantId::new(format!("tenant-{tenant}"));
            let key = (txid.clone(),id.clone());
            match action {
                0 => {
                    if let Some((guard,t,id,_)) = owners[owner].take() { drop(guard); busy.remove(&(t,id)); }
                    let window = ScanIndices::new([window]);
                    let generation = window.generation();
                    let tuple = (id.clone(),WalletHandle::generate(),window);
                    let expected = !busy.contains_key(&key) && done.get(&key)!=Some(&generation);
                    let (guard,claimed) = state.claim(&txid,&[&tuple]);
                    prop_assert_eq!(claimed.len(),usize::from(expected));
                    if expected { busy.insert(key,owner); owners[owner] = Some((guard,txid,id,generation)); }
                }
                1 | 2 => {
                    if let Some((mut guard,t,id,generation)) = owners[owner].take() {
                        if action==2 { guard.complete(&id,generation); done.insert((t.clone(),id.clone()),generation); }
                        drop(guard); busy.remove(&(t,id));
                    }
                }
                3 => { state.forget(); done.clear(); }
                _ => {
                    state.retain_pool(&HashSet::from([txid.clone()]));
                    done.retain(|(t,_),_| t==&txid);
                }
            }
            prop_assert_eq!(state.inner.lock().in_flight.len(),busy.len());
        }
        drop(owners);
        prop_assert!(state.inner.lock().in_flight.is_empty());
    }
}

async fn competing_scans(boundary: u8) {
    let path = TempFile::new();
    let store = Store::open_file(&path.0).unwrap();
    let custody = GateCustody::default();
    let (id, handle, order) = fixture_tenant(&store, &custody, i64::MAX).await;
    let db = Db::open(&path.0, &store).unwrap();
    let daemon = crate::daemon::fake::FakeDaemonClient::new();
    let tenants = [(id.clone(), handle)];
    let inputs = RoundInputs {
        db: &db,
        custody: &custody,
        daemon: &daemon,
        network: monero::Network::Mainnet,
        tenants: &tenants,
        reorg_check_depth: 20,
        grace_period_seconds: 0,
        scan_chunk_memory_budget_mb: 16,
    };
    let state = ScanState::default();
    let tx = fixture_tx();
    let txid = crate::daemon::fake::tx_id_hex(&tx);
    let window = (id.clone(), handle, ScanIndices::range(0..3));
    let due = [&window];
    custody.scan_mode.store(2, Ordering::SeqCst);
    let mut first = Box::pin(scan_and_record(&state, &inputs, &tx, &txid, &due, None));
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            () = custody.scan_entered.notified() => {}
            _ = &mut first => panic!("scan ended before its custody rendezvous"),
        }
    })
    .await
    .unwrap();
    // Both callers may have selected this tenant before either reserved it.
    // The losing caller returns immediately and does not contact custody.
    let duplicate = scan_and_record(&state, &inputs, &tx, &txid, &due, None).await;
    assert_eq!(duplicate.touched, 0);
    assert_eq!(custody.scans.load(Ordering::SeqCst), 1);
    assert!(
        state.mempool.inner.try_lock().is_some(),
        "scan held the cache mutex across custody await"
    );
    if boundary == 4 {
        // Advance the actual custody deadline while its call is held.
        tokio::time::pause();
        tokio::time::advance(
            state.tuning().reserved(super::super::Tier::Mempool) + Duration::from_millis(1),
        )
        .await;
        let outcome = first.await;
        tokio::time::resume();
        assert_eq!(outcome.failed, vec![id.clone()]);
        assert_eq!(outcome.touched, 0);
    } else if boundary == 1 {
        drop(first); // abandoned before any payment publication
    } else {
        if boundary == 5 {
            store.execute_raw_for_test("CREATE TRIGGER reservation_write_failure BEFORE INSERT ON order_payments BEGIN SELECT RAISE(ABORT, 'injected publication failure'); END;").unwrap();
        }
        if boundary == 6 {
            state.mempool.forget();
            state.mempool.retain_pool(&HashSet::new());
            assert!(
                !scan_and_record(&state, &inputs, &tx, &txid, &due, None)
                    .await
                    .attempted
            );
        }
        let mut blocker = if boundary == 3 {
            Some(hold_worker(&db).await)
        } else {
            None
        };
        custody
            .scan_mode
            .store(u8::from(boundary == 2), Ordering::SeqCst);
        custody.scan_release.notify_one();
        if boundary == 3 {
            tokio::time::timeout(Duration::from_secs(10), async {
                tokio::select! {
                    () = wait_queued(&db,Class::Scanner,1) => {}
                    _ = &mut first => panic!("scan ended before its accepted database job"),
                }
            })
            .await
            .unwrap();
            assert!(
                state.mempool.inner.try_lock().is_some(),
                "scan held the cache mutex across database await"
            );
            assert_eq!(
                scan_and_record(&state, &inputs, &tx, &txid, &due, None)
                    .await
                    .touched,
                0
            );
            drop(first); // accepted job can still commit: retry must be safe
            blocker.as_mut().unwrap().release();
            db.run(Class::Scanner, Store::count_tenants).await.unwrap();
        } else {
            let outcome = first.await;
            if boundary == 2 {
                assert_eq!(outcome.failed, vec![id.clone()]);
            } else if boundary == 5 {
                assert!(outcome.store_error.is_some());
                assert_eq!(outcome.touched, 0);
                assert!(store.get_all_payments(&order).unwrap().is_empty());
                store
                    .execute_raw_for_test("DROP TRIGGER reservation_write_failure;")
                    .unwrap();
            } else {
                assert!(outcome.touched > 0);
            }
        }
    }
    assert!(state.mempool.inner.lock().in_flight.is_empty());
    custody.scan_mode.store(0, Ordering::SeqCst);
    let retry = scan_and_record(&state, &inputs, &tx, &txid, &due, None).await;
    assert!(retry.failed.is_empty());
    assert!(retry.store_error.is_none());
    assert_eq!(
        custody.scans.load(Ordering::SeqCst),
        if matches!(boundary, 0 | 6) { 1 } else { 2 }
    );
    let payments = store.get_all_payments(&order).unwrap();
    assert!(!payments.is_empty());
    let identities: Vec<_> = payments.iter().map(|p| p.id).collect();
    assert_eq!(
        scan_and_record(&state, &inputs, &tx, &txid, &due, None)
            .await
            .touched,
        0
    );
    assert_eq!(
        store
            .get_all_payments(&order)
            .unwrap()
            .iter()
            .map(|p| p.id)
            .collect::<Vec<_>>(),
        identities
    );
}

#[test]
fn competing_scans_release_on_success_failure_and_cancellation() {
    runtime().block_on(async {
        for boundary in 0..7 {
            competing_scans(boundary).await;
        }
    });
}

#[test]
fn ownership_survives_pool_eviction_and_serializes_changed_windows() {
    let state = MempoolState::default();
    let old = (
        TenantId::new("tenant"),
        WalletHandle::generate(),
        ScanIndices::new([1]),
    );
    let new = (old.0.clone(), old.1, ScanIndices::new([1, 2]));
    let (mut owner, claimed) = state.claim("tx", &[&old]);
    assert_eq!(claimed.len(), 1);
    state.forget();
    state.retain_pool(&HashSet::new());
    assert!(state.claim("tx", &[&new]).1.is_empty());
    owner.complete(&old.0, old.2.generation());
    drop(owner);
    let (mut next, claimed) = state.claim("tx", &[&new]);
    assert_eq!(claimed.len(), 1);
    next.complete(&new.0, new.2.generation());
    assert!(state.claim("tx", &[&new]).1.is_empty());
}

#[test]
fn unrelated_transactions_and_tenants_are_not_serialized() {
    let state = MempoolState::default();
    let a = (
        TenantId::new("a"),
        WalletHandle::generate(),
        ScanIndices::new([1]),
    );
    let b = (
        TenantId::new("b"),
        WalletHandle::generate(),
        ScanIndices::new([1]),
    );
    let (one, _) = state.claim("tx-1", &[&a]);
    let (two, claimed) = state.claim("tx-1", &[&b]);
    assert_eq!(claimed.len(), 1);
    let (three, claimed) = state.claim("tx-2", &[&a]);
    assert_eq!(claimed.len(), 1);
    drop((one, two, three));
    assert!(state.inner.lock().in_flight.is_empty());
}

#[test]
fn partially_completed_batches_and_panics_release_only_unfinished_work() {
    let state = MempoolState::default();
    let a = (
        TenantId::new("a"),
        WalletHandle::generate(),
        ScanIndices::new([1]),
    );
    let b = (
        TenantId::new("b"),
        WalletHandle::generate(),
        ScanIndices::new([1]),
    );
    let (mut owner, claimed) = state.claim("tx", &[&a, &b]);
    assert_eq!(claimed.len(), 2);
    owner.complete(&a.0, a.2.generation());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _owner = owner;
        panic!("injected panic with a partially completed scan batch");
    }));
    assert!(result.is_err());
    assert!(state.inner.lock().in_flight.is_empty());
    let (_retry, claimed) = state.claim("tx", &[&a, &b]);
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].0, b.0);
}

#[test]
fn simultaneous_threads_cannot_both_reserve_the_same_scan() {
    for changed_window in [false, true] {
        let state = MempoolState::default();
        let a = (
            TenantId::new("tenant"),
            WalletHandle::generate(),
            ScanIndices::new([1]),
        );
        let b = (
            a.0.clone(),
            a.1,
            ScanIndices::new(if changed_window { vec![1, 2] } else { vec![1] }),
        );
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let compete = |tuple: &TenantWindow| {
                barrier.wait();
                let (guard, claimed) = state.claim("tx", &[tuple]);
                let count = claimed.len();
                barrier.wait(); // keep the winning claim until both tried
                drop(guard);
                count
            };
            let one = scope.spawn(move || compete(&a));
            let two = scope.spawn(move || compete(&b));
            assert_eq!(one.join().unwrap() + two.join().unwrap(), 1);
        });
        assert!(state.inner.lock().in_flight.is_empty());
    }
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn overlapping_thread_batches_have_exactly_one_owner_per_key(
        callers in prop::collection::vec((0u8..4, 1u8..=255, any::<u32>()), 2..9),
    ) {
        let state = MempoolState::default();
        let barrier = std::sync::Barrier::new(callers.len() + 1);
        let expected: std::collections::BTreeSet<_> = callers.iter().flat_map(|(tx,mask,_)| (0..8u8).filter(move |i| mask & (1 << i) != 0).map(move |i| (format!("tx-{tx}"), TenantId::new(format!("tenant-{i}"))))).collect();
        std::thread::scope(|scope| {
            let jobs: Vec<_> = callers.iter().map(|&(tx,mask,window)| {
                let state = &state;
                let barrier = &barrier;
                scope.spawn(move || {
                    let windows: Vec<_> = (0..8u8).filter(|i| mask & (1 << i) != 0).map(|i| (TenantId::new(format!("tenant-{i}")),WalletHandle::from_bytes([i;16]),ScanIndices::new([window]))).collect();
                    let txid = format!("tx-{tx}");
                    barrier.wait();
                    let (guard,claimed) = state.claim(&txid,&windows.iter().collect::<Vec<_>>());
                    let keys: Vec<_> = claimed.iter().map(|w| (txid.clone(),w.0.clone())).collect();
                    barrier.wait(); // every caller has acquired or lost
                    barrier.wait(); // main inspected while all guards are alive
                    drop(guard);
                    keys
                })
            }).collect();
            barrier.wait();
            barrier.wait();
            let actual: std::collections::BTreeSet<_> = state.inner.lock().in_flight.iter().cloned().collect();
            // Release threads before asserting so even a regression cannot
            // strand scoped threads behind a barrier during panic unwinding.
            barrier.wait();
            assert_eq!(actual,expected);
            let admitted: Vec<_> = jobs.into_iter().flat_map(|j| j.join().unwrap()).collect();
            assert_eq!(admitted.len(),expected.len());
            assert_eq!(admitted.into_iter().collect::<std::collections::BTreeSet<_>>(),expected);
        });
        prop_assert!(state.inner.lock().in_flight.is_empty());
    }

    #[test]
    fn cache_accounting_obeys_full_width_budgets(
        count in 0usize..17,
        budget in any::<usize>(),
        events in prop::collection::vec((any::<bool>(),0u8..16,any::<usize>()),1..128),
    ) {
        let tx = Arc::new(Transaction::default());
        let mut actual = Bodies::default();
        let mut expected = BTreeMap::<String,usize>::new();
        for (insert,id,size) in events {
            let key = format!("tx-{id}");
            if insert {
                let total: u128 = expected.values().map(|&n| n as u128).sum();
                if !expected.contains_key(&key) && expected.len()<count && total + size as u128 <= budget as u128 {
                    expected.insert(key.clone(),size);
                }
                actual.remember(&key,&tx,size,count,budget);
            } else {
                actual.retain(|txid| txid != &key);
                expected.remove(&key);
            }
            let entries: BTreeMap<_,_> = actual.by_txid.iter().map(|(id,(_,size))| (id.clone(),*size)).collect();
            prop_assert_eq!(&entries,&expected);
            prop_assert_eq!(actual.bytes as u128,expected.values().map(|&n| n as u128).sum::<u128>());
            prop_assert!(actual.bytes<=budget);
            prop_assert!(actual.by_txid.len()<=count);
        }
    }
}

#[test]
fn reviewed_fuzz_histories_remain_valid_regressions() {
    for data in [
        include_bytes!("../../../../fuzz/seeds/mempool/partial-cancel").as_slice(),
        include_bytes!("../../../../fuzz/seeds/mempool/evict-live-window").as_slice(),
        include_bytes!("../../../../fuzz/seeds/mempool/cache-boundaries").as_slice(),
        include_bytes!("../../../../fuzz/seeds/mempool/stale-owner").as_slice(),
    ] {
        crate::exploration::mempool(data);
    }
}

async fn entry_point_contention(fast_first: bool, cancel: bool, competitors: usize) {
    let path = TempFile::new();
    let store = Store::open_file(&path.0).unwrap();
    let custody = GateCustody::default();
    let (id, handle, order) = fixture_tenant(&store, &custody, i64::MAX).await;
    let db = Db::open(&path.0, &store).unwrap();
    let daemon = crate::daemon::fake::FakeDaemonClient::new();
    daemon.push_block("reservation-tip", vec![]);
    daemon.set_mempool(vec![fixture_tx()]);
    let tenants = [(id.clone(), handle)];
    let inputs = RoundInputs {
        db: &db,
        custody: &custody,
        daemon: &daemon,
        network: monero::Network::Mainnet,
        tenants: &tenants,
        reorg_check_depth: 20,
        grace_period_seconds: 0,
        scan_chunk_memory_budget_mb: 16,
    };
    let state = ScanState::default();
    // Bootstrap before holding custody so the rendezvous belongs to a mempool
    // scan, rather than chain initialization or any block scan.
    super::super::run_round(&state, &inputs, Duration::ZERO)
        .await
        .into_result()
        .unwrap();
    state.mempool.forget();
    custody.scans.store(0, Ordering::SeqCst);
    custody.scan_mode.store(2, Ordering::SeqCst);
    let run = async |fast| {
        if fast {
            fast_pass(&state, &inputs).await.unwrap();
        } else {
            super::super::run_round(&state, &inputs, Duration::ZERO)
                .await
                .into_result()
                .unwrap();
        }
    };
    let mut owner = Box::pin(run(fast_first));
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            () = custody.scan_entered.notified() => {}
            () = &mut owner => panic!("owner ended before custody rendezvous"),
        }
    })
    .await
    .unwrap();
    for _ in 0..competitors {
        tokio::time::timeout(Duration::from_secs(10), run(!fast_first))
            .await
            .unwrap();
        assert_eq!(custody.scans.load(Ordering::SeqCst), 1);
        assert_eq!(state.mempool.inner.lock().in_flight.len(), 1);
    }
    if cancel {
        drop(owner);
    } else {
        custody.scan_mode.store(0, Ordering::SeqCst);
        custody.scan_release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), owner)
            .await
            .unwrap();
    }
    assert!(state.mempool.inner.lock().in_flight.is_empty());
    custody.scan_mode.store(0, Ordering::SeqCst);
    run(!fast_first).await;
    assert_eq!(
        custody.scans.load(Ordering::SeqCst),
        1 + usize::from(cancel)
    );
    let identities: Vec<_> = store
        .get_all_payments(&order)
        .unwrap()
        .iter()
        .map(|p| p.id)
        .collect();
    assert!(!identities.is_empty());
    run(true).await;
    run(false).await;
    assert_eq!(
        custody.scans.load(Ordering::SeqCst),
        1 + usize::from(cancel)
    );
    assert_eq!(
        store
            .get_all_payments(&order)
            .unwrap()
            .iter()
            .map(|p| p.id)
            .collect::<Vec<_>>(),
        identities
    );
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn real_fast_and_round_callers_share_reservations(
        fast_first in any::<bool>(), cancel in any::<bool>(), competitors in 1usize..9,
    ) {
        runtime().block_on(entry_point_contention(fast_first, cancel, competitors));
    }
}

async fn mixed_tenant_batch(failure: u8, reverse: bool, repeats: usize) {
    let path = TempFile::new();
    let store = Store::open_file(&path.0).unwrap();
    let custody = GateCustody::default();
    let (a, ah, ao) = fixture_tenant(&store, &custody, i64::MAX).await;
    let (b, bh, bo) = fixture_tenant(&store, &custody, i64::MAX).await;
    let db = Db::open(&path.0, &store).unwrap();
    let daemon = crate::daemon::fake::FakeDaemonClient::new();
    let tenants = [(a.clone(), ah), (b.clone(), bh)];
    let inputs = RoundInputs {
        db: &db,
        custody: &custody,
        daemon: &daemon,
        network: monero::Network::Mainnet,
        tenants: &tenants,
        reorg_check_depth: 20,
        grace_period_seconds: 0,
        scan_chunk_memory_budget_mb: 16,
    };
    let trigger = match failure {
        1 => Some(format!("CREATE TRIGGER batch_fault BEFORE INSERT ON order_payments WHEN NEW.order_id = '{bo}' BEGIN SELECT RAISE(ABORT, 'payment publication'); END;")),
        3 => Some(format!("CREATE TRIGGER batch_fault BEFORE INSERT ON pending_payment_recomputes WHEN NEW.order_id = '{bo}' BEGIN SELECT RAISE(ABORT, 'durable recompute obligation'); END;")),
        4 => Some(format!("CREATE TRIGGER batch_fault BEFORE UPDATE ON orders WHEN NEW.id = '{bo}' BEGIN SELECT RAISE(ABORT, 'inline recompute'); END;")),
        _ => None,
    };
    if let Some(sql) = &trigger {
        store.execute_raw_for_test(sql).unwrap();
    }
    let state = ScanState::default();
    let tx = fixture_tx();
    let txid = crate::daemon::fake::tx_id_hex(&tx);
    let good = (a.clone(), ah, ScanIndices::range(0..3));
    let impaired = (
        b.clone(),
        if failure == 0 {
            WalletHandle::from_bytes([0; 16])
        } else {
            bh
        },
        if failure == 2 {
            ScanIndices::new([0])
        } else {
            ScanIndices::range(0..3)
        },
    );
    let batch = if reverse {
        [&impaired, &good]
    } else {
        [&good, &impaired]
    };
    let outcome = scan_and_record(&state, &inputs, &tx, &txid, &batch, Some(1)).await;
    assert_eq!(outcome.touched, 1);
    assert_eq!(outcome.failed.len(), usize::from(failure == 0));
    assert_eq!(outcome.store_error.is_some(), matches!(failure, 1 | 3 | 4));
    assert!(state.mempool.inner.lock().in_flight.is_empty());
    let first: Vec<_> = store
        .get_all_payments(&ao)
        .unwrap()
        .iter()
        .map(|p| p.id)
        .collect();
    assert!(!first.is_empty());
    assert!(store.get_all_payments(&bo).unwrap().is_empty());
    assert_eq!(
        store
            .get_order(&b, &bo)
            .unwrap()
            .unwrap()
            .amount_received_piconero,
        0
    );
    assert_eq!(custody.scans.load(Ordering::SeqCst), 2);
    if failure == 2 {
        assert!(
            !scan_and_record(&state, &inputs, &tx, &txid, &batch, Some(1))
                .await
                .attempted,
            "empty results should complete the original window"
        );
        assert_eq!(custody.scans.load(Ordering::SeqCst), 2);
    }
    if trigger.is_some() {
        store
            .execute_raw_for_test("DROP TRIGGER batch_fault;")
            .unwrap();
    }
    let repaired = (b.clone(), bh, ScanIndices::range(0..3));
    let retry = scan_and_record(&state, &inputs, &tx, &txid, &[&good, &repaired], Some(1)).await;
    assert_eq!(retry.touched, 1);
    assert!(retry.failed.is_empty());
    assert!(retry.store_error.is_none());
    assert_eq!(
        custody.scans.load(Ordering::SeqCst),
        3,
        "only the failed or expanded tenant should rescan"
    );
    let second: Vec<_> = store
        .get_all_payments(&bo)
        .unwrap()
        .iter()
        .map(|p| p.id)
        .collect();
    assert!(!second.is_empty());
    assert!(
        store
            .get_order(&b, &bo)
            .unwrap()
            .unwrap()
            .amount_received_piconero
            > 0
    );
    for _ in 0..repeats {
        assert!(
            !scan_and_record(&state, &inputs, &tx, &txid, &[&good, &repaired], Some(1))
                .await
                .attempted
        );
    }
    assert_eq!(custody.scans.load(Ordering::SeqCst), 3);
    assert_eq!(
        store
            .get_all_payments(&ao)
            .unwrap()
            .iter()
            .map(|p| p.id)
            .collect::<Vec<_>>(),
        first
    );
    assert_eq!(
        store
            .get_all_payments(&bo)
            .unwrap()
            .iter()
            .map(|p| p.id)
            .collect::<Vec<_>>(),
        second
    );
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn mixed_tenant_success_failure_and_empty_matches_retry_independently(
        failure in 0u8..5, reverse in any::<bool>(), repeats in 1usize..9,
    ) {
        runtime().block_on(mixed_tenant_batch(failure,reverse,repeats));
    }
}

#[test]
fn every_mixed_batch_failure_boundary_runs_in_both_orders() {
    runtime().block_on(async {
        for failure in 0..5 {
            for reverse in [false, true] {
                mixed_tenant_batch(failure, reverse, 2).await;
            }
        }
    });
}

#[test]
fn both_entry_points_cover_owner_success_and_cancellation() {
    runtime().block_on(async {
        for fast_first in [false, true] {
            for cancel in [false, true] {
                entry_point_contention(fast_first, cancel, 2).await;
            }
        }
    });
}
