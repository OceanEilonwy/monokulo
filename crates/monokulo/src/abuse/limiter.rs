//! Tiered per-client limits (step 9c): under the soft limit nothing
//! changes; past it the client must solve a challenge (`super::challenge`);
//! past the hard limit everything is refused with `429` and `Retry-After`.
//!
//! Requests are counted over a rolling minute, approximated the usual way
//! with two fixed one-minute windows: the estimate is this window's count
//! plus the previous window's, weighted by how much of it still overlaps the
//! last 60 seconds. That smooths out the edge of a fixed window (where a
//! client could otherwise send twice the limit across a boundary) without
//! storing a timestamp per request.
//!
//! A solved challenge gives the client a pass: for [`PASS_SECS`] it is
//! treated as under the soft limit (the hard limit still applies). Passes are
//! held here, in memory, keyed by the client - no cookie is set.
//!
//! Memory is capped at [`MAX_TRACKED_CLIENTS`]: when full, the client seen
//! least recently is forgotten first, so a flood of new Tor circuits or
//! addresses can't grow the table without bound (forgetting a client only
//! ever gives it a fresh budget, never a stricter one).

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::sync::Mutex;

/// How long a solved challenge lets a client through without another one.
pub const PASS_SECS: i64 = 10 * 60;

/// The most clients tracked at once (see the module doc comment).
pub const MAX_TRACKED_CLIENTS: usize = 100_000;

const WINDOW_SECS: i64 = 60;

/// What a client's next request may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Under the soft limit, or holding a pass.
    Allowed,
    /// Past the soft limit and holding no pass: solve a challenge first.
    Challenge,
    /// Past the hard limit: refused until `retry_after_secs` have passed.
    Blocked { retry_after_secs: u64 },
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub soft_per_min: u32,
    pub hard_per_min: u32,
}

struct Entry {
    window_start: i64,
    current: u32,
    previous: u32,
    pass_until: i64,
    /// Position in the least-recently-seen order.
    seen: u64,
}

impl Entry {
    fn roll(&mut self, now: i64) {
        let window_start = now - now.rem_euclid(WINDOW_SECS);
        if window_start == self.window_start {
            return;
        }
        self.previous = if window_start - self.window_start == WINDOW_SECS { self.current } else { 0 };
        self.current = 0;
        self.window_start = window_start;
    }

    /// Requests over the last rolling minute, including the one being made.
    fn estimate(&self, now: i64) -> f64 {
        let into_window = (now - self.window_start) as f64 / WINDOW_SECS as f64;
        self.previous as f64 * (1.0 - into_window) + self.current as f64
    }
}

struct State<K> {
    entries: HashMap<K, Entry>,
    /// `seen` sequence number -> client, oldest first.
    order: BTreeMap<u64, K>,
    next_seen: u64,
}

pub struct TieredLimiter<K> {
    max_clients: usize,
    state: Mutex<State<K>>,
}

impl<K: Eq + Hash + Clone> Default for TieredLimiter<K> {
    fn default() -> Self {
        TieredLimiter::with_capacity(MAX_TRACKED_CLIENTS)
    }
}

impl<K: Eq + Hash + Clone> TieredLimiter<K> {
    pub fn with_capacity(max_clients: usize) -> Self {
        TieredLimiter {
            max_clients,
            state: Mutex::new(State { entries: HashMap::new(), order: BTreeMap::new(), next_seen: 0 }),
        }
    }

    /// Counts one request from `client` and says what it may do.
    /// `force_soft` treats the client as past the soft limit whatever its
    /// count (under-attack mode); a pass still lets it through.
    pub fn check(&self, client: &K, limits: Limits, force_soft: bool, now: i64) -> Tier {
        let mut state = self.state.lock().unwrap();
        let state = &mut *state;
        let seen = state.next_seen;
        state.next_seen += 1;
        if let Some(entry) = state.entries.get_mut(client) {
            state.order.remove(&entry.seen);
            entry.seen = seen;
        } else {
            while state.entries.len() >= self.max_clients {
                let Some((_, oldest)) = state.order.pop_first() else { break };
                state.entries.remove(&oldest);
            }
            state.entries.insert(
                client.clone(),
                Entry { window_start: now - now.rem_euclid(WINDOW_SECS), current: 0, previous: 0, pass_until: 0, seen },
            );
        }
        state.order.insert(seen, client.clone());

        let entry = state.entries.get_mut(client).expect("just inserted");
        entry.roll(now);
        entry.current = entry.current.saturating_add(1);
        let estimate = entry.estimate(now);
        if estimate > limits.hard_per_min as f64 {
            let retry_after_secs = (entry.window_start + WINDOW_SECS - now).max(1) as u64;
            return Tier::Blocked { retry_after_secs };
        }
        let has_pass = entry.pass_until > now;
        if !has_pass && (force_soft || estimate > limits.soft_per_min as f64) {
            return Tier::Challenge;
        }
        Tier::Allowed
    }

    /// Gives `client` a pass for [`PASS_SECS`] after a solved challenge.
    pub fn grant_pass(&self, client: &K, now: i64) {
        let mut state = self.state.lock().unwrap();
        if let Some(entry) = state.entries.get_mut(client) {
            entry.pass_until = now + PASS_SECS;
        }
    }

    pub fn has_pass(&self, client: &K, now: i64) -> bool {
        self.state.lock().unwrap().entries.get(client).is_some_and(|entry| entry.pass_until > now)
    }

    pub fn tracked(&self) -> usize {
        self.state.lock().unwrap().entries.len()
    }

    /// Every client currently tracked (for tests and diagnostics).
    pub fn clients(&self) -> Vec<K> {
        self.state.lock().unwrap().entries.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits { soft_per_min: 3, hard_per_min: 6 };

    #[test]
    fn under_soft_is_allowed_past_soft_is_challenged_past_hard_is_blocked() {
        let limiter = TieredLimiter::<u32>::default();
        let now = 1_000_040; // 20s into a window (1_000_020 is a multiple of 60)
        for _ in 0..3 {
            assert_eq!(limiter.check(&1, LIMITS, false, now), Tier::Allowed);
        }
        for _ in 0..3 {
            assert_eq!(limiter.check(&1, LIMITS, false, now), Tier::Challenge);
        }
        assert_eq!(limiter.check(&1, LIMITS, false, now), Tier::Blocked { retry_after_secs: 40 });
        assert_eq!(limiter.check(&2, LIMITS, false, now), Tier::Allowed, "another client is unaffected");
    }

    #[test]
    fn a_pass_skips_the_challenge_for_ten_minutes_but_not_the_hard_limit() {
        let limiter = TieredLimiter::<u32>::default();
        let now = 1_000_000;
        for _ in 0..4 {
            limiter.check(&1, LIMITS, false, now);
        }
        limiter.grant_pass(&1, now);
        assert!(limiter.has_pass(&1, now + PASS_SECS - 1));
        assert!(!limiter.has_pass(&1, now + PASS_SECS));
        assert_eq!(limiter.check(&1, LIMITS, false, now), Tier::Allowed);
        assert_eq!(limiter.check(&1, LIMITS, false, now), Tier::Allowed);
        assert!(matches!(limiter.check(&1, LIMITS, false, now), Tier::Blocked { .. }), "past hard even with a pass");
    }

    #[test]
    fn under_attack_challenges_every_client_without_a_pass() {
        let limiter = TieredLimiter::<u32>::default();
        assert_eq!(limiter.check(&1, LIMITS, true, 1_000_000), Tier::Challenge);
        limiter.grant_pass(&1, 1_000_000);
        assert_eq!(limiter.check(&1, LIMITS, true, 1_000_001), Tier::Allowed);
    }

    #[test]
    fn the_count_is_a_rolling_minute() {
        let limiter = TieredLimiter::<u32>::default();
        let start = 1_000_040; // 20s into the window starting at 1_000_020
        for _ in 0..3 {
            limiter.check(&1, LIMITS, false, start);
        }
        // 30s into the next window, half of the previous window still counts:
        // 1.5 + 1 = 2.5, under the soft limit of 3.
        assert_eq!(limiter.check(&1, LIMITS, false, start + 70), Tier::Allowed);
        // 1.5 + 2 = 3.5: challenged.
        assert_eq!(limiter.check(&1, LIMITS, false, start + 70), Tier::Challenge);
        // Two windows later nothing of it remains.
        assert_eq!(limiter.check(&1, LIMITS, false, start + 200), Tier::Allowed);
    }

    #[test]
    fn memory_is_capped_by_forgetting_the_least_recently_seen_client() {
        let limiter = TieredLimiter::<u32>::with_capacity(3);
        let now = 1_000_000;
        for client in 1..=3 {
            limiter.check(&client, LIMITS, false, now);
        }
        limiter.check(&1, LIMITS, false, now); // 1 is now the most recent
        limiter.grant_pass(&1, now);
        limiter.check(&4, LIMITS, false, now); // evicts 2, the least recent
        assert_eq!(limiter.tracked(), 3);
        assert!(limiter.has_pass(&1, now), "a recently seen client is kept");
        for client in 5..1000 {
            limiter.check(&client, LIMITS, false, now);
        }
        assert_eq!(limiter.tracked(), 3, "a flood of new clients never grows the table");
    }
}
