//! The standalone engine: read the command line and the options file, start
//! logging and the async runtime, start the engine (`engine::run::Engine`:
//! storage, settings, key custody, the scan and webhook loops), and serve
//! its admin API on `server.bind` until SIGTERM or Ctrl-C.
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

use engine::cli;
use engine::engine_settings::{RuntimeConfig, ALL, LOGGING_FORMAT, LOGGING_LEVEL};
use engine::run::{Engine, EngineConfig, Host, Stopped};
use live_settings::{OptionsFile, Snapshot};

/// Builds the runtime from `server.worker_threads`, `server.cpus` and
/// `server.nice` (`engine::threads`; task 2.8: read
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
    // The engine's threads: how many, on which CPUs, at what niceness
    // (`engine::threads`). A plan this machine can't take stops it here.
    let threads = live_settings::read_sync_with_env::<RuntimeConfig>(Ok(file), &start.env).threads;
    if let Err(e) = threads.check() {
        tracing::error!(error = %e, "the engine's threads can't be set up as asked");
        std::process::exit(1);
    }
    let config = EngineConfig {
        database_path: cli::database_path(&early),
        options: OptionsFile::at(&start.options),
        env: start.env,
        host: Host::Standalone,
    };
    let runtime = match threads.build_runtime() {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::error!(workers = threads.workers, error = %e, "failed to start the async runtime");
            std::process::exit(1);
        }
    };
    runtime.block_on(run(config));
}

#[expect(
    clippy::expect_used,
    reason = "boot-time: a listener that can't bind or a server that can't start ends the process"
)]
async fn run(config: EngineConfig) {
    // CPU and memory every 10 s, for the admin page (docs/engine_scaling.md
    // 6): the process's, so its host samples it, not the engine.
    shared::resources::start_sampling();

    let engine = match Engine::start(config).await {
        Ok(engine) => engine,
        Err(e) => {
            tracing::error!("{e}");
            std::process::exit(1);
        }
    };

    let bind = engine.bind_address();
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .expect("failed to bind server address");
    // The address listened on, as bound: a port of 0 (any free port) says
    // which one it got.
    let local = listener.local_addr();
    if let Ok(local) = &local {
        tracing::info!(server.address = %local, "engine listening");
    } else {
        tracing::info!(server.address = %bind, "engine listening");
    }
    // The engine is private: only monokulo, on this machine or a private
    // network, should ever reach it. Nothing stops an operator binding it
    // elsewhere, but it must not happen by accident.
    if let Ok(local) = local {
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
    // requests in flight finish, for up to shared::shutdown::GRACE, then stop
    // the engine's loops.
    let server = axum::serve(
        listener,
        engine
            .router()
            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
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
    if engine.shutdown(shared::shutdown::GRACE).await == Stopped::TimedOut {
        tracing::warn!(grace = ?shared::shutdown::GRACE, "the engine's loops were still stopping after the grace period, exiting anyway");
    }
    // The lines above, and any still on their way, stored (and exported)
    // before the process ends.
    if let Some(telemetry) = telemetry::global() {
        telemetry.flush(shared::shutdown::LOG_FLUSH).await;
    }
}
