//! What the engine has measured of one node's link (docs/engine_scaling.md
//! section 1), and the timeouts that follow from it (section 2).
//!
//! Three running averages, each from the calls that show it best:
//!
//! - **Round-trip time**, from small calls (heights, tips, single lookups).
//! - **Time to first byte per block**, from `get_blocks.bin`: monerod builds
//!   the whole answer before it sends any of it, so its own work grows with
//!   the number of blocks asked for.
//! - **Transfer rate**, from response bodies of at least
//!   [`MIN_RATE_SAMPLE_BYTES`], timed from the first byte to the last. A
//!   smaller body is over before the rate shows.
//!
//! A new link starts from pessimistic guesses, so the first requests are
//! small and given plenty of time, and corrects within a few calls. A
//! timeout halves the rate estimate: a link that only ever times out still
//! learns that it is slow. Nothing here is persisted; after a restart a
//! link is measured again.

use std::collections::VecDeque;
use std::time::Duration;

use parking_lot::Mutex;

/// How strongly a new sample moves an average.
const ALPHA: f64 = 0.3;
/// The guesses a link starts from: 1 Mbit/s, a 1 s round trip, 50 ms of the
/// node's own work per block and 50 kB per block (pruned blocks average far
/// less on mainnet today).
pub const COLD_RATE_BYTES_PER_SEC: f64 = 125_000.0;
const COLD_RTT_SECS: f64 = 1.0;
const COLD_TTFB_PER_BLOCK_SECS: f64 = 0.05;
pub const COLD_BYTES_PER_BLOCK: f64 = 50_000.0;
/// The smallest response body that counts towards the transfer rate.
pub const MIN_RATE_SAMPLE_BYTES: usize = 256 * 1024;
/// The slowest a link's rate estimate can fall to: 1 kB/s. Below that a
/// timeout would always be the ceiling anyway.
const MIN_RATE_BYTES_PER_SEC: f64 = 1_000.0;
/// A call may take this many times what it is expected to take.
pub(crate) const SAFETY: f64 = 3.0;
/// The shortest timeout any call gets: the client's request timeout for
/// small calls, so a measured link never gets less than an unmeasured one.
pub const MIN_TIMEOUT: Duration = crate::daemon_rpc::REQUEST_TIMEOUT;
/// The longest timeout any call gets.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Minutes of history kept, for the admin page's charts.
const HISTORY_MINUTES: usize = 60;

/// `expected`, with room to spare, within [`MIN_TIMEOUT`] and [`MAX_TIMEOUT`].
pub fn timeout_for(expected: Duration) -> Duration {
    expected.mul_f64(SAFETY).clamp(MIN_TIMEOUT, MAX_TIMEOUT)
}

/// One link's measurements. Cheap to share: every method takes `&self`.
#[derive(Default)]
pub struct Link {
    state: Mutex<State>,
}

struct State {
    rtt_secs: f64,
    ttfb_per_block_secs: f64,
    rate_bytes_per_sec: f64,
    bytes_per_block: f64,
    /// Whether any real sample has been taken, as opposed to the guesses.
    measured: bool,
    last_measured_unix: Option<i64>,
    history: VecDeque<Minute>,
}

impl Default for State {
    fn default() -> Self {
        State {
            rtt_secs: COLD_RTT_SECS,
            ttfb_per_block_secs: COLD_TTFB_PER_BLOCK_SECS,
            rate_bytes_per_sec: COLD_RATE_BYTES_PER_SEC,
            bytes_per_block: COLD_BYTES_PER_BLOCK,
            measured: false,
            last_measured_unix: None,
            history: VecDeque::new(),
        }
    }
}

/// One minute of a link's history: the estimates as they stood at its end,
/// and what went wrong during it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Minute {
    minute_unix: i64,
    rate_bytes_per_sec: f64,
    rtt_secs: f64,
    ttfb_per_block_secs: f64,
    timeouts: u32,
    failures: u32,
}

pub use shared::scaling::{LinkPoint, LinkSnapshot};

fn ewma(average: f64, sample: f64) -> f64 {
    ALPHA * sample + (1.0 - ALPHA) * average
}

fn ms(secs: f64) -> u64 {
    (secs * 1000.0).round() as u64
}

impl Link {
    pub fn new() -> Self {
        Link::default()
    }

    /// A small call answered in `elapsed`: a round-trip sample.
    pub fn record_small(&self, elapsed: Duration) {
        self.record_small_at(elapsed, shared::time::now_unix());
    }

    fn record_small_at(&self, elapsed: Duration, now_unix: i64) {
        let mut state = self.state.lock();
        state.rtt_secs = if state.measured {
            ewma(state.rtt_secs, elapsed.as_secs_f64())
        } else {
            elapsed.as_secs_f64()
        };
        state.note_measured(now_unix);
    }

    /// A `get_blocks.bin` answer: `blocks` blocks in `bytes` bytes, the
    /// first byte after `to_first_byte` and the rest over `transfer`.
    pub fn record_blocks(
        &self,
        blocks: u64,
        bytes: usize,
        to_first_byte: Duration,
        transfer: Duration,
    ) {
        self.record_blocks_at(
            blocks,
            bytes,
            to_first_byte,
            transfer,
            shared::time::now_unix(),
        );
    }

    fn record_blocks_at(
        &self,
        blocks: u64,
        bytes: usize,
        to_first_byte: Duration,
        transfer: Duration,
        now_unix: i64,
    ) {
        if blocks == 0 {
            return;
        }
        let mut state = self.state.lock();
        let blocks_f = blocks as f64;
        // The node's own work: what the first byte took beyond a round trip.
        let work = (to_first_byte.as_secs_f64() - state.rtt_secs).max(0.0);
        state.ttfb_per_block_secs = ewma(state.ttfb_per_block_secs, work / blocks_f);
        state.bytes_per_block = ewma(state.bytes_per_block, bytes as f64 / blocks_f);
        if bytes >= MIN_RATE_SAMPLE_BYTES && !transfer.is_zero() {
            let rate = bytes as f64 / transfer.as_secs_f64();
            state.rate_bytes_per_sec =
                ewma(state.rate_bytes_per_sec, rate).max(MIN_RATE_BYTES_PER_SEC);
        }
        state.note_measured(now_unix);
    }

    /// A call that ran out of time: the link is slower than estimated.
    pub fn record_timeout(&self) {
        self.record_timeout_at(shared::time::now_unix());
    }

    fn record_timeout_at(&self, now_unix: i64) {
        let mut state = self.state.lock();
        state.rate_bytes_per_sec = (state.rate_bytes_per_sec / 2.0).max(MIN_RATE_BYTES_PER_SEC);
        state.in_minute(now_unix, |minute| minute.timeouts += 1);
    }

    /// A call that failed for any other reason.
    pub fn record_failure(&self) {
        self.state
            .lock()
            .in_minute(shared::time::now_unix(), |minute| minute.failures += 1);
    }

    pub fn rate_bytes_per_sec(&self) -> f64 {
        self.state.lock().rate_bytes_per_sec
    }

    pub fn bytes_per_block(&self) -> f64 {
        self.state.lock().bytes_per_block
    }

    /// How long `blocks` blocks of `bytes_per_block` each should take.
    pub fn expected_for_blocks(&self, blocks: u64, bytes_per_block: f64) -> Duration {
        let state = self.state.lock();
        let secs = state.rtt_secs
            + blocks as f64 * state.ttfb_per_block_secs
            + blocks as f64 * bytes_per_block / state.rate_bytes_per_sec;
        Duration::from_secs_f64(secs.max(0.0))
    }

    /// The timeout for a `get_blocks.bin` call asking for `blocks` blocks,
    /// sized by this link's own bytes-per-block estimate.
    pub fn timeout_for_blocks(&self, blocks: u64) -> Duration {
        timeout_for(self.expected_for_blocks(blocks, self.bytes_per_block()))
    }

    pub fn snapshot(&self) -> LinkSnapshot {
        self.snapshot_at(shared::time::now_unix())
    }

    fn snapshot_at(&self, now_unix: i64) -> LinkSnapshot {
        let state = self.state.lock();
        let hour_ago = now_unix - 3600;
        let recent = state.history.iter().filter(|m| m.minute_unix > hour_ago);
        let (timeouts, failures) = recent
            .clone()
            .fold((0, 0), |(t, f), m| (t + m.timeouts, f + m.failures));
        LinkSnapshot {
            measured: state.measured,
            rtt_ms: ms(state.rtt_secs),
            ttfb_per_block_ms: ms(state.ttfb_per_block_secs),
            rate_bytes_per_sec: state.rate_bytes_per_sec.round() as u64,
            bytes_per_block: state.bytes_per_block.round() as u64,
            last_measured_unix: state.last_measured_unix,
            timeouts_last_hour: timeouts,
            failures_last_hour: failures,
            history: recent
                .filter(|m| m.rate_bytes_per_sec > 0.0)
                .map(|m| LinkPoint {
                    minute_unix: m.minute_unix,
                    rate_bytes_per_sec: m.rate_bytes_per_sec.round() as u64,
                    rtt_ms: ms(m.rtt_secs),
                    ttfb_per_block_ms: ms(m.ttfb_per_block_secs),
                })
                .collect(),
        }
    }
}

impl State {
    fn note_measured(&mut self, now_unix: i64) {
        self.measured = true;
        self.last_measured_unix = Some(now_unix);
        let (rate, rtt, ttfb) = (
            self.rate_bytes_per_sec,
            self.rtt_secs,
            self.ttfb_per_block_secs,
        );
        self.in_minute(now_unix, |minute| {
            minute.rate_bytes_per_sec = rate;
            minute.rtt_secs = rtt;
            minute.ttfb_per_block_secs = ttfb;
        });
    }

    /// Applies `f` to the history entry for the minute holding `now_unix`,
    /// started if new.
    fn in_minute(&mut self, now_unix: i64, f: impl FnOnce(&mut Minute)) {
        let minute_unix = now_unix - now_unix.rem_euclid(60);
        if self
            .history
            .back()
            .is_none_or(|m| m.minute_unix != minute_unix)
        {
            self.history.push_back(Minute {
                minute_unix,
                rate_bytes_per_sec: 0.0,
                rtt_secs: 0.0,
                ttfb_per_block_secs: 0.0,
                timeouts: 0,
                failures: 0,
            });
            while self.history.len() > HISTORY_MINUTES {
                self.history.pop_front();
            }
        }
        if let Some(minute) = self.history.back_mut() {
            f(minute);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000;

    #[test]
    fn a_new_link_starts_slow_and_is_given_time() {
        let link = Link::new();
        let snapshot = link.snapshot_at(T0);
        assert!(!snapshot.measured);
        assert_eq!(snapshot.rate_bytes_per_sec, 125_000);
        // 100 blocks of the 50 kB guess at 1 Mbit/s: 1 s + 5 s + 40 s,
        // times three.
        assert_eq!(link.timeout_for_blocks(100), Duration::from_secs(138));
        // A tiny request still gets the floor.
        assert_eq!(link.timeout_for_blocks(1), MIN_TIMEOUT);
    }

    #[test]
    fn the_rate_comes_from_large_bodies_and_the_first_byte_from_the_nodes_work() {
        let link = Link::new();
        link.record_small_at(Duration::from_millis(100), T0);
        assert_eq!(
            link.snapshot_at(T0).rtt_ms,
            100,
            "the first sample replaces the guess"
        );
        // 10 MB over 2 s after the first byte: 5 MB/s, several times.
        for _ in 0..20 {
            link.record_blocks_at(
                100,
                10_000_000,
                Duration::from_millis(600),
                Duration::from_secs(2),
                T0,
            );
        }
        let snapshot = link.snapshot_at(T0);
        assert!(snapshot.measured);
        assert!(
            (4_900_000..=5_000_000).contains(&snapshot.rate_bytes_per_sec),
            "{snapshot:?}"
        );
        assert!(
            (99_000..=101_000).contains(&snapshot.bytes_per_block),
            "{snapshot:?}"
        );
        // 500 ms of the node's own work over 100 blocks.
        assert_eq!(snapshot.ttfb_per_block_ms, 5);

        // A small body says nothing about the rate.
        link.record_blocks_at(
            1,
            10_000,
            Duration::from_millis(100),
            Duration::from_secs(5),
            T0,
        );
        assert!(link.rate_bytes_per_sec() > 4_000_000.0);
    }

    #[test]
    fn a_timeout_halves_the_rate_and_the_next_timeout_grows() {
        let link = Link::new();
        let before = link.timeout_for_blocks(200);
        link.record_timeout_at(T0);
        assert_eq!(link.snapshot_at(T0).rate_bytes_per_sec, 62_500);
        assert!(link.timeout_for_blocks(200) > before);
        for _ in 0..100 {
            link.record_timeout_at(T0);
        }
        assert_eq!(
            link.rate_bytes_per_sec(),
            MIN_RATE_BYTES_PER_SEC,
            "it has a floor"
        );
        assert_eq!(
            link.timeout_for_blocks(500),
            MAX_TIMEOUT,
            "and the timeout a ceiling"
        );
        assert_eq!(link.snapshot_at(T0).timeouts_last_hour, 101);
    }

    #[test]
    fn history_keeps_an_hour_by_the_minute() {
        let link = Link::new();
        for minute in 0..90 {
            link.record_small_at(Duration::from_millis(100 + minute), T0 + minute as i64 * 60);
        }
        let snapshot = link.snapshot_at(T0 + 89 * 60);
        assert_eq!(snapshot.history.len(), 60);
        assert!(snapshot
            .history
            .windows(2)
            .all(|w| w[0].minute_unix < w[1].minute_unix));
        assert_eq!(snapshot.history.last().unwrap().minute_unix % 60, 0);
        // Failures older than an hour drop out of the count.
        link.state
            .lock()
            .in_minute(T0 + 89 * 60, |minute| minute.failures += 2);
        assert_eq!(link.snapshot_at(T0 + 200 * 60).failures_last_hour, 0);
    }
}
