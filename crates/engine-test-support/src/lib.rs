//! Test-only helper for standing up a real, network-bound `engine`
//! engine instance for cross-crate integration tests. See WBS 0.6
//! (`docs/WOOCOMMERCE_WBS.md`) for the outcome this is meant to satisfy.
//!
//! ## Why this is its own crate, not `shared`
//!
//! `shared` is a *dependency of* `engine` (`shared::auth`,
//! `shared::password`, `shared::migrations` are all
//! pulled in by the engine crate). A helper that boots a real
//! `engine` engine instance necessarily needs `engine` itself
//! as a dependency - even as a dev-dependency, that would make `shared`
//! depend (for its own test/dev build) on a crate that depends on `shared`,
//! i.e. `scanner -> shared -> scanner`. Cargo rejects dependency
//! cycles like this outright (a dev-dependency edge back onto a crate that is
//! itself depended on, directly or transitively, still forms a cycle in the
//! resolved dependency graph), so this genuinely cannot live in `shared`.
//!
//! Instead it lives in its own crate, layered *above* both:
//! `engine-test-support -> scanner -> shared`. That's not a
//! deviation from the WBS - 0.6 explicitly hedges with "a small helper (in
//! `shared`, or a dev-only sibling crate)", anticipating exactly this.
//!
//! Any workspace crate that needs a real, bound engine for its own tests
//! (`mock-woocommerce`, `monokulo`, ...) adds this as a
//! `[dev-dependencies]` entry. Because it is only ever reached that way, this
//! crate needs no `#[cfg(test)]`/feature gate of its own to stay out of
//! release builds the way it would if it lived inside `shared` - a
//! dev-dependency is never linked into a normal (non-test, non-dev) build of
//! whatever depends on it, by cargo's own rules.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use engine::daemon::{
    ChainBlock, DaemonError, FetchedTx, KeyImageStatus, MoneroDaemonClient, TxLocation,
};
use engine::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use engine::http::rate_limit::RateLimiter;
use engine::http::{build_router, AppState};
use engine::key_custody::{KeyCustody, PlainKeyCustody, WalletHandle};
use engine::network::network_str;
use engine::scanner::run_scan_tick;
use engine::store::Store;
use monero::Network;

/// The engine token every engine this crate spawns accepts: what a
/// test gives monokulo's `EngineClient` (or sends in
/// `shared::auth::ENGINE_TOKEN_HEADER`) to reach it.
pub use engine::http::TEST_ENGINE_TOKEN;

/// A plain HTTP client for calling a test engine directly, as monokulo
/// does: it sends [`TEST_ENGINE_TOKEN`] on every request.
pub fn engine_http_client() -> reqwest::Client {
    let headers = reqwest::header::HeaderMap::from_iter([(
        reqwest::header::HeaderName::from_static(shared::auth::ENGINE_TOKEN_HEADER),
        reqwest::header::HeaderValue::from_static(TEST_ENGINE_TOKEN),
    )]);
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("a client with one static header builds")
}

/// How often the background loops (see [`TestEngineConfig::with_background_loops`])
/// re-run, when enabled. Real deployments poll on the order of seconds (see
/// `main.rs`'s `mempool_poll_interval_ms`) - a test
/// harness can afford to poll far more aggressively than that so a test forcing a
/// real event (e.g. an order crossing its `expires_at`) doesn't have to wait long for
/// it to actually happen.
const BACKGROUND_LOOP_INTERVAL: Duration = Duration::from_millis(150);

/// A `MoneroDaemonClient` that does the least possible to let [`run_scan_tick`] run
/// at all, for callers that only need its status-recompute sweep (`docs/DESIGN.md`
/// §7.6 - confirmation growth and, especially, expiry) rather than genuine chain
/// scanning for payment matches.
///
/// This is deliberately *not* `engine::daemon::fake::FakeDaemonClient`: that
/// type is `#[cfg(test)]`-gated inside `engine` itself, which means it is
/// only compiled during the engine crate's *own* test builds - a dev-dependency such
/// as this crate never sees it, cycle or not. Implementing the (ungated, public)
/// `MoneroDaemonClient` trait fresh here needs no change to the engine crate at all;
/// see this crate's own module-level doc comment section on why that matters for
/// WBS 1.4.4's background-loop harness specifically.
///
/// Always reports height 0 and a deterministic, height-keyed block hash - never
/// advancing, never disagreeing with itself between calls - so `run_scan_tick`'s
/// reorg-reconciliation logic never has anything to react to. Every block/mempool
/// query returns empty. None of this matters for `run_scan_tick`'s expiry sweep
/// (`due_order_ids`/`recompute_and_notify`), which is driven by wall-clock
/// time and the store alone, not by anything this daemon reports - see
/// `TestEngineConfig::with_background_loops`'s own doc comment for the full
/// reasoning on why an inert daemon is sufficient here.
struct NoopDaemonClient;

/// The id of a whole transaction.
fn tx_id_hex(tx: &monero::Transaction) -> String {
    use monero::cryptonote::hash::Hashable;
    hex::encode(tx.hash().to_bytes())
}

/// The block of a chain that is its genesis block alone, named
/// `{prefix}-0` and empty, if the run asked for starts there.
fn empty_blocks(prefix: &str, start_height: u64, count: u64) -> Vec<ChainBlock> {
    if start_height != 0 || count == 0 {
        return Vec::new();
    }
    vec![ChainBlock {
        height: 0,
        hash: format!("{prefix}-0"),
        prev_hash: String::new(),
        timestamp: 0,
        txs: vec![],
        txids: vec![],
        wire_bytes: 0,
    }]
}

#[async_trait::async_trait]
impl MoneroDaemonClient for NoopDaemonClient {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        Ok(0)
    }

    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        Ok(format!("noop-block-{height}"))
    }

    async fn get_chain_blocks(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainBlock>, DaemonError> {
        Ok(empty_blocks("noop-block", start_height, count))
    }

    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        Ok(vec![])
    }

    async fn get_transactions_with_ids(
        &self,
        _txids: &[String],
    ) -> Result<Vec<FetchedTx>, DaemonError> {
        Ok(vec![])
    }

    async fn locate_transaction(&self, _txid: &str) -> Result<TxLocation, DaemonError> {
        Ok(TxLocation::NotFound)
    }

    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        Ok(vec![KeyImageStatus::Unspent; key_images.len()])
    }
}

/// The node the engine's admin payment lookup asks when
/// [`TestEngineConfig::with_admin_lookup_daemon`] is used: an empty chain
/// whose mempool holds whatever [`TestEngineHandle::add_mempool_transaction`]
/// put there, so a merchant's "look up this txid" can find a real
/// transaction.
#[derive(Default)]
struct LookupDaemonClient {
    mempool: Arc<parking_lot::Mutex<Vec<monero::Transaction>>>,
}

impl LookupDaemonClient {
    fn find(&self, txid: &str) -> Option<monero::Transaction> {
        self.mempool
            .lock()
            .iter()
            .find(|tx| tx_id_hex(tx) == txid)
            .cloned()
    }
}

#[async_trait::async_trait]
impl MoneroDaemonClient for LookupDaemonClient {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        Ok(0)
    }

    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        Ok(format!("lookup-block-{height}"))
    }

    async fn get_chain_blocks(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainBlock>, DaemonError> {
        Ok(empty_blocks("lookup-block", start_height, count))
    }

    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        Ok(self.mempool.lock().iter().map(tx_id_hex).collect())
    }

    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<FetchedTx>, DaemonError> {
        Ok(txids
            .iter()
            .filter_map(|txid| {
                self.find(txid).map(|tx| FetchedTx {
                    txid: txid.clone(),
                    tx,
                })
            })
            .collect())
    }

    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        Ok(if self.find(txid).is_some() {
            TxLocation::InPool
        } else {
            TxLocation::NotFound
        })
    }

    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        Ok(vec![KeyImageStatus::Unspent; key_images.len()])
    }
}

/// Matches the order of magnitude `main.rs` and `tests/e2e_stagenet.rs` use;
/// nothing a test sends against this harness should come close to it.
const MAX_BODY_BYTES: usize = 1_000_000;

/// The variable that turns test logging on: a `tracing` filter, such as
/// `debug` or `engine=debug,monokulo=info,hyper=warn`.
pub const TEST_LOG_VAR: &str = "MONOKULO_TEST_LOG";

/// Logs from everything under test (the engine, monokulo, the harness) to
/// standard error, when [`TEST_LOG_VAR`] is set: for diagnosing a test that
/// fails or hangs (`MONOKULO_TEST_LOG=debug cargo test -p <crate> <test>`).
/// Does nothing otherwise, and nothing after the first call or when another
/// subscriber is already installed (a test capturing logs of its own).
///
/// Written straight to standard error rather than through the test
/// harness's capture, so a test that never finishes still shows how far it
/// got. Every test engine calls it as it starts ([`TestEngineConfig::spawn`]);
/// a test that starts no engine can call it itself.
pub fn init_test_logging() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Ok(filter) = std::env::var(TEST_LOG_VAR) else {
            return;
        };
        let filter = tracing_subscriber::EnvFilter::try_new(&filter)
            .unwrap_or_else(|e| panic!("{TEST_LOG_VAR}={filter:?} is not a tracing filter: {e}"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_thread_names(true)
            .try_init();
    });
}

/// A running, real (network-bound) `engine` engine instance started by
/// [`spawn_test_engine`], live for as long as this handle is held.
pub struct TestEngineHandle {
    /// The real local address the engine is listening on. Build requests
    /// against `format!("http://{addr}")` with a genuine `reqwest::Client`.
    pub addr: SocketAddr,
    /// The same engine's admin API as a router, for a client that calls it
    /// in-process (`EngineClient::embedded`, docs/engine_as_library.md)
    /// instead of over `addr`. Both reach one engine.
    router: axum::Router,
    server_task: tokio::task::JoinHandle<()>,
    /// The scanner-tick background loop, present only when
    /// [`TestEngineConfig::with_background_loops`] was used. Empty otherwise, so
    /// `Drop` has nothing extra to abort for every other caller (the overwhelming
    /// majority of this crate's existing use, which never touches the scanner at
    /// all).
    background_tasks: Vec<tokio::task::JoinHandle<()>>,
    /// This engine's own store/key-custody/wallet-handles registry - kept so
    /// [`TestEngineHandle::run_scan_tick_now`] can drive a real, one-off scan tick
    /// against them directly. See that method's own doc comment for why a caller
    /// might want this instead of (or alongside) `with_background_loops`'s automatic
    /// interval.
    store: engine::store::SharedStore,
    key_custody: Arc<dyn KeyCustody>,
    wallet_handles: Arc<RwLock<HashMap<engine::store::TenantId, WalletHandle>>>,
    /// How many tenant admin API requests (`/api/v1/admin/tenant/...`) this
    /// engine has received - the requests its per-token rate limit counts.
    tenant_requests: Arc<std::sync::atomic::AtomicUsize>,
    /// The admin lookup node's mempool (see [`LookupDaemonClient`]).
    lookup_mempool: Arc<parking_lot::Mutex<Vec<monero::Transaction>>>,
    /// What the engine's `/status` and activity API report per network.
    scanner_status: engine::scanner_status::ScannerStatusMap,
    /// With [`TestEngineConfig::with_snp_backend`]: the key the stand-in
    /// security processor signs its reports with.
    snp_vcek: Option<p384::ecdsa::VerifyingKey>,
}

/// The ID key digest and security version the snp test backend's stand-in
/// guest reports, and so what a client checking its bundles trusts.
pub fn snp_test_trust() -> engine::key_custody::transport::TrustPolicy {
    engine::key_custody::transport::TrustPolicy {
        id_key_digest: snp_attest::guest::TestIdentity::default().id_key_digest,
        min_guest_svn: 0,
        min_tcb: engine::key_custody::transport::TcbFloor::default(),
    }
}

impl TestEngineHandle {
    /// What a merchant's client does with a bundle from this engine's snp
    /// backend (`with_snp_backend`): checks it, against the stand-in
    /// security processor's key in place of AMD's chain, and encrypts the
    /// keys to it. Returns the envelope as the text a form submits.
    pub fn seal_keys_for_snp(
        &self,
        bundle: &engine::key_custody::transport::Bundle,
        view_key_hex: &str,
        spend_pubkey_hex: &str,
    ) -> String {
        use engine::key_custody::transport;
        let vcek = self.snp_vcek.expect("spawned with_snp_backend");
        let verified = transport::verify_bundle(
            bundle,
            &snp_test_trust(),
            &transport::Anchor::Vcek(vcek),
            shared::time::now_unix(),
        )
        .expect("the test engine's bundle checks out");
        let mut keys = hex::decode(view_key_hex).unwrap();
        keys.extend(hex::decode(spend_pubkey_hex).unwrap());
        transport::seal(&verified, &keys).unwrap().to_text()
    }
}

/// AMD's certificates, as far as the snp test backend needs them: present.
/// Its bundles are checked against the stand-in's key, not AMD's chain.
#[cfg(feature = "snp")]
struct TestEvidence;

#[cfg(feature = "snp")]
#[async_trait::async_trait]
impl engine::key_custody::snp::EvidenceSource for TestEvidence {
    async fn evidence(
        &self,
        _product: snp_attest::report::Product,
        _report: &snp_attest::report::AttestationReport,
    ) -> Result<snp_attest::verify::Evidence, String> {
        Ok(snp_attest::verify::Evidence {
            ask_der: Vec::new(),
            vcek_der: Vec::new(),
            crl_der: Vec::new(),
        })
    }
}

/// `plain` and `snp` behind a router, `snp` on a stand-in security
/// processor; the router's default is `plain`. With the slot, for the
/// engine's reloads, and the key the stand-in signs its reports with.
#[cfg(feature = "snp")]
async fn snp_custody(
    store: &engine::store::SharedStore,
) -> (
    Arc<dyn KeyCustody>,
    Arc<engine::key_custody::SnpSlot>,
    p384::ecdsa::VerifyingKey,
) {
    let guest =
        snp_attest::guest::TestGuest::new([7; 32], snp_attest::guest::TestIdentity::default());
    let vcek = guest.vcek();
    let slot = Arc::new(
        engine::key_custody::SnpSlot::new(
            Ok(engine::key_custody::snp::SnpConfig {
                product: snp_attest::report::Product::Genoa,
                trust: snp_test_trust(),
            }),
            Arc::new(guest),
            Arc::new(engine::key_custody::StoreWraps(store.clone())),
        )
        .with_anchor(engine::key_custody::transport::Anchor::Vcek(vcek)),
    );
    let snp = slot.start().expect("the snp test backend starts");
    snp.refresh_evidence(&TestEvidence)
        .await
        .expect("test evidence");
    let backends: HashMap<String, Arc<dyn KeyCustody>> = HashMap::from([
        (
            "plain".to_string(),
            Arc::new(PlainKeyCustody::default()) as Arc<dyn KeyCustody>,
        ),
        ("snp".to_string(), snp as Arc<dyn KeyCustody>),
    ]);
    (
        Arc::new(engine::key_custody::CustodyRouter::new(backends, "plain")),
        slot,
        vcek,
    )
}

impl TestEngineHandle {
    /// This engine's admin API as a router, as an embedded engine hands it
    /// to monokulo: what `addr` serves over HTTP, called in-process.
    pub fn router(&self) -> axum::Router {
        self.router.clone()
    }

    /// `network`'s activity record, which the engine serves at
    /// `/api/v1/admin/engine/activity` (`docs/engine_visualizer.md`): a test
    /// records events into it to see them reach monokulo's engine page.
    pub fn activity(&self, network: Network) -> Arc<engine::activity::Activity> {
        engine::scanner_status::activity_of(&self.scanner_status, network)
    }

    /// Runs exactly one real `run_scan_tick` against this engine's own store/
    /// key-custody and its *current* `wallet_handles` registry (re-read fresh on
    /// every call, same as the background loop) - for a caller that wants precise,
    /// foreground control over exactly when a real scan happens against a real
    /// `daemon`, rather than relying on `with_background_loops`'s own automatic
    /// interval.
    ///
    /// This matters beyond convenience: observed directly while building WBS 1.4.5's
    /// real stagenet connect-flow test, running `with_background_loops` *with* a real
    /// per-network daemon continuously ticking in the background, concurrently with
    /// that same test's own foreground use of a real daemon (connecting a spend
    /// wallet, building/broadcasting a transaction), caused real, consistent
    /// connection failures against the public node - most likely a modest
    /// concurrent-connections-per-IP limit on that node's own end being exceeded by
    /// two independent, simultaneously-active real daemon clients. A single caller
    /// driving scan ticks itself, sequentially, with the *same* daemon client it
    /// already uses for everything else real-network-related (never two clients
    /// racing each other against the same real node at once) avoided the problem
    /// entirely - see that test for the actual pattern.
    pub async fn run_scan_tick_now(
        &self,
        daemon: &dyn MoneroDaemonClient,
        network: Network,
        reorg_check_depth: u64,
    ) -> Result<(), engine::scanner::ScannerError> {
        let tenants: Vec<(engine::store::TenantId, WalletHandle)> = self
            .wallet_handles
            .read()
            .iter()
            .map(|(id, h)| (id.clone(), *h))
            .collect();
        run_scan_tick(
            &self.store,
            self.key_custody.as_ref(),
            daemon,
            network_str(network),
            &tenants,
            reorg_check_depth,
            0, // no grace period - this helper's own callers don't test expiry timing
        )
        .await
    }

    /// Registers every store on `network` that has no live handle, as the
    /// scan loop does on its own (within seconds of a handle being lost).
    pub async fn register_missing_wallets_now(&self, network: &str) -> usize {
        engine::scanner::register_missing_wallets_checking_state(
            &self.store,
            self.key_custody.as_ref(),
            &self.wallet_handles,
            None,
            shared::network::parse_network(network).unwrap(),
        )
        .await
    }

    /// This engine's own real, live `SharedStore` - for a caller that needs to
    /// force state a plain HTTP call against the engine can't reach directly
    /// (e.g. `docs/order_rescan_wbs.md` Phase 3's own tests driving an order
    /// straight to `Expired` via `recompute_order_status` against a `now` past
    /// its deadline, rather than waiting out a real 30-minute default expiry).
    /// Same "give the test real, direct access rather than a narrower purpose-
    /// built method per caller" reasoning as `run_scan_tick_now` above.
    pub fn store(&self) -> &engine::store::SharedStore {
        &self.store
    }

    /// Puts `tx` in the mempool of the node the admin payment lookup asks
    /// (only with [`TestEngineConfig::with_admin_lookup_daemon`]): a
    /// transaction a customer has just sent.
    pub fn add_mempool_transaction(&self, tx: monero::Transaction) {
        self.lookup_mempool.lock().push(tx);
    }

    /// Tenant admin API requests received so far, including rate-limited
    /// ones - for a caller asserting how many engine requests its own work
    /// costs against the engine's per-store rate limit.
    pub fn tenant_request_count(&self) -> usize {
        self.tenant_requests
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Pays `order_id` in full, the way a real scan would record it: a
    /// synthetic confirmed payment of exactly the order's amount, then the
    /// same status recompute a scan tick does
    /// (`engine::scanner::recompute_and_notify`), so the order reads `paid`
    /// and an `order.paid` event is written to the order-event log, which
    /// monokulo delivers to the store's webhooks. For end-to-end tests that
    /// need a paid order without a real chain.
    ///
    /// The payment is recorded at height 1000 and the recompute runs as if
    /// the chain tip were 100 blocks later, comfortably past any
    /// confirmation requirement a test would configure.
    pub fn mark_order_paid(&self, order_id: &str) -> Result<(), engine::store::StoreError> {
        let amount = {
            let store = self.store.lock();
            let tenant_id = store
                .get_order_tenant_id(&engine::store::OrderId::new(order_id.to_string()))?
                .ok_or(engine::store::StoreError::NotFound)?;
            store
                .get_order(
                    &tenant_id,
                    &engine::store::OrderId::new(order_id.to_string()),
                )?
                .ok_or(engine::store::StoreError::NotFound)?
                .xmr_amount_piconero
        };
        self.record_order_payment(order_id, shared::xmr_amount::Piconero(amount), Some(101))
    }

    /// Mines every mempool-only payment to `order_id` into a block (height
    /// 1000, tip 100 blocks later), as a scan tick would on seeing them
    /// confirmed, and recomputes its status.
    pub fn confirm_order_payments(&self, order_id: &str) -> Result<(), engine::store::StoreError> {
        const PAYMENT_HEIGHT: i64 = 1000;
        let store = self.store.lock();
        let now = engine::now_unix();
        for payment in store
            .get_all_payments(&engine::store::OrderId::new(order_id.to_string()))?
            .into_iter()
            .filter(|p| p.block_height.is_none())
        {
            store.record_payment_match(
                &engine::store::OrderId::new(order_id.to_string()),
                &payment.txid,
                payment.output_index,
                payment.amount_piconero,
                &payment.key_images_json,
                payment.first_seen_at,
                Some(PAYMENT_HEIGHT),
                None,
            )?;
        }
        engine::scanner::recompute_and_notify(
            &store,
            &engine::store::OrderId::new(order_id.to_string()),
            PAYMENT_HEIGHT as u64 + 100,
            now,
        )
        .map_err(|e| match e {
            engine::scanner::ScannerError::Store(e) => e,
            other => panic!("recomputing a test order's status failed: {other}"),
        })
        .map(|_changed| ())
    }

    /// Records a payment of `piconero` to `order_id` the way a scan would,
    /// then recomputes its status. `confirmations: None` is a payment seen
    /// in the mempool only (`unconfirmed`); `Some(n)` puts it in a block
    /// (height 1000) with the chain tip giving it `n` confirmations. Less
    /// than the order's amount leaves it `partial`. Each call is a separate
    /// transaction.
    pub fn record_order_payment(
        &self,
        order_id: &str,
        piconero: shared::xmr_amount::Piconero,
        confirmations: Option<u64>,
    ) -> Result<(), engine::store::StoreError> {
        let piconero = piconero.get();
        const PAYMENT_HEIGHT: i64 = 1000;
        let store = self.store.lock();
        let now = engine::now_unix();
        let existing = store
            .get_all_payments(&engine::store::OrderId::new(order_id.to_string()))
            .map(|p| p.len())
            .unwrap_or(0);
        store.record_payment_match(
            &engine::store::OrderId::new(order_id.to_string()),
            &format!("test-payment-{order_id}-{existing}"),
            0,
            piconero,
            "[]",
            now,
            confirmations.map(|_| PAYMENT_HEIGHT),
            None,
        )?;
        let tip = PAYMENT_HEIGHT as u64 + confirmations.unwrap_or(1).max(1) - 1;
        engine::scanner::recompute_and_notify(
            &store,
            &engine::store::OrderId::new(order_id.to_string()),
            tip,
            now,
        )
        .map_err(|e| match e {
            engine::scanner::ScannerError::Store(e) => e,
            other => panic!("recomputing a test order's status failed: {other}"),
        })
        .map(|_changed| ())
    }
}

impl TestEngineHandle {
    /// Flags a double spend of `order_id`'s payment, as the scanner does when
    /// a key image it recorded turns up spent elsewhere.
    pub fn mark_order_double_spent(&self, order_id: &str) -> Result<(), engine::store::StoreError> {
        let store = self.store.lock();
        store.mark_double_spend_detected(
            &engine::store::OrderId::new(order_id.to_string()),
            engine::now_unix(),
        )?;
        Ok(())
    }

    /// Expires `order_id` the way a scan tick after its deadline would: the
    /// same status recompute, run as if the clock were past `expires_at`, so
    /// the order reads `expired` and an `order.expired` event is written.
    /// For tests of a customer who never pays.
    pub fn mark_order_expired(&self, order_id: &str) -> Result<(), engine::store::StoreError> {
        let store = self.store.lock();
        let tenant_id = store
            .get_order_tenant_id(&engine::store::OrderId::new(order_id.to_string()))?
            .ok_or(engine::store::StoreError::NotFound)?;
        let order = store
            .get_order(
                &tenant_id,
                &engine::store::OrderId::new(order_id.to_string()),
            )?
            .ok_or(engine::store::StoreError::NotFound)?;
        engine::scanner::recompute_and_notify(
            &store,
            &engine::store::OrderId::new(order_id.to_string()),
            1000,
            order.expires_at + 1,
        )
        .map_err(|e| match e {
            engine::scanner::ScannerError::Store(e) => e,
            other => panic!("recomputing a test order's status failed: {other}"),
        })
        .map(|_changed| ())
    }
}

impl Drop for TestEngineHandle {
    /// Aborts the background `axum::serve` task (and, if spawned, the scanner
    /// loop) so the port and tasks don't outlive the test. A hard
    /// `abort()` rather than a graceful shutdown handshake is deliberately simple and
    /// sufficient at this scale: each test gets its own ephemeral port and its own
    /// tasks, there are no persistent connections worth draining, and the in-memory
    /// `Store` behind it is dropped along with everything else once the handle goes
    /// out of scope.
    fn drop(&mut self) {
        self.server_task.abort();
        for task in &self.background_tasks {
            task.abort();
        }
    }
}

/// Configuration for spawning a test engine: which Monero networks are
/// configured (`state.configured_networks`). Added for WBS 1.3.3 as a
/// generalization of the narrower `spawn_test_engine_with_networks` added for
/// WBS 1.2.1 - rather than a third near-duplicate spawn function per new
/// dimension of test setup, [`spawn_test_engine`] and
/// [`spawn_test_engine_with_networks`] are now both thin wrappers over
/// [`TestEngineConfig::spawn`], with no change to either's signature or
/// behavior.
#[derive(Debug, Default, Clone)]
pub struct TestEngineConfig {
    networks: Vec<Network>,
    background_loops: bool,
    background_scan_loop: bool,
    /// `true` when [`TestEngineConfig::with_snp_backend`] has been used.
    #[cfg(feature = "snp")]
    snp_backend: bool,
    /// `true` when [`TestEngineConfig::with_admin_lookup_daemon`] has been used -
    /// see that method's own doc comment.
    admin_lookup_daemon: bool,
    /// `Some(n)` when [`TestEngineConfig::with_rate_limit`] has been used.
    rate_limit_per_minute: Option<u32>,
    /// Served by the engine's log API (`with_log_store`).
    log_store: Option<telemetry::store::LogStore>,
    /// `true` when [`TestEngineConfig::with_live_nodes`] has been used.
    live_nodes: bool,
    /// `true` when [`TestEngineConfig::embedded`] has been used.
    embedded: bool,
    /// The engine's options file, from [`TestEngineConfig::with_options`];
    /// an empty one in memory without it.
    options: Option<live_settings::OptionsFile>,
}

impl TestEngineConfig {
    /// Limits each tenant token to `per_minute` admin requests, like a
    /// production engine (whose default is 120), instead of this harness's
    /// effectively unlimited default.
    pub fn with_rate_limit(mut self, per_minute: u32) -> Self {
        self.rate_limit_per_minute = Some(per_minute);
        self
    }

    /// Serves `store` from the engine's log API (`GET /api/v1/admin/logs`),
    /// as a real engine serves its own `logs.db`.
    pub fn with_log_store(mut self, store: telemetry::store::LogStore) -> Self {
        self.log_store = Some(store);
        self
    }

    /// Loads the engine's settings as an engine inside monokulo does: the
    /// standalone-only ones (`server.bind`, `server.token`, `logging.*`) are
    /// left out of its settings API and refused if saved. For a test that
    /// reaches this engine in-process ([`TestEngineHandle::router`]) the way
    /// monokulo does by default.
    pub fn embedded(mut self) -> Self {
        self.embedded = true;
        self
    }

    /// Keeps the engine's settings in `options`: with
    /// `monokulo_file.scoped("engine")`, the `[engine.*]` tables of the
    /// options file monokulo keeps its own settings in, as monokulo's
    /// `main` hands the engine inside it.
    pub fn with_options(mut self, options: live_settings::OptionsFile) -> Self {
        self.options = Some(options);
        self
    }

    /// Starts from the same defaults `spawn_test_engine` has always used: no
    /// configured networks, no exchange rates.
    pub fn new() -> Self {
        TestEngineConfig::default()
    }

    /// Sets `configured_networks` to `networks` - see
    /// `spawn_test_engine_with_networks`'s doc comment for why a test
    /// needing a real tenant (via `create_tenant`) needs at least one.
    pub fn with_networks(mut self, networks: &[Network]) -> Self {
        self.networks = networks.to_vec();
        self
    }

    /// Wires an inert [`NoopDaemonClient`] into `AppState::daemons` (the live
    /// scanner's own map) for every configured network - opt-in, since most
    /// callers of this harness never need it (see `spawn`'s own doc comment on
    /// `daemons` for why an empty map is the honest default). A caller
    /// driving `POST /api/v1/admin/tenant/payments/lookup`
    /// (`docs/txid_lookup_and_scan_chunking_wbs.md` Part B) through a genuine
    /// HTTP round trip: that handler looks a daemon up from `daemons`
    /// unconditionally too. A `NoopDaemonClient` can resolve request-shape
    /// logic (a malformed txid, a genuinely-absent one) but never simulate a
    /// real match.
    pub fn with_admin_lookup_daemon(mut self) -> Self {
        self.admin_lookup_daemon = true;
        self
    }

    /// Opts into running the real scanner-tick loop in the background against
    /// the spawned engine's own store - `main.rs`'s `run_scanner_loop`, minus the
    /// supervisor restart
    /// wrapper (a test that panics here should fail loudly, not get quietly
    /// restarted) and on a much shorter interval (see [`BACKGROUND_LOOP_INTERVAL`]).
    ///
    /// This is what lets a caller force a *genuine* order event in a test
    /// (WBS 1.4.4/1.4.5): create a tenant with a short `order_expiry_seconds`, create
    /// an order against it, and simply wait - the real `run_scan_tick` recomputes
    /// every non-terminal order's status every tick regardless of whether anything
    /// was scanned (`docs/DESIGN.md` §7.6; confirmed directly against
    /// `src/scanner.rs`'s own
    /// `an_unpaid_order_past_its_deadline_becomes_expired_on_a_tick_that_matches_nothing`
    /// test, and against the fact that its non-terminal-order recompute sweep is keyed
    /// off `network` alone, not off the `tenants`/watchlist parameter that gates
    /// payment-matching), so an order past its deadline flips to `expired` and writes
    /// a real `order.expired` event to the order-event log purely from wall-clock
    /// time passing - no real Monero payment, node, or even a non-trivial
    /// `MoneroDaemonClient` required. Monokulo, subscribed to
    /// `GET /api/v1/admin/order-events`, delivers it to the store's webhooks.
    ///
    /// Chain scanning for payment *matches* is deliberately not exercised by this
    /// path: [`NoopDaemonClient`] is inert, and `run_scan_tick` is always called with
    /// an empty `tenants` list here (see `spawn`), so the active-watchlist
    /// intersection is empty and no wallet-scanning work ever happens - only the
    /// unconditional non-terminal-order recompute sweep runs. A caller that also
    /// needs genuine payment detection needs a real `MoneroDaemonClient` and wallet
    /// handles, neither of which this harness provides; that is out of scope for
    /// what this opt-in exists for.
    ///
    /// A caller that instead needs genuine payment-*matching* chain scanning against a
    /// real daemon (WBS 1.4.5's real stagenet connect-flow test) should drive that
    /// itself via [`TestEngineHandle::run_scan_tick_now`] rather than through this
    /// method - see that method's own doc comment for why a second, independently-
    /// ticking real daemon client running concurrently in the background turned out to
    /// be the wrong shape for that need (real connection contention against the same
    /// live node). This method's own scan-tick loop always uses the inert
    /// [`NoopDaemonClient`] (per the "chain scanning... deliberately not exercised"
    /// paragraph above) regardless of whether `run_scan_tick_now` is also in use -
    /// the two don't conflict since the `NoopDaemonClient` never touches the network.
    ///
    /// CORRECTION (found while debugging WBS 1.4.5's real stagenet test hanging for
    /// minutes on its very first `run_scan_tick_now` call, regardless of which public
    /// node it pointed at): the paragraph above is wrong about there being no
    /// conflict. `run_scan_tick`'s block-scan watermark (`max_scanned_height`/
    /// `set_scanned_block`) is keyed by `network` alone, in the same `Store` this
    /// loop's `NoopDaemonClient` tick shares with any real daemon later driven
    /// through `run_scan_tick_now` for that same network. `NoopDaemonClient::
    /// get_height` always reports `0`, so this loop's very first tick seeds
    /// `last_scanned = Some(0)` for the network - and a subsequent real-daemon call
    /// via `run_scan_tick_now` then computes `scan_range = (1, current_real_height)`
    /// and tries to fetch every block one at a time from 1 up to the real chain tip
    /// (millions of blocks on stagenet), which looks exactly like an indefinite hang:
    /// node-independent, low-CPU (blocked on sequential network round-trips), and
    /// unaffected by `reorg_check_depth`. See [`without_background_scan_loop`] for the
    /// fix a caller in that situation needs.
    ///
    /// [`without_background_scan_loop`]: TestEngineConfig::without_background_scan_loop
    pub fn with_background_loops(mut self) -> Self {
        self.background_loops = true;
        self.background_scan_loop = true;
        self
    }

    /// Drops the `NoopDaemonClient`-driven scan-tick loop of
    /// [`with_background_loops`] (leaving nothing in the background), for a caller
    /// that drives scanning
    /// itself against a real daemon via [`TestEngineHandle::run_scan_tick_now`] - see
    /// the correction on [`with_background_loops`]'s own doc comment for why running
    /// both against the same network poisons the real scan's watermark and makes it
    /// try to walk the entire real chain from block 1.
    ///
    /// [`with_background_loops`]: TestEngineConfig::with_background_loops
    pub fn without_background_scan_loop(mut self) -> Self {
        self.background_scan_loop = false;
        self
    }

    /// Applies saved `monero_node.<network>` settings to the engine's daemon
    /// clients, as a production engine does, instead of keeping this
    /// harness's fixed fakes: a saved node gets a real RPC client, a cleared
    /// network goes, and the settings API probes the saved nodes (and warns
    /// about networks stores use that no node answers for). Networks from
    /// [`with_networks`] keep their inert fake until the first node save.
    ///
    /// [`with_networks`]: TestEngineConfig::with_networks
    pub fn with_live_nodes(mut self) -> Self {
        self.live_nodes = true;
        self
    }

    /// Per-store key custody with two backends enabled: `plain` (the
    /// default) and `snp`, running on a stand-in security processor
    /// (`snp_attest::guest::TestGuest`), so stores can be created in either
    /// and moved between them. Keys for `snp` go in encrypted: get a bundle
    /// from the engine and seal them with
    /// [`TestEngineHandle::seal_keys_for_snp`]. Needs this crate's `snp`
    /// feature.
    #[cfg(feature = "snp")]
    pub fn with_snp_backend(mut self) -> Self {
        self.snp_backend = true;
        self
    }

    /// Boots a real `engine` engine - in-memory `Store`,
    /// `PlainKeyCustody` (or `plain` and `snp` behind a router, with
    /// [`TestEngineConfig::with_snp_backend`]), `configured_networks` from this
    /// config - bound to an OS-assigned ephemeral port on `127.0.0.1`, served
    /// in a background task. Returns only once the listener is actually
    /// bound, so the returned address is immediately connectable.
    ///
    /// This packages the same construction `tests/e2e_stagenet.rs` and
    /// `main.rs` use (`Store` -> `KeyCustody` -> `AppState` ->
    /// `build_router`), but binds a real `tokio::net::TcpListener` and drives
    /// it with `axum::serve` instead of exercising the router in-process via
    /// `tower::ServiceExt::oneshot` - the whole point is a genuine socket
    /// that an independent `reqwest::Client` (standing in for a
    /// separately-deployed caller, e.g. the control plane) can connect to.
    pub async fn spawn(self) -> TestEngineHandle {
        init_test_logging();
        let scanner_status = engine::scanner_status::new_scanner_status_map();
        let store = Store::open_in_memory()
            .expect("failed to open in-memory store for test engine")
            .into_shared();
        let custody: (
            Arc<dyn KeyCustody>,
            Option<Arc<engine::key_custody::SnpSlot>>,
            _,
        ) = (Arc::new(PlainKeyCustody::default()), None, None);
        #[cfg(feature = "snp")]
        let custody = if self.snp_backend {
            let (custody, slot, vcek) = snp_custody(&store).await;
            (custody, Some(slot), Some(vcek))
        } else {
            custody
        };
        let (key_custody, snp_slot, snp_vcek) = custody;
        let key_custody_backend = "plain";

        // Held separately (not just inline in `AppState`) so the background scan loop
        // below can clone the same `Arc` and re-read it fresh every tick, exactly like
        // `main.rs`'s own `run_scanner_loop` does against the real production
        // `AppState::wallet_handles` - see `with_real_daemon`'s doc comment.
        let wallet_handles: Arc<RwLock<HashMap<engine::store::TenantId, WalletHandle>>> =
            Arc::new(RwLock::new(HashMap::new()));

        let lookup_mempool: Arc<parking_lot::Mutex<Vec<monero::Transaction>>> = Arc::default();
        let admin_rate_limiter = Arc::new(RateLimiter::new(
            self.rate_limit_per_minute.unwrap_or(10_000),
        ));
        // Real settings, so the instance-admin settings API works; node
        // settings are saved but not applied (this harness's daemons are
        // fixed fakes) unless `with_live_nodes`. The rate limit keeps this
        // harness's own value unless a test saves one.
        let daemons = engine::engine_settings::Daemons::fixed(if self.admin_lookup_daemon {
            self.networks
                .iter()
                .map(|&network| {
                    (
                        network,
                        Arc::new(FallbackDaemonClient::new(vec![FallbackNode {
                            label: "lookup-test-daemon".to_string(),
                            client: Arc::new(LookupDaemonClient {
                                mempool: lookup_mempool.clone(),
                            }),
                        }])),
                    )
                })
                .collect::<HashMap<_, _>>()
        } else {
            // A network is "configured" exactly when it has a daemon client
            // (admin_settings_v2.md task 2.1), so each configured network gets
            // an inert one: tenants can be created on it, nothing is scanned.
            self.networks
                .iter()
                .map(|&network| {
                    (
                        network,
                        Arc::new(FallbackDaemonClient::new(vec![FallbackNode {
                            label: "noop-test-daemon".to_string(),
                            client: Arc::new(NoopDaemonClient),
                        }])),
                    )
                })
                .collect::<HashMap<_, _>>()
        });
        let engine_settings = engine::engine_settings::EngineSettings::load_full(
            store.clone(),
            self.live_nodes
                .then(|| engine::engine_settings::NodesReloadable {
                    daemons: daemons.clone(),
                }),
            None,
            Arc::new(RateLimiter::new(1)),
            live_settings::Env::fixed(Vec::<(String, String)>::new()).or_var(
                engine::engine_settings::SERVER_TOKEN.env_var,
                TEST_ENGINE_TOKEN,
            ),
            self.options
                .clone()
                .unwrap_or_else(|| live_settings::OptionsFile::in_memory("")),
            self.embedded,
        )
        .await
        .expect("test engine settings load from an empty store");
        let app_state = AppState {
            db: engine::store::Database::inline(store.clone()),
            admin_rate_limiter: admin_rate_limiter.clone(),
            log_store: self.log_store.clone(),
            engine_token: Arc::new(shared::auth::RawToken::presented(TEST_ENGINE_TOKEN).hash()),
            settings: engine_settings,
            custody: engine::http::Custody {
                backends: key_custody.clone(),
                default_backend: key_custody_backend.to_string(),
                wallet_handles: wallet_handles.clone(),
                snp: snp_slot,
            },
            // This harness's own background scan loop (below) talks to a
            // bare `NoopDaemonClient` directly, never through
            // `AppState::daemons` - no caller of this crate exercises the
            // engine's `/status` page or its admin payment-lookup endpoint by
            // default, so an empty map here is honest, not a stub standing in
            // for something real. See [`TestEngineConfig::
            // with_admin_lookup_daemon`] for the opt-in that wires one in.
            // Fixed fakes unless `with_live_nodes`: this harness's own
            // background scan loop (below) talks to a bare `NoopDaemonClient`
            // directly, never through `AppState::daemons` - see
            // [`TestEngineConfig::with_admin_lookup_daemon`] for the opt-in
            // that wires a lookup fake in instead.
            networks: engine::http::Networks {
                daemons,
                scanner_status: scanner_status.clone(),
            },
        };
        let tenant_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = tenant_requests.clone();
        let router = build_router(app_state, MAX_BODY_BYTES).layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                if request.uri().path().starts_with("/api/v1/admin/tenant/") {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                next.run(request)
            },
        ));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind an ephemeral local port for the test engine");
        let addr = listener
            .local_addr()
            .expect("bound listener has no local address");

        let served = router.clone();
        let server_task = tokio::spawn(async move {
            axum::serve(
                listener,
                served.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("test engine server error");
        });

        let mut background_tasks = Vec::new();
        if self.background_loops && self.background_scan_loop {
            let networks = self.networks.clone();
            let scan_store = store.clone();
            let scan_key_custody = key_custody.clone();
            let scan_wallet_handles = wallet_handles.clone();
            background_tasks.push(tokio::spawn(async move {
                let daemon = NoopDaemonClient;
                loop {
                    // Rebuilt fresh every tick (not a boot-time snapshot) so a tenant
                    // created at runtime - e.g. via a real connect flow through
                    // monokulo - is picked up without needing a restart, exactly
                    // like `main.rs`'s own `run_scanner_loop` re-reads its production
                    // `wallet_handles` registry every round. Harmless either way here -
                    // `NoopDaemonClient` never finds a payment match regardless of the
                    // tenant list - but this keeps the loop's own watchlist-building
                    // logic faithful to production, for a caller that later swaps in a
                    // real daemon via `run_scan_tick_now` instead.
                    let tenants: Vec<(engine::store::TenantId, WalletHandle)> = scan_wallet_handles
                        .read()
                        .iter()
                        .map(|(id, h)| (id.clone(), *h))
                        .collect();
                    for network in &networks {
                        // Errors are deliberately swallowed here, exactly like
                        // `main.rs`'s own supervised loop logs and continues rather
                        // than dying - a test relying on this loop observes its
                        // effect (a status transition, an order event), not its
                        // per-tick `Result`.
                        let _ = run_scan_tick(
                            &scan_store,
                            scan_key_custody.as_ref(),
                            &daemon,
                            network_str(*network),
                            &tenants,
                            20,
                            0,
                        )
                        .await;
                    }
                    tokio::time::sleep(BACKGROUND_LOOP_INTERVAL).await;
                }
            }));
        }

        TestEngineHandle {
            addr,
            router,
            server_task,
            background_tasks,
            store,
            key_custody,
            wallet_handles,
            tenant_requests,
            lookup_mempool,
            scanner_status,
            snp_vcek,
        }
    }
}

/// Boots a real `engine` engine with no configured Monero networks -
/// see [`TestEngineConfig::spawn`] for what "boots" means concretely.
/// Equivalent to `TestEngineConfig::new().spawn()`.
///
/// No tenant is created and `configured_networks` is empty, so any route that
/// depends on the scanner or a real `[monero_node]` isn't meaningfully usable
/// yet - but routes with no such dependency, e.g. `GET /status`, work with no
/// further setup. A caller that needs a tenant should create one against the
/// returned address via the engine's own admin API (`POST /api/v1/admin/tenants`).
pub async fn spawn_test_engine() -> TestEngineHandle {
    TestEngineConfig::new().spawn().await
}

/// Same as [`spawn_test_engine`], but with `configured_networks` set to the
/// given list instead of empty. Added for WBS 1.2.1 (`monokulo`'s
/// engine admin-API client): `POST /api/v1/admin/tenants`
/// (`src/http/admin.rs::create_tenant` at the repo root) rejects any
/// request for a network not in `state.configured_networks`, so a test that
/// needs a real tenant actually created — not just the route reachable —
/// needs at least one network configured. `spawn_test_engine` itself is
/// kept deliberately network-less (see its own doc comment: it's the
/// common case, and most callers only need dependency-free routes), so this
/// is a separate, explicit opt-in rather than a behavior change to the
/// existing function. Equivalent to
/// `TestEngineConfig::new().with_networks(networks).spawn()`; a test that
/// also needs a seeded exchange rate (e.g. to create a real order) should
/// use [`TestEngineConfig`] directly instead.
pub async fn spawn_test_engine_with_networks(networks: &[Network]) -> TestEngineHandle {
    TestEngineConfig::new()
        .with_networks(networks)
        .spawn()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The WBS 0.6 smoke test: start a real, network-bound engine instance and
    /// hit its most dependency-free route - `/status` (deliberately
    /// unauthenticated, no tenant or `[monero_node]` required - it reports on
    /// the instance as a whole, empty `configured_networks` included) -
    /// through a genuine `reqwest::Client` over a real TCP socket, not
    /// `tower::ServiceExt::oneshot`, proving the harness itself works
    /// end to end.
    #[tokio::test]
    async fn status_route_is_reachable_over_a_real_socket() {
        let engine = spawn_test_engine().await;

        // A real socket, not an in-process `tower::Service` call: the address
        // came back from a bound `TcpListener`, and this is an independent
        // `reqwest::Client` making an actual TCP connection to it.
        let response = engine_http_client()
            .get(format!("http://{}/status", engine.addr))
            .send()
            .await
            .expect("request to test engine failed");

        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    /// Same fixed-scalar view-key/spend-pubkey construction every other test in this
    /// workspace uses (see `monokulo/src/engine_client.rs`'s own tests for the
    /// reasoning).
    const TEST_VIEW_KEY_HEX: &str =
        "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str =
        "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";

    /// Direct proof that [`TestEngineConfig::with_background_loops`] genuinely runs
    /// the real scanner-tick machinery, entirely through the engine's own admin HTTP
    /// API - no `mock-woocommerce`/`monokulo` involved, since this crate sits below
    /// both of them and this capability needs to stand on its own.
    ///
    /// Forces the event the same way WBS 1.4.4's real test does: a tenant created
    /// with `order_expiry_seconds: 1`, then an order created against it with no
    /// payment ever made. `run_scan_tick`'s non-terminal-order recompute sweep is
    /// unconditional (see `with_background_loops`'s doc comment) - once one second of
    /// wall-clock time passes, the very next tick must flip the order to `expired`
    /// and write a real `order.expired` event, which the order-event stream
    /// (`GET /api/v1/admin/order-events`, what monokulo reads) then sends.
    #[tokio::test]
    async fn background_loops_genuinely_write_a_real_expired_order_event() {
        let engine = TestEngineConfig::new()
            .with_networks(&[Network::Mainnet])
            .with_background_loops()
            .spawn()
            .await;
        let base_url = format!("http://{}", engine.addr);
        let client = engine_http_client();

        let created: serde_json::Value = client
            .post(format!("{base_url}/api/v1/admin/tenants"))
            .json(&serde_json::json!({
                "view_key_hex": TEST_VIEW_KEY_HEX,
                "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
                "network": "mainnet",
                "order_expiry_seconds": 1,
            }))
            .send()
            .await
            .expect("create_tenant request failed")
            .json()
            .await
            .expect("create_tenant response was not valid JSON");
        let secret_token = created["secret_token"].as_str().unwrap().to_string();

        let order: serde_json::Value = client
            .post(format!("{base_url}/api/v1/admin/tenant/orders"))
            .bearer_auth(&secret_token)
            .json(&serde_json::json!({ "xmr_amount_piconero": 1_000_000_000_000u64 }))
            .send()
            .await
            .expect("create_order request failed")
            .json()
            .await
            .expect("create_order response was not valid JSON");
        let order_id = order["order_id"].as_str().unwrap().to_string();

        // The stream sends the event as it commits; the deadline bounds only a
        // hung run.
        let mut stream = client
            .get(format!("{base_url}/api/v1/admin/order-events?after=0"))
            .send()
            .await
            .expect("opening the order-event stream failed");
        assert_eq!(stream.status(), reqwest::StatusCode::OK);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let mut text = String::new();
        let event = loop {
            if let Some(event) = text
                .split("\n\n")
                .filter_map(|block| {
                    block
                        .lines()
                        .find_map(|line| line.strip_prefix("data: "))
                        .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok())
                })
                .find(|event| {
                    event["order_id"] == order_id.as_str() && event["event"] == "order.expired"
                })
            {
                break event;
            }
            let chunk = tokio::time::timeout_at(deadline, stream.chunk())
                .await
                .expect("expected a real order.expired event within the deadline")
                .expect("reading the order-event stream failed")
                .expect("the order-event stream ended");
            text.push_str(std::str::from_utf8(&chunk).unwrap());
        };
        assert_eq!(event["status"], serde_json::json!("expired"));
        assert_eq!(event["tenant"], created["public_key"]);
        assert!(event["event_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("evt_")));
    }

    /// The snp backend: the same outcomes as plain, through encrypted keys.
    #[cfg(feature = "snp")]
    mod snp {
        use super::*;

        /// A `MoneroDaemonClient` serving exactly one real transaction: the same
        /// fixture `key-custody`'s and `src/scanner.rs`'s own test suites use
        /// (`tests/fixtures/subaddress_tx.hex`, lifted from monero-rs's own
        /// `code_coverage_owned_tx_out` test - real RingCT amount decryption, not a
        /// synthetic tx) - deliberately reused rather than inventing a fresh one, per
        /// this task's own framing ("there should already be an existing test doing
        /// this... just swapping which backend answers"). Height stuck at 1 with one
        /// already-seeded empty block, tx served from the mempool - mirrors
        /// `src/scanner.rs`'s own
        /// `run_scan_tick_matches_mempool_tx_recomputes_status_and_writes_an_order_event`
        /// setup exactly (`daemon.push_block("h1", vec![])` then
        /// `daemon.set_mempool(vec![fixture_tx()])`), not a fresh scenario.
        struct FixtureTxDaemonClient;

        #[async_trait::async_trait]
        impl MoneroDaemonClient for FixtureTxDaemonClient {
            async fn get_height(&self) -> Result<u64, DaemonError> {
                Ok(1)
            }

            async fn get_block_hash(&self, _height: u64) -> Result<String, DaemonError> {
                Ok("h1".to_string())
            }

            /// Blocks 0 and 1, both empty.
            async fn get_chain_blocks(
                &self,
                start_height: u64,
                count: u64,
            ) -> Result<Vec<ChainBlock>, DaemonError> {
                Ok((start_height..start_height.saturating_add(count))
                    .take_while(|height| *height <= 1)
                    .map(|height| ChainBlock {
                        height,
                        hash: "h1".to_string(),
                        prev_hash: if height == 0 {
                            String::new()
                        } else {
                            "h1".to_string()
                        },
                        timestamp: 0,
                        txs: vec![],
                        txids: vec![],
                        wire_bytes: 0,
                    })
                    .collect())
            }

            async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
                Ok(vec![tx_id_hex(&fixture_tx())])
            }

            async fn get_transactions_with_ids(
                &self,
                txids: &[String],
            ) -> Result<Vec<FetchedTx>, DaemonError> {
                let tx = fixture_tx();
                let txid = tx_id_hex(&tx);
                Ok(if txids.contains(&txid) {
                    vec![FetchedTx { txid, tx }]
                } else {
                    vec![]
                })
            }

            async fn locate_transaction(&self, _txid: &str) -> Result<TxLocation, DaemonError> {
                Ok(TxLocation::NotFound)
            }

            async fn is_key_image_spent(
                &self,
                key_images: &[String],
            ) -> Result<Vec<KeyImageStatus>, DaemonError> {
                Ok(vec![KeyImageStatus::Unspent; key_images.len()])
            }
        }

        /// Same fixture bytes/keys `src/key_custody/plain.rs::scan_tx_outputs_finds_
        /// output_paid_to_subaddress` and `src/scanner.rs::setup_with_zero_conf_
        /// ceiling` use, copied verbatim (not re-derived) so this test provably
        /// exercises the identical scenario those already-trusted tests do.
        fn fixture_tx() -> monero::Transaction {
            let raw = hex::decode(include_str!(
                "../../engine/tests/fixtures/subaddress_tx.hex"
            ))
            .expect("fixture is valid hex");
            monero::consensus::encode::deserialize(&raw).expect("fixture is a valid monero tx")
        }

        const FIXTURE_VIEW_KEY_HEX: &str =
            "bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07";
        const FIXTURE_SECRET_SPEND_HEX: &str =
            "e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907";

        /// The fixture transaction pays subaddress 0/1 for the *public* spend key
        /// derived from [`FIXTURE_SECRET_SPEND_HEX`] - `admin::create_tenant`'s HTTP
        /// API (unlike the internal engine tests, which can construct `WalletMaterial`
        /// directly) only ever takes the public spend key, exactly as a real tenant
        /// pasting watch-only keys would.
        fn fixture_spend_pubkey_hex() -> String {
            let secret_spend =
                monero::PrivateKey::from_slice(&hex::decode(FIXTURE_SECRET_SPEND_HEX).unwrap())
                    .expect("fixture secret spend key is a valid scalar");
            hex::encode(monero::PublicKey::from_private_key(&secret_spend).to_bytes())
        }

        /// The keys a request creating or moving a store carries for `backend`:
        /// the fixture wallet's, in the clear for `plain`, encrypted to the
        /// engine's snp backend (under a bundle fetched from `bundle_url`) for
        /// `snp`.
        async fn fixture_keys_for(
            engine: &TestEngineHandle,
            backend: &str,
            bundle_url: &str,
            secret_token: Option<&str>,
        ) -> serde_json::Value {
            if backend != "snp" {
                return serde_json::json!({
                    "view_key_hex": FIXTURE_VIEW_KEY_HEX,
                    "spend_pubkey_hex": fixture_spend_pubkey_hex(),
                });
            }
            let mut request = engine_http_client()
                .post(bundle_url)
                .json(&serde_json::json!({ "backend": "snp" }));
            if let Some(token) = secret_token {
                request = request.bearer_auth(token);
            }
            let answer: serde_json::Value = request.send().await.unwrap().json().await.unwrap();
            let bundle = serde_json::from_value(answer["bundle"].clone()).unwrap();
            serde_json::json!({
                "encrypted_keys": engine.seal_keys_for_snp(
                    &bundle,
                    FIXTURE_VIEW_KEY_HEX,
                    &fixture_spend_pubkey_hex(),
                ),
            })
        }

        /// Runs the real order-creation-plus-chain-scan scenario against a freshly
        /// spawned engine built from `engine_config`, entirely through the engine's
        /// own public/admin HTTP API plus one real `run_scan_tick_now` call - exactly
        /// the pattern `background_loops_genuinely_write_a_real_expired_order_event`
        /// above already established for this crate, generalized to take the
        /// `KeyCustody` backend as a parameter instead of hardcoding it. Returns
        /// `(status, amount_received_piconero)` so the caller can compare two runs for
        /// exact equality rather than each asserting the expected values separately
        /// (a divergence between the two backends would otherwise have to coincidentally
        /// both match the same hardcoded expectation to go unnoticed - comparing the
        /// two results directly rules that out).
        async fn run_order_creation_and_scan_scenario(
            engine_config: TestEngineConfig,
            backend: &str,
        ) -> (String, u64) {
            // A deliberately tiny target amount (1000 piconero), not a realistic one:
            // the fixture transaction's real, already-fixed amount is unknown ahead of
            // time (it's a real historical Monero transaction, not something this test
            // controls), so the target amount only needs to be trivially satisfied by
            // whatever it actually paid - same reasoning
            // `src/scanner.rs::setup_with_zero_conf_ceiling` already documents for its
            // own `xmr_amount_piconero: 1`. A too-large amount here would make the
            // order land on `partial` instead of `unconfirmed`, which is exactly what
            // the first version of this test got wrong before this comment was added.
            let engine = engine_config
                .with_networks(&[Network::Mainnet])
                .spawn()
                .await;
            let base_url = format!("http://{}", engine.addr);
            let client = engine_http_client();

            let mut request = fixture_keys_for(
                &engine,
                backend,
                &format!("{base_url}/api/v1/admin/key-custody/bundle"),
                None,
            )
            .await;
            request["network"] = "mainnet".into();
            request["key_custody_backend"] = backend.into();
            let created: serde_json::Value = client
                .post(format!("{base_url}/api/v1/admin/tenants"))
                .json(&request)
                .send()
                .await
                .expect("create_tenant request failed")
                .json()
                .await
                .expect("create_tenant response was not valid JSON");
            let secret_token = created["secret_token"].as_str().unwrap().to_string();

            // The tenant's very first order lands on minor index 1 (`next_minor_index`
            // starts at 1 - see `migrations/0001_init.sql`) - exactly the subaddress
            // the fixture transaction pays, same as `src/scanner.rs::setup_with_zero_
            // conf_ceiling`'s own assertion pins this for the internal test.
            let order: serde_json::Value = client
                .post(format!("{base_url}/api/v1/admin/tenant/orders"))
                .bearer_auth(&secret_token)
                .json(&serde_json::json!({ "xmr_amount_piconero": 1_000u64 }))
                .send()
                .await
                .expect("create_order request failed")
                .json()
                .await
                .expect("create_order response was not valid JSON");
            let order_id = order["order_id"].as_str().unwrap().to_string();

            engine
                .run_scan_tick_now(&FixtureTxDaemonClient, Network::Mainnet, 20)
                .await
                .expect("real scan tick failed");

            let status: serde_json::Value = client
                .get(format!("{base_url}/api/v1/admin/tenant/orders/{order_id}"))
                .bearer_auth(&secret_token)
                .send()
                .await
                .expect("get_order_status request failed")
                .json()
                .await
                .expect("get_order_status response was not valid JSON");

            (
                status["status"]
                    .as_str()
                    .expect("status field present")
                    .to_string(),
                status["amount_received_piconero"]
                    .as_u64()
                    .expect("amount_received_piconero field present"),
            )
        }

        async fn order_status(base_url: &str, secret_token: &str, order_id: &str) -> String {
            let status: serde_json::Value = engine_http_client()
                .get(format!("{base_url}/api/v1/admin/tenant/orders/{order_id}"))
                .bearer_auth(secret_token)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            status["status"].as_str().unwrap().to_string()
        }

        /// A store that moves from plain to snp, its keys sent encrypted under a
        /// bundle for that store, has a payment to an order made before the
        /// move matched by the snp backend; keys sent in the clear for snp, or
        /// another store's encrypted keys, are refused.
        #[tokio::test]
        async fn a_payment_is_matched_after_a_store_moves_its_keys_to_snp() {
            let engine = TestEngineConfig::new()
                .with_networks(&[Network::Mainnet])
                .with_snp_backend()
                .spawn()
                .await;
            let base_url = format!("http://{}", engine.addr);
            let client = engine_http_client();

            // `/status` says which images the backend trusts, for monokulo to
            // compare with its own key entry policy.
            let status: serde_json::Value = client
                .get(format!("{base_url}/status"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(
                status["key_custody_snp_trust"],
                serde_json::json!({
                    "id_key_digest": hex::encode(snp_test_trust().id_key_digest),
                    "min_guest_svn": 0,
                    "min_tcb": "",
                }),
                "{status}"
            );

            let created: serde_json::Value = client
                .post(format!("{base_url}/api/v1/admin/tenants"))
                .json(&serde_json::json!({
                    "view_key_hex": FIXTURE_VIEW_KEY_HEX,
                    "spend_pubkey_hex": fixture_spend_pubkey_hex(),
                    "network": "mainnet",
                }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let secret_token = created["secret_token"].as_str().unwrap().to_string();
            let backend = || async {
                let me: serde_json::Value = client
                    .get(format!("{base_url}/api/v1/admin/tenant"))
                    .bearer_auth(&secret_token)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                me["key_custody_backend"].as_str().unwrap().to_string()
            };
            assert_eq!(backend().await, "plain", "the default");
            // Minor index 1: the subaddress the fixture transaction pays.
            let order: serde_json::Value = client
                .post(format!("{base_url}/api/v1/admin/tenant/orders"))
                .bearer_auth(&secret_token)
                .json(&serde_json::json!({ "xmr_amount_piconero": 1_000u64 }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let order_id = order["order_id"].as_str().unwrap().to_string();

            let move_url = format!("{base_url}/api/v1/admin/tenant/key-custody");
            let in_the_clear = client
                .put(&move_url)
                .bearer_auth(&secret_token)
                .json(&serde_json::json!({
                    "backend": "snp",
                    "view_key_hex": FIXTURE_VIEW_KEY_HEX,
                    "spend_pubkey_hex": fixture_spend_pubkey_hex(),
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(in_the_clear.status(), reqwest::StatusCode::BAD_REQUEST);
            let for_creating = fixture_keys_for(
                &engine,
                "snp",
                &format!("{base_url}/api/v1/admin/key-custody/bundle"),
                None,
            )
            .await;
            let wrong_challenge = client
                .put(&move_url)
                .bearer_auth(&secret_token)
                .json(&serde_json::json!({
                    "backend": "snp",
                    "encrypted_keys": for_creating["encrypted_keys"],
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(
                wrong_challenge.status(),
                reqwest::StatusCode::BAD_REQUEST,
                "keys sealed for creating a store don't move one"
            );
            assert_eq!(backend().await, "plain");

            let mut keys = fixture_keys_for(
                &engine,
                "snp",
                &format!("{base_url}/api/v1/admin/tenant/key-custody/bundle"),
                Some(&secret_token),
            )
            .await;
            keys["backend"] = "snp".into();
            let moved = client
                .put(&move_url)
                .bearer_auth(&secret_token)
                .json(&keys)
                .send()
                .await
                .unwrap();
            assert_eq!(moved.status(), reqwest::StatusCode::OK);
            assert_eq!(backend().await, "snp");

            engine
                .run_scan_tick_now(&FixtureTxDaemonClient, Network::Mainnet, 20)
                .await
                .unwrap();
            assert_eq!(
                order_status(&base_url, &secret_token, &order_id).await,
                "unconfirmed",
                "the snp backend matched the payment"
            );
        }

        /// The engine's order-creation-plus-chain-scan outcome is the same
        /// whichever backend holds the keys: reproduced end to end through the
        /// HTTP API once per backend, and compared for equality, not just
        /// plausibility.
        #[tokio::test]
        async fn order_creation_and_chain_scanning_behave_identically_on_the_snp_backend() {
            let plain_result =
                run_order_creation_and_scan_scenario(TestEngineConfig::new(), "plain").await;
            let snp_result = run_order_creation_and_scan_scenario(
                TestEngineConfig::new().with_snp_backend(),
                "snp",
            )
            .await;

            assert_eq!(
                plain_result, snp_result,
                "plain and snp must produce identical order-creation-plus-scan outcomes"
            );
            // And that shared outcome is the genuine, expected match - not two
            // backends agreeing on a no-op.
            assert_eq!(plain_result.0, "unconfirmed");
            assert!(
                plain_result.1 > 0,
                "the fixture transaction's amount must have been detected"
            );
        }

        /// An upgraded engine image (another measurement, a later security
        /// version, signed by the same ID key) on the same chip finds only the
        /// old image's wrap of the master key, asks the running engine for it
        /// over HTTP, and then opens the keys that engine sealed.
        #[tokio::test]
        async fn an_upgraded_engine_takes_the_master_key_over_http() {
            use engine::key_custody::snp::{SnpConfig, SnpKeyCustody, StoredWrap, WrapStore};
            use engine::key_custody::KeyCustody as _;

            struct OtherImagesWrap;
            impl WrapStore for OtherImagesWrap {
                fn load(&self) -> Result<Vec<StoredWrap>, String> {
                    Ok(vec![StoredWrap {
                        measurement: [0xEE; 48],
                        guest_svn: 1,
                        tcb: snp_attest::guest::TestIdentity::default().tcb,
                        wrapped: vec![0; 60],
                    }])
                }
                fn save(&self, _wrap: &StoredWrap) -> Result<(), String> {
                    Ok(())
                }
            }

            let old = TestEngineConfig::new()
                .with_networks(&[Network::Mainnet])
                .with_snp_backend()
                .spawn()
                .await;
            let base_url = format!("http://{}", old.addr);
            let mut request = fixture_keys_for(
                &old,
                "snp",
                &format!("{base_url}/api/v1/admin/key-custody/bundle"),
                None,
            )
            .await;
            request["network"] = "mainnet".into();
            request["key_custody_backend"] = "snp".into();
            let created = engine_http_client()
                .post(format!("{base_url}/api/v1/admin/tenants"))
                .json(&request)
                .send()
                .await
                .unwrap();
            assert_eq!(created.status(), reqwest::StatusCode::OK);
            let sealed = old
                .store
                .lock()
                .list_active_tenants()
                .unwrap()
                .remove(0)
                .sealed_key_material;

            let new = SnpKeyCustody::start(
                Arc::new(snp_attest::guest::TestGuest::new(
                    [7; 32],
                    snp_attest::guest::TestIdentity {
                        measurement: [0x22; 48],
                        guest_svn: 2,
                        ..snp_attest::guest::TestIdentity::default()
                    },
                )),
                SnpConfig {
                    product: snp_attest::report::Product::Genoa,
                    trust: snp_test_trust(),
                },
                Arc::new(OtherImagesWrap),
            )
            .unwrap();
            new.refresh_evidence(&TestEvidence).await.unwrap();
            assert!(new.awaiting_handoff());
            // The old engine's answer is checked against its stand-in VCEK.
            let old_anchor = engine::key_custody::transport::Anchor::Vcek(old.snp_vcek.unwrap());

            let wrong_token = engine::key_custody::request_handoff(
                &reqwest::Client::new(),
                &new,
                &engine::key_custody::Handoff {
                    url: base_url.clone(),
                    token: "not-the-engine-token".into(),
                },
                &old_anchor,
            )
            .await;
            assert!(wrong_token.is_err());
            engine::key_custody::request_handoff(
                &reqwest::Client::new(),
                &new,
                &engine::key_custody::Handoff {
                    url: base_url,
                    token: TEST_ENGINE_TOKEN.into(),
                },
                &old_anchor,
            )
            .await
            .unwrap();
            assert!(!new.awaiting_handoff());
            new.unseal_and_register(&sealed).await.unwrap();
        }
    }
}
