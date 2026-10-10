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

use crate::scanner::{
    check_vanished_candidates, recompute_and_notify, vanished_hints, ScannerError,
};
use crate::store::position::VanishedPayments;

use shared::activity::Event;

use super::{Progress, Round, Wait};

const VANISHED_PAGE: usize = 64;
const RECOMPUTE_PAGE: usize = 64;
const RECOMPUTES_PER_JOB: usize = 16;

#[derive(Default)]
pub(crate) struct SettlementState {
    obligations_after: parking_lot::Mutex<String>,
    /// Payments (by id) whose transaction is nowhere and isn't proven
    /// double-spent: looked at again at once, then at lengthening
    /// intervals up to a minute, instead of every round for as long as
    /// they stay that way.
    unresolved: super::Backoff<i64>,
}

#[derive(Default)]
pub(crate) struct SettlementRound {
    tip_observed: bool,
    vanished_done: bool,
    recomputed: HashSet<crate::store::OrderId>,
}

/// One unit: the round's vanished-payment page (first unit only), then a
/// page of recomputes. Both in one unit, so even a round with no time to
/// spare recomputes something.
pub(super) async fn step(round: &mut Round<'_>, until: Instant) -> Progress {
    let Some(tip) = round.tip else {
        return Progress::Blocked(Wait::ChainHeightUnknown);
    };
    if !round.settlement.tip_observed {
        if let Err(error) = round
            .db(move |s, network| s.observe_settlement_tip(network, tip))
            .await
        {
            return Progress::Failed(error);
        }
        round.settlement.tip_observed = true;
    }
    let mut failure = None;
    if !round.settlement.vanished_done {
        round.settlement.vanished_done = true;
        if let Err(error) = vanished(round, tip, until).await {
            failure = Some(error);
        }
    }
    let recomputed = match recompute_page(round, tip).await {
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
///
/// The node is asked about the whole page at once where it can answer that
/// way (`engine::vanished_hints`): where the transactions are in one round
/// trip, and the key images of those that are nowhere in another. A payment
/// that stays nowhere and unproven (dropped, evicted) is then looked at
/// less and less often, up to once a minute.
async fn vanished(round: &Round<'_>, tip: u64, until: Instant) -> Result<(), ScannerError> {
    let Some(txids) = round.pool_txids.clone() else {
        return Ok(());
    };
    // One payment's lookups may take the settlement tier's share of a
    // round, so they can't hold up the tiers after it for longer than the
    // tier was given.
    let vanished_deadline = round.state.tuning().reserved(super::Tier::Settlement);
    let network = round.network();
    let db = round.inputs.db;
    let page = round
        .db(|s, network| -> Result<_, ScannerError> {
            let after: i64 = s
                .scheduler_position::<VanishedPayments>(network)?
                .unwrap_or(0);
            let mut page = s.unconfirmed_payments_page(network, after, VANISHED_PAGE)?;
            if page.is_empty() && after != 0 {
                page = s.unconfirmed_payments_page(network, 0, VANISHED_PAGE)?;
            }
            if page.is_empty() {
                s.set_scheduler_position::<VanishedPayments>(network, &0)?;
            }
            Ok(page)
        })
        .await?;
    if page.is_empty() {
        return Ok(());
    }
    round.state.activity().record(Event::Vanished {
        looked: super::count(page.len()),
    });
    let unresolved = &round.state.settlement.unresolved;
    // (Also forgets payments that stopped being looked at long ago.)
    let waiting: HashSet<i64> = unresolved.waiting().into_iter().collect();
    let hints = {
        let due: Vec<_> = page
            .iter()
            .map(|(_, payment)| payment)
            .filter(|payment| !waiting.contains(&payment.id))
            .collect();
        tokio::time::timeout(
            vanished_deadline,
            crate::daemon_fallback::with_corroboration_deadline(
                Instant::now() + vanished_deadline,
                1,
                vanished_hints(round.inputs.daemon, &txids, &due),
            ),
        )
        .await
    };
    // Hints are an optimisation: without them (a slow node, or one that
    // can't batch) each payment is asked about on its own below, within its
    // own deadline, and the position still moves.
    let hints = match hints {
        Ok(Ok(hints)) => hints,
        Ok(Err(error)) => {
            tracing::warn!(network = crate::network::network_str(network), error = %error, "asking about a page of vanished mempool payments at once failed; asking one by one");
            crate::scanner::VanishedHints::default()
        }
        Err(_) => {
            tracing::warn!(
                network = crate::network::network_str(network),
                "asking about a page of vanished mempool payments at once took too long; asking one by one"
            );
            crate::scanner::VanishedHints::default()
        }
    };
    // Until the time runs out (at least one), or the node fails: a node that
    // fails or hangs for one payment would for the next. Its failures are
    // retried, not reported; a storage failure is.
    let mut last = None;
    let mut failure = None;
    for (id, payment) in page {
        if last.is_some() && Instant::now() >= until {
            break;
        }
        if waiting.contains(&payment.id) {
            last = Some(id);
            continue;
        }
        let payment_id = payment.id;
        let checked = tokio::time::timeout(
            vanished_deadline,
            crate::daemon_fallback::with_corroboration_deadline(
                Instant::now() + vanished_deadline,
                2,
                check_vanished_candidates(
                    db,
                    round.inputs.daemon,
                    &txids,
                    tip,
                    round.now,
                    vec![payment],
                    &hints,
                ),
            ),
        )
        .await;
        match checked {
            Ok(Ok(report)) => {
                if report.unresolved.contains(&payment_id) {
                    unresolved.failed(&payment_id);
                } else {
                    unresolved.succeeded(&payment_id);
                }
                last = Some(id);
            }
            Ok(Err(ScannerError::Daemon(error))) => {
                tracing::warn!(network = crate::network::network_str(network), error = %error, "checking a vanished mempool payment failed (retried)");
                break;
            }
            Ok(Err(error)) => {
                failure = Some(error);
                break;
            }
            Err(_) => {
                tracing::warn!(
                    network = crate::network::network_str(network),
                    "checking a vanished mempool payment took too long (retried)"
                );
                break;
            }
        }
    }
    // The position moves past what was checked, once per page.
    if let Some(last) = last {
        round
            .db(move |s, network| s.set_scheduler_position::<VanishedPayments>(network, &last))
            .await?;
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Recomputes up to a page of orders not yet recomputed this round:
/// obligations first, then due orders. Returns how many.
async fn recompute_page(round: &mut Round<'_>, tip: u64) -> Result<usize, ScannerError> {
    let now = round.now;
    let after = round.state.settlement.obligations_after.lock().clone();
    // Skipped: orders recomputed this round, and orders waiting to retry.
    let mut skip = round.settlement.recomputed.clone();
    skip.extend(round.state.order_backoff.waiting());
    let (ids, next_after) = round
        .db(move |s, network| pick(s, network, &after, &skip, now, tip))
        .await?;
    *round.state.settlement.obligations_after.lock() = next_after;
    round.settlement.recomputed.extend(ids.iter().cloned());
    let count = ids.len();
    // Each recompute is its own transaction, a few per database job so a page
    // never holds the worker long, and each order's failure is its own: it
    // waits to be retried, and the others carry on.
    let mut succeeded = 0;
    let mut first_failure = None;
    let mut transitions = Vec::new();
    for chunk in ids.chunks(RECOMPUTES_PER_JOB) {
        let chunk = chunk.to_vec();
        let outcomes = round
            .db(move |s, _| {
                let outcomes = chunk.into_iter().map(|id| {
                    let outcome = recompute_and_notify(s, &id, tip, now);
                    (id, outcome)
                });
                Ok::<_, ScannerError>(outcomes.collect::<Vec<_>>())
            })
            .await?;
        for (id, outcome) in outcomes {
            match outcome {
                Ok(transition) => {
                    succeeded += 1;
                    transitions.extend(transition);
                    round.state.order_backoff.succeeded(&id);
                }
                Err(error) => {
                    tracing::warn!(network = crate::network::network_str(round.network()), order.id = %id, error = %error, "recomputing an order's status failed (retried later)");
                    round.state.order_backoff.failed(&id);
                    first_failure.get_or_insert(error);
                }
            }
        }
    }
    if count > 0 {
        round.state.activity().record(Event::Recomputed {
            orders: super::count(count),
            transitions,
        });
    }
    match first_failure {
        // Only when nothing in the page went through does the tier stop.
        Some(error) if succeeded == 0 => Err(error),
        _ => Ok(count),
    }
}

/// The next page to recompute, and where the obligation rotation goes on
/// from: obligations first (from `after`, wrapping once), then due orders,
/// skipping anything already recomputed this round.
fn pick(
    s: &crate::store::Store,
    network: monero::Network,
    after: &str,
    recomputed: &HashSet<crate::store::OrderId>,
    now: i64,
    tip: u64,
) -> Result<(Vec<crate::store::OrderId>, String), ScannerError> {
    let mut ids: Vec<crate::store::OrderId> = Vec::new();
    let take = |page: Vec<crate::store::OrderId>, ids: &mut Vec<crate::store::OrderId>| {
        for id in page {
            if ids.len() < RECOMPUTE_PAGE && !recomputed.contains(&id) && !ids.contains(&id) {
                ids.push(id);
            }
        }
    };
    let page = s.pending_payment_recomputes_page(network, after, RECOMPUTE_PAGE)?;
    let full = page.len() == RECOMPUTE_PAGE;
    let wrap = !full && !after.is_empty();
    let next_after = if full {
        page.last().map_or_default(ToString::to_string)
    } else {
        String::new()
    };
    take(page, &mut ids);
    if wrap {
        take(
            s.pending_payment_recomputes_page(network, "", RECOMPUTE_PAGE)?,
            &mut ids,
        );
    }
    if ids.len() < RECOMPUTE_PAGE {
        take(
            s.due_order_ids(network, now, tip, RECOMPUTE_PAGE + recomputed.len())?,
            &mut ids,
        );
    }
    Ok((ids, next_after))
}
