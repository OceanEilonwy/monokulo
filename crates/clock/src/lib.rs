//! What time it is, read from a [`Clock`] that is passed in rather than from
//! the system directly, so a test can say what time it is.
//!
//! - [`UnixTime`] is a moment, in whole Unix seconds. It has no arithmetic
//!   operators: a moment plus a moment means nothing, and a raw `i64` is too
//!   easy to mix up with a duration or a different unit. It moves by a
//!   [`Duration`] ([`UnixTime::saturating_add`]) and two moments give the
//!   [`Duration`] between them ([`UnixTime::since`], [`UnixTime::until`]).
//!   [`UnixTime::as_secs`] and [`UnixTime::from_secs`] are for the edges
//!   only: a database column, a wire format, a log field.
//! - [`Clock`] says what time it is: [`Clock::System`] in production,
//!   [`Clock::Fixed`] for a test that compares two renders, and
//!   [`Clock::Manual`] for a test that moves time on (expiry, a countdown,
//!   "2 min ago") without waiting.
//!
//! Code takes `now: UnixTime`; only the edges (a handler, a loop, a stream)
//! hold a `Clock`, and each reads it once per operation and passes that
//! moment down, so one operation never sees two different times.
//!
//! Monotonic time for timeouts and backoff is `tokio::time::Instant`, which
//! a paused tokio test controls; `std::time::Instant::now` and
//! `std::time::SystemTime::now` are refused by clippy (`clippy.toml`).

use std::{
    fmt,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
    time::Duration,
};

/// A moment, in whole seconds since the Unix epoch.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct UnixTime(i64);

impl UnixTime {
    /// 1970-01-01T00:00:00Z.
    pub const EPOCH: UnixTime = UnixTime(0);

    /// The moment `secs` seconds after the epoch: for reading a stored or
    /// received time.
    pub const fn from_secs(secs: i64) -> Self {
        UnixTime(secs)
    }

    /// Seconds since the epoch: for storing or sending this moment, never
    /// for arithmetic.
    pub const fn as_secs(self) -> i64 {
        self.0
    }

    /// `by` later, saturating at the end of time.
    #[must_use]
    pub fn saturating_add(self, by: Duration) -> Self {
        UnixTime(self.0.saturating_add(secs_of(by)))
    }

    /// `by` earlier, saturating at the start of time.
    #[must_use]
    pub fn saturating_sub(self, by: Duration) -> Self {
        UnixTime(self.0.saturating_sub(secs_of(by)))
    }

    /// How long after `earlier` this is: zero if `earlier` is in fact later.
    pub fn since(self, earlier: UnixTime) -> Duration {
        Duration::from_secs(u64::try_from(self.0.saturating_sub(earlier.0)).unwrap_or(0))
    }

    /// How long until `later`: zero once it's passed.
    pub fn until(self, later: UnixTime) -> Duration {
        later.since(self)
    }
}

impl fmt::Display for UnixTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

fn secs_of(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

/// Where the time comes from.
#[derive(Clone, Debug, Default)]
pub enum Clock {
    /// The system's clock. A clock set before 1970 reads as the epoch rather
    /// than panicking in a request or a loop.
    #[default]
    System,
    /// Always this moment: two renders a test compares see the same time.
    Fixed(UnixTime),
    /// A moment the test holding the [`ManualTime`] sets and moves on.
    Manual(ManualTime),
}

impl Clock {
    /// What time it is, by this clock.
    pub fn now(&self) -> UnixTime {
        match self {
            Clock::System => system_now(),
            Clock::Fixed(at) => *at,
            Clock::Manual(time) => time.now(),
        }
    }

    /// Nanoseconds since the epoch, for comparing with log records, which
    /// are stamped to the nanosecond. A fixed or hand-moved clock has whole
    /// seconds only, so it reads as the start of its second.
    pub fn now_nanos(&self) -> i64 {
        match self {
            Clock::System => system_now_nanos(),
            Clock::Fixed(_) | Clock::Manual(_) => {
                self.now().as_secs().saturating_mul(1_000_000_000)
            }
        }
    }

    /// A clock starting at `at` that only moves when the returned hand
    /// moves it.
    pub fn manual(at: UnixTime) -> (Clock, ManualTime) {
        let time = ManualTime::new(at);
        (Clock::Manual(time.clone()), time)
    }
}

/// The one place the system's wall clock is read.
#[allow(clippy::disallowed_methods)]
fn system_now() -> UnixTime {
    UnixTime(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| {
                i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
            }),
    )
}

/// The system's wall clock to the nanosecond, saturating rather than
/// wrapping.
#[allow(clippy::disallowed_methods)]
fn system_now_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        })
}

/// The hand of a [`Clock::Manual`]: every clone moves the same time.
#[derive(Clone, Debug)]
pub struct ManualTime(Arc<AtomicI64>);

impl ManualTime {
    pub fn new(at: UnixTime) -> Self {
        ManualTime(Arc::new(AtomicI64::new(at.0)))
    }

    pub fn now(&self) -> UnixTime {
        UnixTime(self.0.load(Ordering::SeqCst))
    }

    pub fn set(&self, at: UnixTime) {
        self.0.store(at.0, Ordering::SeqCst);
    }

    pub fn advance(&self, by: Duration) {
        self.0.fetch_add(secs_of(by), Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moments_move_by_durations_and_differ_by_durations_never_below_zero() {
        let at = UnixTime::from_secs(1_000);
        assert_eq!(at.saturating_add(Duration::from_secs(90)).as_secs(), 1_090);
        assert_eq!(at.saturating_sub(Duration::from_secs(90)).as_secs(), 910);
        assert_eq!(
            UnixTime::from_secs(1_090).since(at),
            Duration::from_secs(90)
        );
        assert_eq!(at.since(UnixTime::from_secs(1_090)), Duration::ZERO);
        assert_eq!(
            at.until(UnixTime::from_secs(1_090)),
            Duration::from_secs(90)
        );
        assert_eq!(
            UnixTime::from_secs(i64::MAX)
                .saturating_add(Duration::MAX)
                .as_secs(),
            i64::MAX
        );
    }

    #[test]
    fn a_manual_clock_moves_only_when_its_hand_does_and_every_copy_sees_it() {
        let (clock, hand) = Clock::manual(UnixTime::from_secs(100));
        let copy = clock.clone();
        assert_eq!(clock.now().as_secs(), 100);
        hand.advance(Duration::from_secs(61));
        assert_eq!(copy.now().as_secs(), 161);
        hand.set(UnixTime::from_secs(5));
        assert_eq!(clock.now().as_secs(), 5);
    }

    #[test]
    fn a_fixed_clock_never_moves_and_the_system_one_is_after_2025() {
        let clock = Clock::Fixed(UnixTime::from_secs(42));
        assert_eq!(clock.now(), clock.now());
        assert!(Clock::System.now() > UnixTime::from_secs(1_735_689_600));
    }

    #[test]
    fn a_pinned_clock_in_nanoseconds_is_the_start_of_its_second() {
        let (clock, hand) = Clock::manual(UnixTime::from_secs(3));
        assert_eq!(clock.now_nanos(), 3_000_000_000);
        hand.advance(Duration::from_secs(1));
        assert_eq!(clock.now_nanos(), 4_000_000_000);
        assert!(Clock::System.now_nanos() > 1_735_689_600_000_000_000);
    }

    #[test]
    fn a_moment_is_plain_seconds_on_the_wire() {
        assert_eq!(serde_json::to_string(&UnixTime::from_secs(7)).unwrap(), "7");
        assert_eq!(
            serde_json::from_str::<UnixTime>("7").unwrap(),
            UnixTime::from_secs(7)
        );
    }
}
