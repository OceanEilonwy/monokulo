//! A generic, fixed-window per-key rate limiter - moved here (from the
//! engine's own `src/http/rate_limit.rs`, WBS `fx_refactor.md` Phase 0.1) so
//! the control-plane can use the exact same, already-proven limiter for its
//! own new public endpoints rather than reimplementing it. The HTTP-specific
//! parts (axum middleware, which key to extract from a request) stay in
//! each crate that actually serves HTTP - this module is pure logic with no
//! HTTP/axum dependency at all.
//!
//! Fixed-window counters, not a proper token bucket - simpler, and
//! sufficient for "stop one source from hammering this endpoint," which is
//! the actual goal. A smarter algorithm is a reasonable future improvement,
//! not a v1 requirement.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::Mutex;

/// Window length. Also the age past which an entry carries no information: a bucket
/// whose window started more than this long ago resets to zero on its next lookup,
/// so keeping it is indistinguishable from having never seen that address.
const WINDOW_SECONDS: i64 = 60;

/// How many entries may accumulate before a `check` sweeps expired ones out. Sized
/// so a sweep is rare under any plausible legitimate load (an instance genuinely
/// serving 10k distinct client addresses inside one minute is not what this is
/// protecting against) while still capping what an attacker can force the map to
/// hold: a botnet, or a flood from a spoofable source, would otherwise grow it
/// without bound for the lifetime of the process, since nothing else ever removes an
/// entry.
const PRUNE_THRESHOLD: usize = 10_000;

/// Absolute ceiling on tracked addresses. Sweeping only removes *expired* entries,
/// so it is no defence against a flood of distinct addresses arriving inside a
/// single window - those entries are all live, and every one of them is load-bearing
/// for someone's budget. When even a fresh sweep can't get under this, the map is
/// dropped wholesale: everyone's window restarts, which under an attack of that
/// scale costs one window of accounting and is strictly better than an unbounded
/// allocation in a process that also holds customer funds' worth of state. Chosen as
/// a multiple of the sweep threshold so the wholesale drop is only ever reached
/// after sweeping has demonstrably failed.
const MAX_TRACKED_ADDRESSES: usize = PRUNE_THRESHOLD * 4;

/// Generic over the bucket key - `IpAddr` for a per-IP limiter, `String` (a
/// raw presented token) for a per-token one. The windowing/pruning/ceiling
/// logic is identical either way; only what identifies "one caller"
/// differs.
pub struct RateLimiter<K = IpAddr> {
    limit_per_minute: u32,
    state: Mutex<LimiterState<K>>,
}

struct LimiterState<K> {
    buckets: HashMap<K, (u32, i64)>, // (count, window_start_unix)
    /// Earliest time a sweep may run again. Without this, a sustained flood that
    /// holds the map above `PRUNE_THRESHOLD` with entries too fresh to remove would
    /// make every single request pay an O(n) scan - converting a memory problem into
    /// a CPU one. One sweep per second bounds that cost regardless of request rate.
    next_prune_at: i64,
}

impl<K> Default for LimiterState<K> {
    fn default() -> Self {
        LimiterState { buckets: HashMap::new(), next_prune_at: 0 }
    }
}

impl<K: Eq + Hash + Clone> RateLimiter<K> {
    pub fn new(limit_per_minute: u32) -> Self {
        RateLimiter { limit_per_minute, state: Mutex::new(LimiterState::default()) }
    }

    /// Returns `true` if this request is allowed, having consumed one unit of the
    /// caller's budget for the current window; `false` if the window is exhausted.
    pub fn check(&self, key: K, now: i64) -> bool {
        let mut state = self.state.lock().unwrap();
        // Opportunistic rather than on a timer: this map is only ever touched from
        // inside this lock, so a sweep here needs no background task and no second
        // synchronization point. Sweeping *before* inserting also means the entry
        // this call is about to create can never be swept by the same call.
        if state.buckets.len() >= PRUNE_THRESHOLD && now >= state.next_prune_at {
            state
                .buckets
                .retain(|_, (_, window_start)| now.saturating_sub(*window_start) < WINDOW_SECONDS);
            state.buckets.shrink_to_fit();
            state.next_prune_at = now.saturating_add(1);
        }
        // Checked unconditionally, *after* giving the sweep its chance: the sweep is
        // rate-limited to once a second, so between sweeps there is nothing else
        // stopping a flood from allocating without bound. This branch is O(1) on
        // every call and is what actually makes the ceiling a ceiling.
        if state.buckets.len() >= MAX_TRACKED_ADDRESSES {
            state.buckets.clear();
            state.buckets.shrink_to_fit();
        }
        let entry = state.buckets.entry(key).or_insert((0, now));
        if now - entry.1 >= WINDOW_SECONDS {
            *entry = (0, now);
        }
        if entry.0 >= self.limit_per_minute {
            false
        } else {
            entry.0 += 1;
            true
        }
    }

    #[cfg(test)]
    fn tracked_addresses(&self) -> usize {
        self.state.lock().unwrap().buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_the_limit_then_rejects_within_the_same_window() {
        let limiter = RateLimiter::new(3);
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        assert!(limiter.check(ip, 1000));
        assert!(limiter.check(ip, 1000));
        assert!(limiter.check(ip, 1000));
        assert!(!limiter.check(ip, 1000), "fourth request in the same window must be rejected");
    }

    #[test]
    fn resets_after_the_window_elapses() {
        let limiter = RateLimiter::new(1);
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        assert!(limiter.check(ip, 1000));
        assert!(!limiter.check(ip, 1030), "still within the same 60s window");
        assert!(limiter.check(ip, 1061), "a new window must reset the count");
    }

    /// Distinct IPv4 addresses, in a range wide enough to exceed any threshold here.
    fn nth_address(n: usize) -> IpAddr {
        IpAddr::from(std::net::Ipv4Addr::from(n as u32))
    }

    #[test]
    fn expired_windows_are_actually_removed_not_merely_ignored() {
        // The bug this fixes: nothing ever deleted a bucket, so one entry per
        // distinct client address accumulated for the entire uptime of the process.
        // Asserting the map *shrinks* is the point - asserting the limit still works
        // (the tests above) never would have caught it.
        let limiter = RateLimiter::new(5);
        for n in 0..PRUNE_THRESHOLD {
            assert!(limiter.check(nth_address(n), 1000));
        }
        assert_eq!(limiter.tracked_addresses(), PRUNE_THRESHOLD, "nothing is expired yet");

        // One more request, a window later: every existing entry is now stale.
        assert!(limiter.check(nth_address(PRUNE_THRESHOLD), 1000 + WINDOW_SECONDS));
        assert_eq!(
            limiter.tracked_addresses(),
            1,
            "every expired bucket must be dropped, leaving only the caller that triggered the sweep"
        );
    }

    #[test]
    fn a_swept_address_gets_a_fresh_budget_rather_than_a_stale_one() {
        // Pruning must be indistinguishable from having kept the entry: a bucket
        // that expired resets either way, so sweeping can neither grant a returning
        // client extra budget nor deny it any.
        let limiter = RateLimiter::new(1);
        let victim = nth_address(7);
        assert!(limiter.check(victim, 1000));
        assert!(!limiter.check(victim, 1000), "budget exhausted within the window");

        for n in 100_000..(100_000 + PRUNE_THRESHOLD) {
            limiter.check(nth_address(n), 1000);
        }
        // A window later, the sweep discards `victim` along with everyone else.
        assert!(limiter.check(nth_address(1), 1000 + WINDOW_SECONDS));
        assert!(limiter.check(victim, 1000 + WINDOW_SECONDS), "a new window must grant a fresh budget");
        assert!(!limiter.check(victim, 1000 + WINDOW_SECONDS), "...and only one unit of it");
    }

    #[test]
    fn a_flood_of_live_addresses_inside_one_window_is_still_bounded() {
        // Sweeping only removes *expired* entries, so a flood arriving faster than a
        // window can't be swept at all. Memory still has to stay bounded.
        // Every request at the same instant: nothing can expire, and the
        // once-per-second sweep gate opens at most once, so the hard ceiling is the
        // only thing standing between this and unbounded growth.
        let limiter = RateLimiter::new(5);
        for n in 0..(MAX_TRACKED_ADDRESSES * 3) {
            limiter.check(nth_address(n), 1000);
            assert!(
                limiter.tracked_addresses() <= MAX_TRACKED_ADDRESSES,
                "tracked {} addresses after {n}, above the {MAX_TRACKED_ADDRESSES} ceiling",
                limiter.tracked_addresses()
            );
        }
        // The limiter is still functional afterwards, not wedged.
        let ip = nth_address(999_999);
        assert!(limiter.check(ip, 2000));
        for _ in 0..4 {
            assert!(limiter.check(ip, 2000));
        }
        assert!(!limiter.check(ip, 2000), "the limit must still be enforced after a wholesale drop");
    }

    #[test]
    fn different_ips_have_independent_budgets() {
        let limiter = RateLimiter::new(1);
        let a: IpAddr = "1.2.3.4".parse().unwrap();
        let b: IpAddr = "5.6.7.8".parse().unwrap();
        assert!(limiter.check(a, 1000));
        assert!(!limiter.check(a, 1000));
        assert!(limiter.check(b, 1000), "a different IP must not be affected by another IP's usage");
    }
}
