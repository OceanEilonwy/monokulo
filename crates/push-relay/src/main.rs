//! `monokulo-push-relay`, the push relay on its own: read the command line
//! and the options file, start logging, load and check every setting,
//! open the handles' database and serve on `server.listen` until SIGTERM
//! or Ctrl-C.
//!
//! Configuration comes from the options file (`--options`, else
//! `~/.config/monokulo/relay.toml`) or its command-line option, which
//! wins; the APNs key and the FCM service account from the environment
//! only. Anything invalid stops the relay before it listens.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
// A failure before logging is set up is told on stderr, and ends the
// process with a failing status.
#![expect(
    clippy::print_stderr,
    clippy::exit,
    reason = "boot failures are reported on stderr and end the process"
)]

use live_settings::{OptionsFile, Snapshot};
use push_relay::settings::{self, Host, LOGGING_FORMAT, LOGGING_LEVEL};
use push_relay::Relay;

/// Ends the process at start, saying why: in the log once it is set up,
/// on stderr before.
fn stop(message: impl std::fmt::Display) -> ! {
    if telemetry::global().is_some() {
        tracing::error!("{message}");
    } else {
        eprintln!("{message}");
    }
    std::process::exit(1);
}

fn main() {
    // `--help`, `--version` and a mistyped option end here.
    let start = push_relay::cli::parse_args(std::env::args_os()).unwrap_or_else(|e| e.exit());
    if start.init {
        match live_settings::cli::init("monokulo-push-relay", &start.options, settings::ALL) {
            Ok(()) => std::process::exit(0),
            Err(e) => stop(e),
        }
    }
    // Anything wrong in the file stops the relay here, line by line.
    let values = OptionsFile::at(&start.options)
        .read(settings::ALL)
        .unwrap_or_else(|e| stop(e));
    let early = Snapshot::new(values.clone(), start.env.clone());
    let telemetry = telemetry::init_with(
        "push-relay",
        &early.get(&LOGGING_LEVEL),
        telemetry::Format::chosen(early.get(&LOGGING_FORMAT)),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| stop(format!("failed to start the async runtime: {e}")));
    runtime.block_on(async {
        let config = settings::load(values, start.env, Host::Standalone)
            .await
            .unwrap_or_else(|e| stop(e));
        // On SIGTERM or Ctrl-C: stop accepting connections, and let
        // requests in flight finish for up to the grace period. One stop
        // for the process, which the relay's sweep, the server and the
        // wait below all see.
        let stopping = service_runtime::shutdown::on_signal();
        let relay = Relay::start(&config, stopping.clone())
            .await
            .unwrap_or_else(|e| stop(format!("the push relay didn't start: {e}")));
        let listener = tokio::net::TcpListener::bind(config.listen)
            .await
            .unwrap_or_else(|e| stop(format!("failed to listen on {}: {e}", config.listen)));
        let address = listener.local_addr().unwrap_or(config.listen);
        tracing::info!(server.address = %address, "push relay listening");
        let serving = {
            let (relay, stopping) = (relay.clone(), stopping.clone());
            tokio::spawn(async move { relay.serve(listener, stopping.cancelled_owned()).await })
        };
        stopping.cancelled().await;
        tracing::info!(grace = ?service_runtime::shutdown::GRACE, "shutting down: finishing requests in flight");
        match tokio::time::timeout(service_runtime::shutdown::GRACE, serving).await {
            Ok(Ok(Ok(()))) => tracing::info!("shut down cleanly"),
            Ok(Ok(Err(e))) => tracing::error!(error = %e, "server error while shutting down"),
            Ok(Err(e)) => tracing::error!(error = %e, "server task failed while shutting down"),
            Err(_) => tracing::warn!(grace = ?service_runtime::shutdown::GRACE, "requests still running after the grace period, exiting anyway"),
        }
        relay.stopped().await;
        telemetry.flush(service_runtime::shutdown::LOG_FLUSH).await;
    });
}
