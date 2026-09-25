//! Thin binary wrapper — all real logic lives in the library (`src/lib.rs`),
//! same split as the engine's own `main.rs`/`lib.rs`. No config file, CLI
//! parsing, or deployment wiring yet (that's for a later WBS task); this is
//! just enough to actually run the one endpoint that exists so far.

use monokulo::db::Db;
use monokulo::engine_client::EngineClient;
use monokulo::exchange_rate_config::{self, ExchangeRateConfig};
use monokulo::http::status_page::new_status_cache;
use monokulo::http::{AppState, build_router};
use monokulo::settings::{self, ScalarSetting};
use shared::rate_limit::RateLimiter;
use std::sync::Arc;

/// Builds the real `ExchangeRateConfig` from `db`'s own settings
/// (`env > database > default`, `monokulo::settings`) rather than calling
/// `exchange_rate_config::from_real_env` directly - that function only ever
/// sees the process environment, with no database fallback, which would
/// leave this one config struct unable to honor a value the admin settings
/// page saved. `exchange_rate_config::parse` itself is reused unchanged: its
/// `get_env` closure here just resolves through `settings::get_raw` first, so
/// the exact same validation (and error messages) as the env-only path still
/// applies to whichever value - env, database, or default - actually wins.
fn exchange_rate_config_from_settings(db: &Db) -> ExchangeRateConfig {
    let get = |env_var: &str| -> Option<String> {
        let setting: &ScalarSetting = match env_var {
            "MONOKULO_EXCHANGE_RATE_COINGECKO_ENABLED" => &settings::EXCHANGE_RATE_COINGECKO_ENABLED,
            "MONOKULO_EXCHANGE_RATE_COINGECKO_BASE_URL" => &settings::EXCHANGE_RATE_COINGECKO_BASE_URL,
            "MONOKULO_EXCHANGE_RATE_CACHE_SECONDS" => &settings::EXCHANGE_RATE_CACHE_SECONDS,
            _ => unreachable!("exchange_rate_config::parse only ever asks for its own three known env vars"),
        };
        Some(settings::get_raw(db, setting).0)
    };
    exchange_rate_config::parse(get).expect(
        "invalid exchange-rate configuration - check MONOKULO_EXCHANGE_RATE_COINGECKO_ENABLED/\
         MONOKULO_EXCHANGE_RATE_COINGECKO_BASE_URL/MONOKULO_EXCHANGE_RATE_CACHE_SECONDS \
         (or their saved admin-settings equivalents)",
    )
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
    let db = Db::open_file("monokulo.db").expect("failed to open monokulo database");
    // The one setting an operator can save from the admin settings page
    // that changes where monokulo itself points (`monokulo::settings::
    // ENGINE_URL`) - defaults to the same placeholder address this always
    // hardcoded before that page existed.
    let http_cache_max_mb: u64 = settings::get(&db, &settings::HTTP_CACHE_MAX_MB);
    let engine_client =
        EngineClient::with_cache_limit(settings::get::<String>(&db, &settings::ENGINE_URL), http_cache_max_mb * 1024 * 1024);
    let encryption_key = encryption_key_from_env();
    let exchange_rate_cfg = exchange_rate_config_from_settings(&db);
    // No background refresh loop any more (`docs/fx_refactor.md` follow-up:
    // "looked up with an async call, rather than having it poll in the
    // background") - `ExchangeRateProviders::piconero_per_unit_for` does a live
    // Coingecko fetch inline the first time (or first time after its cache
    // goes stale) a request actually needs one; an XMR-denominated order
    // never needs one at all.
    let exchange_rate = Arc::new(monokulo::exchange_rate_config::ExchangeRateProviders::build(&exchange_rate_cfg));
    let rate_limiter = Arc::new(RateLimiter::new(settings::get(&db, &settings::RATE_LIMIT_PER_IP_PER_MIN)));
    // Verified embed domains: the machine's own resolver. If it can't be set
    // up, the dashboard still works and every check says why it failed.
    let dns: Arc<dyn monokulo::embed_domains::TxtLookup> = match monokulo::embed_domains::SystemDns::new() {
        Ok(dns) => Arc::new(dns),
        Err(e) => {
            eprintln!("DNS resolver unavailable, domain verification will fail: {e}");
            Arc::new(monokulo::embed_domains::UnavailableDns(format!("this server's DNS resolver is unavailable ({e})")))
        }
    };
    let db = db.into_shared();
    monokulo::embed_domains::spawn_rechecks(db.clone(), dns.clone());
    {
        let (db, engine_client) = (db.clone(), engine_client.clone());
        tokio::spawn(async move { monokulo::embed_domains::import_existing_domains(&db, &engine_client, &encryption_key).await });
    }
    let app_state = AppState {
        db,
        engine_client,
        encryption_key,
        status_cache: new_status_cache(),
        exchange_rate,
        rate_limiter,
        event_streams: Default::default(),
        dns,
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
