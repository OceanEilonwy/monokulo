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
//! held here, in memory, keyed by the client - no cookie is set. A pass has
//! a kind ([`Pass`]): a solved proof of work covers every request, the
//! no-JavaScript wait only pages ([`Pass::covers`]), so waiting can't stand
//! in for the work the machine API asks of a client.
//!
//! Memory is capped at [`MAX_TRACKED_CLIENTS`]: when full, the client seen
//! least recently is forgotten first, so a flood of new Tor circuits or
//! addresses can't grow the table without bound (forgetting a client only
//! ever gives it a fresh budget, never a stricter one).

use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

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

/// How a route may be treated - see `http::abuse`'s module doc comment's
/// table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteClass {
    Page,
    Api,
    Stream,
    Other,
    /// Log reports: their own budget, never challenged.
    Logs,
    /// Not counted at all (the status indicator's cached poll).
    Exempt,
}

/// What a client did to earn a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    /// Solved a proof of work.
    Work,
    /// Waited out the no-JavaScript timer, which costs nothing but time.
    Wait,
}

impl Pass {
    /// Whether this pass lets a request of `class` skip the challenge. A
    /// wait is the no-JavaScript visitor's way onto a page; anything a
    /// script calls (the machine API above all, where a fresh Tor circuit
    /// is free) still costs the work.
    pub fn covers(self, class: RouteClass) -> bool {
        match (self, class) {
            (Pass::Work, _) => true,
            (Pass::Wait, RouteClass::Page) => true,
            (
                Pass::Wait,
                RouteClass::Api
                | RouteClass::Stream
                | RouteClass::Other
                | RouteClass::Logs
                | RouteClass::Exempt,
            ) => false,
        }
    }
}

/// A pass a redeemed challenge earns: its kind, and when it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PassGrant {
    pub pass: Pass,
    pub until: i64,
}

struct Entry {
    window_start: i64,
    current: u32,
    previous: u32,
    /// When each kind of pass the client holds ends (`0` for none).
    work_pass_until: i64,
    wait_pass_until: i64,
    /// Position in the least-recently-seen order.
    seen: u64,
}

impl Entry {
    fn new(seen: u64, now: i64) -> Self {
        Entry {
            window_start: now - now.rem_euclid(WINDOW_SECS),
            current: 0,
            previous: 0,
            work_pass_until: 0,
            wait_pass_until: 0,
            seen,
        }
    }

    fn pass_until(&mut self, pass: Pass) -> &mut i64 {
        match pass {
            Pass::Work => &mut self.work_pass_until,
            Pass::Wait => &mut self.wait_pass_until,
        }
    }

    /// Whether a pass the client holds covers a request of `class`.
    fn has_pass(&self, class: RouteClass, now: i64) -> bool {
        (Pass::Work.covers(class) && self.work_pass_until > now)
            || (Pass::Wait.covers(class) && self.wait_pass_until > now)
    }

    fn roll(&mut self, now: i64) {
        let window_start = now - now.rem_euclid(WINDOW_SECS);
        if window_start == self.window_start {
            return;
        }
        self.previous = if window_start - self.window_start == WINDOW_SECS {
            self.current
        } else {
            0
        };
        self.current = 0;
        self.window_start = window_start;
    }

    /// Requests counted over the last rolling minute, not including the one
    /// being made.
    fn estimate(&self, now: i64) -> f64 {
        let into_window = (now - self.window_start) as f64 / WINDOW_SECS as f64;
        self.previous as f64 * (1.0 - into_window) + self.current as f64
    }

    /// How many seconds until one more request would be under `hard`: what
    /// `Retry-After` promises. Refused requests aren't counted, so the
    /// estimate only falls with time: within this window as the previous
    /// window's count decays, and failing that from the next window on as
    /// this window's count does.
    fn seconds_until_allowed(&self, hard: u32, now: i64) -> u64 {
        let window = WINDOW_SECS as f64;
        let into_window = (now - self.window_start) as f64;
        let (previous, current, hard) = (self.previous as f64, self.current as f64, hard as f64);
        let wait = if current + 1.0 <= hard {
            // previous * (1 - (into + d) / window) + current + 1 <= hard
            let room = hard - current - 1.0;
            if previous <= room {
                0.0
            } else {
                (window * (1.0 - room / previous) - into_window).ceil()
            }
        } else {
            // Next window: current * (1 - t / window) + 1 <= hard.
            let room = hard - 1.0;
            let decay = if current <= room {
                0.0
            } else {
                (window * (1.0 - room / current)).ceil()
            };
            (self.window_start + WINDOW_SECS - now) as f64 + decay
        };
        wait.max(1.0) as u64
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
            state: Mutex::new(State {
                entries: HashMap::new(),
                order: BTreeMap::new(),
                next_seen: 0,
            }),
        }
    }

    /// The client's entry, made if it isn't tracked (forgetting the least
    /// recently seen client when full), and marked as just seen.
    fn touch<'a>(&self, state: &'a mut State<K>, client: &K, now: i64) -> &'a mut Entry {
        let seen = state.next_seen;
        state.next_seen += 1;
        if let Some(entry) = state.entries.get_mut(client) {
            state.order.remove(&entry.seen);
            entry.seen = seen;
        } else {
            while state.entries.len() >= self.max_clients {
                let Some((_, oldest)) = state.order.pop_first() else {
                    break;
                };
                state.entries.remove(&oldest);
            }
            state.entries.insert(client.clone(), Entry::new(seen, now));
        }
        state.order.insert(seen, client.clone());
        state.entries.get_mut(client).expect("just inserted")
    }

    /// Counts one request of `class` from `client` and says what it may do.
    /// `force_soft` treats the client as past the soft limit whatever its
    /// count (under-attack mode); a pass that covers `class` still lets it
    /// through.
    pub fn check(
        &self,
        client: &K,
        limits: Limits,
        force_soft: bool,
        class: RouteClass,
        now: i64,
    ) -> Tier {
        let mut state = self.state.lock();
        let entry = self.touch(&mut state, client, now);
        entry.roll(now);
        // With this request counted: a refused request is not counted, or
        // a client that obeyed `Retry-After` would be refused again by its
        // own refused requests.
        let estimate = entry.estimate(now) + 1.0;
        if estimate > limits.hard_per_min as f64 {
            let retry_after_secs = entry.seconds_until_allowed(limits.hard_per_min, now);
            return Tier::Blocked { retry_after_secs };
        }
        entry.current = entry.current.saturating_add(1);
        if !entry.has_pass(class, now) && (force_soft || estimate > limits.soft_per_min as f64) {
            return Tier::Challenge;
        }
        Tier::Allowed
    }

    /// Gives `client` the pass a redeemed challenge earned. The token fixes
    /// when it ends, and a pass is only ever extended, so redeeming the same
    /// token again earns nothing. A client not tracked (evicted between its
    /// challenge and its proof, which under attack is the usual case) is
    /// tracked again: a pass that went nowhere would challenge it again,
    /// proof after proof.
    pub fn grant_pass(&self, client: &K, grant: PassGrant, now: i64) {
        let mut state = self.state.lock();
        let until = self.touch(&mut state, client, now).pass_until(grant.pass);
        *until = (*until).max(grant.until);
    }

    pub fn tracked(&self) -> usize {
        self.state.lock().entries.len()
    }

    /// Every client currently tracked (for tests and diagnostics).
    pub fn clients(&self) -> Vec<K> {
        self.state.lock().entries.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        soft_per_min: 3,
        hard_per_min: 6,
    };

    /// Whether `client` holds a pass that covers a request of `class`,
    /// without counting a request.
    fn has_pass(limiter: &TieredLimiter<u32>, client: &u32, class: RouteClass, now: i64) -> bool {
        limiter
            .state
            .lock()
            .entries
            .get(client)
            .is_some_and(|entry| entry.has_pass(class, now))
    }

    #[test]
    fn under_soft_is_allowed_past_soft_is_challenged_past_hard_is_blocked() {
        let limiter = TieredLimiter::<u32>::default();
        let now = 1_000_040; // 20s into a window (1_000_020 is a multiple of 60)
        for _ in 0..3 {
            assert_eq!(
                limiter.check(&1, LIMITS, false, RouteClass::Page, now),
                Tier::Allowed
            );
        }
        for _ in 0..3 {
            assert_eq!(
                limiter.check(&1, LIMITS, false, RouteClass::Page, now),
                Tier::Challenge
            );
        }
        // 40 s to the next window, then 10 s for this window's 6 to decay
        // to 5, leaving room for one request.
        assert_eq!(
            limiter.check(&1, LIMITS, false, RouteClass::Page, now),
            Tier::Blocked {
                retry_after_secs: 50
            }
        );
        assert_eq!(
            limiter.check(&2, LIMITS, false, RouteClass::Page, now),
            Tier::Allowed,
            "another client is unaffected"
        );
    }

    /// `Retry-After` is kept: a client that waits exactly that long is let
    /// through, because the requests refused meanwhile were not counted
    /// against it.
    #[test]
    fn a_client_that_waits_for_retry_after_is_let_through() {
        let limiter = TieredLimiter::<u32>::default();
        let now = 1_000_040;
        for _ in 0..6 {
            limiter.check(&1, LIMITS, false, RouteClass::Page, now);
        }
        let Tier::Blocked { retry_after_secs } =
            limiter.check(&1, LIMITS, false, RouteClass::Page, now)
        else {
            panic!("past hard")
        };
        // Hammering while blocked changes nothing.
        for i in 1..30 {
            assert!(matches!(
                limiter.check(&1, LIMITS, false, RouteClass::Page, now + i),
                Tier::Blocked { .. }
            ));
        }
        let later = now + retry_after_secs as i64;
        assert!(
            !matches!(
                limiter.check(&1, LIMITS, false, RouteClass::Page, later),
                Tier::Blocked { .. }
            ),
            "allowed (or challenged) at the promised time"
        );
    }

    /// A pass reaches a client that was evicted between its challenge and
    /// its proof (under attack, every request evicts someone).
    #[test]
    fn a_pass_granted_to_an_evicted_client_still_counts() {
        let limiter = TieredLimiter::<u32>::with_capacity(2);
        let now = 1_000_000;
        assert_eq!(
            limiter.check(&1, LIMITS, true, RouteClass::Page, now),
            Tier::Challenge
        );
        limiter.check(&2, LIMITS, true, RouteClass::Page, now);
        limiter.check(&3, LIMITS, true, RouteClass::Page, now); // evicts 1
        assert!(!limiter.clients().contains(&1));
        limiter.grant_pass(&1, work(now), now);
        assert!(has_pass(&limiter, &1, RouteClass::Page, now));
        assert_eq!(
            limiter.check(&1, LIMITS, true, RouteClass::Page, now + 1),
            Tier::Allowed
        );
        assert_eq!(limiter.tracked(), 2, "the cap still holds");
    }

    #[test]
    fn a_pass_skips_the_challenge_for_ten_minutes_but_not_the_hard_limit() {
        let limiter = TieredLimiter::<u32>::default();
        let now = 1_000_000;
        for _ in 0..4 {
            limiter.check(&1, LIMITS, false, RouteClass::Page, now);
        }
        limiter.grant_pass(&1, work(now), now);
        assert!(has_pass(
            &limiter,
            &1,
            RouteClass::Page,
            now + PASS_SECS - 1
        ));
        assert!(!has_pass(&limiter, &1, RouteClass::Page, now + PASS_SECS));
        // The same token again, later: its pass still ends when it did.
        limiter.grant_pass(&1, work(now), now + 60);
        assert!(!has_pass(&limiter, &1, RouteClass::Page, now + PASS_SECS));
        assert_eq!(
            limiter.check(&1, LIMITS, false, RouteClass::Page, now),
            Tier::Allowed
        );
        assert_eq!(
            limiter.check(&1, LIMITS, false, RouteClass::Api, now),
            Tier::Allowed
        );
        assert!(
            matches!(
                limiter.check(&1, LIMITS, false, RouteClass::Page, now),
                Tier::Blocked { .. }
            ),
            "past hard even with a pass"
        );

        // Waiting earns a page, never the API's work.
        for _ in 0..4 {
            limiter.check(&2, LIMITS, false, RouteClass::Page, now);
        }
        let wait = PassGrant {
            pass: Pass::Wait,
            until: now + PASS_SECS,
        };
        limiter.grant_pass(&2, wait, now);
        assert_eq!(
            limiter.check(&2, LIMITS, false, RouteClass::Page, now),
            Tier::Allowed
        );
        assert_eq!(
            limiter.check(&2, LIMITS, false, RouteClass::Api, now),
            Tier::Challenge
        );
    }

    fn work(now: i64) -> PassGrant {
        PassGrant {
            pass: Pass::Work,
            until: now + PASS_SECS,
        }
    }

    #[test]
    fn under_attack_challenges_every_client_without_a_pass() {
        let limiter = TieredLimiter::<u32>::default();
        assert_eq!(
            limiter.check(&1, LIMITS, true, RouteClass::Page, 1_000_000),
            Tier::Challenge
        );
        limiter.grant_pass(&1, work(1_000_000), 1_000_000);
        assert_eq!(
            limiter.check(&1, LIMITS, true, RouteClass::Page, 1_000_001),
            Tier::Allowed
        );
    }

    #[test]
    fn the_count_is_a_rolling_minute() {
        let limiter = TieredLimiter::<u32>::default();
        let start = 1_000_040; // 20s into the window starting at 1_000_020
        for _ in 0..3 {
            limiter.check(&1, LIMITS, false, RouteClass::Page, start);
        }
        // 30s into the next window, half of the previous window still counts:
        // 1.5 + 1 = 2.5, under the soft limit of 3.
        assert_eq!(
            limiter.check(&1, LIMITS, false, RouteClass::Page, start + 70),
            Tier::Allowed
        );
        // 1.5 + 2 = 3.5: challenged.
        assert_eq!(
            limiter.check(&1, LIMITS, false, RouteClass::Page, start + 70),
            Tier::Challenge
        );
        // Two windows later nothing of it remains.
        assert_eq!(
            limiter.check(&1, LIMITS, false, RouteClass::Page, start + 200),
            Tier::Allowed
        );
    }

    #[test]
    fn memory_is_capped_by_forgetting_the_least_recently_seen_client() {
        let limiter = TieredLimiter::<u32>::with_capacity(3);
        let now = 1_000_000;
        for client in 1..=3 {
            limiter.check(&client, LIMITS, false, RouteClass::Page, now);
        }
        limiter.check(&1, LIMITS, false, RouteClass::Page, now); // 1 is now the most recent
        limiter.grant_pass(&1, work(now), now);
        limiter.check(&4, LIMITS, false, RouteClass::Page, now); // evicts 2, the least recent
        assert_eq!(limiter.tracked(), 3);
        assert!(
            has_pass(&limiter, &1, RouteClass::Page, now),
            "a recently seen client is kept"
        );
        for client in 5..1000 {
            limiter.check(&client, LIMITS, false, RouteClass::Page, now);
        }
        assert_eq!(
            limiter.tracked(),
            3,
            "a flood of new clients never grows the table"
        );
    }
}
