//! Thin binary wrapper: all real logic lives in the library (`src/lib.rs`),
//! the same split as the engine's own `main.rs`/`lib.rs`. Every setting is
//! declared in `monokulo::settings` and comes from its command-line option
//! (`monokulo::cli`), its environment variable, the value saved on the admin
//! page, or its default, in that order.

use live_settings::Env;
use monokulo::db::{Database, Db};
use monokulo::engine_client::EngineClient;
use monokulo::http::{build_router, AppState};
use monokulo::settings;
use std::sync::Arc;

/// A required setting, or the reason the process can't start without it.
fn required<T: live_settings::SettingValue>(setting: &live_settings::Setting<T>, env: &Env) -> T {
    setting.require(env).unwrap_or_else(|e| {
        tracing::error!("{e}");
        std::process::exit(1);
    })
}

#[tokio::main]
async fn main() {
    // The command line first: `--help` and a mistyped option end here, and
    // a setting given as an option counts from the start, per request too.
    let env = monokulo::cli::parse_args(std::env::args_os()).unwrap_or_else(|e| e.exit());
    settings::set_process_env(env.clone());
    // Then logging, so everything after it is logged (structured_logging.md
    // 1.1), at the level and in the format the settings give. A problem
    // with either is reported with the rest when the settings load.
    let (level, _) = settings::LOGGING_LEVEL.read_unstored(&env);
    let (format, _) = settings::LOGGING_FORMAT.read_unstored(&env);
    let telemetry = telemetry::init_with("monokulo", &level, telemetry::Format::chosen(format));
    // What has to be known before the database opens, or must never be kept
    // in it: given at start only. The engine answers nothing without the
    // token, and stores' secrets can't be read without the key, so monokulo
    // doesn't start without either.
    let engine_token = required(&settings::ENGINE_TOKEN, &env);
    let encryption_key = required(&settings::CRYPTO_ENCRYPTION_KEY, &env);
    let encryption_key = match settings::encryption_key_bytes(encryption_key.expose()) {
        Ok(bytes) => monokulo::crypto::AtRestKey::new(bytes),
        Err(e) => {
            tracing::error!("{}: {e}", settings::CRYPTO_ENCRYPTION_KEY.env_var);
            std::process::exit(1);
        }
    };
    let engine_url = required(&settings::ENGINE_URL, &env);
    let db_path = required(&settings::DATABASE_PATH, &env);
    let db_path = db_path.to_string_lossy().to_string();
    // CPU and memory every 10 s, for the admin page (docs/engine_scaling.md 6).
    shared::resources::start_sampling();
    // How many readers to open is itself a setting, read on a connection
    // of its own before the pool exists; opening it also brings the schema
    // up to date.
    let read_connections = {
        let db = Db::open_file(&db_path).unwrap_or_else(|e| {
            tracing::error!(path = %db_path, error = %e, "failed to open the database");
            std::process::exit(1);
        });
        live_settings::read_sync_with_env::<settings::DatabaseConfig>(
            db.list_settings().map_err(live_settings::StoreError::new),
            &env,
        )
        .read_connections
    };
    // Everything else: read-only connections and one writer, each on its
    // own thread (`db::Database`).
    let db = Database::open(&db_path, read_connections).unwrap_or_else(|e| {
        tracing::error!(path = %db_path, error = %e, "failed to open the database");
        std::process::exit(1);
    });
    // Beside the main database; lines logged since start-up go in too.
    let log_store =
        telemetry::global().and_then(|t| t.open_store_beside(std::path::Path::new(&db_path)));
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
    let engine_client = EngineClient::with_cache_limit(
        engine_url.as_str().to_string(),
        shared::auth::engine_token(engine_token.expose()),
        shared::http_cache::DEFAULT_MAX_CACHE_BYTES,
    );
    let exchange_rate = Arc::new(monokulo::exchange_rate_config::ExchangeRateProviders::xmr_only());
    let abuse = Arc::new(monokulo::abuse::AbuseProtection::default());
    let onion = settings::OnionReloadable::default();
    let monokulo_settings = match settings::MonokuloSettings::load(
        db.clone(),
        engine_client.clone(),
        exchange_rate.clone(),
        abuse.clone(),
        Some(onion.clone()),
        env.clone(),
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
        encryption_key,
        exchange_rate,
        abuse,
        dns,
        settings: monokulo_settings.clone(),
        log_store,
        engine: monokulo::http::Engine::new(engine_client),
    };
    let app_state_settings = monokulo_settings;
    let router = build_router(app_state);
    // The onion listener (`monokulo::abuse::proxy_protocol`): same router,
    // but every connection must start with tor's PROXY header, which names
    // the Tor circuit - each circuit is then its own client.
    onion.router_ready(router.clone());

    // Read once: the listen address applies on restart.
    let bind = app_state_settings.server.load().bind;
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .unwrap_or_else(|e| {
            tracing::error!(server.address = %bind, error = %e, "failed to listen");
            std::process::exit(1);
        });
    tracing::info!(server.address = %bind, "monokulo listening");
    // `with_connect_info` - without this, `http::abuse`'s client lookup
    // would never see a real peer address in production, and would fail
    // open for every request (the "no signal at all" case that should only
    // ever happen in a test harness driven via `tower::ServiceExt::oneshot`).
    // On SIGTERM or Ctrl-C: stop accepting connections, let requests in
    // flight finish for up to the grace period, then store the last lines.
    let server = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shared::shutdown::signal());
    let served = tokio::spawn(async move { server.await });
    shared::shutdown::signal().await;
    tracing::info!(grace = ?shared::shutdown::GRACE, "shutting down: finishing requests in flight");
    match tokio::time::timeout(shared::shutdown::GRACE, served).await {
        Ok(Ok(Ok(()))) => tracing::info!("shut down cleanly"),
        Ok(Ok(Err(e))) => tracing::error!(error = %e, "server error while shutting down"),
        Ok(Err(e)) => tracing::error!(error = %e, "server task failed while shutting down"),
        Err(_) => {
            tracing::warn!(grace = ?shared::shutdown::GRACE, "requests still running after the grace period, exiting anyway")
        }
    }
    telemetry.flush(shared::shutdown::LOG_FLUSH).await;
}
