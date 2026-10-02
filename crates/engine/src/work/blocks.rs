//! The blocks tier: scanning blocks for tenants (docs/scanner_microtasks.md).
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
//! Each block's header comes first (a run of them at a time): a block too
//! large for one answer, or for the node's link to send in time, is scanned
//! a page of transactions at a time from its outline (its transactions'
//! ids), a page per step, across as many units and rounds as it takes
//! (docs/engine_scaling.md section 4). Blocks around it are fetched whole.

use crate::store::TenantId;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::time::Instant;

use crate::daemon::{BlockOutline, ChainBlock, ChainHeader, ScanTx};
use crate::key_custody::{ScanIndices, ScanInput, WalletHandle};
use crate::scanner::{
    record_scan_match, scan_txs_for_tenants, stage_block_match, ScanResult, ScannerError,
    SCAN_CONCURRENCY,
};
use crate::store::position::CatchUpGroup;
use crate::store::{BlockCheckpoint, Store};

use super::{bounded, Progress, Round, Wait};

/// Most tenants of one group scanned by one unit. The rest of the group
/// stays at its cursor and becomes its own catch-up group.
const GROUP_PAGE: usize = 256;
/// Most blocks one unit scans for its group before yielding the tier.
const BLOCKS_PER_UNIT: usize = 8;
/// Most headers fetched at once for blocks recorded without being scanned.
const HEADERS_PER_FETCH: u64 = 256;
/// Transactions of a block scanned for a tenant in one key-custody call. A
/// call costs a hop to a worker thread or a round trip to another process,
/// which a run of transactions shares. It is also how far a unit gets
/// between looks at the clock, and how much of a block a tenant whose call
/// fails has to be scanned for again.
pub(super) const TXS_PER_SCAN: usize = 32;
/// The time a round needs when the smallest unit of a large block takes
/// `unit_secs`: the base while that fits the round's share for blocks, else
/// half as much again as the unit, within [`MAX_ROUND_BUDGET`].
fn round_budget_for(unit_secs: f64) -> std::time::Duration {
    let base = super::ROUND_BUDGET;
    let share = base.as_secs_f64() * f64::from(super::Tier::Blocks.reserved_percent()) / 100.0;
    if unit_secs > share {
        std::time::Duration::from_secs_f64((unit_secs * 1.5).min(MAX_ROUND_BUDGET.as_secs_f64()))
            .max(base)
    } else {
        base
    }
}

/// The most a round may be given for one page of a large block
/// (docs/engine_scaling.md section 4).
pub(crate) const MAX_ROUND_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);
/// How far ahead of real time consensus lets a block's timestamp run.
/// Catch-up windows start this much before a block's own timestamp, so a
/// forward-dated block can't hide an order that was open when it was mined.
const BLOCK_TIMESTAMP_DRIFT_SECONDS: i64 = 2 * 60 * 60;

/// Proof that a block was scanned in full for a tenant, with the results.
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
    pub(crate) fn for_test(tenant_id: &crate::store::TenantId, height: u64) -> Self {
        Self {
            tenant_id: shared::ids::TenantId::new(tenant_id.to_string()),
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
}

/// A large block being scanned a page at a time (docs/engine_scaling.md
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
        BlockState::with_progress(crate::scaling::new_progress())
    }
}

impl BlockState {
    /// State whose progress (sizing, the block in progress, recent blocks)
    /// is `progress`, which `/status` reads.
    pub(crate) fn with_progress(progress: crate::scaling::SharedProgress) -> Self {
        BlockState {
            catch_up_turn: AtomicBool::new(false),
            progress,
            paged: parking_lot::Mutex::new(None),
        }
    }

    /// The time a round needs (docs/engine_scaling.md section 4): the base,
    /// unless the smallest unit of a large block in progress (one page of
    /// one transaction, fetched and scanned for `stores` stores) is
    /// expected to need more than the round's share for blocks. Then half
    /// as much again as that unit, within two minutes.
    pub(crate) fn round_budget(
        &self,
        daemon: &dyn crate::daemon::MoneroDaemonClient,
        stores: usize,
    ) -> std::time::Duration {
        let base = super::ROUND_BUDGET;
        let Some(avg_tx_bytes) = self.paged.lock().as_ref().map(|paged| paged.avg_tx_bytes) else {
            self.progress.lock().round_budget = base;
            return base;
        };
        let link = daemon.link();
        let fetch = link.as_ref().map_or(0.0, |link| {
            (link.rtt_ms + link.ttfb_per_block_ms) as f64 / 1000.0
        }) + daemon
            .transfer_rate()
            .map_or(0.0, |rate| avg_tx_bytes / rate.max(1.0));
        let scan = self.progress.lock().secs_per_tx_scan().unwrap_or(0.0) * stores.max(1) as f64;
        let budget = round_budget_for(fetch + scan);
        self.progress.lock().round_budget = budget;
        budget
    }

    /// How many blocks to ask for from `from`, up to `end`.
    fn plan(&self, round: &Round<'_>, from: u64, end: u64) -> crate::scanner::ChunkPlan {
        let mut progress = self.progress.lock();
        let plan = crate::scanner::next_scan_chunk(
            crate::scanner::response_cap_bytes(round.inputs.scan_chunk_memory_budget_mb),
            round.inputs.daemon.transfer_rate(),
            progress.avg_bytes_per_block,
            end.saturating_sub(from) + 1,
        );
        progress.last_chunk = Some(plan);
        plan
    }

    /// A fetched run of `blocks` blocks totalling `bytes`, in `secs`.
    fn note_fetched(&self, bytes: usize, blocks: usize) {
        if blocks == 0 {
            return;
        }
        let mut progress = self.progress.lock();
        progress.avg_bytes_per_block =
            crate::scanner::update_avg_bytes_per_block(progress.avg_bytes_per_block, bytes, blocks);
    }

    /// A block request that failed: one that ran out of time or came back
    /// too large halves the next.
    fn note_failed(&self, error: &ScannerError) {
        if matches!(error, ScannerError::Daemon(e) if e.asks_for_less()) {
            let mut progress = self.progress.lock();
            progress.avg_bytes_per_block =
                crate::scanner::avg_after_failed_fetch(progress.avg_bytes_per_block);
        }
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
/// for that many (docs/engine_scaling.md section 2).
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
}

impl Rotation {
    /// The next group after the rotation's position, wrapping once.
    async fn next(
        &mut self,
        round: &Round<'_>,
        high_water: u64,
    ) -> Result<Option<u64>, ScannerError> {
        let (last, visited) = (self.last, self.visited.clone());
        let group = round
            .db(move |s, network| -> Result<_, ScannerError> {
                let after = match last {
                    Some(last) => Some(last),
                    None => s.scheduler_position::<CatchUpGroup>(network)?,
                };
                let mut next = s.scan_group_cursors(network, high_water, after, 1)?;
                if next.is_empty() && after.is_some() {
                    next = s.scan_group_cursors(network, high_water, None, 1)?;
                }
                Ok(next
                    .first()
                    .copied()
                    .filter(|group| !visited.contains(group)))
            })
            .await?;
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
        let position = reached.max(group);
        self.last = Some(position);
        round
            .db(move |s, network| s.set_scheduler_position::<CatchUpGroup>(network, &position))
            .await
    }
}

/// Blocks fetched this round, bounded by the scan memory budget. Each block
/// carries its own id, so a cached body can never be paired with another
/// block's hash; the cache is still per round, so nothing stale is kept.
#[derive(Default)]
struct BlockCache {
    blocks: BTreeMap<u64, (Arc<ChainBlock>, usize)>,
    bytes: usize,
}

impl BlockCache {
    fn insert(&mut self, block: ChainBlock, bytes: usize) -> Arc<ChainBlock> {
        let block = Arc::new(block);
        if let Some((_, old)) = self.blocks.insert(block.height, (block.clone(), bytes)) {
            self.bytes -= old;
        }
        self.bytes += bytes;
        block
    }

    /// Evicts the blocks farthest from `keep` until within `budget`; `keep`
    /// itself always stays.
    fn trim(&mut self, keep: u64, budget: usize) {
        // Farthest first; of two as far, the lower (already scanned past).
        let mut victims: Vec<u64> = self
            .blocks
            .keys()
            .copied()
            .filter(|&height| height != keep)
            .collect();
        victims.sort_by_key(|&height| std::cmp::Reverse(height.abs_diff(keep)));
        for height in victims {
            if self.bytes <= budget {
                break;
            }
            self.bytes -= self.blocks.remove(&height).map_or(0, |(_, bytes)| bytes);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Group {
    Frontier,
    CatchUp,
}

enum BlockOutcome {
    Committed,
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
    if round.chain.rewound {
        return Ok(Progress::Blocked(Wait::RewoundThisRound));
    }
    let repair = !round.blocks.repaired;
    let (reorg_open, high_water) = round
        .db(move |s, network| -> Result<_, ScannerError> {
            let reorg_open = s.reorg_job(network)?.is_some();
            let high_water = s.max_scanned_height(network)?;
            if let (false, true, Some(high_water)) = (reorg_open, repair, high_water) {
                // Cheap repairs whatever happened before: no cursor ahead of
                // the network, new tenants anchored at it, disabled ones
                // moved along.
                s.clamp_cursors(network, Some(high_water))?;
                s.anchor_unset_cursors(network, high_water)?;
                s.snap_disabled_cursors(network, high_water)?;
            }
            Ok((reorg_open, high_water))
        })
        .await?;
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
    round
        .db(move |s, network| s.advance_idle_cursors(network, group, high_water, i64::MIN / 2, 0))
        .await?;
    // A group that diverged stays where it is; the rotation still moves on
    // past it, so the round's other groups are served.
    let reached = advance_group(round, Group::CatchUp, group, tip, until).await?;
    rotation.served(round, group, reached.cursor).await?;
    Ok(true)
}

/// First run on a network: start just below the node's tip rather than
/// replaying history (a payment gateway watches for new payments). One block
/// of margin, for a node reporting a tip it can't serve yet.
async fn seed(round: &mut Round<'_>, tip: u64) -> Result<Progress, ScannerError> {
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
            Ok(Progress::Advanced)
        }
        Err(_) => Ok(Progress::Blocked(Wait::NodeCannotServeTip)),
    }
}

/// Scans up to `BLOCKS_PER_UNIT` blocks for the group at `cursor`, one
/// after another, as far as its time allows (always at least one step of
/// progress). The frontier stops at the tip; catch-up stops at the
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
    let mut high_water = round
        .db(|s, network| s.max_scanned_height(network))
        .await?
        .unwrap_or(cursor);
    for scanned in 0..BLOCKS_PER_UNIT {
        let end = match group {
            Group::Frontier => tip,
            Group::CatchUp => high_water,
        };
        if cursor >= end || (scanned > 0 && Instant::now() >= until) {
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
            must_progress: scanned == 0,
        };
        let (outcome, prefetched) = tokio::join!(scan_block(round, task), async {
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
                round.blocks.cache.add_chunk(
                    chunk,
                    cursor + 2,
                    round.inputs.scan_chunk_memory_budget_mb,
                    &state.blocks,
                );
            }
            // A failed prefetch is asked for again when it's needed.
            Some(Err(e)) => state.blocks.note_failed(&e),
            None => {}
        }
        match outcome? {
            BlockOutcome::Committed => {
                cursor += 1;
                high_water = high_water.max(cursor);
            }
            BlockOutcome::Interrupted | BlockOutcome::NobodyToScan => break,
            BlockOutcome::Diverged(reason) => {
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
struct Reached {
    cursor: u64,
    diverged: bool,
}

/// What one database read tells a block scan before it starts.
struct Plan {
    /// Tenants at the parent cursor with something in scope, and their
    /// windows.
    members: Vec<(crate::store::TenantId, Vec<u32>)>,
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
    /// This is the unit's first block: it makes progress even past `until`.
    must_progress: bool,
}

/// Scans block `task.parent + 1` for the tenants at cursor `task.parent`.
async fn scan_block(round: &mut Round<'_>, task: BlockTask) -> Result<BlockOutcome, ScannerError> {
    let BlockTask {
        group,
        parent,
        high_water,
        end,
        until,
        must_progress,
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
    let waiting = round.state.backoff.waiting();
    // A catch-up group none of whose stores can be scanned (keys not
    // registered, waiting to retry) waits where it is, before anything is
    // fetched. The frontier still records the block for the network.
    if group == Group::CatchUp {
        let excluded = waiting.clone();
        let ids = round
            .db(move |s, network| s.tenants_at_cursor(network, parent, &excluded, GROUP_PAGE))
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
        let header = known_header(round, height, end).await?;
        i64::try_from(header.timestamp)
            .unwrap_or(i64::MAX)
            .saturating_sub(BLOCK_TIMESTAMP_DRIFT_SECONDS)
            .min(round.now)
    };
    let plan = round
        .db(move |s, network| -> Result<_, ScannerError> {
            let ids = s.tenants_at_cursor(network, parent, &waiting, GROUP_PAGE)?;
            let mut windows = s.scan_windows(&ids, since, grace)?;
            let members: Vec<(crate::store::TenantId, Vec<u32>)> = ids
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
                members,
                recorded: s.get_scanned_block_hash(network, height)?,
                parent: s.get_scanned_block_hash(network, parent)?,
                checkpoints,
            })
        })
        .await?;
    let scannable: Vec<(crate::store::TenantId, WalletHandle, ScanIndices)> = plan
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
        round
            .db(move |s, network| s.advance_idle_cursors(network, parent, high_water, since, grace))
            .await?;
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
        .is_some_and(|parent| parent != prev_hash)
    {
        return Ok(BlockOutcome::Diverged(
            "the node's block doesn't extend the recorded chain",
        ));
    }
    let (hash, prev_hash) = (hash.to_string(), prev_hash.to_string());

    let mut scan = BlockScan::new(&scannable, &plan.checkpoints, &hash, source.tx_count());
    let mut progressed = !must_progress;
    let at = ScanAt {
        height,
        until,
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
    let commit_block = CommitBlock {
        height,
        checkpointed,
        hash,
        prev_hash,
        parent,
        idle_to,
        since,
        grace,
    };
    let now = round.now;
    let committed = round
        .db(move |s, network| commit(s, network, &commit_block, scanned, now))
        .await?;
    if committed {
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
    Ok(if committed {
        BlockOutcome::Committed
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
        scannable: &[(crate::store::TenantId, WalletHandle, ScanIndices)],
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
        scannable: &'a [(crate::store::TenantId, WalletHandle, ScanIndices)],
        start: usize,
        end: usize,
    ) -> Vec<(
        &'a (crate::store::TenantId, WalletHandle, ScanIndices),
        usize,
    )> {
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
    fn into_checkpoint(mut self) -> Vec<(crate::store::TenantId, usize, Vec<ScanResult>)> {
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
    progress: Vec<(crate::store::TenantId, usize, Vec<ScanResult>)>,
    now: i64,
) -> Result<(), ScannerError> {
    s.in_transaction(|s| -> Result<(), ScannerError> {
        for (tenant_id, next_tx, scans) in progress {
            s.save_block_checkpoint(
                network,
                &tenant_id,
                &BlockCheckpoint {
                    height,
                    hash: hash.to_string(),
                    next_tx,
                },
            )?;
            for scan in scans {
                stage_block_match(s, network, &tenant_id, &scan, now)?;
            }
        }
        Ok(())
    })
}

/// The block being committed and where its idle tenants go.
struct CommitBlock {
    height: u64,
    /// Tenants with a checkpoint (for any block): theirs is taken, promoted
    /// or dropped.
    checkpointed: HashSet<TenantId>,
    hash: String,
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
/// `false` (nothing written) unless the block still extends the recorded
/// chain: the recorded hash at its height (if any) is its own, and the
/// recorded parent (if any) is its parent.
fn commit(
    s: &Store,
    network: monero::Network,
    block: &CommitBlock,
    scanned: Vec<ScannedBlock>,
    now: i64,
) -> Result<bool, ScannerError> {
    s.in_transaction(|s| -> Result<bool, ScannerError> {
        let height = block.height;
        if s.get_scanned_block_hash(network, block.parent)?
            .is_some_and(|parent| parent != block.prev_hash)
        {
            return Ok(false);
        }
        match s.get_scanned_block_hash(network, height)? {
            Some(stored) if stored != block.hash => return Ok(false),
            Some(_) => {}
            None => {
                if s.max_scanned_height(network)?
                    .is_none_or(|max| max + 1 == height)
                {
                    s.set_scanned_block(network, height, &block.hash)?;
                }
            }
        }
        let moved = s.advance_scanned_cursors(network, height, &scanned)?;
        for scanned in &scanned {
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
            }
            for scan in &scanned.scans {
                record_scan_match(s, scanned.tenant_id(), scan, now, Some(height))?;
            }
        }
        s.advance_idle_cursors(
            network,
            block.parent,
            block.idle_to,
            block.since,
            block.grace,
        )?;
        Ok(true)
    })
}

/// The run of blocks to fetch ahead, from `next` up to `end`, if `next`
/// isn't held yet but the block before it is (the scan is about to run off
/// the end of what it has).
fn prefetch_range(round: &Round<'_>, next: u64, end: u64) -> Option<(u64, u64)> {
    let cache = &round.blocks.cache;
    if next > end || cache.blocks.contains_key(&next) || !cache.blocks.contains_key(&(next - 1)) {
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
    /// around `keep`. Returns block `keep`, if the run had it.
    fn add_chunk(
        &mut self,
        chunk: Vec<ChainBlock>,
        keep: u64,
        budget_mb: u32,
        state: &BlockState,
    ) -> Option<Arc<ChainBlock>> {
        if chunk.is_empty() {
            return None;
        }
        // As the blocks came from the node: what requests are sized by.
        let sizes: Vec<usize> = chunk
            .iter()
            .map(|b| usize::try_from(b.wire_bytes).unwrap_or(usize::MAX))
            .collect();
        state.note_fetched(sizes.iter().sum(), chunk.len());
        let mut kept = None;
        for (block, bytes) in chunk.into_iter().zip(sizes) {
            let block = self.insert(block, bytes);
            if block.height == keep {
                kept = Some(block);
            }
        }
        self.trim(
            keep,
            usize::try_from(budget_mb)
                .unwrap_or(usize::MAX)
                .saturating_mul(1024 * 1024),
        );
        state
            .progress
            .lock()
            .cache_bytes(self.bytes as u64, crate::now_unix());
        kept
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
            Source::Whole(block) => (&block.hash, &block.prev_hash),
            Source::Pages(paged) => (&paged.outline.hash, &paged.outline.prev_hash),
        }
    }

    fn tx_count(&self) -> usize {
        match self {
            Source::Whole(block) => block.txs.len(),
            Source::Pages(paged) => paged.outline.txids.len(),
        }
    }
}

/// The block a scan is of, the tenants it is for and when its unit's time
/// is up.
struct ScanAt<'s> {
    height: u64,
    until: Instant,
    scannable: &'s [(crate::store::TenantId, WalletHandle, ScanIndices)],
}

/// Scans transactions `offset..offset + txs.len()` of the block for every
/// tenant still due them, [`TXS_PER_SCAN`] at a time. `false` if the unit's
/// time ran out first (once it had made progress): the caller writes down
/// how far each tenant got.
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
    for start in (0..txs.len()).step_by(TXS_PER_SCAN) {
        let end = (start + TXS_PER_SCAN).min(txs.len());
        for batch in scan
            .due(at.scannable, offset + start, offset + end)
            .chunks(SCAN_CONCURRENCY)
        {
            if *progressed && Instant::now() >= at.until {
                return false;
            }
            let scanning = Instant::now();
            let results = scan_txs_for_tenants(
                round.inputs.custody,
                &txids[start..end],
                &txs[start..end],
                &inputs[start..end],
                batch,
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
/// (docs/engine_scaling.md section 4), from the first transaction some
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
        if *progressed && Instant::now() >= at.until {
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
    let share = super::ROUND_BUDGET.as_secs_f64()
        * f64::from(super::Tier::Blocks.reserved_percent())
        / 100.0;
    let plan = crate::scanner::next_page(
        crate::scanner::response_cap_bytes(round.inputs.scan_chunk_memory_budget_mb),
        round.inputs.daemon.transfer_rate(),
        avg_tx_bytes,
        scan_secs,
        share,
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
    crate::scanner::scan_in_pages(
        header.weight,
        crate::scanner::response_cap_bytes(round.inputs.scan_chunk_memory_budget_mb),
        round.inputs.daemon.transfer_rate(),
    )
}

/// How many of `count` blocks from `from` can come whole in one run: up to
/// the first whose header isn't held this round, or that is scanned in
/// pages.
fn whole_run(round: &Round<'_>, from: u64, count: u64) -> u64 {
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
        let count = (end.saturating_sub(height) + 1).min(HEADERS_PER_FETCH);
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
    if let Some((block, _)) = round.blocks.cache.blocks.get(&height) {
        return Ok(Source::Whole(block.clone()));
    }
    let header = known_header(round, height, end).await?;
    if in_pages(round, &header) {
        return Ok(Source::Pages(paged(round, &header).await?));
    }
    Ok(Source::Whole(block(round, height, end).await?))
}

/// Fetches block `height` whole ahead of its scan, unless it is held
/// already or is scanned in pages.
async fn hold(round: &mut Round<'_>, height: u64, end: u64) -> Result<(), ScannerError> {
    if round.blocks.cache.blocks.contains_key(&height) {
        return Ok(());
    }
    let header = known_header(round, height, end).await?;
    if !in_pages(round, &header) {
        block(round, height, end).await?;
    }
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
    if outline.hash != header.hash {
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
    if let Some((block, _)) = round.blocks.cache.blocks.get(&height) {
        return Ok(block.clone());
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
    round
        .blocks
        .cache
        .add_chunk(
            chunk,
            height,
            round.inputs.scan_chunk_memory_budget_mb,
            &state.blocks,
        )
        .ok_or_else(|| {
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
    if let Some((block, _)) = round.blocks.cache.blocks.get(&height) {
        return Ok(block.clone());
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

    /// A round keeps its base time unless one page of one transaction
    /// can't fit the share for blocks; then it gets half as much again as
    /// that page, never past two minutes (docs/engine_scaling.md section 4).
    #[test]
    fn the_round_grows_only_for_a_page_that_cannot_fit_its_share() {
        use std::time::Duration;
        assert_eq!(round_budget_for(0.0), Duration::from_secs(10));
        assert_eq!(
            round_budget_for(4.0),
            Duration::from_secs(10),
            "the share is 4 s"
        );
        assert_eq!(
            round_budget_for(5.0),
            Duration::from_secs(10),
            "1.5 x 5 s is under the base"
        );
        assert_eq!(round_budget_for(20.0), Duration::from_secs(30));
        assert_eq!(round_budget_for(1_000.0), MAX_ROUND_BUDGET);
        // Nothing in pages: the base.
        let state = BlockState::default();
        let daemon = crate::daemon::fake::FakeDaemonClient::new();
        assert_eq!(state.round_budget(&daemon, 3), Duration::from_secs(10));
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
        cache.trim(12, 500);
        assert!(cache.bytes <= 500);
        let kept: Vec<u64> = cache.blocks.keys().copied().collect();
        assert!(kept.contains(&12));
        assert!(kept.iter().all(|h| h.abs_diff(12) <= 3), "{kept:?}");
        cache.trim(12, 0);
        assert_eq!(
            cache.blocks.keys().copied().collect::<Vec<_>>(),
            vec![12],
            "the block in use always stays"
        );
    }
}
