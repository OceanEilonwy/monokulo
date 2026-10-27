//! Stopping a server process: the signal to stop on, and how long the
//! steps after it get.

use std::future::Future;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// How long requests in flight get to finish after SIGTERM or Ctrl-C.
pub const GRACE: Duration = Duration::from_secs(10);

/// How long a stopping process waits for its last lines to be stored and
/// exported (`telemetry::Telemetry::flush`).
pub const LOG_FLUSH: Duration = Duration::from_secs(5);

/// The process's stop: cancelled on SIGTERM (what a service manager
/// sends) or Ctrl-C. Made once, at the start, before anything that must
/// stop with the process, and handed to each part, which waits on its own
/// clone: a token stays cancelled, so a part that starts waiting after the
/// signal still sees it. A second listener for the signal itself would
/// not (each hears only signals after it is made), and the first one
/// made would already have taken over the default of ending the process.
///
/// The handlers are installed before this returns. Called inside a Tokio
/// runtime.
pub fn on_signal() -> CancellationToken {
    let stop = CancellationToken::new();
    let signal = signal();
    let cancel = stop.clone();
    tokio::spawn(async move {
        signal.await;
        cancel.cancel();
    });
    stop
}

/// SIGTERM or Ctrl-C, listened for from now on.
fn signal() -> impl Future<Output = ()> + Send + 'static {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, Signal, SignalKind};
        let listen = |kind: SignalKind, name: &'static str| {
            signal(kind)
                .inspect_err(|e| tracing::warn!(error = %e, "could not listen for {name}"))
                .ok()
        };
        let received = |signal: Option<Signal>| async move {
            match signal {
                Some(mut signal) => {
                    signal.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        let interrupt = received(listen(SignalKind::interrupt(), "Ctrl-C"));
        let terminate = received(listen(SignalKind::terminate(), "SIGTERM"));
        async move {
            tokio::select! {
                () = interrupt => {}
                () = terminate => {}
            }
        }
    }
    #[cfg(not(unix))]
    {
        async {
            if let Err(e) = tokio::signal::ctrl_c().await {
                tracing::warn!(error = %e, "could not listen for Ctrl-C");
                std::future::pending::<()>().await;
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A SIGTERM that comes before a part waits for the stop (monokulo
    /// still starting, say) is seen when it does. nextest runs each test
    /// in its own process, and the handler is in place before the signal.
    #[tokio::test]
    async fn a_signal_before_a_part_waits_for_the_stop_is_still_seen() {
        let stop = on_signal();
        let raised = std::process::Command::new("kill")
            .args(["-TERM", &std::process::id().to_string()])
            .status()
            .unwrap();
        assert!(raised.success());
        tokio::time::timeout(Duration::from_secs(5), stop.cancelled())
            .await
            .expect("the signal stopped the process's token");
        // A part started after the signal.
        tokio::time::timeout(Duration::from_secs(1), stop.clone().cancelled_owned())
            .await
            .expect("a later part sees the stop that came before it");
    }
}
