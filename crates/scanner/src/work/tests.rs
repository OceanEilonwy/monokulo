//! The scheduler's own guarantees: every tier advances every round, work
//! resumes across rounds and restarts, failures are isolated and backed
//! off, and reorg detection stays cheap.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use monero::Transaction;

use super::*;
use crate::daemon::fake::FakeDaemonClient;
use crate::daemon::{DaemonError, KeyImageStatus, TxLocation};
use crate::scanner::tests::{cursor_of, fixture_tenant, fixture_tx, order_status, unrelated_tx, FlakyKeyCustody};
use crate::status::OrderStatus;
use crate::store::Store;

/// A fake node that counts the block-hash lookups made against it.
struct CountingDaemon<'a> {
    inner: &'a FakeDaemonClient,
    hash_lookups: AtomicU64,
}

impl<'a> CountingDaemon<'a> {
    fn new(inner: &'a FakeDaemonClient) -> Self {
        Self { inner, hash_lookups: AtomicU64::new(0) }
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
    async fn get_block_transactions(&self, height: u64) -> Result<Vec<Transaction>, DaemonError> {
        self.inner.get_block_transactions(height).await
    }
    async fn get_blocks_range(&self, start: u64, count: u64) -> Result<Vec<Vec<Transaction>>, DaemonError> {
        self.inner.get_blocks_range(start, count).await
    }
    async fn get_mempool_transactions(&self) -> Result<Vec<Transaction>, DaemonError> {
        self.inner.get_mempool_transactions().await
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.inner.get_mempool_txids().await
    }
    async fn get_transactions(&self, txids: &[String]) -> Result<Vec<Transaction>, DaemonError> {
        self.inner.get_transactions(txids).await
    }
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        self.inner.locate_transaction(txid).await
    }
    async fn get_transaction(&self, txid: &str) -> Result<Transaction, DaemonError> {
        self.inner.get_transaction(txid).await
    }
    async fn is_key_image_spent(&self, key_images: &[String]) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.inner.is_key_image_spent(key_images).await
    }
    async fn get_block_timestamp(&self, height: u64) -> Result<u64, DaemonError> {
        self.inner.get_block_timestamp(height).await
    }
}

fn inputs<'a>(
    store: &'a SharedStore,
    custody: &'a dyn KeyCustody,
    daemon: &'a dyn MoneroDaemonClient,
    tenants: &'a [(String, WalletHandle)],
) -> RoundInputs<'a> {
    RoundInputs {
        store,
        custody,
        daemon,
        network: "mainnet",
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
    run_round(&state, &inputs(&store, &custody, &daemon, &tenants), Duration::ZERO).await.into_result().unwrap();
    let seeded = cursor_of(&store, &tenant).unwrap();

    for i in 0..3 {
        daemon.push_block(&format!("n{i}"), vec![unrelated_tx(40 + i)]);
    }
    daemon.set_mempool(vec![fixture_tx()]);
    let mut blocks_moved = 0;
    for _ in 0..3 {
        let before = cursor_of(&store, &tenant).unwrap();
        let report = run_round(&state, &inputs(&store, &custody, &daemon, &tenants), Duration::ZERO).await;
        for tier in Tier::ALL {
            assert!(report.steps[tier.index()] >= 1, "{} got no unit", tier.name());
        }
        blocks_moved += cursor_of(&store, &tenant).unwrap() - before;
    }
    assert_eq!(blocks_moved, 3, "one block per round at least, all three scanned");
    assert!(cursor_of(&store, &tenant).unwrap() >= seeded + 3);
    assert_eq!(store.lock().get_all_payments(&order).unwrap().len(), 1, "the mempool was scanned");
    assert_eq!(order_status(&store, &order), OrderStatus::Unconfirmed, "and its status recomputed");
}

/// Block hashes chain, so agreeing at the high-water mark settles the whole
/// window with one lookup; a divergence is found by binary search.
#[tokio::test]
async fn reorg_detection_costs_one_lookup_when_the_chain_agrees_and_log_depth_when_not() {
    let store = Store::open_in_memory().unwrap().into_shared();
    let fake = FakeDaemonClient::new();
    for h in 1..=60 {
        let height = fake.push_block(&format!("a{h}"), vec![]);
        store.lock().set_scanned_block("mainnet", height, &format!("a{h}")).unwrap();
    }
    let daemon = CountingDaemon::new(&fake);
    let chain = chain::Chain { store: &store, daemon: &daemon, network: "mainnet", reorg_check_depth: 20, now: 1000 };
    assert_eq!(chain.detect(60).await.unwrap(), None);
    assert_eq!(daemon.take_hash_lookups(), 1);

    for fork in [41, 45, 52, 59, 60] {
        let tip = fake.get_height().await.unwrap();
        let replacement: Vec<(String, Vec<Transaction>)> = (fork..=tip).map(|h| (format!("b{fork}_{h}"), vec![])).collect();
        fake.reorg_from(fork, replacement.iter().map(|(hash, txs)| (hash.as_str(), txs.clone())).collect());
        assert_eq!(chain.detect(60).await.unwrap(), Some(fork), "fork at {fork}");
        let lookups = daemon.take_hash_lookups();
        assert!(lookups <= 2 + 5, "fork at {fork}: {lookups} lookups for a 21-block window");
        // Put the original chain back for the next case.
        let original: Vec<(String, Vec<Transaction>)> = (fork..=tip).map(|h| (format!("a{h}"), vec![])).collect();
        fake.reorg_from(fork, original.iter().map(|(hash, txs)| (hash.as_str(), txs.clone())).collect());
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
    store.create_webhook(&tenant, "https://merchant.example/hook", "{}", "secret", 1000).unwrap();
    let fake = FakeDaemonClient::new();
    for h in 1..=60 {
        let txs = if h == 30 { vec![fixture_tx()] } else { vec![] };
        let height = fake.push_block(&format!("a{h}"), txs);
        store.set_scanned_block("mainnet", height, &format!("a{h}")).unwrap();
    }
    store.execute_raw_for_test("UPDATE tenants SET scanned_through_height = 60").unwrap();
    // The order's payment, 31 blocks deep: settled as soon as it's recomputed.
    store.record_payment_match(&order, &crate::scanner::tx_id_hex(&fixture_tx()), 0, 1, "[\"ki\"]", 1000, Some(30)).unwrap();
    // Plenty of other payments above the fork, more than one unit handles.
    for i in 0..40u8 {
        let other = crate::scanner::tests::fixture_tenant(&store, &custody, crate::now_unix() + 3600).await.2;
        store.record_payment_match(&other, &format!("{i:064x}"), 0, 1, "[\"ki\"]", 1000, Some(55)).unwrap();
    }
    fake.reorg_from(50, (50..=60).map(|_| ("b", vec![])).collect());
    let store = store.into_shared();
    let tenants = [(tenant.clone(), handle)];

    let state = ScanState::default();
    run_round(&state, &inputs(&store, &custody, &fake, &tenants), Duration::ZERO).await.into_result().unwrap();
    assert!(store.lock().reorg_job("mainnet").unwrap().is_some(), "one unit doesn't finish a 41-payment job");
    assert_ne!(order_status(&store, &order), OrderStatus::Paid, "no settlement while a reorg is open");
    let paid_events = |store: &SharedStore| {
        store.lock().due_webhook_deliveries(i64::MAX, 100).unwrap().iter().filter(|d| d.event_type == "order.paid").count()
    };
    assert_eq!(paid_events(&store), 0);

    // Restart: a new process, a new connection, no memory.
    drop(store);
    let store = Store::open_file(&path).unwrap().into_shared();
    let state = ScanState::default();
    for _ in 0..20 {
        run_round(&state, &inputs(&store, &custody, &fake, &tenants), ROUND_BUDGET).await.into_result().unwrap();
        if store.lock().reorg_job("mainnet").unwrap().is_none() && order_status(&store, &order) == OrderStatus::Paid {
            break;
        }
    }
    assert!(store.lock().reorg_job("mainnet").unwrap().is_none(), "the job finished");
    assert_eq!(order_status(&store, &order), OrderStatus::Paid);
    assert_eq!(paid_events(&store), 1, "announced once, after the rewind");
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
    let (failing, failing_handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let (healthy, healthy_handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let daemon = FakeDaemonClient::new();
    daemon.push_block("h1", vec![]);
    daemon.push_block("h2", vec![]);
    let tenants = [(failing.clone(), failing_handle), (healthy.clone(), healthy_handle)];
    let state = ScanState::default();
    run_round(&state, &inputs(&store, &custody, &daemon, &tenants), ROUND_BUDGET).await.into_result().unwrap();
    let start = cursor_of(&store, &healthy).unwrap();

    custody.fail(failing_handle);
    for i in 0..6u8 {
        daemon.push_block(&format!("n{i}"), vec![unrelated_tx(60 + i)]);
        run_round(&state, &inputs(&store, &custody, &daemon, &tenants), ROUND_BUDGET).await.into_result().unwrap();
        assert_eq!(cursor_of(&store, &healthy).unwrap(), start + 1 + i as u64, "the healthy tenant never waits");
    }
    let attempts = custody.attempts.lock().get(&failing_handle).copied().unwrap_or(0);
    assert!(attempts <= 4, "backed off after its free retries, but was tried {attempts} times in 6 rounds");
    assert_eq!(cursor_of(&store, &failing), Some(start), "never moved past a block it wasn't scanned for");

    custody.recover(failing_handle);
    tokio::time::advance(Duration::from_secs(120)).await;
    for _ in 0..3 {
        run_round(&state, &inputs(&store, &custody, &daemon, &tenants), ROUND_BUDGET).await.into_result().unwrap();
    }
    assert_eq!(cursor_of(&store, &failing), cursor_of(&store, &healthy), "caught up once its backend recovered");
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
    run_round(&ScanState::default(), &inputs(&store, &custody, &daemon, &tenants), ROUND_BUDGET).await.into_result().unwrap();
    let before = cursor_of(&store, &tenant).unwrap();

    // The payment first, then plenty of unrelated transactions.
    let mut txs = vec![fixture_tx()];
    txs.extend((0..24u8).map(|i| unrelated_tx(100 + i)));
    let height = daemon.push_block("big", txs);
    let mut store = store;
    let mut rounds = 0;
    let mut last_checkpoint = 0;
    while cursor_of(&store, &tenant).unwrap() == before {
        rounds += 1;
        assert!(rounds < 100, "never finished the block");
        let state = ScanState::default();
        run_round(&state, &inputs(&store, &custody, &daemon, &tenants), Duration::ZERO).await.into_result().unwrap();
        if cursor_of(&store, &tenant).unwrap() == before {
            assert!(store.lock().get_all_payments(&order).unwrap().is_empty(), "no payment before the block commits");
            let checkpoint = store.lock().block_checkpoint("mainnet", &tenant).unwrap().expect("progress is checkpointed");
            assert!(checkpoint.next_tx > last_checkpoint, "every round moves the checkpoint forward");
            last_checkpoint = checkpoint.next_tx;
        }
        // Every few rounds, restart.
        if rounds % 3 == 0 {
            drop(store);
            store = Store::open_file(&path).unwrap().into_shared();
        }
    }
    assert!(rounds > 1, "the block really was split across rounds");
    assert_eq!(cursor_of(&store, &tenant), Some(height));
    let payments = store.lock().get_all_payments(&order).unwrap();
    assert_eq!(payments.len(), 1);
    assert_eq!(payments[0].block_height, Some(height as i64));
    assert_eq!(store.lock().block_checkpoint("mainnet", &tenant).unwrap(), None, "checkpoint cleared at commit");
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
    let (lagging, lagging_handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let (live, live_handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let daemon = FakeDaemonClient::new();
    daemon.push_block("h1", vec![]);
    daemon.push_block("h2", vec![]);
    let tenants = [(lagging.clone(), lagging_handle), (live.clone(), live_handle)];
    let state = ScanState::default();
    run_round(&state, &inputs(&store, &custody, &daemon, &tenants), ROUND_BUDGET).await.into_result().unwrap();
    custody.fail(lagging_handle);
    for i in 0..3u8 {
        daemon.push_block(&format!("m{i}"), vec![unrelated_tx(150 + i)]);
    }
    run_round(&state, &inputs(&store, &custody, &daemon, &tenants), ROUND_BUDGET).await.into_result().unwrap();
    let behind = cursor_of(&store, &lagging).unwrap();
    assert!(behind < cursor_of(&store, &live).unwrap());
    custody.recover(lagging_handle);
    tokio::time::advance(Duration::from_secs(120)).await; // past its retry delay

    // Now the node is 100 blocks ahead; with no time to spare, each round
    // gives the blocks tier one unit, so turns must alternate.
    for i in 0..100u8 {
        daemon.push_block(&format!("far{i}"), vec![]);
    }
    let frontier_start = store.lock().max_scanned_height("mainnet").unwrap().unwrap();
    for _ in 0..4 {
        run_round(&state, &inputs(&store, &custody, &daemon, &tenants), Duration::ZERO).await.into_result().unwrap();
    }
    assert!(cursor_of(&store, &lagging).unwrap() > behind, "catch-up got a turn");
    assert!(store.lock().max_scanned_height("mainnet").unwrap().unwrap() > frontier_start, "and so did the frontier");
}
