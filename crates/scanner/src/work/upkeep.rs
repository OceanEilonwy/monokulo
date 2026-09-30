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

use crate::scanner::{recheck_voided_payment, ScannerError, DOUBLE_SPEND_RECHECK_WINDOW_SECS};
use crate::store::Position;

use super::{Progress, Round};

const RANGE_PAGE: usize = 32;
const VOID_PAGE: usize = 16;
/// How often a full pass over recent voids starts.
const VOID_RECHECK_INTERVAL_SECS: i64 = 5 * 60;

#[derive(Default)]
pub(crate) struct UpkeepRound {
    first_done: bool,
    ranges_done: bool,
}

/// The first unit of a round prunes, rechecks a page of voids and brings a
/// page of scanned ranges up to date; later units only continue the ranges.
/// So every kind of upkeep advances every round, however little time is left.
pub(super) async fn step(round: &mut Round<'_>) -> Progress {
    if round.upkeep.first_done && round.upkeep.ranges_done {
        return Progress::Idle;
    }
    let first = !round.upkeep.first_done;
    round.upkeep.first_done = true;
    let mut result = Ok(());
    if first {
        result = prune(round).await.and(result);
        result = recheck_voids(round).await.and(result);
    }
    if !round.upkeep.ranges_done {
        result = scanned_ranges(round).await.and(result);
    }
    match result {
        Ok(()) => Progress::Advanced,
        Err(error) => Progress::Failed(error),
    }
}

async fn prune(round: &Round<'_>) -> Result<(), ScannerError> {
    let depth = round.inputs.reorg_check_depth;
    round
        .db(move |s, network| {
            if let Some(high_water) = s.max_scanned_height(network)? {
                s.prune_scanned_blocks_below(network, high_water.saturating_sub(depth.saturating_mul(4)))?;
            }
            Ok(())
        })
        .await
}

/// One page of stores' scanned-range bookkeeping. Each store's orders show
/// its own cursor, never the network's height: a store that is behind
/// hasn't been checked against the blocks above its cursor.
async fn scanned_ranges(round: &mut Round<'_>) -> Result<(), ScannerError> {
    let (grace, now) = (round.inputs.grace_period_seconds, round.now);
    let finished = round
        .db(move |s, network| {
            s.in_transaction(|s| -> Result<bool, ScannerError> {
                let after = s.scheduler_position(network, Position::ScanRange)?.unwrap_or_default();
                let page = s.active_tenants_page(network, now, grace, &after, RANGE_PAGE)?;
                for (tenant_id, cursor) in &page {
                    if let Some(cursor) = cursor {
                        s.bump_scanned_heights_for_tenant(tenant_id, *cursor, now, grace)?;
                    }
                }
                let finished = page.len() < RANGE_PAGE;
                let next = if finished { String::new() } else { page.last().map(|(id, _)| id.clone()).unwrap_or_default() };
                s.set_scheduler_position(network, Position::ScanRange, &next)?;
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
async fn recheck_voids(round: &mut Round<'_>) -> Result<(), ScannerError> {
    let Some(tip) = round.tip else { return Ok(()) };
    let now = round.now;
    let cutoff = now - DOUBLE_SPEND_RECHECK_WINDOW_SECS;
    let page = round
        .db(move |s, network| {
            let started: i64 =
                s.scheduler_position(network, Position::VoidRecheckPassStarted)?.and_then(|v| v.parse().ok()).unwrap_or(i64::MIN);
            let after: i64 = s.scheduler_position(network, Position::VoidRecheck)?.and_then(|v| v.parse().ok()).unwrap_or(0);
            if after == 0 {
                if now.saturating_sub(started) < VOID_RECHECK_INTERVAL_SECS {
                    return Ok(Vec::new());
                }
                s.set_scheduler_position(network, Position::VoidRecheckPassStarted, &now.to_string())?;
            }
            Ok(s.voided_payments_page(network, cutoff, after, VOID_PAGE)?)
        })
        .await?;
    let Some(last) = page.last().map(|p| p.id) else { return Ok(()) };
    for payment in &page {
        recheck_voided_payment(round.inputs.db, round.inputs.daemon, round.network(), payment, tip, now).await;
    }
    // Back to 0 (the pass is over) once nothing follows this page.
    round
        .db(move |s, network| {
            let next = if s.voided_payments_page(network, cutoff, last, 1)?.is_empty() { 0 } else { last };
            Ok(s.set_scheduler_position(network, Position::VoidRecheck, &next.to_string())?)
        })
        .await
}
