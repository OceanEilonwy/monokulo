//! `monokular-bell`: read the command line and the options file, start
//! logging, load and check every setting, open the registrations' database
//! and serve on `bell.listen` until SIGTERM or Ctrl-C, then let requests
//! and the dings they started finish.
//!
//! Configuration comes from the options file (`--options`, else
//! `~/.config/monokulo/bell.toml`) or its command-line option, which
//! wins; the Square webhook key and the VAPID key from the environment
//! only. Anything invalid stops the Bell before it listens.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::io::IsTerminal as _;
use std::time::Instant;

use live_settings::{OptionsFile, Snapshot};
use monokular_bell::Bell;
use monokular_bell::settings::{self, LOGGING_LEVEL};
use service_runtime::shutdown::GRACE;
use tracing_subscriber::EnvFilter;

/// Ends the process at start, saying why: in the log once it is set up,
/// on stderr before.
fn stop(message: impl std::fmt::Display) -> ! {
    if tracing::dispatcher::has_been_set() {
        tracing::error!("{message}");
    } else {
        eprintln!("{message}");
    }
    std::process::exit(1);
}

fn main() {
    // `--help`, `--version` and a mistyped option end here.
    let start = monokular_bell::cli::parse_args(std::env::args_os()).unwrap_or_else(|e| e.exit());
    if start.init {
        match live_settings::cli::init("monokular-bell", &start.options, settings::ALL) {
            Ok(()) => std::process::exit(0),
            Err(e) => stop(e),
        }
    }
    // Anything wrong in the file stops the Bell here, line by line.
    let values = OptionsFile::at(&start.options)
        .read(settings::ALL)
        .unwrap_or_else(|e| stop(e));
    let early = Snapshot::new(values.clone(), start.env.clone());
    // A level that doesn't parse is reported by the registry below; until
    // then the default stands in.
    let filter = EnvFilter::builder()
        .parse(early.get(&LOGGING_LEVEL).trim())
        .unwrap_or_else(|_| EnvFilter::new(LOGGING_LEVEL.default_value()));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        // Colours at a terminal only, not in a service manager's journal.
        .with_ansi(std::io::stderr().is_terminal())
        .init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| stop(format!("failed to start the async runtime: {e}")));
    runtime.block_on(async {
        let config = settings::load(values, start.env)
            .await
            .unwrap_or_else(|e| stop(e));
        let bell = Bell::start(&config)
            .await
            .unwrap_or_else(|e| stop(format!("the Bell didn't start: {e}")));
        let listener = tokio::net::TcpListener::bind(config.listen)
            .await
            .unwrap_or_else(|e| stop(format!("failed to listen on {}: {e}", config.listen)));
        let address = listener.local_addr().unwrap_or(config.listen);
        tracing::info!(server.address = %address, "the Bell is listening");
        // On SIGTERM or Ctrl-C: stop accepting connections, and let
        // requests in flight and the dings they started finish, for up to
        // the grace period together.
        let stopping = service_runtime::shutdown::on_signal();
        let serving = {
            let bell = bell.clone();
            let stopping = stopping.clone();
            tokio::spawn(async move { bell.serve(listener, stopping.cancelled_owned()).await })
        };
        stopping.cancelled().await;
        let deadline = Instant::now() + GRACE;
        tracing::info!(grace = ?GRACE, "shutting down: finishing requests and dings in flight");
        let finished = tokio::time::timeout_at(deadline.into(), async {
            let served = serving.await;
            bell.dings_sent().await;
            served
        })
        .await;
        match finished {
            Ok(Ok(Ok(()))) => tracing::info!("shut down cleanly"),
            Ok(Ok(Err(e))) => tracing::error!(error = %e, "server error while shutting down"),
            Ok(Err(e)) => tracing::error!(error = %e, "server task failed while shutting down"),
            Err(_) => tracing::warn!(grace = ?GRACE, "requests or dings still running after the grace period, exiting anyway"),
        }
    });
}
