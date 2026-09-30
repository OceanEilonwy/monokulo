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
use crate::store::{Db, SharedStore, Store};

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
    db: &'a Db,
    custody: &'a dyn KeyCustody,
    daemon: &'a dyn MoneroDaemonClient,
    tenants: &'a [(String, WalletHandle)],
) -> RoundInputs<'a> {
    RoundInputs {
        db,
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
    run_round(&state, &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants), Duration::ZERO).await.into_result().unwrap();
    let seeded = cursor_of(&store, &tenant).unwrap();

    for i in 0..3 {
        daemon.push_block(&format!("n{i}"), vec![unrelated_tx(40 + i)]);
    }
    daemon.set_mempool(vec![fixture_tx()]);
    let mut blocks_moved = 0;
    for _ in 0..3 {
        let before = cursor_of(&store, &tenant).unwrap();
        let report = run_round(&state, &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants), Duration::ZERO).await;
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
    let db = Db::over_shared(store.clone());
    let chain = chain::Chain { db: &db, daemon: &daemon, network: "mainnet", reorg_check_depth: 20, now: 1000 };
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
    let db = worker(&store, &path);
    run_round(&state, &inputs(&db, &custody, &fake, &tenants), Duration::ZERO).await.into_result().unwrap();
    assert!(store.lock().reorg_job("mainnet").unwrap().is_some(), "one unit doesn't finish a 41-payment job");
    assert_ne!(order_status(&store, &order), OrderStatus::Paid, "no settlement while a reorg is open");
    let paid_events = |store: &SharedStore| {
        store.lock().due_webhook_deliveries(i64::MAX, 100).unwrap().iter().filter(|d| d.event_type == "order.paid").count()
    };
    assert_eq!(paid_events(&store), 0);

    // Restart: a new process, new connections, no memory.
    drop(db);
    drop(store);
    let store = Store::open_file(&path).unwrap().into_shared();
    let db = worker(&store, &path);
    let state = ScanState::default();
    for _ in 0..20 {
        run_round(&state, &inputs(&db, &custody, &fake, &tenants), ROUND_BUDGET).await.into_result().unwrap();
        if store.lock().reorg_job("mainnet").unwrap().is_none() && order_status(&store, &order) == OrderStatus::Paid {
            break;
        }
    }
    assert!(store.lock().reorg_job("mainnet").unwrap().is_none(), "the job finished");
    assert_eq!(order_status(&store, &order), OrderStatus::Paid);
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
    let (failing, failing_handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let (healthy, healthy_handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
    let store = store.into_shared();
    let daemon = FakeDaemonClient::new();
    daemon.push_block("h1", vec![]);
    daemon.push_block("h2", vec![]);
    let tenants = [(failing.clone(), failing_handle), (healthy.clone(), healthy_handle)];
    let state = ScanState::default();
    run_round(&state, &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants), ROUND_BUDGET).await.into_result().unwrap();
    let start = cursor_of(&store, &healthy).unwrap();

    custody.fail(failing_handle);
    for i in 0..6u8 {
        daemon.push_block(&format!("n{i}"), vec![unrelated_tx(60 + i)]);
        run_round(&state, &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants), ROUND_BUDGET).await.into_result().unwrap();
        assert_eq!(cursor_of(&store, &healthy).unwrap(), start + 1 + i as u64, "the healthy tenant never waits");
    }
    let attempts = custody.attempts.lock().get(&failing_handle).copied().unwrap_or(0);
    assert!(attempts <= 4, "backed off after its free retries, but was tried {attempts} times in 6 rounds");
    assert_eq!(cursor_of(&store, &failing), Some(start), "never moved past a block it wasn't scanned for");

    custody.recover(failing_handle);
    tokio::time::advance(Duration::from_secs(120)).await;
    for _ in 0..3 {
        run_round(&state, &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants), ROUND_BUDGET).await.into_result().unwrap();
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
    run_round(&ScanState::default(), &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants), ROUND_BUDGET).await.into_result().unwrap();
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
        let db = worker(&store, &path);
        run_round(&state, &inputs(&db, &custody, &daemon, &tenants), Duration::ZERO).await.into_result().unwrap();
        drop(db);
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
    run_round(&state, &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants), ROUND_BUDGET).await.into_result().unwrap();
    custody.fail(lagging_handle);
    for i in 0..3u8 {
        daemon.push_block(&format!("m{i}"), vec![unrelated_tx(150 + i)]);
    }
    run_round(&state, &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants), ROUND_BUDGET).await.into_result().unwrap();
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
        run_round(&state, &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants), Duration::ZERO).await.into_result().unwrap();
    }
    assert!(cursor_of(&store, &lagging).unwrap() > behind, "catch-up got a turn");
    assert!(store.lock().max_scanned_height("mainnet").unwrap().unwrap() > frontier_start, "and so did the frontier");
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
        if txid == self.txid {
            return Err(DaemonError::Request("this node can't find that transaction right now".into()));
        }
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

/// While a reorg is being reconciled, only what depends on the chain being
/// settled waits: blocks aren't scanned and no order newly settles. The
/// mempool is still scanned and orders still expire.
#[tokio::test]
async fn an_open_reorg_pauses_blocks_and_settlement_but_not_the_mempool_or_expiry() {
    let store = Store::open_in_memory().unwrap();
    let custody = FlakyKeyCustody::default();
    let now = crate::now_unix();
    let (tenant, handle, open_order) = fixture_tenant(&store, &custody, now + 3600).await;
    let (_, _, overdue_order) = fixture_tenant(&store, &custody, now - 10).await;
    let (_, _, candidate_order) = fixture_tenant(&store, &custody, now + 3600).await;
    let fake = FakeDaemonClient::new();
    for h in 1..=10 {
        let height = fake.push_block(&format!("a{h}"), vec![]);
        store.set_scanned_block("mainnet", height, &format!("a{h}")).unwrap();
    }
    store.execute_raw_for_test("UPDATE tenants SET scanned_through_height = 10").unwrap();
    // A reorg is open, with a candidate the node can't answer about.
    let stuck = "ab".repeat(32);
    store.record_payment_match(&candidate_order, &stuck, 0, 1, "[\"ki\"]", now, Some(9)).unwrap();
    store.open_reorg_job("mainnet", 9, now).unwrap();
    let store = store.into_shared();
    let daemon = CannotLocate { inner: &fake, txid: stuck };
    fake.push_block("new", vec![unrelated_tx(200)]);
    fake.set_mempool(vec![fixture_tx()]);
    let tenants = [(tenant.clone(), handle)];

    let report = run_round(&ScanState::default(), &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants), ROUND_BUDGET).await;
    assert!(store.lock().reorg_job("mainnet").unwrap().is_some(), "the job is still open");
    assert_eq!(report.outcome(Tier::Blocks), TierOutcome::Blocked("a reorganisation is being reconciled"));
    assert_eq!(store.lock().max_scanned_height("mainnet").unwrap(), Some(10), "no block scanned on a chain being reconciled");
    assert_eq!(store.lock().get_all_payments(&open_order).unwrap().len(), 1, "the mempool was still scanned");
    assert_eq!(order_status(&store, &open_order), OrderStatus::Unconfirmed);
    assert_eq!(order_status(&store, &overdue_order), OrderStatus::Expired, "and orders still expire");
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
            run_round(state, &inputs(&Db::over_shared(store.clone()), custody, fake, tenants), budget).await.into_result().unwrap();
        }
    };
    round(ROUND_BUDGET).await;
    for h in 3..=40u64 {
        fake.push_block(&format!("a{h}"), if h == 35 { vec![fixture_tx()] } else { vec![] });
    }
    round(ROUND_BUDGET).await;
    round(ROUND_BUDGET).await;
    round(ROUND_BUDGET).await;
    round(ROUND_BUDGET).await;
    round(ROUND_BUDGET).await;
    assert_eq!(store.lock().get_all_payments(&order).unwrap()[0].block_height, Some(35));

    // First fork at 35: the payment moves to 36.
    let first: Vec<(String, Vec<Transaction>)> =
        (35..=40u64).map(|h| (format!("b{h}"), if h == 36 { vec![fixture_tx()] } else { vec![] })).collect();
    fake.reorg_from(35, first.iter().map(|(hash, txs)| (hash.as_str(), txs.clone())).collect());
    round(Duration::ZERO).await; // detects and starts the job, no more
    assert!(store.lock().reorg_job("mainnet").unwrap().is_some());

    // Before it finishes, a deeper fork at 30: the payment moves to 31.
    let second: Vec<(String, Vec<Transaction>)> =
        (30..=41u64).map(|h| (format!("c{h}"), if h == 31 { vec![fixture_tx()] } else { vec![] })).collect();
    fake.reorg_from(30, second.iter().map(|(hash, txs)| (hash.as_str(), txs.clone())).collect());
    for _ in 0..10 {
        round(ROUND_BUDGET).await;
    }
    assert!(store.lock().reorg_job("mainnet").unwrap().is_none());
    let payments = store.lock().get_all_payments(&order).unwrap();
    assert_eq!(payments.len(), 1, "recorded once");
    assert_eq!(payments[0].block_height, Some(31), "at its height on the final chain");
    assert_eq!(payments[0].voided_at, None);
    assert_eq!(store.lock().get_scanned_block_hash("mainnet", 41).unwrap().as_deref(), Some("c41"), "rescanned to the new tip");
    assert_eq!(cursor_of(&store, &tenant), Some(41));
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
    async fn get_block_transactions(&self, height: u64) -> Result<Vec<Transaction>, DaemonError> {
        self.inner.get_block_transactions(height).await
    }
    async fn get_chain_blocks(&self, start: u64, count: u64) -> Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
        self.chain_fetches.fetch_add(1, Ordering::Relaxed);
        let blocks = self.inner.get_chain_blocks(start, count).await?;
        if self.armed.swap(false, Ordering::Relaxed) {
            let tip = self.inner.get_height().await?;
            let replacement: Vec<(String, Vec<Transaction>)> = (self.fork..=tip).map(|h| (format!("new{h}"), vec![])).collect();
            self.inner.reorg_from(self.fork, replacement.iter().map(|(hash, txs)| (hash.as_str(), txs.clone())).collect());
        }
        Ok(blocks)
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
    assert_eq!(store.lock().get_all_payments(&order).unwrap()[0].block_height, Some(3));

    // The chain replaces block 3 with one that doesn't hold the payment,
    // and nothing proves its inputs were spent elsewhere.
    fake.reorg_from(3, vec![("b3", vec![])]);
    for i in 0..15 {
        fake.push_block(&format!("b{}", 4 + i), vec![]);
    }
    for _ in 0..6 {
        round().await;
    }
    let payment = &store.lock().get_all_payments(&order).unwrap()[0];
    assert_eq!(payment.block_height, None, "not in any block any more");
    assert_eq!(payment.voided_at, None, "and not voided without proof");
    assert_ne!(order_status(&store, &order), OrderStatus::Paid, "no confirmations on a discarded block");
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
        store.set_scanned_block("mainnet", height, &format!("a{h}")).unwrap();
    }
    store.execute_raw_for_test(&format!("UPDATE tenants SET scanned_through_height = 100 WHERE id = '{far}'")).unwrap();
    store.execute_raw_for_test(&format!("UPDATE tenants SET scanned_through_height = 300 WHERE id = '{near}'")).unwrap();
    let store = store.into_shared();
    let tenants = [(far.clone(), far_handle), (near.clone(), near_handle)];
    let state = ScanState::default();
    for _ in 0..2 {
        run_round(&state, &inputs(&Db::over_shared(store.clone()), &custody, &fake, &tenants), Duration::ZERO).await.into_result().unwrap();
    }
    assert!(cursor_of(&store, &far).unwrap() > 100, "the far group had its turn");
    assert!(cursor_of(&store, &near).unwrap() > 300, "and so did the other, on the very next turn");
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
        fake.push_block(&format!("a{h}"), if h == 11 { vec![fixture_tx()] } else { vec![] });
    }
    let daemon = ReorgsAfterFetch { inner: &fake, fork: 11, armed: true.into(), chain_fetches: AtomicU64::new(0) };
    for _ in 0..6 {
        run_round_on(&store, &custody, &daemon, &tenants).await;
    }
    assert_eq!(store.lock().get_scanned_block_hash("mainnet", 11).unwrap().as_deref(), Some("new11"), "reconciled to the new tip");
    let payments = store.lock().get_all_payments(&order).unwrap();
    assert!(payments.iter().all(|p| p.block_height.is_none()), "nothing counted in a discarded block: {payments:?}");
    assert_ne!(order_status(&store, &order), OrderStatus::Paid);
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
        store.set_scanned_block("mainnet", height, &format!("a{h}")).unwrap();
    }
    store.execute_raw_for_test(&format!("UPDATE tenants SET scanned_through_height = 5 WHERE id = '{unregistered}'")).unwrap();
    let store = store.into_shared();
    let daemon = ReorgsAfterFetch { inner: &fake, fork: 0, armed: false.into(), chain_fetches: AtomicU64::new(0) };
    run_round_on(&store, &custody, &daemon, &[]).await;
    assert_eq!(daemon.chain_fetches.load(Ordering::Relaxed), 0);
    assert_eq!(cursor_of(&store, &unregistered), Some(5), "still where it was, to be caught up once registered");
}

/// One round with the default budget and fresh memory.
async fn run_round_on(store: &SharedStore, custody: &dyn KeyCustody, daemon: &dyn MoneroDaemonClient, tenants: &[(String, WalletHandle)]) {
    run_round(&ScanState::default(), &inputs(&Db::over_shared(store.clone()), custody, daemon, tenants), ROUND_BUDGET)
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
        Self { inner, locate_calls: AtomicU64::new(0), fail_bodies: false.into(), body_requests: Default::default() }
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
    async fn get_block_transactions(&self, height: u64) -> Result<Vec<Transaction>, DaemonError> {
        self.inner.get_block_transactions(height).await
    }
    async fn get_chain_blocks(&self, start: u64, count: u64) -> Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
        self.inner.get_chain_blocks(start, count).await
    }
    async fn get_mempool_transactions(&self) -> Result<Vec<Transaction>, DaemonError> {
        self.inner.get_mempool_transactions().await
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.inner.get_mempool_txids().await
    }
    async fn get_transactions(&self, txids: &[String]) -> Result<Vec<Transaction>, DaemonError> {
        let mut asked = txids.to_vec();
        asked.sort();
        self.body_requests.lock().push(asked);
        if self.fail_bodies.load(Ordering::Relaxed) {
            return Err(DaemonError::Request("bodies unavailable".into()));
        }
        self.inner.get_transactions(txids).await
    }
    async fn locate_transaction(&self, _txid: &str) -> Result<TxLocation, DaemonError> {
        self.locate_calls.fetch_add(1, Ordering::Relaxed);
        Err(DaemonError::Request("this node can't look transactions up right now".into()))
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
        store.set_scanned_block("mainnet", height, &format!("a{h}")).unwrap();
    }
    for i in 0..20u8 {
        let order = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await.2;
        store.record_payment_match(&order, &format!("{i:064x}"), 0, 1, "[]", 1000, Some(9)).unwrap();
    }
    store.open_reorg_job("mainnet", 9, crate::now_unix()).unwrap();
    let store = store.into_shared();
    let daemon = Lookups::new(&fake);
    let state = ScanState::default();
    let report = run_round(&state, &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &[]), ROUND_BUDGET).await;
    assert_eq!(daemon.locate_calls.load(Ordering::Relaxed), 1);
    assert_eq!(report.outcome(Tier::Chain), TierOutcome::Blocked("the node failed"));
    assert!(report.error.is_none(), "a node failure is retried, not a failed round");
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
        store.set_scanned_block("mainnet", height, &format!("a{h}")).unwrap();
    }
    store.execute_raw_for_test("UPDATE tenants SET scanned_through_height = 3").unwrap();
    store
        .execute_raw_for_test(&format!(
            "CREATE TRIGGER poisoned_order BEFORE UPDATE OF status ON orders WHEN NEW.id = '{poisoned}'
             BEGIN SELECT RAISE(ABORT, 'simulated persistent failure'); END;"
        ))
        .unwrap();
    let store = store.into_shared();
    let state = ScanState::default();
    let db = Db::over_shared(store.clone());
    let round = || async { run_round(&state, &inputs(&db, &custody, &fake, &[]), ROUND_BUDGET).await };
    let first = round().await;
    assert!(first.error.is_none(), "the other order went through: {:?}", first.error);
    assert_eq!(order_status(&store, &healthy), OrderStatus::Expired);
    // Alone in its page it is reported (a real storage failure), until its
    // free retries are spent; then it waits, and the round is clean.
    round().await;
    round().await;
    assert!(state.order_backoff.is_waiting(&poisoned));
    let later = round().await;
    assert!(later.error.is_none(), "{:?}", later.error);
    assert_eq!(order_status(&store, &poisoned), OrderStatus::Pending);
}

/// Backoff entries for keys that stopped failing (and stopped being tried)
/// are forgotten after an hour, so the map can't grow without bound.
#[tokio::test(start_paused = true)]
async fn backoff_forgets_keys_that_stopped_failing() {
    let backoff = Backoff::default();
    for _ in 0..4 {
        backoff.failed("gone");
    }
    assert_eq!(backoff.waiting(), vec!["gone".to_string()]);
    tokio::time::advance(Duration::from_secs(61 * 60)).await;
    assert!(backoff.waiting().is_empty());
    assert!(backoff.failures.lock().is_empty(), "forgotten, not just no longer waiting");
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
    fake.set_mempool((0..100u8).map(|i| unrelated_tx(i)).collect());
    let daemon = Lookups::new(&fake);
    daemon.fail_bodies.store(true, Ordering::Relaxed);
    let tenants = [(tenant, handle)];
    let state = ScanState::default();
    run_round(&state, &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants), ROUND_BUDGET).await;
    daemon.fail_bodies.store(false, Ordering::Relaxed);
    run_round(&state, &inputs(&Db::over_shared(store.clone()), &custody, &daemon, &tenants), ROUND_BUDGET).await;
    let requests = daemon.body_requests.lock();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1], "the same slice again");
}
