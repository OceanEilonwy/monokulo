//! What the engine's scanner is doing, event by event, for monokulo's engine
//! page (docs/engine_visualizer.md). The engine records these as its loops
//! work and serves them on its admin API; monokulo turns them into the
//! page's sentences and animation.
//!
//! Events carry facts, never prose: heights, counts, tiers, outcomes.
//! Wording is monokulo's. No store, order or payment is named, and no amount
//! appears; block hashes and transaction ids are public chain data, and are
//! shortened.

use serde::{Deserialize, Serialize};

use crate::order_status::OrderStatus;

/// The kinds of the scanner's work, in priority order within a round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Reorg detection and reconciliation.
    Chain,
    /// Scanning blocks for tenants: the new ones first, then catch-up.
    Blocks,
    /// Scanning the mempool for zero-confirmation payments.
    Mempool,
    /// Payments that left the pool, and order status recomputes.
    Settlement,
    /// Bookkeeping that can lag: scanned ranges, void rechecks, pruning.
    Upkeep,
}

impl Tier {
    pub const ALL: [Tier; 5] = [
        Tier::Chain,
        Tier::Blocks,
        Tier::Mempool,
        Tier::Settlement,
        Tier::Upkeep,
    ];

    /// Its place in [`Tier::ALL`].
    pub const fn index(self) -> usize {
        self as usize
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Tier::Chain => "chain",
            Tier::Blocks => "blocks",
            Tier::Mempool => "mempool",
            Tier::Settlement => "settlement",
            Tier::Upkeep => "upkeep",
        })
    }
}

/// What a tier is waiting for when it has work it can't do yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wait {
    /// The node's chain height couldn't be read this round.
    ChainHeightUnknown,
    /// A reorg is being reconciled: blocks wait for the rewind.
    ReorgBeingReconciled,
    /// A rewind happened this round: replacement blocks are scanned from
    /// the next, against a freshly read chain.
    RewoundThisRound,
    /// The node failed or didn't answer; retried next round.
    NodeFailed,
    /// The node reports a tip it can't serve yet (first run).
    NodeCannotServeTip,
    /// The mempool couldn't be read.
    MempoolUnreadable,
    /// Every remaining reorg candidate is waiting out a retry delay.
    ReorgCandidatesRetrying,
    /// The node's next block doesn't extend the recorded chain: a reorg the
    /// chain tier hasn't opened a job for yet (it failed this round, or the
    /// fork happened since it looked).
    ChainDiverged,
}

impl std::fmt::Display for Wait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Wait::ChainHeightUnknown => "the chain height is unknown",
            Wait::ReorgBeingReconciled => "a reorganisation is being reconciled",
            Wait::RewoundThisRound => {
                "rewound this round; replacement blocks are scanned from the next"
            }
            Wait::NodeFailed => "the node failed",
            Wait::NodeCannotServeTip => "the node can't serve its own tip yet",
            Wait::MempoolUnreadable => "the mempool couldn't be read",
            Wait::ReorgCandidatesRetrying => "reorg candidates are waiting to be retried",
            Wait::ChainDiverged => {
                "the node's chain differs from the recorded one; waiting for reorg reconciliation"
            }
        })
    }
}

/// How a tier ended its round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TierOutcome {
    /// Ran out of work.
    Idle,
    /// Ran out of time with work left.
    Backlogged,
    Blocked(Wait),
    Failed,
}

/// What one unit of work did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitProgress {
    Advanced,
    Idle,
    Blocked(Wait),
    Failed,
}

/// Which group of stores a block was scanned for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    /// The stores at the network's high-water mark, scanning new blocks.
    Frontier,
    /// Stores behind it, catching up.
    CatchUp,
}

/// Which of the two mempool paths looked at the pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolPath {
    /// The fast path, every fraction of a second, new transactions only.
    Fast,
    /// The round's mempool tier, a rotating slice of the whole pool.
    Round,
}

/// What ended the scan loop's wait between two rounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wake {
    /// The poll interval ran out.
    Interval,
    /// The node announced a new block (docs/monero_zmq.md).
    NewBlock,
}

/// An order's status changing in a recompute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    pub from: OrderStatus,
    pub to: OrderStatus,
}

/// One thing the scanner did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// The state the page draws, taken every few seconds: where a page
    /// opened later starts, and what keeps an open page from drifting.
    Snapshot(Box<Snapshot>),
    /// A round started, having read the node's tip (`None`: it couldn't).
    RoundStarted {
        round: u64,
        budget_ms: u64,
        tip: Option<u64>,
    },
    /// One unit of `tier`'s work, from `start_ms` into its round.
    Unit {
        tier: Tier,
        /// 1 for the tier's reserved share, 2 for time left over.
        pass: u8,
        start_ms: u64,
        ms: u64,
        progress: UnitProgress,
    },
    /// A tier stopped for the rest of its round: out of work, waiting, or
    /// failed. A tier still open at the round's end was out of time.
    TierEnded { tier: Tier, outcome: TierOutcome },
    RoundFinished {
        round: u64,
        ms: u64,
        backlogged: bool,
    },
    /// The loop waited `ms` between two rounds.
    Slept { ms: u64, woken_by: Wake },
    /// Reorg detection: whether the recorded chain agrees with the node's.
    ChainChecked {
        agrees: bool,
        /// The node was asked for a block's hash. False when the hash of
        /// the node's tip, which came with the round's tip request, was
        /// enough.
        looked_up: bool,
    },
    /// A reorg job was opened, or deepened, from `fork`.
    ReorgFound { fork: u64 },
    /// A page of the payments a reorg may affect was queued.
    ReorgCollected,
    /// Queued payments were re-examined against the node's chain.
    ReorgProcessed {
        examined: u64,
        changed: u64,
        voided: u64,
    },
    /// Every candidate was handled: blocks from `fork` up were deleted.
    ReorgRewound { fork: u64 },
    /// First run on the network: the scan starts at `height`.
    Seeded { height: u64 },
    /// Blocks `from..from + count` came from the node.
    Fetched {
        from: u64,
        count: u64,
        bytes: u64,
        /// Fetched while the block before them was being scanned.
        ahead: bool,
    },
    BlockScanStarted {
        height: u64,
        group: Group,
        stores: u64,
        txs: u64,
        /// Nobody to scan it for: recorded from its header alone.
        header_only: bool,
    },
    /// A large block's scan has got this far.
    BlockProgress {
        height: u64,
        done_txs: u64,
        total_txs: u64,
    },
    /// Out of time partway through a block: how far each store got was
    /// saved, and the next unit carries on from there.
    Checkpointed {
        height: u64,
        stores: u64,
        done_txs: u64,
        total_txs: u64,
    },
    /// A block's scan was committed: `stores` moved past it, with `matches`
    /// payments found in it, and `idle_moved` stores with nothing that could
    /// have been paid moved on with it (to this block on the frontier,
    /// straight to the high-water mark when catching up).
    Committed {
        height: u64,
        group: Group,
        stores: u64,
        matches: u64,
        idle_moved: u64,
        header_only: bool,
    },
    /// The node's block at `height` doesn't extend the recorded chain.
    Diverged { height: u64 },
    /// Stores with nothing that could have been paid moved straight on.
    IdleAdvanced { from: u64, to: u64, stores: u64 },
    /// The node's whole pool, as the next block would be mined from it:
    /// asked for only while someone watches the engine page.
    NodePool {
        /// Transactions in the pool.
        txs: u64,
        /// Their size in bytes, when the node said.
        bytes: Option<u64>,
        /// The block weight a miner can fill without its reward being cut
        /// (the penalty-free zone: the median of recent blocks, 300,000 at
        /// least).
        penalty_free: u64,
    },
    /// Round time spent outside a tier's units, counted to the tier it was
    /// for, so a round's parts add up to the round.
    Work {
        tier: Tier,
        start_ms: u64,
        ms: u64,
        what: Work,
    },
    /// A mempool path looked at the pool and scanned `scanned` transactions.
    PoolScanned {
        path: PoolPath,
        pool: u64,
        scanned: u64,
    },
    /// A pool transaction pays an order: recorded at once.
    TxMatched {
        path: PoolPath,
        /// The transaction id's first 8 characters.
        txid: String,
    },
    /// Orders' statuses were recomputed; those that changed are listed.
    Recomputed {
        orders: u64,
        transitions: Vec<Transition>,
    },
    /// Unconfirmed payments were checked for having left the pool.
    Vanished { looked: u64 },
    /// The upkeep tier's first unit of a round ran.
    Upkeep { pruned: u64 },
}

impl Event {
    /// The tier whose work this event is, if any.
    pub fn tier(&self) -> Option<Tier> {
        match self {
            Event::Unit { tier, .. } | Event::TierEnded { tier, .. } | Event::Work { tier, .. } => {
                Some(*tier)
            }
            Event::ChainChecked { .. }
            | Event::ReorgFound { .. }
            | Event::ReorgCollected
            | Event::ReorgProcessed { .. }
            | Event::ReorgRewound { .. } => Some(Tier::Chain),
            Event::Seeded { .. }
            | Event::Fetched { .. }
            | Event::BlockScanStarted { .. }
            | Event::BlockProgress { .. }
            | Event::Checkpointed { .. }
            | Event::Committed { .. }
            | Event::Diverged { .. }
            | Event::IdleAdvanced { .. } => Some(Tier::Blocks),
            Event::NodePool { .. } | Event::PoolScanned { .. } | Event::TxMatched { .. } => {
                Some(Tier::Mempool)
            }
            Event::Recomputed { .. } | Event::Vanished { .. } => Some(Tier::Settlement),
            Event::Upkeep { .. } => Some(Tier::Upkeep),
            Event::Snapshot(_)
            | Event::RoundStarted { .. }
            | Event::RoundFinished { .. }
            | Event::Slept { .. } => None,
        }
    }
}

/// The first 8 characters of a block hash or transaction id: enough to
/// tell them apart on the page.
pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// What a [`Event::Work`] span was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Work {
    /// Whether the pool needs looking at this round (a database read):
    /// the mempool tier's.
    PoolCheck,
    /// The round's request for the node's tip (and its pool, when the
    /// mempool tier will look at it, in the same request): the chain
    /// tier's, which reads the chain as the node has it.
    TipRequest,
    /// Keeping the fetched blocks for the next round: the blocks tier's.
    CacheCarry,
}

/// One recorded event: its place in the engine's record and when it
/// happened (Unix milliseconds, the engine's clock).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recorded {
    pub seq: u64,
    pub at_ms: i64,
    #[serde(flatten)]
    pub event: Event,
}

/// The state the engine page draws, at one moment.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The last round started.
    pub round: u64,
    /// The node's tip as the last round read it.
    pub tip: Option<u64>,
    /// The highest block recorded for the network.
    pub high_water: Option<u64>,
    /// Groups of stores by cursor, highest first (the frontier, then the
    /// catch-up groups nearest to it), at most [`Snapshot::GROUPS`].
    pub groups: Vec<StoreGroup>,
    /// Catch-up groups beyond those listed.
    pub more_groups: u64,
    /// Blocks held in the scan's cache for the next round.
    pub cached: Vec<u64>,
    pub cache_bytes: u64,
    pub cache_budget_bytes: u64,
    /// Blocks with a scan saved partway.
    pub checkpoints: Vec<u64>,
    pub reorg: Option<ReorgJob>,
    pub pool: Pool,
    /// Orders waiting to be recomputed because a payment changed.
    pub recomputes_pending: u64,
    /// Orders due a recompute by time or height.
    pub orders_due: u64,
    pub webhooks: Webhooks,
    pub database: Database,
    pub nodes: Vec<Node>,
}

impl Snapshot {
    /// Groups listed at most.
    pub const GROUPS: usize = 32;
    /// Pool transactions listed at most.
    pub const POOL_TXS: usize = 14;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreGroup {
    pub cursor: u64,
    pub stores: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReorgPhase {
    Collect,
    Process,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReorgJob {
    pub fork: u64,
    pub phase: ReorgPhase,
    /// Payments still to re-examine.
    pub candidates: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pool {
    /// Whether the scanner looks at the pool: only while an order could be
    /// paid from it, or a payment waits for a block.
    pub watched: bool,
    /// Transactions the scanner remembers from the pool.
    pub size: u64,
    /// The first 8 characters of up to [`Snapshot::POOL_TXS`] of them.
    pub txids: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Webhooks {
    /// Deliveries due and not yet sent.
    pub due: u64,
    /// Deliveries made in each 10 s of the last five minutes, oldest first.
    pub sent: Vec<u32>,
}

impl Webhooks {
    pub const BUCKET_SECS: i64 = 10;
    pub const BUCKETS: usize = 30;
}

/// The engine's database worker (docs/scanner_microtasks.md, "Database
/// access").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Database {
    /// Jobs queued for the Scanner, Webhook and Admin classes.
    pub queued: [u64; 3],
    /// Jobs each class may queue before callers wait.
    pub capacity: u64,
    pub completed: u64,
    pub max_queue_wait_us: u64,
    pub max_run_us: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub label: String,
    /// The node the loops use now.
    pub active: bool,
    /// Skipped for now after failing.
    pub cooling_down: bool,
}

/// How the scanner's rounds are sized: what the page needs to draw a
/// round to scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tuning {
    pub round_ms: u64,
    /// Each tier's share of a round in percent, in [`Tier::ALL`] order.
    pub shares: [u32; 5],
    pub group_page: u64,
    pub blocks_per_unit: u64,
    pub reorg_check_depth: u64,
    pub poll_ms: u64,
}

/// `GET /api/v1/admin/engine/activity`: one network's recorded events.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActivityPage {
    pub network: String,
    /// Changes when the engine restarts: sequence numbers start over.
    pub epoch: String,
    pub now_ms: i64,
    pub tuning: Tuning,
    /// Oldest first. Without `from` (or when `from` has left the record),
    /// from the oldest snapshot kept.
    pub events: Vec<Recorded>,
    /// The `from` to ask with next time.
    pub next: u64,
    /// `from` had already left the record (or belongs to another epoch):
    /// the page starts over.
    pub gap: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An event's JSON carries its kind beside its fields, and every shape
    /// comes back as it went in: monokulo reads what the engine writes.
    #[test]
    fn recorded_events_round_trip_through_json() {
        let events = vec![
            Event::Snapshot(Box::new(Snapshot {
                round: 7,
                tip: Some(100),
                groups: vec![StoreGroup {
                    cursor: 100,
                    stores: 3,
                }],
                reorg: Some(ReorgJob {
                    fork: 98,
                    phase: ReorgPhase::Process,
                    candidates: 2,
                }),
                ..Snapshot::default()
            })),
            Event::Unit {
                tier: Tier::Blocks,
                pass: 2,
                start_ms: 40,
                ms: 3,
                progress: UnitProgress::Blocked(Wait::NodeFailed),
            },
            Event::TierEnded {
                tier: Tier::Chain,
                outcome: TierOutcome::Blocked(Wait::ChainDiverged),
            },
            Event::Recomputed {
                orders: 2,
                transitions: vec![Transition {
                    from: OrderStatus::Confirming,
                    to: OrderStatus::Paid,
                }],
            },
            Event::ReorgCollected,
        ];
        for (seq, event) in events.into_iter().enumerate() {
            let recorded = Recorded {
                seq: seq as u64,
                at_ms: 1_000,
                event,
            };
            let json = serde_json::to_value(&recorded).unwrap();
            assert!(json.get("kind").is_some(), "{json}");
            assert_eq!(json["seq"], seq as u64);
            let back: Recorded = serde_json::from_value(json).unwrap();
            assert_eq!(back, recorded);
        }
    }

    #[test]
    fn every_event_but_the_round_s_own_belongs_to_a_tier() {
        assert_eq!(Event::Upkeep { pruned: 1 }.tier(), Some(Tier::Upkeep));
        assert_eq!(Event::Diverged { height: 1 }.tier(), Some(Tier::Blocks));
        assert_eq!(
            Event::RoundStarted {
                round: 1,
                budget_ms: 1,
                tip: None,
            }
            .tier(),
            None
        );
        for (i, tier) in Tier::ALL.iter().enumerate() {
            assert_eq!(tier.index(), i);
        }
    }
}
