//! What the engine reports about its scaling in `/status`
//! (docs/engine_scaling.md section 6): each node's link, each network's
//! scan, and the slow-block state. The engine fills these in; monokulo
//! renders them on the status and admin pages.

use serde::{Deserialize, Serialize};

/// One block taking longer than this, while its node answers, is slow: the
/// status page shows it yellow and says why (docs/engine_scaling.md 5).
pub const SLOW_BLOCK_SECS: i64 = 120;

/// What has been measured of one node's link.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkSnapshot {
    /// Whether these come from real calls, not the starting guesses.
    pub measured: bool,
    pub rtt_ms: u64,
    pub ttfb_per_block_ms: u64,
    pub rate_bytes_per_sec: u64,
    pub bytes_per_block: u64,
    pub last_measured_unix: Option<i64>,
    pub timeouts_last_hour: u32,
    pub failures_last_hour: u32,
    /// One point per minute with a measurement, oldest first.
    pub history: Vec<LinkPoint>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkPoint {
    pub minute_unix: i64,
    pub rate_bytes_per_sec: u64,
    pub rtt_ms: u64,
    pub ttfb_per_block_ms: u64,
}

/// What set a block request's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkLimit {
    /// The response cap from the scan memory budget.
    Memory,
    /// What the node's link delivers in a target call.
    Link,
    /// The most blocks (or a large block's transactions) one request asks
    /// for.
    Maximum,
    /// There were no more blocks to ask for.
    Remaining,
    /// For a large block's page: what the scan gets through in the round's
    /// time for blocks.
    Cpu,
}

/// How many blocks a request asks for, and why that many. For a large
/// block's page, `blocks` counts its transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkPlan {
    pub blocks: u64,
    pub limited_by: ChunkLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trend {
    Rising,
    Falling,
    Steady,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockInProgress {
    pub height: u64,
    pub started_unix: i64,
    /// Its size on the wire, once fetched (for a large block, its weight).
    pub wire_bytes: Option<u64>,
    /// For a large block scanned a page at a time: how far it has got.
    #[serde(default)]
    pub pages: Option<PageProgress>,
}

/// How far a large block's scan in pages has got
/// (docs/engine_scaling.md section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageProgress {
    /// Transactions scanned for every store.
    pub done_txs: u64,
    pub total_txs: u64,
    /// Transactions on the page being fetched.
    pub page_txs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DoneBlock {
    pub height: u64,
    pub wire_bytes: u64,
    pub secs: f64,
    pub finished_unix: i64,
}

/// One network's block scan, as it has gone lately.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanReport {
    pub avg_block_bytes: u64,
    pub block_size_trend: Trend,
    pub last_chunk: Option<ChunkPlan>,
    pub blocks_per_minute: f64,
    /// Seconds spent fetching blocks and scanning them in the last ten
    /// minutes.
    pub fetch_secs_recent: f64,
    pub scan_secs_recent: f64,
    pub largest_recent: Option<DoneBlock>,
    pub in_progress: Option<BlockInProgress>,
    pub in_progress_secs: Option<i64>,
    pub peak_cache_bytes: Option<u64>,
    /// The time the last round was given, when known.
    #[serde(default)]
    pub round_budget_secs: Option<u64>,
}

/// What sets a network's scan pace right now: the one thing an admin would
/// change to make it faster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pace {
    /// Nothing: the scan is at the node's tip.
    CaughtUp,
    /// The scan memory budget caps each request.
    Memory,
    /// The node's link delivers less than the budget allows.
    Link,
    /// Scanning transactions for stores takes longer than fetching them.
    Cpu,
    /// The round's time slice for blocks.
    Round,
}

/// A block that has taken us longer than [`SLOW_BLOCK_SECS`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlowBlock {
    pub height: u64,
    pub wire_bytes: Option<u64>,
    pub elapsed_secs: i64,
    /// The node it comes from, and that node's measured rate.
    pub node: Option<String>,
    pub rate_bytes_per_sec: Option<u64>,
    /// How much longer it should take at that rate.
    pub remaining_secs: Option<i64>,
}

/// One network's scaling figures in `/status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkScaling {
    pub scan: ScanReport,
    /// Blocks between what the scan has recorded and the node's tip.
    pub blocks_behind: u64,
    /// At the recent pace, how long until caught up.
    pub catch_up_secs: Option<u64>,
    pub pace: Pace,
    pub budget_mb: u32,
    /// The most this machine allows each network, if its memory is known.
    pub max_budget_mb: Option<u32>,
    /// The round deadline in force, and the base it starts from.
    pub round_deadline_secs: u64,
    pub round_base_secs: u64,
    pub slow: Option<SlowBlock>,
}

impl NetworkScaling {
    /// What sets the pace, from the facts: caught up, else whatever limited
    /// the last block request, else whichever of fetching and scanning took
    /// longer lately.
    pub fn pace_of(blocks_behind: u64, scan: &ScanReport) -> Pace {
        if blocks_behind == 0 {
            return Pace::CaughtUp;
        }
        match scan.last_chunk.map(|chunk| chunk.limited_by) {
            Some(ChunkLimit::Link) => Pace::Link,
            Some(ChunkLimit::Memory) => Pace::Memory,
            _ if scan.scan_secs_recent > scan.fetch_secs_recent => Pace::Cpu,
            _ => Pace::Round,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(limit: Option<ChunkLimit>, fetch: f64, scan: f64) -> ScanReport {
        ScanReport {
            avg_block_bytes: 0,
            block_size_trend: Trend::Steady,
            last_chunk: limit.map(|limited_by| ChunkPlan {
                blocks: 1,
                limited_by,
            }),
            blocks_per_minute: 0.0,
            fetch_secs_recent: fetch,
            scan_secs_recent: scan,
            largest_recent: None,
            in_progress: None,
            in_progress_secs: None,
            peak_cache_bytes: None,
            round_budget_secs: None,
        }
    }

    #[test]
    fn the_pace_is_named_by_what_limits_the_scan() {
        let pace = NetworkScaling::pace_of;
        assert_eq!(
            pace(0, &scan(Some(ChunkLimit::Link), 1.0, 0.0)),
            Pace::CaughtUp
        );
        assert_eq!(pace(5, &scan(Some(ChunkLimit::Link), 1.0, 0.0)), Pace::Link);
        assert_eq!(
            pace(5, &scan(Some(ChunkLimit::Memory), 1.0, 0.0)),
            Pace::Memory
        );
        assert_eq!(
            pace(5, &scan(Some(ChunkLimit::Maximum), 1.0, 3.0)),
            Pace::Cpu
        );
        assert_eq!(
            pace(5, &scan(Some(ChunkLimit::Remaining), 3.0, 1.0)),
            Pace::Round
        );
        assert_eq!(pace(5, &scan(None, 0.0, 0.0)), Pace::Round);
    }
}
