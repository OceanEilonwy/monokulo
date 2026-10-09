//! The blocks tier: scanning blocks for tenants (`docs/scanner_microtasks.md`).
//!
//! Tenants are grouped by their cursor (the highest block scanned for them).
//! A unit takes one group and scans the next few blocks for it: one fetch
//! for a run of blocks (each with its own id and parent id), then one
//! view-key scan per tenant per transaction. The *frontier* group (at the
//! network's high-water mark) scans new blocks; the others catch up, served
//! round-robin ([`Rotation`]). While the frontier is behind, turns alternate
//! between it and catch-up, so neither starves the other.
//!
//! A new block nobody can be scanned for (no store has an order in scope, or
//! none of those has its keys registered) is recorded from its header alone:
//! its id, its parent's and its time, about a kilobyte, with no transactions
//! fetched. The stores that were left behind catch up on whole blocks later.
//!
//! A block's results stay in memory ([`BlockScan`]) until the whole block
//! is scanned, then commit in one transaction with the cursor moves, and
//! only if the block still extends the recorded chain. If a unit runs out of
//! time partway through a block, its progress is written down (a checkpoint
//! with staged matches), to resume from.
//!
//! While blocks may be large, each block's header comes first (a run of
//! them at a time): a block too large for one answer, or for the node's
//! link to send in time, is scanned a page of transactions at a time from
//! its outline (its transactions' ids), a page per step, across as many
//! units and rounds as it takes (`docs/engine_scaling.md` section 4). Blocks
//! around it are fetched whole. Headers are read first only for an hour
//! after a sign that blocks may be large: a block request that ran out of
//! time or came back too large, or a block within a quarter of the size
//! that is paged. Otherwise blocks are fetched whole without asking.

use crate::store::TenantId;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::time::Instant;

use crate::daemon::{BlockOutline, ChainBlock, ChainHeader, ScanTx};
use crate::key_custody::{ScanIndices, ScanInput, WalletHandle};
use crate::scanner::{
    record_scan_match, scan_txs_for_tenants, stage_block_match, ScanResult, ScannerError,
};
use crate::store::position::CatchUpGroup;
use crate::store::{BlockCheckpoint, Store};

use shared::activity::Event;

use super::{bounded, count, Progress, Round, Wait};

/// How far ahead of real time consensus lets a block's timestamp run.
/// Catch-up windows start this much before a block's own timestamp, so a
/// forward-dated block can't hide an order that was open when it was mined.
const BLOCK_TIMESTAMP_DRIFT_SECONDS: i64 = 2 * 60 * 60;

/// Proof that a block was scanned in full for a tenant, with the results.
///
/// Moving a tenant's cursor takes one (`Store::advance_scanned_cursor`), and
/// only this module can build one: from a [`BlockScan`] that got through
/// every transaction of the block for that tenant.
pub struct ScannedBlock {
    tenant_id: TenantId,
    height: u64,
    scans: Vec<ScanResult>,
}

impl ScannedBlock {
    /// A proof without a scan, for the store's own tests.
    #[cfg(test)]
    pub(crate) fn for_test(tenant_id: &TenantId, height: u64) -> Self {
        Self {
            tenant_id: TenantId::new(tenant_id.to_string()),
            height,
            scans: Vec::new(),
        }
    }

    pub(crate) fn tenant_id(&self) -> &TenantId {
        &self.tenant_id
    }

    pub(crate) fn height(&self) -> u64 {
        self.height
    }
}

/// Kept across rounds: whose turn it is while the frontier is behind, and
/// how big block requests are. Across rounds, not per round, so a round
/// with time for one unit alternates too, and a request halved after a
/// failure stays halved in the next round.
pub(crate) struct BlockState {
    catch_up_turn: AtomicBool,
    progress: crate::scaling::SharedProgress,
    /// The large block being scanned in pages, kept across rounds so its
    /// outline is fetched once.
    paged: parking_lot::Mutex<Option<Paged>>,
    /// The block cache the last round left for the next ([`carry`]): taken
    /// whole at a round's start and put back at its end, never shared while
    /// a round runs.
    carried: parking_lot::Mutex<Option<Carried>>,
}

/// A large block being scanned a page at a time (`docs/engine_scaling.md`
/// section 4).
#[derive(Clone)]
struct Paged {
    outline: Arc<BlockOutline>,
    /// Its weight, from its header.
    weight: u64,
    /// Bytes a transaction, from the header; doubled after a page that ran
    /// out of time or came back too large, so the next is half as long.
    avg_tx_bytes: f64,
}

impl Default for BlockState {
    fn default() -> Self {
        Self::with_progress(crate::scaling::new_progress())
    }
}

impl BlockState {
    /// The blocks the last round left for the next, lowest first, and
    /// their bytes on the wire.
    pub(super) fn carried_cache(&self) -> (Vec<u64>, u64) {
        self.carried.lock().as_ref().map_or_default(|carried| {
            (
                carried.cache.blocks.keys().copied().collect(),
                u64::try_from(carried.cache.bytes).unwrap_or(u64::MAX),
            )
        })
    }

    /// State whose progress (sizing, the block in progress, recent blocks)
    /// is `progress`, which `/status` reads.
    pub(crate) fn with_progress(progress: crate::scaling::SharedProgress) -> Self {
        Self {
            catch_up_turn: AtomicBool::new(false),
            progress,
            paged: parking_lot::Mutex::new(None),
            carried: parking_lot::Mutex::new(None),
        }
    }

    /// The time a round needs (`docs/engine_scaling.md` section 4): the base,
    /// unless the smallest unit of a large block in progress (one page of
    /// one transaction, fetched and scanned for `stores` stores) is
    /// expected to need more than the round's share for blocks. Then half
    /// as much again as that unit, within two minutes.
    pub(crate) fn round_budget(
        &self,
        daemon: &dyn crate::daemon::MoneroDaemonClient,
        stores: usize,
        tuning: &super::ScanTuning,
    ) -> std::time::Duration {
        let base = tuning.round_budget;
        let Some(avg_tx_bytes) = self.paged.lock().as_ref().map(|paged| paged.avg_tx_bytes) else {
            self.progress.lock().round_budget = base;
            return base;
        };
        let fetch = daemon
            .link_cost()
            .map_or(0.0, |link| link.secs(1, avg_tx_bytes));
        let scan = self.progress.lock().secs_per_tx_scan().unwrap_or(0.0) * stores.max(1) as f64;
        let budget = tuning.round_budget_for(fetch + scan);
        self.progress.lock().round_budget = budget;
        budget
    }

    /// How many blocks to ask for from `from`, up to `end`.
    fn plan(&self, round: &Round<'_>, from: u64, end: u64) -> crate::scanner::ChunkPlan {
        let mut progress = self.progress.lock();
        let tuning = round.state.tuning();
        let plan = crate::scanner::next_scan_chunk(
            tuning,
            tuning.group_response_cap_bytes(
                round.inputs.scan_chunk_memory_budget_mb,
                round.blocks.groups.unwrap_or(1),
            ),
            round.inputs.daemon.link_cost(),
            progress.avg_bytes_per_block,
            end.saturating_sub(from) + 1,
        );
        progress.last_chunk = Some(plan);
        plan
    }

    /// A fetched run of `blocks` blocks totalling `bytes`, in `secs`.
    fn note_fetched(&self, tuning: &super::ScanTuning, bytes: usize, blocks: usize) {
        if blocks == 0 {
            return;
        }
        let mut progress = self.progress.lock();
        progress.avg_bytes_per_block = crate::scanner::update_avg_bytes_per_block(
            tuning,
            progress.avg_bytes_per_block,
            bytes,
            blocks,
        );
    }

    /// A block request that failed: one that ran out of time or came back
    /// too large halves the next.
    fn note_failed(&self, error: &ScannerError) {
        if matches!(error, ScannerError::Daemon(e) if e.asks_for_less()) {
            let mut progress = self.progress.lock();
            progress.avg_bytes_per_block =
                crate::scanner::avg_after_failed_fetch(progress.avg_bytes_per_block);
            // The block may be too large to fetch whole at all: read
            // headers first, so the next try can page it.
            progress.want_headers_first(
                crate::now_unix(),
                crate::scaling::HeadersFirstReason::FailedRequest,
            );
        }
    }

    /// Fetched blocks of `sizes` bytes: one within a quarter of the size
    /// that is paged (under a `response_cap` and over `link`) means blocks
    /// may grow past it, so headers are read first for a while.
    fn note_sizes(
        &self,
        tuning: &super::ScanTuning,
        sizes: &[usize],
        response_cap: u64,
        link: Option<crate::link::LinkCost>,
    ) {
        let near = sizes.iter().any(|size| {
            crate::scanner::scan_in_pages(
                tuning,
                Some((*size as u64).saturating_mul(4)),
                response_cap,
                link,
            )
        });
        if near {
            self.progress.lock().want_headers_first(
                crate::now_unix(),
                crate::scaling::HeadersFirstReason::LargeBlock,
            );
        }
    }

    /// Whether blocks' headers are read before the blocks: for a while
    /// after a sign that blocks may be large, and throughout a block
    /// scanned in pages.
    fn headers_first(&self) -> bool {
        self.paged.lock().is_some() || self.progress.lock().headers_first_on(crate::now_unix())
    }

    /// The large block's current bytes-a-transaction estimate, if one is
    /// being scanned in pages.
    fn avg_tx_bytes(&self) -> Option<f64> {
        self.paged.lock().as_ref().map(|paged| paged.avg_tx_bytes)
    }

    /// A page request that failed: one that ran out of time or came back
    /// too large halves the next.
    fn note_page_failed(&self, error: &ScannerError) {
        if matches!(error, ScannerError::Daemon(e) if e.asks_for_less()) {
            if let Some(paged) = self.paged.lock().as_mut() {
                paged.avg_tx_bytes = crate::scanner::avg_after_failed_fetch(paged.avg_tx_bytes);
            }
        }
    }

    /// Time spent fetching blocks.
    fn note_fetch_time(&self, secs: f64) {
        self.progress.lock().spent(crate::now_unix(), secs, 0.0, 0);
    }
}

/// Fetches `count` blocks from `from`, given the time the node's link needs
/// for that many (`docs/engine_scaling.md` section 2).
/// Records a run of blocks fetched from `from` for the engine page.
fn record_fetched(state: &super::ScanState, from: u64, chunk: &[ChainBlock], ahead: bool) {
    state.activity().record(Event::Fetched {
        from,
        count: count(chunk.len()),
        bytes: chunk
            .iter()
            .map(|block| block.wire_bytes)
            .fold(0, u64::saturating_add),
        ahead,
    });
}

async fn fetch_chunk(
    daemon: &dyn crate::daemon::MoneroDaemonClient,
    from: u64,
    count: u64,
) -> Result<Vec<ChainBlock>, ScannerError> {
    let deadline = (daemon.chain_blocks_timeout(count) + crate::daemon_fallback::DEADLINE_MARGIN)
        .max(super::CALL_DEADLINE);
    super::bounded_by(deadline, daemon.get_chain_blocks(from, count)).await
}

#[derive(Default)]
pub(crate) struct BlocksRound {
    repaired: bool,
    frontier_done: bool,
    rotation: Rotation,
    cache: BlockCache,
    /// Headers fetched this round, for new blocks nobody is scanned for.
    /// Never a source of transactions: kept apart from `cache`.
    headers: BTreeMap<u64, ChainHeader>,
    /// Whether the last new block was recorded from its header alone
    /// (nobody to scan it for). Not yet known at the start of a round.
    frontier_header_only: Option<bool>,
    /// How many groups share the block cache this round (the catch-up
    /// groups and the frontier), counted at the tier's first unit.
    groups: Option<u64>,
}

impl BlocksRound {
    /// A round's block state, starting from the cache the last round left,
    /// if its blocks came from the node this round reads, trimmed to the
    /// memory budget as it is now (the setting may have been lowered).
    pub(crate) fn resume(state: &BlockState, inputs: &super::RoundInputs<'_>) -> Self {
        let mut round = Self::default();
        let Some(mut carried) = state.carried.lock().take() else {
            return round;
        };
        let discarded = if inputs.daemon.node() == Some(carried.node) {
            let budget = budget_bytes(inputs.scan_chunk_memory_budget_mb);
            let discarded = carried.cache.trim(None, budget);
            round.cache = carried.cache;
            discarded
        } else {
            carried.cache.clear()
        };
        let mut progress = state.progress.lock();
        progress.discarded(discarded, crate::now_unix());
        progress.cache_bytes(round.cache.bytes as u64, crate::now_unix());
        round
    }
}

/// Leaves the next round what may serve it: blocks above every tenant's
/// cursor (below it, nobody needs them again) and at least
/// `reorg_check_depth` below the tip, where reorg detection treats the
/// chain as settled; a block nearer the tip may be replaced before the next
/// round. Nothing is left after a rewind, while a reorg job is open, when
/// the tip couldn't be read, or from a client that may ask a different node
/// each call. A caught-up network leaves nothing: every held block is at or
/// below a cursor, or near the tip.
pub(super) async fn carry(round: &mut Round<'_>) {
    let mut cache = std::mem::take(&mut round.blocks.cache);
    if cache.blocks.is_empty() {
        return;
    }
    let node = round.inputs.daemon.node();
    let lowest_cursor = match (node, round.tip, round.chain.rewound()) {
        (Some(_), Some(_), false) => round
            .db(|s, network| -> Result<Option<u64>, ScannerError> {
                if s.reorg_job(network)?.is_some() {
                    return Ok(None);
                }
                let Some(high_water) = s.max_scanned_height(network)? else {
                    return Ok(None);
                };
                let lowest_group = s.scan_group_cursors(network, high_water, None, 1)?;
                Ok(Some(lowest_group.first().copied().unwrap_or(high_water)))
            })
            .await
            .ok()
            .flatten(),
        _ => None,
    };
    let settled = round
        .tip
        .map(|tip| tip.saturating_sub(round.inputs.reorg_check_depth));
    let discarded = match (lowest_cursor, settled) {
        (Some(lowest), Some(settled)) => cache.retain(lowest.saturating_add(1)..=settled),
        _ => cache.clear(),
    };
    let mut progress = round.state.blocks.progress.lock();
    progress.discarded(discarded, crate::now_unix());
    progress.cache_bytes(cache.bytes as u64, crate::now_unix());
    drop(progress);
    if let (Some(node), false) = (node, cache.blocks.is_empty()) {
        *round.state.blocks.carried.lock() = Some(Carried { cache, node });
    }
}

/// Which catch-up group is served next. Groups are keyed by cursor height;
/// after a group is served, the rotation moves past where that group *ended
/// up*, so a group far behind can't be "next" again just because it moved
/// up by a few blocks, and every group gets a turn in height order.
#[derive(Default)]
struct Rotation {
    /// Cursors served this round: meeting one again means the rotation has
    /// come full circle.
    visited: HashSet<u64>,
    /// Where the rotation stands this round (else the persisted position).
    last: Option<u64>,
    /// Next existing group before this unit splits or merges tenant pages.
    next_existing: Option<u64>,
}

impl Rotation {
    /// The next group after the rotation's position, wrapping once.
    async fn next(
        &mut self,
        round: &Round<'_>,
        high_water: u64,
    ) -> Result<Option<u64>, ScannerError> {
        let (last, visited) = (self.last, self.visited.clone());
        let (group, next_existing) = round
            .db(move |s, network| -> Result<_, ScannerError> {
                let after = match last {
                    Some(last) => Some(last),
                    None => s.scheduler_position::<CatchUpGroup>(network)?,
                };
                let mut next = s.scan_group_cursors(network, high_water, after, 1)?;
                if next.is_empty() && after.is_some() {
                    next = s.scan_group_cursors(network, high_water, None, 1)?;
                }
                let group = next
                    .first()
                    .copied()
                    .filter(|group| !visited.contains(group));
                let next_existing = match group {
                    Some(group) => s
                        .scan_group_cursors(network, high_water, Some(group), 1)?
                        .first()
                        .copied(),
                    None => None,
                };
                Ok((group, next_existing))
            })
            .await?;
        self.next_existing = next_existing;
        if let Some(group) = group {
            self.visited.insert(group);
        }
        Ok(group)
    }

    /// Records that the group at `group` was served and ended at `reached`.
    async fn served(
        &mut self,
        round: &Round<'_>,
        group: u64,
        reached: u64,
    ) -> Result<(), ScannerError> {
        // A bounded tenant page may move only part of a group. Never skip
        // another group that existed before this unit, even if this page
        // reaches or passes its cursor.
        let position = self.next_existing.map_or_else(
            || reached.max(group),
            |next| reached.max(group).min(next.saturating_sub(1)),
        );
        self.last = Some(position);
        round
            .db(move |s, network| s.set_scheduler_position::<CatchUpGroup>(network, &position))
            .await
    }
}

/// Blocks fetched for scanning, within the scan memory budget. A round
/// starts with what the last one left ([`BlockState::resume`]) and leaves
/// what may serve the next ([`BlocksRound::carry`]), so a run fetched ahead
/// isn't fetched again because a round ended. Each block carries its own id
/// and is checked against the recorded chain before it is scanned, so a
/// cached body can never be paired with another block's hash.
#[derive(Default)]
struct BlockCache {
    blocks: BTreeMap<u64, Cached>,
    bytes: usize,
}

struct Cached {
    block: Arc<ChainBlock>,
    /// Its size on the wire: what the memory budget counts.
    bytes: usize,
    /// A group's scan of it committed, for the whole group. A block let go
    /// of before that (not just handed to a scan that was interrupted) was
    /// fetched for nothing.
    scanned: bool,
}

impl BlockCache {
    fn insert(&mut self, block: ChainBlock, bytes: usize) -> Arc<ChainBlock> {
        let block = Arc::new(block);
        let cached = Cached {
            block: Arc::clone(&block),
            bytes,
            scanned: false,
        };
        if let Some(old) = self.blocks.insert(block.height, cached) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        block
    }

    fn contains(&self, height: u64) -> bool {
        self.blocks.contains_key(&height)
    }

    /// Block `height`, if held.
    fn get(&self, height: u64) -> Option<Arc<ChainBlock>> {
        self.blocks
            .get(&height)
            .map(|cached| Arc::clone(&cached.block))
    }

    /// A group's scan of block `height` committed, for the whole group.
    fn mark_scanned(&mut self, height: u64) {
        if let Some(cached) = self.blocks.get_mut(&height) {
            cached.scanned = true;
        }
    }

    /// Lets go of block `height`; returns its bytes if no group's scan of it
    /// committed.
    fn remove(&mut self, height: u64) -> u64 {
        match self.blocks.remove(&height) {
            Some(cached) => {
                self.bytes -= cached.bytes;
                if cached.scanned {
                    0
                } else {
                    cached.bytes as u64
                }
            }
            None => 0,
        }
    }

    /// Evicts blocks until within `budget`, `keep` never: first those a
    /// group has scanned (and moved past), then the rest; of each, the
    /// farthest from `keep` (the lowest held, without one) first. Returns
    /// the bytes let go of unscanned.
    fn trim(&mut self, keep: Option<u64>, budget: usize) -> u64 {
        let Some(anchor) = keep.or_else(|| self.blocks.keys().next().copied()) else {
            return 0;
        };
        let mut victims: Vec<(bool, u64)> = self
            .blocks
            .iter()
            .filter(|(height, _)| Some(**height) != keep)
            .map(|(height, cached)| (cached.scanned, *height))
            .collect();
        victims.sort_by_key(|&(scanned, height)| {
            (!scanned, std::cmp::Reverse(height.abs_diff(anchor)))
        });
        let mut discarded = 0;
        for (_, height) in victims {
            if self.bytes <= budget {
                break;
            }
            discarded += self.remove(height);
        }
        discarded
    }

    /// Lets go of every block; returns the bytes let go of unscanned.
    #[expect(
        clippy::needless_collect,
        reason = "collecting ends the borrow of `self.blocks` that `remove` needs"
    )]
    fn clear(&mut self) -> u64 {
        let heights: Vec<u64> = self.blocks.keys().copied().collect();
        heights.into_iter().map(|height| self.remove(height)).sum()
    }

    /// Keeps only blocks in `heights`; returns the bytes let go of unscanned.
    #[expect(
        clippy::needless_collect,
        reason = "collecting ends the borrow of `self.blocks` that `remove` needs"
    )]
    fn retain(&mut self, heights: std::ops::RangeInclusive<u64>) -> u64 {
        let outside: Vec<u64> = self
            .blocks
            .keys()
            .copied()
            .filter(|height| !heights.contains(height))
            .collect();
        outside.into_iter().map(|height| self.remove(height)).sum()
    }
}

/// A scan memory budget of `budget_mb` in bytes.
fn budget_bytes(budget_mb: u32) -> usize {
    usize::try_from(budget_mb)
        .unwrap_or(usize::MAX)
        .saturating_mul(1024 * 1024)
}

/// A block cache left by one round for the next, with the node its blocks
/// came from.
struct Carried {
    cache: BlockCache,
    node: crate::daemon::NodeKey,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Group {
    Frontier,
    CatchUp,
}

impl From<Group> for shared::activity::Group {
    fn from(group: Group) -> Self {
        match group {
            Group::Frontier => Self::Frontier,
            Group::CatchUp => Self::CatchUp,
        }
    }
}

/// Records stores moved straight from `from` to `to` for the engine page.
fn record_idle(round: &Round<'_>, from: u64, to: u64, moved: usize) {
    if moved > 0 {
        round.state.activity().record(Event::IdleAdvanced {
            from,
            to,
            stores: count(moved),
        });
    }
}

/// Records how far a block's scan got before it stopped, for the engine
/// page.
fn record_checkpoint(round: &Round<'_>, height: u64, scan: &BlockScan, tx_count: usize) {
    round.state.activity().record(Event::Checkpointed {
        height,
        stores: count(scan.next_tx.len().saturating_sub(scan.failed.len())),
        done_txs: count(scan.first_due(tx_count)),
        total_txs: count(tx_count),
    });
}

/// Which page of a group's tenants a committed block was scanned for.
enum Page {
    /// A full page: more of the group may still be at the parent cursor.
    Full(Vec<TenantId>),
    /// The last of the group at the parent cursor.
    Last,
}

enum BlockOutcome {
    Committed(Page),
    /// Out of time partway through; progress is checkpointed.
    Interrupted,
    /// Nobody in the group could be scanned (keys not registered, retry
    /// delays): nothing was fetched or recorded.
    NobodyToScan,
    /// The block doesn't extend the recorded chain: a reorg the chain tier
    /// will handle.
    Diverged(&'static str),
}

pub(super) async fn step(round: &mut Round<'_>, until: Instant) -> Progress {
    match run(round, until).await {
        Ok(progress) => progress,
        // A node that fails or doesn't answer stops block scanning where it
        // is, with nothing recorded past it; the next round retries from
        // there. Only this engine's own failures (storage) fail the round.
        Err(ScannerError::Daemon(error)) => {
            shared::throttled!(
                format!("blocks-node:{}", crate::network::network_str(round.network())),
                warn,
                network = crate::network::network_str(round.network()),
                error = %error,
                "block scanning stopped: the node failed. If this repeats, no payment on this network is being detected"
            );
            Progress::Blocked(Wait::NodeFailed)
        }
        Err(error) => Progress::Failed(error),
    }
}

async fn run(round: &mut Round<'_>, until: Instant) -> Result<Progress, ScannerError> {
    let Some(tip) = round.tip else {
        return Ok(Progress::Blocked(Wait::ChainHeightUnknown));
    };
    if round.chain.rewound() {
        return Ok(Progress::Blocked(Wait::RewoundThisRound));
    }
    let repair = !round.blocks.repaired;
    let count_groups = round.blocks.groups.is_none();
    let (reorg_open, high_water, groups) = round
        .db(move |s, network| -> Result<_, ScannerError> {
            let reorg_open = s.reorg_job(network)?.is_some();
            let high_water = s.max_scanned_height(network)?;
            // Once a round: the catch-up groups and the frontier, which
            // share the block cache.
            let groups = match (count_groups, high_water) {
                (true, Some(high_water)) => Some(s.count_scan_groups(network, high_water)? + 1),
                _ => None,
            };
            if let (false, true, Some(high_water)) = (reorg_open, repair, high_water) {
                // Cheap repairs whatever happened before: no cursor ahead of
                // the network, new tenants anchored at it, disabled ones
                // moved along.
                s.clamp_cursors(network, Some(high_water))?;
                s.anchor_unset_cursors(network, high_water)?;
                s.snap_disabled_cursors(network, high_water)?;
            }
            Ok((reorg_open, high_water, groups))
        })
        .await?;
    if groups.is_some() {
        round.blocks.groups = groups;
    }
    if reorg_open {
        return Ok(Progress::Blocked(Wait::ReorgBeingReconciled));
    }
    let Some(high_water) = high_water else {
        return seed(round, tip).await;
    };
    round.blocks.repaired = true;

    let frontier_behind = !round.blocks.frontier_done && high_water < tip;
    let turn = &round.state.blocks.catch_up_turn;
    if frontier_behind && !turn.load(Ordering::Relaxed) {
        turn.store(true, Ordering::Relaxed);
        let reached = advance_group(round, Group::Frontier, high_water, tip, until).await?;
        return Ok(frontier_progress(round, reached));
    }
    turn.store(false, Ordering::Relaxed);
    let mut rotation = std::mem::take(&mut round.blocks.rotation);
    let served = serve_catch_up(round, &mut rotation, high_water, tip, until).await;
    round.blocks.rotation = rotation;
    if served? {
        return Ok(Progress::Advanced);
    }
    if frontier_behind {
        let reached = advance_group(round, Group::Frontier, high_water, tip, until).await?;
        return Ok(frontier_progress(round, reached));
    }
    round.blocks.frontier_done = true;
    Ok(Progress::Idle)
}

/// What a frontier unit amounts to. A frontier that diverged from the
/// recorded chain can't move until the chain tier reconciles the fork, so
/// it stops for the round instead of asking again and again.
fn frontier_progress(round: &mut Round<'_>, reached: Reached) -> Progress {
    if reached.diverged {
        round.blocks.frontier_done = true;
        Progress::Blocked(Wait::ChainDiverged)
    } else {
        Progress::Advanced
    }
}

/// Serves the next catch-up group, if the rotation has one this round.
async fn serve_catch_up(
    round: &mut Round<'_>,
    rotation: &mut Rotation,
    high_water: u64,
    tip: u64,
    until: Instant,
) -> Result<bool, ScannerError> {
    let Some(group) = rotation.next(round, high_water).await? else {
        return Ok(false);
    };
    // Tenants with nothing that could ever have been paid need no block read
    // to decide: straight to the high-water mark.
    let idle = round
        .db(move |s, network| s.advance_idle_cursors(network, group, high_water, i64::MIN / 2, 0))
        .await?;
    record_idle(round, group, high_water, idle);
    // A group that diverged stays where it is; the rotation still moves on
    // past it, so the round's other groups are served.
    let reached = advance_group(round, Group::CatchUp, group, tip, until).await?;
    rotation.served(round, group, reached.cursor).await?;
    Ok(true)
}

/// First run on a network: start just below the node's tip rather than
/// replaying history (a payment gateway watches for new payments). One block
/// of margin, for a node reporting a tip it can't serve yet.
async fn seed(round: &Round<'_>, tip: u64) -> Result<Progress, ScannerError> {
    let seed = tip.saturating_sub(1);
    match bounded(round.inputs.daemon.get_block_hash(seed)).await {
        Ok(hash) => {
            round
                .db(move |s, network| {
                    s.in_transaction(|s| -> Result<(), ScannerError> {
                        s.set_scanned_block(network, seed, &hash)?;
                        s.anchor_unset_cursors(network, seed)?;
                        Ok(())
                    })
                })
                .await?;
            tracing::info!(
                network = crate::network::network_str(round.network()),
                height = seed,
                "started scanning this network"
            );
            round
                .state
                .activity()
                .record(Event::Seeded { height: seed });
            Ok(Progress::Advanced)
        }
        Err(_) => Ok(Progress::Blocked(Wait::NodeCannotServeTip)),
    }
}

/// Makes up to the tuning's `blocks_per_unit` tenant-page block scans for the group at
/// `cursor`, one after another, as far as its time allows (always at least
/// one step of progress). The frontier stops at the tip; catch-up stops at the
/// network's high-water mark, where it joins the frontier. Returns the
/// cursor the group reached, and whether it stopped at a block that doesn't
/// extend the recorded chain.
async fn advance_group(
    round: &mut Round<'_>,
    group: Group,
    cursor: u64,
    tip: u64,
    until: Instant,
) -> Result<Reached, ScannerError> {
    let mut cursor = cursor;
    let mut high_water = round.db(Store::max_scanned_height).await?.unwrap_or(cursor);
    // Tenants already given block `cursor + 1` this unit, a page at a
    // time, while the rest of the group at `cursor` waits for its page.
    let mut given: Vec<TenantId> = Vec::new();
    let mut scanned = 0;
    while scanned < round.state.tuning().blocks_per_unit {
        let end = match group {
            Group::Frontier => tip,
            Group::CatchUp => high_water,
        };
        // Each unit has a hard tenant-page bound. Remaining tenants retain
        // their durable cursor and join catch-up on a subsequent unit.
        if cursor >= end || (given.is_empty() && scanned > 0 && Instant::now() >= until) {
            break;
        }
        // The node's round trip for the next run of blocks overlaps this
        // block's scan, when this block is the last one held.
        // (This block is `cursor + 1`; the run after it starts at `cursor + 2`.)
        // This block is fetched first (usually already held) so the fetch
        // ahead can't duplicate it; not for a catch-up group's first block,
        // where the scan first checks there is anyone to scan it for, and
        // not for a new block unless the one before it was scanned for
        // somebody: a block nobody is scanned for needs only its header, and
        // the scan finds that out before it fetches anything.
        let fetch_first = match group {
            Group::Frontier => round.blocks.frontier_header_only == Some(false),
            Group::CatchUp => scanned > 0,
        };
        if fetch_first {
            hold(round, cursor + 1, end).await?;
        }
        let prefetch = prefetch_range(round, cursor + 2, end);
        let daemon = round.inputs.daemon;
        let state = round.state;
        let task = BlockTask {
            group,
            parent: cursor,
            high_water,
            end,
            until,
            on_timeout: if !given.is_empty() {
                OnTimeout::Finish
            } else if scanned == 0 {
                OnTimeout::StopAfterProgress
            } else {
                OnTimeout::Stop
            },
        };
        let (outcome, prefetched) = tokio::join!(scan_block(round, task, &given), async {
            match prefetch {
                Some((from, count)) => {
                    let started = Instant::now();
                    let fetched = fetch_chunk(daemon, from, count).await;
                    state
                        .blocks
                        .note_fetch_time(started.elapsed().as_secs_f64());
                    Some(fetched)
                }
                None => None,
            }
        });
        match prefetched {
            Some(Ok(chunk)) => {
                record_fetched(state, cursor + 2, &chunk, true);
                round.blocks.cache.add_chunk(
                    chunk,
                    cursor + 2,
                    round.inputs.scan_chunk_memory_budget_mb,
                    daemon.link_cost(),
                    &state.blocks,
                    state.tuning(),
                );
            }
            // A failed prefetch is asked for again when it's needed.
            Some(Err(e)) => state.blocks.note_failed(&e),
            None => {}
        }
        match outcome? {
            // A whole page: the rest of the group at this cursor gets the
            // same block (held already) before the group moves on, so a
            // group larger than a page moves together rather than its
            // first page running ahead of the rest.
            BlockOutcome::Committed(Page::Full(page)) => {
                given.extend(page);
                scanned += 1;
            }
            BlockOutcome::Committed(Page::Last) => {
                round.blocks.cache.mark_scanned(cursor + 1);
                given.clear();
                cursor += 1;
                high_water = high_water.max(cursor);
                scanned += 1;
            }
            BlockOutcome::Interrupted | BlockOutcome::NobodyToScan => break,
            BlockOutcome::Diverged(reason) => {
                round
                    .state
                    .activity()
                    .record(Event::Diverged { height: cursor + 1 });
                tracing::warn!(
                    network = crate::network::network_str(round.network()),
                    height = cursor + 1,
                    reason,
                    "block differs from the stored chain; waiting for reorg reconciliation"
                );
                return Ok(Reached {
                    cursor,
                    diverged: true,
                });
            }
        }
    }
    if group == Group::Frontier && cursor >= tip {
        round.blocks.frontier_done = true;
    }
    Ok(Reached {
        cursor,
        diverged: false,
    })
}

/// Where a group's unit left it.
#[derive(Clone, Copy)]
struct Reached {
    cursor: u64,
    diverged: bool,
}

/// What one database read tells a block scan before it starts.
struct Plan {
    /// The page of tenants at the parent cursor this scan is for.
    page: Page,
    /// Tenants at the parent cursor with something in scope, and their
    /// windows.
    members: Vec<(TenantId, Vec<u32>)>,
    /// The recorded hashes of this block and its parent, if any.
    recorded: Option<String>,
    parent: Option<String>,
    /// Checkpoints of those tenants.
    checkpoints: HashMap<TenantId, BlockCheckpoint>,
}

/// One block to scan for one group.
#[derive(Clone, Copy)]
struct BlockTask {
    group: Group,
    /// The group's cursor; the block is `parent + 1`.
    parent: u64,
    high_water: u64,
    /// Where the group's run ends (the tip, or the high-water mark).
    end: u64,
    until: Instant,
    on_timeout: OnTimeout,
}

/// What a block scan does once its unit's time is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OnTimeout {
    /// Stops where it is, with a checkpoint.
    Stop,
    /// Stops once it has scanned something: the unit's first block, so
    /// every unit makes progress however little time it has.
    StopAfterProgress,
    /// Doesn't stop: a later page of a block the unit started, so a group
    /// larger than a page isn't split. Bounded by the page and the unit's
    /// `blocks_per_unit` scans.
    Finish,
}

/// Scans block `task.parent + 1` for the tenants at cursor `task.parent`.
/// Scans block `task.parent + 1` for the next page of tenants at cursor
/// `task.parent`, leaving out those `given` it already this unit.
async fn scan_block(
    round: &mut Round<'_>,
    task: BlockTask,
    given: &[TenantId],
) -> Result<BlockOutcome, ScannerError> {
    let BlockTask {
        group,
        parent,
        high_water,
        end,
        until,
        on_timeout,
    } = task;
    let height = parent + 1;
    let grace = round.inputs.grace_period_seconds;
    let frontier = height > high_water;
    round
        .state
        .blocks
        .progress
        .lock()
        .start_block(height, crate::now_unix());
    let group_page = round.state.tuning().group_page;
    let mut waiting = round.state.backoff.waiting();
    waiting.extend_from_slice(given);
    let registered: Option<Vec<TenantId>> = (group == Group::CatchUp).then(|| {
        round
            .inputs
            .tenants
            .iter()
            .map(|(id, _)| id.clone())
            .collect()
    });
    // A catch-up group none of whose stores can be scanned (keys not
    // registered, waiting to retry) waits where it is, before anything is
    // fetched. The frontier still records the block for the network.
    if group == Group::CatchUp {
        let excluded = waiting.clone();
        let available = registered.clone().unwrap_or_default();
        let ids = round
            .db(move |s, network| {
                s.registered_tenants_at_cursor(network, parent, &excluded, &available, group_page)
            })
            .await?;
        if !ids.iter().any(|id| round.handles.contains_key(id.as_str())) {
            return Ok(BlockOutcome::NobodyToScan);
        }
    }
    // Which orders could have been paid in this block: as of now for a new
    // block; for an old one, as of the block's own time (less the drift
    // consensus allows), so an order that closed during the gap is still
    // looked for.
    let since = if frontier {
        round.now
    } else {
        let timestamp =
            if round.state.blocks.headers_first() || round.blocks.headers.contains_key(&height) {
                known_header(round, height, end).await?.timestamp
            } else {
                block(round, height, end).await?.timestamp
            };
        i64::try_from(timestamp)
            .unwrap_or(i64::MAX)
            .saturating_sub(BLOCK_TIMESTAMP_DRIFT_SECONDS)
            .min(round.now)
    };
    let plan = round
        .db(move |s, network| -> Result<_, ScannerError> {
            let ids = match registered {
                Some(available) => s.registered_tenants_at_cursor(
                    network, parent, &waiting, &available, group_page,
                )?,
                None => s.tenants_at_cursor(network, parent, &waiting, group_page)?,
            };
            // Persist attempts as well as successes: a custody failure must
            // yield to the next tenant even when the round or process ends.
            if let Some(last) = ids.last() {
                s.set_scheduler_position::<crate::store::position::BlockTenantPage>(
                    network,
                    &last.to_string(),
                )?;
            }
            let page = if ids.len() == group_page {
                Page::Full(ids.clone())
            } else {
                Page::Last
            };
            let mut windows = s.scan_windows(&ids, since, grace)?;
            let members: Vec<(TenantId, Vec<u32>)> = ids
                .into_iter()
                .filter_map(|id| windows.remove(&id).map(|w| (id, w)))
                .collect();
            let mut checkpoints = HashMap::new();
            for (tenant_id, _) in &members {
                if let Some(checkpoint) = s.block_checkpoint(network, tenant_id)? {
                    checkpoints.insert(tenant_id.clone(), checkpoint);
                }
            }
            Ok(Plan {
                page,
                members,
                recorded: s.get_scanned_block_hash(network, height)?,
                parent: s.get_scanned_block_hash(network, parent)?,
                checkpoints,
            })
        })
        .await?;
    let scannable: Vec<(TenantId, WalletHandle, ScanIndices)> = plan
        .members
        .into_iter()
        .filter_map(|(id, window)| {
            round
                .handles
                .get(id.as_str())
                .map(|handle| (id, *handle, ScanIndices::new(window)))
        })
        .collect();
    if scannable.is_empty() && group == Group::CatchUp {
        // Nobody here can be scanned now. Those with nothing that could
        // have been paid from this block on (every order closed before its
        // time) needn't wait: straight to the high-water mark. The others
        // wait for their keys.
        let idle = round
            .db(move |s, network| s.advance_idle_cursors(network, parent, high_water, since, grace))
            .await?;
        record_idle(round, parent, high_water, idle);
        return Ok(BlockOutcome::NobodyToScan);
    }

    // A new block with nobody to scan it for is recorded from its header:
    // its transactions would be fetched and then read by no one.
    let header_only = group == Group::Frontier && frontier && scannable.is_empty();
    if group == Group::Frontier {
        round.blocks.frontier_header_only = Some(header_only);
    }
    let source = if header_only {
        Source::Whole(header_block(round, height, end).await?)
    } else {
        source(round, height, end).await?
    };
    let (hash, prev_hash) = source.identity();
    if plan
        .recorded
        .as_ref()
        .is_some_and(|recorded| recorded != hash)
    {
        return Ok(BlockOutcome::Diverged(
            "the node's block differs from the one recorded",
        ));
    }
    if plan
        .parent
        .as_ref()
        .is_some_and(|parent_hash| parent_hash != prev_hash)
    {
        return Ok(BlockOutcome::Diverged(
            "the node's block doesn't extend the recorded chain",
        ));
    }
    let (hash, prev_hash) = (hash.to_owned(), prev_hash.to_owned());

    let tx_count = source.tx_count();
    round.state.activity().record(Event::BlockScanStarted {
        height,
        group: group.into(),
        stores: count(scannable.len()),
        txs: count(tx_count),
        header_only,
    });
    let mut scan = BlockScan::new(&scannable, &plan.checkpoints, &hash, tx_count);
    let mut progressed = on_timeout == OnTimeout::Stop;
    let at = ScanAt {
        height,
        until: (on_timeout != OnTimeout::Finish).then_some(until),
        scannable: &scannable,
    };
    let finished = match &source {
        Source::Whole(block) => {
            round
                .state
                .blocks
                .progress
                .lock()
                .fetched_block(height, block.wire_bytes);
            // Each transaction is recorded under the id it came with.
            if block.txids.len() != block.txs.len() {
                return Err(ScannerError::Internal(format!(
                    "block {height} has {} transactions and {} ids",
                    block.txs.len(),
                    block.txids.len()
                )));
            }
            scan_txs(
                round,
                &mut scan,
                &at,
                0,
                &block.txids,
                &block.txs,
                &mut progressed,
            )
            .await
        }
        Source::Pages(paged) => {
            match scan_pages(round, &mut scan, &at, paged, &mut progressed).await {
                Ok(finished) => finished,
                Err(error) => {
                    // The pages scanned before the node failed are kept.
                    record_checkpoint(round, height, &scan, tx_count);
                    let (progress, now, hash) = (scan.into_checkpoint(), round.now, hash.clone());
                    round
                        .db(move |s, network| checkpoint(s, network, height, &hash, progress, now))
                        .await?;
                    return Err(error);
                }
            }
        }
    };
    if !finished {
        record_checkpoint(round, height, &scan, tx_count);
        let (progress, now) = (scan.into_checkpoint(), round.now);
        round
            .db(move |s, network| checkpoint(s, network, height, &hash, progress, now))
            .await?;
        return Ok(BlockOutcome::Interrupted);
    }

    let scanned = scan.into_scanned(height);
    for block in &scanned {
        round.state.backoff.succeeded(block.tenant_id());
    }
    // Tenants with nothing in scope: to this block on the frontier, straight
    // to the high-water mark when catching up (nothing could have been paid
    // to them in the whole gap).
    let idle_to = if frontier { height } else { high_water };
    let checkpointed = plan.checkpoints.into_keys().collect();
    let txids = (!header_only).then(|| match &source {
        Source::Whole(block) => block.txids.iter().cloned().collect(),
        Source::Pages(paged) => paged.outline.txids.iter().cloned().collect(),
    });
    let commit_block = CommitBlock {
        height,
        checkpointed,
        hash,
        txids,
        prev_hash,
        parent,
        idle_to,
        since,
        grace,
    };
    let now = round.now;
    let stores = count(scanned.len());
    let committed = round
        .db(move |s, network| commit(s, network, &commit_block, &scanned, now))
        .await?;
    if let Some((matches, idle_moved)) = committed {
        round.state.activity().record(Event::Committed {
            height,
            group: group.into(),
            stores,
            matches: count(matches),
            idle_moved: count(idle_moved),
            header_only,
        });
        round
            .state
            .blocks
            .progress
            .lock()
            .finish_block(height, crate::now_unix());
        let mut paged = round.state.blocks.paged.lock();
        if paged.as_ref().is_some_and(|p| p.outline.height == height) {
            *paged = None;
        }
    }
    Ok(if committed.is_some() {
        BlockOutcome::Committed(plan.page)
    } else {
        BlockOutcome::Diverged("the recorded chain changed before commit")
    })
}

/// One block's scan in progress, for the tenants of one group: where each
/// tenant is in the block, what it found, and who failed. No I/O.
struct BlockScan {
    next_tx: HashMap<TenantId, usize>,
    found: HashMap<TenantId, Vec<ScanResult>>,
    failed: HashSet<TenantId>,
}

impl BlockScan {
    /// Every tenant starts at its checkpoint for this very block, else at
    /// the first transaction.
    fn new(
        scannable: &[(TenantId, WalletHandle, ScanIndices)],
        checkpoints: &HashMap<TenantId, BlockCheckpoint>,
        hash: &str,
        tx_count: usize,
    ) -> Self {
        let next_tx = scannable
            .iter()
            .map(|(id, _, _)| {
                // A block's hash names it, height and all: a checkpoint
                // for any other block is stale.
                let resume = match checkpoints.get(id) {
                    Some(c) if c.hash == hash => c.next_tx.min(tx_count),
                    _ => 0,
                };
                (id.clone(), resume)
            })
            .collect();
        Self {
            next_tx,
            found: HashMap::new(),
            failed: HashSet::new(),
        }
    }

    /// The tenants still to be scanned for some of the transactions
    /// `start..end`, each with how many of those it is already past.
    fn due<'a>(
        &self,
        scannable: &'a [(TenantId, WalletHandle, ScanIndices)],
        start: usize,
        end: usize,
    ) -> Vec<(&'a (TenantId, WalletHandle, ScanIndices), usize)> {
        scannable
            .iter()
            .filter(|(id, _, _)| !self.failed.contains(id))
            .filter_map(|tenant| {
                let next = *self.next_tx.get(&tenant.0)?;
                (next < end).then(|| (tenant, next.saturating_sub(start)))
            })
            .collect()
    }

    /// The first transaction some tenant still has to be scanned for;
    /// `tx_count` when none has.
    fn first_due(&self, tx_count: usize) -> usize {
        self.next_tx
            .iter()
            .filter(|(id, _)| !self.failed.contains(*id))
            .map(|(_, next)| *next)
            .min()
            .unwrap_or(tx_count)
            .min(tx_count)
    }

    /// `tenant_id` has now been scanned for every transaction before `next`.
    fn scanned(&mut self, tenant_id: TenantId, next: usize, found: Vec<ScanResult>) {
        if !found.is_empty() {
            self.found
                .entry(tenant_id.clone())
                .or_default()
                .extend(found);
        }
        self.next_tx.insert(tenant_id, next);
    }

    fn failed(&mut self, tenant_id: TenantId) {
        self.failed.insert(tenant_id);
    }

    /// What to write down if the unit stops here: each tenant that got
    /// anywhere, how far, and its matches so far.
    fn into_checkpoint(mut self) -> Vec<(TenantId, usize, Vec<ScanResult>)> {
        self.next_tx
            .into_iter()
            .filter(|(id, next)| !self.failed.contains(id) && *next > 0)
            .map(|(id, next)| {
                let found = self.found.remove(&id).unwrap_or_default();
                (id, next, found)
            })
            .collect()
    }

    /// The proofs for every tenant that got through the whole block.
    fn into_scanned(mut self, height: u64) -> Vec<ScannedBlock> {
        self.next_tx
            .into_keys()
            .filter(|id| !self.failed.contains(id))
            .map(|tenant_id| ScannedBlock {
                scans: self.found.remove(&tenant_id).unwrap_or_default(),
                tenant_id,
                height,
            })
            .collect()
    }
}

/// Records how far each tenant got through the block, with its matches
/// staged, so the next unit resumes there.
fn checkpoint(
    s: &Store,
    network: monero::Network,
    height: u64,
    hash: &str,
    progress: Vec<(TenantId, usize, Vec<ScanResult>)>,
    now: i64,
) -> Result<(), ScannerError> {
    let result = s.in_transaction(|s| -> Result<(), ScannerError> {
        for (tenant_id, next_tx, scans) in progress {
            s.save_block_checkpoint(
                network,
                &tenant_id,
                &BlockCheckpoint {
                    height,
                    hash: hash.to_owned(),
                    next_tx,
                },
            )?;
            for scan in scans {
                stage_block_match(s, network, &tenant_id, &scan, now)?;
            }
        }
        #[cfg(test)]
        crate::store::crash_checkpoint("staging.before_commit");
        Ok(())
    });
    #[cfg(test)]
    if result.is_ok() {
        crate::store::crash_checkpoint("staging.after_commit");
    }
    result
}

/// The block being committed and where its idle tenants go.
struct CommitBlock {
    height: u64,
    /// Tenants with a checkpoint (for any block): theirs is taken, promoted
    /// or dropped.
    checkpointed: HashSet<TenantId>,
    hash: String,
    /// The ids of the block's transactions, unless it was recorded from its
    /// header alone.
    txids: Option<HashSet<String>>,
    prev_hash: String,
    parent: u64,
    idle_to: u64,
    since: i64,
    grace: i64,
}

/// One transaction: record the block for the network (if it is the next
/// one), and for each tenant it was scanned for, move the cursor and record
/// its payments, staged and new. Idle tenants at the parent move along. The
/// payments' recompute obligations are left by the payment triggers.
///
/// Returns how many payments the block was found to hold for the tenants
/// that moved (staged from a checkpoint, or found now) and how many idle
/// tenants moved along, or `None` (nothing
/// written) unless the block still extends the recorded chain: the
/// recorded hash at its height (if any) is its own, and the recorded parent
/// (if any) is its parent.
fn commit(
    s: &Store,
    network: monero::Network,
    block: &CommitBlock,
    scanned: &[ScannedBlock],
    now: i64,
) -> Result<Option<(usize, usize)>, ScannerError> {
    let result = s.in_transaction(|s| -> Result<Option<(usize, usize)>, ScannerError> {
        let height = block.height;
        if s.settlement_frozen(network)? {
            return Ok(None);
        }
        if s.get_scanned_block_hash(network, block.parent)?
            .is_some_and(|parent| parent != block.prev_hash)
        {
            return Ok(None);
        }
        match s.get_scanned_block_hash(network, height)? {
            Some(stored) if stored != block.hash => return Ok(None),
            Some(_) => {}
            None => {
                if s.max_scanned_height(network)?
                    .is_none_or(|max| max + 1 == height)
                {
                    s.set_scanned_block(network, height, &block.hash)?;
                }
            }
        }
        // A payment can be given this height on a node's word before the
        // block is scanned (the vanished-payment check). The block it named
        // may since have been replaced above every recorded hash, where no
        // fork is detected: if this block doesn't hold the transaction, the
        // payment isn't in it. Unconfirmed, the vanished-payment check
        // follows it again.
        if let Some(txids) = &block.txids {
            for payment in s.payments_at_height(network, height)? {
                if !txids.contains(&payment.txid) {
                    s.update_payment_block_height(
                        &payment.order_id,
                        &payment.txid,
                        payment.output_index,
                        None,
                    )?;
                }
            }
        }
        let moved = s.advance_scanned_cursors(network, height, scanned)?;
        let mut matches = 0;
        for scanned in scanned {
            // A checkpoint's staged matches go either way: promoted if the
            // cursor moved, dropped if a rewind moved it meanwhile (they are
            // for a chain it no longer stands on). Only tenants that have one
            // are asked.
            let staged = if block.checkpointed.contains(scanned.tenant_id()) {
                s.take_staged_payments(network, scanned.tenant_id(), &block.hash)?
            } else {
                Vec::new()
            };
            if !moved.contains(scanned.tenant_id()) {
                continue;
            }
            matches += staged.len() + scanned.scans.len();
            for staged in staged {
                s.record_payment_match(
                    &staged.order_id,
                    &staged.txid,
                    staged.output_index,
                    staged.amount_piconero,
                    &staged.key_images_json,
                    staged.seen_at,
                    Some(crate::store::sql_height(height)?),
                    staged.output_key.as_deref(),
                )?;
                // Found in this block, whose id the scan computed from it:
                // what proof-of-work checking settles on
                // (docs/proof_of_work.md).
                s.attest_payment_block(&staged.txid, height, &block.hash)?;
            }
            for scan in &scanned.scans {
                if !record_scan_match(s, scanned.tenant_id(), scan, now, Some(height))?.is_empty() {
                    s.attest_payment_block(&scan.txid, height, &block.hash)?;
                }
            }
        }
        let idle = s.advance_idle_cursors(
            network,
            block.parent,
            block.idle_to,
            block.since,
            block.grace,
        )?;
        #[cfg(test)]
        if matches > 0 {
            crate::store::crash_checkpoint("publication.before_commit");
        }
        Ok(Some((matches, idle)))
    });
    #[cfg(test)]
    if matches!(result, Ok(Some((matches, _))) if matches > 0) {
        crate::store::crash_checkpoint("publication.after_commit");
    }
    result
}

/// The run of blocks to fetch ahead, from `next` up to `end`, if `next`
/// isn't held yet but the block before it is (the scan is about to run off
/// the end of what it has).
fn prefetch_range(round: &Round<'_>, next: u64, end: u64) -> Option<(u64, u64)> {
    let cache = &round.blocks.cache;
    if next > end || cache.contains(next) || !cache.contains(next - 1) {
        return None;
    }
    let count = whole_run(
        round,
        next,
        round.state.blocks.plan(round, next, end).blocks,
    );
    (count > 0).then_some((next, count))
}

impl BlockCache {
    /// Adds a fetched run of blocks, keeping within the memory budget
    /// around `keep`, which stays.
    fn add_chunk(
        &mut self,
        chunk: Vec<ChainBlock>,
        keep: u64,
        budget_mb: u32,
        link: Option<crate::link::LinkCost>,
        state: &BlockState,
        tuning: &super::ScanTuning,
    ) {
        if chunk.is_empty() {
            return;
        }
        // As the blocks came from the node: what requests are sized by.
        let sizes: Vec<usize> = chunk
            .iter()
            .map(|b| usize::try_from(b.wire_bytes).unwrap_or(usize::MAX))
            .collect();
        state.note_fetched(tuning, sizes.iter().sum(), chunk.len());
        state.note_sizes(tuning, &sizes, tuning.response_cap_bytes(budget_mb), link);
        for (block, bytes) in chunk.into_iter().zip(sizes) {
            self.insert(block, bytes);
        }
        let discarded = self.trim(Some(keep), budget_bytes(budget_mb));
        let mut progress = state.progress.lock();
        progress.discarded(discarded, crate::now_unix());
        progress.cache_bytes(self.bytes as u64, crate::now_unix());
    }
}

/// Where a block's transactions come from: the block fetched whole, or a
/// large block's outline, its transactions fetched a page at a time.
enum Source {
    Whole(Arc<ChainBlock>),
    Pages(Paged),
}

impl Source {
    /// The block's id and its parent's.
    fn identity(&self) -> (&str, &str) {
        match self {
            Self::Whole(block) => (&block.hash, &block.prev_hash),
            Self::Pages(paged) => (&paged.outline.hash, &paged.outline.prev_hash),
        }
    }

    fn tx_count(&self) -> usize {
        match self {
            Self::Whole(block) => block.txs.len(),
            Self::Pages(paged) => paged.outline.txids.len(),
        }
    }
}

/// The block a scan is of, the tenants it is for and when its unit's time
/// is up (`None`: it finishes the block whatever the time).
struct ScanAt<'s> {
    height: u64,
    until: Option<Instant>,
    scannable: &'s [(TenantId, WalletHandle, ScanIndices)],
}

/// Scans transactions `offset..offset + txs.len()` of the block for every
/// tenant still due them, the tuning's `txs_per_scan` at a time. `false` if
/// the unit's time ran out first (once it had made progress): the caller
/// writes down how far each tenant got.
async fn scan_txs(
    round: &Round<'_>,
    scan: &mut BlockScan,
    at: &ScanAt<'_>,
    offset: usize,
    txids: &[String],
    txs: &[ScanTx],
    progressed: &mut bool,
) -> bool {
    let inputs: Vec<ScanInput> = txs.iter().map(|tx| tx.input.clone()).collect();
    let tuning = round.state.tuning();
    for start in (0..txs.len()).step_by(tuning.txs_per_scan) {
        let end = (start + tuning.txs_per_scan).min(txs.len());
        for batch in scan
            .due(at.scannable, offset + start, offset + end)
            .chunks(tuning.scan_concurrency)
        {
            if *progressed && at.until.is_some_and(|until| Instant::now() >= until) {
                return false;
            }
            let scanning = Instant::now();
            let results = scan_txs_for_tenants(
                round.inputs.custody,
                &txids[start..end],
                &txs[start..end],
                &inputs[start..end],
                batch,
                tuning,
                super::Tier::Blocks,
            )
            .await;
            round.state.blocks.progress.lock().spent(
                crate::now_unix(),
                0.0,
                scanning.elapsed().as_secs_f64(),
                ((end - start) * batch.len()) as u64,
            );
            let height = at.height;
            for (tenant_id, result) in results {
                match result {
                    Ok(found) => scan.scanned(tenant_id, offset + end, found),
                    Err(error) => {
                        shared::throttled!(format!("block-scan:{tenant_id}"), warn, store.id = %tenant_id, network = crate::network::network_str(round.network()),
                            height, error = %error, "scanning a block failed for this store; it is caught up later");
                        round.state.backoff.failed(&tenant_id);
                        scan.failed(tenant_id);
                    }
                }
            }
            *progressed = true;
        }
    }
    true
}

/// Scans a large block a page of transactions at a time
/// (`docs/engine_scaling.md` section 4), from the first transaction some
/// tenant still needs: a page is fetched, scanned for every tenant due it
/// and dropped before the next. `Ok(false)` if the unit's time ran out
/// first.
async fn scan_pages(
    round: &Round<'_>,
    scan: &mut BlockScan,
    at: &ScanAt<'_>,
    paged: &Paged,
    progressed: &mut bool,
) -> Result<bool, ScannerError> {
    let txids = &paged.outline.txids;
    let total = txids.len();
    round
        .state
        .blocks
        .progress
        .lock()
        .fetched_block(at.height, paged.weight);
    let mut next = scan.first_due(total);
    while next < total {
        if *progressed && at.until.is_some_and(|until| Instant::now() >= until) {
            return Ok(false);
        }
        let avg_tx_bytes = round
            .state
            .blocks
            .avg_tx_bytes()
            .unwrap_or(paged.avg_tx_bytes);
        let plan = page_plan(
            round,
            avg_tx_bytes,
            (total - next) as u64,
            at.scannable.len(),
        );
        let len = usize::try_from(plan.blocks)
            .unwrap_or(usize::MAX)
            .min(total - next);
        round
            .state
            .blocks
            .progress
            .lock()
            .page(at.height, next as u64, total as u64, len as u64);
        round.state.activity().record(Event::BlockProgress {
            height: at.height,
            done_txs: count(next),
            total_txs: count(total),
        });
        let ids = &txids[next..next + len];
        let txs = fetch_page(round, at.height, ids, avg_tx_bytes).await?;
        if !scan_txs(round, scan, at, next, ids, &txs, progressed).await {
            return Ok(false);
        }
        next = scan.first_due(total).max(next + len);
    }
    Ok(true)
}

/// How many transactions the next page holds: sized to the response cap,
/// the link and the scan's own cost for this many stores, in the round's
/// share for blocks (`scanner::next_page`).
fn page_plan(
    round: &Round<'_>,
    avg_tx_bytes: f64,
    remaining: u64,
    stores: usize,
) -> crate::scanner::ChunkPlan {
    let mut progress = round.state.blocks.progress.lock();
    let scan_secs = progress
        .secs_per_tx_scan()
        .map(|secs| secs * stores.max(1) as f64);
    let tuning = round.state.tuning();
    let plan = crate::scanner::next_page(
        tuning,
        tuning.response_cap_bytes(round.inputs.scan_chunk_memory_budget_mb),
        round.inputs.daemon.link_cost(),
        avg_tx_bytes,
        scan_secs,
        remaining,
    );
    progress.last_chunk = Some(plan);
    plan
}

/// One page of a large block's transactions, in the outline's order,
/// given the time the node's link needs for it. A transaction the node
/// doesn't send fails the page: the block can't be scanned without it.
async fn fetch_page(
    round: &Round<'_>,
    height: u64,
    txids: &[String],
    avg_tx_bytes: f64,
) -> Result<Vec<ScanTx>, ScannerError> {
    let daemon = round.inputs.daemon;
    let bytes = (avg_tx_bytes * txids.len() as f64) as u64;
    let deadline = (daemon.transfer_timeout(bytes) + crate::daemon_fallback::DEADLINE_MARGIN)
        .max(super::CALL_DEADLINE);
    let started = Instant::now();
    let fetched = super::bounded_by(deadline, daemon.get_transactions_with_ids(txids)).await;
    round
        .state
        .blocks
        .note_fetch_time(started.elapsed().as_secs_f64());
    let fetched = match fetched {
        Ok(fetched) => fetched,
        Err(e) => {
            round.state.blocks.note_page_failed(&e);
            return Err(e);
        }
    };
    let mut by_id: HashMap<String, ScanTx> = fetched
        .into_iter()
        .map(|fetched| (fetched.txid, ScanTx::of(&fetched.tx)))
        .collect();
    txids
        .iter()
        .map(|txid| {
            by_id.remove(txid).ok_or_else(|| {
                ScannerError::Daemon(crate::daemon::DaemonError::Request(format!(
                    "the node didn't send transaction {txid} of block {height}"
                )))
            })
        })
        .collect()
}

/// Whether a block with this header is scanned in pages rather than
/// fetched whole (`scanner::scan_in_pages`).
fn in_pages(round: &Round<'_>, header: &ChainHeader) -> bool {
    let tuning = round.state.tuning();
    crate::scanner::scan_in_pages(
        tuning,
        header.weight,
        tuning.response_cap_bytes(round.inputs.scan_chunk_memory_budget_mb),
        round.inputs.daemon.link_cost(),
    )
}

/// How many of `count` blocks from `from` can come whole in one run: all of
/// them while headers aren't read first; else up to the first whose header
/// isn't held this round, or that is scanned in pages.
fn whole_run(round: &Round<'_>, from: u64, count: u64) -> u64 {
    if !round.state.blocks.headers_first() {
        return count;
    }
    (from..from.saturating_add(count))
        .take_while(|height| {
            round
                .blocks
                .headers
                .get(height)
                .is_some_and(|header| !in_pages(round, header))
        })
        .count() as u64
}

/// Block `height`'s header, fetched with the headers after it (up to `end`)
/// unless already held this round.
async fn known_header(
    round: &mut Round<'_>,
    height: u64,
    end: u64,
) -> Result<ChainHeader, ScannerError> {
    if !round.blocks.headers.contains_key(&height) {
        let count = (end.saturating_sub(height) + 1).min(round.state.tuning().headers_per_fetch);
        let headers = bounded(round.inputs.daemon.get_chain_headers(height, count)).await?;
        round
            .blocks
            .headers
            .extend(headers.into_iter().map(|header| (header.height, header)));
    }
    round.blocks.headers.get(&height).cloned().ok_or_else(|| {
        ScannerError::Daemon(crate::daemon::DaemonError::Request(format!(
            "the node returned no header at height {height}"
        )))
    })
}

/// Block `height` ready to scan: held whole, or, if it is large, its
/// outline.
async fn source(round: &mut Round<'_>, height: u64, end: u64) -> Result<Source, ScannerError> {
    if let Some(block) = round.blocks.cache.get(height) {
        return Ok(Source::Whole(block));
    }
    if round.state.blocks.headers_first() {
        let header = known_header(round, height, end).await?;
        if in_pages(round, &header) {
            return Ok(Source::Pages(paged(round, &header).await?));
        }
    }
    Ok(Source::Whole(block(round, height, end).await?))
}

/// Fetches block `height` whole ahead of its scan, unless it is held
/// already or is scanned in pages.
async fn hold(round: &mut Round<'_>, height: u64, end: u64) -> Result<(), ScannerError> {
    if round.blocks.cache.contains(height) {
        return Ok(());
    }
    if round.state.blocks.headers_first() {
        let header = known_header(round, height, end).await?;
        if in_pages(round, &header) {
            return Ok(());
        }
    }
    block(round, height, end).await?;
    Ok(())
}

/// A large block's outline, fetched once and kept across rounds until the
/// block is committed. An outline that isn't the block the header named
/// (the chain moved between the two) is refused; the next round asks again.
async fn paged(round: &Round<'_>, header: &ChainHeader) -> Result<Paged, ScannerError> {
    let kept = round
        .state
        .blocks
        .paged
        .lock()
        .clone()
        .filter(|paged| paged.outline.hash == header.hash);
    if let Some(paged) = kept {
        return Ok(paged);
    }
    let daemon = round.inputs.daemon;
    let bytes = header.tx_count.unwrap_or(0).saturating_mul(256);
    let deadline = (daemon.transfer_timeout(bytes) + crate::daemon_fallback::DEADLINE_MARGIN)
        .max(super::CALL_DEADLINE);
    let started = Instant::now();
    let outline = super::bounded_by(
        deadline,
        daemon.get_block_outline(header.height, header.tx_count),
    )
    .await;
    round
        .state
        .blocks
        .note_fetch_time(started.elapsed().as_secs_f64());
    let outline = outline?;
    if outline.hash != header.hash
        || outline.height != header.height
        || outline.prev_hash != header.prev_hash
        || outline.timestamp != header.timestamp
    {
        return Err(ScannerError::Daemon(crate::daemon::DaemonError::Request(
            format!(
                "block {} changed between its header and its outline",
                header.height
            ),
        )));
    }
    let weight = header.weight.unwrap_or(0);
    let paged = Paged {
        avg_tx_bytes: weight as f64 / outline.txids.len().max(1) as f64,
        outline: Arc::new(outline),
        weight,
    };
    tracing::info!(
        network = crate::network::network_str(round.network()),
        height = header.height,
        weight,
        transactions = paged.outline.txids.len(),
        "scanning a large block a page of transactions at a time"
    );
    *round.state.blocks.paged.lock() = Some(paged.clone());
    Ok(paged)
}

/// Block `height`, fetched with the blocks after it (up to `end`) in one
/// call sized to the scan memory budget, stopping before any block scanned
/// in pages.
async fn block(
    round: &mut Round<'_>,
    height: u64,
    end: u64,
) -> Result<Arc<ChainBlock>, ScannerError> {
    if let Some(block) = round.blocks.cache.get(height) {
        return Ok(block);
    }
    let count = round.state.blocks.plan(round, height, end).blocks;
    let count = whole_run(round, height, count).max(1);
    let started = Instant::now();
    let fetched = fetch_chunk(round.inputs.daemon, height, count).await;
    round
        .state
        .blocks
        .note_fetch_time(started.elapsed().as_secs_f64());
    let chunk = match fetched {
        Ok(chunk) => chunk,
        Err(e) => {
            round.state.blocks.note_failed(&e);
            return Err(e);
        }
    };
    // The block asked for is never evicted by its own fetch.
    let state = round.state;
    record_fetched(state, height, &chunk, false);
    round.blocks.cache.add_chunk(
        chunk,
        height,
        round.inputs.scan_chunk_memory_budget_mb,
        round.inputs.daemon.link_cost(),
        &state.blocks,
        state.tuning(),
    );
    round.blocks.cache.get(height).ok_or_else(|| {
        ScannerError::Daemon(crate::daemon::DaemonError::Request(format!(
            "the node returned no block at height {height}"
        )))
    })
}

/// Block `height` as its header alone (no transactions), for recording a
/// block nobody is scanned for. Headers come a run at a time, up to `end`. A
/// whole block already held this round serves as well and costs nothing.
///
/// The result is never put in the block cache, so a scan can't take it for
/// the block's contents.
async fn header_block(
    round: &mut Round<'_>,
    height: u64,
    end: u64,
) -> Result<Arc<ChainBlock>, ScannerError> {
    if let Some(block) = round.blocks.cache.get(height) {
        return Ok(block);
    }
    let header = known_header(round, height, end).await?;
    Ok(Arc::new(ChainBlock {
        height,
        hash: header.hash,
        prev_hash: header.prev_hash,
        timestamp: header.timestamp,
        txs: Vec::new(),
        txids: Vec::new(),
        wire_bytes: 0,
    }))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn block(height: u64) -> ChainBlock {
        ChainBlock {
            height,
            hash: format!("h{height}"),
            prev_hash: format!("h{}", height - 1),
            timestamp: 0,
            txs: vec![],
            txids: Vec::new(),
            wire_bytes: 0,
        }
    }

    /// With no large block in pages, a round gets the tuning's base round
    /// (the growth itself: `tuning::tests`).
    #[test]
    fn a_round_without_a_paged_block_gets_the_base() {
        let state = BlockState::default();
        let daemon = crate::daemon::fake::FakeDaemonClient::new();
        let tuning = super::super::ScanTuning::DEFAULT;
        assert_eq!(state.round_budget(&daemon, 3, &tuning), tuning.round_budget);
    }

    /// A fetched block within a quarter of the size that is paged turns
    /// headers-first on; ordinary blocks and a failure that isn't about
    /// size or time leave it off.
    #[test]
    fn a_block_near_the_paging_size_turns_headers_first_on() {
        let state = BlockState::default();
        let cap = 32_000_000;
        state.note_sizes(
            &super::super::ScanTuning::DEFAULT,
            &[300_000, 2_000_000],
            cap,
            None,
        );
        state.note_failed(&ScannerError::Daemon(crate::daemon::DaemonError::Request(
            "refused".into(),
        )));
        assert!(!state.headers_first());
        state.note_sizes(
            &super::super::ScanTuning::DEFAULT,
            &[300_000, 8_000_001],
            cap,
            None,
        );
        assert!(state.headers_first(), "a quarter of the cap");

        let state = BlockState::default();
        state.note_failed(&ScannerError::Daemon(crate::daemon::DaemonError::TooLarge(
            "over the cap".into(),
        )));
        assert!(state.headers_first(), "a request refused as too large");
    }

    /// Replacing a cached block keeps the byte count exact, and trimming
    /// evicts the blocks farthest from the one in use, never that one.
    #[test]
    fn the_block_cache_keeps_count_and_evicts_the_farthest_first() {
        let mut cache = BlockCache::default();
        for h in 10..=20 {
            cache.insert(block(h), 100);
        }
        cache.insert(block(15), 40);
        assert_eq!(cache.bytes, 10 * 100 + 40);
        assert_eq!(cache.trim(Some(12), 500), 540, "none had been read");
        assert!(cache.bytes <= 500);
        let kept: Vec<u64> = cache.blocks.keys().copied().collect();
        assert!(kept.contains(&12));
        assert!(kept.iter().all(|h| h.abs_diff(12) <= 3), "{kept:?}");
        cache.trim(Some(12), 0);
        assert_eq!(
            cache.blocks.keys().copied().collect::<Vec<_>>(),
            vec![12],
            "the block in use always stays"
        );
    }

    /// Blocks a group has scanned go before blocks fetched ahead: with many
    /// groups sharing the budget, one group's run fetched ahead isn't
    /// evicted while blocks already scanned remain. Handing a block to a
    /// scan doesn't count: only a committed scan does, so a block whose scan
    /// was interrupted is kept like an unscanned one, and counts as
    /// discarded if it goes.
    #[test]
    fn the_block_cache_evicts_scanned_blocks_before_the_rest() {
        let mut cache = BlockCache::default();
        for h in 10..=20 {
            cache.insert(block(h), 100);
        }
        for h in [11, 19] {
            cache.mark_scanned(h);
        }
        assert!(cache.get(12).is_some(), "handed to a scan, not scanned");
        assert_eq!(cache.trim(Some(15), 900), 0, "the scanned ones went");
        assert!(!cache.contains(11) && !cache.contains(19));
        assert_eq!(
            cache.trim(Some(15), 800),
            100,
            "then the farthest unscanned"
        );
        assert!(!cache.contains(10) && cache.contains(20) && cache.contains(12));
        assert_eq!(cache.retain(12..=16), 300, "17, 18 and 20, unscanned");
        assert_eq!(cache.bytes, 500);
        cache.mark_scanned(15);
        assert_eq!(cache.clear(), 400, "15 was scanned");
        assert_eq!(cache.bytes, 0);
    }

    /// A round starts from what the last one left, trimmed to the memory
    /// budget as it is now, and only on the node the blocks came from. What
    /// is let go of unscanned is counted for `/status`.
    #[tokio::test]
    async fn a_round_resumes_the_cache_within_todays_budget() {
        let state = BlockState::default();
        let carry = |state: &BlockState, node| {
            let mut cache = BlockCache::default();
            for h in 1..=4 {
                cache.insert(block(h), 512 * 1024);
            }
            *state.carried.lock() = Some(Carried { cache, node });
        };
        let store = Store::open_in_memory().unwrap().into_shared();
        let db = crate::store::Db::over_shared(store);
        let custody = crate::key_custody::PlainKeyCustody::default();
        let daemon = crate::daemon::fake::FakeDaemonClient::new();
        let inputs = super::super::RoundInputs {
            db: &db,
            custody: &custody,
            daemon: &daemon,
            network: monero::Network::Mainnet,
            tenants: &[],
            reorg_check_depth: 10,
            grace_period_seconds: 0,
            scan_chunk_memory_budget_mb: 1,
            order_event_retention_secs: crate::store::DEFAULT_ORDER_EVENT_RETENTION_SECS,
        };

        carry(&state, crate::daemon::NodeKey::default());
        let round = BlocksRound::resume(&state, &inputs);
        assert_eq!(round.cache.bytes, 1024 * 1024, "two of four fit 1 MB");
        assert!(round.cache.contains(1) && round.cache.contains(2));
        assert!(state.carried.lock().is_none(), "taken, not shared");
        assert_eq!(state.progress.lock().discarded_cache_bytes, 1024 * 1024);

        carry(&state, crate::daemon::NodeKey(7));
        let round = BlocksRound::resume(&state, &inputs);
        assert_eq!(round.cache.bytes, 0, "another node's blocks");
        assert_eq!(state.progress.lock().discarded_cache_bytes, 3 * 1024 * 1024);
    }
}

#[cfg(test)]
#[path = "../../tests/verification/work/blocks/properties.rs"]
#[cfg_attr(coverage_nightly, coverage(off))]
mod properties;

#[cfg(test)]
#[path = "../../tests/verification/work/blocks/scale.rs"]
#[cfg_attr(coverage_nightly, coverage(off))]
mod scale;
