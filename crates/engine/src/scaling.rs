//! What one network's block scan is doing, kept live for `/status`
//! (docs/engine_scaling.md sections 2, 5 and 6): how requests are sized,
//! the block in progress and how long it has taken, how fast recent blocks
//! went, and where the time goes. The scan writes it as it works; `/status`
//! reads it at any moment, even in the middle of a long round.

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;

pub use shared::scaling::{
    BlockInProgress, ChunkPlan, DoneBlock, HeadersFirst, HeadersFirstReason, ScanReport, Trend,
    SLOW_BLOCK_SECS,
};

/// Blocks kept for the recent figures.
const RECENT_BLOCKS: usize = 64;
/// How far back "recent" reaches for blocks a minute and the time split.
const RECENT_SECS: i64 = 600;

/// One network's scan progress, shared between its scan loop and `/status`.
/// How long blocks' headers are read before the blocks after the last sign
/// that a block may be too large to fetch whole (docs/engine_scaling.md
/// section 4).
pub const HEADERS_FIRST_SECS: i64 = 60 * 60;

pub type SharedProgress = Arc<Mutex<ScanProgress>>;

pub fn new_progress() -> SharedProgress {
    Arc::new(Mutex::new(ScanProgress::default()))
}

#[derive(Debug, Clone)]
pub struct ScanProgress {
    /// Running average of a block's size on the wire; doubled after a
    /// request that ran out of time or came back too large.
    pub avg_bytes_per_block: f64,
    /// The last block request's plan.
    pub last_chunk: Option<ChunkPlan>,
    /// The block being scanned and when the scan of it started.
    pub in_progress: Option<BlockInProgress>,
    /// Blocks finished lately, oldest first.
    pub recent: VecDeque<DoneBlock>,
    /// Time spent fetching blocks and scanning their transactions, by when
    /// it was spent: what sets the pace.
    pub time: VecDeque<TimeSpent>,
    /// The largest the block cache has been, and when (for an hour).
    pub peak_cache: Option<(i64, u64)>,
    /// Bytes of blocks the cache let go of before any scan read them, since
    /// the engine started: fetched for nothing, and fetched again if needed.
    pub discarded_cache_bytes: u64,
    /// The same, by when, for the last [`RECENT_SECS`].
    discarded_recent: VecDeque<(i64, u64)>,
    /// The time the last round was given: the base, unless one page of a
    /// large block needed more (docs/engine_scaling.md section 4).
    pub round_budget: std::time::Duration,
    /// Until when blocks' headers are read before the blocks, and why
    /// (docs/engine_scaling.md section 4).
    pub headers_first: Option<(i64, HeadersFirstReason)>,
}

impl Default for ScanProgress {
    fn default() -> Self {
        ScanProgress {
            avg_bytes_per_block: crate::scanner::SCAN_CHUNK_INITIAL_AVG_BYTES,
            last_chunk: None,
            in_progress: None,
            recent: VecDeque::new(),
            time: VecDeque::new(),
            peak_cache: None,
            discarded_cache_bytes: 0,
            discarded_recent: VecDeque::new(),
            round_budget: crate::work::ROUND_BUDGET,
            headers_first: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeSpent {
    pub unix: i64,
    pub fetch_secs: f64,
    pub scan_secs: f64,
    /// Transactions scanned times stores scanned for, during `scan_secs`.
    pub tx_scans: u64,
}

impl ScanProgress {
    /// Blocks' headers are read before the blocks for the next
    /// [`HEADERS_FIRST_SECS`], because of `reason`.
    pub fn want_headers_first(&mut self, now_unix: i64, reason: HeadersFirstReason) {
        self.headers_first = Some((now_unix + HEADERS_FIRST_SECS, reason));
    }

    /// Whether blocks' headers are being read before the blocks.
    pub fn headers_first_on(&self, now_unix: i64) -> bool {
        self.headers_first
            .is_some_and(|(until, _)| until > now_unix)
    }

    /// The scan of block `height` is starting (or carrying on).
    pub fn start_block(&mut self, height: u64, now_unix: i64) {
        if self.in_progress.as_ref().is_none_or(|b| b.height != height) {
            self.in_progress = Some(BlockInProgress {
                height,
                started_unix: now_unix,
                wire_bytes: None,
                pages: None,
            });
        }
    }

    /// Block `height`, scanned in pages, is `done_txs` of `total_txs`
    /// transactions through, fetching `page_txs` more.
    pub fn page(&mut self, height: u64, done_txs: u64, total_txs: u64, page_txs: u64) {
        if let Some(block) = self.in_progress.as_mut().filter(|b| b.height == height) {
            block.pages = Some(shared::scaling::PageProgress {
                done_txs,
                total_txs,
                page_txs,
            });
        }
    }

    /// Block `height` was fetched: `wire_bytes` of it.
    pub fn fetched_block(&mut self, height: u64, wire_bytes: u64) {
        if let Some(block) = self.in_progress.as_mut().filter(|b| b.height == height) {
            block.wire_bytes = Some(wire_bytes);
        }
    }

    /// The scan of block `height` was committed.
    pub fn finish_block(&mut self, height: u64, now_unix: i64) {
        let Some(block) = self.in_progress.take() else {
            return;
        };
        if block.height != height {
            self.in_progress = Some(block);
            return;
        }
        self.recent.push_back(DoneBlock {
            height,
            wire_bytes: block.wire_bytes.unwrap_or(0),
            secs: (now_unix - block.started_unix).max(0) as f64,
            finished_unix: now_unix,
        });
        while self.recent.len() > RECENT_BLOCKS {
            self.recent.pop_front();
        }
    }

    pub fn spent(&mut self, now_unix: i64, fetch_secs: f64, scan_secs: f64, tx_scans: u64) {
        self.time.push_back(TimeSpent {
            unix: now_unix,
            fetch_secs,
            scan_secs,
            tx_scans,
        });
        while self
            .time
            .front()
            .is_some_and(|t| t.unix < now_unix - RECENT_SECS)
        {
            self.time.pop_front();
        }
    }

    /// The block cache let go of `bytes` before any scan read them.
    pub fn discarded(&mut self, bytes: u64, now_unix: i64) {
        if bytes == 0 {
            return;
        }
        self.discarded_cache_bytes += bytes;
        self.discarded_recent.push_back((now_unix, bytes));
        while self
            .discarded_recent
            .front()
            .is_some_and(|(unix, _)| *unix < now_unix - RECENT_SECS)
        {
            self.discarded_recent.pop_front();
        }
    }

    pub fn cache_bytes(&mut self, bytes: u64, now_unix: i64) {
        let stale = self.peak_cache.is_some_and(|(at, _)| at < now_unix - 3600);
        if stale || self.peak_cache.is_none_or(|(_, peak)| bytes >= peak) {
            self.peak_cache = Some((now_unix, bytes));
        }
    }

    /// Seconds one transaction takes to scan for one store, lately.
    pub fn secs_per_tx_scan(&self) -> Option<f64> {
        let (secs, scans) = self
            .time
            .iter()
            .fold((0.0, 0u64), |(s, n), t| (s + t.scan_secs, n + t.tx_scans));
        (scans > 0).then(|| secs / scans as f64)
    }

    /// Figures for `/status`, as of `now_unix`.
    pub fn report(&self, now_unix: i64) -> ScanReport {
        let recent: Vec<&DoneBlock> = self
            .recent
            .iter()
            .filter(|b| b.finished_unix >= now_unix - RECENT_SECS)
            .collect();
        let blocks_per_minute = recent.len() as f64 / (RECENT_SECS as f64 / 60.0);
        let (fetch, scan) = self
            .time
            .iter()
            .fold((0.0, 0.0), |(f, s), t| (f + t.fetch_secs, s + t.scan_secs));
        let sizes: Vec<u64> = self.recent.iter().map(|b| b.wire_bytes).collect();
        ScanReport {
            avg_block_bytes: self.avg_bytes_per_block.round() as u64,
            block_size_trend: trend(&sizes),
            last_chunk: self.last_chunk,
            blocks_per_minute,
            fetch_secs_recent: fetch,
            scan_secs_recent: scan,
            largest_recent: self.recent.iter().max_by_key(|b| b.wire_bytes).copied(),
            in_progress: self.in_progress.clone(),
            in_progress_secs: self
                .in_progress
                .as_ref()
                .map(|b| (now_unix - b.started_unix).max(0)),
            peak_cache_bytes: self
                .peak_cache
                .filter(|(at, _)| *at >= now_unix - 3600)
                .map(|(_, bytes)| bytes),
            discarded_cache_bytes_recent: self
                .discarded_recent
                .iter()
                .filter(|(unix, _)| *unix >= now_unix - RECENT_SECS)
                .map(|(_, bytes)| bytes)
                .sum(),
            round_budget_secs: Some(self.round_budget.as_secs()),
            headers_first: self
                .headers_first
                .filter(|(until, _)| *until > now_unix)
                .map(|(until, reason)| HeadersFirst {
                    remaining_secs: until - now_unix,
                    reason,
                }),
        }
    }
}

/// Whether recent block sizes are rising, falling or steady: the later
/// half's average against the earlier half's, with a 20 % band.
fn trend(sizes: &[u64]) -> Trend {
    if sizes.len() < 4 {
        return Trend::Steady;
    }
    let half = sizes.len() / 2;
    let avg = |s: &[u64]| s.iter().sum::<u64>() as f64 / s.len() as f64;
    let (earlier, later) = (avg(&sizes[..half]), avg(&sizes[half..]));
    if later > earlier * 1.2 {
        Trend::Rising
    } else if later * 1.2 < earlier {
        Trend::Falling
    } else {
        Trend::Steady
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Headers come first for an hour after the last sign that blocks may
    /// be large, and each sign starts the hour again
    /// (docs/engine_scaling.md section 4).
    #[test]
    fn headers_come_first_for_an_hour_after_each_sign() {
        let mut progress = ScanProgress::default();
        assert!(!progress.headers_first_on(1_000));
        assert_eq!(progress.report(1_000).headers_first, None);
        progress.want_headers_first(1_000, HeadersFirstReason::FailedRequest);
        assert!(progress.headers_first_on(1_000 + HEADERS_FIRST_SECS - 1));
        assert!(!progress.headers_first_on(1_000 + HEADERS_FIRST_SECS));
        progress.want_headers_first(2_000, HeadersFirstReason::LargeBlock);
        assert!(progress.headers_first_on(1_000 + HEADERS_FIRST_SECS));
        assert_eq!(
            progress.report(2_600).headers_first,
            Some(HeadersFirst {
                remaining_secs: HEADERS_FIRST_SECS - 600,
                reason: HeadersFirstReason::LargeBlock,
            })
        );
    }

    #[test]
    fn a_block_is_timed_from_its_start_to_its_commit_across_rounds() {
        let mut progress = ScanProgress::default();
        progress.start_block(10, 1_000);
        // A later round carrying on with the same block keeps its start.
        progress.start_block(10, 1_050);
        progress.fetched_block(10, 4_000_000);
        let report = progress.report(1_130);
        assert_eq!(report.in_progress_secs, Some(130));
        assert_eq!(
            report.in_progress.as_ref().unwrap().wire_bytes,
            Some(4_000_000)
        );

        progress.finish_block(10, 1_140);
        let report = progress.report(1_140);
        assert_eq!(report.in_progress, None);
        assert_eq!(
            report.largest_recent,
            Some(DoneBlock {
                height: 10,
                wire_bytes: 4_000_000,
                secs: 140.0,
                finished_unix: 1_140
            })
        );
        assert!(
            (report.blocks_per_minute - 0.1).abs() < 1e-9,
            "one block in ten minutes"
        );
    }

    #[test]
    fn finishing_another_block_leaves_the_one_in_progress() {
        let mut progress = ScanProgress::default();
        progress.start_block(10, 1_000);
        progress.finish_block(9, 1_001);
        assert_eq!(progress.in_progress.as_ref().unwrap().height, 10);
    }

    #[test]
    fn block_sizes_rising_and_falling_are_named() {
        assert_eq!(trend(&[10, 10, 10, 20, 20, 20]), Trend::Rising);
        assert_eq!(trend(&[20, 20, 10, 10]), Trend::Falling);
        assert_eq!(trend(&[10, 11, 10, 11]), Trend::Steady);
        assert_eq!(trend(&[1, 100]), Trend::Steady, "too few to say");
    }

    #[test]
    fn time_is_kept_for_ten_minutes_and_gives_the_cost_of_a_scan() {
        let mut progress = ScanProgress::default();
        progress.spent(1_000, 2.0, 1.0, 100);
        progress.spent(1_500, 1.0, 3.0, 100);
        assert_eq!(progress.secs_per_tx_scan(), Some(0.02));
        progress.spent(1_700, 0.0, 0.0, 0);
        let report = progress.report(1_700);
        assert_eq!(
            (report.fetch_secs_recent, report.scan_secs_recent),
            (1.0, 3.0),
            "the first aged out"
        );
    }

    #[test]
    fn discarded_bytes_are_reported_for_the_last_ten_minutes() {
        let mut progress = ScanProgress::default();
        progress.discarded(0, 1_000);
        progress.discarded(500, 1_000);
        progress.discarded(200, 1_500);
        assert_eq!(progress.report(1_550).discarded_cache_bytes_recent, 700);
        assert_eq!(
            progress.report(1_700).discarded_cache_bytes_recent,
            200,
            "the first is over ten minutes old"
        );
        assert_eq!(progress.report(3_000).discarded_cache_bytes_recent, 0);
        assert_eq!(
            progress.discarded_cache_bytes, 700,
            "the total since start stays"
        );
    }

    #[test]
    fn the_cache_peak_lasts_an_hour() {
        let mut progress = ScanProgress::default();
        progress.cache_bytes(500, 1_000);
        progress.cache_bytes(100, 1_100);
        assert_eq!(progress.report(1_100).peak_cache_bytes, Some(500));
        assert_eq!(progress.report(5_000).peak_cache_bytes, None);
        progress.cache_bytes(100, 5_000);
        assert_eq!(progress.report(5_000).peak_cache_bytes, Some(100));
    }
}
