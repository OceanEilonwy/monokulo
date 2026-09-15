//! Thin binary wrapper — all real logic lives in the library (`src/lib.rs`),
//! same split as the engine's own `main.rs`/`lib.rs`. No config file, CLI
//! parsing, or deployment wiring yet (that's for a later WBS task); this is
//! just enough to actually run the one endpoint that exists so far.

use control_plane::db::Db;
use control_plane::engine_client::EngineClient;
use control_plane::exchange_rate_config::{self, ExchangeRateConfig};
use control_plane::http::status_page::new_status_cache;
use control_plane::http::{AppState, build_router};
use control_plane::templates::TemplateEngine;
use shared::exchange_rate::{CoingeckoRateProvider, ExchangeRateProvider};
use shared::rate_limit::RateLimiter;
use shared::supervise::supervise;
use std::sync::Arc;

/// `CONTROL_PLANE_RATE_LIMIT_PER_IP_PER_MIN` (`docs/fx_refactor.md` Phase
/// 1.3) - the budget `http::rate_limit::rate_limit_middleware` enforces on
/// control-plane's own new public, unauthenticated endpoints. Defaults to
/// 20/min, the same default the engine's own equivalent
/// (`server.rate_limit_per_ip_per_min`) uses.
fn rate_limit_per_ip_per_min_from_env() -> u32 {
    match std::env::var("CONTROL_PLANE_RATE_LIMIT_PER_IP_PER_MIN") {
        Ok(raw) => raw.parse().unwrap_or_else(|_| {
            panic!("CONTROL_PLANE_RATE_LIMIT_PER_IP_PER_MIN must be a positive integer, got {raw:?}")
        }),
        Err(_) => 20,
    }
}

/// Reads the AES-256-GCM key (WBS 1.2.3) used to encrypt the engine's
/// `sk_...` secret token at rest (see `control_plane::crypto`) from
/// `CONTROL_PLANE_ENCRYPTION_KEY`, expected as 64 hex characters (32 bytes).
///
/// Deliberately no hardcoded fallback key: unlike the engine-URL placeholder
/// above (a stub value for a service that isn't really deployed yet), a
/// checked-in "temporary" encryption key would be a real credential leak the
/// moment this ever runs against a real database. A clear startup panic
/// telling the operator exactly what to set is the right placeholder
/// behavior instead.
fn encryption_key_from_env() -> [u8; 32] {
    let hex_key = std::env::var("CONTROL_PLANE_ENCRYPTION_KEY").expect(
        "CONTROL_PLANE_ENCRYPTION_KEY must be set to 64 hex characters (32 bytes) - \
         e.g. generate one with `openssl rand -hex 32`",
    );
    let bytes = hex::decode(&hex_key).expect(
        "CONTROL_PLANE_ENCRYPTION_KEY must be valid hex (64 hex characters decoding to exactly 32 bytes)",
    );
    <[u8; 32]>::try_from(bytes.as_slice())
        .expect("CONTROL_PLANE_ENCRYPTION_KEY must decode to exactly 32 bytes (64 hex characters)")
}

/// Builds the one `Arc<dyn ExchangeRateProvider>` this process shares
/// (`docs/fx_refactor.md` Phase 1.1) - mirrors the engine's own former
/// `main.rs::exchange_rate` dispatch exactly: a `Coingecko` config does a
/// best-effort refresh at boot (a transient outage right now shouldn't stop
/// the whole service from starting - the supervised background loop below
/// keeps retrying) and starts a `supervise`d loop that refreshes it again on
/// `cache_seconds`'s interval; a `Fixed` config needs neither.
async fn build_exchange_rate_provider(config: &ExchangeRateConfig) -> Arc<dyn ExchangeRateProvider> {
    match config {
        ExchangeRateConfig::Fixed { .. } => Arc::new(
            config.build_fixed_rate_provider().expect("invalid CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES entry"),
        ),
        ExchangeRateConfig::Coingecko { cache_seconds, .. } => {
            let provider = Arc::new(config.build_coingecko_rate_provider());
            if let Err(e) = provider.refresh().await {
                eprintln!(
                    "initial coingecko exchange-rate refresh failed: {e} - starting with an empty rate cache; \
                     orders will be rejected as an unsupported currency until the background refresh loop \
                     (every {cache_seconds}s) succeeds"
                );
            }
            let refresh_provider = provider.clone();
            let cache_seconds = *cache_seconds;
            supervise("coingecko exchange-rate refresh", move || {
                run_coingecko_refresh_loop(refresh_provider.clone(), cache_seconds)
            });
            provider
        }
    }
}

async fn run_coingecko_refresh_loop(provider: Arc<CoingeckoRateProvider>, cache_seconds: u64) {
    let interval = std::time::Duration::from_secs(cache_seconds);
    loop {
        tokio::time::sleep(interval).await;
        if let Err(e) = provider.refresh().await {
            eprintln!("coingecko exchange-rate refresh failed: {e} - continuing to serve the last successfully cached rates");
        }
    }
}

#[tokio::main]
async fn main() {
    let db = Db::open_file("control_plane.db").expect("failed to open control-plane database").into_shared();
    // TODO: real config. No config-file system exists yet in control-plane
    // (a later WBS task adds one); until then, this is a placeholder engine
    // URL, not a real deployment wiring.
    let engine_client = EngineClient::new("http://127.0.0.1:8080");
    let encryption_key = encryption_key_from_env();
    let templates =
        std::sync::Arc::new(TemplateEngine::new().expect("built-in signup/login templates must parse"));
    let exchange_rate_cfg = exchange_rate_config::from_real_env().expect(
        "invalid exchange-rate configuration - check CONTROL_PLANE_EXCHANGE_RATE_PROVIDER/\
         CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES/CONTROL_PLANE_EXCHANGE_RATE_CURRENCIES/\
         CONTROL_PLANE_EXCHANGE_RATE_CACHE_SECONDS",
    );
    let exchange_rate_provider = exchange_rate_cfg.provider_name();
    let exchange_rate = build_exchange_rate_provider(&exchange_rate_cfg).await;
    let rate_limiter = Arc::new(RateLimiter::new(rate_limit_per_ip_per_min_from_env()));
    let app_state = AppState {
        db,
        engine_client,
        encryption_key,
        templates,
        status_cache: new_status_cache(),
        exchange_rate,
        exchange_rate_provider,
        rate_limiter,
    };
    let router = build_router(app_state);

    let bind = "127.0.0.1:8081";
    let listener = tokio::net::TcpListener::bind(bind).await.expect("failed to bind server address");
    println!("control-plane listening on {bind}");
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
