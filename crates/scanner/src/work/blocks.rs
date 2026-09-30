//! The blocks tier: scanning blocks for tenants (docs/scanner_microtasks.md).
//!
//! Tenants are grouped by their cursor (the highest block scanned for them).
//! A unit takes one group and scans the next few blocks for it: one fetch
//! and one hash check per block, then one view-key scan per tenant per
//! transaction. The *frontier* group (at the network's high-water mark)
//! scans new blocks; the others catch up, served round-robin from a
//! persisted position. While the frontier is behind, turns alternate between
//! it and catch-up, so neither starves the other.
//!
//! A block's results stay in memory until the whole block is scanned and
//! its hash rechecked, then commit in one transaction with the cursor move.
//! Only if a unit runs out of time partway through a block is its progress
//! written down (a checkpoint with staged matches), to resume from.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use monero::Transaction;
use tokio::time::Instant;

use crate::key_custody::{ScanIndices, WalletHandle};
use crate::scanner::{record_scan_match, scan_for_tenants, stage_block_match, ScanResult, ScannerError, SCAN_CONCURRENCY};
use crate::store::{BlockCheckpoint, Position, Store};

use super::{bounded, Progress, Round};

/// Most tenants of one group scanned by one unit. The rest of the group
/// stays at its cursor and is served by later units.
const GROUP_PAGE: usize = 256;
/// Most blocks one unit scans for its group before yielding the tier.
const BLOCKS_PER_UNIT: usize = 8;

/// Proof that a block was scanned in full for a tenant, with the results.
/// Moving a tenant's cursor takes one (`Store::advance_scanned_cursor`), and
/// only this module can build one: after scanning every transaction of the
/// block for that tenant and rechecking the block's hash.
pub struct ScannedBlock {
    tenant_id: String,
    height: u64,
    scans: Vec<ScanResult>,
}

impl ScannedBlock {
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    pub fn height(&self) -> u64 {
        self.height
    }
}

/// Kept across rounds: whose turn it is while the frontier is behind. Across
/// rounds, not per round, so a round with time for one unit alternates too.
#[derive(Default)]
pub(crate) struct BlockState {
    catch_up_turn: std::sync::atomic::AtomicBool,
}

pub(crate) struct BlocksRound {
    repaired: bool,
    frontier_done: bool,
    /// Catch-up groups served this round; a group seen again means the
    /// rotation has come full circle.
    visited: HashSet<u64>,
    last_group: Option<u64>,
    /// Parent-block timestamps read this round, by height.
    block_times: HashMap<u64, i64>,
    cache: BlockCache,
}

impl Default for BlocksRound {
    fn default() -> Self {
        Self {
            repaired: false,
            frontier_done: false,
            visited: HashSet::new(),
            last_group: None,
            block_times: HashMap::new(),
            cache: BlockCache { blocks: BTreeMap::new(), bytes: 0, avg_bytes_per_block: crate::scanner::SCAN_CHUNK_INITIAL_AVG_BYTES },
        }
    }
}

/// Block bodies fetched this round, bounded by the scan memory budget. Never
/// kept across rounds: a body is only trusted with the hashes read from the
/// same (pinned) node in the same round.
struct BlockCache {
    blocks: BTreeMap<u64, (Arc<Vec<Transaction>>, usize)>,
    bytes: usize,
    avg_bytes_per_block: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Frontier,
    CatchUp,
}

enum BlockOutcome {
    Committed,
    /// Out of time partway through; progress is checkpointed.
    Interrupted,
    /// The block doesn't match the stored chain: a reorg the chain tier
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
                format!("blocks-node:{}", round.network()),
                warn,
                network = %round.network(),
                error = %error,
                "block scanning stopped: the node failed. If this repeats, no payment on this network is being detected"
            );
            Progress::Blocked("the node failed")
        }
        Err(error) => Progress::Failed(error),
    }
}

async fn run(round: &mut Round<'_>, until: Instant) -> Result<Progress, ScannerError> {
    let Some(tip) = round.tip else { return Ok(Progress::Blocked("chain height unknown")) };
    let network = round.network().to_string();
    let store = round.inputs.store;
    if store.lock().reorg_job(&network)?.is_some() {
        return Ok(Progress::Blocked("a reorganisation is being reconciled"));
    }
    if round.chain.rewound {
        return Ok(Progress::Blocked("rewound this round; replacement blocks are scanned from the next"));
    }
    let Some(high_water) = store.lock().max_scanned_height(&network)? else {
        return seed(round, tip).await;
    };
    if !round.blocks.repaired {
        // Cheap repairs whatever happened before: no cursor ahead of the
        // network, new tenants anchored at it, disabled ones moved along.
        let s = store.lock();
        s.clamp_cursors(&network, Some(high_water))?;
        s.anchor_unset_cursors(&network, high_water)?;
        s.snap_disabled_cursors(&network, high_water)?;
        drop(s);
        round.blocks.repaired = true;
    }

    use std::sync::atomic::Ordering::Relaxed;
    let frontier_behind = !round.blocks.frontier_done && high_water < tip;
    let turn = &round.state.blocks.catch_up_turn;
    if frontier_behind && !turn.load(Relaxed) {
        turn.store(true, Relaxed);
        advance_group(round, Group::Frontier, high_water, tip, until).await?;
        return Ok(Progress::Advanced);
    }
    turn.store(false, Relaxed);
    if let Some(cursor) = next_catch_up_group(round, high_water)? {
        // Tenants with nothing that could ever have been paid need no block
        // read to decide: straight to the high-water mark.
        store.lock().advance_idle_cursors(&network, cursor, high_water, i64::MIN / 2, 0)?;
        advance_group(round, Group::CatchUp, cursor, tip, until).await?;
        return Ok(Progress::Advanced);
    }
    if frontier_behind {
        advance_group(round, Group::Frontier, high_water, tip, until).await?;
        return Ok(Progress::Advanced);
    }
    round.blocks.frontier_done = true;
    Ok(Progress::Idle)
}

/// First run on a network: start just below the node's tip rather than
/// replaying history (a payment gateway watches for new payments). One block
/// of margin, for a node reporting a tip it can't serve yet.
async fn seed(round: &mut Round<'_>, tip: u64) -> Result<Progress, ScannerError> {
    let network = round.network();
    let seed = tip.saturating_sub(1);
    match bounded(round.inputs.daemon.get_block_hash(seed)).await {
        Ok(hash) => {
            let s = round.inputs.store.lock();
            s.in_transaction(|s| -> Result<(), ScannerError> {
                s.set_scanned_block(network, seed, &hash)?;
                s.anchor_unset_cursors(network, seed)?;
                Ok(())
            })?;
            tracing::info!(network = %network, height = seed, "started scanning this network");
            Ok(Progress::Advanced)
        }
        Err(_) => Ok(Progress::Blocked("the node can't serve its own tip yet")),
    }
}

/// The next catch-up group after the last one served (wrapping), or `None`
/// once the rotation has come round to a group already served this round.
fn next_catch_up_group(round: &mut Round<'_>, high_water: u64) -> Result<Option<u64>, ScannerError> {
    let network = round.network();
    let s = round.inputs.store.lock();
    let after = match round.blocks.last_group {
        Some(last) => Some(last),
        None => s.scheduler_position(network, Position::CatchUpGroup)?.and_then(|v| v.parse().ok()),
    };
    let mut next = s.scan_group_cursors(network, high_water, after, 1)?;
    if next.is_empty() && after.is_some() {
        next = s.scan_group_cursors(network, high_water, None, 1)?;
    }
    let Some(&group) = next.first() else { return Ok(None) };
    if !round.blocks.visited.insert(group) {
        return Ok(None);
    }
    s.set_scheduler_position(network, Position::CatchUpGroup, &group.to_string())?;
    round.blocks.last_group = Some(group);
    Ok(Some(group))
}

/// Scans up to `BLOCKS_PER_UNIT` blocks for the group at `cursor`, one
/// after another, as far as its time allows (always at least one step of
/// progress). The frontier stops at the tip; catch-up stops at the
/// network's high-water mark, where it joins the frontier.
async fn advance_group(round: &mut Round<'_>, group: Group, cursor: u64, tip: u64, until: Instant) -> Result<(), ScannerError> {
    let mut cursor = cursor;
    for scanned in 0..BLOCKS_PER_UNIT {
        let high_water = round.inputs.store.lock().max_scanned_height(round.network())?.unwrap_or(cursor);
        let end = match group {
            Group::Frontier => tip,
            Group::CatchUp => high_water,
        };
        if cursor >= end || (scanned > 0 && Instant::now() >= until) {
            break;
        }
        match scan_block(round, group, cursor, high_water, end, until, scanned == 0).await? {
            BlockOutcome::Committed => cursor += 1,
            BlockOutcome::Interrupted => return Ok(()),
            BlockOutcome::Diverged(reason) => {
                tracing::warn!(network = %round.network(), height = cursor + 1, reason, "block differs from the stored chain; waiting for reorg reconciliation");
                return Ok(());
            }
        }
    }
    if group == Group::Frontier && cursor >= tip {
        round.blocks.frontier_done = true;
    }
    Ok(())
}

/// Scans block `parent + 1` for the tenants at cursor `parent`.
#[allow(clippy::too_many_arguments)]
async fn scan_block(
    round: &mut Round<'_>,
    group: Group,
    parent: u64,
    high_water: u64,
    end: u64,
    until: Instant,
    must_progress: bool,
) -> Result<BlockOutcome, ScannerError> {
    let height = parent + 1;
    let network = round.network().to_string();
    let store = round.inputs.store;
    let grace = round.inputs.grace_period_seconds;

    // Which orders could have been paid in this block: as of now for a new
    // block, as of the parent block's time when catching up, so an order
    // that closed during the gap is still looked for.
    let since = if height > high_water { round.now } else { block_time(round, parent).await?.min(round.now) };
    let waiting = round.state.backoff.waiting();
    let members = store.lock().tenants_at_cursor(&network, parent, &waiting, GROUP_PAGE)?;
    let mut scannable: Vec<(String, WalletHandle, ScanIndices)> = Vec::new();
    for tenant_id in members {
        let window = store.lock().scan_window(&tenant_id, since, grace)?;
        if window.is_empty() {
            continue; // nothing to find: moved along by `advance_idle_cursors`
        }
        if let Some(handle) = round.handles.get(tenant_id.as_str()) {
            scannable.push((tenant_id, *handle, ScanIndices::new(window)));
        }
        // No registered keys: it stays at its cursor until it has some.
    }

    let hash = bounded(round.inputs.daemon.get_block_hash(height)).await?;
    if store.lock().get_scanned_block_hash(&network, height)?.is_some_and(|stored| stored != hash) {
        return Ok(BlockOutcome::Diverged("the node's block differs from the one recorded"));
    }

    let mut scans: HashMap<String, Vec<ScanResult>> = HashMap::new();
    let mut next_tx: HashMap<String, usize> = HashMap::new();
    let mut failed: HashSet<String> = HashSet::new();
    if !scannable.is_empty() {
        let txs = block_transactions(round, height, end).await?;
        for (tenant_id, _, _) in &scannable {
            let resume = match store.lock().block_checkpoint(&network, tenant_id)? {
                Some(checkpoint) if checkpoint.height == height && checkpoint.hash == hash => checkpoint.next_tx.min(txs.len()),
                _ => 0,
            };
            next_tx.insert(tenant_id.clone(), resume);
        }
        let mut progressed = !must_progress;
        for (index, tx) in txs.iter().enumerate() {
            let due: Vec<&(String, WalletHandle, ScanIndices)> = scannable
                .iter()
                .filter(|(id, _, _)| !failed.contains(id) && next_tx.get(id).is_some_and(|next| *next <= index))
                .collect();
            for batch in due.chunks(SCAN_CONCURRENCY) {
                if progressed && Instant::now() >= until {
                    checkpoint(store, &network, height, &hash, &next_tx, &mut scans, &failed, round.now)?;
                    return Ok(BlockOutcome::Interrupted);
                }
                for (tenant_id, result) in scan_for_tenants(round.inputs.custody, tx, batch).await {
                    match result {
                        Ok(scan) => {
                            if !scan.matches.is_empty() {
                                scans.entry(tenant_id.clone()).or_default().push(scan);
                            }
                            next_tx.insert(tenant_id, index + 1);
                        }
                        Err(error) => {
                            shared::throttled!(format!("block-scan:{tenant_id}"), warn, store.id = %tenant_id, network = %network,
                                height, error = %error, "scanning a block failed for this store; it is caught up later");
                            round.state.backoff.failed(&tenant_id);
                            failed.insert(tenant_id);
                        }
                    }
                }
                progressed = true;
            }
        }
        // The block may have changed while it was scanned.
        let recheck = bounded(round.inputs.daemon.get_block_hash(height)).await?;
        if recheck != hash {
            return Ok(BlockOutcome::Diverged("the block changed while it was scanned"));
        }
    }

    let scanned: Vec<ScannedBlock> = scannable
        .into_iter()
        .filter(|(tenant_id, _, _)| !failed.contains(tenant_id))
        .map(|(tenant_id, _, _)| {
            round.state.backoff.succeeded(&tenant_id);
            ScannedBlock { scans: scans.remove(&tenant_id).unwrap_or_default(), tenant_id, height }
        })
        .collect();
    // Tenants with nothing in scope: to this block on the frontier, straight
    // to the high-water mark when catching up (nothing could have been paid
    // to them in the whole gap).
    let idle_to = match group {
        Group::Frontier => height,
        Group::CatchUp => high_water,
    };
    let s = store.lock();
    let committed = commit(&s, &network, height, &hash, parent, idle_to, since, grace, scanned, round.now)?;
    Ok(if committed { BlockOutcome::Committed } else { BlockOutcome::Diverged("the recorded block changed before commit") })
}

/// Records how far each tenant got through the block, with its matches
/// staged, so the next unit resumes there.
#[allow(clippy::too_many_arguments)]
fn checkpoint(
    store: &crate::store::SharedStore,
    network: &str,
    height: u64,
    hash: &str,
    next_tx: &HashMap<String, usize>,
    scans: &mut HashMap<String, Vec<ScanResult>>,
    failed: &HashSet<String>,
    now: i64,
) -> Result<(), ScannerError> {
    store.lock().in_transaction(|s| -> Result<(), ScannerError> {
        for (tenant_id, next) in next_tx {
            if failed.contains(tenant_id) || *next == 0 {
                continue;
            }
            s.save_block_checkpoint(network, tenant_id, &BlockCheckpoint { height, hash: hash.to_string(), next_tx: *next })?;
            for scan in scans.remove(tenant_id).unwrap_or_default() {
                stage_block_match(s, network, tenant_id, &scan, now)?;
            }
        }
        Ok(())
    })
}

/// One transaction: record the block for the network (if it is the next
/// one), and for each tenant it was scanned for, move the cursor and record
/// its payments, staged and new. Idle tenants at the parent move along. The
/// payments' recompute obligations are left by the payment triggers.
///
/// `false` (nothing written) if the recorded hash for this height changed.
#[allow(clippy::too_many_arguments)]
fn commit(
    s: &Store,
    network: &str,
    height: u64,
    hash: &str,
    parent: u64,
    idle_to: u64,
    since: i64,
    grace: i64,
    scanned: Vec<ScannedBlock>,
    now: i64,
) -> Result<bool, ScannerError> {
    s.in_transaction(|s| -> Result<bool, ScannerError> {
        match s.get_scanned_block_hash(network, height)? {
            Some(stored) if stored != hash => return Ok(false),
            Some(_) => {}
            None => {
                if s.max_scanned_height(network)?.is_none_or(|max| max + 1 == height) {
                    s.set_scanned_block(network, height, hash)?;
                }
            }
        }
        for block in &scanned {
            if !s.advance_scanned_cursor(network, block)? {
                // Moved meanwhile (a rewind): these results are for a chain
                // it no longer stands on.
                s.take_staged_payments(network, &block.tenant_id, height, hash)?;
                continue;
            }
            for staged in s.take_staged_payments(network, &block.tenant_id, height, hash)? {
                s.record_payment_match(
                    &staged.order_id,
                    &staged.txid,
                    staged.output_index,
                    staged.amount_piconero,
                    &staged.key_images_json,
                    staged.seen_at,
                    Some(height as i64),
                )?;
            }
            for scan in &block.scans {
                record_scan_match(s, &block.tenant_id, scan, now, Some(height))?;
            }
        }
        s.advance_idle_cursors(network, parent, idle_to, since, grace)?;
        Ok(true)
    })
}

/// The timestamp of block `height`, read once per round.
async fn block_time(round: &mut Round<'_>, height: u64) -> Result<i64, ScannerError> {
    if let Some(time) = round.blocks.block_times.get(&height) {
        return Ok(*time);
    }
    let time = bounded(round.inputs.daemon.get_block_timestamp(height)).await? as i64;
    round.blocks.block_times.insert(height, time);
    Ok(time)
}

/// Block `height`'s transactions, fetched with the blocks after it (up to
/// `end`) in one call sized to the scan memory budget.
async fn block_transactions(round: &mut Round<'_>, height: u64, end: u64) -> Result<Arc<Vec<Transaction>>, ScannerError> {
    if let Some((txs, _)) = round.blocks.cache.blocks.get(&height) {
        return Ok(txs.clone());
    }
    let budget = (round.inputs.scan_chunk_memory_budget_mb as u64).saturating_mul(1024 * 1024);
    let cache = &mut round.blocks.cache;
    let count = crate::scanner::next_scan_chunk_size(budget, cache.avg_bytes_per_block, end.saturating_sub(height) + 1);
    let chunk = bounded(round.inputs.daemon.get_blocks_range(height, count)).await?;
    if chunk.is_empty() {
        return Err(ScannerError::Internal(format!("the node returned no blocks from height {height}")));
    }
    let sizes: Vec<usize> =
        chunk.iter().map(|txs| txs.iter().map(|tx| monero::consensus::encode::serialize(tx).len()).sum()).collect();
    cache.avg_bytes_per_block =
        crate::scanner::update_avg_bytes_per_block(cache.avg_bytes_per_block, sizes.iter().sum(), chunk.len());
    // Blocks below this one are done with; drop them first.
    let done: Vec<u64> = cache.blocks.range(..height).map(|(h, _)| *h).collect();
    for h in done {
        if let Some((_, bytes)) = cache.blocks.remove(&h) {
            cache.bytes -= bytes;
        }
    }
    for (offset, (txs, bytes)) in chunk.into_iter().zip(sizes).enumerate() {
        let h = height + offset as u64;
        if offset > 0 && cache.bytes + bytes > budget as usize {
            break;
        }
        cache.bytes += bytes;
        cache.blocks.insert(h, (Arc::new(txs), bytes));
    }
    Ok(cache.blocks.get(&height).map(|(txs, _)| txs.clone()).unwrap_or_default())
}
