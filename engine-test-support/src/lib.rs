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

use monero::Network;
use moneropay_core::daemon::{DaemonError, KeyImageStatus, MoneroDaemonClient, TxLocation};
use moneropay_core::exchange_rate::{ExchangeRateProvider, FixedRateProvider};
use moneropay_core::http::rate_limit::RateLimiter;
use moneropay_core::http::{build_router, AppState};
use moneropay_core::key_custody::{KeyCustody, PlainKeyCustody};
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

    async fn get_block_transactions(&self, _height: u64) -> Result<Vec<monero::Transaction>, DaemonError> {
        Ok(vec![])
    }

    async fn get_mempool_transactions(&self) -> Result<Vec<monero::Transaction>, DaemonError> {
        Ok(vec![])
    }

    async fn locate_transaction(&self, _txid: &str) -> Result<TxLocation, DaemonError> {
        Ok(TxLocation::NotFound)
    }

    async fn is_key_image_spent(&self, key_images: &[String]) -> Result<Vec<KeyImageStatus>, DaemonError> {
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
    pub fn with_background_loops(mut self) -> Self {
        self.background_loops = true;
        self
    }

    /// Boots a real `moneropay-core` engine - in-memory `Store`,
    /// `PlainKeyCustody`, `configured_networks`/exchange rates from this
    /// config - bound to an OS-assigned ephemeral port on `127.0.0.1`, served
    /// in a background task. Returns only once the listener is actually
    /// bound, so the returned address is immediately connectable.
    ///
    /// This packages the same construction `tests/e2e_stagenet.rs` and
    /// `main.rs` use (`Store` -> `PlainKeyCustody` -> `AppState` ->
    /// `build_router`), but binds a real `tokio::net::TcpListener` and drives
    /// it with `axum::serve` instead of exercising the router in-process via
    /// `tower::ServiceExt::oneshot` - the whole point is a genuine socket
    /// that an independent `reqwest::Client` (standing in for a
    /// separately-deployed caller, e.g. the control plane) can connect to.
    pub async fn spawn(self) -> TestEngineHandle {
        let store = Store::open_in_memory().expect("failed to open in-memory store for test engine").into_shared();
        let key_custody: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let exchange_rate: Arc<dyn ExchangeRateProvider> = Arc::new(FixedRateProvider::new(self.rates));

        let app_state = AppState {
            store: store.clone(),
            key_custody: key_custody.clone(),
            exchange_rate,
            wallet_handles: Arc::new(RwLock::new(HashMap::new())),
            rate_limiter: Arc::new(RateLimiter::new(10_000)),
            configured_networks: Arc::new(self.networks.iter().copied().collect::<HashSet<Network>>()),
        };
        let router = build_router(app_state, MAX_BODY_BYTES);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind an ephemeral local port for the test engine");
        let addr = listener.local_addr().expect("bound listener has no local address");

        let server_task = tokio::spawn(async move {
            axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
                .await
                .expect("test engine server error");
        });

        let mut background_tasks = Vec::new();
        if self.background_loops {
            let networks = self.networks.clone();
            let scan_store = store.clone();
            let scan_key_custody = key_custody.clone();
            background_tasks.push(tokio::spawn(async move {
                let daemon = NoopDaemonClient;
                loop {
                    for network in &networks {
                        // Errors are deliberately swallowed here, exactly like
                        // `main.rs`'s own supervised loop logs and continues rather
                        // than dying - a test relying on this loop observes its
                        // effect (a status transition, a delivered webhook), not its
                        // per-tick `Result`.
                        let _ = run_scan_tick(&scan_store, scan_key_custody.as_ref(), &daemon, network_str(*network), &[], 20).await;
                    }
                    tokio::time::sleep(BACKGROUND_LOOP_INTERVAL).await;
                }
            }));

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

        TestEngineHandle { addr, server_task, background_tasks }
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
    TestEngineConfig::new().with_networks(networks).spawn().await
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
    const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";

    /// Binds a tiny local receiver recording every POST body it gets, alongside the
    /// `X-MoneroPay-Signature` header - just enough to prove a real webhook delivery
    /// actually arrived, without pulling in anything from `mock-woocommerce` (this
    /// crate sits *below* it in the dependency graph, and `with_background_loops`
    /// needs to be provably useful entirely on its own).
    async fn spawn_recording_receiver() -> (SocketAddr, Arc<std::sync::Mutex<Vec<(Option<String>, serde_json::Value)>>>, tokio::task::JoinHandle<()>)
    {
        use axum::extract::State as AxumState;
        use axum::http::HeaderMap;

        let received: Arc<std::sync::Mutex<Vec<(Option<String>, serde_json::Value)>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let received_for_state = received.clone();

        async fn hook(
            AxumState(received): AxumState<Arc<std::sync::Mutex<Vec<(Option<String>, serde_json::Value)>>>>,
            headers: HeaderMap,
            body: axum::body::Bytes,
        ) -> axum::http::StatusCode {
            let signature = headers.get("X-MoneroPay-Signature").and_then(|v| v.to_str().ok()).map(str::to_string);
            if let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&body) {
                received.lock().unwrap().push((signature, parsed));
            }
            axum::http::StatusCode::OK
        }

        let router = axum::Router::new().route("/hook", axum::routing::post(hook)).with_state(received_for_state);
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
                .find(|(_, body)| body.get("payment_id").and_then(|v| v.as_str()) == Some(payment_id.as_str()))
                .cloned();
            if let Some(found) = found {
                break Some(found);
            }
            if tokio::time::Instant::now() >= deadline {
                break None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };

        let (signature, payload) = matched.expect("expected a real order.expired webhook delivery within the deadline");
        assert_eq!(payload["event"], serde_json::json!("order.expired"));
        assert_eq!(payload["status"], serde_json::json!("expired"));
        assert!(payload["event_id"].as_str().is_some_and(|id| id.starts_with("evt_")));

        // Strong proof this is a genuine, correctly-signed delivery, not just a
        // request that happened to arrive: recompute the HMAC over the exact payload
        // string this test received and require it to match what was sent, using
        // *this tenant's real* `signing_secret` handed back by `create_webhook`
        // above - a signature computed with any other secret must not verify.
        let signature = signature.expect("a real delivery must carry X-MoneroPay-Signature");
        let raw_payload = payload.to_string();
        assert!(
            moneropay_core::webhook_sign::verify_signature(&signing_secret, raw_payload.as_bytes(), &signature),
            "the delivered signature must verify against this tenant's real signing_secret"
        );

        receiver_task.abort();
    }
}
