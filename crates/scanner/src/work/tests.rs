//! The scheduler's own guarantees: every tier advances every round, work
//! resumes across rounds and restarts, failures are isolated and backed
//! off, and reorg detection stays cheap.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use monero::Transaction;

use super::*;
use crate::daemon::fake::FakeDaemonClient;
use crate::daemon::{DaemonError, KeyImageStatus, TxLocation};
use crate::scanner::tests::{
    cursor_of, fixture_tenant, fixture_tx, order_status, unrelated_tx, FlakyKeyCustody,
};
use crate::status::OrderStatus;
use crate::store::{Db, SharedStore, Store};

/// A fake node that counts the block-hash lookups made against it.
struct CountingDaemon<'a> {
    inner: &'a FakeDaemonClient,
    hash_lookups: AtomicU64,
}

impl<'a> CountingDaemon<'a> {
    fn new(inner: &'a FakeDaemonClient) -> Self {
        Self {
            inner,
            hash_lookups: AtomicU64::new(0),
        }
    }
    fn take_hash_lookups(&self) -> u64 {
        self.hash_lookups.swap(0, Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl MoneroDaemonClient for CountingDaemon<'_> {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.inner.get_height().await
    }
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        self.hash_lookups.fetch_add(1, Ordering::Relaxed);
        self.inner.get_block_hash(height).await
    }
    async fn get_chain_blocks(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
        self.inner.get_chain_blocks(start_height, count).await
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.inner.get_mempool_txids().await
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
        self.inner.get_transactions_with_ids(txids).await
    }
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        self.inner.locate_transaction(txid).await
    }
    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.inner.is_key_image_spent(key_images).await
    }
}

fn inputs<'a>(
    db: &'a Db,
    custody: &'a dyn KeyCustody,
    daemon: &'a dyn MoneroDaemonClient,
    tenants: &'a [(crate::store::TenantId, WalletHandle)],
) -> RoundInputs<'a> {
    RoundInputs {
        db,
        custody,
        daemon,
        network: monero::Network::Mainnet,
        tenants,
        reorg_check_depth: 20,
        grace_period_seconds: 0,
        scan_chunk_memory_budget_mb: 16,
    }
}

fn file_store() -> (Store, String) {
    let path = std::env::temp_dir().join(format!("scanner_rounds_{}.db", uuid::Uuid::new_v4()));
    let path = path.to_string_lossy().into_owned();
    (Store::open_file(&path).unwrap(), path)
}

/// The production database path: a worker thread with its own connection.
fn worker(store: &SharedStore, path: &str) -> Db {
    Db::open(path, &store.lock()).unwrap()
}

fn cleanup(path: &str) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{path}{suffix}"));
    }
}

/// With no time at all, each tier with work still completes one unit a
/// round: blocks move, the mempool is scanned, and status recomputes happen.
/// A throttled CPU slows everything; it never stops one kind of work.
#[tokio::test]
async fn with_no_time_at_all_every_tier_with_work_still_advances() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let (tenant, handle, order) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let daemon = FakeDaemonClient::new();
    daemon.push_block("h1", vec![]);
    daemon.push_block("h2", vec![]);
    let tenants = [(tenant.clone(), handle)];
    let state = ScanState::default();
    run_round(
        &state,
        &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants),
        Duration::ZERO,
    )
    .await
    .into_result()
    .unwrap();
    let seeded = cursor_of(&store, tenant.as_str()).unwrap();

    for i in 0..3 {
        daemon.push_block(&format!("n{i}"), vec![unrelated_tx(40 + i)]);
    }
    daemon.set_mempool(vec![fixture_tx()]);
    let mut blocks_moved = 0;
    for _ in 0..3 {
        let before = cursor_of(&store, tenant.as_str()).unwrap();
        let report = run_round(
            &state,
            &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants),
            Duration::ZERO,
        )
        .await;
        for tier in Tier::ALL {
            assert!(report.steps[tier] >= 1, "{tier} got no unit");
        }
        blocks_moved += cursor_of(&store, tenant.as_str()).unwrap() - before;
    }
    assert_eq!(
        blocks_moved, 3,
        "one block per round at least, all three scanned"
    );
    assert!(cursor_of(&store, tenant.as_str()).unwrap() >= seeded + 3);
    assert_eq!(
        store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order.to_string()))
            .unwrap()
            .len(),
        1,
        "the mempool was scanned"
    );
    assert_eq!(
        order_status(
            &store,
            &shared::ids::OrderId::new(order.as_str().to_string())
        ),
        OrderStatus::Unconfirmed,
        "and its status recomputed"
    );
}

/// Block hashes chain, so agreeing at the high-water mark settles the whole
/// window with one lookup; a divergence is found by binary search.
#[tokio::test]
async fn reorg_detection_costs_one_lookup_when_the_chain_agrees_and_log_depth_when_not() {
    let store = Store::open_in_memory().unwrap().into_shared();
    let fake = FakeDaemonClient::new();
    for h in 1..=60 {
        let height = fake.push_block(&format!("a{h}"), vec![]);
        store
            .lock()
            .set_scanned_block(monero::Network::Mainnet, height, &format!("a{h}"))
            .unwrap();
    }
    let daemon = CountingDaemon::new(&fake);
    let db = Db::over_shared(store.clone());
    let chain = chain::Chain::new(&db, &daemon, monero::Network::Mainnet, 20, 1000);
    assert_eq!(chain.detect(60).await.unwrap(), None);
    assert_eq!(daemon.take_hash_lookups(), 1);

    for fork in [41, 45, 52, 59, 60] {
        let tip = fake.get_height().await.unwrap();
        let replacement: Vec<(String, Vec<Transaction>)> = (fork..=tip)
            .map(|h| (format!("b{fork}_{h}"), vec![]))
            .collect();
        fake.reorg_from(
            fork,
            replacement
                .iter()
                .map(|(hash, txs)| (hash.as_str(), txs.clone()))
                .collect(),
        );
        assert_eq!(
            chain.detect(60).await.unwrap(),
            Some(fork),
            "fork at {fork}"
        );
        let lookups = daemon.take_hash_lookups();
        assert!(
            lookups <= 2 + 5,
            "fork at {fork}: {lookups} lookups for a 21-block window"
        );
        // Put the original chain back for the next case.
        let original: Vec<(String, Vec<Transaction>)> =
            (fork..=tip).map(|h| (format!("a{h}"), vec![])).collect();
        fake.reorg_from(
            fork,
            original
                .iter()
                .map(|(hash, txs)| (hash.as_str(), txs.clone()))
                .collect(),
        );
    }
}

/// A reorg job is durable: stopped after its first unit and restarted with a
/// fresh process, it finishes from where it was. While it is open no order
/// on the network newly settles; afterwards it settles once.
#[tokio::test]
async fn a_reorg_job_resumes_after_a_restart_and_settlement_waits_for_it() {
    let (store, path) = file_store();
    let custody = FlakyKeyCustody::default();
    let (tenant, handle, order) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    store
        .create_webhook(
            &shared::ids::TenantId::new(tenant.to_string()),
            "https://merchant.example/hook",
            "{}",
            "secret",
            1000,
        )
        .unwrap();
    let fake = FakeDaemonClient::new();
    for h in 1..=60 {
        let txs = if h == 30 { vec![fixture_tx()] } else { vec![] };
        let height = fake.push_block(&format!("a{h}"), txs);
        store
            .set_scanned_block(monero::Network::Mainnet, height, &format!("a{h}"))
            .unwrap();
    }
    store
        .execute_raw_for_test("UPDATE tenants SET scanned_through_height = 60")
        .unwrap();
    // The order's payment, 31 blocks deep: settled as soon as it's recomputed.
    store
        .record_payment_match(
            &shared::ids::OrderId::new(order.to_string()),
            &crate::daemon::fake::tx_id_hex(&fixture_tx()),
            0,
            1,
            "[\"ki\"]",
            1000,
            Some(30),
        )
        .unwrap();
    // Plenty of other payments above the fork, more than one unit handles.
    for i in 0..40u8 {
        let other =
            crate::scanner::tests::fixture_tenant(&store, &custody, crate::now_unix() + 3600)
                .await
                .2;
        store
            .record_payment_match(
                &shared::ids::OrderId::new(other.to_string()),
                &format!("{i:064x}"),
                0,
                1,
                "[\"ki\"]",
                1000,
                Some(55),
            )
            .unwrap();
    }
    fake.reorg_from(50, (50..=60).map(|_| ("b", vec![])).collect());
    let store = store.into_shared();
    let tenants = [(tenant.clone(), handle)];

    let state = ScanState::default();
    let db = worker(&store, &path);
    run_round(
        &state,
        &inputs(&db, &custody, &fake, &tenants),
        Duration::ZERO,
    )
    .await
    .into_result()
    .unwrap();
    assert!(
        store
            .lock()
            .reorg_job(monero::Network::Mainnet)
            .unwrap()
            .is_some(),
        "one unit doesn't finish a 41-payment job"
    );
    assert_ne!(
        order_status(
            &store,
            &shared::ids::OrderId::new(order.as_str().to_string())
        ),
        OrderStatus::Paid,
        "no settlement while a reorg is open"
    );
    let paid_events = |store: &SharedStore| {
        store
            .lock()
            .due_webhook_deliveries(i64::MAX, 100)
            .unwrap()
            .iter()
            .filter(|d| d.event_type == "order.paid")
            .count()
    };
    assert_eq!(paid_events(&store), 0);

    // Restart: a new process, new connections, no memory.
    drop(db);
    drop(store);
    let store = Store::open_file(&path).unwrap().into_shared();
    let db = worker(&store, &path);
    let state = ScanState::default();
    for _ in 0..20 {
        run_round(
            &state,
            &inputs(&db, &custody, &fake, &tenants),
            ROUND_BUDGET,
        )
        .await
        .into_result()
        .unwrap();
        if store
            .lock()
            .reorg_job(monero::Network::Mainnet)
            .unwrap()
            .is_none()
            && order_status(
                &store,
                &shared::ids::OrderId::new(order.as_str().to_string()),
            ) == OrderStatus::Paid
        {
            break;
        }
    }
    assert!(
        store
            .lock()
            .reorg_job(monero::Network::Mainnet)
            .unwrap()
            .is_none(),
        "the job finished"
    );
    assert_eq!(
        order_status(
            &store,
            &shared::ids::OrderId::new(order.as_str().to_string())
        ),
        OrderStatus::Paid
    );
    assert_eq!(paid_events(&store), 1, "announced once, after the rewind");
    drop(db);
    drop(store);
    cleanup(&path);
}

/// A tenant whose key custody keeps failing is retried at once twice, then
/// waits longer each time; the others keep advancing every round. Once its
/// backend recovers and its wait is over, it catches up.
#[tokio::test(start_paused = true)]
async fn a_failing_tenant_backs_off_without_holding_up_the_others() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let (failing, failing_handle, _) =
        fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let (healthy, healthy_handle, _) =
        fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let daemon = FakeDaemonClient::new();
    daemon.push_block("h1", vec![]);
    daemon.push_block("h2", vec![]);
    let tenants = [
        (failing.clone(), failing_handle),
        (healthy.clone(), healthy_handle),
    ];
    let state = ScanState::default();
    run_round(
        &state,
        &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    let start = cursor_of(&store, healthy.as_str()).unwrap();

    custody.fail(failing_handle);
    for i in 0..6u8 {
        daemon.push_block(&format!("n{i}"), vec![unrelated_tx(60 + i)]);
        run_round(
            &state,
            &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants),
            ROUND_BUDGET,
        )
        .await
        .into_result()
        .unwrap();
        assert_eq!(
            cursor_of(&store, healthy.as_str()).unwrap(),
            start + 1 + i as u64,
            "the healthy tenant never waits"
        );
    }
    let attempts = custody
        .attempts
        .lock()
        .get(&failing_handle)
        .copied()
        .unwrap_or(0);
    assert!(
        attempts <= 4,
        "backed off after its free retries, but was tried {attempts} times in 6 rounds"
    );
    assert_eq!(
        cursor_of(&store, failing.as_str()),
        Some(start),
        "never moved past a block it wasn't scanned for"
    );

    custody.recover(failing_handle);
    tokio::time::advance(Duration::from_secs(120)).await;
    for _ in 0..3 {
        run_round(
            &state,
            &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants),
            ROUND_BUDGET,
        )
        .await
        .into_result()
        .unwrap();
    }
    assert_eq!(
        cursor_of(&store, failing.as_str()),
        cursor_of(&store, healthy.as_str()),
        "caught up once its backend recovered"
    );
}

/// A block too big for one unit is scanned across rounds from a durable
/// checkpoint, surviving a restart, and its payment appears exactly once,
/// only after the whole block was scanned.
#[tokio::test]
async fn a_block_too_big_for_one_unit_resumes_from_its_checkpoint_across_restarts() {
    let (store, path) = file_store();
    let custody = FlakyKeyCustody::default();
    let (tenant, handle, order) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let daemon = FakeDaemonClient::new();
    daemon.push_block("h1", vec![]);
    daemon.push_block("h2", vec![]);
    let tenants = [(tenant.clone(), handle)];
    run_round(
        &ScanState::default(),
        &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    let before = cursor_of(&store, tenant.as_str()).unwrap();

    // The payment first, then unrelated transactions: several units' worth,
    // so a restart lands in the middle.
    let mut txs = vec![fixture_tx()];
    txs.extend((0..4 * blocks::TXS_PER_SCAN as u8).map(|i| unrelated_tx(100 + i)));
    let height = daemon.push_block("big", txs);
    let mut store = store;
    let mut rounds = 0;
    let mut last_checkpoint = 0;
    while cursor_of(&store, tenant.as_str()).unwrap() == before {
        rounds += 1;
        assert!(rounds < 100, "never finished the block");
        let state = ScanState::default();
        let db = worker(&store, &path);
        run_round(
            &state,
            &inputs(&db, &custody, &daemon, &tenants),
            Duration::ZERO,
        )
        .await
        .into_result()
        .unwrap();
        drop(db);
        if cursor_of(&store, tenant.as_str()).unwrap() == before {
            assert!(
                store
                    .lock()
                    .get_all_payments(&shared::ids::OrderId::new(order.to_string()))
                    .unwrap()
                    .is_empty(),
                "no payment before the block commits"
            );
            let checkpoint = store
                .lock()
                .block_checkpoint(
                    monero::Network::Mainnet,
                    &shared::ids::TenantId::new(tenant.to_string()),
                )
                .unwrap()
                .expect("progress is checkpointed");
            assert!(
                checkpoint.next_tx > last_checkpoint,
                "every round moves the checkpoint forward"
            );
            last_checkpoint = checkpoint.next_tx;
        }
        // Every few rounds, restart.
        if rounds % 3 == 0 {
            drop(store);
            store = Store::open_file(&path).unwrap().into_shared();
        }
    }
    assert!(rounds > 1, "the block really was split across rounds");
    assert_eq!(cursor_of(&store, tenant.as_str()), Some(height));
    let payments = store
        .lock()
        .get_all_payments(&shared::ids::OrderId::new(order.to_string()))
        .unwrap();
    assert_eq!(payments.len(), 1);
    assert_eq!(payments[0].block_height, Some(height as i64));
    assert_eq!(
        store
            .lock()
            .block_checkpoint(
                monero::Network::Mainnet,
                &shared::ids::TenantId::new(tenant.to_string())
            )
            .unwrap(),
        None,
        "checkpoint cleared at commit"
    );
    drop(store);
    cleanup(&path);
}

/// A tenant catching up gets turns while the frontier is still far behind
/// the node: turns alternate, across rounds too, so neither starves the
/// other even when each round has time for one unit.
#[tokio::test(start_paused = true)]
async fn catch_up_gets_turns_while_the_frontier_is_far_behind() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let (lagging, lagging_handle, _) =
        fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let (live, live_handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let daemon = FakeDaemonClient::new();
    daemon.push_block("h1", vec![]);
    daemon.push_block("h2", vec![]);
    let tenants = [
        (lagging.clone(), lagging_handle),
        (live.clone(), live_handle),
    ];
    let state = ScanState::default();
    run_round(
        &state,
        &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    custody.fail(lagging_handle);
    for i in 0..3u8 {
        daemon.push_block(&format!("m{i}"), vec![unrelated_tx(150 + i)]);
    }
    run_round(
        &state,
        &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    let behind = cursor_of(&store, lagging.as_str()).unwrap();
    assert!(behind < cursor_of(&store, live.as_str()).unwrap());
    custody.recover(lagging_handle);
    tokio::time::advance(Duration::from_secs(120)).await; // past its retry delay

    // Now the node is 100 blocks ahead; with no time to spare, each round
    // gives the blocks tier one unit, so turns must alternate.
    for i in 0..100u8 {
        daemon.push_block(&format!("far{i}"), vec![]);
    }
    let frontier_start = store
        .lock()
        .max_scanned_height(monero::Network::Mainnet)
        .unwrap()
        .unwrap();
    for _ in 0..4 {
        run_round(
            &state,
            &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants),
            Duration::ZERO,
        )
        .await
        .into_result()
        .unwrap();
    }
    assert!(
        cursor_of(&store, lagging.as_str()).unwrap() > behind,
        "catch-up got a turn"
    );
    assert!(
        store
            .lock()
            .max_scanned_height(monero::Network::Mainnet)
            .unwrap()
            .unwrap()
            > frontier_start,
        "and so did the frontier"
    );
}

/// A node that can't locate one transaction (every lookup for it fails):
/// keeps a reorg job open for as long as a test needs.
struct CannotLocate<'a> {
    inner: &'a FakeDaemonClient,
    txid: String,
}

#[async_trait::async_trait]
impl MoneroDaemonClient for CannotLocate<'_> {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.inner.get_height().await
    }
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        self.inner.get_block_hash(height).await
    }
    async fn get_chain_blocks(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
        self.inner.get_chain_blocks(start_height, count).await
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.inner.get_mempool_txids().await
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
        self.inner.get_transactions_with_ids(txids).await
    }
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        if txid == self.txid {
            return Err(DaemonError::Request(
                "this node can't find that transaction right now".into(),
            ));
        }
        self.inner.locate_transaction(txid).await
    }
    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.inner.is_key_image_spent(key_images).await
    }
}

/// While a reorg is being reconciled, only what depends on the chain being
/// settled waits: blocks aren't scanned and no order newly settles. The
/// mempool is still scanned and orders still expire.
#[tokio::test]
async fn an_open_reorg_pauses_blocks_and_settlement_but_not_the_mempool_or_expiry() {
    let (_guard, logs) = crate::test_log::capture();
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let now = crate::now_unix();
    let (tenant, handle, open_order) = fixture_tenant(&store, &custody, now + 3600).await;
    let (_, _, overdue_order) = fixture_tenant(&store, &custody, now - 10).await;
    let (_, _, candidate_order) = fixture_tenant(&store, &custody, now + 3600).await;
    let fake = FakeDaemonClient::new();
    for h in 1..=10 {
        let height = fake.push_block(&format!("a{h}"), vec![]);
        store
            .set_scanned_block(monero::Network::Mainnet, height, &format!("a{h}"))
            .unwrap();
    }
    store
        .execute_raw_for_test("UPDATE tenants SET scanned_through_height = 10")
        .unwrap();
    // A reorg is open, with a candidate the node can't answer about.
    let stuck = "ab".repeat(32);
    store
        .record_payment_match(
            &shared::ids::OrderId::new(candidate_order.to_string()),
            &stuck,
            0,
            1,
            "[\"ki\"]",
            now,
            Some(9),
        )
        .unwrap();
    store
        .open_reorg_job(monero::Network::Mainnet, 9, now)
        .unwrap();
    let store = store.into_shared();
    let daemon = CannotLocate {
        inner: &fake,
        txid: stuck,
    };
    fake.push_block("new", vec![unrelated_tx(200)]);
    fake.set_mempool(vec![fixture_tx()]);
    let tenants = [(tenant.clone(), handle)];

    let report = run_round(
        &ScanState::default(),
        &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants),
        ROUND_BUDGET,
    )
    .await;
    assert!(
        store
            .lock()
            .reorg_job(monero::Network::Mainnet)
            .unwrap()
            .is_some(),
        "the job is still open"
    );
    assert_eq!(
        report.outcome(Tier::Blocks),
        TierOutcome::Blocked(Wait::ReorgBeingReconciled)
    );
    assert_eq!(
        store
            .lock()
            .max_scanned_height(monero::Network::Mainnet)
            .unwrap(),
        Some(10),
        "no block scanned on a chain being reconciled"
    );
    assert_eq!(
        store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(open_order.to_string()))
            .unwrap()
            .len(),
        1,
        "the mempool was still scanned"
    );
    assert_eq!(
        order_status(
            &store,
            &shared::ids::OrderId::new(open_order.as_str().to_string())
        ),
        OrderStatus::Unconfirmed
    );
    assert_eq!(
        order_status(
            &store,
            &shared::ids::OrderId::new(overdue_order.as_str().to_string())
        ),
        OrderStatus::Expired,
        "and orders still expire"
    );
    assert_eq!(
        report.outcome(Tier::Chain),
        TierOutcome::Blocked(Wait::NodeFailed),
        "a node failure is waited out"
    );
    assert_eq!(
        logs.count("reorg work stopped: the node failed"),
        1,
        "{}",
        logs.text()
    );
}

/// A deeper reorg arriving while one is being reconciled widens the open
/// job; once both are reconciled the payment is recorded once, at its
/// height on the final chain, and the whole window is scanned again.
#[tokio::test]
async fn a_second_deeper_fork_during_a_reorg_job_ends_on_the_final_chain() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let (tenant, handle, order) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let fake = FakeDaemonClient::new();
    fake.push_block("a1", vec![]);
    fake.push_block("a2", vec![]);
    let tenants = [(tenant.clone(), handle)];
    let state = ScanState::default();
    let round = |budget| {
        let (store, fake, tenants, state, custody) = (&store, &fake, &tenants, &state, &custody);
        async move {
            run_round(
                state,
                &inputs(&Db::over_shared(store.clone()), custody, fake, tenants),
                budget,
            )
            .await
            .into_result()
            .unwrap();
        }
    };
    round(ROUND_BUDGET).await;
    for h in 3..=40u64 {
        fake.push_block(
            &format!("a{h}"),
            if h == 35 { vec![fixture_tx()] } else { vec![] },
        );
    }
    round(ROUND_BUDGET).await;
    round(ROUND_BUDGET).await;
    round(ROUND_BUDGET).await;
    round(ROUND_BUDGET).await;
    round(ROUND_BUDGET).await;
    assert_eq!(
        store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order.to_string()))
            .unwrap()[0]
            .block_height,
        Some(35)
    );

    // First fork at 35: the payment moves to 36.
    let first: Vec<(String, Vec<Transaction>)> = (35..=40u64)
        .map(|h| {
            (
                format!("b{h}"),
                if h == 36 { vec![fixture_tx()] } else { vec![] },
            )
        })
        .collect();
    fake.reorg_from(
        35,
        first
            .iter()
            .map(|(hash, txs)| (hash.as_str(), txs.clone()))
            .collect(),
    );
    round(Duration::ZERO).await; // detects and starts the job, no more
    assert!(store
        .lock()
        .reorg_job(monero::Network::Mainnet)
        .unwrap()
        .is_some());

    // Before it finishes, a deeper fork at 30: the payment moves to 31.
    let second: Vec<(String, Vec<Transaction>)> = (30..=41u64)
        .map(|h| {
            (
                format!("c{h}"),
                if h == 31 { vec![fixture_tx()] } else { vec![] },
            )
        })
        .collect();
    fake.reorg_from(
        30,
        second
            .iter()
            .map(|(hash, txs)| (hash.as_str(), txs.clone()))
            .collect(),
    );
    for _ in 0..10 {
        round(ROUND_BUDGET).await;
    }
    assert!(store
        .lock()
        .reorg_job(monero::Network::Mainnet)
        .unwrap()
        .is_none());
    let payments = store
        .lock()
        .get_all_payments(&shared::ids::OrderId::new(order.to_string()))
        .unwrap();
    assert_eq!(payments.len(), 1, "recorded once");
    assert_eq!(
        payments[0].block_height,
        Some(31),
        "at its height on the final chain"
    );
    assert_eq!(payments[0].voided_at, None);
    assert_eq!(
        store
            .lock()
            .get_scanned_block_hash(monero::Network::Mainnet, 41)
            .unwrap()
            .as_deref(),
        Some("c41"),
        "rescanned to the new tip"
    );
    assert_eq!(cursor_of(&store, tenant.as_str()), Some(41));
}

/// A node that replaces its tip right after serving a run of blocks: the
/// scanner holds (and commits) the old tip, which the chain then discards.
struct ReorgsAfterFetch<'a> {
    inner: &'a FakeDaemonClient,
    fork: u64,
    armed: std::sync::atomic::AtomicBool,
    chain_fetches: AtomicU64,
}

#[async_trait::async_trait]
impl MoneroDaemonClient for ReorgsAfterFetch<'_> {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.inner.get_height().await
    }
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        self.inner.get_block_hash(height).await
    }
    async fn get_chain_blocks(
        &self,
        start: u64,
        count: u64,
    ) -> Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
        self.chain_fetches.fetch_add(1, Ordering::Relaxed);
        let blocks = self.inner.get_chain_blocks(start, count).await?;
        if self.armed.swap(false, Ordering::Relaxed) {
            let tip = self.inner.get_height().await?;
            let replacement: Vec<(String, Vec<Transaction>)> = (self.fork..=tip)
                .map(|h| (format!("new{h}"), vec![]))
                .collect();
            self.inner.reorg_from(
                self.fork,
                replacement
                    .iter()
                    .map(|(hash, txs)| (hash.as_str(), txs.clone()))
                    .collect(),
            );
        }
        Ok(blocks)
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.inner.get_mempool_txids().await
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
        self.inner.get_transactions_with_ids(txids).await
    }
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        self.inner.locate_transaction(txid).await
    }
    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.inner.is_key_image_spent(key_images).await
    }
}

/// A payment in a block the chain then discards, whose transaction the node
/// can no longer find and which isn't provably double-spent, stops counting
/// confirmations: it goes back to unconfirmed, and its order never settles
/// on the strength of a block that no longer exists.
#[tokio::test]
async fn a_reorged_payment_the_node_cannot_find_never_settles_its_order() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let (tenant, handle, order) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let fake = FakeDaemonClient::new();
    fake.push_block("a1", vec![]);
    fake.push_block("a2", vec![]);
    let tenants = [(tenant.clone(), handle)];
    let round = || run_round_on(&store, &custody, &fake, &tenants);
    round().await;
    fake.push_block("a3", vec![fixture_tx()]);
    round().await;
    assert_eq!(
        store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order.to_string()))
            .unwrap()[0]
            .block_height,
        Some(3)
    );

    // The chain replaces block 3 with one that doesn't hold the payment,
    // and nothing proves its inputs were spent elsewhere.
    fake.reorg_from(3, vec![("b3", vec![])]);
    for i in 0..15 {
        fake.push_block(&format!("b{}", 4 + i), vec![]);
    }
    for _ in 0..6 {
        round().await;
    }
    let payment = &store
        .lock()
        .get_all_payments(&shared::ids::OrderId::new(order.to_string()))
        .unwrap()[0];
    assert_eq!(payment.block_height, None, "not in any block any more");
    assert_eq!(payment.voided_at, None, "and not voided without proof");
    assert_ne!(
        order_status(
            &store,
            &shared::ids::OrderId::new(order.as_str().to_string())
        ),
        OrderStatus::Paid,
        "no confirmations on a discarded block"
    );
}

/// Catch-up groups are served in turn: a store far behind doesn't get every
/// catch-up turn just because each turn moves it up a few blocks.
#[tokio::test]
async fn a_store_far_behind_does_not_hold_every_catch_up_turn() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let (far, far_handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let (near, near_handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let fake = FakeDaemonClient::new();
    for h in 1..=400 {
        let height = fake.push_block(&format!("a{h}"), vec![]);
        store
            .set_scanned_block(monero::Network::Mainnet, height, &format!("a{h}"))
            .unwrap();
    }
    store
        .execute_raw_for_test(&format!(
            "UPDATE tenants SET scanned_through_height = 100 WHERE id = '{far}'"
        ))
        .unwrap();
    store
        .execute_raw_for_test(&format!(
            "UPDATE tenants SET scanned_through_height = 300 WHERE id = '{near}'"
        ))
        .unwrap();
    let store = store.into_shared();
    let tenants = [(far.clone(), far_handle), (near.clone(), near_handle)];
    let state = ScanState::default();
    for _ in 0..2 {
        run_round(
            &state,
            &inputs(&Db::over_shared(store.clone()), &custody, &fake, &tenants),
            Duration::ZERO,
        )
        .await
        .into_result()
        .unwrap();
    }
    assert!(
        cursor_of(&store, far.as_str()).unwrap() > 100,
        "the far group had its turn"
    );
    assert!(
        cursor_of(&store, near.as_str()).unwrap() > 300,
        "and so did the other, on the very next turn"
    );
}

/// The node replaces its tip after the scanner fetched it: the scanner
/// commits the old tip with its own contents (never one block's payments
/// under another's hash), then detects the reorg and reconciles, so no
/// payment from the discarded block survives.
#[tokio::test]
async fn a_tip_replaced_after_it_was_fetched_leaves_no_phantom_payment() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let (tenant, handle, order) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let fake = FakeDaemonClient::new();
    fake.push_block("a1", vec![]);
    fake.push_block("a2", vec![]);
    let tenants = [(tenant.clone(), handle)];
    run_round_on(&store, &custody, &fake, &tenants).await;
    for h in 3..=11 {
        fake.push_block(
            &format!("a{h}"),
            if h == 11 { vec![fixture_tx()] } else { vec![] },
        );
    }
    let daemon = ReorgsAfterFetch {
        inner: &fake,
        fork: 11,
        armed: true.into(),
        chain_fetches: AtomicU64::new(0),
    };
    for _ in 0..6 {
        run_round_on(&store, &custody, &daemon, &tenants).await;
    }
    assert_eq!(
        store
            .lock()
            .get_scanned_block_hash(monero::Network::Mainnet, 11)
            .unwrap()
            .as_deref(),
        Some("new11"),
        "reconciled to the new tip"
    );
    let payments = store
        .lock()
        .get_all_payments(&shared::ids::OrderId::new(order.to_string()))
        .unwrap();
    assert!(
        payments.iter().all(|p| p.block_height.is_none()),
        "nothing counted in a discarded block: {payments:?}"
    );
    assert_ne!(
        order_status(
            &store,
            &shared::ids::OrderId::new(order.as_str().to_string())
        ),
        OrderStatus::Paid
    );
}

/// A catch-up group whose stores can't be scanned (keys not registered)
/// costs no block fetches: it waits at its cursor.
#[tokio::test]
async fn a_group_with_nobody_to_scan_fetches_nothing() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let (unregistered, _, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let fake = FakeDaemonClient::new();
    for h in 1..=20 {
        let height = fake.push_block(&format!("a{h}"), vec![]);
        store
            .set_scanned_block(monero::Network::Mainnet, height, &format!("a{h}"))
            .unwrap();
    }
    store
        .execute_raw_for_test(&format!(
            "UPDATE tenants SET scanned_through_height = 5 WHERE id = '{unregistered}'"
        ))
        .unwrap();
    let store = store.into_shared();
    let daemon = ReorgsAfterFetch {
        inner: &fake,
        fork: 0,
        armed: false.into(),
        chain_fetches: AtomicU64::new(0),
    };
    run_round_on(&store, &custody, &daemon, &[]).await;
    assert_eq!(daemon.chain_fetches.load(Ordering::Relaxed), 0);
    assert_eq!(
        cursor_of(&store, unregistered.as_str()),
        Some(5),
        "still where it was, to be caught up once registered"
    );
}

/// One round with the default budget and fresh memory.
async fn run_round_on(
    store: &SharedStore,
    custody: &dyn KeyCustody,
    daemon: &dyn MoneroDaemonClient,
    tenants: &[(crate::store::TenantId, WalletHandle)],
) {
    run_round(
        &ScanState::default(),
        &inputs(&Db::over_shared(store.clone()), custody, daemon, tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
}

/// A node that fails every transaction lookup (and counts them), or every
/// body fetch until told otherwise (recording what was asked).
struct Lookups<'a> {
    inner: &'a FakeDaemonClient,
    locate_calls: AtomicU64,
    fail_bodies: std::sync::atomic::AtomicBool,
    body_requests: parking_lot::Mutex<Vec<Vec<String>>>,
}

impl<'a> Lookups<'a> {
    fn new(inner: &'a FakeDaemonClient) -> Self {
        Self {
            inner,
            locate_calls: AtomicU64::new(0),
            fail_bodies: false.into(),
            body_requests: Default::default(),
        }
    }
}

#[async_trait::async_trait]
impl MoneroDaemonClient for Lookups<'_> {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.inner.get_height().await
    }
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        self.inner.get_block_hash(height).await
    }
    async fn get_chain_blocks(
        &self,
        start: u64,
        count: u64,
    ) -> Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
        self.inner.get_chain_blocks(start, count).await
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.inner.get_mempool_txids().await
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
        let mut asked = txids.to_vec();
        asked.sort();
        self.body_requests.lock().push(asked);
        if self.fail_bodies.load(Ordering::Relaxed) {
            return Err(DaemonError::Request("bodies unavailable".into()));
        }
        self.inner.get_transactions_with_ids(txids).await
    }
    async fn locate_transaction(&self, _txid: &str) -> Result<TxLocation, DaemonError> {
        self.locate_calls.fetch_add(1, Ordering::Relaxed);
        Err(DaemonError::Request(
            "this node can't look transactions up right now".into(),
        ))
    }
    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.inner.is_key_image_spent(key_images).await
    }
}

/// A node that fails reorg lookups is asked once a round, not once per
/// candidate: waiting on it payment after payment (each up to the call
/// deadline) would stall the whole round.
#[tokio::test]
async fn a_failing_node_is_asked_once_a_round_about_reorg_candidates() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let fake = FakeDaemonClient::new();
    for h in 1..=10 {
        let height = fake.push_block(&format!("a{h}"), vec![]);
        store
            .set_scanned_block(monero::Network::Mainnet, height, &format!("a{h}"))
            .unwrap();
    }
    for i in 0..20u8 {
        let order = fixture_tenant(&store, &custody, crate::now_unix() + 3600)
            .await
            .2;
        store
            .record_payment_match(
                &shared::ids::OrderId::new(order.to_string()),
                &format!("{i:064x}"),
                0,
                1,
                "[]",
                1000,
                Some(9),
            )
            .unwrap();
    }
    store
        .open_reorg_job(monero::Network::Mainnet, 9, crate::now_unix())
        .unwrap();
    let store = store.into_shared();
    let daemon = Lookups::new(&fake);
    let state = ScanState::default();
    let report = run_round(
        &state,
        &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &[]),
        ROUND_BUDGET,
    )
    .await;
    assert_eq!(daemon.locate_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        report.outcome(Tier::Chain),
        TierOutcome::Blocked(Wait::NodeFailed)
    );
    assert!(
        report.error.is_none(),
        "a node failure is retried, not a failed round"
    );
}

/// One order whose recompute keeps failing waits to be retried; the others
/// are still recomputed and the round doesn't fail for it.
#[tokio::test]
async fn one_failing_recompute_does_not_hold_up_the_others() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let now = crate::now_unix();
    let (_, _, poisoned) = fixture_tenant(&store, &custody, now - 10).await;
    let (_, _, healthy) = fixture_tenant(&store, &custody, now - 10).await;
    let fake = FakeDaemonClient::new();
    for h in 1..=3 {
        let height = fake.push_block(&format!("a{h}"), vec![]);
        store
            .set_scanned_block(monero::Network::Mainnet, height, &format!("a{h}"))
            .unwrap();
    }
    store
        .execute_raw_for_test("UPDATE tenants SET scanned_through_height = 3")
        .unwrap();
    store
        .execute_raw_for_test(&format!(
            "CREATE TRIGGER poisoned_order BEFORE UPDATE OF status ON orders WHEN NEW.id = '{poisoned}'
             BEGIN SELECT RAISE(ABORT, 'simulated persistent failure'); END;"
        ))
        .unwrap();
    let store = store.into_shared();
    let state = ScanState::default();
    let db = Db::over_shared(store.clone());
    let round =
        || async { run_round(&state, &inputs(&db, &custody, &fake, &[]), ROUND_BUDGET).await };
    let first = round().await;
    assert!(
        first.error.is_none(),
        "the other order went through: {:?}",
        first.error
    );
    assert_eq!(
        order_status(
            &store,
            &shared::ids::OrderId::new(healthy.as_str().to_string())
        ),
        OrderStatus::Expired
    );
    // Alone in its page it is reported (a real storage failure), until its
    // free retries are spent; then it waits, and the round is clean.
    round().await;
    round().await;
    assert!(state
        .order_backoff
        .is_waiting(&shared::ids::OrderId::new(poisoned.to_string())));
    let later = round().await;
    assert!(later.error.is_none(), "{:?}", later.error);
    assert_eq!(
        order_status(
            &store,
            &shared::ids::OrderId::new(poisoned.as_str().to_string())
        ),
        OrderStatus::Pending
    );
}

/// Backoff entries for keys that stopped failing (and stopped being tried)
/// are forgotten after an hour, so the map can't grow without bound.
#[tokio::test(start_paused = true)]
async fn backoff_forgets_keys_that_stopped_failing() {
    let backoff = Backoff::<crate::store::TenantId>::default();
    for _ in 0..4 {
        backoff.failed(&shared::ids::TenantId::new("gone"));
    }
    assert_eq!(backoff.waiting(), vec!["gone".to_string()]);
    tokio::time::advance(Duration::from_secs(61 * 60)).await;
    assert!(backoff.waiting().is_empty());
    assert!(
        backoff.failures.lock().is_empty(),
        "forgotten, not just no longer waiting"
    );
}

/// Mempool bodies that couldn't be fetched are asked for again next round,
/// not skipped until the rotation comes round again.
#[tokio::test]
async fn a_failed_mempool_body_fetch_is_retried_next_round() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let (tenant, handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let fake = FakeDaemonClient::new();
    fake.push_block("a1", vec![]);
    fake.push_block("a2", vec![]);
    fake.set_mempool((0..100u8).map(unrelated_tx).collect());
    let daemon = Lookups::new(&fake);
    daemon.fail_bodies.store(true, Ordering::Relaxed);
    let tenants = [(tenant, handle)];
    let state = ScanState::default();
    run_round(
        &state,
        &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants),
        ROUND_BUDGET,
    )
    .await;
    daemon.fail_bodies.store(false, Ordering::Relaxed);
    run_round(
        &state,
        &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants),
        ROUND_BUDGET,
    )
    .await;
    let requests = daemon.body_requests.lock();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1], "the same slice again");
}

/// A node that takes its time serving blocks.
struct SlowBlocks<'a> {
    inner: &'a FakeDaemonClient,
    delay: Duration,
    chain_fetches: AtomicU64,
}

#[async_trait::async_trait]
impl MoneroDaemonClient for SlowBlocks<'_> {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.inner.get_height().await
    }
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        self.inner.get_block_hash(height).await
    }
    async fn get_chain_blocks(
        &self,
        start: u64,
        count: u64,
    ) -> Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
        self.chain_fetches.fetch_add(1, Ordering::Relaxed);
        tokio::time::sleep(self.delay).await;
        self.inner.get_chain_blocks(start, count).await
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
        self.inner.get_transactions_with_ids(txids).await
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.inner.get_mempool_txids().await
    }
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        self.inner.locate_transaction(txid).await
    }
    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.inner.is_key_image_spent(key_images).await
    }
}

/// The next blocks are fetched while the current one is scanned, and a
/// block fetched ahead is used, not fetched again. With a 100 ms fetch and a
/// 100 ms scan per block, five blocks take about 600 ms overlapped rather
/// than 1000 ms one after the other, and five fetches, not ten.
#[tokio::test(start_paused = true)]
async fn the_next_block_is_fetched_while_this_one_is_scanned_and_used() {
    let store = Store::open_in_memory().unwrap();
    let custody = crate::scanner::tests::SlowKeyCustody::default();
    let (tenant, handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let fake = FakeDaemonClient::new();
    fake.push_block("a1", vec![]);
    fake.push_block("a2", vec![]);
    let tenants = [(tenant.clone(), handle)];
    run_round_on(&store, &custody, &fake, &tenants).await;
    let start = cursor_of(&store, tenant.as_str()).unwrap();
    for h in 3..=7 {
        fake.push_block(&format!("a{h}"), vec![fixture_tx()]);
    }
    custody
        .delays
        .lock()
        .insert(handle, Duration::from_millis(100));
    let daemon = SlowBlocks {
        inner: &fake,
        delay: Duration::from_millis(100),
        chain_fetches: AtomicU64::new(0),
    };
    let db = Db::over_shared(store.clone());
    let inputs = RoundInputs {
        scan_chunk_memory_budget_mb: 0,
        ..inputs(&db, &custody, &daemon, &tenants)
    };
    let started = tokio::time::Instant::now();
    run_round(&ScanState::default(), &inputs, ROUND_BUDGET)
        .await
        .into_result()
        .unwrap();
    let took = started.elapsed();
    assert_eq!(cursor_of(&store, tenant.as_str()), Some(start + 5));
    assert_eq!(
        daemon.chain_fetches.load(Ordering::Relaxed),
        5,
        "each block fetched once"
    );
    assert!(
        took < Duration::from_millis(800),
        "fetches overlapped scans: {took:?}"
    );
}

/// A payment is settled from the pool without waiting for a round: the fast
/// pass records it, recomputes its order, enqueues the webhook and wakes
/// delivery, and the round's rotation then has nothing left to do for it.
#[tokio::test]
async fn the_fast_path_settles_a_new_pool_payment_at_once() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let (tenant, handle, order) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    store
        .create_webhook(
            &shared::ids::TenantId::new(tenant.to_string()),
            "https://merchant.example/hook",
            "{}",
            "secret",
            1000,
        )
        .unwrap();
    let store = store.into_shared();
    let fake = FakeDaemonClient::new();
    fake.push_block("a1", vec![]);
    fake.set_mempool(vec![fixture_tx()]);
    let wake = std::sync::Arc::new(tokio::sync::Notify::new());
    let state = ScanState::waking(wake.clone());
    let tenants = [(tenant.clone(), handle)];
    let db = Db::over_shared(store.clone());
    let report = fast_pass(&state, &inputs(&db, &custody, &fake, &tenants))
        .await
        .unwrap();
    assert_eq!(
        report,
        FastReport {
            scanned: 1,
            paid_orders: 1,
            deferred: 0
        }
    );
    assert_eq!(
        order_status(
            &store,
            &shared::ids::OrderId::new(order.as_str().to_string())
        ),
        OrderStatus::Unconfirmed
    );
    assert!(store
        .lock()
        .due_webhook_deliveries(i64::MAX / 2, 10)
        .unwrap()
        .iter()
        .any(|d| d.event_type == "order.unconfirmed"));
    tokio::time::timeout(Duration::from_millis(100), wake.notified())
        .await
        .expect("delivery was woken");

    // Seen: the next pass has nothing new, and the rotation skips it.
    let again = fast_pass(&state, &inputs(&db, &custody, &fake, &tenants))
        .await
        .unwrap();
    assert_eq!(again, FastReport::default());
    let before = custody.attempts.lock().get(&handle).copied();
    run_round(
        &state,
        &inputs(&db, &custody, &fake, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    assert_eq!(
        custody.attempts.lock().get(&handle).copied(),
        before,
        "already scanned for this store"
    );
}

/// A flood of new transactions is scanned up to the pass's budget; the rest
/// is left to the next passes and the rotation, never dropped.
#[tokio::test]
async fn the_fast_path_defers_what_its_budget_does_not_cover() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let mut tenants = Vec::new();
    for _ in 0..64 {
        let (tenant, handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        tenants.push((tenant, handle));
    }
    let store = store.into_shared();
    let fake = FakeDaemonClient::new();
    fake.push_block("a1", vec![]);
    fake.set_mempool((0..100u8).map(unrelated_tx).collect());
    let state = ScanState::default();
    let db = Db::over_shared(store.clone());
    let first = fast_pass(&state, &inputs(&db, &custody, &fake, &tenants))
        .await
        .unwrap();
    assert_eq!(first.scanned, 4096 / 64);
    assert_eq!(first.deferred, 100 - 4096 / 64);
    let second = fast_pass(&state, &inputs(&db, &custody, &fake, &tenants))
        .await
        .unwrap();
    assert_eq!(second.scanned, 100 - 4096 / 64, "the rest on the next pass");
    assert_eq!(second.deferred, 0);
}

/// A pool that can't be read is not an empty pool.
#[tokio::test]
async fn the_fast_path_reports_an_unreadable_pool() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let (tenant, handle, _order) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let fake = FakeDaemonClient::new();
    fake.set_online(false);
    let db = Db::over_shared(store.clone());
    let tenants = [(tenant, handle)];
    assert_eq!(
        fast_pass(
            &ScanState::default(),
            &inputs(&db, &custody, &fake, &tenants)
        )
        .await,
        None
    );
}

/// With no store to scan for, the fast path doesn't ask the node about its
/// pool at all: a node that would fail the poll is never reached.
#[tokio::test]
async fn the_fast_path_leaves_the_node_alone_while_no_store_has_an_order_in_scope() {
    let store = Store::open_in_memory().unwrap().into_shared();
    let custody = FlakyKeyCustody::default();
    let fake = FakeDaemonClient::new();
    fake.set_online(false);
    let db = Db::over_shared(store.clone());
    assert_eq!(
        fast_pass(&ScanState::default(), &inputs(&db, &custody, &fake, &[])).await,
        Some(FastReport::default())
    );
}

// -- Fault sweeps ------------------------------------------------------------
//
// Every SQL statement a round runs is failed in turn, one per run: the round
// may report the failure or wait it out, but it must not panic, and it must
// leave nothing a later clean round can't finish. The database has to end
// exactly where a run without the fault ends. A failure that left a cursor
// moved without its matches, a job half-open or a webhook sent twice shows
// up as a difference.

/// One run of a sweep's story: a tenant with one order, a chain and a pool
/// that each step of the story changes before its round.
struct Story {
    store: SharedStore,
    custody: FlakyKeyCustody,
    daemon: FakeDaemonClient,
    tenants: Vec<(crate::store::TenantId, WalletHandle)>,
    order: crate::store::OrderId,
    state: ScanState,
}

impl Story {
    async fn new() -> Self {
        let store = Store::open_in_memory().unwrap();
        let custody = FlakyKeyCustody::default();
        let (tenant, handle, order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        store
            .create_webhook(
                &shared::ids::TenantId::new(tenant.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        Self {
            store: store.into_shared(),
            custody,
            daemon,
            tenants: vec![(tenant, handle)],
            order,
            state: ScanState::default(),
        }
    }

    fn confirm(&self) {
        for i in 0..10 {
            self.daemon.push_block(&format!("c{i}"), vec![]);
        }
    }

    async fn round(&self) -> RoundReport {
        let db = Db::over_shared(self.store.clone());
        let report = run_round(
            &self.state,
            &inputs(&db, &self.custody, &self.daemon, &self.tenants),
            ROUND_BUDGET,
        )
        .await;
        // The story is a few blocks: a tier that needs more units than this
        // is asking again for work it can't do.
        for (tier, steps) in report.steps.iter() {
            assert!(
                steps < 64,
                "{tier} ran {steps} units in one round: {report:?}"
            );
        }
        report
    }

    /// Clean rounds until nothing is left.
    async fn settle(&self) {
        for _ in 0..8 {
            let report = self.round().await;
            if report.error.is_none() && !report.backlogged() {
                return;
            }
        }
        panic!("the story never settled");
    }

    /// What the story's outcome is, without ids and timestamps that differ
    /// between runs.
    fn outcome(&self) -> String {
        let s = self.store.lock();
        let rows = |sql: &str| -> Vec<String> {
            let mut stmt = s.conn_for_test().prepare(sql).unwrap();
            let columns = stmt.column_count();
            let mut rows: Vec<String> = stmt
                .query_map([], |row| {
                    Ok((0..columns)
                        .map(|i| crate::store::value_text(row.get_ref(i).unwrap()))
                        .collect::<Vec<_>>()
                        .join("|"))
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            rows.sort();
            rows
        };
        format!(
            "orders {:?}\npayments {:?}\ncursors {:?}\nblocks {:?}\nreorg {:?}\nwebhooks {:?}\npartial {:?}",
            rows("SELECT status, amount_received_piconero, double_spend_detected_at_utc IS NOT NULL FROM orders"),
            rows("SELECT txid, output_index, amount_piconero, block_height, voided_at_utc IS NOT NULL FROM order_payments"),
            rows("SELECT scanned_through_height FROM tenants"),
            rows("SELECT height, block_hash FROM scanned_blocks"),
            rows("SELECT (SELECT COUNT(*) FROM reorg_jobs), (SELECT COUNT(*) FROM reorg_work)"),
            rows("SELECT event_type FROM webhook_deliveries"),
            rows("SELECT (SELECT COUNT(*) FROM partial_block_progress), (SELECT COUNT(*) FROM partial_block_matches)"),
        )
    }
}

/// A story's steps: each sets up the chain and pool before its round.
type Steps = [fn(&Story)];

/// A payment seen in the pool, mined, reorged out and back in, then
/// confirmed.
const PAID: &Steps = &[
    |_| {},
    |story| {
        story
            .daemon
            .set_mempool(vec![fixture_tx(), unrelated_tx(3)])
    },
    |story| {
        story.daemon.set_mempool(vec![]);
        story
            .daemon
            .push_block("b3", vec![fixture_tx(), unrelated_tx(4)]);
    },
    |story| {
        // Reorged out: back in the pool, the block replaced.
        story
            .daemon
            .reorg_from(3, vec![("b3x", vec![unrelated_tx(5)]), ("b4x", vec![])]);
        story.daemon.set_mempool(vec![fixture_tx()]);
    },
    |story| {
        story.daemon.set_mempool(vec![]);
        story.daemon.push_block("b5", vec![fixture_tx()]);
    },
    Story::confirm,
];

/// A payment seen in the pool that leaves it because a conflicting
/// transaction spending the same inputs was mined: voided, and the merchant
/// told.
const DOUBLE_SPENT: &Steps = &[
    |_| {},
    |story| story.daemon.set_mempool(vec![fixture_tx()]),
    |story| {
        story.daemon.drop_from_mempool(&fixture_tx());
        story
            .daemon
            .push_block("b3", vec![crate::scanner::tests::conflicting_tx(11)]);
        let payments = story
            .store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(story.order.to_string()))
            .unwrap();
        for payment in payments {
            for image in serde_json::from_str::<Vec<String>>(&payment.key_images_json).unwrap() {
                story
                    .daemon
                    .set_key_image_status(&image, KeyImageStatus::SpentInBlockchain);
            }
        }
    },
    Story::confirm,
];

/// The story with a fault at SQL access `fault` during step `faulted`'s
/// round. Returns the outcome and whether the fault was reached.
async fn run_story(steps: &Steps, faulted: Option<(usize, usize)>) -> (String, bool) {
    let story = Story::new().await;
    let mut reached = faulted.is_none();
    for (step, stage) in steps.iter().enumerate() {
        stage(&story);
        match faulted {
            Some((at, fault)) if at == step => {
                let seen = story.store.lock().fail_nth_access(Some(fault));
                // The round may report the failure or wait it out; what
                // matters is where the story ends.
                story.round().await;
                story.store.lock().fail_nth_access(None);
                reached = seen.load(Ordering::Relaxed) > fault;
            }
            _ => {
                story.round().await;
            }
        }
        story.settle().await;
    }
    (story.outcome(), reached)
}

/// Fails every SQL statement of step `step`'s round in turn; the story must
/// end where it ends without a fault, whose orders are `ending`.
async fn sweep_step(steps: &Steps, step: usize, ending: &str) {
    // Every event enabled, so the failure paths' log lines run too.
    let (_logs, _) = crate::test_log::capture();
    let (expected, _) = run_story(steps, None).await;
    assert!(
        expected.contains(ending),
        "the story ends {ending}: {expected}"
    );
    let mut faults = 0;
    for fault in 0.. {
        let (outcome, reached) = run_story(steps, Some((step, fault))).await;
        if !reached {
            break;
        }
        faults += 1;
        assert_eq!(
            outcome, expected,
            "a fault at SQL statement {fault} in step {step} changed the outcome"
        );
    }
    assert!(faults > 10, "the sweep reached only {faults} statements");
}

const PAID_ENDING: &str = "orders [\"overpaid|7000000000|0\"]";
const VOIDED_ENDING: &str = "orders [\"pending|0|1\"]";

// One test per step, so they run side by side.
#[tokio::test]
async fn every_sql_failure_while_seeding_is_recovered_from() {
    sweep_step(PAID, 0, PAID_ENDING).await;
}

#[tokio::test]
async fn every_sql_failure_while_a_payment_is_in_the_pool_is_recovered_from() {
    sweep_step(PAID, 1, PAID_ENDING).await;
}

#[tokio::test]
async fn every_sql_failure_while_a_payment_is_mined_is_recovered_from() {
    sweep_step(PAID, 2, PAID_ENDING).await;
}

#[tokio::test]
async fn every_sql_failure_during_a_reorg_is_recovered_from() {
    sweep_step(PAID, 3, PAID_ENDING).await;
}

#[tokio::test]
async fn every_sql_failure_while_a_payment_is_mined_again_is_recovered_from() {
    sweep_step(PAID, 4, PAID_ENDING).await;
}

#[tokio::test]
async fn every_sql_failure_while_a_payment_confirms_is_recovered_from() {
    sweep_step(PAID, 5, PAID_ENDING).await;
}

/// A node that fails block-hash lookups (the chain tier's fork check) but
/// serves blocks.
struct HashLookupsFail<'a>(&'a FakeDaemonClient);

#[async_trait::async_trait]
impl MoneroDaemonClient for HashLookupsFail<'_> {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.0.get_height().await
    }
    async fn get_block_hash(&self, _height: u64) -> Result<String, DaemonError> {
        Err(DaemonError::Request("hash lookups are failing".into()))
    }
    async fn get_chain_blocks(
        &self,
        start: u64,
        count: u64,
    ) -> Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
        self.0.get_chain_blocks(start, count).await
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.0.get_mempool_txids().await
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
        self.0.get_transactions_with_ids(txids).await
    }
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        self.0.locate_transaction(txid).await
    }
    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.0.is_key_image_spent(key_images).await
    }
}

/// A fork the chain tier couldn't open a job for this round (its node
/// lookups failed) stops the frontier for the round: it waits, rather than
/// asking for the diverging block again and again until the round's time
/// runs out.
#[tokio::test]
async fn a_fork_not_yet_opened_stops_the_frontier_instead_of_spinning() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let (tenant, handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let fake = FakeDaemonClient::new();
    for i in 0..4 {
        fake.push_block(&format!("a{i}"), vec![]);
    }
    let tenants = [(tenant, handle)];
    let state = ScanState::default();
    let db = Db::over_shared(store.clone());
    run_round(
        &state,
        &inputs(&db, &custody, &fake, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    fake.push_block("a4", vec![]);
    run_round(
        &state,
        &inputs(&db, &custody, &fake, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    fake.reorg_from(5, vec![("b5", vec![]), ("b6", vec![])]);

    let started = std::time::Instant::now();
    let report = run_round(
        &state,
        &inputs(&db, &custody, &HashLookupsFail(&fake), &tenants),
        ROUND_BUDGET,
    )
    .await;
    assert_eq!(
        report.outcome(Tier::Blocks),
        TierOutcome::Blocked(Wait::ChainDiverged),
        "{report:?}"
    );
    assert_eq!(report.steps[Tier::Blocks], 1);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the round didn't spend its budget re-asking"
    );
    assert!(
        store
            .lock()
            .reorg_job(monero::Network::Mainnet)
            .unwrap()
            .is_none(),
        "no job yet: the node couldn't be asked"
    );

    // With the node answering again, the fork is reconciled and scanning
    // carries on along the new chain.
    for _ in 0..3 {
        run_round(
            &state,
            &inputs(&db, &custody, &fake, &tenants),
            ROUND_BUDGET,
        )
        .await
        .into_result()
        .unwrap();
    }
    let s = store.lock();
    assert_eq!(
        s.scanned_blocks_between(monero::Network::Mainnet, 5, 6)
            .unwrap(),
        vec![(5, "b5".to_string()), (6, "b6".to_string())]
    );
}

/// A call the node never answers fails at the deadline, as a node failure.
#[tokio::test(start_paused = true)]
async fn a_call_the_node_never_answers_fails_at_the_deadline() {
    let never = std::future::pending::<Result<(), DaemonError>>();
    let started = tokio::time::Instant::now();
    let result = bounded(never).await;
    assert!(
        matches!(result, Err(ScannerError::Daemon(DaemonError::Request(ref m))) if m.contains("no answer within")),
        "{result:?}"
    );
    assert_eq!(started.elapsed(), CALL_DEADLINE);
}

/// Tiers and wait reasons read as words in logs and reports, each its own.
#[test]
fn tiers_and_wait_reasons_display_distinctly() {
    let tiers: Vec<String> = Tier::ALL.iter().map(ToString::to_string).collect();
    assert_eq!(
        tiers,
        ["chain", "blocks", "mempool", "settlement", "upkeep"]
    );
    let waits = [
        Wait::ChainHeightUnknown,
        Wait::ReorgBeingReconciled,
        Wait::RewoundThisRound,
        Wait::NodeFailed,
        Wait::NodeCannotServeTip,
        Wait::MempoolUnreadable,
        Wait::ReorgCandidatesRetrying,
        Wait::ChainDiverged,
    ];
    let texts: std::collections::HashSet<String> = waits.iter().map(ToString::to_string).collect();
    assert_eq!(texts.len(), waits.len());
    assert!(texts.iter().all(|t| !t.is_empty()));
}

/// A round that can't read the chain height reports it (once a minute in
/// the log) and still scans the mempool.
#[tokio::test]
async fn a_round_without_the_chain_height_reports_it_and_scans_the_pool() {
    let (_guard, logs) = crate::test_log::capture();
    let store = Store::open_in_memory().unwrap().into_shared();
    let custody = FlakyKeyCustody::default();
    let fake = FakeDaemonClient::new();
    fake.set_online(false);
    let db = Db::over_shared(store.clone());
    let report = run_round(
        &ScanState::default(),
        &inputs(&db, &custody, &fake, &[]),
        ROUND_BUDGET,
    )
    .await;
    assert!(
        matches!(report.error, Some(ScannerError::Daemon(_))),
        "{:?}",
        report.error
    );
    assert_eq!(
        report.outcome(Tier::Blocks),
        TierOutcome::Blocked(Wait::ChainHeightUnknown)
    );
    assert_eq!(
        logs.count("reading the chain height failed"),
        1,
        "{}",
        logs.text()
    );
    run_round(
        &ScanState::default(),
        &inputs(&db, &custody, &fake, &[]),
        ROUND_BUDGET,
    )
    .await;
    assert_eq!(
        logs.count("reading the chain height failed"),
        1,
        "throttled: {}",
        logs.text()
    );
}

// -- Reorg job edge cases, at the chain tier's own level ----------------------

/// A node that runs `hook` on each transaction lookup before answering it:
/// for changing the database in the middle of a reorg page.
struct OnLocate<'a> {
    inner: &'a FakeDaemonClient,
    hook: Box<dyn Fn(&str) + Send + Sync + 'a>,
}

#[async_trait::async_trait]
impl MoneroDaemonClient for OnLocate<'_> {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.inner.get_height().await
    }
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        self.inner.get_block_hash(height).await
    }
    async fn get_chain_blocks(
        &self,
        start: u64,
        count: u64,
    ) -> Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
        self.inner.get_chain_blocks(start, count).await
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.inner.get_mempool_txids().await
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
        self.inner.get_transactions_with_ids(txids).await
    }
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        (self.hook)(txid);
        self.inner.locate_transaction(txid).await
    }
    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.inner.is_key_image_spent(key_images).await
    }
}

/// Blocks a1..a10 on the node and recorded, one order per payment, each
/// payment `(txid, height)` recorded, and a reorg job open at 9.
async fn open_reorg_with(
    payments: &[(&str, u64)],
) -> (SharedStore, FakeDaemonClient, Vec<crate::store::OrderId>) {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let now = crate::now_unix();
    let fake = FakeDaemonClient::new();
    for h in 1..=10 {
        let height = fake.push_block(&format!("a{h}"), vec![]);
        store
            .set_scanned_block(monero::Network::Mainnet, height, &format!("a{h}"))
            .unwrap();
    }
    let mut orders = Vec::new();
    for (txid, height) in payments {
        let (_, _, order) = fixture_tenant(&store, &custody, now + 3600).await;
        store
            .record_payment_match(&order, txid, 0, 1, "[\"ki\"]", now, Some(*height as i64))
            .unwrap();
        orders.push(order);
    }
    store
        .execute_raw_for_test("UPDATE tenants SET scanned_through_height = 10")
        .unwrap();
    store
        .open_reorg_job(monero::Network::Mainnet, 9, now)
        .unwrap();
    (store.into_shared(), fake, orders)
}

/// Runs the job one unit at a time, at `now`, until it rewinds; returns the
/// steps it took.
async fn run_job(chain: &chain::Chain<'_>) -> Vec<&'static str> {
    let mut steps = Vec::new();
    loop {
        let step = chain
            .advance_job(
                10,
                &mut HashSet::new(),
                tokio::time::Instant::now() + ROUND_BUDGET,
            )
            .await
            .unwrap();
        steps.push(match step {
            None => panic!("no job"),
            Some(chain::JobStep::Collected) => "collected",
            Some(chain::JobStep::Processed { failure: None, .. }) => "processed",
            Some(chain::JobStep::Processed {
                failure: Some(_), ..
            }) => "failed",
            Some(chain::JobStep::Waiting) => "waiting",
            Some(chain::JobStep::Rewound) => return steps,
        });
        assert!(steps.len() < 64, "the job never finished: {steps:?}");
    }
}

/// A payment the node never answers about is retried with growing delays,
/// then given up on after `MAX_CANDIDATE_ATTEMPTS`: left as recorded, so
/// the job, and settlement behind it, can't be held up forever.
#[tokio::test]
async fn a_candidate_the_node_never_answers_about_is_given_up_on() {
    let (_guard, logs) = crate::test_log::capture();
    let stuck = "ab".repeat(32);
    let (store, fake, orders) = open_reorg_with(&[(&stuck, 9)]).await;
    let daemon = CannotLocate {
        inner: &fake,
        txid: stuck,
    };
    let db = Db::over_shared(store.clone());
    let mut now = crate::now_unix();
    let mut failed = 0;
    loop {
        // Far enough on for any retry delay to have passed.
        now += 1000;
        let chain = chain::Chain::new(&db, &daemon, monero::Network::Mainnet, 20, now);
        match chain
            .advance_job(
                10,
                &mut HashSet::new(),
                tokio::time::Instant::now() + ROUND_BUDGET,
            )
            .await
            .unwrap()
        {
            Some(chain::JobStep::Processed {
                failure: Some(_), ..
            }) => failed += 1,
            Some(chain::JobStep::Rewound) => break,
            Some(_) => {}
            None => panic!("the job vanished"),
        }
        assert!(failed < 20, "never given up on");
    }
    assert_eq!(
        failed, 11,
        "eleven failures retried, the twelfth given up on"
    );
    assert_eq!(
        logs.count("giving up re-examining a payment"),
        1,
        "{}",
        logs.text()
    );
    let payment = &store
        .lock()
        .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
        .unwrap()[0];
    assert_eq!(
        (payment.block_height, payment.voided_at),
        (Some(9), None),
        "left as recorded"
    );
    assert!(store
        .lock()
        .reorg_job(monero::Network::Mainnet)
        .unwrap()
        .is_none());
}

/// A candidate whose payment is deleted (its order removed) between the
/// page being read and its turn is simply done.
#[tokio::test]
async fn a_candidate_deleted_mid_page_is_skipped() {
    let tx = fixture_tx();
    let txid = crate::daemon::fake::tx_id_hex(&tx);
    let gone = "cd".repeat(32);
    let (store, fake, orders) = open_reorg_with(&[(&txid, 9), (&gone, 9)]).await;
    fake.reorg_from(9, vec![("b9", vec![tx]), ("b10", vec![])]);
    let deleted = orders[1].clone();
    let daemon = OnLocate {
        inner: &fake,
        hook: Box::new(|_| {
            let _ = store.lock().execute_raw_for_test(&format!(
                "DELETE FROM order_payments WHERE order_id = '{deleted}'"
            ));
        }),
    };
    let db = Db::over_shared(store.clone());
    let chain = chain::Chain::new(
        &db,
        &daemon,
        monero::Network::Mainnet,
        20,
        crate::now_unix(),
    );
    let steps = run_job(&chain).await;
    assert_eq!(
        steps,
        ["collected", "collected", "processed"],
        "both handled in one page, neither failed"
    );
    assert_eq!(
        store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
            .unwrap()[0]
            .block_height,
        Some(9)
    );
}

/// A voided payment the void recheck restores while the reorg job is
/// re-examining it: the job doesn't restore it twice, and still records
/// the block the node now has it in.
#[tokio::test]
async fn a_void_restored_meanwhile_still_gets_its_new_height() {
    let tx = fixture_tx();
    let txid = crate::daemon::fake::tx_id_hex(&tx);
    let (store, fake, orders) = open_reorg_with(&[(&txid, 9)]).await;
    store
        .lock()
        .void_payment(
            &shared::ids::OrderId::new(orders[0].to_string()),
            &txid,
            0,
            crate::now_unix(),
        )
        .unwrap();
    // Mined again, one block later, on the new chain.
    fake.reorg_from(9, vec![("b9", vec![]), ("b10", vec![tx])]);
    let order = orders[0].clone();
    let daemon = OnLocate {
        inner: &fake,
        hook: Box::new(|txid| {
            store
                .lock()
                .unvoid_payment(&shared::ids::OrderId::new(order.to_string()), txid, 0)
                .unwrap();
        }),
    };
    let db = Db::over_shared(store.clone());
    let chain = chain::Chain::new(
        &db,
        &daemon,
        monero::Network::Mainnet,
        20,
        crate::now_unix(),
    );
    run_job(&chain).await;
    let payment = &store
        .lock()
        .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
        .unwrap()[0];
    assert_eq!((payment.block_height, payment.voided_at), (Some(10), None));
}

/// A page with no time left re-examines one candidate and stops.
#[tokio::test]
async fn a_reorg_page_with_no_time_left_does_one_candidate() {
    let tx = fixture_tx();
    let txid = crate::daemon::fake::tx_id_hex(&tx);
    let other = "ef".repeat(32);
    let (store, fake, _) = open_reorg_with(&[(&txid, 9), (&other, 9)]).await;
    fake.reorg_from(9, vec![("b9", vec![tx]), ("b10", vec![])]);
    let db = Db::over_shared(store.clone());
    let chain = chain::Chain::new(&db, &fake, monero::Network::Mainnet, 20, crate::now_unix());
    let mut skip = HashSet::new();
    let far = tokio::time::Instant::now() + ROUND_BUDGET;
    assert!(matches!(
        chain.advance_job(10, &mut skip, far).await.unwrap(),
        Some(chain::JobStep::Collected)
    ));
    assert!(matches!(
        chain.advance_job(10, &mut skip, far).await.unwrap(),
        Some(chain::JobStep::Collected)
    ));
    let spent = tokio::time::Instant::now();
    let (processed, _, failure) = chain.process_page(10, &mut skip, spent).await.unwrap();
    assert_eq!((processed, failure.is_none()), (1, true));
    assert_eq!(
        store
            .lock()
            .reorg_work_remaining(monero::Network::Mainnet)
            .unwrap()
            .0,
        1
    );
}

// -- Block tier edge cases ----------------------------------------------------

/// A node whose answers a test can change: block hashes can fail, block
/// fetches can run a hook first or come back empty.
struct Hooked<'a> {
    inner: &'a FakeDaemonClient,
    fail_hashes: bool,
    empty_blocks: bool,
    no_bodies: bool,
    locate: LocateBehaviour,
    on_blocks: Box<dyn Fn(u64) + Send + Sync + 'a>,
}

/// How a [`Hooked`] node answers transaction lookups.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LocateBehaviour {
    Answer,
    Fail,
    /// Never answers (until the test's clock runs far past any deadline).
    Stall,
}

impl<'a> Hooked<'a> {
    fn new(inner: &'a FakeDaemonClient) -> Self {
        Self {
            inner,
            fail_hashes: false,
            empty_blocks: false,
            no_bodies: false,
            locate: LocateBehaviour::Answer,
            on_blocks: Box::new(|_| {}),
        }
    }
}

#[async_trait::async_trait]
impl MoneroDaemonClient for Hooked<'_> {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.inner.get_height().await
    }
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        if self.fail_hashes {
            return Err(DaemonError::Request("hash lookups are failing".into()));
        }
        self.inner.get_block_hash(height).await
    }
    async fn get_chain_blocks(
        &self,
        start: u64,
        count: u64,
    ) -> Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
        (self.on_blocks)(start);
        if self.empty_blocks {
            return Ok(Vec::new());
        }
        self.inner.get_chain_blocks(start, count).await
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.inner.get_mempool_txids().await
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
        if self.no_bodies {
            return Ok(Vec::new());
        }
        self.inner.get_transactions_with_ids(txids).await
    }
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        match self.locate {
            LocateBehaviour::Answer => self.inner.locate_transaction(txid).await,
            LocateBehaviour::Fail => Err(DaemonError::Request("lookups are failing".into())),
            LocateBehaviour::Stall => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                self.inner.locate_transaction(txid).await
            }
        }
    }
    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.inner.is_key_image_spent(key_images).await
    }
}

/// A network seeded at 20 (blocks 19 and 20 recorded), with `tenants` fixture
/// tenants whose cursors are at `cursor`.
async fn seeded_network(
    tenants: usize,
    cursor: u64,
) -> (
    SharedStore,
    FlakyKeyCustody,
    FakeDaemonClient,
    Vec<(crate::store::TenantId, WalletHandle)>,
    Vec<crate::store::OrderId>,
) {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let fake = FakeDaemonClient::new();
    for h in 1..=20 {
        fake.push_block(&format!("a{h}"), vec![]);
    }
    let mut handles = Vec::new();
    let mut orders = Vec::new();
    for _ in 0..tenants {
        let (tenant, handle, order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        handles.push((tenant, handle));
        orders.push(order);
    }
    let store = store.into_shared();
    let db = Db::over_shared(store.clone());
    run_round(
        &ScanState::default(),
        &inputs(&db, &custody, &fake, &handles),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    assert_eq!(
        store
            .lock()
            .max_scanned_height(monero::Network::Mainnet)
            .unwrap(),
        Some(20)
    );
    store
        .lock()
        .execute_raw_for_test(&format!(
            "UPDATE tenants SET scanned_through_height = {cursor}"
        ))
        .unwrap();
    (store, custody, fake, handles, orders)
}

/// A payment further into a block than one key-custody call covers is found
/// and recorded once, whether the block is scanned in one go or a unit at a
/// time.
#[tokio::test]
async fn a_payment_deep_in_a_big_block_is_found_in_one_go_and_a_unit_at_a_time() {
    for budget in [ROUND_BUDGET, Duration::ZERO] {
        let (store, custody, fake, tenants, orders) = seeded_network(1, 20).await;
        let mut txs: Vec<Transaction> = (0..blocks::TXS_PER_SCAN as u8 + 3)
            .map(|i| unrelated_tx(100 + i))
            .collect();
        txs.push(fixture_tx());
        txs.push(unrelated_tx(99));
        fake.push_block("big", txs);
        let state = ScanState::default();
        let db = Db::over_shared(store.clone());
        let mut rounds = 0;
        while cursor_of(&store, tenants[0].0.as_str()) != Some(21) {
            rounds += 1;
            assert!(rounds < 10, "never finished the block");
            run_round(&state, &inputs(&db, &custody, &fake, &tenants), budget)
                .await
                .into_result()
                .unwrap();
        }
        if budget == Duration::ZERO {
            assert!(rounds > 1, "the block really was split across rounds");
        }
        let payments = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
            .unwrap();
        assert_eq!(payments.len(), 1);
        assert_eq!(
            payments[0].txid,
            crate::daemon::fake::tx_id_hex(&fixture_tx())
        );
        assert_eq!(payments[0].block_height, Some(21));
    }
}

/// Anyone who knows one of a store's addresses can send it an output whose
/// amount can't be read. That output is not a payment, and the store's scan
/// carries on past its block: a real payment mined later is still found.
#[tokio::test(start_paused = true)]
async fn a_payment_whose_amount_cannot_be_read_does_not_stop_the_store_at_its_block() {
    let (store, custody, fake, tenants, orders) = seeded_network(1, 20).await;
    // The fixture payment with its output's commitment swapped for another.
    let mut unreadable = fixture_tx();
    let rct = unreadable.rct_signatures.sig.as_mut().unwrap();
    rct.out_pk[1] = rct.out_pk[0];
    fake.push_block("unreadable", vec![unrelated_tx(1), unreadable]);
    fake.push_block("paid", vec![fixture_tx()]);
    let state = ScanState::default();
    let db = Db::over_shared(store.clone());

    for _ in 0..3 {
        run_round(
            &state,
            &inputs(&db, &custody, &fake, &tenants),
            ROUND_BUDGET,
        )
        .await
        .into_result()
        .unwrap();
        tokio::time::advance(Duration::from_secs(120)).await;
    }

    assert_eq!(cursor_of(&store, tenants[0].0.as_str()), Some(22));
    let payments = store
        .lock()
        .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
        .unwrap();
    assert_eq!(payments.len(), 1, "only the payment that can be read");
    assert_eq!(
        payments[0].txid,
        crate::daemon::fake::tx_id_hex(&fixture_tx())
    );
    assert_eq!(payments[0].block_height, Some(22));
}

/// A checkpoint for a block the node has since replaced is stale: the
/// replacement is scanned from its start, the stale block's staged matches
/// are dropped, and the payment is recorded once, from the block that is
/// on the chain.
#[tokio::test]
async fn a_checkpoint_for_a_replaced_block_is_dropped_and_the_replacement_scanned_whole() {
    let (store, custody, fake, tenants, orders) = seeded_network(1, 20).await;
    let tenant = tenants[0].0.clone();
    let mut txs = vec![fixture_tx()];
    txs.extend((0..blocks::TXS_PER_SCAN as u8 + 8).map(|i| unrelated_tx(100 + i)));
    fake.push_block("big", txs);
    let state = ScanState::default();
    let db = Db::over_shared(store.clone());
    run_round(
        &state,
        &inputs(&db, &custody, &fake, &tenants),
        Duration::ZERO,
    )
    .await
    .into_result()
    .unwrap();
    let stale = store
        .lock()
        .block_checkpoint(
            monero::Network::Mainnet,
            &shared::ids::TenantId::new(tenant.to_string()),
        )
        .unwrap()
        .expect("checkpointed partway");
    assert_eq!(stale.hash, "big");

    // Replaced before it committed: the payment is now further in.
    let mut replacement: Vec<Transaction> = (0..5u8).map(|i| unrelated_tx(150 + i)).collect();
    replacement.push(fixture_tx());
    fake.reorg_from(21, vec![("big2", replacement)]);
    for _ in 0..3 {
        run_round(
            &state,
            &inputs(&db, &custody, &fake, &tenants),
            ROUND_BUDGET,
        )
        .await
        .into_result()
        .unwrap();
    }
    assert_eq!(cursor_of(&store, tenant.as_str()), Some(21));
    let payments = store
        .lock()
        .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
        .unwrap();
    assert_eq!(payments.len(), 1, "once, not once per version of the block");
    assert_eq!(payments[0].block_height, Some(21));
    assert_eq!(
        store
            .lock()
            .get_scanned_block_hash(monero::Network::Mainnet, 21)
            .unwrap()
            .as_deref(),
        Some("big2")
    );
    assert_eq!(
        store
            .lock()
            .block_checkpoint(
                monero::Network::Mainnet,
                &shared::ids::TenantId::new(tenant.to_string())
            )
            .unwrap(),
        None
    );
}

/// More stores in a group than one scan batch, with no time: the first
/// batch is checkpointed, the rest haven't started. A store that failed is
/// left out of the checkpoint; next round it starts from the beginning while
/// the others resume, and every store ends with its payment exactly once.
#[tokio::test]
async fn a_big_group_resumes_each_store_from_its_own_place() {
    let (_guard, logs) = crate::test_log::capture();
    let count = crate::scanner::SCAN_CONCURRENCY + 1;
    let (store, custody, fake, tenants, orders) = seeded_network(count, 20).await;
    // The first store in id order: a group is scanned in that order, so it
    // is always started, even in a round with no time to spare. (Ids are
    // random, so `tenants[0]` could be last and never reached.)
    let (failing_id, failing) = tenants
        .iter()
        .min_by(|a, b| a.0.cmp(&b.0))
        .cloned()
        .unwrap();
    custody.fail(failing);
    let mut txs = vec![fixture_tx()];
    txs.extend((0..4u8).map(|i| unrelated_tx(100 + i)));
    fake.push_block("big", txs);
    let state = ScanState::default();
    let db = Db::over_shared(store.clone());
    run_round(
        &state,
        &inputs(&db, &custody, &fake, &tenants),
        Duration::ZERO,
    )
    .await
    .into_result()
    .unwrap();
    let checkpointed = tenants
        .iter()
        .filter(|(id, _)| {
            store
                .lock()
                .block_checkpoint(
                    monero::Network::Mainnet,
                    &shared::ids::TenantId::new(id.to_string()),
                )
                .unwrap()
                .is_some()
        })
        .count();
    assert!(
        checkpointed > 0 && checkpointed < count,
        "{checkpointed} of {count} got anywhere"
    );
    assert_eq!(
        store
            .lock()
            .block_checkpoint(monero::Network::Mainnet, &failing_id)
            .unwrap(),
        None,
        "the failed store has no checkpoint"
    );
    assert!(
        logs.count("scanning a block failed for this store") >= 1,
        "{}",
        logs.text()
    );

    custody.recover(failing);
    for _ in 0..6 {
        run_round(
            &state,
            &inputs(&db, &custody, &fake, &tenants),
            ROUND_BUDGET,
        )
        .await
        .into_result()
        .unwrap();
    }
    for ((tenant, _), order) in tenants.iter().zip(&orders) {
        assert_eq!(cursor_of(&store, tenant.as_str()), Some(21));
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(order.to_string()))
                .unwrap()
                .len(),
            1
        );
    }
}

/// Catching up onto a recorded block the node no longer has (a fork the
/// chain tier couldn't look at this round): the group stays where it is.
#[tokio::test]
async fn catching_up_onto_a_replaced_recorded_block_waits() {
    let (store, custody, fake, tenants, _) = seeded_network(1, 18).await;
    fake.reorg_from(19, vec![("b19", vec![]), ("b20", vec![])]);
    let mut daemon = Hooked::new(&fake);
    daemon.fail_hashes = true;
    let db = Db::over_shared(store.clone());
    let report = run_round(
        &ScanState::default(),
        &inputs(&db, &custody, &daemon, &tenants),
        ROUND_BUDGET,
    )
    .await;
    assert!(report.error.is_none(), "{:?}", report.error);
    assert_eq!(
        cursor_of(&store, tenants[0].0.as_str()),
        Some(18),
        "no block recorded against the old chain"
    );
    assert_eq!(
        store
            .lock()
            .get_scanned_block_hash(monero::Network::Mainnet, 19)
            .unwrap()
            .as_deref(),
        Some("a19")
    );
}

/// A rewind (or anything else) that changes the recorded chain while a
/// block is being scanned stops the commit: nothing is written for it.
#[tokio::test]
async fn a_recorded_chain_changed_mid_scan_stops_the_commit() {
    // The recorded block itself changes, then (next case) its parent.
    for (cursor, changed) in [(18u64, 19u64), (19, 19)] {
        let (store, custody, fake, tenants, _) = seeded_network(1, cursor).await;
        let block = cursor + 1;
        fake.seed_block_at(block, &format!("a{block}"), vec![unrelated_tx(1)]);
        let hook_store = store.clone();
        custody.on_next_scan(move || {
            hook_store
                .lock()
                .execute_raw_for_test(&format!(
                    "UPDATE scanned_blocks SET block_hash = 'zzz' WHERE height = {changed}"
                ))
                .unwrap();
        });
        let db = Db::over_shared(store.clone());
        run_round(
            &ScanState::default(),
            &inputs(&db, &custody, &fake, &tenants),
            ROUND_BUDGET,
        )
        .await
        .into_result()
        .unwrap();
        assert_eq!(
            store
                .lock()
                .get_scanned_block_hash(monero::Network::Mainnet, changed)
                .unwrap()
                .as_deref(),
            Some("zzz"),
            "the hook ran"
        );
        assert_eq!(
            cursor_of(&store, tenants[0].0.as_str()),
            Some(cursor),
            "cursor {cursor}: nothing committed"
        );
    }
}

/// A store catching up from below the recorded history (older than the
/// retained window) scans those blocks without recording them for the
/// network: the network's record only grows at its tip.
#[tokio::test]
async fn catching_up_below_the_recorded_history_records_nothing_for_the_network() {
    let (store, custody, fake, tenants, orders) = seeded_network(1, 10).await;
    fake.seed_block_at(12, "a12", vec![fixture_tx()]);
    let db = Db::over_shared(store.clone());
    for _ in 0..3 {
        run_round(
            &ScanState::default(),
            &inputs(&db, &custody, &fake, &tenants),
            ROUND_BUDGET,
        )
        .await
        .into_result()
        .unwrap();
    }
    assert_eq!(cursor_of(&store, tenants[0].0.as_str()), Some(20));
    assert_eq!(
        store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
            .unwrap()[0]
            .block_height,
        Some(12)
    );
    assert_eq!(
        store
            .lock()
            .get_scanned_block_hash(monero::Network::Mainnet, 12)
            .unwrap(),
        None,
        "only the tip grows the record"
    );
}

/// A node that answers a block fetch with nothing is a node failure: the
/// tier waits (and says so), and nothing is recorded.
#[tokio::test]
async fn a_node_that_returns_no_block_is_waited_out() {
    let (_guard, logs) = crate::test_log::capture();
    let (store, custody, fake, tenants, _) = seeded_network(1, 20).await;
    fake.push_block("a21", vec![]);
    let mut daemon = Hooked::new(&fake);
    daemon.empty_blocks = true;
    let db = Db::over_shared(store.clone());
    let report = run_round(
        &ScanState::default(),
        &inputs(&db, &custody, &daemon, &tenants),
        ROUND_BUDGET,
    )
    .await;
    assert_eq!(
        report.outcome(Tier::Blocks),
        TierOutcome::Blocked(Wait::NodeFailed)
    );
    assert!(report.error.is_none());
    assert_eq!(cursor_of(&store, tenants[0].0.as_str()), Some(20));
    assert_eq!(
        logs.count("block scanning stopped: the node failed"),
        1,
        "{}",
        logs.text()
    );
}

/// A store catching up whose every order closed long before the blocks it
/// is behind on has nothing to find in them: it moves straight on, rather
/// than waiting at the first of them (and fetching it) round after round.
#[tokio::test]
async fn a_store_whose_orders_all_closed_before_the_gap_moves_straight_on() {
    let (store, custody, fake, tenants, orders) = seeded_network(1, 10).await;
    store
        .lock()
        .execute_raw_for_test(&format!(
            "UPDATE orders SET status = 'expired', closed_at_utc = 1 WHERE id = '{}'",
            orders[0]
        ))
        .unwrap();
    let db = Db::over_shared(store.clone());
    run_round(
        &ScanState::default(),
        &inputs(&db, &custody, &fake, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    assert_eq!(cursor_of(&store, tenants[0].0.as_str()), Some(20));
}

// -- Mempool tier edge cases --------------------------------------------------

/// Transactions that left the pool between the listing and the body fetch
/// (the node returns none of them) are moved past, not asked for again and
/// again.
#[tokio::test]
async fn pool_transactions_gone_before_their_bodies_came_are_moved_past() {
    let (store, custody, fake, tenants, orders) = seeded_network(1, 20).await;
    fake.set_mempool(vec![fixture_tx()]);
    let mut daemon = Hooked::new(&fake);
    daemon.no_bodies = true;
    let db = Db::over_shared(store.clone());
    let report = run_round(
        &ScanState::default(),
        &inputs(&db, &custody, &daemon, &tenants),
        ROUND_BUDGET,
    )
    .await;
    assert!(report.error.is_none(), "{:?}", report.error);
    assert!(store
        .lock()
        .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
        .unwrap()
        .is_empty());
}

/// A fast pass that can't load the scan windows (a storage failure) scans
/// nothing, says so, and the next pass tries again.
#[tokio::test]
async fn a_fast_pass_that_cannot_load_windows_tries_again_next_pass() {
    let (_guard, logs) = crate::test_log::capture();
    let (store, custody, fake, tenants, orders) = seeded_network(1, 20).await;
    fake.set_mempool(vec![fixture_tx()]);
    let state = ScanState::default();
    let db = Db::over_shared(store.clone());
    store.lock().fail_nth_access(Some(0));
    let report = fast_pass(&state, &inputs(&db, &custody, &fake, &tenants))
        .await
        .unwrap();
    store.lock().fail_nth_access(None);
    assert_eq!(report, FastReport::default());
    assert_eq!(
        logs.count("loading scan windows for the mempool failed"),
        1,
        "{}",
        logs.text()
    );
    let report = fast_pass(&state, &inputs(&db, &custody, &fake, &tenants))
        .await
        .unwrap();
    assert_eq!(report.paid_orders, 1);
    assert_eq!(
        store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
            .unwrap()
            .len(),
        1
    );
}

/// With no store to scan for, a fast pass does nothing (and costs no scan).
#[tokio::test]
async fn a_fast_pass_with_no_store_in_scope_does_nothing() {
    let store = Store::open_in_memory().unwrap().into_shared();
    let custody = FlakyKeyCustody::default();
    let fake = FakeDaemonClient::new();
    fake.set_mempool(vec![fixture_tx()]);
    let db = Db::over_shared(store.clone());
    assert_eq!(
        fast_pass(&ScanState::default(), &inputs(&db, &custody, &fake, &[])).await,
        Some(FastReport::default())
    );
}

/// After a round, the fast path settles against the round's chain height
/// rather than asking the node again.
#[tokio::test]
async fn the_fast_path_uses_the_last_rounds_chain_height() {
    let (store, custody, fake, tenants, orders) = seeded_network(1, 20).await;
    let state = ScanState::default();
    let db = Db::over_shared(store.clone());
    run_round(
        &state,
        &inputs(&db, &custody, &fake, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    fake.set_mempool(vec![fixture_tx()]);
    fake.set_online(false);
    // The pool is read through a node that answers only for the pool.
    let pool_only = PoolOnly(&fake);
    let report = fast_pass(&state, &inputs(&db, &custody, &pool_only, &tenants))
        .await
        .unwrap();
    assert_eq!(report.paid_orders, 1);
    assert_eq!(
        order_status(&store, &orders[0]),
        OrderStatus::Unconfirmed,
        "recomputed with the round's height"
    );
}

/// A node that serves the pool while everything else about it fails.
struct PoolOnly<'a>(&'a FakeDaemonClient);

#[async_trait::async_trait]
impl MoneroDaemonClient for PoolOnly<'_> {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        Err(DaemonError::Request("down".into()))
    }
    async fn get_block_hash(&self, _: u64) -> Result<String, DaemonError> {
        Err(DaemonError::Request("down".into()))
    }
    async fn get_chain_blocks(
        &self,
        _start_height: u64,
        _count: u64,
    ) -> Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
        Err(DaemonError::Request("down".into()))
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.0.set_online(true);
        let pool = self.0.get_mempool_txids().await;
        self.0.set_online(false);
        pool
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
        self.0.set_online(true);
        let txs = self.0.get_transactions_with_ids(txids).await;
        self.0.set_online(false);
        txs
    }
    async fn locate_transaction(&self, _: &str) -> Result<TxLocation, DaemonError> {
        Err(DaemonError::Request("down".into()))
    }
    async fn is_key_image_spent(&self, _: &[String]) -> Result<Vec<KeyImageStatus>, DaemonError> {
        Err(DaemonError::Request("down".into()))
    }
}

/// A store whose scan fails is tried once per pass, not once per
/// transaction, and the failure is reported; after repeated failures it
/// waits out a delay and the pool isn't scanned for it meanwhile.
#[tokio::test]
async fn a_failing_store_is_tried_once_per_pass_then_waits() {
    let (_guard, logs) = crate::test_log::capture();
    let (store, custody, fake, tenants, _) = seeded_network(1, 20).await;
    let handle = tenants[0].1;
    custody.fail(handle);
    fake.set_mempool(vec![unrelated_tx(1), unrelated_tx(2)]);
    let state = ScanState::default();
    let db = Db::over_shared(store.clone());
    let report = fast_pass(&state, &inputs(&db, &custody, &fake, &tenants))
        .await
        .unwrap();
    assert_eq!((report.scanned, report.paid_orders), (2, 0));
    assert_eq!(
        custody.attempts.lock().get(&handle).copied(),
        Some(1),
        "once for the pass"
    );
    assert_eq!(
        logs.count("scanning a mempool transaction failed"),
        1,
        "{}",
        logs.text()
    );

    // Past its free retries, the store waits: the rotation doesn't scan the
    // pool for it.
    state
        .backoff
        .failed(&shared::ids::TenantId::new(tenants[0].0.to_string()));
    state
        .backoff
        .failed(&shared::ids::TenantId::new(tenants[0].0.to_string()));
    let before = custody.attempts.lock().get(&handle).copied();
    run_round(
        &state,
        &inputs(&db, &custody, &fake, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    assert_eq!(custody.attempts.lock().get(&handle).copied(), before);
}

/// More stores than one page: the rotation's pages wrap round and the fast
/// path loads every page, so every store's payment is found.
#[tokio::test]
async fn every_store_is_scanned_when_there_are_more_than_a_page() {
    let count = 257;
    let (store, custody, fake, tenants, orders) = seeded_network(count, 20).await;
    fake.set_mempool(vec![fixture_tx()]);
    let db = Db::over_shared(store.clone());

    // The fast path, in one pass.
    let report = fast_pass(
        &ScanState::default(),
        &inputs(&db, &custody, &fake, &tenants),
    )
    .await
    .unwrap();
    assert_eq!(report.paid_orders, count);
    store
        .lock()
        .execute_raw_for_test("DELETE FROM order_payments")
        .unwrap();

    // The rotation, a slice a round.
    let state = ScanState::default();
    for _ in 0..12 {
        run_round(
            &state,
            &inputs(&db, &custody, &fake, &tenants),
            ROUND_BUDGET,
        )
        .await
        .into_result()
        .unwrap();
    }
    for order in &orders {
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(order.to_string()))
                .unwrap()
                .len(),
            1,
            "order {order}"
        );
    }
}

#[tokio::test]
async fn every_sql_failure_while_a_double_spent_payment_is_in_the_pool_is_recovered_from() {
    sweep_step(DOUBLE_SPENT, 1, VOIDED_ENDING).await;
}

#[tokio::test]
async fn every_sql_failure_while_a_double_spend_is_found_is_recovered_from() {
    sweep_step(DOUBLE_SPENT, 2, VOIDED_ENDING).await;
}

// -- Settlement tier edge cases -----------------------------------------------

/// Records an unconfirmed payment with this txid on the order.
fn unconfirmed(store: &SharedStore, order: &crate::store::OrderId, txid: &str) {
    store
        .lock()
        .record_payment_match(order, txid, 0, 1, "[\"ki\"]", crate::now_unix(), None)
        .unwrap();
}

/// A node that fails or hangs while a vanished payment is checked is waited
/// out: the round isn't failed, the payment is untouched, and the rotation
/// doesn't move past it.
#[tokio::test(start_paused = true)]
async fn a_vanished_check_the_node_fails_or_stalls_is_retried() {
    for stall in [false, true] {
        let (_guard, logs) = crate::test_log::capture();
        let (store, custody, fake, tenants, orders) = seeded_network(1, 20).await;
        unconfirmed(&store, &orders[0], &"ab".repeat(32));
        let mut daemon = Hooked::new(&fake);
        daemon.locate = if stall {
            LocateBehaviour::Stall
        } else {
            LocateBehaviour::Fail
        };
        let position = || {
            store
                .lock()
                .scheduler_position::<crate::store::position::VanishedPayments>(
                    monero::Network::Mainnet,
                )
                .unwrap()
        };
        let before = position();
        let db = Db::over_shared(store.clone());
        let report = run_round(
            &ScanState::default(),
            &inputs(&db, &custody, &daemon, &tenants),
            ROUND_BUDGET,
        )
        .await;
        assert!(report.error.is_none(), "{:?}", report.error);
        let message = if stall {
            "took too long"
        } else {
            "checking a vanished mempool payment failed"
        };
        assert_eq!(logs.count(message), 1, "{}", logs.text());
        let payment = &store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
            .unwrap()[0];
        assert_eq!((payment.block_height, payment.voided_at), (None, None));
        assert_eq!(position(), before, "the rotation waits at it");
    }
}

/// With no time to spare, one vanished payment is checked a round, and the
/// rotation moves on to the next.
#[tokio::test]
async fn vanished_payments_are_checked_one_a_round_with_no_time_to_spare() {
    let (store, custody, fake, tenants, orders) = seeded_network(1, 20).await;
    unconfirmed(&store, &orders[0], &"ab".repeat(32));
    unconfirmed(&store, &orders[0], &"cd".repeat(32));
    let ids: Vec<i64> = store
        .lock()
        .unconfirmed_payments_page(monero::Network::Mainnet, 0, 10)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let db = Db::over_shared(store.clone());
    let state = ScanState::default();
    let position = || {
        store
            .lock()
            .scheduler_position::<crate::store::position::VanishedPayments>(
                monero::Network::Mainnet,
            )
            .unwrap()
    };
    run_round(
        &state,
        &inputs(&db, &custody, &fake, &tenants),
        Duration::ZERO,
    )
    .await
    .into_result()
    .unwrap();
    assert_eq!(position(), Some(ids[0]));
    run_round(
        &state,
        &inputs(&db, &custody, &fake, &tenants),
        Duration::ZERO,
    )
    .await
    .into_result()
    .unwrap();
    assert_eq!(position(), Some(ids[1]));
}

/// A payment seen in the pool and mined before the next pool snapshot is
/// given its height by the vanished check, without waiting for the block
/// scan; one back in the node's own pool is left alone.
#[tokio::test]
async fn a_vanished_payment_found_mined_gets_its_height_and_one_back_in_the_pool_is_left() {
    let (store, _, fake, _, orders) = seeded_network(2, 20).await;
    let mined = fixture_tx();
    let pooled = unrelated_tx(9);
    let (mined_id, pooled_id) = (
        crate::daemon::fake::tx_id_hex(&mined),
        crate::daemon::fake::tx_id_hex(&pooled),
    );
    unconfirmed(&store, &orders[0], &mined_id);
    unconfirmed(&store, &orders[1], &pooled_id);
    let height = fake.push_block("m", vec![mined]);
    fake.set_mempool(vec![pooled]);
    let candidates: Vec<_> = store
        .lock()
        .unconfirmed_payments_page(monero::Network::Mainnet, 0, 10)
        .unwrap()
        .into_iter()
        .map(|(_, p)| p)
        .collect();
    let db = Db::over_shared(store.clone());
    // The snapshot was taken before either moved: neither is in it.
    let report = crate::scanner::check_vanished_candidates(
        &db,
        &fake,
        &HashSet::new(),
        height,
        crate::now_unix(),
        candidates,
        &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(report.dirty_orders, vec![orders[0].clone()]);
    assert!(report.double_spent_orders.is_empty());
    assert_eq!(
        store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
            .unwrap()[0]
            .block_height,
        Some(height as i64)
    );
    assert_eq!(
        store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(orders[1].to_string()))
            .unwrap()[0]
            .block_height,
        None
    );
}

// -- Upkeep tier edge cases ---------------------------------------------------

/// `count` voided payments on `order`, each with a well-formed key image the
/// fake node calls unspent: every one a false accusation the recheck
/// restores.
fn voided_payments(store: &SharedStore, order: &crate::store::OrderId, count: u8) {
    let s = store.lock();
    let now = crate::now_unix();
    for i in 0..count {
        let txid = format!("{i:02x}").repeat(32);
        let image = format!("{:02x}", 0x80 + i).repeat(32);
        s.record_payment_match(
            &shared::ids::OrderId::new(order.to_string()),
            &txid,
            0,
            1,
            &format!("[\"{image}\"]"),
            now,
            Some(15),
        )
        .unwrap();
        s.void_payment(&shared::ids::OrderId::new(order.to_string()), &txid, 0, now)
            .unwrap();
    }
    s.mark_double_spend_detected(&shared::ids::OrderId::new(order.to_string()), now)
        .unwrap();
}

/// Makes a void recheck pass due now.
fn void_recheck_due(store: &SharedStore) {
    let s = store.lock();
    s.set_scheduler_position::<crate::store::position::VoidRecheckPassStarted>(
        monero::Network::Mainnet,
        &i64::MIN,
    )
    .unwrap();
    s.set_scheduler_position::<crate::store::position::VoidRecheck>(monero::Network::Mainnet, &0)
        .unwrap();
}

/// More recent voids than a page, and no time to spare: each round rechecks
/// one, the pass carries on from where it stopped, and ends once the last is
/// done.
#[tokio::test]
async fn a_void_recheck_pass_longer_than_a_page_carries_on_across_rounds() {
    let (store, custody, fake, tenants, orders) = seeded_network(1, 20).await;
    voided_payments(&store, &orders[0], 18);
    void_recheck_due(&store);
    let db = Db::over_shared(store.clone());
    let state = ScanState::default();
    let voided = || {
        store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
            .unwrap()
            .iter()
            .filter(|p| p.voided_at.is_some())
            .count()
    };
    run_round(
        &state,
        &inputs(&db, &custody, &fake, &tenants),
        Duration::ZERO,
    )
    .await
    .into_result()
    .unwrap();
    assert_eq!(voided(), 17, "one a round with no time to spare");
    let position = || {
        store
            .lock()
            .scheduler_position::<crate::store::position::VoidRecheck>(monero::Network::Mainnet)
            .unwrap()
    };
    assert_ne!(position(), Some(0), "the pass is still going");
    for _ in 0..17 {
        run_round(
            &state,
            &inputs(&db, &custody, &fake, &tenants),
            Duration::ZERO,
        )
        .await
        .into_result()
        .unwrap();
    }
    assert_eq!(voided(), 0);
    assert_eq!(position(), Some(0), "and the pass is over");
}

/// Every SQL statement of a void recheck failed in turn: the void is
/// restored once, whichever failed, and the merchant told once.
#[tokio::test]
async fn every_sql_failure_in_a_void_recheck_is_recovered_from() {
    let (_logs, _) = crate::test_log::capture();
    let mut faults = 0;
    for fault in 0.. {
        let (store, custody, fake, tenants, orders) = seeded_network(1, 20).await;
        store
            .lock()
            .create_webhook(
                &shared::ids::TenantId::new(tenants[0].0.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();
        voided_payments(&store, &orders[0], 1);
        void_recheck_due(&store);
        let db = Db::over_shared(store.clone());
        let state = ScanState::default();
        let seen = store.lock().fail_nth_access(Some(fault));
        run_round(
            &state,
            &inputs(&db, &custody, &fake, &tenants),
            ROUND_BUDGET,
        )
        .await;
        store.lock().fail_nth_access(None);
        if seen.load(Ordering::Relaxed) <= fault {
            break;
        }
        faults += 1;
        void_recheck_due(&store);
        run_round(
            &state,
            &inputs(&db, &custody, &fake, &tenants),
            ROUND_BUDGET,
        )
        .await
        .into_result()
        .unwrap();
        let payment = &store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
            .unwrap()[0];
        assert_eq!(payment.voided_at, None, "fault {fault}");
        let reversed = store
            .lock()
            .due_webhook_deliveries(i64::MAX / 2, 100)
            .unwrap()
            .into_iter()
            .filter(|d| d.event_type == "order.double_spend_reversed")
            .count();
        assert_eq!(reversed, 1, "fault {fault}: told {reversed} times");
    }
    assert!(faults > 10, "reached {faults}");
}

// -- More edge cases: settlement pages, block commits, job failures -----------

/// More orders owed a recompute than a page: the settlement tier works
/// through them page by page, wrapping round, until none is owed.
#[tokio::test]
async fn more_recomputes_owed_than_a_page_are_all_done() {
    let (store, custody, fake, tenants, _) = seeded_network(1, 20).await;
    let tenant = tenants[0].0.clone();
    let now = crate::now_unix();
    let mut orders = Vec::new();
    {
        let s = store.lock();
        for i in 0..70 {
            let index = s
                .allocate_minor_index(&shared::ids::TenantId::new(tenant.to_string()))
                .unwrap();
            let order = s
                .create_order(crate::store::NewOrder {
                    confirmations_required_override: None,
                    tenant_id: tenant.clone(),
                    merchant_order_id: None,
                    minor_index: index,
                    address: format!("addr{i}"),
                    xmr_amount_piconero: 1,
                    description: None,
                    created_at: now,
                    expires_at: now + 3600,
                })
                .unwrap();
            s.record_payment_match(&order.id, &format!("{i:064x}"), 0, 1, "[]", now, None)
                .unwrap();
            orders.push(order.id);
        }
    }
    let db = Db::over_shared(store.clone());
    let state = ScanState::default();
    for _ in 0..3 {
        run_round(
            &state,
            &inputs(&db, &custody, &fake, &tenants),
            Duration::ZERO,
        )
        .await
        .into_result()
        .unwrap();
    }
    for order in &orders {
        assert_eq!(
            order_status(
                &store,
                &shared::ids::OrderId::new(order.as_str().to_string())
            ),
            OrderStatus::Unconfirmed,
            "{order}"
        );
    }
}

/// A store whose scan fails partway through a block is left behind for that
/// block (and caught up later); the others commit it, and it isn't asked
/// again for the block's later transactions.
#[tokio::test]
async fn a_store_failing_partway_through_a_block_is_left_behind_and_not_asked_again() {
    let (store, custody, fake, tenants, orders) = seeded_network(2, 20).await;
    let failing = tenants[1].1;
    custody.fail(failing);
    fake.push_block("b21", vec![unrelated_tx(1), fixture_tx(), unrelated_tx(2)]);
    let db = Db::over_shared(store.clone());
    let state = ScanState::default();
    run_round(
        &state,
        &inputs(&db, &custody, &fake, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    assert_eq!(cursor_of(&store, tenants[0].0.as_str()), Some(21));
    assert_eq!(
        store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        cursor_of(&store, tenants[1].0.as_str()),
        Some(20),
        "left behind"
    );
    // Once by the frontier, once more when catch-up retries it: never once
    // per transaction (three).
    assert_eq!(
        custody.attempts.lock().get(&failing).copied(),
        Some(2),
        "asked once per scan of the block, not per transaction"
    );

    custody.recover(failing);
    for _ in 0..3 {
        run_round(
            &state,
            &inputs(&db, &custody, &fake, &tenants),
            ROUND_BUDGET,
        )
        .await
        .into_result()
        .unwrap();
    }
    assert_eq!(cursor_of(&store, tenants[1].0.as_str()), Some(21));
    assert_eq!(
        store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(orders[1].to_string()))
            .unwrap()
            .len(),
        1,
        "and caught up"
    );
}

/// A store whose cursor is moved while its block is being scanned (a
/// rescan, a rewind) gets nothing recorded from that scan: its cursor stays
/// where it was put.
#[tokio::test]
async fn a_cursor_moved_mid_scan_keeps_what_moved_it() {
    let (store, custody, fake, tenants, orders) = seeded_network(1, 20).await;
    fake.push_block("b21", vec![fixture_tx()]);
    let hook_store = store.clone();
    let tenant = tenants[0].0.clone();
    custody.on_next_scan(move || {
        hook_store
            .lock()
            .execute_raw_for_test(&format!(
                "UPDATE tenants SET scanned_through_height = 19 WHERE id = '{tenant}'"
            ))
            .unwrap();
    });
    let db = Db::over_shared(store.clone());
    run_round(
        &ScanState::default(),
        &inputs(&db, &custody, &fake, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    // The moved cursor was caught up again from 19, and the payment found
    // then, once.
    assert_eq!(
        store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(cursor_of(&store, tenants[0].0.as_str()), Some(21));
}

/// Every SQL statement of the round that commits a checkpointed block,
/// failed in turn: the payment is recorded once, at the block's height.
#[tokio::test]
async fn every_sql_failure_committing_a_checkpointed_block_is_recovered_from() {
    let (_logs, _) = crate::test_log::capture();
    let mut faults = 0;
    for fault in 0.. {
        let (store, custody, fake, tenants, orders) = seeded_network(1, 20).await;
        // One transaction more than a unit scans before it looks at the clock.
        let mut txs = vec![fixture_tx()];
        txs.extend((0..blocks::TXS_PER_SCAN as u8).map(|i| unrelated_tx(100 + i)));
        fake.push_block("big", txs);
        let db = Db::over_shared(store.clone());
        let state = ScanState::default();
        run_round(
            &state,
            &inputs(&db, &custody, &fake, &tenants),
            Duration::ZERO,
        )
        .await
        .into_result()
        .unwrap();
        assert!(store
            .lock()
            .block_checkpoint(
                monero::Network::Mainnet,
                &shared::ids::TenantId::new(tenants[0].0.to_string())
            )
            .unwrap()
            .is_some());
        let seen = store.lock().fail_nth_access(Some(fault));
        run_round(
            &state,
            &inputs(&db, &custody, &fake, &tenants),
            ROUND_BUDGET,
        )
        .await;
        store.lock().fail_nth_access(None);
        if seen.load(Ordering::Relaxed) <= fault {
            break;
        }
        faults += 1;
        for _ in 0..3 {
            run_round(
                &state,
                &inputs(&db, &custody, &fake, &tenants),
                ROUND_BUDGET,
            )
            .await
            .into_result()
            .unwrap();
        }
        let payments = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
            .unwrap();
        assert_eq!(payments.len(), 1, "fault {fault}");
        assert_eq!(payments[0].block_height, Some(21), "fault {fault}");
        assert_eq!(
            cursor_of(&store, tenants[0].0.as_str()),
            Some(21),
            "fault {fault}"
        );
    }
    assert!(faults > 10, "reached {faults}");
}

/// Every SQL statement of a round working an open reorg job, failed in
/// turn: the job still finishes, with the payment where the node has it.
#[tokio::test]
async fn every_sql_failure_working_a_reorg_job_is_recovered_from() {
    let (_logs, _) = crate::test_log::capture();
    let tx = fixture_tx();
    let txid = crate::daemon::fake::tx_id_hex(&tx);
    let mut faults = 0;
    for fault in 0.. {
        let (store, fake, orders) = open_reorg_with(&[(&txid, 9)]).await;
        fake.reorg_from(9, vec![("b9", vec![]), ("b10", vec![tx.clone()])]);
        let custody = FlakyKeyCustody::default();
        let db = Db::over_shared(store.clone());
        let state = ScanState::default();
        let seen = store.lock().fail_nth_access(Some(fault));
        run_round(&state, &inputs(&db, &custody, &fake, &[]), ROUND_BUDGET).await;
        store.lock().fail_nth_access(None);
        if seen.load(Ordering::Relaxed) <= fault {
            break;
        }
        faults += 1;
        for _ in 0..4 {
            run_round(&state, &inputs(&db, &custody, &fake, &[]), ROUND_BUDGET).await;
        }
        assert!(
            store
                .lock()
                .reorg_job(monero::Network::Mainnet)
                .unwrap()
                .is_none(),
            "fault {fault}: the job finished"
        );
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
                .unwrap()[0]
                .block_height,
            Some(10),
            "fault {fault}"
        );
    }
    assert!(faults > 5, "reached {faults}");
}

/// An open job whose every candidate is waiting out a retry delay: the
/// chain tier waits, and says why, without failing the round.
#[tokio::test]
async fn a_reorg_job_with_every_candidate_waiting_waits() {
    let stuck = "ab".repeat(32);
    let (store, fake, orders) = open_reorg_with(&[(&stuck, 9)]).await;
    let db = Db::over_shared(store.clone());
    while store
        .lock()
        .collect_reorg_candidates(monero::Network::Mainnet, 64, crate::now_unix())
        .unwrap()
        != crate::store::ReorgPhase::Process
    {}
    // Now in its processing phase: put the candidate well into the future.
    let id = store
        .lock()
        .get_all_payments(&shared::ids::OrderId::new(orders[0].to_string()))
        .unwrap()[0]
        .id;
    for _ in 0..6 {
        store
            .lock()
            .defer_reorg_candidate(monero::Network::Mainnet, id, crate::now_unix())
            .unwrap();
    }
    let custody = FlakyKeyCustody::default();
    let report = run_round(
        &ScanState::default(),
        &inputs(&db, &custody, &fake, &[]),
        ROUND_BUDGET,
    )
    .await;
    assert_eq!(
        report.outcome(Tier::Chain),
        TierOutcome::Blocked(Wait::ReorgCandidatesRetrying),
        "{report:?}"
    );
    assert!(report.error.is_none());
}

/// A page of recomputes mixes what is owed and what is due, and never
/// holds more than a page.
#[tokio::test]
async fn a_recompute_page_fills_with_due_orders_up_to_its_size() {
    let (store, custody, fake, tenants, _) = seeded_network(1, 20).await;
    let tenant = tenants[0].0.clone();
    let now = crate::now_unix();
    {
        let s = store.lock();
        for i in 0..200 {
            let index = s
                .allocate_minor_index(&shared::ids::TenantId::new(tenant.to_string()))
                .unwrap();
            let order = s
                .create_order(crate::store::NewOrder {
                    confirmations_required_override: None,
                    tenant_id: tenant.clone(),
                    merchant_order_id: None,
                    minor_index: index,
                    address: format!("due{i}"),
                    xmr_amount_piconero: 1,
                    description: None,
                    created_at: now,
                    expires_at: now + 3600,
                })
                .unwrap();
            if i < 10 {
                s.record_payment_match(&order.id, &format!("{i:064x}"), 0, 1, "[]", now, None)
                    .unwrap();
            }
        }
        s.execute_raw_for_test("UPDATE orders SET next_due_at_utc = 1")
            .unwrap();
    }
    let db = Db::over_shared(store.clone());
    let state = ScanState::default();
    run_round(
        &state,
        &inputs(&db, &custody, &fake, &tenants),
        Duration::ZERO,
    )
    .await
    .into_result()
    .unwrap();
    let still_due = store
        .lock()
        .due_order_ids(monero::Network::Mainnet, now, 20, 1000)
        .unwrap()
        .len();
    assert!(
        still_due >= 200 - 64,
        "at most a page recomputed in the unit: {still_due} left"
    );
    for _ in 0..3 {
        run_round(
            &state,
            &inputs(&db, &custody, &fake, &tenants),
            ROUND_BUDGET,
        )
        .await
        .into_result()
        .unwrap();
    }
    assert_eq!(
        store
            .lock()
            .due_order_ids(monero::Network::Mainnet, now, 20, 1000)
            .unwrap()
            .len(),
        0
    );
}

// -- What a round asks of the node (docs/node_rpc_efficiency.md) --------------

/// A fake node that keeps the name of every call made to it, in order.
struct Asked<'a> {
    inner: &'a FakeDaemonClient,
    calls: parking_lot::Mutex<Vec<&'static str>>,
}

impl<'a> Asked<'a> {
    fn new(inner: &'a FakeDaemonClient) -> Self {
        Self {
            inner,
            calls: Default::default(),
        }
    }

    fn note(&self, call: &'static str) {
        self.calls.lock().push(call);
    }

    /// The calls made since the last `take`.
    fn take(&self) -> Vec<&'static str> {
        std::mem::take(&mut self.calls.lock())
    }
}

#[async_trait::async_trait]
impl MoneroDaemonClient for Asked<'_> {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.note("get_height");
        self.inner.get_height().await
    }
    async fn get_tip(&self) -> Result<crate::daemon::ChainTip, DaemonError> {
        self.note("get_tip");
        self.inner.get_tip().await
    }
    async fn get_tip_and_mempool(
        &self,
    ) -> (
        Result<crate::daemon::ChainTip, DaemonError>,
        crate::daemon::PoolAnswer,
    ) {
        self.note("get_tip_and_mempool");
        self.inner.get_tip_and_mempool().await
    }
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        self.note("get_block_hash");
        self.inner.get_block_hash(height).await
    }
    async fn get_chain_blocks(
        &self,
        start: u64,
        count: u64,
    ) -> Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
        self.note("get_chain_blocks");
        self.inner.get_chain_blocks(start, count).await
    }
    async fn get_chain_headers(
        &self,
        start: u64,
        count: u64,
    ) -> Result<Vec<crate::daemon::ChainHeader>, DaemonError> {
        self.note("get_chain_headers");
        self.inner.get_chain_headers(start, count).await
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.note("get_mempool_txids");
        self.inner.get_mempool_txids().await
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
        self.note("get_transactions_with_ids");
        self.inner.get_transactions_with_ids(txids).await
    }
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        self.note("locate_transaction");
        self.inner.locate_transaction(txid).await
    }
    async fn locate_transactions(
        &self,
        txids: &[String],
    ) -> Result<std::collections::HashMap<String, TxLocation>, DaemonError> {
        self.note("locate_transactions");
        self.inner.locate_transactions(txids).await
    }
    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.note("is_key_image_spent");
        self.inner.is_key_image_spent(key_images).await
    }
}

fn recorded_height(store: &SharedStore) -> Option<u64> {
    store
        .lock()
        .max_scanned_height(monero::Network::Mainnet)
        .unwrap()
}

/// While no store has an order in scope and no payment is unconfirmed, a
/// round asks the node one thing: its tip. The pool isn't polled, no block
/// hash is looked up (the tip's id came with its height), and a new block is
/// recorded from its header, with no transactions fetched. The fast mempool
/// path asks nothing at all.
#[tokio::test]
async fn a_network_with_nothing_to_watch_costs_one_small_request_a_round() {
    let store = Store::open_in_memory().unwrap().into_shared();
    let custody = FlakyKeyCustody::default();
    let fake = FakeDaemonClient::new();
    for h in 1..=3 {
        fake.push_block(&format!("a{h}"), vec![]);
    }
    let node = Asked::new(&fake);
    let db = Db::over_shared(store.clone());
    let state = ScanState::default();

    // The first round starts the network just below the tip and records
    // the tip's block: from its header.
    run_round(&state, &inputs(&db, &custody, &node, &[]), ROUND_BUDGET)
        .await
        .into_result()
        .unwrap();
    assert_eq!(recorded_height(&store), Some(3));
    assert_eq!(
        node.take(),
        vec!["get_tip", "get_block_hash", "get_chain_headers"]
    );

    // Nothing new, and a pool with a transaction in it: one request.
    fake.set_mempool(vec![unrelated_tx(1)]);
    for _ in 0..3 {
        run_round(&state, &inputs(&db, &custody, &node, &[]), ROUND_BUDGET)
            .await
            .into_result()
            .unwrap();
        assert_eq!(node.take(), vec!["get_tip"]);
    }
    assert_eq!(
        fast_pass(&state, &inputs(&db, &custody, &node, &[])).await,
        Some(FastReport::default())
    );
    assert!(node.take().is_empty(), "the fast path asks nothing");

    // A new block: one hash (does the recorded chain still hold below the
    // new tip?) and the block's header.
    fake.push_block("a4", vec![fixture_tx()]);
    run_round(&state, &inputs(&db, &custody, &node, &[]), ROUND_BUDGET)
        .await
        .into_result()
        .unwrap();
    assert_eq!(recorded_height(&store), Some(4));
    assert_eq!(
        node.take(),
        vec!["get_tip", "get_block_hash", "get_chain_headers"]
    );
}

/// A store that gets an order after the network sat idle misses nothing: from
/// the next round the pool is polled (in the same call as the tip) and its
/// transactions fetched and scanned, and the next block is fetched whole and
/// scanned, for that store.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // the fixture writes to the store it is given
async fn a_store_that_gets_an_order_while_idle_is_scanned_for_from_the_next_round() {
    let store = Store::open_in_memory().unwrap().into_shared();
    let custody = FlakyKeyCustody::default();
    let fake = FakeDaemonClient::new();
    for h in 1..=3 {
        fake.push_block(&format!("a{h}"), vec![]);
    }
    let node = Asked::new(&fake);
    let db = Db::over_shared(store.clone());
    let state = ScanState::default();
    for _ in 0..2 {
        run_round(&state, &inputs(&db, &custody, &node, &[]), ROUND_BUDGET)
            .await
            .into_result()
            .unwrap();
    }
    assert_eq!(node.take().last(), Some(&"get_tip"));

    // The store and its order arrive; its payment is in the pool.
    let (tenant, handle, order) = {
        let guard = store.lock();
        fixture_tenant(&guard, &custody, crate::now_unix() + 3600).await
    };
    let tenants = [(tenant.clone(), handle)];
    fake.set_mempool(vec![fixture_tx()]);
    run_round(
        &state,
        &inputs(&db, &custody, &node, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    let asked = node.take();
    // The tip and the pool are asked for together, in one call.
    assert_eq!(asked[0], "get_tip_and_mempool", "{asked:?}");
    for apart in ["get_tip", "get_mempool_txids"] {
        assert!(!asked.contains(&apart), "{asked:?}");
    }
    assert!(asked.contains(&"get_transactions_with_ids"), "{asked:?}");
    let payments = || store.lock().get_all_payments(&order).unwrap();
    assert_eq!(payments().len(), 1, "seen in the pool");
    assert_eq!(payments()[0].block_height, None);

    // Its block: fetched whole, scanned, and the payment given its height.
    fake.set_mempool(vec![]);
    let height = fake.push_block("a4", vec![fixture_tx()]);
    run_round(
        &state,
        &inputs(&db, &custody, &node, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    let asked = node.take();
    assert!(asked.contains(&"get_chain_blocks"), "{asked:?}");
    assert!(!asked.contains(&"get_chain_headers"), "{asked:?}");
    assert_eq!(payments().len(), 1);
    assert_eq!(payments()[0].block_height, Some(height as i64));
    assert_eq!(cursor_of(&store, tenant.as_str()), Some(height));
}

/// A store with an order in scope but no keys registered can't be scanned:
/// the pool is polled (its orders are being watched for) but no bodies are
/// fetched, and new blocks are recorded from their headers. The store stays
/// behind, and catches up on whole blocks once its keys are registered.
#[tokio::test]
async fn nothing_is_fetched_for_a_store_whose_keys_are_not_registered() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let (tenant, handle, order) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let fake = FakeDaemonClient::new();
    for h in 1..=3 {
        fake.push_block(&format!("a{h}"), vec![]);
    }
    let node = Asked::new(&fake);
    let db = Db::over_shared(store.clone());
    let state = ScanState::default();
    run_round(&state, &inputs(&db, &custody, &node, &[]), ROUND_BUDGET)
        .await
        .into_result()
        .unwrap();
    node.take();

    fake.set_mempool(vec![unrelated_tx(3)]);
    let paid_in = fake.push_block("a4", vec![fixture_tx()]);
    run_round(&state, &inputs(&db, &custody, &node, &[]), ROUND_BUDGET)
        .await
        .into_result()
        .unwrap();
    let asked = node.take();
    assert!(asked.contains(&"get_tip_and_mempool"), "{asked:?}");
    assert!(asked.contains(&"get_chain_headers"), "{asked:?}");
    for fetch in ["get_transactions_with_ids", "get_chain_blocks"] {
        assert!(!asked.contains(&fetch), "{asked:?}");
    }
    assert_eq!(recorded_height(&store), Some(paid_in));
    assert!(cursor_of(&store, tenant.as_str()) < Some(paid_in));

    // Its keys are registered: it catches up on the whole block it missed.
    let tenants = [(tenant.clone(), handle)];
    run_round(
        &state,
        &inputs(&db, &custody, &node, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    assert!(node.take().contains(&"get_chain_blocks"));
    assert_eq!(cursor_of(&store, tenant.as_str()), Some(paid_in));
    let payments = store.lock().get_all_payments(&order).unwrap();
    assert_eq!(payments.len(), 1);
    assert_eq!(payments[0].block_height, Some(paid_in as i64));
}

/// Records an unconfirmed payment with this txid and (well-formed) key image.
fn unconfirmed_with_image(
    store: &SharedStore,
    order: &crate::store::OrderId,
    txid: &str,
    output: i64,
    image: &str,
) {
    store
        .lock()
        .record_payment_match(
            order,
            txid,
            output,
            1,
            &format!("[\"{image}\"]"),
            crate::now_unix(),
            None,
        )
        .unwrap();
}

/// A page of payments that left the pool is asked about in two round trips
/// (where they are; then the key images of those that are nowhere), however
/// many there are. Those still nowhere and unproven are looked at again at
/// once, then less and less often; one later proven double-spent is voided
/// when its turn comes.
#[tokio::test(start_paused = true)]
async fn a_page_of_vanished_payments_costs_two_round_trips_and_the_stuck_ones_back_off() {
    let (store, custody, fake, tenants, orders) = seeded_network(1, 20).await;
    let txid = |n: u8| hex::encode([n; 32]);
    let image = |n: u8| hex::encode([0x40 + n; 32]);
    for n in 0..5u8 {
        unconfirmed_with_image(&store, &orders[0], &txid(n), i64::from(n), &image(n));
    }
    let node = Asked::new(&fake);
    let db = Db::over_shared(store.clone());
    let state = ScanState::default();
    let lookups = |asked: &[&'static str]| -> Vec<&'static str> {
        asked
            .iter()
            .copied()
            .filter(|call| call.starts_with("locate") || call.starts_with("is_key_image"))
            .collect()
    };

    // The first rounds look at once: a transaction may only be slow to arrive.
    for _ in 0..3 {
        run_round(
            &state,
            &inputs(&db, &custody, &node, &tenants),
            ROUND_BUDGET,
        )
        .await
        .into_result()
        .unwrap();
        assert_eq!(
            lookups(&node.take()),
            vec!["locate_transactions", "is_key_image_spent"],
            "five payments, two round trips"
        );
    }
    // Still nowhere, still unproven: now they wait.
    run_round(
        &state,
        &inputs(&db, &custody, &node, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    assert!(lookups(&node.take()).is_empty());
    let voided = || {
        store
            .lock()
            .get_all_payments(&orders[0])
            .unwrap()
            .iter()
            .filter(|p| p.voided_at.is_some())
            .count()
    };
    assert_eq!(voided(), 0, "never voided on absence alone");

    // One of them is proven double-spent; within a minute it is looked at
    // again and voided.
    fake.set_key_image_status(&image(2), KeyImageStatus::SpentInBlockchain);
    tokio::time::advance(Duration::from_secs(61)).await;
    run_round(
        &state,
        &inputs(&db, &custody, &node, &tenants),
        ROUND_BUDGET,
    )
    .await
    .into_result()
    .unwrap();
    assert_eq!(
        lookups(&node.take()),
        vec!["locate_transactions", "is_key_image_spent"]
    );
    assert_eq!(voided(), 1);
}
