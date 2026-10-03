//! File-backed, deterministic scanner workload for `cargo xtask stress`
//! (`docs/engine_stress.md`). Setup is excluded from measured tick timings.
//! This executable drives the production scanner, `Store` migrations and plain
//! key custody against a scripted daemon.
//!
//! Progress is measured over the fixture's own read-only SQLite connection,
//! never through engine APIs, so the same measurements stay valid while the
//! engine's store and scheduling code change underneath them.

// A command-line tool: its result goes to stdout and its errors to stderr.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command-line tool reports on stdout and stderr"
)]
mod stress_common;

use std::collections::HashMap;
use std::error::Error;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{body::Body, http::Request};
use engine::daemon::{
    ChainBlock, DaemonError, FetchedTx, KeyImageStatus, MoneroDaemonClient, TxLocation,
};
use engine::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use engine::engine_settings::{Daemons, EngineSettings};
use engine::http::{build_router, rate_limit::RateLimiter, AppState};
use monero::{Network, Transaction};
use parking_lot::RwLock;

/// The engine token the fixture's in-process router accepts.
const FIXTURE_ENGINE_TOKEN: &str = "stress_fixture_engine_token_0123456789abcdef";
use engine::key_custody::{
    KeyCustody, KeyCustodyError, MatchedOutput, PlainKeyCustody, ScanIndices, ScanInput,
    SubaddressIndex, TxMatches, WalletHandle, WalletMaterial,
};
use engine::scanner_status::new_scanner_status_map;
use engine::store::{Db, NewOrder, NewTenant, ReadStorePool, Store};
use serde_json::json;
use tower::ServiceExt as _;

/// The result format `xtask/src/stress.rs` checks against the scenario file.
const SCHEMA_VERSION: u64 = 3;
const NETWORK: &str = "mainnet";

struct FixtureDaemon {
    height: AtomicU64,
    tx: Transaction,
    txid: String,
    rpc_delay_ms: u64,
    rpc_fail_until_height: u64,
    rpc_fail_every: u64,
    rpc_calls: AtomicU64,
    rpc_failures: AtomicU64,
}

impl FixtureDaemon {
    async fn rpc(&self) -> Result<(), DaemonError> {
        if self.rpc_delay_ms != 0 {
            tokio::time::sleep(Duration::from_millis(self.rpc_delay_ms)).await;
        }
        let call = self.rpc_calls.fetch_add(1, Ordering::Relaxed) + 1;
        if self.rpc_fail_every != 0
            && self.height.load(Ordering::Relaxed) <= self.rpc_fail_until_height
            && call.is_multiple_of(self.rpc_fail_every)
        {
            self.rpc_failures.fetch_add(1, Ordering::Relaxed);
            return Err(DaemonError::Request("scheduled fixture RPC failure".into()));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl MoneroDaemonClient for FixtureDaemon {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.rpc().await?;
        Ok(self.height.load(Ordering::Relaxed))
    }
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        self.rpc().await?;
        Ok(format!("stress-block-{height}"))
    }
    /// One round trip for the run of blocks, as the real client makes.
    /// Every block but the first holds the fixture's transaction.
    async fn get_chain_blocks(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainBlock>, DaemonError> {
        self.rpc().await?;
        let tip = self.height.load(Ordering::Relaxed);
        Ok((start_height..start_height.saturating_add(count))
            .take_while(|height| *height <= tip)
            .map(|height| ChainBlock {
                height,
                hash: format!("stress-block-{height}"),
                prev_hash: height
                    .checked_sub(1)
                    .map_or_default(|parent| format!("stress-block-{parent}")),
                timestamp: 1_700_000_000 + height * 120,
                txs: if height == 0 {
                    vec![]
                } else {
                    vec![engine::daemon::ScanTx::of(&self.tx)]
                },
                txids: if height == 0 {
                    vec![]
                } else {
                    vec![self.txid.clone()]
                },
                wire_bytes: if height == 0 {
                    0
                } else {
                    monero::consensus::encode::serialize(&self.tx).len() as u64
                },
            })
            .collect())
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.rpc().await?;
        Ok(vec![])
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<FetchedTx>, DaemonError> {
        self.rpc().await?;
        Ok(if txids.contains(&self.txid) {
            vec![FetchedTx {
                txid: self.txid.clone(),
                tx: self.tx.clone(),
            }]
        } else {
            vec![]
        })
    }
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        self.rpc().await?;
        Ok(if txid == self.txid {
            TxLocation::InBlock(1)
        } else {
            TxLocation::NotFound
        })
    }
    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.rpc().await?;
        Ok(vec![KeyImageStatus::Unspent; key_images.len()])
    }
}

/// Reproducible finite-capacity custody backend for the fault point. The
/// underlying scan is real plain custody; only admission and delay are scripted.
struct SaturatedCustody {
    inner: Arc<PlainKeyCustody>,
    slots: tokio::sync::Semaphore,
    delay: Duration,
    max_wait_us: AtomicU64,
    completed: AtomicU64,
}

impl SaturatedCustody {
    async fn acquire(&self) -> Result<tokio::sync::SemaphorePermit<'_>, KeyCustodyError> {
        let started = Instant::now();
        let permit = self
            .slots
            .acquire()
            .await
            .map_err(|error| KeyCustodyError::BackendUnavailable(error.to_string()))?;
        self.max_wait_us
            .fetch_max(started.elapsed().as_micros() as u64, Ordering::Relaxed);
        Ok(permit)
    }
}

#[async_trait::async_trait]
impl KeyCustody for SaturatedCustody {
    async fn register_wallet(
        &self,
        material: WalletMaterial,
    ) -> Result<WalletHandle, KeyCustodyError> {
        self.inner.register_wallet(material).await
    }
    async fn remove_wallet(&self, handle: WalletHandle) -> Result<(), KeyCustodyError> {
        self.inner.remove_wallet(handle).await
    }
    async fn seal(&self, material: &WalletMaterial) -> Result<Vec<u8>, KeyCustodyError> {
        self.inner.seal(material).await
    }
    async fn unseal_and_register(&self, sealed: &[u8]) -> Result<WalletHandle, KeyCustodyError> {
        self.inner.unseal_and_register(sealed).await
    }
    async fn derive_subaddress(
        &self,
        handle: WalletHandle,
        index: SubaddressIndex,
        network: Network,
    ) -> Result<monero::Address, KeyCustodyError> {
        self.inner.derive_subaddress(handle, index, network).await
    }
    async fn scan_tx_outputs(
        &self,
        handle: WalletHandle,
        tx: &ScanInput,
        major_range: Range<u32>,
        minor_range: Range<u32>,
    ) -> Result<Vec<MatchedOutput>, KeyCustodyError> {
        let _permit = self.acquire().await?;
        tokio::time::sleep(self.delay).await;
        let found = self
            .inner
            .scan_tx_outputs(handle, tx, major_range, minor_range)
            .await;
        self.completed.fetch_add(1, Ordering::Relaxed);
        found
    }
    async fn scan_txs_for_indices(
        &self,
        handle: WalletHandle,
        txs: &[ScanInput],
        indices: &ScanIndices,
    ) -> Result<Vec<TxMatches>, KeyCustodyError> {
        let _permit = self.acquire().await?;
        tokio::time::sleep(self.delay).await;
        let found = self.inner.scan_txs_for_indices(handle, txs, indices).await;
        self.completed.fetch_add(1, Ordering::Relaxed);
        found
    }
}

struct Args(Vec<String>);

impl Args {
    fn value(&self, name: &str) -> Option<&str> {
        self.0
            .windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].as_str())
    }
    fn required<T: std::str::FromStr>(&self, name: &str) -> Result<T, Box<dyn Error>>
    where
        T::Err: Error + 'static,
    {
        Ok(self
            .value(name)
            .ok_or_else(|| format!("missing {name}"))?
            .parse()?)
    }
    fn optional(&self, name: &str) -> Result<u64, Box<dyn Error>> {
        Ok(self.value(name).map(str::parse).transpose()?.unwrap_or(0))
    }
}

/// Which engine code runs each tick. `scheduler` is one scheduler round
/// (`engine::work::run_round`, `docs/scanner_microtasks.md`), as the
/// production loop runs it. The phase-by-phase tick it replaced was
/// measured as `legacy`; its results are kept in `docs/stress/`.
#[derive(Clone, Copy)]
enum Driver {
    Scheduler,
}

impl Driver {
    fn parse(name: &str) -> Result<Self, Box<dyn Error>> {
        match name {
            "scheduler" => Ok(Self::Scheduler),
            other => Err(format!("unknown driver {other}").into()),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Scheduler => "scheduler",
        }
    }
}

/// One tick's worth of engine state the driver needs, kept across ticks.
struct Engine {
    driver: Driver,
    db: Db,
    custody: Arc<dyn KeyCustody>,
    daemon: Arc<FixtureDaemon>,
    handles: Arc<RwLock<HashMap<engine::store::TenantId, WalletHandle>>>,
    memory: Arc<engine::work::ScanState>,
}

impl Engine {
    /// Runs one tick as production does: a round on the multi-threaded
    /// runtime, through the database worker, with the production defaults
    /// for reorg depth, grace period and memory budget.
    async fn tick(&self) -> Result<(), String> {
        match self.driver {
            Driver::Scheduler => {
                let (memory, db, custody, daemon) = (
                    Arc::clone(&self.memory),
                    self.db.clone(),
                    Arc::clone(&self.custody),
                    Arc::clone(&self.daemon),
                );
                let tenants: Vec<(engine::store::TenantId, WalletHandle)> = self
                    .handles
                    .read()
                    .iter()
                    .map(|(id, handle)| (id.clone(), *handle))
                    .collect();
                let scan = EngineSettings::defaults().scan.load();
                tokio::spawn(async move {
                    let inputs = engine::work::RoundInputs {
                        db: &db,
                        custody: custody.as_ref(),
                        daemon: daemon.as_ref(),
                        network: Network::Mainnet,
                        tenants: &tenants,
                        reorg_check_depth: scan.reorg_check_depth,
                        grace_period_seconds: scan.expired_order_grace_period_seconds,
                        scan_chunk_memory_budget_mb: scan.scan_chunk_memory_budget_mb,
                    };
                    engine::work::run_round(&memory, &inputs, memory.tuning().round_budget)
                        .await
                        .into_result()
                })
                .await
                .map_err(|error| format!("tick task failed: {error}"))?
                .map_err(|error| format!("{error:?}"))
            }
        }
    }
}

/// Network and tenant progress, read over the fixture's own connection.
struct Progress {
    highwater: u64,
    lagging: u64,
    min_cursor: u64,
    progressed: u64,
}

fn progress(conn: &rusqlite::Connection, progressed_through: u64) -> rusqlite::Result<Progress> {
    let highwater: i64 = conn.query_row(
        "SELECT COALESCE(MAX(height), 0) FROM scanned_blocks WHERE network = ?1",
        [NETWORK],
        |row| row.get(0),
    )?;
    let (lagging, min_cursor, progressed): (i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*) FILTER (WHERE COALESCE(scanned_through_height, 0) < ?2),
                COALESCE(MIN(COALESCE(scanned_through_height, 0)), 0),
                COUNT(*) FILTER (WHERE COALESCE(scanned_through_height, 0) >= ?3)
         FROM tenants WHERE network = ?1 AND disabled_at_utc IS NULL",
        rusqlite::params![NETWORK, highwater, progressed_through as i64],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let count = |n: i64| n.max(0) as u64;
    Ok(Progress {
        highwater: count(highwater),
        lagging: count(lagging),
        min_cursor: count(min_cursor),
        progressed: count(progressed),
    })
}

async fn fixture() -> Result<(), Box<dyn Error>> {
    let args = Args(std::env::args().collect());
    let db_path: String = args.required("--db")?;
    let driver = Driver::parse(args.value("--driver").unwrap_or("scheduler"))?;
    let tenants: usize = args.required("--tenants")?;
    let orders: usize = args.required("--orders")?;
    let large_window_orders: usize = args.required("--large-window-orders")?;
    let warmup: usize = args.required("--warmup")?;
    let measured: usize = args.required("--measured")?;
    let drain: usize = args.required("--drain")?;
    let seed: u64 = args.required("--seed")?;
    let background_readers: usize = args.required("--background-readers")?;
    let background_http_readers: usize = args.required("--background-http-readers")?;
    let background_writers: usize = args.required("--background-writers")?;
    let write_lock_ms: u64 = args.required("--write-lock-ms")?;
    let write_lock_until_tick = match args.optional("--write-lock-until-tick")? {
        0 => u64::MAX,
        ticks => ticks,
    };
    let rpc_delay_ms = args.optional("--rpc-delay-ms")?;
    let rpc_fail_until_height = args.optional("--rpc-fail-until-height")?;
    let rpc_fail_every = args.optional("--rpc-fail-every")?;
    let custody_slots = args.optional("--custody-slots")?;
    let custody_delay_ms = args.optional("--custody-delay-ms")?;

    // Setup: tenants, keys and orders, with stable ids so every run of a
    // scenario builds the same database.
    let store = Store::open_file(&db_path)?;
    let fixture_sql = rusqlite::Connection::open(&db_path)?;
    fixture_sql.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")?;
    let plain_custody = Arc::new(PlainKeyCustody::default());
    let custody_probe = (custody_slots > 0).then(|| {
        Arc::new(SaturatedCustody {
            inner: Arc::clone(&plain_custody),
            slots: tokio::sync::Semaphore::new(custody_slots as usize),
            delay: Duration::from_millis(custody_delay_ms),
            max_wait_us: AtomicU64::new(0),
            completed: AtomicU64::new(0),
        })
    });
    let custody: Arc<dyn KeyCustody> = custody_probe
        .as_ref()
        .map(|probe| -> Arc<dyn KeyCustody> { Arc::<SaturatedCustody>::clone(probe) })
        .unwrap_or(plain_custody);
    let mut handles = HashMap::with_capacity(tenants);
    let now = engine::now_unix();
    for index in 0..tenants {
        let keys = stress_common::material(seed, index)?;
        let handle = custody.register_wallet(keys.clone()).await?;
        let sealed = custody.seal(&keys).await?;
        let address = custody
            .derive_subaddress(handle, SubaddressIndex::default(), Network::Mainnet)
            .await?;
        let created = store.create_tenant(
            &NewTenant {
                key_custody_backend: "plain".into(),
                sealed_key_material: sealed,
                primary_address: address.to_string(),
                network: NETWORK.into(),
                confirmations_required: Some(10),
                order_expiry_seconds: Some(3600),
            },
            now - 10,
        )?;
        let id = engine::store::TenantId::new(format!("tn_stress_{index:08}"));
        fixture_sql.execute(
            "UPDATE tenants SET id = ?1 WHERE id = ?2",
            (&id, &created.tenant.id),
        )?;
        handles.insert(id.clone(), handle);
        let tenant_orders = if index == 0 {
            orders.max(large_window_orders)
        } else {
            orders
        };
        for order_index in 0..tenant_orders {
            let minor = store.allocate_minor_index(&shared::ids::TenantId::new(id.to_string()))?;
            let subaddress = custody
                .derive_subaddress(
                    handle,
                    SubaddressIndex { major: 0, minor },
                    Network::Mainnet,
                )
                .await?;
            let created_order = store.create_order(&NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: id.clone(),
                merchant_order_id: None,
                minor_index: minor,
                address: subaddress.to_string(),
                xmr_amount_piconero: 1,
                description: None,
                created_at: now - 10,
                expires_at: now + 3600,
            })?;
            let order_id = format!("order_stress_{index:08}_{order_index:04}");
            fixture_sql.execute(
                "UPDATE orders SET id = ?1 WHERE id = ?2",
                (&order_id, &created_order.id),
            )?;
        }
    }
    drop(fixture_sql);
    let tx = stress_common::fixture_tx()?;
    let daemon = Arc::new(FixtureDaemon {
        height: AtomicU64::new(0),
        txid: stress_common::txid(&tx),
        tx,
        rpc_delay_ms,
        rpc_fail_until_height,
        rpc_fail_every,
        rpc_calls: AtomicU64::new(0),
        rpc_failures: AtomicU64::new(0),
    });

    // The engine's connections, as `main.rs` opens them: the database
    // worker (its own connection) for scanning, the shared store for API
    // writes, and a read pool for HTTP.
    let db = Db::open(&db_path, &store)?;
    let store = store.into_shared();
    let reader_pool = ReadStorePool::open(&db_path, background_readers.max(1))?;
    let background_stop = Arc::new(AtomicBool::new(false));
    let reads_done = Arc::new(AtomicU64::new(0));
    let writes_done = Arc::new(AtomicU64::new(0));
    let read_max_us = Arc::new(AtomicU64::new(0));
    let write_max_us = Arc::new(AtomicU64::new(0));
    let mut background_tasks = Vec::new();
    for _ in 0..background_readers {
        let (pool, stop, completed, maximum) = (
            reader_pool.clone(),
            Arc::clone(&background_stop),
            Arc::clone(&reads_done),
            Arc::clone(&read_max_us),
        );
        background_tasks.push(tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                if pool.query(Store::count_tenants).await.is_ok() {
                    completed.fetch_add(1, Ordering::Relaxed);
                }
                maximum.fetch_max(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }));
    }
    // Admin setting writes take the same path an API write does: the
    // database worker's admin queue.
    for worker in 0..background_writers {
        let (db, stop, completed, maximum) = (
            db.clone(),
            Arc::clone(&background_stop),
            Arc::clone(&writes_done),
            Arc::clone(&write_max_us),
        );
        background_tasks.push(tokio::spawn(async move {
            let mut sequence = 0u64;
            while !stop.load(Ordering::Relaxed) {
                sequence += 1;
                let started = Instant::now();
                let (key, value) = (format!("stress.admin.{worker}"), sequence.to_string());
                let written = db
                    .run(engine::store::db::Class::Admin, move |s| {
                        s.set_setting(&key, &value)
                    })
                    .await;
                if written.is_ok() {
                    completed.fetch_add(1, Ordering::Relaxed);
                }
                maximum.fetch_max(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }));
    }
    let handles = Arc::new(RwLock::new(handles));
    let app_state = AppState {
        db: engine::store::Database::from_parts(db.clone(), reader_pool.clone(), &store.lock()),
        admin_rate_limiter: Arc::new(RateLimiter::new(100_000)),
        log_store: None,
        engine_token: Arc::new(shared::auth::RawToken::presented(FIXTURE_ENGINE_TOKEN).hash()),
        settings: EngineSettings::defaults(),
        custody: engine::http::Custody {
            backends: Arc::clone(&custody),
            default_backend: "plain".into(),
            wallet_handles: Arc::clone(&handles),
        },
        networks: engine::http::Networks {
            daemons: Daemons::fixed(HashMap::from([(
                Network::Mainnet,
                Arc::new(FallbackDaemonClient::new(vec![FallbackNode {
                    label: "fixture".into(),
                    client: Arc::<FixtureDaemon>::clone(&daemon),
                }])),
            )])),
            scanner_status: new_scanner_status_map(),
        },
    };
    let http = build_router(app_state, 1_000_000);
    let http_done = Arc::new(AtomicU64::new(0));
    let http_max_us = Arc::new(AtomicU64::new(0));
    for _ in 0..background_http_readers {
        let (router, stop, completed, maximum) = (
            http.clone(),
            Arc::clone(&background_stop),
            Arc::clone(&http_done),
            Arc::clone(&http_max_us),
        );
        background_tasks.push(tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                let mut request = Request::new(Body::empty());
                *request.uri_mut() = axum::http::Uri::from_static("/status");
                request.headers_mut().insert(
                    shared::auth::ENGINE_TOKEN_HEADER,
                    axum::http::HeaderValue::from_static(FIXTURE_ENGINE_TOKEN),
                );
                if router
                    .clone()
                    .oneshot(request)
                    .await
                    .is_ok_and(|response| response.status().is_success())
                {
                    completed.fetch_add(1, Ordering::Relaxed);
                }
                maximum.fetch_max(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }));
    }
    // How late a 10 ms timer fires: a stalled runtime worker shows up here.
    let heartbeat_stop = Arc::new(AtomicBool::new(false));
    let heartbeat_max_us = Arc::new(AtomicU64::new(0));
    let heartbeat = {
        let (stop, max_delay) = (Arc::clone(&heartbeat_stop), Arc::clone(&heartbeat_max_us));
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let deadline = Instant::now() + Duration::from_millis(10);
                tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                max_delay.fetch_max(
                    Instant::now()
                        .saturating_duration_since(deadline)
                        .as_micros() as u64,
                    Ordering::Relaxed,
                );
            }
        })
    };

    let engine = Engine {
        driver,
        db: db.clone(),
        custody: Arc::clone(&custody),
        daemon: Arc::clone(&daemon),
        handles,
        memory: Arc::new(engine::work::ScanState::default()),
    };
    let observer = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    observer.busy_timeout(Duration::from_secs(5))?;
    let total = warmup + measured + drain;
    let mut points = Vec::new();
    for tick in 0..total {
        let blocker = if write_lock_ms > 0 && (tick as u64) < write_lock_until_tick {
            let conn = rusqlite::Connection::open(&db_path)?;
            conn.busy_timeout(Duration::from_secs(5))?;
            conn.execute_batch("BEGIN IMMEDIATE")?;
            Some(std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(write_lock_ms));
                conn.execute_batch("COMMIT")
            }))
        } else {
            None
        };
        daemon.height.store((tick + 1) as u64, Ordering::Relaxed);
        let started = Instant::now();
        let outcome = engine.tick().await;
        if let Some(blocker) = blocker {
            blocker.join().map_err(|panic| {
                let message = panic
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("no message");
                std::io::Error::other(format!("database contention thread panicked: {message}"))
            })??;
        }
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let snapshot = progress(&observer, 2)?;
        let phase = if tick < warmup {
            "warmup"
        } else if tick < warmup + measured {
            "measured"
        } else {
            "drain"
        };
        points.push(json!({"tick":tick,"phase":phase,"duration_ms":elapsed_ms,"highwater":snapshot.highwater,
            "lagging_tenants":snapshot.lagging,"oldest_lag_blocks":snapshot.highwater.saturating_sub(snapshot.min_cursor),
            "min_tenant_cursor":snapshot.min_cursor,"progressed_tenants":snapshot.progressed,
            "ok":outcome.is_ok(),"error":outcome.err()}));
    }
    heartbeat_stop.store(true, Ordering::Relaxed);
    let _ = heartbeat.await;
    background_stop.store(true, Ordering::Relaxed);
    for task in background_tasks {
        let _ = task.await;
    }
    let measured_ms: u64 = points
        .iter()
        .filter(|p| p["phase"] == "measured")
        .filter_map(|p| p["duration_ms"].as_u64())
        .sum();
    let wal_bytes = std::fs::metadata(format!("{db_path}-wal")).map_or(0, |m| m.len());
    let checkpoint = rusqlite::Connection::open(&db_path)?;
    let (checkpoint_busy, wal_log_pages, wal_checkpointed_pages): (i64, i64, i64) = checkpoint
        .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let db_metrics = db.metrics();
    let result = json!({"schema_version":SCHEMA_VERSION,
        "db_write_queue_wait_max_us":db_metrics.max_queue_wait_us,"db_write_query_max_us":db_metrics.max_run_us,
        "db_jobs_completed":db_metrics.completed,"driver":driver.name(),"seed":seed,"tenants":tenants,
        "orders_per_tenant":orders,"large_window_orders":large_window_orders,
        "background_readers":background_readers,"background_admin_writers":background_writers,
        "background_http_readers":background_http_readers,
        "background_http_reads_completed":http_done.load(Ordering::Relaxed),
        "http_max_latency_us":http_max_us.load(Ordering::Relaxed),
        "write_lock_hold_ms_per_tick":write_lock_ms,"write_lock_until_tick":write_lock_until_tick,
        "rpc_delay_ms":rpc_delay_ms,"rpc_fail_until_height":rpc_fail_until_height,"rpc_fail_every":rpc_fail_every,
        "custody_slots":custody_slots,"custody_delay_ms":custody_delay_ms,
        "custody_scans_completed":custody_probe.as_ref().map_or(0, |p| p.completed.load(Ordering::Relaxed)),
        "custody_max_wait_us":custody_probe.as_ref().map_or(0, |p| p.max_wait_us.load(Ordering::Relaxed)),
        "rpc_calls":daemon.rpc_calls.load(Ordering::Relaxed),"rpc_failures":daemon.rpc_failures.load(Ordering::Relaxed),
        "background_reads_completed":reads_done.load(Ordering::Relaxed),
        "background_writes_completed":writes_done.load(Ordering::Relaxed),
        "read_max_latency_us":read_max_us.load(Ordering::Relaxed),
        "write_max_latency_us":write_max_us.load(Ordering::Relaxed),
        "warmup_ticks":warmup,"measured_ticks":measured,"drain_ticks":drain,
        "measured_duration_ms":measured_ms,"timer_max_delay_us":heartbeat_max_us.load(Ordering::Relaxed),
        "points":points,"sqlite_version":rusqlite::version(),
        "database_bytes":std::fs::metadata(&db_path)?.len(),
        "wal_bytes":wal_bytes,"checkpoint_busy":checkpoint_busy,
        "wal_log_pages":wal_log_pages,"wal_checkpointed_pages":wal_checkpointed_pages});
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn main() {
    if std::env::args().any(|arg| arg == "--version") {
        println!("{}", rusqlite::version());
        return;
    }
    // Two workers, as `server.worker_threads` defaults to; the xtask pins
    // the whole process to one CPU.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build();
    let result = match runtime {
        Ok(runtime) => runtime.block_on(fixture()),
        Err(error) => Err(error.into()),
    };
    if let Err(error) = result {
        eprintln!("stress fixture: {error}");
        std::process::exit(1);
    }
}
