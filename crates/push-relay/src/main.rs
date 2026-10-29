//! `monokulo-push-relay`, the push relay on its own: read the command line
//! and the options file, start logging, load and check every setting,
//! open the handles' database and serve on `server.listen`, with its
//! status on `status.listen` for this machine, until SIGTERM or Ctrl-C.
//! `--check` stops after the checks, saying what it would run on.
//!
//! Configuration comes from the options file (`--options`, else
//! `~/.config/monokulo/relay.toml`) or its command-line option, which
//! wins; the APNs key and the FCM service account from the environment
//! only. Anything invalid stops the relay before it listens.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
// A failure before logging is set up is told on stderr, and ends the
// process with a failing status; `--check` says what it found on stdout.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::exit,
    reason = "boot failures are reported on stderr and end the process, and --check reports on stdout"
)]

use std::collections::HashMap;
use std::net::SocketAddr;

use live_settings::{Env, OptionsFile, Snapshot};
use push_relay::settings::{self, Standalone, LOGGING_FORMAT, LOGGING_LEVEL};
use push_relay::{Relay, StartError};

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

/// A start error as the relay says it, at start and in `--check` alike.
fn didnt_start(error: StartError) -> String {
    format!("the push relay didn't start: {error}")
}

/// `--check`: the settings loaded and checked as a start does them, with
/// nothing opened or bound; what the relay would run on, then `OK`, or the
/// error a start would stop with.
fn check(values: HashMap<String, String>, env: Env) -> ! {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| stop(format!("failed to start the async runtime: {e}")));
    let checked = runtime.block_on(async {
        let Standalone { relay, status } = settings::load_standalone(values, env)
            .await
            .map_err(|e| e.to_string())?;
        let mut facts = Relay::check(&relay).map_err(didnt_start)?.facts();
        facts.push(match status.listen {
            Some(address) => format!("status on {address}"),
            None => "status: off".to_owned(),
        });
        Ok::<_, String>(facts)
    });
    match checked {
        Ok(facts) => {
            for fact in facts {
                println!("{fact}");
            }
            println!("OK");
            std::process::exit(0);
        }
        Err(error) => stop(error),
    }
}

/// A listener on `address`, or the process stops saying which (`what`).
async fn bind(address: SocketAddr, what: &str) -> tokio::net::TcpListener {
    tokio::net::TcpListener::bind(address)
        .await
        .unwrap_or_else(|e| stop(format!("failed to listen on {address}{what}: {e}")))
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
    if start.check {
        check(values, start.env);
    }
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
        let Standalone {
            relay: config,
            status,
        } = settings::load_standalone(values, start.env)
            .await
            .unwrap_or_else(|e| stop(e));
        // On SIGTERM or Ctrl-C: stop accepting connections, and let
        // requests in flight finish for up to the grace period. One stop
        // for the process, which the relay's sweep, the servers and the
        // wait below all see.
        let stopping = service_runtime::shutdown::on_signal();
        let relay = Relay::start(&config, stopping.clone())
            .await
            .unwrap_or_else(|e| stop(didnt_start(e)));
        let listener = bind(config.listen, "").await;
        let address = listener.local_addr().unwrap_or(config.listen);
        let status_listener = match status.listen {
            Some(at) => Some(bind(at, " for the status").await),
            None => None,
        };
        tracing::info!(server.address = %address, "push relay listening");
        let started_at = service_runtime::time::now_unix();
        let status_serving = status_listener.map(|listener| {
            if let Ok(at) = listener.local_addr() {
                tracing::info!(status.address = %at, "push relay status listening");
            }
            let relay = relay.clone();
            let report = move || {
                let relay = relay.clone();
                async move { relay.report(address, started_at).await }
            };
            tokio::spawn(service_runtime::status::serve(
                listener,
                report,
                stopping.clone().cancelled_owned(),
            ))
        });
        let serving = {
            let (relay, stopping) = (relay.clone(), stopping.clone());
            tokio::spawn(async move { relay.serve(listener, stopping.cancelled_owned()).await })
        };
        stopping.cancelled().await;
        tracing::info!(grace = ?service_runtime::shutdown::GRACE, "shutting down: finishing requests in flight");
        let finished = tokio::time::timeout(service_runtime::shutdown::GRACE, async {
            let served = serving.await;
            if let Some(status) = status_serving {
                match status.await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!(error = %e, "status server error while shutting down"),
                    Err(e) => tracing::warn!(error = %e, "status server task failed while shutting down"),
                }
            }
            served
        })
        .await;
        match finished {
            Ok(Ok(Ok(()))) => tracing::info!("shut down cleanly"),
            Ok(Ok(Err(e))) => tracing::error!(error = %e, "server error while shutting down"),
            Ok(Err(e)) => tracing::error!(error = %e, "server task failed while shutting down"),
            Err(_) => tracing::warn!(grace = ?service_runtime::shutdown::GRACE, "requests still running after the grace period, exiting anyway"),
        }
        relay.stopped().await;
        telemetry.flush(service_runtime::shutdown::LOG_FLUSH).await;
    });
}
