//! Boot sequence: read the command line, read the engine token (`server.token`,
//! required), open storage, register every tenant's wallet with
//! `KeyCustody` (tenants are created by monokulo, through the admin API, when
//! a merchant connects a store), then run the chain scanner (once per configured network), the
//! webhook delivery loop, and the HTTP server concurrently.
//!
//! Every setting is declared once (`engine::engine_settings`). Configuration
//! comes from the options file (`--options`, else
//! `~/.config/monokulo/engine.toml`), which the admin page saves to, or its
//! command-line option, which wins; runtime switches from the database;
//! secrets from the environment. The file is read first, since it says where
//! the database is.

// See `lib.rs`: no panics in loop code. `main` itself may still exit at boot
// (a listener that can't bind), which is marked where it happens.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
// A boot failure comes before logging is set up: it is told on stderr and
// ends the process with a failing status.
#![expect(
    clippy::print_stderr,
    clippy::exit,
    reason = "boot failures are reported on stderr and end the process"
)]

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use engine::cli;
use engine::engine_settings::{
    Daemons, EngineSettings, RuntimeConfig, ALL, LOGGING_FORMAT, LOGGING_LEVEL, SERVER_TOKEN,
};
use engine::http::rate_limit::RateLimiter;
use engine::http::{build_router, AppState};
use engine::key_custody::{CustodyRouter, KeyCustody, WalletHandle};
use engine::loops;
use engine::scanner_status;
use engine::store::{SharedStore, Store};
use live_settings::{Env, OptionsFile, Snapshot};
use std::path::PathBuf;

/// What start-up read before anything else: the command line and
/// environment, the options file and its values, and where the database is.
struct Boot {
    env: Env,
    options: PathBuf,
    file: HashMap<String, String>,
    db_path: PathBuf,
}
use shared::supervise::supervise;

/// A fixed outer ceiling on request bodies; `server.max_body_bytes` (the
/// live limit, task 2.6) is what normally applies, and its own range tops
/// out here.
const MAX_BODY_CEILING: usize = 16 * 1024 * 1024;

fn open_store(db_path: &std::path::Path) -> Store {
    Store::open_file(&db_path.to_string_lossy()).unwrap_or_else(|e| {
        tracing::error!(path = %db_path.display(), error = %e, "failed to open database");
        std::process::exit(1);
    })
}

/// Builds the runtime with `server.worker_threads` threads (task 2.8: read
/// before the runtime exists, so it applies at the next start), then runs.
fn main() {
    // The command line first: `--help` and a mistyped option end here, and
    // a setting given as an option counts from the start.
    let start = cli::parse_args(std::env::args_os()).unwrap_or_else(|e| e.exit());
    if start.init {
        match live_settings::cli::init("monokulo-engine", &start.options, ALL) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    // The options file next: it says where the database is, and anything
    // wrong in it stops the engine here, line by line.
    let file = OptionsFile::at(&start.options)
        .read(ALL)
        .unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });
    let early = Snapshot::new(file.clone(), start.env.clone());
    // Then logging, so everything after it is logged (structured_logging.md
    // 1.1), at the level and in the format the settings give.
    let _telemetry = telemetry::init_with(
        "engine",
        &early.get(&LOGGING_LEVEL),
        telemetry::Format::chosen(early.get(&LOGGING_FORMAT)),
    );
    let boot = Boot {
        db_path: cli::database_path(&early),
        env: start.env,
        options: start.options,
        file,
    };
    let worker_threads =
        live_settings::read_sync_with_env::<RuntimeConfig>(Ok(boot.file.clone()), &boot.env)
            .worker_threads;
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::error!(worker_threads, error = %e, "failed to start the async runtime");
            std::process::exit(1);
        }
    };
    runtime.block_on(run(boot));
}

#[expect(
    clippy::expect_used,
    reason = "boot-time: a listener that can't bind or a server that can't start ends the process"
)]
async fn run(boot: Boot) {
    let env = &boot.env;
    // Every request must carry it (`http::engine_token_middleware`): without
    // one, nothing could talk to this engine, so it doesn't start.
    let engine_token = match SERVER_TOKEN.require(env) {
        Ok(token) => Arc::new(shared::auth::engine_token(token.expose()).hash()),
        Err(e) => {
            tracing::error!("{e}");
            std::process::exit(1);
        }
    };

    // CPU and memory every 10 s, for the admin page (docs/engine_scaling.md 6).
    shared::resources::start_sampling();

    let store = open_store(&boot.db_path).into_shared();
    let db_path = boot.db_path.clone();
    // Beside the main database; lines logged since start-up go in too.
    let log_store = telemetry::global().and_then(|t| t.open_store_beside(&db_path));

    // Every setting, live (admin_settings_v2.md part 1). Node clients are
    // built into `daemons` from the saved node settings, and rebuilt whenever
    // they are saved; the rate limiter and the key custody backends (part 5)
    // follow their settings the same way.
    let daemons = Daemons::default();
    let admin_rate_limiter = Arc::new(RateLimiter::new(1));
    let router = Arc::new(CustodyRouter::default());
    let engine_settings = match EngineSettings::load(
        Arc::clone(&store),
        daemons.clone(),
        Arc::clone(&router),
        Arc::clone(&admin_rate_limiter),
        env.clone(),
        OptionsFile::at(&boot.options),
    )
    .await
    {
        Ok(settings) => settings,
        Err(e) => {
            tracing::error!(error = %e, "failed to load settings");
            std::process::exit(1);
        }
    };
    if daemons.networks().is_empty() {
        // A warning, not an exit: the settings API has to be reachable to
        // configure a node at all, and one saved there applies straight away.
        tracing::warn!(
            "no Monero node is configured for any network (mainnet/stagenet/testnet) yet - nothing is \
             scanned until one is saved on the admin settings page (or POST /api/v1/admin/settings, \
             monero_node.<network>); it applies without a restart"
        );
    }

    let enabled = router.enabled_backends();
    let stranded: Vec<(String, usize)> = {
        let tenants = store.lock().tenant_custody_backends().unwrap_or_default();
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for (_, _, backend) in tenants
            .into_iter()
            .filter(|(_, _, backend)| !enabled.contains(backend))
        {
            *counts.entry(backend).or_default() += 1;
        }
        counts.into_iter().collect()
    };
    for (backend, count) in stranded {
        tracing::warn!(
            custody.backend = %backend,
            stores = count,
            enabled_backends = %enabled.join(","),
            "{count} store(s) keep their keys in the {backend:?} key custody backend, which is not enabled. Their \
             payments are NOT being detected until it is enabled again or they move their keys to an enabled backend."
        );
    }

    let key_custody: Arc<dyn KeyCustody> = Arc::<CustodyRouter>::clone(&router);
    let key_custody_backend = router.default_backend();
    let scanner_status = scanner_status::new_scanner_status_map();
    let wallet_handles = Arc::new(RwLock::new(
        register_all_tenants(&store, &key_custody).await,
    ));

    // The database worker: its own connection, on its own thread, for the
    // scanner, webhook delivery and API writes (docs/scanner_microtasks.md).
    let db =
        engine::store::Db::open(&db_path.to_string_lossy(), &store.lock()).unwrap_or_else(|e| {
            eprintln!("failed to start the database worker: {e}");
            std::process::exit(1)
        });

    let read_pool = engine::store::ReadStorePool::open(
        &db_path.to_string_lossy(),
        engine_settings.runtime.load().read_connections,
    )
    .unwrap_or_else(|e| {
        eprintln!("failed to open database read pool: {e}");
        std::process::exit(1)
    });
    let app_state = AppState {
        db: engine::store::Database::from_parts(db.clone(), read_pool, &store.lock()),
        admin_rate_limiter,
        log_store: log_store.clone(),
        engine_token,
        settings: Arc::clone(&engine_settings),
        custody: engine::http::Custody {
            backends: Arc::clone(&key_custody),
            default_backend: key_custody_backend,
            wallet_handles: Arc::clone(&wallet_handles),
        },
        networks: engine::http::Networks {
            daemons: daemons.clone(),
            scanner_status: Arc::clone(&scanner_status),
        },
    };

    let delivery_db = db.clone();
    let delivery_settings = Arc::clone(&engine_settings);
    // Woken by the scanner as soon as it enqueues a webhook.
    let webhook_wake = Arc::new(tokio::sync::Notify::new());
    let delivery_wake = Arc::clone(&webhook_wake);
    supervise("webhook delivery", move || {
        loops::run_webhook_delivery_loop(
            delivery_db.clone(),
            Arc::clone(&delivery_settings),
            Arc::clone(&delivery_wake),
        )
    });

    // One scanner loop per configured network
    // (task 7.4), started and stopped as node settings are saved (task 2.1).
    // Supervised like the loops it starts: if it panics, dropping it stops
    // them, and its restart starts them again.
    let (loops_db, loops_custody, loops_daemons, loops_handles, loops_status, loops_settings) = (
        db.clone(),
        Arc::clone(&key_custody),
        daemons.clone(),
        Arc::clone(&wallet_handles),
        Arc::clone(&scanner_status),
        Arc::clone(&engine_settings),
    );
    supervise("network loop manager", move || {
        loops::manage_network_loops(
            loops_db.clone(),
            Arc::clone(&webhook_wake),
            Arc::clone(&loops_custody),
            loops_daemons.clone(),
            Arc::clone(&loops_handles),
            Arc::clone(&loops_status),
            Arc::clone(&loops_settings),
        )
    });

    // Read once: the listen address is restart-only (decision D8).
    let bind = engine_settings.runtime.load().bind;
    let router = build_router(app_state, MAX_BODY_CEILING);
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .expect("failed to bind server address");
    tracing::info!(server.address = %bind, "engine listening");
    // The engine is private: only monokulo, on this machine or a private
    // network, should ever reach it. Nothing stops an operator binding it
    // elsewhere, but it must not happen by accident.
    if let Ok(local) = listener.local_addr() {
        if !engine::settings::is_private_bind_address(local.ip()) {
            tracing::warn!(
                server.address = %local,
                "the engine is listening on {local}, which is not a loopback or private address. \
                 The engine is meant to be reached only by monokulo; anything that can connect to it can \
                 create tenants and hit its API directly. Set server.bind (ENGINE_SERVER_BIND) to a \
                 loopback or private address such as 127.0.0.1:8443 unless you really mean this."
            );
        }
    }
    // On SIGTERM or Ctrl-C (task 7.11): stop accepting connections and let
    // requests in flight finish, for up to shared::shutdown::GRACE, then exit. The
    // background loops simply stop with the process: every step they take is
    // safe to interrupt (payments are recorded idempotently, a block is only
    // marked scanned after everything in it is recorded, webhooks are marked
    // delivered only after they went out), so the next start carries on.
    let server = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shared::shutdown::signal());
    let serving = tokio::spawn(async move { server.await });
    shared::shutdown::signal().await;
    tracing::info!(grace = ?shared::shutdown::GRACE, "shutting down: finishing requests in flight");
    match tokio::time::timeout(shared::shutdown::GRACE, serving).await {
        Ok(Ok(Ok(()))) => tracing::info!("shut down cleanly"),
        Ok(Ok(Err(e))) => tracing::error!(error = %e, "server error while shutting down"),
        Ok(Err(e)) => tracing::error!(error = %e, "server task failed while shutting down"),
        Err(_) => {
            tracing::warn!(grace = ?shared::shutdown::GRACE, "requests still running after the grace period, exiting anyway");
        }
    }
    // The lines above, and any still on their way, stored (and exported)
    // before the process ends.
    if let Some(telemetry) = telemetry::global() {
        telemetry.flush(shared::shutdown::LOG_FLUSH).await;
    }
}

// `supervise` itself moved to `shared::supervise` (`docs/fx_refactor.md`
// Phase 1.1, imported at the top of this file) so monokulo's own
// background loops (its Coingecko exchange-rate refresh loop) can reuse
// the exact same panic-catching restart shape - see that module's own doc
// comment for the full reasoning (still applies unchanged: a bare
// `tokio::spawn` of an infinite loop panicking would silently kill chain
// scanning/webhook delivery while the HTTP server keeps serving happily,
// with no signal short of a merchant eventually complaining).

/// Eagerly registers every non-disabled tenant's sealed key material with
/// `KeyCustody`, so `AppState::wallet_handles` starts populated rather than relying
/// solely on the lazy on-first-use path in `http::resolve_wallet_handle`.
async fn register_all_tenants(
    store: &SharedStore,
    key_custody: &Arc<dyn KeyCustody>,
) -> HashMap<engine::store::TenantId, WalletHandle> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    // A database error here must not kill the engine at boot: retry with
    // backoff until the store answers, logging each failure.
    let mut delay = Duration::from_millis(500);
    let tenants = loop {
        // Bound first: the lock must not be held through the retry's sleep.
        let listed = {
            let store = Arc::clone(store);
            tokio::task::spawn_blocking(move || store.lock().list_active_tenants())
                .await
                .unwrap_or_else(|e| {
                    Err(engine::store::StoreError::WorkerUnavailable(e.to_string()))
                })
        };
        match listed {
            Ok(tenants) => break tenants,
            Err(e) => {
                tracing::warn!(error = %e, retry_in = ?delay, "failed to list tenants at boot, retrying");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(30));
            }
        }
    };
    let mut handles = HashMap::new();
    for tenant in tenants {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!("boot registration allowance exhausted; remaining tenants will be retried by the scanner");
            break;
        }
        match tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + Duration::from_secs(10)),
            key_custody.unseal_and_register_in_idempotent(
                &tenant.key_custody_backend,
                &tenant.sealed_key_material,
                tenant.id.as_str(),
            ),
        )
        .await
        {
            Ok(Ok(handle)) => {
                handles.insert(tenant.id, handle);
            }
            Ok(Err(e)) => {
                tracing::error!(store.id = %tenant.id, error = %e, "failed to register store with key custody");
            }
            Err(_) => {
                tracing::error!(store.id = %tenant.id, "registering a store with key custody exceeded its deadline");
            }
        }
    }
    handles
}
