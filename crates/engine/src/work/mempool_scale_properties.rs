//! Large pool and sustained-arrival tests exercise the actual scan/publication path.
use super::*;
use crate::store::NewOrder;
use crate::work::portfolio_fixture::{recorded_pair, transaction};

fn foreign(index: usize) -> Transaction {
    let mut tx = transaction(31, &[(&crate::work::portfolio_fixture::pair(83), 1, 17)]);
    let mut extra = tx.prefix.extra.try_parse().0;
    extra.push(monero::blockdata::transaction::SubField::Nonce(
        index.to_le_bytes().to_vec(),
    ));
    tx.prefix.extra = monero::blockdata::transaction::ExtraField(extra).into();
    tx
}

async fn sustained_arrivals(wave: usize, rounds: usize) {
    let path = TempFile::new();
    let store = Store::open_file(&path.0).unwrap();
    let custody = crate::work::history_fixture::FlakyKeyCustody::default();
    let (id, handle, first) = fixture_tenant(&store, &custody, i64::MAX).await;
    let db = Db::open(&path.0, &store).unwrap();
    let daemon = crate::daemon::fake::FakeDaemonClient::new();
    for h in 1..=2 {
        daemon.push_block(&format!("scale-{h}"), vec![]);
        store
            .set_scanned_block(monero::Network::Mainnet, h, &format!("scale-{h}"))
            .unwrap();
    }
    store
        .execute_raw_for_test("UPDATE tenants SET scanned_through_height=2")
        .unwrap();
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
    // It pays an address allocated AFTER the successful initial scan. Merely
    // deduplicating it forever would miss real money.
    let old = transaction(71, &[(&recorded_pair(), 2, 1234)]);
    let mut pool = vec![old];
    daemon.set_mempool(pool.clone());
    assert_eq!(fast_pass(&state, &inputs).await.unwrap().scanned, 1);
    crate::work::run_round_at(&state, &inputs, Duration::ZERO, 1000)
        .await
        .into_result()
        .unwrap();
    assert!(store.get_all_payments(&first).unwrap().is_empty());
    let minor = store.allocate_minor_index(&id).unwrap();
    assert_eq!(minor, 2);
    let second = store
        .create_order(&NewOrder {
            tenant_id: id.clone(),
            merchant_order_id: None,
            minor_index: minor,
            address: "scale-second".into(),
            xmr_amount_piconero: 1234,
            description: None,
            created_at: 1000,
            expires_at: i64::MAX,
            confirmations_required_override: Some(0),
            idempotency_key: None,
        })
        .unwrap();
    for round in 0..rounds {
        pool.extend((round * wave..(round + 1) * wave).map(foreign));
        daemon.set_mempool(pool.clone());
        fast_pass(&state, &inputs).await.unwrap();
        crate::work::run_round_at(&state, &inputs, Duration::ZERO, 1000 + round as i64)
            .await
            .into_result()
            .unwrap();
    }
    let payments = store.get_all_payments(&second.id).unwrap();
    assert_eq!(
        payments.len(),
        1,
        "assertion failed: BOUNDARY: sustained-arrival-starvation"
    );
    assert_eq!(payments[0].amount_piconero, 1234);
    assert_eq!(payments[0].output_index, 0);
    assert!(store.get_all_payments(&first).unwrap().is_empty());
    assert!(state.mempool.inner.lock().in_flight.is_empty());
}

#[test]
fn expanded_windows_are_served_during_continuous_new_transaction_floods() {
    runtime().block_on(sustained_arrivals(320, 8));
}

async fn large_pool(count: usize) {
    tokio::time::pause();
    let _clock = crate::property_support::hold_virtual_clock();
    let path = TempFile::new();
    let store = Store::open_file(&path.0).unwrap();
    let custody = crate::work::history_fixture::FlakyKeyCustody::default();
    let (id, handle, order) = fixture_tenant(&store, &custody, i64::MAX).await;
    let db = Db::open(&path.0, &store).unwrap();
    let daemon = crate::daemon::fake::FakeDaemonClient::new();
    daemon.push_block("scale-pool-tip", vec![]);
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
    let mut pool: Vec<_> = (0..count - 1).map(foreign).collect();
    let payer = fixture_tx();
    let payer_id = crate::daemon::fake::tx_id_hex(&payer);
    pool.push(payer);
    daemon.set_mempool(pool);
    custody.fail(handle);
    fast_pass(&state, &inputs).await.unwrap();
    let failed_attempts = custody.attempts.lock().get(&handle).copied().unwrap_or(0);
    assert!(failed_attempts > 0);
    assert!(store.get_all_payments(&order).unwrap().is_empty());
    custody.recover(handle);
    tokio::time::advance(Duration::from_secs(61)).await;
    for pass in 0..count.div_ceil(FAST_TXS_PER_PASS) + 2 {
        // Two live callers must still perform exactly one successful custody
        // scan per transaction/window, including nonpaying transactions.
        let (left, right) = tokio::join!(fast_pass(&state, &inputs), fast_pass(&state, &inputs));
        assert!(left.is_some() && right.is_some());
        if state.mempool.inner.lock().scanned.len() == count {
            break;
        }
        assert!(
            pass + 1 < count.div_ceil(FAST_TXS_PER_PASS) + 2,
            "large pool failed to drain"
        );
    }
    let rows = store.get_all_payments(&order).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].txid, payer_id);
    assert_eq!(rows[0].output_index, 1);
    assert_eq!(rows[0].amount_piconero, 7_000_000_000);
    assert_eq!(
        custody.attempts.lock()[&handle],
        failed_attempts + count as u32,
        "duplicate parallel scans or unserved transactions"
    );
    {
        let remembered = state.mempool.inner.lock();
        assert_eq!(remembered.scanned.len(), count);
        assert!(remembered.in_flight.is_empty());
        assert!(remembered.bodies.by_txid.len() <= MAX_BODIES);
        assert!(remembered.bodies.bytes <= MAX_BODY_BYTES);
        assert_eq!(
            remembered.bodies.bytes,
            remembered
                .bodies
                .by_txid
                .values()
                .map(|(_, size)| size)
                .sum::<usize>()
        );
    }
    crate::work::run_round_at(&state, &inputs, Duration::ZERO, 1000)
        .await
        .into_result()
        .unwrap();
    assert_eq!(state.mempool.rotation.lock().queue.len(), count);
    daemon.set_mempool(vec![]);
    fast_pass(&state, &inputs).await.unwrap();
    assert!(state.mempool.rotation.lock().queue.is_empty());
    assert!(state.mempool.rotation.lock().members.is_empty());
    assert!(state.mempool.tenant_page_after.lock().is_empty());
    let remembered = state.mempool.inner.lock();
    assert_eq!(remembered.bodies.bytes, 0);
    assert!(remembered.scanned.is_empty());
    assert_eq!(
        store.get_all_payments(&order).unwrap().len(),
        1,
        "pool departure erased payment evidence"
    );
    println!("ENGINE_SCALE_RESULT pool_transactions={count} successful_unique_scans={count}");
}
proptest! {
    #![proptest_config(config())]
    #[test]
    fn parallel_pool_backlogs_recover_and_release_cache(count in 2usize..129) {
        runtime().block_on(large_pool(count));
    }
}
#[test]
#[ignore = "large deterministic scale package; scripts/engine-scale.sh runs it"]
fn eight_thousand_pool_transactions_recover_and_release_cache() {
    for count in [257, 1025, 4097, 8193] {
        runtime().block_on(large_pool(count));
    }
}

#[test]
fn production_body_caps_reject_overflow_and_reuse_freed_space() {
    let tx = Arc::new(fixture_tx());
    let size = monero::consensus::encode::serialize(tx.as_ref()).len();
    let mut bodies = Bodies::default();
    for index in 0..MAX_BODIES + 65 {
        bodies.remember(
            &format!("body-{index}"),
            &tx,
            size,
            MAX_BODIES,
            MAX_BODY_BYTES,
        );
    }
    assert_eq!(bodies.by_txid.len(), MAX_BODIES);
    assert_eq!(bodies.bytes, MAX_BODIES * size);
    bodies.remember("body-0", &tx, usize::MAX, MAX_BODIES, MAX_BODY_BYTES);
    assert_eq!(bodies.bytes, MAX_BODIES * size);
    bodies.retain(|id| id.strip_prefix("body-").unwrap().parse::<usize>().unwrap() % 2 == 0);
    assert_eq!(bodies.bytes, MAX_BODIES / 2 * size);
    for index in MAX_BODIES..MAX_BODIES + MAX_BODIES / 2 {
        bodies.remember(
            &format!("body-{index}"),
            &tx,
            size,
            MAX_BODIES,
            MAX_BODY_BYTES,
        );
    }
    assert_eq!(bodies.by_txid.len(), MAX_BODIES);
    let mut large = fixture_tx();
    large.prefix.extra = monero::blockdata::transaction::ExtraField(vec![
        monero::blockdata::transaction::SubField::Nonce(vec![42; 8 * 1024 * 1024]),
    ])
    .into();
    let large = Arc::new(large);
    let large_size = monero::consensus::encode::serialize(large.as_ref()).len();
    let fits = MAX_BODY_BYTES / large_size;
    let mut bytes = Bodies::default();
    for index in 0..fits + 2 {
        bytes.remember(
            &format!("large-{index}"),
            &large,
            large_size,
            MAX_BODIES,
            MAX_BODY_BYTES,
        );
    }
    assert_eq!(bytes.by_txid.len(), fits);
    assert_eq!(bytes.bytes, fits * large_size);
    bytes.remember("overflow", &large, usize::MAX, MAX_BODIES, MAX_BODY_BYTES);
    assert!(!bytes.contains_key("overflow"));
    bytes.retain(|id| id != "large-0");
    bytes.remember(
        "replacement",
        &large,
        large_size,
        MAX_BODIES,
        MAX_BODY_BYTES,
    );
    assert_eq!(bytes.bytes, fits * large_size);
    bytes.retain(|_| false);
    assert_eq!(bytes.bytes, 0);
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn arrivals_cannot_increase_existing_work_wait(
        initial in 1usize..258, batch in 1usize..65, arrivals in 1usize..33,
    ) {
        let state=MempoolState::default();
        let mut pool:Vec<_>=(0..initial).map(|i|format!("old-{i:08}")).collect();
        let protected=pool.last().unwrap().clone();
        select(&state,pool.clone());
        let mut served=false;
        for round in 0..initial.div_ceil(batch) {
            pool.extend((0..arrivals).map(|i|format!("new-{round:08}-{i:08}")));
            let selected=select(&state,pool.clone());
            let completed:HashSet<&str>=selected.iter().take(batch).map(String::as_str).collect();
            served |= completed.contains(protected.as_str());
            state.rotation.lock().served(&completed);
            if served {break;}
        }
        prop_assert!(served,"new arrivals displaced already admitted work");
    }
}

async fn transaction_tenant_matrix(count: usize, transactions: usize) {
    let path = TempFile::new();
    let store = Store::open_file(&path.0).unwrap();
    let custody = crate::work::history_fixture::FlakyKeyCustody::default();
    let mut tenants = Vec::new();
    let mut original_orders = Vec::new();
    for _ in 0..count {
        let (id, handle, order) = fixture_tenant(&store, &custody, i64::MAX).await;
        tenants.push((id, handle));
        original_orders.push(order);
    }
    let db = Db::open(&path.0, &store).unwrap();
    let daemon = crate::daemon::fake::FakeDaemonClient::new();
    for height in 1..=2 {
        let hash = format!("matrix-{height}");
        daemon.push_block(&hash, vec![]);
        store
            .set_scanned_block(monero::Network::Mainnet, height, &hash)
            .unwrap();
    }
    store
        .execute_raw_for_test("UPDATE tenants SET scanned_through_height=2")
        .unwrap();
    let pool: Vec<_> = (0..transactions)
        .map(|index| transaction((index + 70) as u8, &[(&recorded_pair(), 2, 1234)]))
        .collect();
    daemon.set_mempool(pool);
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
    for _ in 0..transactions {
        fast_pass(&state, &inputs).await.unwrap();
    }
    assert_eq!(state.mempool.inner.lock().scanned.len(), transactions);
    let mut orders = Vec::new();
    for (id, _) in &tenants {
        let minor = store.allocate_minor_index(id).unwrap();
        assert_eq!(minor, 2);
        orders.push(
            store
                .create_order(&NewOrder {
                    tenant_id: id.clone(),
                    merchant_order_id: None,
                    minor_index: minor,
                    address: "matrix".into(),
                    xmr_amount_piconero: 1234 * transactions as u64,
                    description: None,
                    created_at: 1000,
                    expires_at: i64::MAX,
                    confirmations_required_override: Some(0),
                    idempotency_key: None,
                })
                .unwrap(),
        );
    }
    let bound = transactions * count.div_ceil(TENANTS_PER_TX) + count.div_ceil(16) + 10;
    for round in 0..bound {
        crate::work::run_round_at(&state, &inputs, Duration::ZERO, 1000 + round as i64)
            .await
            .into_result()
            .unwrap();
        if orders
            .iter()
            .all(|order| store.get_all_payments(&order.id).unwrap().len() == transactions)
        {
            break;
        }
        assert!(
            round + 1 < bound,
            "assertion failed: BOUNDARY: transaction-tenant-fairness"
        );
    }
    for order in &orders {
        let rows = store.get_all_payments(&order.id).unwrap();
        assert_eq!(rows.len(), transactions);
        assert!(rows
            .iter()
            .all(|r| r.amount_piconero == 1234 && r.output_index == 0));
        assert_eq!(
            rows.iter().map(|r| &r.txid).collect::<HashSet<_>>().len(),
            transactions
        );
    }
    for order in original_orders {
        assert!(store.get_all_payments(&order).unwrap().is_empty());
    }
    assert!(state.mempool.inner.lock().in_flight.is_empty());
    println!(
        "ENGINE_SCALE_RESULT matrix_tenants={count} transactions={transactions} complete_pairs={}",
        count * transactions
    );
}
proptest! {
    #![proptest_config(config())]
    #[test]
    fn each_transaction_visits_each_tenant_independently(count in 2usize..66,transactions in 2usize..7) {
        runtime().block_on(transaction_tenant_matrix(count,transactions));
    }
}
#[test]
#[ignore = "large deterministic scale package; scripts/engine-scale.sh runs it"]
fn tenant_and_transaction_rotations_cannot_phase_lock() {
    for (tenants, transactions) in [(257, 2), (513, 3), (1025, 5)] {
        runtime().block_on(transaction_tenant_matrix(tenants, transactions));
    }
}

#[test]
fn transaction_cursors_cannot_be_shared_between_three_transactions() {
    runtime().block_on(transaction_tenant_matrix(96, 3));
}
