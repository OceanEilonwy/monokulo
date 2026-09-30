//! Thin binary wrapper — all real logic lives in the library (`src/lib.rs`),
//! same split as the engine's own `main.rs`/`lib.rs`. No config file, CLI
//! parsing, or deployment wiring yet (that's for a later WBS task); this is
//! just enough to actually run the one endpoint that exists so far.

use monokulo::db::{Database, Db};
use monokulo::engine_client::EngineClient;
use monokulo::http::status_page::new_status_cache;
use monokulo::http::{build_router, AppState};
use monokulo::settings;
use std::sync::Arc;

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
    // First, so everything after it is logged (structured_logging.md 1.1).
    let _telemetry = telemetry::init("monokulo", "MONOKULO");
    // Where the database lives and where monokulo listens: boot-only, from
    // the environment, like `MONOKULO_ENCRYPTION_KEY` (task 6.0 needs them
    // to run a test instance on a temporary database and a free port).
    let db_path = std::env::var("MONOKULO_DB_PATH").unwrap_or_else(|_| "monokulo.db".to_string());
    let bind = std::env::var("MONOKULO_BIND").unwrap_or_else(|_| "127.0.0.1:8081".to_string());
    // The settings store's own connection (it is synchronous); opening it
    // also brings the schema up to date.
    let settings_db = Db::open_file(&db_path)
        .expect("failed to open monokulo database")
        .into_shared();
    let read_connections = live_settings::read_sync::<settings::DatabaseConfig>(
        &settings::DbSettings(settings_db.clone()),
    )
    .read_connections;
    // Everything else: read-only connections and one writer, each on its
    // own thread (`db::Database`).
    let db = Database::open(&db_path, read_connections).expect("failed to open monokulo database");
    // Beside the main database; lines logged since start-up go in too.
    let log_store =
        telemetry::global().and_then(|t| t.open_store_beside(std::path::Path::new(&db_path)));
    let encryption_key = encryption_key_from_env();
    // Verified embed domains: the machine's own resolver. If it can't be set
    // up, the dashboard still works and every check says why it failed.
    let dns: Arc<dyn monokulo::embed_domains::TxtLookup> =
        match monokulo::embed_domains::SystemDns::new() {
            Ok(dns) => Arc::new(dns),
            Err(e) => {
                tracing::error!(error = %e, "DNS resolver unavailable, domain verification will fail");
                Arc::new(monokulo::embed_domains::UnavailableDns(format!(
                    "this server's DNS resolver is unavailable ({e})"
                )))
            }
        };

    // Every setting, live (admin_settings_v2.md parts 1 and 3): the engine
    // client, exchange-rate providers, abuse protection and onion listener
    // are built here with placeholders and then configured from the saved
    // settings by the registry, which reconfigures them in place whenever
    // the admin page saves.
    let engine_client = EngineClient::with_cache_limit("http://127.0.0.1:8443", 16 * 1024 * 1024);
    let exchange_rate = Arc::new(monokulo::exchange_rate_config::ExchangeRateProviders::xmr_only());
    let abuse = Arc::new(monokulo::abuse::AbuseProtection::default());
    let onion = settings::OnionReloadable::default();
    let monokulo_settings = match settings::MonokuloSettings::load(
        settings_db,
        engine_client.clone(),
        exchange_rate.clone(),
        abuse.clone(),
        Some(onion.clone()),
        live_settings::Env::process(),
    )
    .await
    {
        Ok(settings) => settings,
        Err(e) => {
            tracing::error!(error = %e, "failed to load settings");
            std::process::exit(1);
        }
    };

    monokulo::embed_domains::spawn_rechecks(db.clone(), dns.clone());
    if let Err(e) = db
        .write(|db| {
            monokulo::embed_domains::import_existing_domains(db);
            Ok::<_, monokulo::db::DbError>(())
        })
        .await
    {
        tracing::error!(error = %e, "could not import existing stores' domains");
    }
    let app_state = AppState {
        db,
        engine_client,
        encryption_key,
        status_cache: new_status_cache(),
        exchange_rate,
        abuse,
        dns,
        settings: monokulo_settings,
        log_store,
    };
    let router = build_router(app_state);
    // The onion listener (`monokulo::abuse::proxy_protocol`): same router,
    // but every connection must start with tor's PROXY header, which names
    // the Tor circuit - each circuit is then its own client.
    onion.router_ready(router.clone());

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .expect("failed to bind server address");
    tracing::info!(server.address = %bind, "monokulo listening");
    // `with_connect_info` - without this, `http::abuse`'s client lookup
    // would never see a real peer address in production, and would fail
    // open for every request (the "no signal at all" case that should only
    // ever happen in a test harness driven via `tower::ServiceExt::oneshot`).
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .expect("server error");
}
