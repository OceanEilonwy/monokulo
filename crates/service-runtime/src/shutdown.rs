//! Stopping a server process: the signal to stop on, and how long the
//! steps after it get.

use std::time::Duration;

/// How long requests in flight get to finish after SIGTERM or Ctrl-C.
pub const GRACE: Duration = Duration::from_secs(10);

/// How long a stopping process waits for its last lines to be stored and
/// exported (`telemetry::Telemetry::flush`).
pub const LOG_FLUSH: Duration = Duration::from_secs(5);

/// Resolves on SIGTERM (what a service manager sends) or Ctrl-C.
pub async fn signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %e, "could not listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
