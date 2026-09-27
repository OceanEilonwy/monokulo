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
    let mut counts: Vec<_> = RESTARTS.lock().iter().map(|(name, count)| (*name, *count)).collect();
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
            match tokio::spawn(make_loop()).await {
                Ok(()) => eprintln!("BUG: {name} loop returned; it is not supposed to terminate. Restarting in {backoff:?}."),
                Err(e) if e.is_panic() => {
                    eprintln!("FATAL: {name} loop PANICKED: {e}. No {name} work is happening until it restarts. Restarting in {backoff:?}.");
                }
                Err(e) => {
                    eprintln!("{name} loop was cancelled: {e}. Not restarting.");
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

/// `supervise`, until `stop` becomes `true` (or its sender is dropped):
/// then the running loop is aborted and not restarted. For loops that exist
/// only while something is configured, such as one network's scanner, which
/// stops when that network's node setting is cleared (admin_settings_v2.md
/// task 2.1).
pub fn supervise_until<F, Fut>(name: &'static str, mut stop: tokio::sync::watch::Receiver<bool>, make_loop: F)
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
            let mut running = tokio::spawn(make_loop());
            tokio::select! {
                outcome = &mut running => {
                    match outcome {
                        Ok(()) => eprintln!("BUG: {name} loop returned; it is not supposed to terminate. Restarting in {backoff:?}."),
                        Err(e) if e.is_panic() => {
                            eprintln!("FATAL: {name} loop PANICKED: {e}. No {name} work is happening until it restarts. Restarting in {backoff:?}.");
                        }
                        Err(e) => {
                            eprintln!("{name} loop was cancelled: {e}. Not restarting.");
                            return;
                        }
                    }
                }
                _ = stop.wait_for(|stopped| *stopped) => {
                    running.abort();
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
    });
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    /// A background loop (the scanner's, webhook delivery's, the rate
    /// refresh) that panics is started again after the backoff, and one that
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
        assert_eq!(starts.load(Ordering::SeqCst), 2, "restarted after the panic");
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(starts.load(Ordering::SeqCst), 3, "restarted after returning, with the backoff doubled");
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(starts.load(Ordering::SeqCst), 3, "a loop that keeps running is left alone");
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
            assert_eq!(starts.load(Ordering::SeqCst), expected, "after waiting {wait_secs}s more");
        }
        let restarts = restart_counts().into_iter().find(|(name, _)| *name == "backoff-test").map(|(_, n)| n);
        assert!(restarts >= Some(4), "got {restarts:?}");
        // Capped: it never waits longer than 5 minutes.
        tokio::time::sleep(Duration::from_secs(60 * 60)).await;
        assert!(starts.load(Ordering::SeqCst) >= 5 + 11, "at most 5 minutes between restarts once capped");
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
}
