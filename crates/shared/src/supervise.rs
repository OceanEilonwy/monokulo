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

use std::time::Duration;

/// Panic in `make_loop`'s own returned future is caught (not propagated) and
/// logged loudly, then the loop is restarted after a fixed backoff; a plain
/// `Ok` return (the loop ending normally) is itself treated as a bug, since
/// every caller's own loop is meant to run forever. The one case that does
/// *not* restart is the task being cancelled (e.g. the whole process
/// shutting down) - retrying that would fight the shutdown rather than
/// respect it.
pub fn supervise<F, Fut>(name: &'static str, make_loop: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            // The inner spawn is what makes the panic catchable: a panic propagating
            // through `.await` in *this* task would kill the supervisor too.
            match tokio::spawn(make_loop()).await {
                Ok(()) => eprintln!("BUG: {name} loop returned; it is not supposed to terminate. Restarting in 5s."),
                Err(e) if e.is_panic() => {
                    eprintln!("FATAL: {name} loop PANICKED: {e}. No {name} work is happening until it restarts. Restarting in 5s.");
                }
                Err(e) => {
                    eprintln!("{name} loop was cancelled: {e}. Not restarting.");
                    return;
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
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
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(starts.load(Ordering::SeqCst), 3, "restarted after returning");
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(starts.load(Ordering::SeqCst), 3, "a loop that keeps running is left alone");
    }
}
