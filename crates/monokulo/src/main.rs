//! Thin binary wrapper: all real logic lives in the library (`src/lib.rs`),
//! the same split as the engine's own `main.rs`/`lib.rs`. Every setting is
//! declared in `monokulo::settings`. Configuration comes from the options
//! file (`--options`, else `~/.config/monokulo/monokulo.toml`), which the
//! admin page saves to, or its command-line option (`monokulo::cli`), which
//! wins; runtime switches from the database; secrets from the environment.
//!
//! The engine runs inside monokulo unless `engine.mode` is remote
//! (docs/engine_as_library.md): on a runtime of its own, its threads named
//! `engine-…`, with its settings in this options file's `[engine.*]`
//! tables, reached through its router in-process with a token made for
//! this run.

use std::sync::Arc;

use live_settings::{Env, OptionsFile, Snapshot};
use monokulo::db::Database;
use monokulo::engine_client::EngineClient;
use monokulo::http::{build_router, AppState};
use monokulo::settings::{self, EngineMode};

/// Ends the process at start, saying why: before logging is set up, on
/// stderr; after, in the log.
fn stop(message: impl std::fmt::Display) -> ! {
    if telemetry::global().is_some() {
        tracing::error!("{message}");
    } else {
        eprintln!("{message}");
    }
    std::process::exit(1);
}

/// A required setting, or the reason the process can't start without it.
fn required<T: live_settings::SettingValue>(setting: &live_settings::Setting<T>, env: &Env) -> T {
    setting.require(env).unwrap_or_else(|e| stop(e))
}

fn main() {
    // The command line first: `--help`, `--init` and a mistyped option end
    // here.
    let args = monokulo::cli::parse_args(std::env::args_os()).unwrap_or_else(|e| e.exit());
    let start = args.start;
    if start.init {
        match monokulo::cli::write_init(&start.options) {
            Ok(()) => std::process::exit(0),
            Err(e) => stop(e),
        }
    }
    let env = start.env;
    // The options file next: it says where the database is, and anything
    // wrong in it stops monokulo here, line by line. The engine's tables
    // are left to the engine at first; they are refused below if there is
    // no engine here to read them.
    let file = OptionsFile::at(&start.options);
    let own_options = file.clone().leaving(settings::ENGINE_TABLE);
    let values = own_options.read(settings::ALL).unwrap_or_else(|e| stop(e));
    let early = Snapshot::new(values, env.clone());
    // Then logging, so everything after it is logged (structured_logging.md
    // 1.1), at the level and in the format the settings give.
    let telemetry = telemetry::init_with(
        "monokulo",
        &early.get(&settings::LOGGING_LEVEL),
        telemetry::Format::chosen(early.get(&settings::LOGGING_FORMAT)),
    );
    let mode = settings::engine_mode(&early, &env).unwrap_or_else(|e| stop(e));
    let own_options = match mode {
        EngineMode::Embedded => own_options,
        // No engine here to read `[engine.*]`: refused, with why.
        EngineMode::Remote => {
            let strict = file
                .clone()
                .with_hint(settings::ENGINE_TABLE, settings::ENGINE_TABLE_HINT);
            if let Err(e) = strict.read(settings::ALL) {
                stop(e);
            }
            strict
        }
    };
    let engine = match mode {
        EngineMode::Embedded => Some(embedded::prepare(&file, args.engine)),
        EngineMode::Remote => None,
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| stop(format!("failed to start the async runtime: {e}")));
    let engine_runtime = runtime.block_on(run(Boot {
        env,
        early,
        mode,
        own_options,
        engine,
    }));
    // Every engine task stopped in `run`; whatever is left of its runtime
    // (idle threads) goes now, without waiting on anything stuck.
    if let Some(engine_runtime) = engine_runtime {
        engine_runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    }
    // The lines above, and any still on their way, stored (and exported)
    // before the process ends.
    runtime.block_on(telemetry.flush(shared::shutdown::LOG_FLUSH));
}

/// What `main` read before the runtime started.
struct Boot {
    env: Env,
    early: Snapshot,
    mode: EngineMode,
    own_options: OptionsFile,
    engine: Option<embedded::Prepared>,
}

/// Runs monokulo until SIGTERM or Ctrl-C, and the embedded engine with it;
/// hands back the engine's runtime, its tasks stopped, to be dropped
/// outside this one.
async fn run(boot: Boot) -> Option<tokio::runtime::Runtime> {
    let Boot {
        env,
        early,
        mode,
        own_options,
        engine,
    } = boot;
    // What has to be known before the database opens, or must never be kept
    // in it: given at start only. Stores' secrets can't be read without the
    // key, so monokulo doesn't start without it.
    let encryption_key = required(&settings::CRYPTO_ENCRYPTION_KEY, &env);
    let encryption_key = match settings::encryption_key_bytes(encryption_key.expose()) {
        Ok(bytes) => monokulo::crypto::AtRestKey::new(bytes),
        Err(e) => stop(format!("{}: {e}", settings::CRYPTO_ENCRYPTION_KEY.env_var)),
    };
    let db_path = monokulo::cli::database_path(&early);
    // CPU and memory every 10 s, for the admin page (docs/engine_scaling.md 6):
    // the process's, the engine's included when it runs inside.
    shared::resources::start_sampling();
    // How many readers to open applies on restart.
    let read_connections = early.get(&settings::DATABASE_READ_CONNECTIONS);
    // Read-only connections and one writer, each on its own thread
    // (`db::Database`); opening it also brings the schema up to date.
    let db_file = db_path.to_string_lossy().to_string();
    let db = Database::open(&db_file, read_connections)
        .unwrap_or_else(|e| stop(format!("failed to open the database at {db_file}: {e}")));
    // Beside the main database; lines logged since start-up go in too. The
    // process's one log store: opened before an embedded engine starts, so
    // its lines (named `engine`) come here too.
    let log_store = telemetry::global().and_then(|t| t.open_store_beside(&db_path));

    // The engine, inside or over HTTP.
    let (engine_client, embedded) = match (mode, engine) {
        (EngineMode::Embedded, Some(prepared)) => {
            let running = embedded::start(prepared, &db_path).await;
            (running.client.clone(), Some(running))
        }
        _ => {
            let engine_url = early.get(&settings::ENGINE_URL);
            let engine_token = required(&settings::ENGINE_TOKEN, &env);
            let client = EngineClient::with_cache_limit(
                engine_url.as_str().to_string(),
                shared::auth::engine_token(engine_token.expose()),
                shared::http_cache::DEFAULT_MAX_CACHE_BYTES,
            );
            (client, None)
        }
    };

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
    // are built here with placeholders and then configured from the
    // settings by the registry, which reconfigures them in place whenever
    // the admin page saves or reloads the options file.
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
        own_options,
    )
    .await
    {
        Ok(settings) => settings,
        Err(e) => stop(format!("failed to load settings: {e}")),
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
    let router = build_router(app_state);
    // The onion listener (`monokulo::abuse::proxy_protocol`): same router,
    // but every connection must start with tor's PROXY header, which names
    // the Tor circuit - each circuit is then its own client.
    onion.router_ready(router.clone());

    // Read once: the listen address applies on restart.
    let bind = monokulo_settings.server.load().bind;
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .unwrap_or_else(|e| stop(format!("failed to listen on {bind}: {e}")));
    tracing::info!(server.address = %bind, "monokulo listening");
    // `with_connect_info` - without this, `http::abuse`'s client lookup
    // would never see a real peer address in production, and would fail
    // open for every request (the "no signal at all" case that should only
    // ever happen in a test harness driven via `tower::ServiceExt::oneshot`).
    // On SIGTERM or Ctrl-C: stop accepting connections, let requests in
    // flight finish for up to the grace period, then stop the engine.
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
    match embedded {
        Some(running) => Some(running.stop().await),
        None => None,
    }
}

/// The engine inside monokulo (`engine.mode = "embedded"`).
#[cfg(feature = "embedded-engine")]
mod embedded {
    use std::collections::HashMap;
    use std::path::Path;

    use engine::engine_settings::{RuntimeConfig, DATABASE_PATH};
    use engine::run::{Engine, EngineConfig, Host, Stopped};
    use live_settings::Section as _;
    use live_settings::{Env, OptionsFile, SettingSource, Snapshot};
    use monokulo::engine_client::EngineClient;

    use super::stop;

    /// The engine's settings and runtime, before monokulo's runtime starts.
    pub struct Prepared {
        options: OptionsFile,
        env: Env,
        early: Snapshot,
        runtime: tokio::runtime::Runtime,
    }

    /// Reads the engine's tables of the options file (anything wrong in them
    /// stops monokulo, line by line), and builds its runtime with
    /// `server.worker_threads` workers, its threads named `engine-…`. The
    /// engine's lines are named `engine` in monokulo's log.
    pub fn prepare(file: &OptionsFile, cli: HashMap<String, String>) -> Prepared {
        let options = file.scoped(monokulo::settings::ENGINE_TABLE);
        let env = Env::process().with_cli(cli);
        let values = options
            .read(engine::engine_settings::ALL)
            .unwrap_or_else(|e| stop(e));
        // Before anything is opened; `Engine::start` checks again.
        engine::run::refuse_standalone_settings(&options, &env).unwrap_or_else(|e| stop(e));
        let early = Snapshot::new(values, env.clone());
        // How many threads, on which CPUs, at what niceness: only the
        // engine's threads, never monokulo's (`engine::threads`).
        let threads = RuntimeConfig::from_snapshot(&early)
            .unwrap_or_else(|errors| {
                stop(
                    errors
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("; "),
                )
            })
            .threads;
        threads.check().unwrap_or_else(|e| {
            stop(format!(
                "the engine's threads can't be set up as asked: {e}"
            ))
        });
        let runtime = threads
            .build_runtime()
            .unwrap_or_else(|e| stop(format!("failed to start the engine's runtime: {e}")));
        if let Some(telemetry) = telemetry::global() {
            telemetry.host("engine", &["engine"], engine::threads::THREAD_PREFIX);
        }
        // Its share of the process's CPU, for the admin page.
        shared::resources::sampler().host_threads(engine::threads::THREAD_PREFIX);
        Prepared {
            options,
            env,
            early,
            runtime,
        }
    }

    /// The engine, running.
    pub struct Running {
        engine: Engine,
        runtime: tokio::runtime::Runtime,
        pub client: EngineClient,
    }

    /// Starts the engine on its own runtime, with a token made for this
    /// run, its database beside monokulo's unless `database.path` says
    /// otherwise. A problem stops monokulo.
    pub async fn start(prepared: Prepared, monokulo_db: &Path) -> Running {
        let Prepared {
            options,
            env,
            early,
            runtime,
        } = prepared;
        let database_path = if early.source(&DATABASE_PATH) == SettingSource::Default {
            monokulo_db.with_file_name("engine.db")
        } else {
            early.get(&DATABASE_PATH)
        };
        let token = shared::auth::generate_engine_token();
        let config = EngineConfig {
            options,
            env,
            database_path,
            host: Host::Embedded {
                token: token.clone(),
            },
        };
        let engine = match runtime.spawn(Engine::start(config)).await {
            Ok(Ok(engine)) => engine,
            Ok(Err(e)) => stop(format!("the engine didn't start: {e}")),
            Err(e) => stop(format!("the engine didn't start: {e}")),
        };
        let client = EngineClient::embedded(engine.router(), token, Some(runtime.handle().clone()));
        tracing::info!("the engine is running inside monokulo");
        Running {
            engine,
            runtime,
            client,
        }
    }

    impl Running {
        /// Stops the engine's loops, waiting up to the grace period, and
        /// hands back its runtime.
        pub async fn stop(self) -> tokio::runtime::Runtime {
            let grace = shared::shutdown::GRACE;
            match self.runtime.spawn(self.engine.shutdown(grace)).await {
                Ok(Stopped::Cleanly) => tracing::info!("the engine stopped"),
                Ok(Stopped::TimedOut) => {
                    tracing::warn!(grace = ?grace, "the engine's loops were still stopping after the grace period")
                }
                Err(e) => tracing::error!(error = %e, "stopping the engine failed"),
            }
            self.runtime
        }
    }
}

/// Without the engine built in, only a remote engine can be used.
#[cfg(not(feature = "embedded-engine"))]
mod embedded {
    use std::collections::HashMap;
    use std::path::Path;

    use live_settings::OptionsFile;
    use monokulo::engine_client::EngineClient;

    const NO_ENGINE: &str = "this monokulo was built without the engine (its embedded-engine feature): set engine.mode = \"remote\" and run monokulo-engine";

    /// Never made: [`prepare`] stops monokulo first.
    pub struct Prepared(());

    pub fn prepare(_file: &OptionsFile, _cli: HashMap<String, String>) -> Prepared {
        super::stop(NO_ENGINE)
    }

    /// Never made either.
    pub struct Running {
        pub client: EngineClient,
    }

    pub async fn start(_prepared: Prepared, _monokulo_db: &Path) -> Running {
        super::stop(NO_ENGINE)
    }

    impl Running {
        pub async fn stop(self) -> tokio::runtime::Runtime {
            super::stop(NO_ENGINE)
        }
    }
}
