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
//! A block's results stay in memory ([`BlockScan`]) until the whole block
//! is scanned, then commit in one transaction with the cursor moves, and
//! only if the block still extends the recorded chain. If a unit runs out of
//! time partway through a block, its progress is written down (a checkpoint
//! with staged matches), to resume from.

use crate::store::TenantId;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::time::Instant;

use crate::daemon::ChainBlock;
use crate::key_custody::{ScanIndices, WalletHandle};
use crate::scanner::{
    record_scan_match, scan_for_tenants, stage_block_match, ScanResult, ScannerError,
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

/// Kept across rounds: whose turn it is while the frontier is behind. Across
/// rounds, not per round, so a round with time for one unit alternates too.
#[derive(Default)]
pub(crate) struct BlockState {
    catch_up_turn: AtomicBool,
}

#[derive(Default)]
pub(crate) struct BlocksRound {
    repaired: bool,
    frontier_done: bool,
    rotation: Rotation,
    cache: BlockCache,
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
struct BlockCache {
    blocks: BTreeMap<u64, (Arc<ChainBlock>, usize)>,
    bytes: usize,
    avg_bytes_per_block: f64,
}

impl Default for BlockCache {
    fn default() -> Self {
        Self {
            blocks: BTreeMap::new(),
            bytes: 0,
            avg_bytes_per_block: crate::scanner::SCAN_CHUNK_INITIAL_AVG_BYTES,
        }
    }
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
        // where the scan first checks there is anyone to scan it for.
        if group == Group::Frontier || scanned > 0 {
            block(round, cursor + 1, end).await?;
        }
        let prefetch = prefetch_range(round, cursor + 2, end);
        let daemon = round.inputs.daemon;
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
                Some((from, count)) => bounded(daemon.get_chain_blocks(from, count)).await.ok(),
                None => None,
            }
        });
        if let Some(chunk) = prefetched {
            round.blocks.cache.add_chunk(
                chunk,
                cursor + 2,
                round.inputs.scan_chunk_memory_budget_mb,
            );
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
        let block = block(round, height, end).await?;
        i64::try_from(block.timestamp)
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

    let block = block(round, height, end).await?;
    if plan
        .recorded
        .as_ref()
        .is_some_and(|recorded| *recorded != block.hash)
    {
        return Ok(BlockOutcome::Diverged(
            "the node's block differs from the one recorded",
        ));
    }
    if plan
        .parent
        .as_ref()
        .is_some_and(|parent| *parent != block.prev_hash)
    {
        return Ok(BlockOutcome::Diverged(
            "the node's block doesn't extend the recorded chain",
        ));
    }

    let mut scan = BlockScan::new(&scannable, &plan.checkpoints, &block);
    let mut progressed = !must_progress;
    for (index, tx) in block.txs.iter().enumerate() {
        for batch in scan.due(&scannable, index).chunks(SCAN_CONCURRENCY) {
            if progressed && Instant::now() >= until {
                let (progress, hash, now) = (scan.into_checkpoint(), block.hash.clone(), round.now);
                round
                    .db(move |s, network| checkpoint(s, network, height, &hash, progress, now))
                    .await?;
                return Ok(BlockOutcome::Interrupted);
            }
            for (tenant_id, result) in scan_for_tenants(round.inputs.custody, tx, batch).await {
                match result {
                    Ok(found) => scan.scanned(tenant_id, index, found),
                    Err(error) => {
                        shared::throttled!(format!("block-scan:{tenant_id}"), warn, store.id = %tenant_id, network = crate::network::network_str(round.network()),
                            height, error = %error, "scanning a block failed for this store; it is caught up later");
                        round.state.backoff.failed(&tenant_id);
                        scan.failed(tenant_id);
                    }
                }
            }
            progressed = true;
        }
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
        hash: block.hash.clone(),
        prev_hash: block.prev_hash.clone(),
        parent,
        idle_to,
        since,
        grace,
    };
    let now = round.now;
    let committed = round
        .db(move |s, network| commit(s, network, &commit_block, scanned, now))
        .await?;
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
        block: &ChainBlock,
    ) -> Self {
        let next_tx = scannable
            .iter()
            .map(|(id, _, _)| {
                // A block's hash names it, height and all: a checkpoint
                // for any other block is stale.
                let resume = match checkpoints.get(id) {
                    Some(c) if c.hash == block.hash => c.next_tx.min(block.txs.len()),
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

    /// The tenants still to be scanned for transaction `index`.
    fn due<'a>(
        &self,
        scannable: &'a [(crate::store::TenantId, WalletHandle, ScanIndices)],
        index: usize,
    ) -> Vec<&'a (crate::store::TenantId, WalletHandle, ScanIndices)> {
        scannable
            .iter()
            .filter(|(id, _, _)| {
                !self.failed.contains(id) && self.next_tx.get(id).is_some_and(|next| *next <= index)
            })
            .collect()
    }

    fn scanned(&mut self, tenant_id: TenantId, index: usize, found: ScanResult) {
        if !found.matches.is_empty() {
            self.found.entry(tenant_id.clone()).or_default().push(found);
        }
        self.next_tx.insert(tenant_id, index + 1);
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
    let budget = u64::from(round.inputs.scan_chunk_memory_budget_mb).saturating_mul(1024 * 1024);
    Some((
        next,
        crate::scanner::next_scan_chunk_size(budget, cache.avg_bytes_per_block, end - next + 1),
    ))
}

impl BlockCache {
    /// Adds a fetched run of blocks, keeping within the memory budget
    /// around `keep`. Returns block `keep`, if the run had it.
    fn add_chunk(
        &mut self,
        chunk: Vec<ChainBlock>,
        keep: u64,
        budget_mb: u32,
    ) -> Option<Arc<ChainBlock>> {
        if chunk.is_empty() {
            return None;
        }
        let sizes: Vec<usize> = chunk
            .iter()
            .map(|b| {
                b.txs
                    .iter()
                    .map(|tx| monero::consensus::encode::serialize(tx).len())
                    .sum()
            })
            .collect();
        self.avg_bytes_per_block = crate::scanner::update_avg_bytes_per_block(
            self.avg_bytes_per_block,
            sizes.iter().sum(),
            chunk.len(),
        );
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
        kept
    }
}

/// Block `height`, fetched with the blocks after it (up to `end`) in one
/// call sized to the scan memory budget.
async fn block(
    round: &mut Round<'_>,
    height: u64,
    end: u64,
) -> Result<Arc<ChainBlock>, ScannerError> {
    if let Some((block, _)) = round.blocks.cache.blocks.get(&height) {
        return Ok(block.clone());
    }
    let budget = u64::from(round.inputs.scan_chunk_memory_budget_mb).saturating_mul(1024 * 1024);
    let count = crate::scanner::next_scan_chunk_size(
        budget,
        round.blocks.cache.avg_bytes_per_block,
        end.saturating_sub(height) + 1,
    );
    let chunk = bounded(round.inputs.daemon.get_chain_blocks(height, count)).await?;
    // The block asked for is never evicted by its own fetch.
    round
        .blocks
        .cache
        .add_chunk(chunk, height, round.inputs.scan_chunk_memory_budget_mb)
        .ok_or_else(|| {
            ScannerError::Daemon(crate::daemon::DaemonError::Request(format!(
                "the node returned no block at height {height}"
            )))
        })
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
        }
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
