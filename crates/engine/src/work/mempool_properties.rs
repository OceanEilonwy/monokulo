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
    if boundary == 1 {
        drop(first); // abandoned before any payment publication
    } else {
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
        if boundary == 0 { 1 } else { 2 }
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
        for boundary in 0..4 {
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
