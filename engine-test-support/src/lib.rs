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

use monero::Network;
use moneropay_core::exchange_rate::{ExchangeRateProvider, FixedRateProvider};
use moneropay_core::http::rate_limit::RateLimiter;
use moneropay_core::http::{build_router, AppState};
use moneropay_core::key_custody::{KeyCustody, PlainKeyCustody};
use moneropay_core::store::Store;

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
}

impl Drop for TestEngineHandle {
    /// Aborts the background `axum::serve` task so the port and task don't
    /// outlive the test. A hard `abort()` rather than a graceful
    /// shutdown handshake is deliberately simple and sufficient at this
    /// scale: each test gets its own ephemeral port and its own task, there
    /// are no persistent connections worth draining, and the in-memory
    /// `Store` behind it is dropped along with everything else once the
    /// handle goes out of scope.
    fn drop(&mut self) {
        self.server_task.abort();
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
            store,
            key_custody,
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

        TestEngineHandle { addr, server_task }
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
}
