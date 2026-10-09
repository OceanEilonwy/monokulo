//! Chain scanning: matching transactions against active tenants, and reorg /
//! double-spend detection. See `docs/DESIGN.md` §7.
//!
//! Deliberately built against the `MoneroDaemonClient` trait rather than a live
//! node, and against `Store` directly rather than the (not-yet-built) writer-actor
//! wrapper - the correctness of the reconciliation logic doesn't depend on which
//! thread runs it, only on doing the right thing with what the daemon reports.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use monero::blockdata::transaction::TxOutTarget;
use monero::Transaction;

use crate::daemon::{DaemonError, KeyImageStatus, MoneroDaemonClient, TxLocation};
use crate::key_custody::{
    KeyCustody, KeyCustodyError, MatchedOutput, ScanIndices, ScanInput, WalletHandle,
};
use crate::store::{Store, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum ScannerError {
    #[error(transparent)]
    Daemon(#[from] DaemonError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    KeyCustody(#[from] KeyCustodyError),
    #[error("invalid stored payment key images: {0}")]
    InvalidPaymentEvidence(String),
    /// Something that should be impossible happened. Returned instead of a
    /// panic so the tick that hit it fails and retries, and the loop lives.
    #[error("internal error: {0}")]
    Internal(String),
}

type Result<T> = std::result::Result<T, ScannerError>;

pub(crate) fn parse_payment_key_images(raw: &str) -> Result<Vec<String>> {
    let images: Vec<String> = serde_json::from_str(raw)
        .map_err(|e| ScannerError::InvalidPaymentEvidence(e.to_string()))?;
    if images.is_empty() {
        return Err(ScannerError::InvalidPaymentEvidence(
            "empty key-image list".to_owned(),
        ));
    }
    if images
        .iter()
        .any(|image| image.len() != 64 || !image.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(ScannerError::InvalidPaymentEvidence(
            "malformed key image".to_owned(),
        ));
    }
    Ok(images)
}

fn key_images_json_of(tx: &crate::daemon::ScanTx) -> String {
    serde_json::Value::from(
        tx.key_images
            .iter()
            .map(hex::encode)
            .collect::<Vec<String>>(),
    )
    .to_string()
}

/// The result of scanning one transaction against one wallet - pure `KeyCustody`
/// output, no `Store` involved.
///
/// Deliberately separate from persisting it (see `record_scan_match`):
/// `rusqlite::Connection` is `Send` but not `Sync`, so a `&Store` reference held
/// across an `.await` (as combining these two steps into one async function would
/// require, since `KeyCustody::scan_tx_outputs` awaits) makes the containing
/// future `!Send` - fine for a future only ever `.await`ed directly inside another
/// task (every test in this file does that), but fatal the moment anything
/// containing it is handed to `tokio::spawn`, which requires the whole future to
/// be `Send + 'static`. `engine::run_scan_tick` is spawned in production
/// (`main.rs`), so this split isn't a style preference - the combined version
/// simply cannot be spawned. Caught by the compiler, not a test, the first time
/// this code was actually wired into a spawned task rather than awaited directly
/// in a test.
pub struct ScanResult {
    pub matches: Vec<MatchedOutput>,
    pub txid: String,
    pub key_images_json: String,
    /// The one-time key (hex) of each matched output, by output index: what
    /// a payment is recorded with so the same key is never credited twice
    /// (see `Store::record_payment_match`). Read from the transaction here,
    /// not reported by key custody: it is public chain data, and the
    /// engine's own refusal to credit a key twice should not depend on a
    /// backend reporting it right.
    pub output_keys: HashMap<usize, String>,
}

impl ScanResult {
    /// What `tx`, named `txid`, pays. The id comes with the transaction: it
    /// may be pruned, and then can't be hashed to it.
    fn of(txid: &str, tx: &crate::daemon::ScanTx, mut matches: Vec<MatchedOutput>) -> Self {
        let prefix = tx.input.prefix();
        // A locked output is no payment: until its unlock time (a height,
        // or from 500,000,000 up a timestamp) nobody can spend it, and the
        // merchant would be told they were paid with money they can't move.
        // No wallet sends a locked payment by accident, so it is refused
        // rather than held.
        if !matches.is_empty() && prefix.unlock_time.0 != 0 {
            tracing::warn!(
                tx.id = txid,
                unlock_time = prefix.unlock_time.0,
                outputs = matches.len(),
                "a transaction paying a store has a time lock on its outputs - not crediting them"
            );
            matches.clear();
        }
        let output_keys = matches
            .iter()
            .filter_map(|m| {
                let (TxOutTarget::ToKey { key } | TxOutTarget::ToTaggedKey { key, view_tag: _ }) =
                    &prefix.outputs.get(m.output_index)?.target;
                Some((m.output_index, hex::encode(key)))
            })
            .collect();
        Self {
            matches,
            txid: txid.to_owned(),
            key_images_json: key_images_json_of(tx),
            output_keys,
        }
    }
}

/// Scans `tx` for the wallet behind `handle`. The transaction's id comes
/// with it.
pub async fn scan_transaction_as(
    key_custody: &dyn KeyCustody,
    handle: WalletHandle,
    txid: &str,
    tx: &Transaction,
    minor_range: Range<u32>,
) -> Result<ScanResult> {
    let tx = crate::daemon::ScanTx::of(tx);
    let matches = key_custody
        .scan_tx_outputs(handle, &tx.input, 0..1, minor_range)
        .await?;
    Ok(ScanResult::of(txid, &tx, matches))
}

/// `scan_transaction` for a store's scan window (task 7.3): only the indices
/// of its open and recently closed orders. For tests; the scan loop goes
/// through `scan_txs_for_tenants`.
#[cfg(test)]
pub(crate) async fn scan_transaction_in_window(
    key_custody: &dyn KeyCustody,
    handle: WalletHandle,
    txid: &str,
    tx: &Transaction,
    window: &ScanIndices,
) -> Result<ScanResult> {
    let tx = crate::daemon::ScanTx::of(tx);
    let found = key_custody
        .scan_txs_for_indices(handle, std::slice::from_ref(&tx.input), window)
        .await?;
    let matches = found.into_iter().flat_map(|found| found.outputs).collect();
    Ok(ScanResult::of(txid, &tx, matches))
}

/// Scans a run of transactions for many tenants at once:
/// `tuning.scan_concurrency` at a time, one key-custody call per tenant for
/// the whole run, so a slow tenant (a slow key-custody backend) doesn't
/// hold up the others.
///
/// A tenant's call may take `tier`'s share of a round, the tier the caller
/// works in ([`crate::work::ScanTuning::reserved`]); past that it counts as
/// a failure for that tenant (task 7.4). A backend that answers, but
/// slowly, is then treated like one that is down, left behind and caught
/// up later: a scan that took longer would hold up the tiers after it.
/// `txids` and `inputs` are the ids and the scan inputs of `txs`, in the
/// same order (the ids come with the transactions: they may be pruned, and
/// then can't be hashed to them). Each tenant comes with how many of the
/// transactions, from the front, it has already been scanned for. Results
/// come back in the order given: for each tenant, the transactions that pay
/// it.
///
/// Nearly every transaction pays a tenant nothing, so a transaction's key
/// images (what a payment is recorded with) are worked out only for a match.
pub(crate) async fn scan_txs_for_tenants(
    key_custody: &dyn KeyCustody,
    txids: &[String],
    txs: &[crate::daemon::ScanTx],
    inputs: &[ScanInput],
    tenants: &[(&(crate::store::TenantId, WalletHandle, ScanIndices), usize)],
    tuning: &crate::work::ScanTuning,
    tier: crate::work::Tier,
) -> Vec<(crate::store::TenantId, Result<Vec<ScanResult>>)> {
    use futures_util::stream::{self, StreamExt as _};
    let deadline = tuning.reserved(tier);
    // By index: a closure over borrowed tuples trips a rustc limitation that
    // makes the future not `Send`.
    stream::iter(0..tenants.len())
        .map(|i| {
            let ((tenant_id, handle, window), done) = tenants[i];
            let done = done.min(inputs.len());
            let span = tracing::debug_span!("scan for store", store.id = %tenant_id);
            tracing::Instrument::instrument(
                async move {
                    let result = match tokio::time::timeout(
                        deadline,
                        key_custody.scan_txs_for_indices(*handle, &inputs[done..], window),
                    )
                    .await
                    {
                        Ok(Ok(found)) => found
                            .into_iter()
                            .filter(|found| !found.outputs.is_empty())
                            .map(|found| {
                                let at = done + found.tx;
                                match (txids.get(at), txs.get(at)) {
                                    (Some(txid), Some(tx)) => {
                                        Ok(ScanResult::of(txid, tx, found.outputs))
                                    }
                                    _ => {
                                        Err(ScannerError::KeyCustody(KeyCustodyError::ScanFailed(
                                            "a match for a transaction that wasn't in the batch"
                                                .into(),
                                        )))
                                    }
                                }
                            })
                            .collect(),
                        Ok(Err(error)) => Err(error.into()),
                        Err(_) => Err(ScannerError::KeyCustody(
                            KeyCustodyError::BackendUnavailable(format!(
                                "scan took longer than {deadline:?}"
                            )),
                        )),
                    };
                    (tenant_id.clone(), result)
                },
                span,
            )
        })
        .buffered(tuning.scan_concurrency)
        .collect()
        .await
}

/// `scan_txs_for_tenants` for one transaction: `None` is a tenant it pays
/// nothing.
pub(crate) async fn scan_for_tenants(
    key_custody: &dyn KeyCustody,
    txid: &str,
    tx: &Transaction,
    tenants: &[&(crate::store::TenantId, WalletHandle, ScanIndices)],
    tuning: &crate::work::ScanTuning,
    tier: crate::work::Tier,
) -> Vec<(crate::store::TenantId, Result<Option<ScanResult>>)> {
    let tx = crate::daemon::ScanTx::of(tx);
    let inputs = [tx.input.clone()];
    let tenants: Vec<_> = tenants.iter().map(|tenant| (*tenant, 0)).collect();
    scan_txs_for_tenants(
        key_custody,
        std::slice::from_ref(&txid.to_owned()),
        std::slice::from_ref(&tx),
        &inputs,
        &tenants,
        tuning,
        tier,
    )
    .await
    .into_iter()
    .map(|(tenant_id, result)| (tenant_id, result.map(|mut found| found.pop())))
    .collect()
}

/// Whether a scan that began at `scanned_at` must not be recorded against
/// the tenant row: it changed wallet since, so the scan may have used the
/// old wallet's keys, and an index it found may now name a new order on the
/// new wallet (migration 0029). The scan-only row holding the old keys looks
/// at the same transactions itself.
fn stale_after_wallet_change(
    store: &Store,
    tenant_id: &crate::store::TenantId,
    scan: &ScanResult,
    scanned_at: i64,
) -> Result<bool> {
    if scan.matches.is_empty() || !store.wallet_changed_since(tenant_id, scanned_at)? {
        return Ok(false);
    }
    tracing::info!(
        store.id = %tenant_id,
        tx.id = %scan.txid,
        "a scan from before the store changed wallet found outputs - leaving them to the old wallet's watch"
    );
    Ok(true)
}

/// Persists a `ScanResult` against one tenant. `seen_at` is when the scan
/// began (a round's start), or earlier.
///
/// Purely synchronous - no `.await` anywhere in this function, so a `&Store`
/// parameter here is never an issue. Returns the set of order ids touched, so
/// the caller knows which orders need `Store::recompute_order_status`.
pub fn record_scan_match(
    store: &Store,
    tenant_id: &crate::store::TenantId,
    scan: &ScanResult,
    seen_at: i64,
    block_height: Option<u64>,
) -> Result<std::collections::BTreeSet<crate::store::OrderId>> {
    let mut touched = std::collections::BTreeSet::new();
    if stale_after_wallet_change(store, tenant_id, scan, seen_at)? {
        return Ok(touched);
    }
    for m in &scan.matches {
        let Some(order) = store.find_order_by_minor_index(tenant_id, m.subaddress_index.minor)?
        else {
            continue; // a match against an index with no order row is not this scanner's problem to solve
        };
        // An output whose amount couldn't be decrypted is skipped, not recorded as
        // zero. A present-but-zero row is strictly worse than no row: it contributes
        // nothing to the received total while still dragging `min_confirmations` and
        // `all_zero_conf` around in the status derivation, and it
        // can never be cleaned up afterwards - voiding requires affirmative
        // double-spend proof, which will never arrive for a perfectly valid output.
        // Skipping leaves the tick free to record it properly once the amount is
        // recoverable.
        let Some(amount) = m.amount_piconero else {
            tracing::warn!(
                order.id = %order.id,
                tx.id = %scan.txid,
                output_index = m.output_index,
                "an output matched an order but its amount could not be decrypted - not recording it"
            );
            continue;
        };
        store.record_payment_match(
            &order.id,
            &scan.txid,
            output_index(m.output_index)?,
            amount,
            &scan.key_images_json,
            seen_at,
            block_height.map(crate::store::sql_height).transpose()?,
            scan.output_keys.get(&m.output_index).map(String::as_str),
        )?;
        touched.insert(order.id);
    }
    Ok(touched)
}

/// Store a match from an unfinished block without making it visible as a
/// payment. The caller commits this and the transaction checkpoint together.
pub(crate) fn stage_block_match(
    store: &Store,
    network: monero::Network,
    tenant_id: &crate::store::TenantId,
    scan: &ScanResult,
    seen_at: i64,
) -> Result<()> {
    if stale_after_wallet_change(store, tenant_id, scan, seen_at)? {
        return Ok(());
    }
    for m in &scan.matches {
        let Some(order) = store.find_order_by_minor_index(tenant_id, m.subaddress_index.minor)?
        else {
            continue;
        };
        let Some(amount) = m.amount_piconero else {
            tracing::warn!(
                order.id = %order.id,
                tx.id = %scan.txid,
                output_index = m.output_index,
                "an output matched an order but its amount could not be decrypted - not staging it"
            );
            continue;
        };
        store.stage_partial_match(&crate::store::StagedMatch {
            network,
            tenant_id,
            order_id: &order.id,
            txid: &scan.txid,
            output_index: output_index(m.output_index)?,
            amount,
            key_images_json: &scan.key_images_json,
            seen_at,
            output_key: scan.output_keys.get(&m.output_index).map(String::as_str),
        })?;
    }
    Ok(())
}

/// An output's index as SQLite stores it. A transaction has a handful of
/// outputs; one past `i64::MAX` is refused rather than wrapped.
fn output_index(index: usize) -> Result<i64> {
    i64::try_from(index).map_err(|e| {
        ScannerError::Store(StoreError::Sqlite(rusqlite::Error::ToSqlConversionFailure(
            Box::new(e),
        )))
    })
}

/// What one `check_vanished_mempool_payments` sweep concluded: orders
/// whose payments changed, and the reorg point is not part of it (this sweep
/// is not about the chain changing shape).
pub struct VanishedPoolReport {
    /// Orders whose payments changed and therefore need a status recompute.
    pub dirty_orders: Vec<crate::store::OrderId>,
    /// The subset of `dirty_orders` where a payment was voided on affirmative
    /// double-spend proof - the `order.double_spend_detected` event's trigger.
    pub double_spent_orders: Vec<crate::store::OrderId>,
    /// Payments (by id) whose transaction is nowhere and isn't proven
    /// double-spent: dropped, evicted or still propagating. They stay as
    /// they are and are looked at again; a caller spaces those looks out.
    pub unresolved: Vec<i64>,
}

/// What the node said about several vanished payments at once
/// ([`vanished_hints`]), so checking them one by one needn't ask again.
#[derive(Default)]
pub(crate) struct VanishedHints {
    /// Where each transaction is, by txid.
    locations: HashMap<String, TxLocation>,
    /// For payments whose transaction is nowhere: whether a double spend is
    /// proven, by payment id.
    proven: HashMap<i64, bool>,
}

/// Asks the node about every payment in `unconfirmed` whose transaction isn't
/// in the pool snapshot, in as few round trips as it can: one for where the
/// transactions are, and one (corroborated across nodes) for the key images
/// of those that are nowhere. Whatever isn't answered that way (a node
/// client that can't batch, a transaction the answer didn't settle) is left
/// out, and [`check_vanished_candidates`] asks about it payment by payment.
/// A node that fails is an error: it would fail payment by payment too.
pub(crate) async fn vanished_hints(
    daemon: &dyn MoneroDaemonClient,
    mempool_txids: &HashSet<String>,
    unconfirmed: &[&crate::store::OrderPaymentRow],
) -> std::result::Result<VanishedHints, DaemonError> {
    let mut hints = VanishedHints::default();
    let mut txids: Vec<String> = unconfirmed
        .iter()
        .map(|payment| payment.txid.clone())
        .filter(|txid| !mempool_txids.contains(txid))
        .collect();
    txids.sort_unstable();
    txids.dedup();
    if txids.is_empty() {
        return Ok(hints);
    }
    hints.locations = daemon.locate_transactions(&txids).await?;
    // The key images of every payment that is nowhere, asked about together.
    let mut images: Vec<String> = Vec::new();
    let mut spans: Vec<(i64, Range<usize>)> = Vec::new();
    for payment in unconfirmed {
        if hints.locations.get(&payment.txid) != Some(&TxLocation::NotFound) {
            continue;
        }
        if let Ok(own) = parse_payment_key_images(&payment.key_images_json) {
            spans.push((payment.id, images.len()..images.len() + own.len()));
            images.extend(own);
        }
    }
    // One payment gains nothing from being asked about here first.
    if spans.len() > 1 {
        let statuses = daemon.is_key_image_spent_corroborated(&images).await?;
        if statuses.len() == images.len() {
            for (id, span) in spans {
                let proven = statuses[span].contains(&KeyImageStatus::SpentInBlockchain);
                hints.proven.insert(id, proven);
            }
        }
    }
    Ok(hints)
}

pub(crate) async fn check_vanished_candidates(
    db: &crate::store::Db,
    daemon: &dyn MoneroDaemonClient,
    mempool_txids: &HashSet<String>,
    current_height: u64,
    now: i64,
    unconfirmed: Vec<crate::store::OrderPaymentRow>,
    hints: &VanishedHints,
) -> Result<VanishedPoolReport> {
    let mut dirty_orders = std::collections::BTreeSet::new();
    let mut double_spent_orders = HashSet::new();
    let mut unresolved = Vec::new();

    for payment in unconfirmed {
        if mempool_txids.contains(&payment.txid) {
            continue; // still pending in the pool - nothing has been decided about it yet
        }
        // Without a batch hint, ask for corroborated location directly. Asking
        // one node first would duplicate its RPC and spend the settlement budget
        // before the key-image evidence can be checked on slower connections.
        let (mut location, corroborated) = match hints.locations.get(&payment.txid) {
            Some(location) => (*location, false),
            None => match daemon
                .locate_transaction_corroborated(&payment.txid)
                .await?
            {
                Some(location) => (location, true),
                None => (daemon.locate_transaction(&payment.txid).await?, true),
            },
        };
        // A batch hint from one node still needs independent corroboration
        // before missing-transaction evidence can void money.
        if location == TxLocation::NotFound && !corroborated {
            if let Some(agreed) = daemon
                .locate_transaction_corroborated(&payment.txid)
                .await?
            {
                location = agreed;
            }
        }
        match location {
            // Mined after all: the pool snapshot was taken before the block arrived,
            // or the block scan stopped short of that height this tick. Recording the
            // height here is the same write the block scan would have made, and
            // costs the payment nothing if the block scan gets there first.
            TxLocation::InBlock(new_height) => {
                let (order_id, txid, output) = (
                    payment.order_id.clone(),
                    payment.txid.clone(),
                    payment.output_index,
                );
                db.run(crate::store::db::Class::Scanner, move |s| {
                    s.update_payment_block_height(
                        &order_id,
                        &txid,
                        output,
                        Some(crate::store::sql_height(new_height)?),
                    )
                })
                .await?;
                dirty_orders.insert(payment.order_id.clone());
            }
            // The daemon's live pool disagrees with the snapshot taken at the top of
            // this tick (it was re-broadcast, or the snapshot was simply a moment
            // stale). Nothing to conclude either way.
            TxLocation::InPool => {}
            TxLocation::NotFound => {
                // Dropped, evicted, still-propagating, and genuinely double-spent
                // transactions are all indistinguishable from here except by their key
                // images - `void_if_double_spend_proven` is where that's resolved, the
                // same way `check_for_reorg_and_reconcile` resolves the identical
                // question. Voiding on anything less would write off a payment the
                // customer really made.
                let proven = hints.proven.get(&payment.id).copied();
                if void_if_double_spend_proven(db, daemon, &payment, current_height, now, proven)
                    .await?
                {
                    dirty_orders.insert(payment.order_id.clone());
                    double_spent_orders.insert(payment.order_id.clone());
                } else {
                    unresolved.push(payment.id);
                }
            }
        }
    }

    Ok(VanishedPoolReport {
        dirty_orders: dirty_orders.into_iter().collect(),
        double_spent_orders: double_spent_orders.into_iter().collect(),
        unresolved,
    })
}

/// Adds an event to the order-event log and, until monokulo delivers the
/// log's events itself, queues the engine's own webhook delivery of it too
/// (with the same `event_id`), in the caller's transaction.
fn record_order_event(
    store: &Store,
    order_id: &crate::store::OrderId,
    event_type: &str,
    fields: &[(&str, &str)],
    now: i64,
) -> Result<()> {
    let seq = store.append_order_event(order_id, event_type, fields, now)?;
    let event = store
        .order_events_after(seq - 1, 1)?
        .into_iter()
        .next()
        .ok_or(StoreError::NotFound)?;
    let webhooks: Vec<_> = store
        .list_webhooks(&event.tenant_id)?
        .into_iter()
        .filter(|w| w.enabled)
        .collect();
    if webhooks.is_empty() {
        return Ok(());
    }
    let mut envelope: serde_json::Map<String, serde_json::Value> = fields
        .iter()
        .map(|(name, value)| ((*name).to_owned(), serde_json::Value::from(*value)))
        .collect();
    envelope.insert("order_id".into(), serde_json::json!(order_id.as_str()));
    envelope.insert("event_id".into(), serde_json::json!(event.event_id));
    envelope.insert("event".into(), serde_json::json!(event_type));
    envelope.insert("created_at".into(), serde_json::json!(now));
    let body = serde_json::Value::Object(envelope).to_string();
    for webhook in webhooks {
        store.enqueue_webhook_delivery(&webhook.id, order_id, event_type, &body, now)?;
    }
    Ok(())
}

/// Recomputes an order's status and, on an actual transition, adds an
/// `order.<status>` event to the order-event log, which monokulo delivers
/// to the store's webhooks.
///
/// This is the *only* place status-transition events are written - see
/// `docs/DESIGN.md` §11 for why that's a transition-triggered event, never a
/// per-confirmation-count tick.
///
/// The status write and the event it implies happen in one transaction. They are
/// not independently retryable: `recompute_order_status` decides "did anything
/// change" by comparing against the *stored* status, so the instant the new status
/// commits the transition stops being detectable. Writing the event separately
/// meant a transient store error (or a crash) in the window between them didn't
/// postpone the merchant's `order.paid`, it destroyed it - permanently, for an
/// order that really is paid. Rolling the status back with the failed write leaves
/// the next tick to redo both.
///
/// Public so `engine-test-support` can settle an order the same way a real
/// scan does (`TestEngineHandle::mark_order_paid`).
pub fn recompute_and_notify(
    store: &Store,
    order_id: &crate::store::OrderId,
    current_height: u64,
    now: i64,
) -> Result<Option<shared::activity::Transition>> {
    let result = store
        .in_transaction(|store| recompute_and_notify_in_tx(store, order_id, current_height, now));
    #[cfg(test)]
    if matches!(result, Ok(Some(_))) {
        crate::store::crash_checkpoint("recompute.after_commit");
    }
    result
}

/// The body of `recompute_and_notify`, minus the transaction, for callers that are
/// already inside one - `Store::in_transaction` uses `unchecked_transaction`, so
/// nesting it would fail at SQLite's "cannot start a transaction within a
/// transaction" rather than composing.
pub(crate) fn recompute_and_notify_in_tx(
    store: &Store,
    order_id: &crate::store::OrderId,
    current_height: u64,
    now: i64,
) -> Result<Option<shared::activity::Transition>> {
    let (old_status, new_status) = store.recompute_order_status(order_id, current_height, now)?;
    if old_status == new_status {
        return Ok(None);
    }
    record_order_event(
        store,
        order_id,
        &format!("order.{new_status}"),
        &[("status", new_status.as_str())],
        now,
    )?;
    #[cfg(test)]
    crate::store::crash_checkpoint("recompute.before_commit");
    Ok(Some(shared::activity::Transition {
        from: old_status,
        to: new_status,
    }))
}

/// Checks whether `payment`'s own key images prove a double-spend and, if so, voids
/// it (see `void_and_notify`) and reports that it did.
///
/// Shared by `check_for_reorg_and_reconcile` and `check_vanished_mempool_payments`,
/// whose "this payment's transaction is nowhere to be found" case both resolve
/// identically: never void on absence alone, only on an affirmative
/// `SpentInBlockchain` for one of the payment's own key images (§DESIGN.md 7.5). One
/// copy of that evidence rule, not two that could quietly drift apart under a future
/// edit to just one of them.
///
/// Takes `&SharedStore` rather than an already-held `&Store`, and does the daemon
/// call *before* acquiring the lock, for the same reason every other lock hold in
/// this file is kept brief: `is_key_image_spent` is network I/O, and nothing may
/// `.await` while holding the store mutex.
///
/// `proven` is the answer when the node was already asked about this
/// payment's key images along with others' ([`vanished_hints`]): the same
/// corroborated evidence, fetched in one round trip instead of one each.
async fn void_if_double_spend_proven(
    db: &crate::store::Db,
    daemon: &dyn MoneroDaemonClient,
    payment: &crate::store::OrderPaymentRow,
    current_height: u64,
    now: i64,
    proven: Option<bool>,
) -> Result<bool> {
    // Invalid stored evidence is never proof, and never an error that would
    // stop the check for every other payment: log it and leave the payment.
    let key_images = match parse_payment_key_images(&payment.key_images_json) {
        Ok(images) => images,
        Err(error) => {
            tracing::warn!(payment.id = payment.id, order.id = %payment.order_id, error = %error,
                "a vanished payment's stored key images are invalid - never voiding it on that");
            return Ok(false);
        }
    };
    // Corroborated, not the bare call: this is the one place a false accusation
    // permanently voids real money, so a `daemon` that knows about more than one
    // node (`daemon_fallback::FallbackDaemonClient`) cross-checks them here rather
    // than trusting whichever single one happened to answer - see
    // `MoneroDaemonClient::is_key_image_spent_corroborated`'s doc comment.
    let proven = match proven {
        Some(proven) => proven,
        None => daemon
            .is_key_image_spent_corroborated(&key_images)
            .await?
            .contains(&KeyImageStatus::SpentInBlockchain),
    };
    if !proven {
        // Still ambiguous (still propagating, or a re-check will catch it next tick)
        // - never void on this evidence alone.
        return Ok(false);
    }
    let (order_id, txid, output) = (
        payment.order_id.clone(),
        payment.txid.clone(),
        payment.output_index,
    );
    db.run(crate::store::db::Class::Scanner, move |s| {
        void_and_notify(s, &order_id, &txid, output, current_height, now)
    })
    .await
}

/// Voids a payment proven double-spent and, in the *same transaction*, records every
/// consequence: the sticky `double_spend_detected_at` stamp, the owning order's
/// recomputed status, the `order.<status>` event if that status moved, and the
/// `order.double_spend_detected` event.
///
/// The atomicity is the point. A void is a committed write, but everything that makes
/// it visible to a merchant used to happen only after the whole reconciliation pass
/// finished - so one unreachable node partway through that pass discarded all of it,
/// and not recoverably. The voided row drops out of the input set of both sweeps by
/// construction (it is no longer an unvoided payment, nor an unconfirmed one), and an
/// order in a terminal status is skipped by the per-tick recompute as well - which is
/// exactly the state an order sits in when this matters, since the merchant was told
/// it was paid. The order was left permanently reading `paid` for money that had been
/// double-spent, with no event ever sent.
fn void_and_notify(
    store: &Store,
    order_id: &crate::store::OrderId,
    txid: &str,
    output_index: i64,
    current_height: u64,
    now: i64,
) -> Result<bool> {
    store.in_transaction(|store| {
        void_and_notify_in_tx(store, order_id, txid, output_index, current_height, now)
    })
}

/// The body of `void_and_notify`, for callers already inside a transaction
/// (the reorg job commits the void and the removal of its candidate together).
pub(crate) fn void_and_notify_in_tx(
    store: &Store,
    order_id: &crate::store::OrderId,
    txid: &str,
    output_index: i64,
    current_height: u64,
    now: i64,
) -> Result<bool> {
    // Another payment on the order carries its output key: its inputs being
    // spent is no double spend of the order's money, as a copy of a real
    // payment (a lying node's, carrying the real one's key images) shows.
    // Which of them is credited is `store::conflicts`'s to settle, at the
    // recompute; nothing is voided or flagged here. Returns whether it
    // voided.
    if store.shares_output_key(order_id, txid, output_index)? {
        recompute_and_notify_in_tx(store, order_id, current_height, now)?;
        return Ok(false);
    }
    // A payment is voided on the evidence that its transaction is in no
    // block, so its recorded height (from a discarded chain, if it has one)
    // goes too: a void that is later reversed must come back unconfirmed,
    // followed by the vanished-payment check, not counting confirmations
    // from a block that is no longer on the chain. (Before the void: the
    // height of a voided row is left alone.)
    store.update_payment_block_height(order_id, txid, output_index, None)?;
    store.void_payment(order_id, txid, output_index, now)?;
    store.mark_double_spend_detected(order_id, now)?;
    recompute_and_notify_in_tx(store, order_id, current_height, now)?;
    // One event per voided payment row (docs/DESIGN.md §11), independent of
    // whatever status transition the recompute above may also have announced.
    record_order_event(
        store,
        order_id,
        "order.double_spend_detected",
        &[("txid", txid)],
        now,
    )?;
    Ok(true)
}

/// Reverses a payment void that a later, corroborated re-check no longer supports -
/// the other half of the fix for `is_key_image_spent`'s single-node trust boundary
/// (`docs/DESIGN.md` §7.7), alongside `is_key_image_spent_corroborated` itself.
///
/// Unlike `unvoid_payment`'s reorg-driven counterpart in
/// `check_for_reorg_and_reconcile` (which deliberately leaves
/// `orders.double_spend_detected_at` set - a real conflicting transaction genuinely
/// existed there for a time, even though it was later reorged away), this path
/// exists specifically because the original accusation may never have been true at
/// all, so it clears the flag too - but only once *every* voided payment on the
/// order has been un-voided, since the flag is scoped to the whole order, not to
/// this one payment: another payment on the same order may have been voided for a
/// separate, entirely genuine reason, and correcting this one must not erase that.
///
/// Fires a distinct `order.double_spend_reversed` event rather than silently folding
/// into whatever status the recompute lands on, so a merchant who was told "this was
/// a double-spend" is also told, just as explicitly, "we were wrong about that" -
/// the whole point of the correction is transparency, not a quiet undo.
///
/// `block_height` is where the transaction is now: in that block, or
/// (`None`) in no block, so that the vanished-payment check follows it.
fn unvoid_as_false_positive(
    store: &Store,
    order_id: &crate::store::OrderId,
    txid: &str,
    output_index: i64,
    block_height: Option<u64>,
    current_height: u64,
    now: i64,
) -> Result<bool> {
    store.in_transaction(|store| {
        if !store.unvoid_payment(order_id, txid, output_index)? {
            return Ok(false);
        }
        let block_height = block_height.map(crate::store::sql_height).transpose()?;
        store.update_payment_block_height(order_id, txid, output_index, block_height)?;
        if store
            .get_all_payments(order_id)?
            .iter()
            .all(|p| p.voided_at.is_none() || p.superseded_by.is_some())
        {
            store.clear_double_spend_flag(order_id)?;
        }
        recompute_and_notify_in_tx(store, order_id, current_height, now)?;
        record_order_event(
            store,
            order_id,
            "order.double_spend_reversed",
            &[("txid", txid)],
            now,
        )?;
        Ok(true)
    })
}

/// How far back the upkeep tier's void recheck (`work::upkeep`, `docs/DESIGN.md` §7.7) looks for voided
/// payments to recheck.
///
/// Bounded deliberately: a void that turns out to have been a false accusation is exactly as worth correcting a
/// day later as a minute later (unlike zero-conf detection, there is no latency requirement to trade away here),
/// and an old, long-settled void is not worth the cost of rechecking forever: if it were wrong, the merchant
/// and customer have long since moved on regardless.
pub const DOUBLE_SPEND_RECHECK_WINDOW_SECS: i64 = 48 * 3600;

/// Rechecks one voided payment and restores it if fresh, corroborated
/// evidence no longer supports the double-spend accusation. Returns whether
/// it was restored. Invalid evidence or an inconclusive answer leaves the
/// payment voided (`Ok(false)`); a node or storage failure is an error, so a
/// caller can stop asking a node that isn't answering.
///
/// `statuses` are this payment's key-image statuses when the caller already
/// asked about them along with other payments' ([`voided_key_image_statuses`]).
pub(crate) async fn recheck_voided_payment(
    db: &crate::store::Db,
    daemon: &dyn MoneroDaemonClient,
    network: monero::Network,
    payment: &crate::store::OrderPaymentRow,
    current_height: u64,
    now: i64,
    statuses: Option<&[KeyImageStatus]>,
) -> Result<bool> {
    // The transaction itself, first: a void was a false accusation if the
    // transaction it accused of being nowhere is in a block after all (the
    // conflicting one was reorged out, or never existed). Its own inputs
    // are then spent - by it - so the key-image test below could never
    // clear it.
    let location =
        match crate::work::bounded(daemon.locate_transaction_corroborated(&payment.txid)).await? {
            Some(agreed) => agreed,
            None => crate::work::bounded(daemon.locate_transaction(&payment.txid)).await?,
        };
    let block_height = match location {
        TxLocation::InBlock(height) => Some(height),
        TxLocation::InPool | TxLocation::NotFound => {
            let key_images = match parse_payment_key_images(&payment.key_images_json) {
                Ok(images) => images,
                Err(e) => {
                    tracing::warn!(order.id = %payment.order_id, error = %e, "double-spend revalidation: leaving the order voided because its stored evidence is invalid");
                    return Ok(false);
                }
            };
            let statuses = match statuses {
                Some(statuses) => statuses.to_vec(),
                None => {
                    crate::work::bounded(daemon.is_key_image_spent_corroborated(&key_images))
                        .await?
                }
            };
            if statuses.len() != key_images.len()
                || !statuses
                    .iter()
                    .all(|status| *status == KeyImageStatus::Unspent)
            {
                tracing::info!(order.id = %payment.order_id, "double-spend revalidation: inconclusive key-image statuses; leaving the order voided");
                return Ok(false);
            }
            None
        }
    };
    let found_in = match block_height {
        Some(height) => crate::work::chain::block_holding(daemon, &payment.txid, height).await,
        None => None,
    };
    let (order_id, txid, output) = (
        payment.order_id.clone(),
        payment.txid.clone(),
        payment.output_index,
    );
    let restored = db
        .run(crate::store::db::Class::Scanner, move |s| {
            let restored = unvoid_as_false_positive(
                s,
                &order_id,
                &txid,
                output,
                block_height,
                current_height,
                now,
            )?;
            if let (true, Some(height), Some(hash)) = (restored, block_height, &found_in) {
                s.attest_payment_block(&txid, height, hash)?;
            }
            Ok::<bool, ScannerError>(restored)
        })
        .await?;
    if restored {
        tracing::info!(order.id = %payment.order_id, network = crate::network::network_str(network), "double-spend revalidation reversed a void");
    }
    Ok(restored)
}

/// The corroborated key-image statuses of several voided payments, asked for
/// in one round trip (per node), by payment id. A payment whose stored
/// evidence is invalid is left out, as is everything if there is only one
/// payment to ask about: [`recheck_voided_payment`] then asks for itself. A
/// node that fails is an error: it would fail for each payment too.
pub(crate) async fn voided_key_image_statuses(
    daemon: &dyn MoneroDaemonClient,
    payments: &[crate::store::OrderPaymentRow],
) -> Result<HashMap<i64, Vec<KeyImageStatus>>> {
    let mut images: Vec<String> = Vec::new();
    let mut spans: Vec<(i64, Range<usize>)> = Vec::new();
    for payment in payments {
        if let Ok(own) = parse_payment_key_images(&payment.key_images_json) {
            spans.push((payment.id, images.len()..images.len() + own.len()));
            images.extend(own);
        }
    }
    if spans.len() < 2 {
        return Ok(HashMap::default());
    }
    let statuses = crate::work::bounded(daemon.is_key_image_spent_corroborated(&images)).await?;
    if statuses.len() != images.len() {
        return Ok(HashMap::default());
    }
    Ok(spans
        .into_iter()
        .map(|(id, span)| (id, statuses[span].to_vec()))
        .collect())
}

/// Never request fewer than this many blocks in one `get_chain_blocks` call,
/// regardless of how large `avg_bytes_per_block` has drifted - a pathological
/// (e.g. cold-start-too-low) estimate must not compute a chunk size of `0`
/// and stall the catch-up walk forever.
const SCAN_CHUNK_MIN_BLOCKS: u64 = 1;
/// The cold-start estimate for bytes-per-block, before any chunk in this tick
/// has actually been fetched - deliberately conservative (real average block
/// sizes on a healthy network are often smaller than this), so the very
/// first chunk of a catch-up walk undershoots `scan_chunk_memory_budget_mb`
/// rather than overshoots it. Self-correcting from the second chunk onward
/// regardless. The same guess the node's link starts from, so the first
/// chunk and its timeout agree.
pub(crate) const SCAN_CHUNK_INITIAL_AVG_BYTES: f64 = crate::link::COLD_BYTES_PER_BLOCK;

pub use shared::scaling::{ChunkLimit, ChunkPlan};

/// Pure sizing decision, extracted so it's directly, cheaply unit-testable:
/// the smaller of what fits the response cap and what the link delivers in
/// a target call (its round trip, the node's work per block and the blocks'
/// bytes, at the running bytes-per-block average), within 1 and the
/// tuning's most blocks a request, and the blocks that remain. A link not
/// measured (`None`) doesn't limit.
pub(crate) fn next_scan_chunk(
    tuning: &crate::work::ScanTuning,
    response_cap_bytes: u64,
    link: Option<crate::link::LinkCost>,
    avg_bytes_per_block: f64,
    remaining: u64,
) -> ChunkPlan {
    let avg = avg_bytes_per_block.max(1.0);
    let by_memory = (response_cap_bytes as f64 / avg).floor();
    let by_time = link.map_or(f64::INFINITY, |link| {
        link.items_within(tuning.target_call_secs(), avg, link.ttfb_per_block_secs)
    });
    let (wanted, mut limited_by) = if by_time < by_memory {
        (by_time, ChunkLimit::Link)
    } else {
        (by_memory, ChunkLimit::Memory)
    };
    let mut blocks = if wanted >= tuning.chunk_max_blocks as f64 {
        limited_by = ChunkLimit::Maximum;
        tuning.chunk_max_blocks
    } else {
        (wanted as u64).max(SCAN_CHUNK_MIN_BLOCKS)
    };
    if remaining < blocks {
        blocks = remaining.max(SCAN_CHUNK_MIN_BLOCKS).min(blocks);
        limited_by = ChunkLimit::Remaining;
    }
    ChunkPlan { blocks, limited_by }
}

/// Most transactions on one page of a large block: what monerod's
/// restricted RPC (a public node's) gives in one `/get_transactions`
/// answer, so a page is one request.
pub(crate) const PAGE_MAX_TXS: u64 = crate::daemon_rpc::TXS_PER_REQUEST as u64;

/// Whether a block of `weight` bytes is scanned in pages rather than
/// fetched whole: it would overrun one response (the cap from the scan
/// memory budget), or one request for it would take longer than the
/// tuning's `whole_block_max_secs` over the measured link. A block whose
/// weight the node didn't give is fetched whole, as before.
pub(crate) fn scan_in_pages(
    tuning: &crate::work::ScanTuning,
    weight: Option<u64>,
    response_cap_bytes: u64,
    link: Option<crate::link::LinkCost>,
) -> bool {
    let Some(weight) = weight else {
        return false;
    };
    weight > response_cap_bytes
        || link.is_some_and(|link| link.secs(1, weight as f64) > tuning.whole_block_max_secs)
}

/// How many transactions the next page of a large block holds
/// (`docs/engine_scaling.md` section 4): the fewest of what fits the response
/// cap and what the link delivers in a target call (its round trip and the
/// transactions' bytes), at `avg_tx_bytes` a transaction, and what the scan
/// gets through in the same time when a transaction costs
/// `scan_secs_per_tx` (for every store scanned for); within
/// 1..=[`PAGE_MAX_TXS`] and the transactions that remain. A link not
/// measured or a scan cost not known yet doesn't limit.
pub(crate) fn next_page(
    tuning: &crate::work::ScanTuning,
    response_cap_bytes: u64,
    link: Option<crate::link::LinkCost>,
    avg_tx_bytes: f64,
    scan_secs_per_tx: Option<f64>,
    remaining: u64,
) -> ChunkPlan {
    let slice_secs = tuning.target_call_secs();
    let avg = avg_tx_bytes.max(1.0);
    let mut wanted = (response_cap_bytes as f64 / avg).floor();
    let mut limited_by = ChunkLimit::Memory;
    if let Some(by_time) = link.map(|link| link.items_within(slice_secs, avg, 0.0)) {
        if by_time < wanted {
            (wanted, limited_by) = (by_time, ChunkLimit::Link);
        }
    }
    if let Some(by_cpu) = scan_secs_per_tx
        .filter(|secs| *secs > 0.0)
        .map(|secs| (slice_secs / secs).floor())
    {
        if by_cpu < wanted {
            (wanted, limited_by) = (by_cpu, ChunkLimit::Cpu);
        }
    }
    let mut txs = if wanted >= PAGE_MAX_TXS as f64 {
        limited_by = ChunkLimit::Maximum;
        PAGE_MAX_TXS
    } else {
        (wanted as u64).max(1)
    };
    if remaining < txs {
        txs = remaining.max(1);
        limited_by = ChunkLimit::Remaining;
    }
    ChunkPlan {
        blocks: txs,
        limited_by,
    }
}

/// The bytes-per-block estimate after a block request that ran out of time
/// or came back too large: doubled, so the next request asks for half as
/// many blocks (`docs/engine_scaling.md` section 2). Successes bring it back
/// down through [`update_avg_bytes_per_block`].
pub(crate) fn avg_after_failed_fetch(avg_bytes_per_block: f64) -> f64 {
    (avg_bytes_per_block * 2.0).min(1e12)
}

/// Pure EWMA update, same reasoning as `next_scan_chunk_size` above - `chunk_
/// bytes`/`block_count` are already known before this is called, so this is
/// just the averaging formula on its own (weighted by the tuning's
/// `chunk_ewma_alpha`), testable without any daemon or store at all.
pub(crate) fn update_avg_bytes_per_block(
    tuning: &crate::work::ScanTuning,
    avg_bytes_per_block: f64,
    chunk_bytes: usize,
    block_count: usize,
) -> f64 {
    let observed_avg = chunk_bytes as f64 / block_count as f64;
    let alpha = tuning.chunk_ewma_alpha;
    (1.0 - alpha).mul_add(avg_bytes_per_block, alpha * observed_avg)
}

/// One scan round for one network (`docs/scanner_microtasks.md`).
///
/// Reorg detection and reconciliation, new and lagging blocks, the mempool,
/// status recomputes and upkeep, each a bounded unit with a share of the
/// round's time.
///
/// `network` scopes everything to one chain: the `daemon` passed in must be
/// the client for that same network, and `tenants` are the tenants whose keys
/// are registered (only those on `network` are ever scanned).
///
/// On first run (no `scanned_blocks` history at all), seeds at the current chain
/// tip rather than replaying the entire chain from genesis - this is a payment
/// gateway watching for new incoming payments, not a block explorer backfilling
/// history.
///
/// Returns the first unit failure of the round, after every other tier ran.
///
/// Not the engine's own loop: `loops::run_scanner_loop` keeps a
/// `work::ScanState` across rounds, runs them back to back while work is
/// left, and shares the state with the fast mempool path. This runs one
/// round from empty in-memory state, over an inline store, for tests, the
/// e2e harness and tools that want "scan once now".
pub async fn run_scan_tick(
    store: &crate::store::SharedStore,
    key_custody: &dyn KeyCustody,
    daemon: &dyn MoneroDaemonClient,
    network: &str,
    tenants: &[(crate::store::TenantId, WalletHandle)],
    reorg_check_depth: u64,
    expired_order_grace_period_seconds: i64,
) -> Result<()> {
    let state = crate::work::ScanState::default();
    let budget = crate::engine_settings::EngineSettings::defaults()
        .scan
        .load()
        .scan_chunk_memory_budget_mb;
    run_scan_tick_with(
        &state,
        store,
        key_custody,
        daemon,
        network,
        tenants,
        reorg_check_depth,
        expired_order_grace_period_seconds,
        budget,
    )
    .await
}

/// `run_scan_tick` with the scheduler's in-memory state kept by the caller
/// between rounds. For tests and tools, like `run_scan_tick`.
#[expect(
    clippy::too_many_arguments,
    reason = "one round's genuinely independent inputs"
)]
pub async fn run_scan_tick_with(
    state: &crate::work::ScanState,
    store: &crate::store::SharedStore,
    key_custody: &dyn KeyCustody,
    daemon: &dyn MoneroDaemonClient,
    network: &str,
    tenants: &[(crate::store::TenantId, WalletHandle)],
    reorg_check_depth: u64,
    expired_order_grace_period_seconds: i64,
    scan_chunk_memory_budget_mb: u32,
) -> Result<()> {
    let db = crate::store::Db::over_shared(Arc::clone(store));
    let network = crate::network::parse_network(network)
        .map_err(|e| ScannerError::Internal(e.to_string()))?;
    let inputs = crate::work::RoundInputs {
        db: &db,
        custody: key_custody,
        daemon,
        network,
        tenants,
        reorg_check_depth,
        grace_period_seconds: expired_order_grace_period_seconds,
        scan_chunk_memory_budget_mb,
        order_event_retention_secs: crate::store::DEFAULT_ORDER_EVENT_RETENTION_SECS,
    };
    crate::work::run_round(state, &inputs, state.tuning().round_budget)
        .await
        .into_result()
}

/// Registers the keys of every enabled tenant on `network` that has none
/// registered yet, from its sealed key material, and adds the handles to
/// `wallet_handles`.
///
/// Registration at boot can fail (a key-custody backend that wasn't up
/// yet), and until a tenant's keys are registered its payments can't be
/// detected; this lets the scan loop keep retrying rather than waiting for
/// an API call to register them lazily. Returns how many were registered.
///
/// Uses the same "first handle in wins" rule as
/// `http::resolve_wallet_handle`, which may be registering the same tenant
/// at the same time: the losing registration is removed again.
/// With `handled_epoch`, first asks the key-custody backend whether it still
/// holds the wallets registered with it (`KeyCustody::check_state`, task
/// 5.8). If it has lost them since `handled_epoch` (a backend in another
/// process that restarted with empty memory), every handle in `wallet_handles` is
/// useless, so the map is cleared (once per epoch, however many network
/// loops notice) and this network's tenants are registered again from their
/// sealed material. Nobody has to enter keys again.
pub async fn register_missing_wallets_checking_state(
    store: &crate::store::SharedStore,
    key_custody: &dyn KeyCustody,
    wallet_handles: &parking_lot::RwLock<HashMap<crate::store::TenantId, WalletHandle>>,
    handled_epoch: Option<&std::sync::atomic::AtomicU64>,
    network: monero::Network,
) -> usize {
    let db = crate::store::Db::over_shared(Arc::clone(store));
    register_missing_wallets_reporting(&db, key_custody, wallet_handles, handled_epoch, network)
        .await
        .registered
}

/// [`register_missing_wallets_checking_state`], also saying how many
/// registrations failed.
pub async fn register_missing_wallets_reporting(
    db: &crate::store::Db,
    key_custody: &dyn KeyCustody,
    wallet_handles: &parking_lot::RwLock<HashMap<crate::store::TenantId, WalletHandle>>,
    handled_epoch: Option<&std::sync::atomic::AtomicU64>,
    network: monero::Network,
) -> Registration {
    const REGISTRATION_CALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
    const REGISTRATION_PASS_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);
    const REGISTRATIONS_PER_PASS: usize = 16;
    static NEXT_REGISTRATION_OFFSET: std::sync::LazyLock<
        parking_lot::Mutex<HashMap<String, usize>>,
    > = std::sync::LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));
    let pass_end = tokio::time::Instant::now() + REGISTRATION_PASS_DEADLINE;
    if let Some(handled_epoch) = handled_epoch {
        match tokio::time::timeout_at(
            pass_end.min(tokio::time::Instant::now() + REGISTRATION_CALL_DEADLINE),
            key_custody.check_state(),
        ).await {
            Ok(Ok(epoch)) => {
                // Whichever loop moves the handled epoch up acts on it; the
                // others see it already there.
                if handled_epoch.fetch_max(epoch, std::sync::atomic::Ordering::SeqCst) < epoch {
                    // A single backend can't say which handles it lost, so
                    // they all go; a router forgets just the lost backend's
                    // (`handle_is_live` below).
                    if key_custody.enabled_backends().is_empty() {
                        let dropped = {
                            let mut handles = wallet_handles.write();
                            let n = handles.len();
                            handles.clear();
                            n
                        };
                        tracing::warn!(stores = dropped, "key custody lost its wallets: registering them all again from their sealed keys");
                    }
                }
            }
            Ok(Err(e)) => tracing::warn!("checking the key custody backend's state failed (retried later): {e}"),
            Err(_) => tracing::warn!("checking the key custody backend's state exceeded {REGISTRATION_CALL_DEADLINE:?} (retried later)"),
        }
    }
    // On the database worker: this runs from the async scanner loop.
    let listed = db
        .run(crate::store::db::Class::Scanner, |s| {
            s.list_active_tenants()
        })
        .await;
    let on_network: Vec<crate::store::Tenant> = match listed {
        Ok(tenants) => tenants
            .into_iter()
            .filter(|t| t.network == crate::network::network_str(network))
            .collect(),
        Err(e) => {
            tracing::warn!(network = crate::network::network_str(network), error = %e, "listing stores to register their keys failed (retried later)");
            return Registration {
                registered: 0,
                failed: 1,
            };
        }
    };
    // This network's handles in a backend that was turned off, or that lost
    // its wallets, are dropped: the store is registered again below (if its
    // backend is enabled) or left unserved until it is. Only this network's:
    // another network's loop notices its own lost handles and retries soon.
    {
        let ids: HashSet<&str> = on_network.iter().map(|t| t.id.as_str()).collect();
        wallet_handles
            .write()
            .retain(|id, handle| !ids.contains(id.as_str()) || key_custody.handle_is_live(*handle));
    }
    let mut missing: Vec<crate::store::Tenant> = {
        let handles = wallet_handles.read();
        on_network
            .into_iter()
            .filter(|t| !handles.contains_key(&t.id))
            .collect()
    };
    // Rotate a bounded pass. A failed tenant at the front must not prevent
    // later tenants from ever getting a registration attempt or block scan.
    missing.sort_by(|a, b| a.id.cmp(&b.id));
    let deferred = missing.len().saturating_sub(REGISTRATIONS_PER_PASS);
    if !missing.is_empty() {
        // Keep offsets per network. A single global counter can starve a
        // network when two loops alternate and both have 32 missing tenants.
        let offset = {
            let mut offsets = NEXT_REGISTRATION_OFFSET.lock();
            let next = offsets
                .entry(crate::network::network_str(network).to_owned())
                .or_default();
            let offset = *next % missing.len();
            *next = next.wrapping_add(REGISTRATIONS_PER_PASS);
            offset
        };
        missing.rotate_left(offset);
        missing.truncate(REGISTRATIONS_PER_PASS);
    }
    let mut registered = 0;
    let mut failed = deferred;
    let mut first_error: Option<(String, String)> = None;
    for tenant in missing {
        if tokio::time::Instant::now() >= pass_end {
            failed += 1;
            continue;
        }
        let enabled = key_custody.enabled_backends();
        if !enabled.is_empty() && !enabled.contains(&tenant.key_custody_backend) {
            continue; // its backend is off: left unserved, not an error to log every minute
        }
        match tokio::time::timeout_at(
            pass_end.min(tokio::time::Instant::now() + REGISTRATION_CALL_DEADLINE),
            key_custody.unseal_and_register_in_idempotent(
                &tenant.key_custody_backend,
                &tenant.sealed_key_material,
                tenant.id.as_str(),
            ),
        )
        .await
        {
            Ok(Ok(handle)) => {
                let winner = *wallet_handles
                    .write()
                    .entry(tenant.id.clone())
                    .or_insert(handle);
                if winner == handle {
                    registered += 1;
                } else {
                    crate::key_custody::remove_wallet_logged(
                        key_custody,
                        handle,
                        Some(tenant.id.as_str()),
                        "registering a store's keys, another task registered them first",
                    )
                    .await;
                }
            }
            Ok(Err(e)) => {
                failed += 1;
                first_error.get_or_insert_with(|| (tenant.id.to_string(), e.to_string()));
            }
            Err(_) => {
                failed += 1;
                first_error.get_or_insert_with(|| {
                    (
                        tenant.id.to_string(),
                        format!("registration exceeded {REGISTRATION_CALL_DEADLINE:?}"),
                    )
                });
            }
        }
    }
    if let Some((first_store, first_error)) = first_error {
        // One line per network, and not every retry: a backend that is down
        // would otherwise log every store it holds every few seconds.
        shared::throttled!(
            format!("register-failed:{}", crate::network::network_str(network)),
            warn,
            network = crate::network::network_str(network),
            stores = failed,
            store.id = %first_store,
            error = %first_error,
            "registering the keys of stores failed, retrying shortly (the first failure is shown)"
        );
    }
    Registration { registered, failed }
}

/// What one registration pass did: stores registered, and stores whose
/// registration failed (to be retried soon).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registration {
    pub registered: usize,
    pub failed: usize,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/verification/scanner/tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
mod paging_tests {
    use super::*;

    const T: &crate::work::ScanTuning = &crate::work::ScanTuning::DEFAULT;

    /// A block goes to pages when it would overrun one answer, or take the
    /// link over 30 seconds; one whose weight isn't known is fetched whole.
    #[test]
    fn a_block_is_paged_when_too_large_for_an_answer_or_the_link() {
        let cap = 32_000_000;
        assert!(!scan_in_pages(
            T,
            None,
            cap,
            Some(crate::link::LinkCost::transfer_only(1.0))
        ));
        assert!(!scan_in_pages(T, Some(300_000), cap, None));
        assert!(
            scan_in_pages(T, Some(200_000_000), cap, None),
            "over the cap"
        );
        assert!(
            scan_in_pages(
                T,
                Some(4_000_000),
                cap,
                Some(crate::link::LinkCost::transfer_only(100_000.0))
            ),
            "40 s at 100 kB/s"
        );
        assert!(
            !scan_in_pages(
                T,
                Some(2_000_000),
                cap,
                Some(crate::link::LinkCost::transfer_only(100_000.0))
            ),
            "20 s"
        );
    }

    /// A page is the fewest of what fits the cap, what the link sends in a
    /// target call and what the scan gets through in the round's share,
    /// from 1 to 100 transactions.
    #[test]
    fn a_page_is_sized_by_memory_link_and_cpu() {
        let page = |cap, rate, avg, cpu, remaining| next_page(T, cap, rate, avg, cpu, remaining);
        assert_eq!(
            page(32_000_000, None, 2_000.0, None, 10_000),
            ChunkPlan {
                blocks: PAGE_MAX_TXS,
                limited_by: ChunkLimit::Maximum
            }
        );
        assert_eq!(
            page(100_000, None, 2_000.0, None, 10_000),
            ChunkPlan {
                blocks: 50,
                limited_by: ChunkLimit::Memory
            }
        );
        assert_eq!(
            page(
                32_000_000,
                Some(crate::link::LinkCost::transfer_only(10_000.0)),
                2_000.0,
                None,
                10_000
            ),
            ChunkPlan {
                blocks: 20,
                limited_by: ChunkLimit::Link
            }
        );
        assert_eq!(
            page(32_000_000, None, 2_000.0, Some(0.5), 10_000),
            ChunkPlan {
                blocks: 8,
                limited_by: ChunkLimit::Cpu
            }
        );
        assert_eq!(
            page(32_000_000, None, 2_000.0, None, 7),
            ChunkPlan {
                blocks: 7,
                limited_by: ChunkLimit::Remaining
            }
        );
        // A transaction larger than the cap still makes a page of one.
        assert_eq!(page(256_000, None, 3_000_000.0, None, 10).blocks, 1);
    }
}
