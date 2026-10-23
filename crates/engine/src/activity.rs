//! What each network's scanner has been doing, for monokulo's engine page.
//!
//! Its rounds and units, blocks fetched and scanned, reorgs, the pool,
//! recomputes. The scan loops record events as
//! they work; the admin API streams them to monokulo (`http::activity`),
//! woken by each event recorded ([`Activity::subscribe`]), never polling.
//!
//! Purely observational, like `scanner_status`: nothing here feeds back
//! into scanning, and a restart loses only history. The record is bounded
//! by age and by count, and recording is one short lock, so it is always
//! on: a page opened after something happened can still scrub back to it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use shared::activity::{Event, Recorded};

/// How far back the record reaches.
pub const KEEP: Duration = Duration::from_mins(30);
/// The most events kept, whatever their age: a busy network's record stays
/// bounded (a round records about ten; a catch-up a few per block).
pub const MAX_EVENTS: usize = 50_000;
/// How often the scan loop records a snapshot of the state the page draws.
pub const SNAPSHOT_EVERY: Duration = Duration::from_secs(10);
/// How often, while someone watches (a stream is open), the node is asked
/// about its whole pool (`Event::NodePool`): the engine itself never needs
/// to know.
pub const NODE_POOL_EVERY: Duration = Duration::from_secs(5);

/// How the activity stream (`http::activity`) sends a record to monokulo.
/// The engine runs [`StreamTuning::DEFAULT`]; tests pass their own values
/// explicitly (`AppState::activity_stream`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamTuning {
    /// Events recorded within this long of each other go in one message: a
    /// catch-up records hundreds a second, and the page draws a few frames
    /// a second at most. 200 ms keeps the page live (its replay frames are
    /// 500 ms apart) at five messages a second at most.
    pub coalesce: Duration,
    /// A comment this often on a quiet stream, so the reader can tell a
    /// quiet engine from a dead connection. 15 s, like the engine's other
    /// streams.
    pub keepalive: Duration,
    /// Messages a stream may hold for a reader that isn't reading. Past
    /// that the stream is closed: the reader reconnects from the last
    /// sequence number it got and catches up from the record, so nothing
    /// is lost and nothing is buffered without bound. 16 messages is about
    /// three seconds of a busy network.
    pub buffer: usize,
}

/// Why a [`StreamTuning`] can't be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StreamTuningError {
    #[error("the coalescing interval must be above zero and at most a second")]
    Coalesce,
    #[error("the keep-alive must be longer than the coalescing interval")]
    Keepalive,
    #[error("a stream must be able to hold at least one message")]
    Buffer,
}

impl StreamTuning {
    pub const DEFAULT: Self = Self {
        coalesce: Duration::from_millis(200),
        keepalive: Duration::from_secs(15),
        buffer: 16,
    };

    pub const fn validate(&self) -> Result<(), StreamTuningError> {
        if self.coalesce.is_zero() || self.coalesce.as_millis() > 1_000 {
            Err(StreamTuningError::Coalesce)
        } else if self.keepalive.as_nanos() <= self.coalesce.as_nanos() {
            Err(StreamTuningError::Keepalive)
        } else if self.buffer == 0 {
            Err(StreamTuningError::Buffer)
        } else {
            Ok(())
        }
    }
}

const _: () = match StreamTuning::DEFAULT.validate() {
    Ok(()) => (),
    Err(_) => panic!("the default activity stream tuning is invalid"),
};

/// One network's record.
pub struct Activity {
    /// Names this record: a new one (a restart) starts its sequence over.
    epoch: String,
    record: Mutex<Record>,
    rounds: AtomicU64,
    last_snapshot: Mutex<Option<Instant>>,
    last_node_pool: Mutex<Option<Instant>>,
    /// The next sequence number, sent each time an event is recorded: what
    /// wakes the streams.
    appended: tokio::sync::watch::Sender<u64>,
    /// Streams open on this record: while there are any, someone watches.
    streams: AtomicUsize,
}

/// A stream's hold on the record ([`Activity::watching`]): while it is
/// held, someone watches.
pub struct Watching(Arc<Activity>);

impl Drop for Watching {
    fn drop(&mut self) {
        self.0.streams.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct Record {
    events: VecDeque<Recorded>,
    next_seq: u64,
}

/// What [`Activity::page`] returns.
#[derive(Debug, PartialEq)]
pub struct Page {
    pub events: Vec<Recorded>,
    pub next: u64,
    pub gap: bool,
}

impl Default for Activity {
    fn default() -> Self {
        Self {
            epoch: uuid::Uuid::new_v4().simple().to_string(),
            record: Mutex::default(),
            rounds: AtomicU64::new(0),
            last_snapshot: Mutex::new(None),
            last_node_pool: Mutex::new(None),
            appended: tokio::sync::watch::Sender::new(0),
            streams: AtomicUsize::new(0),
        }
    }
}

impl std::fmt::Debug for Activity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let record = self.record.lock();
        f.debug_struct("Activity")
            .field("epoch", &self.epoch)
            .field("events", &record.events.len())
            .field("next_seq", &record.next_seq)
            .finish_non_exhaustive()
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

impl Activity {
    /// Records `event` as happening now. Several loops record at once, so
    /// the time is read under the record's lock and never goes back: the
    /// record is in time order as well as sequence order (a clock stepped
    /// back holds at the last time until it catches up).
    pub fn record(&self, event: Event) {
        let mut record = self.record.lock();
        let at_ms = record
            .events
            .back()
            .map_or_else(now_ms, |last| now_ms().max(last.at_ms));
        Self::push(&mut record, at_ms, event);
        // Under the lock, so the number sent never goes back.
        self.appended.send_replace(record.next_seq);
    }

    /// Woken each time an event is recorded (with the next sequence
    /// number): a stream waits on it instead of polling.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.appended.subscribe()
    }

    /// Counts a stream as watching for as long as the returned hold lives.
    pub fn watching(self: &Arc<Self>) -> Watching {
        self.streams.fetch_add(1, Ordering::SeqCst);
        Watching(Arc::clone(self))
    }

    /// Streams open on this record now.
    pub fn streams(&self) -> usize {
        self.streams.load(Ordering::SeqCst)
    }

    fn push(record: &mut Record, at_ms: i64, event: Event) {
        let seq = record.next_seq;
        record.next_seq += 1;
        record.events.push_back(Recorded { seq, at_ms, event });
        let keep_from = at_ms.saturating_sub(i64::try_from(KEEP.as_millis()).unwrap_or(i64::MAX));
        while record.events.len() > MAX_EVENTS
            || record.events.front().is_some_and(|e| e.at_ms < keep_from)
        {
            record.events.pop_front();
        }
    }

    /// The number of the round about to start.
    pub fn next_round(&self) -> u64 {
        self.rounds.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// The last round started.
    pub fn round(&self) -> u64 {
        self.rounds.load(Ordering::Relaxed)
    }

    /// Whether a snapshot is due (and, if so, marks it taken now).
    pub fn snapshot_due(&self) -> bool {
        let mut last = self.last_snapshot.lock();
        let due = last.is_none_or(|at| at.elapsed() >= SNAPSHOT_EVERY);
        if due {
            *last = Some(Instant::now());
        }
        due
    }

    /// Whether the node's whole pool is due to be asked for (and, if so,
    /// marks it asked now): only while a stream is open on the record
    /// ([`Self::watching`]), every [`NODE_POOL_EVERY`].
    pub fn node_pool_due(&self) -> bool {
        let watched = self.streams() > 0;
        let mut last = self.last_node_pool.lock();
        let due = watched && last.is_none_or(|at| at.elapsed() >= NODE_POOL_EVERY);
        if due {
            *last = Some(Instant::now());
        }
        due
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// The events from sequence number `from` on, oldest first. Without
    /// `from`, or if `from` has already left the record (or belongs to
    /// another epoch's numbering), everything from the oldest snapshot kept,
    /// so the reader can rebuild the state from there; `gap` says it had to.
    pub fn page(&self, from: Option<u64>) -> Page {
        let record = self.record.lock();
        let oldest = record.events.front().map_or(record.next_seq, |e| e.seq);
        let continues = from.filter(|from| (oldest..=record.next_seq).contains(from));
        let start = continues.map_or_else(
            || {
                record
                    .events
                    .iter()
                    .position(|e| matches!(e.event, Event::Snapshot(_)))
                    .unwrap_or(0)
            },
            |from| usize::try_from(from - oldest).unwrap_or(usize::MAX),
        );
        let events: Vec<Recorded> = record.events.iter().skip(start).cloned().collect();
        Page {
            events,
            next: record.next_seq,
            gap: from.is_some() && continues.is_none(),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use shared::activity::Snapshot;

    /// Records `event` at `at_ms`, whatever the clock says.
    fn record_at(activity: &Activity, at_ms: i64, event: Event) {
        Activity::push(&mut activity.record.lock(), at_ms, event);
    }

    /// Recording never goes back in time: an event recorded after one
    /// stamped ahead of the clock takes that time.
    #[test]
    fn the_record_is_in_time_order() {
        let activity = Activity::default();
        let ahead = now_ms() + 60_000;
        record_at(&activity, ahead, Event::ReorgCollected);
        activity.record(Event::ReorgCollected);
        let page = activity.page(None);
        assert_eq!(page.events[1].at_ms, ahead);
    }

    fn seqs(page: &Page) -> Vec<u64> {
        page.events.iter().map(|e| e.seq).collect()
    }

    /// A reader polling with the `next` it was given gets each event once,
    /// in order, and nothing when nothing happened.
    #[test]
    fn a_reader_following_next_sees_every_event_once() {
        let activity = Activity::default();
        activity.record(Event::Snapshot(Box::default()));
        activity.record(Event::ChainChecked {
            agrees: true,
            looked_up: true,
        });
        let first = activity.page(None);
        assert_eq!(seqs(&first), [0, 1]);
        assert!(!first.gap);
        assert!(activity.page(Some(first.next)).events.is_empty());
        activity.record(Event::Vanished { looked: 2 });
        let second = activity.page(Some(first.next));
        assert_eq!(seqs(&second), [2]);
        assert_eq!(second.next, 3);
        assert!(!second.gap);
    }

    /// A fresh reader starts at the oldest snapshot: the events before it
    /// can't be applied to anything.
    #[test]
    fn a_fresh_reader_starts_at_the_oldest_snapshot() {
        let activity = Activity::default();
        activity.record(Event::ChainChecked {
            agrees: true,
            looked_up: true,
        });
        activity.record(Event::Snapshot(Box::new(Snapshot {
            round: 4,
            ..Snapshot::default()
        })));
        activity.record(Event::Vanished { looked: 1 });
        activity.record(Event::Snapshot(Box::default()));
        assert_eq!(seqs(&activity.page(None)), [1, 2, 3]);
    }

    /// A reader that fell behind the record, or remembers another epoch's
    /// numbers (the engine restarted), starts over from a snapshot and is
    /// told so.
    #[test]
    fn a_reader_behind_the_record_or_from_another_epoch_starts_over() {
        let activity = Activity::default();
        activity.record(Event::Snapshot(Box::default()));
        activity.record(Event::ChainChecked {
            agrees: true,
            looked_up: true,
        });
        let ahead = activity.page(Some(99));
        assert!(ahead.gap);
        assert_eq!(seqs(&ahead), [0, 1]);
        assert_ne!(activity.epoch(), Activity::default().epoch());

        // Push the first events out by age.
        let old = now_ms() - KEEP.as_millis() as i64 - 1;
        let aged = Activity::default();
        record_at(&aged, old, Event::Snapshot(Box::default()));
        record_at(
            &aged,
            old,
            Event::ChainChecked {
                agrees: true,
                looked_up: true,
            },
        );
        aged.record(Event::Snapshot(Box::default()));
        let behind = aged.page(Some(1));
        assert!(behind.gap);
        assert_eq!(seqs(&behind), [2]);
    }

    /// The node's whole pool is asked for only while a stream is open on
    /// the record, and then at most every [`NODE_POOL_EVERY`].
    #[test]
    fn the_node_s_pool_is_asked_for_only_while_someone_watches() {
        let activity = Arc::new(Activity::default());
        activity.page(None);
        assert!(!activity.node_pool_due(), "a read alone isn't watching");
        let watching = activity.watching();
        assert!(activity.node_pool_due());
        assert!(!activity.node_pool_due(), "asked a moment ago");
        *activity.last_node_pool.lock() = Instant::now().checked_sub(NODE_POOL_EVERY);
        assert!(activity.node_pool_due(), "due again after the interval");
        *activity.last_node_pool.lock() = None;
        drop(watching);
        assert_eq!(activity.streams(), 0);
        assert!(!activity.node_pool_due(), "the last stream closed");
    }

    /// Each event recorded wakes a stream waiting on the record, with the
    /// number the next will get.
    #[tokio::test]
    async fn recording_wakes_the_streams() {
        let activity = Activity::default();
        let mut appended = activity.subscribe();
        activity.record(Event::ReorgCollected);
        appended.changed().await.unwrap();
        assert_eq!(*appended.borrow_and_update(), 1);
    }

    /// The default stream tuning is valid; one that would send a message
    /// per event, keep nothing alive, or hold nothing is not.
    #[test]
    fn stream_tuning_is_validated() {
        let ok = StreamTuning::DEFAULT;
        assert_eq!(ok.validate(), Ok(()));
        for (tuning, error) in [
            (
                StreamTuning {
                    coalesce: Duration::ZERO,
                    ..ok
                },
                StreamTuningError::Coalesce,
            ),
            (
                StreamTuning {
                    coalesce: Duration::from_secs(2),
                    ..ok
                },
                StreamTuningError::Coalesce,
            ),
            (
                StreamTuning {
                    keepalive: ok.coalesce,
                    ..ok
                },
                StreamTuningError::Keepalive,
            ),
            (StreamTuning { buffer: 0, ..ok }, StreamTuningError::Buffer),
        ] {
            assert_eq!(tuning.validate(), Err(error));
        }
    }

    /// Older than [`KEEP`] or past [`MAX_EVENTS`], the oldest go.
    #[test]
    fn the_record_is_bounded_by_age_and_count() {
        let activity = Activity::default();
        let now = now_ms();
        record_at(
            &activity,
            now - KEEP.as_millis() as i64 - 1,
            Event::ReorgCollected,
        );
        record_at(&activity, now, Event::ReorgCollected);
        assert_eq!(seqs(&activity.page(Some(0))), [1], "the stale event went");

        let full = Activity::default();
        for _ in 0..=MAX_EVENTS {
            record_at(&full, now, Event::ReorgCollected);
        }
        let page = full.page(Some(1));
        assert_eq!(page.events.len(), MAX_EVENTS);
        assert_eq!(page.events[0].seq, 1);
        assert!(!page.gap);
    }

    #[test]
    fn a_snapshot_is_due_once_per_interval_and_rounds_count_up() {
        let activity = Activity::default();
        assert!(activity.snapshot_due());
        assert!(!activity.snapshot_due(), "taken a moment ago");
        assert_eq!(activity.next_round(), 1);
        assert_eq!(activity.next_round(), 2);
        assert_eq!(activity.round(), 2);
    }
}
