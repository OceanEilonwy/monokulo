//! A generic panic-catching supervisor for a long-running background loop -
//! moved here from the engine's own `src/main.rs` (`docs/fx_refactor.md`
//! Phase 1.1) so monokulo's own background loops (its Coingecko
//! exchange-rate refresh loop, first) can reuse the exact same
//! catch-panic-and-restart shape instead of reimplementing it.
//!
//! `make_loop` is a factory, not the loop itself: the loop is expected to
//! run forever (a real return is a bug, logged as one), and if it panics,
//! `supervise` needs a *fresh* future to retry with - a `Future` can only
//! ever be polled to completion once, so there is no way to "rewind" an
//! already-panicked one.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use parking_lot::Mutex;

/// First restart delay, doubled for each restart in a row, up to
/// `MAX_BACKOFF`, so a loop that fails straight away every time doesn't spin.
const FIRST_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);
/// A loop that ran at least this long before failing is treated as having
/// been healthy: the next restart waits only `FIRST_BACKOFF` again.
const HEALTHY_RUN: Duration = Duration::from_secs(10 * 60);

static RESTARTS: LazyLock<Mutex<HashMap<&'static str, u64>>> = LazyLock::new(Default::default);

/// How many times each supervised loop has been restarted since the process
/// started, for status pages. Loops that never failed aren't listed.
pub fn restart_counts() -> Vec<(&'static str, u64)> {
    let mut counts: Vec<_> = RESTARTS
        .lock()
        .iter()
        .map(|(name, count)| (*name, *count))
        .collect();
    counts.sort();
    counts
}

/// Panic in `make_loop`'s own returned future is caught (not propagated) and
/// logged loudly, then the loop is restarted after a backoff that doubles
/// for each failure in a row (5s, 10s, 20s, ... up to 5 minutes) and resets
/// once a run has stayed up for 10 minutes. A plain return (the loop ending
/// normally) is itself treated as a bug, since every caller's own loop is
/// meant to run forever. The one case that does *not* restart is the task
/// being cancelled (e.g. the whole process shutting down) - retrying that
/// would fight the shutdown rather than respect it. Each restart is counted
/// (`restart_counts`).
pub fn supervise<F, Fut>(name: &'static str, make_loop: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        let mut backoff = FIRST_BACKOFF;
        loop {
            let started = tokio::time::Instant::now();
            let outcome = match make_future(name, &make_loop) {
                Some(future) => tokio::spawn(future).await,
                None => Ok(()),
            };
            match outcome {
                Ok(()) => {
                    tracing::error!(task = name, restart_in = ?backoff, "BUG: loop returned; it is not supposed to terminate. Restarting")
                }
                Err(e) if e.is_panic() => {
                    tracing::error!(task = name, error = %e, restart_in = ?backoff, "loop PANICKED. None of its work is happening until it restarts");
                }
                Err(e) => {
                    tracing::warn!(task = name, error = %e, "loop was cancelled. Not restarting");
                    return;
                }
            }
            *RESTARTS.lock().entry(name).or_default() += 1;
            if started.elapsed() >= HEALTHY_RUN {
                backoff = FIRST_BACKOFF;
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    });
}

/// The loop's future from its factory. A panic in the factory itself (what
/// the closure computes before returning the future) is caught and logged
/// like a panic in the loop, so the supervisor restarts it rather than
/// dying in silence with it: `None` means "treat as panicked".
fn make_future<F, Fut>(name: &'static str, make_loop: &F) -> Option<Fut>
where
    F: Fn() -> Fut,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(make_loop)) {
        Ok(future) => Some(future),
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("(no message)");
            tracing::error!(
                task = name,
                error = message,
                "loop could not be started: its factory PANICKED. Restarting"
            );
            None
        }
    }
}

/// `supervise`, until `stop` becomes `true` (or its sender is dropped):
/// then the running loop is aborted and not restarted. For loops that exist
/// only while something is configured, such as one network's scanner, which
/// stops when that network's node setting is cleared (admin_settings_v2.md
/// task 2.1), and for an engine's own loops, which stop with it
/// (`engine::run::Engine::shutdown`). The returned task ends once the loop
/// is gone, so a caller can wait for that.
pub fn supervise_until<F, Fut>(
    name: &'static str,
    mut stop: tokio::sync::watch::Receiver<bool>,
    make_loop: F,
) -> tokio::task::JoinHandle<()>
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        let mut backoff = FIRST_BACKOFF;
        loop {
            if *stop.borrow() {
                return;
            }
            let started = tokio::time::Instant::now();
            let Some(future) = make_future(name, &make_loop) else {
                *RESTARTS.lock().entry(name).or_default() += 1;
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = stop.wait_for(|stopped| *stopped) => return,
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            };
            let mut running = tokio::spawn(future);
            let outcome = tokio::select! {
                outcome = &mut running => Some(outcome),
                _ = stop.wait_for(|stopped| *stopped) => None,
            };
            match outcome {
                Some(Ok(())) => {
                    tracing::error!(task = name, restart_in = ?backoff, "BUG: loop returned; it is not supposed to terminate. Restarting")
                }
                Some(Err(e)) if e.is_panic() => {
                    tracing::error!(task = name, error = %e, restart_in = ?backoff, "loop PANICKED. None of its work is happening until it restarts");
                }
                Some(Err(e)) => {
                    tracing::warn!(task = name, error = %e, "loop was cancelled. Not restarting");
                    return;
                }
                None => {
                    running.abort();
                    // Gone, not just asked to go, when this task ends.
                    let _ = running.await;
                    return;
                }
            }
            *RESTARTS.lock().entry(name).or_default() += 1;
            if started.elapsed() >= HEALTHY_RUN {
                backoff = FIRST_BACKOFF;
            }
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = stop.wait_for(|stopped| *stopped) => return,
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    /// A background loop (the scanner's, the rate refresh) that panics is started again after the backoff, and one that
    /// returns is too: nothing stays silently stopped.
    #[tokio::test(start_paused = true)]
    async fn a_loop_that_panics_or_returns_is_started_again() {
        let starts = Arc::new(AtomicUsize::new(0));
        let counter = starts.clone();
        supervise("test", move || {
            let run = counter.fetch_add(1, Ordering::SeqCst);
            async move {
                match run {
                    0 => panic!("first run fails"),
                    1 => {}
                    _ => std::future::pending::<()>().await,
                }
            }
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(
            starts.load(Ordering::SeqCst),
            2,
            "restarted after the panic"
        );
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(
            starts.load(Ordering::SeqCst),
            3,
            "restarted after returning, with the backoff doubled"
        );
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(
            starts.load(Ordering::SeqCst),
            3,
            "a loop that keeps running is left alone"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_loop_that_keeps_failing_backs_off_and_is_counted() {
        let starts = Arc::new(AtomicUsize::new(0));
        let counter = starts.clone();
        supervise("backoff-test", move || {
            counter.fetch_add(1, Ordering::SeqCst);
            async move { panic!("always fails") }
        });
        // Starts at 0s, then 5s, 15s, 35s, 75s later.
        for (wait_secs, expected) in [(1, 1), (5, 2), (10, 3), (20, 4), (40, 5)] {
            tokio::time::sleep(Duration::from_secs(wait_secs)).await;
            assert_eq!(
                starts.load(Ordering::SeqCst),
                expected,
                "after waiting {wait_secs}s more"
            );
        }
        let restarts = restart_counts()
            .into_iter()
            .find(|(name, _)| *name == "backoff-test")
            .map(|(_, n)| n);
        assert!(restarts >= Some(4), "got {restarts:?}");
        // Capped: it never waits longer than 5 minutes.
        tokio::time::sleep(Duration::from_secs(60 * 60)).await;
        assert!(
            starts.load(Ordering::SeqCst) >= 5 + 11,
            "at most 5 minutes between restarts once capped"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_loop_supervised_until_stopped_is_aborted_and_not_restarted() {
        let starts = Arc::new(AtomicUsize::new(0));
        let counter = starts.clone();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        supervise_until("until-test", stopped, move || {
            counter.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>()
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        stop.send(true).unwrap();
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert_eq!(starts.load(Ordering::SeqCst), 1, "stopped for good");
    }

    /// Whoever stops a supervised loop can wait for it: the returned task
    /// ends once the loop itself has been dropped, not merely asked to stop.
    /// An engine's shutdown waits on exactly this.
    #[tokio::test]
    async fn the_supervisor_ends_only_once_its_stopped_loop_is_gone() {
        /// Set when the loop's future is dropped.
        struct Dropped(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = gone.clone();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let supervisor = supervise_until("until-join-test", stopped, move || {
            let guard = Dropped(flag.clone());
            async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            }
        });
        tokio::task::yield_now().await;
        assert!(!supervisor.is_finished());

        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), supervisor)
            .await
            .expect("the supervisor ends when stopped")
            .unwrap();
        assert!(gone.load(Ordering::SeqCst), "the loop was dropped first");
    }

    /// Dropping the stop signal's sender stops the loop too: that is how a
    /// network's loops end when the manager holding their senders stops.
    #[tokio::test]
    async fn dropping_the_stop_sender_also_stops_the_loop() {
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let supervisor = supervise_until("until-drop-test", stopped, std::future::pending::<()>);
        tokio::task::yield_now().await;
        drop(stop);
        tokio::time::timeout(Duration::from_secs(5), supervisor)
            .await
            .expect("the supervisor ends when its sender goes")
            .unwrap();
    }
}
