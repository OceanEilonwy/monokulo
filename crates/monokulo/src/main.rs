//! Thin binary wrapper — all real logic lives in the library (`src/lib.rs`),
//! same split as the engine's own `main.rs`/`lib.rs`. No config file, CLI
//! parsing, or deployment wiring yet (that's for a later WBS task); this is
//! just enough to actually run the one endpoint that exists so far.

use monokulo::db::Db;
use monokulo::engine_client::EngineClient;
use monokulo::exchange_rate_config::{self, ExchangeRateProviders};
use monokulo::http::status_page::new_status_cache;
use monokulo::http::{AppState, build_router};
use monokulo::templates::TemplateEngine;
use shared::rate_limit::RateLimiter;
use std::sync::Arc;

/// `MONOKULO_RATE_LIMIT_PER_IP_PER_MIN` (`docs/fx_refactor.md` Phase
/// 1.3) - the budget `http::rate_limit::rate_limit_middleware` enforces on
/// monokulo's own new public, unauthenticated endpoints. Defaults to
/// 20/min, the same default the engine's own equivalent
/// (`server.rate_limit_per_ip_per_min`) uses.
fn rate_limit_per_ip_per_min_from_env() -> u32 {
    match std::env::var("MONOKULO_RATE_LIMIT_PER_IP_PER_MIN") {
        Ok(raw) => raw.parse().unwrap_or_else(|_| {
            panic!("MONOKULO_RATE_LIMIT_PER_IP_PER_MIN must be a positive integer, got {raw:?}")
        }),
        Err(_) => 20,
    }
}

/// Reads the AES-256-GCM key (WBS 1.2.3) used to encrypt the engine's
/// `sk_...` secret token at rest (see `monokulo::crypto`) from
/// `MONOKULO_ENCRYPTION_KEY`, expected as 64 hex characters (32 bytes).
///
/// Deliberately no hardcoded fallback key: unlike the engine-URL placeholder
/// above (a stub value for a service that isn't really deployed yet), a
/// checked-in "temporary" encryption key would be a real credential leak the
/// moment this ever runs against a real database. A clear startup panic
/// telling the operator exactly what to set is the right placeholder
/// behavior instead.
fn encryption_key_from_env() -> [u8; 32] {
    let hex_key = std::env::var("MONOKULO_ENCRYPTION_KEY").expect(
        "MONOKULO_ENCRYPTION_KEY must be set to 64 hex characters (32 bytes) - \
         e.g. generate one with `openssl rand -hex 32`",
    );
    let bytes = hex::decode(&hex_key).expect(
        "MONOKULO_ENCRYPTION_KEY must be valid hex (64 hex characters decoding to exactly 32 bytes)",
    );
    <[u8; 32]>::try_from(bytes.as_slice())
        .expect("MONOKULO_ENCRYPTION_KEY must decode to exactly 32 bytes (64 hex characters)")
}

#[tokio::main]
async fn main() {
    let db = Db::open_file("monokulo.db").expect("failed to open monokulo database").into_shared();
    // TODO: real config. No config-file system exists yet in monokulo
    // (a later WBS task adds one); until then, this is a placeholder engine
    // URL, not a real deployment wiring.
    let engine_client = EngineClient::new("http://127.0.0.1:8080");
    let encryption_key = encryption_key_from_env();
    let templates =
        std::sync::Arc::new(TemplateEngine::new().expect("built-in signup/login templates must parse"));
    let exchange_rate_cfg = exchange_rate_config::from_real_env().expect(
        "invalid exchange-rate configuration - check CONTROL_PLANE_EXCHANGE_RATE_COINGECKO_ENABLED/\
         CONTROL_PLANE_EXCHANGE_RATE_COINGECKO_BASE_URL/CONTROL_PLANE_EXCHANGE_RATE_CACHE_SECONDS",
    );
    // No background refresh loop any more (`docs/fx_refactor.md` follow-up:
    // "looked up with an async call, rather than having it poll in the
    // background") - `ExchangeRateProviders::piconero_per_unit_for` does a live
    // Coingecko fetch inline the first time (or first time after its cache
    // goes stale) a request actually needs one; an XMR-denominated order
    // never needs one at all.
    let exchange_rate = Arc::new(ExchangeRateProviders::build(&exchange_rate_cfg));
    let rate_limiter = Arc::new(RateLimiter::new(rate_limit_per_ip_per_min_from_env()));
    let app_state = AppState {
        db,
        engine_client,
        encryption_key,
        templates,
        status_cache: new_status_cache(),
        exchange_rate,
        rate_limiter,
    };
    let router = build_router(app_state);

    let bind = "127.0.0.1:8081";
    let listener = tokio::net::TcpListener::bind(bind).await.expect("failed to bind server address");
    println!("monokulo listening on {bind}");
    // `with_connect_info` (`docs/fx_refactor.md` Phase 1.3) - without this,
    // `http::rate_limit::rate_limit_middleware`'s own `ConnectInfo` lookup
    // would never see a real peer address in production, and would fail
    // open for every request (the same "no signal at all" case its own doc
    // comment says should only ever happen in a test harness driven via
    // `tower::ServiceExt::oneshot`, not for real traffic).
    axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await
        .expect("server error");
}
