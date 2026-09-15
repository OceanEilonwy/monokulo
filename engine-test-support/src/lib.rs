//! Test-only helper for standing up a real, network-bound `moneropay-core`
//! engine instance for cross-crate integration tests. See WBS 0.6
//! (`docs/WOOCOMMERCE_WBS.md`) for the outcome this is meant to satisfy.
//!
//! ## Why this is its own crate, not `shared`
//!
//! `shared` is a *dependency of* `moneropay-core` (`shared::auth`,
//! `shared::webhook_sign`, `shared::password`, `shared::migrations` are all
//! pulled in by the engine crate). A helper that boots a real
//! `moneropay-core` engine instance necessarily needs `moneropay-core` itself
//! as a dependency - even as a dev-dependency, that would make `shared`
//! depend (for its own test/dev build) on a crate that depends on `shared`,
//! i.e. `moneropay-core -> shared -> moneropay-core`. Cargo rejects dependency
//! cycles like this outright (a dev-dependency edge back onto a crate that is
//! itself depended on, directly or transitively, still forms a cycle in the
//! resolved dependency graph), so this genuinely cannot live in `shared`.
//!
//! Instead it lives in its own crate, layered *above* both:
//! `engine-test-support -> moneropay-core -> shared`. That's not a
//! deviation from the WBS - 0.6 explicitly hedges with "a small helper (in
//! `shared`, or a dev-only sibling crate)", anticipating exactly this.
//!
//! Any workspace crate that needs a real, bound engine for its own tests
//! (`mock-woocommerce`, `control-plane`, ...) adds this as a
//! `[dev-dependencies]` entry. Because it is only ever reached that way, this
//! crate needs no `#[cfg(test)]`/feature gate of its own to stay out of
//! release builds the way it would if it lived inside `shared` - a
//! dev-dependency is never linked into a normal (non-test, non-dev) build of
//! whatever depends on it, by cargo's own rules.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use key_custody_service::client::SocketKeyCustody;
use monero::Network;
use moneropay_core::daemon::{DaemonError, KeyImageStatus, MoneroDaemonClient, TxLocation};
use moneropay_core::exchange_rate::{ExchangeRateProvider, FixedRateProvider};
use moneropay_core::http::rate_limit::RateLimiter;
use moneropay_core::http::{build_router, AppState};
use moneropay_core::key_custody::{KeyCustody, PlainKeyCustody, WalletHandle};
use moneropay_core::network::network_str;
use moneropay_core::scanner::run_scan_tick;
use moneropay_core::store::Store;
use moneropay_core::webhook_delivery::{run_delivery_tick, DEFAULT_MAX_ATTEMPTS};

/// How often the background loops (see [`TestEngineConfig::with_background_loops`])
/// re-run, when enabled. Real deployments poll on the order of seconds (see
/// `main.rs`'s `mempool_poll_interval_ms`/webhook delivery's own 5s sleep) - a test
/// harness can afford to poll far more aggressively than that so a test forcing a
/// real event (e.g. an order crossing its `expires_at`) doesn't have to wait long for
/// it to actually happen.
const BACKGROUND_LOOP_INTERVAL: Duration = Duration::from_millis(150);

/// A `MoneroDaemonClient` that does the least possible to let [`run_scan_tick`] run
/// at all, for callers that only need its status-recompute sweep (`docs/DESIGN.md`
/// §7.6 - confirmation growth and, especially, expiry) rather than genuine chain
/// scanning for payment matches.
///
/// This is deliberately *not* `moneropay_core::daemon::fake::FakeDaemonClient`: that
/// type is `#[cfg(test)]`-gated inside `moneropay-core` itself, which means it is
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
/// (`non_terminal_order_ids`/`recompute_and_notify`), which is driven by wall-clock
/// time and the store alone, not by anything this daemon reports - see
/// `TestEngineConfig::with_background_loops`'s own doc comment for the full
/// reasoning on why an inert daemon is sufficient here.
struct NoopDaemonClient;

#[async_trait::async_trait]
impl MoneroDaemonClient for NoopDaemonClient {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        Ok(0)
    }

    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        Ok(format!("noop-block-{height}"))
    }

    async fn get_block_transactions(
        &self,
        _height: u64,
    ) -> Result<Vec<monero::Transaction>, DaemonError> {
        Ok(vec![])
    }

    async fn get_mempool_transactions(&self) -> Result<Vec<monero::Transaction>, DaemonError> {
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

/// Matches the order of magnitude `main.rs` and `tests/e2e_stagenet.rs` use;
/// nothing a test sends against this harness should come close to it.
const MAX_BODY_BYTES: usize = 1_000_000;

/// A running, real (network-bound) `moneropay-core` engine instance started by
/// [`spawn_test_engine`], live for as long as this handle is held.
pub struct TestEngineHandle {
    /// The real local address the engine is listening on. Build requests
    /// against `format!("http://{addr}")` with a genuine `reqwest::Client`.
    pub addr: SocketAddr,
    server_task: tokio::task::JoinHandle<()>,
    /// The scanner-tick and webhook-delivery-tick background loops, present only when
    /// [`TestEngineConfig::with_background_loops`] was used. Empty otherwise, so
    /// `Drop` has nothing extra to abort for every other caller (the overwhelming
    /// majority of this crate's existing use, which never touches the scanner or
    /// delivery worker at all).
    background_tasks: Vec<tokio::task::JoinHandle<()>>,
    /// This engine's own store/key-custody/wallet-handles registry - kept so
    /// [`TestEngineHandle::run_scan_tick_now`] can drive a real, one-off scan tick
    /// against them directly. See that method's own doc comment for why a caller
    /// might want this instead of (or alongside) `with_background_loops`'s automatic
    /// interval.
    store: moneropay_core::store::SharedStore,
    key_custody: Arc<dyn KeyCustody>,
    wallet_handles: Arc<RwLock<HashMap<String, WalletHandle>>>,
}

impl TestEngineHandle {
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
    ) -> Result<(), moneropay_core::scanner::ScannerError> {
        let tenants: Vec<(String, WalletHandle)> = self
            .wallet_handles
            .read()
            .unwrap()
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
        )
        .await
    }
}

impl Drop for TestEngineHandle {
    /// Aborts the background `axum::serve` task (and, if spawned, the scanner/
    /// webhook-delivery loops) so the port and tasks don't outlive the test. A hard
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
/// configured (`state.configured_networks`) and what fixed exchange rates are
/// seeded into its `FixedRateProvider`. Added for WBS 1.3.3 (order-seeding
/// tests need a real exchange rate, not just a configured network) as a
/// generalization of the narrower `spawn_test_engine_with_networks` added for
/// WBS 1.2.1 - rather than a third near-duplicate spawn function per new
/// dimension of test setup, [`spawn_test_engine`] and
/// [`spawn_test_engine_with_networks`] are now both thin wrappers over
/// [`TestEngineConfig::spawn`], with no change to either's signature or
/// behavior.
#[derive(Debug, Default, Clone)]
pub struct TestEngineConfig {
    networks: Vec<Network>,
    rates: HashMap<String, u64>,
    background_loops: bool,
    background_scan_loop: bool,
    /// `Some(path)` when [`TestEngineConfig::with_socket_key_custody`] has been
    /// used - see that method's own doc comment.
    key_custody_socket_path: Option<String>,
}

impl TestEngineConfig {
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

    /// Seeds a fixed exchange rate (piconero per one whole unit of
    /// `currency`, e.g. per $1.00) into the spawned engine's
    /// `FixedRateProvider`. Needed by any test that creates a real order via
    /// the engine's public `POST /api/v1/t/{pk}/orders` - that handler
    /// rejects any `fiat_currency` with no configured rate (see
    /// `src/http/public.rs::create_order` at the repo root).
    pub fn with_rate(mut self, currency: &str, piconero_per_unit: u64) -> Self {
        self.rates.insert(currency.to_string(), piconero_per_unit);
        self
    }

    /// Opts into running the real scanner-tick and webhook-delivery-tick loops in
    /// the background against the spawned engine's own store - `main.rs`'s
    /// `run_scanner_loop`/`run_webhook_delivery_loop`, minus the supervisor restart
    /// wrapper (a test that panics here should fail loudly, not get quietly
    /// restarted) and on a much shorter interval (see [`BACKGROUND_LOOP_INTERVAL`]).
    ///
    /// This is what lets a caller force a *genuine* webhook delivery in a test
    /// (WBS 1.4.4/1.4.5): create a tenant with a short `order_expiry_seconds`, create
    /// an order against it, and simply wait - the real `run_scan_tick` recomputes
    /// every non-terminal order's status every tick regardless of whether anything
    /// was scanned (`docs/DESIGN.md` §7.6; confirmed directly against
    /// `src/scanner.rs`'s own
    /// `an_unpaid_order_past_its_deadline_becomes_expired_on_a_tick_that_matches_nothing`
    /// test, and against the fact that its non-terminal-order recompute sweep is keyed
    /// off `network` alone, not off the `tenants`/watchlist parameter that gates
    /// payment-matching), so an order past its deadline flips to `expired` and enqueues
    /// a real `order.expired` webhook purely from wall-clock time passing - no real
    /// Monero payment, node, or even a non-trivial `MoneroDaemonClient` required. The
    /// real `run_delivery_tick` then picks that delivery up and performs a genuine
    /// outbound HTTP call, signed exactly like a production delivery.
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
    /// `allow_private_urls` is unconditionally `true` for these loops - a test's own
    /// webhook receiver is essentially always `127.0.0.1`, and there is no
    /// SSRF-relevant "real merchant network" for a test harness to protect.
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

    /// Keeps the webhook-delivery-tick loop from [`with_background_loops`] but drops
    /// its `NoopDaemonClient`-driven scan-tick loop, for a caller that drives scanning
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

    /// Points the spawned engine at a real, already-listening `key-custody-server`
    /// (WBS 2.1.2/2.1.3) instead of the default in-process `PlainKeyCustody` -
    /// `SocketKeyCustody::connect(socket_path)` is called during [`spawn`], so
    /// `socket_path` must already have something bound to it (a caller typically
    /// starts a `key_custody_server::server::KeyCustodyServer` as a background
    /// task first, exactly as `key-custody-server`'s own tests do, then passes
    /// its socket path here) - unlike `main.rs`'s own `connect_socket_key_custody`,
    /// this does not retry, since a test controls both sides of the race itself
    /// and can simply start the server before calling this.
    ///
    /// Added specifically so this crate can host WBS 2.1.3's own regression
    /// check: the engine's existing order-creation-plus-chain-scan behavior must
    /// be unchanged when `KeyCustody` is answered by a real out-of-process
    /// `key-custody-server` instead of an in-process `PlainKeyCustody`. This is
    /// the right crate for that capability, not a new harness - `TestEngineConfig`
    /// already owns every other choice about what backs a spawned engine
    /// (`with_networks`, `with_rate`, `with_background_loops`), and both
    /// `mock-woocommerce`'s and `control-plane`'s own tests already depend on
    /// this crate to get a real, network-bound engine rather than building their
    /// own; a `KeyCustody` backend choice is exactly one more axis of "what backs
    /// the spawned engine," not a different kind of thing.
    ///
    /// [`spawn`]: TestEngineConfig::spawn
    pub fn with_socket_key_custody(mut self, socket_path: impl Into<String>) -> Self {
        self.key_custody_socket_path = Some(socket_path.into());
        self
    }

    /// Boots a real `moneropay-core` engine - in-memory `Store`,
    /// `PlainKeyCustody` (or, if [`TestEngineConfig::with_socket_key_custody`]
    /// was used, a real `SocketKeyCustody` dialed against an already-running
    /// `key-custody-server`), `configured_networks`/exchange rates from this
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
        let store = Store::open_in_memory()
            .expect("failed to open in-memory store for test engine")
            .into_shared();
        let (key_custody, key_custody_backend): (Arc<dyn KeyCustody>, &'static str) =
            match &self.key_custody_socket_path {
                Some(socket_path) => {
                    let client = SocketKeyCustody::connect(socket_path).await.unwrap_or_else(|e| {
                        panic!(
                            "test engine failed to connect to key-custody-server at \
                             {socket_path}: {e} - with_socket_key_custody requires the server \
                             to already be listening before spawn() is called"
                        )
                    });
                    (Arc::new(client), "socket")
                }
                None => (Arc::new(PlainKeyCustody::default()), "plain"),
            };
        let exchange_rate: Arc<dyn ExchangeRateProvider> =
            Arc::new(FixedRateProvider::new(self.rates));

        // Held separately (not just inline in `AppState`) so the background scan loop
        // below can clone the same `Arc` and re-read it fresh every tick, exactly like
        // `main.rs`'s own `run_scanner_loop` does against the real production
        // `AppState::wallet_handles` - see `with_real_daemon`'s doc comment.
        let wallet_handles: Arc<RwLock<HashMap<String, WalletHandle>>> =
            Arc::new(RwLock::new(HashMap::new()));

        let app_state = AppState {
            store: store.clone(),
            key_custody: key_custody.clone(),
            key_custody_backend: key_custody_backend.to_string(),
            exchange_rate,
            wallet_handles: wallet_handles.clone(),
            rate_limiter: Arc::new(RateLimiter::new(10_000)),
            configured_networks: Arc::new(
                self.networks.iter().copied().collect::<HashSet<Network>>(),
            ),
            // This harness's own background scan loop (below) talks to a
            // bare `NoopDaemonClient` directly, never through
            // `AppState::daemons` - no caller of this crate exercises the
            // engine's `/status` page, so an empty map here is honest, not
            // a stub standing in for something real.
            daemons: Arc::new(HashMap::new()),
            scanner_status: moneropay_core::scanner_status::new_scanner_status_map(),
            scan_poll_interval_secs: BACKGROUND_LOOP_INTERVAL.as_secs().max(1),
        };
        let router = build_router(app_state, MAX_BODY_BYTES);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind an ephemeral local port for the test engine");
        let addr = listener
            .local_addr()
            .expect("bound listener has no local address");

        let server_task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("test engine server error");
        });

        let mut background_tasks = Vec::new();
        if self.background_loops {
            if self.background_scan_loop {
                let networks = self.networks.clone();
                let scan_store = store.clone();
                let scan_key_custody = key_custody.clone();
                let scan_wallet_handles = wallet_handles.clone();
                background_tasks.push(tokio::spawn(async move {
                    let daemon = NoopDaemonClient;
                    loop {
                        // Rebuilt fresh every tick (not a boot-time snapshot) so a tenant
                        // created at runtime - e.g. via a real connect flow through
                        // control-plane - is picked up without needing a restart, exactly
                        // like `main.rs`'s own `run_scanner_loop` re-reads its production
                        // `wallet_handles` registry every round. Harmless either way here -
                        // `NoopDaemonClient` never finds a payment match regardless of the
                        // tenant list - but this keeps the loop's own watchlist-building
                        // logic faithful to production, for a caller that later swaps in a
                        // real daemon via `run_scan_tick_now` instead.
                        let tenants: Vec<(String, WalletHandle)> = scan_wallet_handles
                            .read()
                            .unwrap()
                            .iter()
                            .map(|(id, h)| (id.clone(), *h))
                            .collect();
                        for network in &networks {
                            // Errors are deliberately swallowed here, exactly like
                            // `main.rs`'s own supervised loop logs and continues rather
                            // than dying - a test relying on this loop observes its
                            // effect (a status transition, a delivered webhook), not its
                            // per-tick `Result`.
                            let _ = run_scan_tick(
                                &scan_store,
                                scan_key_custody.as_ref(),
                                &daemon,
                                network_str(*network),
                                &tenants,
                                20,
                            )
                            .await;
                        }
                        tokio::time::sleep(BACKGROUND_LOOP_INTERVAL).await;
                    }
                }));
            }

            let delivery_store = store.clone();
            background_tasks.push(tokio::spawn(async move {
                let client = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .expect("failed to build the test engine's webhook delivery HTTP client");
                loop {
                    let _ = run_delivery_tick(
                        &delivery_store,
                        &client,
                        true, // allow_private_urls - see with_background_loops's doc comment
                        Duration::from_secs(5),
                        DEFAULT_MAX_ATTEMPTS,
                        moneropay_core::now_unix(),
                    )
                    .await;
                    tokio::time::sleep(BACKGROUND_LOOP_INTERVAL).await;
                }
            }));
        }

        TestEngineHandle {
            addr,
            server_task,
            background_tasks,
            store,
            key_custody,
            wallet_handles,
        }
    }
}

/// Boots a real `moneropay-core` engine with no configured Monero networks
/// and no exchange rates - see [`TestEngineConfig::spawn`] for what "boots"
/// means concretely. Equivalent to `TestEngineConfig::new().spawn()`.
///
/// No tenant is created and `configured_networks` is empty, so any route that
/// depends on the scanner or a real `[monero_node]` isn't meaningfully usable
/// yet - but routes with no such dependency, e.g.
/// `GET /static/moneropay-client.js`, work with no further setup. A caller
/// that needs a tenant should create one against the returned address via the
/// engine's own admin API (`POST /api/v1/admin/tenants`).
pub async fn spawn_test_engine() -> TestEngineHandle {
    TestEngineConfig::new().spawn().await
}

/// Same as [`spawn_test_engine`], but with `configured_networks` set to the
/// given list instead of empty. Added for WBS 1.2.1 (`control-plane`'s
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
    /// hit its most dependency-free route - `/static/moneropay-client.js`,
    /// which needs no tenant, no `[monero_node]`, and no scanner (confirmed by
    /// reading `src/http/public.rs::client_library`, which takes no state at
    /// all) - through a genuine `reqwest::Client` over a real TCP socket, not
    /// `tower::ServiceExt::oneshot`, proving the harness itself works
    /// end to end.
    #[tokio::test]
    async fn client_library_route_is_reachable_over_a_real_socket() {
        let engine = spawn_test_engine().await;

        // A real socket, not an in-process `tower::Service` call: the address
        // came back from a bound `TcpListener`, and this is an independent
        // `reqwest::Client` making an actual TCP connection to it.
        let response = reqwest::Client::new()
            .get(format!("http://{}/static/moneropay-client.js", engine.addr))
            .send()
            .await
            .expect("request to test engine failed");

        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    /// Same fixed-scalar view-key/spend-pubkey construction every other test in this
    /// workspace uses (see `control-plane/src/engine_client.rs`'s own tests for the
    /// reasoning).
    const TEST_VIEW_KEY_HEX: &str =
        "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str =
        "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";

    /// Binds a tiny local receiver recording every POST body it gets, alongside the
    /// `X-MoneroPay-Signature` header - just enough to prove a real webhook delivery
    /// actually arrived, without pulling in anything from `mock-woocommerce` (this
    /// crate sits *below* it in the dependency graph, and `with_background_loops`
    /// needs to be provably useful entirely on its own).
    async fn spawn_recording_receiver() -> (
        SocketAddr,
        Arc<std::sync::Mutex<Vec<(Option<String>, serde_json::Value)>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::extract::State as AxumState;
        use axum::http::HeaderMap;

        let received: Arc<std::sync::Mutex<Vec<(Option<String>, serde_json::Value)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let received_for_state = received.clone();

        async fn hook(
            AxumState(received): AxumState<
                Arc<std::sync::Mutex<Vec<(Option<String>, serde_json::Value)>>>,
            >,
            headers: HeaderMap,
            body: axum::body::Bytes,
        ) -> axum::http::StatusCode {
            let signature = headers
                .get("X-MoneroPay-Signature")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            if let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&body) {
                received.lock().unwrap().push((signature, parsed));
            }
            axum::http::StatusCode::OK
        }

        let router = axum::Router::new()
            .route("/hook", axum::routing::post(hook))
            .with_state(received_for_state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        (addr, received, task)
    }

    /// Direct proof that [`TestEngineConfig::with_background_loops`] genuinely runs
    /// the real scanner-tick and webhook-delivery-tick machinery, entirely through
    /// the engine's own public/admin HTTP API - no `mock-woocommerce`/`control-plane`
    /// involved, since this crate sits below both of them and this capability needs
    /// to stand on its own.
    ///
    /// Forces the event the same way WBS 1.4.4's real test does: a tenant created
    /// with `order_expiry_seconds: 1`, then an order created against it with no
    /// payment ever made. `run_scan_tick`'s non-terminal-order recompute sweep is
    /// unconditional (see `with_background_loops`'s doc comment) - once one second of
    /// wall-clock time passes, the very next tick must flip the order to `expired`
    /// and enqueue a real, signed `order.expired` webhook, which the delivery loop
    /// then genuinely POSTs to the receiver below.
    #[tokio::test]
    async fn background_loops_genuinely_deliver_a_real_expired_webhook() {
        let engine = TestEngineConfig::new()
            .with_networks(&[Network::Mainnet])
            .with_rate("USD", 1_000_000_000_000)
            .with_background_loops()
            .spawn()
            .await;
        let base_url = format!("http://{}", engine.addr);
        let client = reqwest::Client::new();

        let created: serde_json::Value = client
            .post(format!("{base_url}/api/v1/admin/tenants"))
            .json(&serde_json::json!({
                "view_key_hex": TEST_VIEW_KEY_HEX,
                "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
                "network": "mainnet",
                "allowed_origins": [],
                "order_expiry_seconds": 1,
            }))
            .send()
            .await
            .expect("create_tenant request failed")
            .json()
            .await
            .expect("create_tenant response was not valid JSON");
        let public_key = created["public_key"].as_str().unwrap().to_string();
        let secret_token = created["secret_token"].as_str().unwrap().to_string();

        let (receiver_addr, received, receiver_task) = spawn_recording_receiver().await;
        let webhook: serde_json::Value = client
            .post(format!("{base_url}/api/v1/admin/tenant/webhooks"))
            .bearer_auth(&secret_token)
            .json(&serde_json::json!({ "url": format!("http://{receiver_addr}/hook") }))
            .send()
            .await
            .expect("create_webhook request failed")
            .json()
            .await
            .expect("create_webhook response was not valid JSON");
        let signing_secret = webhook["signing_secret"].as_str().unwrap().to_string();

        let order: serde_json::Value = client
            .post(format!("{base_url}/api/v1/t/{public_key}/orders"))
            .json(&serde_json::json!({ "fiat_amount": "1.00", "fiat_currency": "USD" }))
            .send()
            .await
            .expect("create_order request failed")
            .json()
            .await
            .expect("create_order response was not valid JSON");
        let payment_id = order["payment_id"].as_str().unwrap().to_string();

        // Poll rather than a fixed sleep: the background loop runs every
        // `BACKGROUND_LOOP_INTERVAL`, and this only needs to wait for the first tick
        // after the order's 1-second `expires_at` has actually passed.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let matched = loop {
            let found = received
                .lock()
                .unwrap()
                .iter()
                .find(|(_, body)| {
                    body.get("payment_id").and_then(|v| v.as_str()) == Some(payment_id.as_str())
                })
                .cloned();
            if let Some(found) = found {
                break Some(found);
            }
            if tokio::time::Instant::now() >= deadline {
                break None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };

        let (signature, payload) =
            matched.expect("expected a real order.expired webhook delivery within the deadline");
        assert_eq!(payload["event"], serde_json::json!("order.expired"));
        assert_eq!(payload["status"], serde_json::json!("expired"));
        assert!(payload["event_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("evt_")));

        // Strong proof this is a genuine, correctly-signed delivery, not just a
        // request that happened to arrive: recompute the HMAC over the exact payload
        // string this test received and require it to match what was sent, using
        // *this tenant's real* `signing_secret` handed back by `create_webhook`
        // above - a signature computed with any other secret must not verify.
        let signature = signature.expect("a real delivery must carry X-MoneroPay-Signature");
        let raw_payload = payload.to_string();
        assert!(
            moneropay_core::webhook_sign::verify_signature(
                &signing_secret,
                raw_payload.as_bytes(),
                &signature
            ),
            "the delivered signature must verify against this tenant's real signing_secret"
        );

        receiver_task.abort();
    }

    // -----------------------------------------------------------------------
    // WBS 2.1.3: the socket-backed `KeyCustody` regression check
    // -----------------------------------------------------------------------

    /// A `MoneroDaemonClient` serving exactly one real transaction: the same
    /// fixture `src/key_custody/plain.rs`'s, `src/scanner.rs`'s, and
    /// `key-custody-service`'s own test suites already use
    /// (`tests/fixtures/subaddress_tx.hex`, lifted from monero-rs's own
    /// `code_coverage_owned_tx_out` test - real RingCT amount decryption, not a
    /// synthetic tx) - deliberately reused rather than inventing a fresh one, per
    /// this task's own framing ("there should already be an existing test doing
    /// this... just swapping which backend answers"). Height stuck at 1 with one
    /// already-seeded empty block, tx served from the mempool - mirrors
    /// `src/scanner.rs`'s own
    /// `run_scan_tick_matches_mempool_tx_recomputes_status_and_enqueues_a_webhook`
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

        async fn get_block_transactions(
            &self,
            _height: u64,
        ) -> Result<Vec<monero::Transaction>, DaemonError> {
            Ok(vec![])
        }

        async fn get_mempool_transactions(&self) -> Result<Vec<monero::Transaction>, DaemonError> {
            Ok(vec![fixture_tx()])
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
        let raw = hex::decode(include_str!("../../tests/fixtures/subaddress_tx.hex"))
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

    /// A unique-per-call socket path under the OS temp dir, same convention
    /// `key-custody-server/tests/socket_key_custody.rs::temp_socket_path` already
    /// uses (pid + a monotonic counter, not a new `tempfile`/`uuid` dependency
    /// this crate doesn't otherwise need).
    fn temp_socket_path() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "engine-test-support-key-custody-{}-{n}.sock",
            std::process::id()
        ));
        path
    }

    /// Waits until something is listening at `path` - `KeyCustodyServer::listen`
    /// binds the socket before it starts accepting, but that bind happens inside
    /// the `tokio::spawn`ed task's future, which isn't guaranteed to have been
    /// polled even once by the time `tokio::spawn` returns control to the caller.
    /// Unlike `main.rs`'s own `connect_socket_key_custody` (a bounded *production*
    /// retry), this is pure test scaffolding: a real client connection is made
    /// afterwards, fresh, by `TestEngineConfig::with_socket_key_custody`'s own
    /// (non-retrying, by design - see its doc comment) `SocketKeyCustody::connect`
    /// inside `spawn()`.
    async fn wait_for_unix_socket(path: &std::path::Path) {
        for _ in 0..200 {
            if tokio::net::UnixStream::connect(path).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("key-custody-server never became reachable at {}", path.display());
    }

    /// Runs the real order-creation-plus-chain-scan scenario against a freshly
    /// spawned engine built from `engine_config`, entirely through the engine's
    /// own public/admin HTTP API plus one real `run_scan_tick_now` call - exactly
    /// the pattern `background_loops_genuinely_deliver_a_real_expired_webhook`
    /// above already established for this crate, generalized to take the
    /// `KeyCustody` backend as a parameter instead of hardcoding it. Returns
    /// `(status, amount_received_piconero)` so the caller can compare two runs for
    /// exact equality rather than each asserting the expected values separately
    /// (a divergence between the two backends would otherwise have to coincidentally
    /// both match the same hardcoded expectation to go unnoticed - comparing the
    /// two results directly rules that out).
    async fn run_order_creation_and_scan_scenario(
        engine_config: TestEngineConfig,
    ) -> (String, u64) {
        // A deliberately tiny rate (1000 piconero for a $1.00 order), not a
        // realistic one: the fixture transaction's real, already-fixed amount is
        // unknown ahead of time (it's a real historical Monero transaction, not
        // something this test controls), so the target amount only needs to be
        // trivially satisfied by whatever it actually paid - same reasoning
        // `src/scanner.rs::setup_with_zero_conf_ceiling` already documents for its
        // own `xmr_amount_piconero: 1`. A too-large rate here would make the order
        // land on `partial` instead of `unconfirmed`, which is exactly what the
        // first version of this test got wrong before this comment was added.
        let engine = engine_config
            .with_networks(&[Network::Mainnet])
            .with_rate("USD", 1_000)
            .spawn()
            .await;
        let base_url = format!("http://{}", engine.addr);
        let client = reqwest::Client::new();

        let created: serde_json::Value = client
            .post(format!("{base_url}/api/v1/admin/tenants"))
            .json(&serde_json::json!({
                "view_key_hex": FIXTURE_VIEW_KEY_HEX,
                "spend_pubkey_hex": fixture_spend_pubkey_hex(),
                "network": "mainnet",
                "allowed_origins": [],
            }))
            .send()
            .await
            .expect("create_tenant request failed")
            .json()
            .await
            .expect("create_tenant response was not valid JSON");
        let public_key = created["public_key"].as_str().unwrap().to_string();

        // The tenant's very first order lands on minor index 1 (`next_minor_index`
        // starts at 1 - see `migrations/0001_init.sql`) - exactly the subaddress
        // the fixture transaction pays, same as `src/scanner.rs::setup_with_zero_
        // conf_ceiling`'s own assertion pins this for the internal test.
        let order: serde_json::Value = client
            .post(format!("{base_url}/api/v1/t/{public_key}/orders"))
            .json(&serde_json::json!({ "fiat_amount": "1.00", "fiat_currency": "USD" }))
            .send()
            .await
            .expect("create_order request failed")
            .json()
            .await
            .expect("create_order response was not valid JSON");
        let payment_id = order["payment_id"].as_str().unwrap().to_string();

        engine
            .run_scan_tick_now(&FixtureTxDaemonClient, Network::Mainnet, 20)
            .await
            .expect("real scan tick failed");

        let status: serde_json::Value = client
            .get(format!("{base_url}/api/v1/t/{public_key}/orders/{payment_id}"))
            .send()
            .await
            .expect("get_order_status request failed")
            .json()
            .await
            .expect("get_order_status response was not valid JSON");

        (
            status["status"].as_str().expect("status field present").to_string(),
            status["amount_received_piconero"].as_u64().expect("amount_received_piconero field present"),
        )
    }

    /// WBS 2.1.3's own acceptance bar, quoted directly: "the engine's existing
    /// integration tests (order creation, scanning) pass unmodified against this
    /// configuration - a regression check, not a new test." Taken literally where
    /// it can be: this reuses the *exact* scenario `src/scanner.rs`'s own
    /// `run_scan_tick_matches_mempool_tx_recomputes_status_and_enqueues_a_webhook`
    /// test already proves against `PlainKeyCustody` directly (same fixture
    /// transaction, same view/spend keys, same `unconfirmed`-with-a-nonzero-amount
    /// outcome) - not a new scenario invented for this task.
    ///
    /// It cannot be taken *completely* literally, though, and that gap is worth
    /// being honest about rather than silently working around: that internal test
    /// lives inside `moneropay-core`'s own `#[cfg(test)]` build and constructs a
    /// `PlainKeyCustody`/`Store` directly in-process, so it structurally cannot be
    /// "pointed at" a different `KeyCustody` backend without becoming a different
    /// test - and `moneropay-core` itself can never depend on `key-custody-service`'s
    /// `SocketKeyCustody` at all without recreating the exact Cargo dependency cycle
    /// `shared::key_custody`'s module doc comment describes (this crate,
    /// `engine-test-support`, is what depends on both, same as `mock-woocommerce`/
    /// `control-plane` already do for their own real-engine tests). So instead of
    /// literally re-running that unit test, this reproduces its scenario end-to-end
    /// through the real HTTP API twice - once per backend - via
    /// [`run_order_creation_and_scan_scenario`], and asserts the two runs are
    /// pixel-for-pixel identical, not just individually plausible.
    #[tokio::test]
    async fn order_creation_and_chain_scanning_behave_identically_through_the_socket_backed_key_custody_path(
    ) {
        let plain_result = run_order_creation_and_scan_scenario(TestEngineConfig::new()).await;

        let socket_path = temp_socket_path();
        let server = std::sync::Arc::new(key_custody_server::server::KeyCustodyServer::new(
            moneropay_core::key_custody::PlainKeyCustody::default(),
        ));
        let listen_path = socket_path.clone();
        tokio::spawn(async move {
            if let Err(e) = server.listen(&listen_path).await {
                eprintln!("test key-custody-server on {}: {e}", listen_path.display());
            }
        });
        wait_for_unix_socket(&socket_path).await;

        let socket_result = run_order_creation_and_scan_scenario(
            TestEngineConfig::new().with_socket_key_custody(socket_path.to_string_lossy().to_string()),
        )
        .await;
        let _ = std::fs::remove_file(&socket_path);

        // The real proof: both backends produce the *exact same* observable
        // outcome, not just "each individually looks fine."
        assert_eq!(
            plain_result, socket_result,
            "PlainKeyCustody and SocketKeyCustody must produce identical order-creation-plus-scan outcomes"
        );

        // And that shared outcome is the genuine, expected match - not two
        // backends agreeing on a no-op.
        assert_eq!(plain_result.0, "unconfirmed");
        assert!(plain_result.1 > 0, "the fixture transaction's amount must have been detected");
    }
}
