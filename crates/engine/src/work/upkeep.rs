//! The upkeep tier: bookkeeping that may lag without harming correctness.
//!
//! - Pruning: nothing reads a stored block hash from further back than the
//!   reorg window, so older rows go. Measured from the scanner's own
//!   high-water mark, never the node's reported height, so a node claiming
//!   an absurd tip can't talk the scanner into deleting the window it needs.
//! - Scanned ranges (`docs/order_rescan_wbs.md` Phase 5.1): each in-scope
//!   order shows the range of blocks scanned for it, up to its store's
//!   cursor. A rotating page of stores per unit.
//! - Void recheck: payments voided as double-spent in the last two days are
//!   rechecked every few minutes against fresh, corroborated key-image
//!   evidence, and restored if it no longer supports the accusation
//!   (`docs/DESIGN.md` §7.7). One page per round, from a persisted position.

use crate::scanner::{
    recheck_voided_payment, voided_key_image_statuses, ScannerError,
    DOUBLE_SPEND_RECHECK_WINDOW_SECS,
};
use crate::store::position::{ScanRange, VoidRecheck, VoidRecheckPassStarted};

use super::{Progress, Round};

const RANGE_PAGE: usize = 32;
const VOID_PAGE: usize = 16;
/// How often a full pass over recent voids starts.
const VOID_RECHECK_INTERVAL_SECS: i64 = 5 * 60;

/// How often the write-ahead log is checkpointed.
const CHECKPOINT_INTERVAL: std::time::Duration = std::time::Duration::from_mins(10);

/// Kept across rounds: when the log was last checkpointed.
#[derive(Default)]
pub(crate) struct UpkeepState {
    last_checkpoint: parking_lot::Mutex<Option<tokio::time::Instant>>,
}

impl UpkeepState {
    /// Whether a checkpoint is due (and, if so, marks it done now).
    fn checkpoint_due(&self) -> bool {
        let mut last = self.last_checkpoint.lock();
        let now = tokio::time::Instant::now();
        let due = last.is_none_or(|at| now.saturating_duration_since(at) >= CHECKPOINT_INTERVAL);
        if due {
            *last = Some(now);
        }
        due
    }
}

#[derive(Default)]
pub(crate) struct UpkeepRound {
    first_done: bool,
    ranges_done: bool,
}

/// The first unit of a round prunes, rechecks a page of voids and brings a
/// page of scanned ranges up to date; later units only continue the ranges.
/// So every kind of upkeep advances every round, however little time is left.
pub(super) async fn step(round: &mut Round<'_>, until: tokio::time::Instant) -> Progress {
    if round.upkeep.first_done && round.upkeep.ranges_done {
        return Progress::Idle;
    }
    let first = !round.upkeep.first_done;
    round.upkeep.first_done = true;
    // Every piece runs even if an earlier one failed; the first failure is
    // the one reported.
    // (The ranges aren't done: a first unit hasn't started them, and a later
    // one only runs while they aren't.)
    let mut outcomes = Vec::new();
    if first {
        outcomes.push(prune(round).await);
        outcomes.push(checkpoint(round).await);
        outcomes.push(recheck_voids(round, until).await);
    }
    outcomes.push(scanned_ranges(round).await);
    let result: Result<(), ScannerError> = outcomes.into_iter().collect();
    match result {
        Ok(()) => Progress::Advanced,
        Err(error) => Progress::Failed(error),
    }
}

async fn prune(round: &Round<'_>) -> Result<(), ScannerError> {
    let depth = round.inputs.reorg_check_depth;
    let pruned = round
        .db(move |s, network| -> Result<_, ScannerError> {
            Ok(match s.max_scanned_height(network)? {
                Some(high_water) => s.prune_scanned_blocks_below(
                    network,
                    high_water.saturating_sub(depth.saturating_mul(4)),
                )?,
                None => 0,
            })
        })
        .await?;
    round
        .state
        .activity()
        .record(shared::activity::Event::Upkeep { pruned });
    Ok(())
}

/// A passive WAL checkpoint every `CHECKPOINT_INTERVAL` (see
/// `Store::checkpoint_wal`).
async fn checkpoint(round: &Round<'_>) -> Result<(), ScannerError> {
    if !round.state.upkeep.checkpoint_due() {
        return Ok(());
    }
    let complete = round.db(|s, _| s.checkpoint_wal()).await?;
    tracing::debug!(complete, "checkpointed the write-ahead log");
    Ok(())
}

/// One page of stores' scanned-range bookkeeping. Each store's orders show
/// its own cursor, never the network's height: a store that is behind
/// hasn't been checked against the blocks above its cursor.
async fn scanned_ranges(round: &mut Round<'_>) -> Result<(), ScannerError> {
    let (grace, now) = (round.inputs.grace_period_seconds, round.now);
    let finished = round
        .db(move |s, network| {
            s.in_transaction(|s| -> Result<bool, ScannerError> {
                let after = s
                    .scheduler_position::<ScanRange>(network)?
                    .unwrap_or_default();
                let page = s.active_tenants_page(network, now, grace, &after, RANGE_PAGE)?;
                for (tenant_id, cursor) in &page {
                    if let Some(cursor) = cursor {
                        s.bump_scanned_heights_for_tenant(tenant_id, *cursor, now, grace)?;
                    }
                }
                let finished = page.len() < RANGE_PAGE;
                let next = if finished {
                    String::new()
                } else {
                    page.last().map_or_default(|(id, _)| id.to_string())
                };
                s.set_scheduler_position::<ScanRange>(network, &next)?;
                Ok(finished)
            })
        })
        .await?;
    if finished {
        round.upkeep.ranges_done = true;
    }
    Ok(())
}

/// One page of the recent-void recheck. A pass starts at most every
/// `VOID_RECHECK_INTERVAL_SECS` and walks voided payments in id order; a
/// failed or inconclusive recheck leaves the payment voided for the next
/// pass.
async fn recheck_voids(round: &Round<'_>, until: tokio::time::Instant) -> Result<(), ScannerError> {
    let Some(tip) = round.tip else { return Ok(()) };
    let now = round.now;
    let cutoff = now - DOUBLE_SPEND_RECHECK_WINDOW_SECS;
    let page = round
        .db(move |s, network| -> Result<_, crate::store::StoreError> {
            let started: i64 = s
                .scheduler_position::<VoidRecheckPassStarted>(network)?
                .unwrap_or(i64::MIN);
            let after: i64 = s.scheduler_position::<VoidRecheck>(network)?.unwrap_or(0);
            if after == 0 {
                if now.saturating_sub(started) < VOID_RECHECK_INTERVAL_SECS {
                    return Ok(Vec::new());
                }
                s.set_scheduler_position::<VoidRecheckPassStarted>(network, &now)?;
            }
            s.voided_payments_page(network, cutoff, after, VOID_PAGE)
        })
        .await?;
    // The whole page's key images in one round trip (per node); a payment
    // left out of the answer is asked about on its own. A node that fails
    // here would fail for each payment: the pass waits for the next round.
    let statuses = match voided_key_image_statuses(round.inputs.daemon, &page).await {
        Ok(statuses) => statuses,
        Err(ScannerError::Daemon(error)) => {
            tracing::warn!(network = crate::network::network_str(round.network()), payments = page.len(), error = %error,
                "double-spend revalidation: rechecking a voided payment failed - leaving it voided, retried next pass");
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    // Until the time runs out (at least one) or the node fails: a node that
    // fails for one payment would for the next, so stop asking it.
    let mut last = None;
    let mut failure = None;
    for payment in &page {
        if last.is_some() && tokio::time::Instant::now() >= until {
            break;
        }
        match recheck_voided_payment(
            round.inputs.db,
            round.inputs.daemon,
            round.network(),
            payment,
            tip,
            now,
            statuses.get(&payment.id).map(Vec::as_slice),
        )
        .await
        {
            Ok(_) => last = Some(payment.id),
            Err(error) => {
                tracing::warn!(network = crate::network::network_str(round.network()), order.id = %payment.order_id, error = %error,
                    "double-spend revalidation: rechecking a voided payment failed - leaving it voided, retried next pass");
                failure = Some(error);
                break;
            }
        }
    }
    // Back to 0 (the pass is over) once nothing follows what was checked.
    if let Some(last) = last {
        round
            .db(move |s, network| -> Result<_, crate::store::StoreError> {
                let next = if s.voided_payments_page(network, cutoff, last, 1)?.is_empty() {
                    0
                } else {
                    last
                };
                s.set_scheduler_position::<VoidRecheck>(network, &next)
            })
            .await?;
    }
    match failure {
        Some(ScannerError::Daemon(_)) | None => Ok(()),
        Some(error) => Err(error),
    }
}
