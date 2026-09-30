//! The settlement tier: payments that left the mempool without being mined,
//! and order status recomputes.
//!
//! Recompute serves two queues, both bounded pages:
//! - obligations (`pending_payment_recomputes`): a payment changed;
//! - due orders (`orders.next_due_*`): time or height moved an order's
//!   status, earliest due first.
//!
//! Each order is recomputed at most once per round, so an obligation that
//! has to wait (a settlement during a reorg) can't spin the tier.

use std::collections::HashSet;

use tokio::time::Instant;

use crate::scanner::{check_vanished_candidates, recompute_and_notify, ScannerError};
use crate::store::Position;

use super::{Progress, Round};

const VANISHED_PAGE: usize = 64;
const RECOMPUTE_PAGE: usize = 64;
/// How long one vanished payment's lookups may take.
const VANISHED_CALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Default)]
pub(crate) struct SettlementState {
    obligations_after: parking_lot::Mutex<String>,
}

#[derive(Default)]
pub(crate) struct SettlementRound {
    vanished_done: bool,
    recomputed: HashSet<String>,
}

/// One unit: the round's vanished-payment page (first unit only), then a
/// page of recomputes. Both in one unit, so even a round with no time to
/// spare recomputes something.
pub(super) async fn step(round: &mut Round<'_>, until: Instant) -> Progress {
    let Some(tip) = round.tip else { return Progress::Blocked("chain height unknown") };
    let mut failure = None;
    if !round.settlement.vanished_done {
        round.settlement.vanished_done = true;
        if let Err(error) = vanished(round, tip, until).await {
            failure = Some(error);
        }
    }
    let recomputed = match recompute_page(round, tip) {
        Ok(count) => count,
        Err(error) => return Progress::Failed(error),
    };
    match (failure, recomputed) {
        (Some(error), _) => Progress::Failed(error),
        (None, 0) => Progress::Idle,
        (None, _) => Progress::Advanced,
    }
}

/// The blind spot reorg detection can't cover: a zero-conf payment whose
/// transaction quietly left the pool because a conflicting one won. One
/// page of unconfirmed payments, rotating from a persisted position; only
/// on a round whose mempool poll succeeded (never "we couldn't look" read
/// as "it's gone"). A failed row stays eligible on the next circuit; the
/// position moves past it so the others make progress.
async fn vanished(round: &mut Round<'_>, tip: u64, until: Instant) -> Result<(), ScannerError> {
    let Some(txids) = round.pool_txids.clone() else { return Ok(()) };
    let network = round.network().to_string();
    let store = round.inputs.store;
    let after: i64 = store.lock().scheduler_position(&network, Position::VanishedPayments)?.and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut page = store.lock().unconfirmed_payments_page(&network, after, VANISHED_PAGE)?;
    if page.is_empty() && after != 0 {
        page = store.lock().unconfirmed_payments_page(&network, 0, VANISHED_PAGE)?;
    }
    if page.is_empty() {
        store.lock().set_scheduler_position(&network, Position::VanishedPayments, "0")?;
        return Ok(());
    }
    let mut failure = None;
    for (checked, (id, payment)) in page.into_iter().enumerate() {
        if checked > 0 && Instant::now() >= until {
            break;
        }
        let checked = tokio::time::timeout(
            VANISHED_CALL_DEADLINE,
            check_vanished_candidates(store, round.inputs.daemon, &txids, tip, round.now, vec![payment]),
        )
        .await;
        match checked {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(network = %network, error = %error, "checking a vanished mempool payment failed (retried)");
                failure.get_or_insert(error);
            }
            Err(_) => {
                tracing::warn!(network = %network, "checking a vanished mempool payment took too long (retried)");
                failure.get_or_insert(ScannerError::Internal("vanished mempool lookup exceeded its deadline".into()));
            }
        }
        store.lock().set_scheduler_position(&network, Position::VanishedPayments, &id.to_string())?;
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Recomputes up to a page of orders not yet recomputed this round:
/// obligations first, then due orders. Returns how many.
fn recompute_page(round: &mut Round<'_>, tip: u64) -> Result<usize, ScannerError> {
    let network = round.network().to_string();
    let store = round.inputs.store;
    let mut ids: Vec<String> = Vec::new();
    {
        let s = store.lock();
        // Obligations rotate from a remembered position, wrapping once.
        let mut after = round.state.settlement.obligations_after.lock();
        let recomputed = &round.settlement.recomputed;
        let take = |page: Vec<String>, ids: &mut Vec<String>| {
            for id in page {
                if ids.len() < RECOMPUTE_PAGE && !recomputed.contains(&id) && !ids.contains(&id) {
                    ids.push(id);
                }
            }
        };
        let page = s.pending_payment_recomputes_page(&network, &after, RECOMPUTE_PAGE)?;
        let full = page.len() == RECOMPUTE_PAGE;
        let wrap = !full && !after.is_empty();
        *after = if full { page.last().cloned().unwrap_or_default() } else { String::new() };
        take(page, &mut ids);
        if wrap {
            take(s.pending_payment_recomputes_page(&network, "", RECOMPUTE_PAGE)?, &mut ids);
        }
        drop(after);
        if ids.len() < RECOMPUTE_PAGE {
            let due = s.due_order_ids(&network, round.now, tip, RECOMPUTE_PAGE + round.settlement.recomputed.len())?;
            for id in due {
                if ids.len() < RECOMPUTE_PAGE && !round.settlement.recomputed.contains(&id) && !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    for id in &ids {
        round.settlement.recomputed.insert(id.clone());
        let s = store.lock();
        recompute_and_notify(&s, id, tip, round.now)?;
    }
    Ok(ids.len())
}
