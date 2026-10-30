//! `monokulo-bell`: read the command line and the options file, start
//! logging, load and check every setting, open the registrations' database
//! and serve on `bell.listen`, with its status on `status.listen` for this
//! machine, until SIGTERM or Ctrl-C, then let requests and the dings they
//! started finish. `--check` stops after the checks, saying what it would
//! run on.
//!
//! Configuration comes from the options file (`--options`, else
//! `~/.config/monokulo/bell.toml`) or its command-line option, which
//! wins; the points of sale's secrets and the VAPID key from the
//! environment only. Anything invalid stops the Bell before it listens.

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
use std::time::Instant;

use live_settings::{Env, OptionsFile, Snapshot};
use monokulo_bell::settings::{self, LOGGING_FORMAT, LOGGING_LEVEL, Loaded};
use monokulo_bell::{Bell, StartError};
use service_runtime::shutdown::{GRACE, LOG_FLUSH};

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

/// A start error as the Bell says it, at start and in `--check` alike.
fn didnt_start(error: StartError) -> String {
    format!("the Bell didn't start: {error}")
}

/// `--check`: the settings loaded and checked as a start does them, with
/// nothing opened or bound; what the Bell would run on (a secret still
/// under its old name said first), then `OK`, or the error a start would
/// stop with.
fn check(values: HashMap<String, String>, env: Env) -> ! {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| stop(format!("failed to start the async runtime: {e}")));
    let checked = runtime.block_on(async {
        let mut facts: Vec<String> = env
            .renamed_given()?
            .into_iter()
            .map(|warning| format!("warning: {warning}"))
            .collect();
        let Loaded { bell, status } = settings::load(values, env)
            .await
            .map_err(|e| e.to_string())?;
        facts.extend(Bell::check(&bell).map_err(didnt_start)?.facts());
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
    let start = monokulo_bell::cli::parse_args(std::env::args_os()).unwrap_or_else(|e| e.exit());
    if start.init {
        match live_settings::cli::init("monokulo-bell", &start.options, settings::ALL) {
            Ok(()) => std::process::exit(0),
            Err(e) => stop(e),
        }
    }
    // Anything wrong in the file stops the Bell here, line by line, a
    // setting replaced since (decision 76) with what replaced it.
    let values = OptionsFile::at(&start.options)
        .with_renamed(settings::REPLACED)
        .read(settings::ALL)
        .unwrap_or_else(|e| stop(e));
    if start.check {
        check(values, start.env);
    }
    let early = Snapshot::new(values.clone(), start.env.clone());
    // A level that doesn't parse is reported by the registry below; until
    // then telemetry's default stands in.
    let telemetry = telemetry::init_with(
        "monokulo-bell",
        &early.get(&LOGGING_LEVEL),
        telemetry::Format::chosen(early.get(&LOGGING_FORMAT)),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| stop(format!("failed to start the async runtime: {e}")));
    // A secret still under its monokular-bell name is read, and said; under
    // both names with different values, the Bell can't tell which is meant.
    match start.env.renamed_given() {
        Ok(warnings) => warnings
            .iter()
            .for_each(|warning| tracing::warn!("{warning}")),
        Err(e) => stop(e),
    }
    runtime.block_on(async {
        let Loaded {
            bell: config,
            status,
        } = settings::load(values, start.env)
            .await
            .unwrap_or_else(|e| stop(e));
        let bell = Bell::start(&config)
            .await
            .unwrap_or_else(|e| stop(didnt_start(e)));
        let listener = bind(config.listen, "").await;
        let address = listener.local_addr().unwrap_or(config.listen);
        let status_listener = match status.listen {
            Some(at) => Some(bind(at, " for the status").await),
            None => None,
        };
        tracing::info!(server.address = %address, "the Bell is listening");
        let started_at = service_runtime::time::now_unix();
        // On SIGTERM or Ctrl-C: stop accepting connections, and let
        // requests in flight and the dings they started finish, for up to
        // the grace period together.
        let stopping = service_runtime::shutdown::on_signal();
        let status_serving = status_listener.map(|listener| {
            if let Ok(at) = listener.local_addr() {
                tracing::info!(status.address = %at, "the Bell's status is listening");
            }
            let bell = bell.clone();
            let report = move || {
                let bell = bell.clone();
                async move { bell.report(address, started_at).await }
            };
            tokio::spawn(service_runtime::status::serve(
                listener,
                report,
                stopping.clone().cancelled_owned(),
            ))
        });
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
            Err(_) => tracing::warn!(grace = ?GRACE, "requests or dings still running after the grace period, exiting anyway"),
        }
        telemetry.flush(LOG_FLUSH).await;
    });
}
