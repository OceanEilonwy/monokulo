//! Chain scanning: matching transactions against active tenants, and reorg /
//! double-spend detection. See `docs/DESIGN.md` §7.
//!
//! Deliberately built against the `MoneroDaemonClient` trait rather than a live
//! node, and against `Store` directly rather than the (not-yet-built) writer-actor
//! wrapper - the correctness of the reconciliation logic doesn't depend on which
//! thread runs it, only on doing the right thing with what the daemon reports.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

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
            "empty key-image list".to_string(),
        ));
    }
    if images
        .iter()
        .any(|image| image.len() != 64 || !image.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(ScannerError::InvalidPaymentEvidence(
            "malformed key image".to_string(),
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
/// output, no `Store` involved. Deliberately separate from persisting it (see
/// `record_scan_match`): `rusqlite::Connection` is `Send` but not `Sync`, so a
/// `&Store` reference held across an `.await` (as combining these two steps into
/// one async function would require, since `KeyCustody::scan_tx_outputs` awaits)
/// makes the containing future `!Send` - fine for a future only ever `.await`ed
/// directly inside another task (every test in this file does that), but fatal the
/// moment anything containing it is handed to `tokio::spawn`, which requires the
/// whole future to be `Send + 'static`. `engine::run_scan_tick` is spawned in
/// production (`main.rs`), so this split isn't a style preference - the combined
/// version simply cannot be spawned. Caught by the compiler, not a test, the first
/// time this code was actually wired into a spawned task rather than awaited
/// directly in a test.
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
                let (TxOutTarget::ToKey { key } | TxOutTarget::ToTaggedKey { key, .. }) =
                    &prefix.outputs.get(m.output_index)?.target;
                Some((m.output_index, hex::encode(key)))
            })
            .collect();
        ScanResult {
            matches,
            txid: txid.to_string(),
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

/// Longest one tenant's scan of one batch of transactions may take before it
/// counts as a failure for that tenant (task 7.4). A key-custody backend that
/// answers, but slowly, is then treated like one that is down: that tenant is
/// left behind and caught up later, and nobody else waits on it.
pub const SCAN_CALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Most tenants scanned at the same time.
pub(crate) const SCAN_CONCURRENCY: usize = 32;

/// Scans a run of transactions for many tenants at once: one key-custody
/// call per tenant for the whole run, each with `SCAN_CALL_DEADLINE`, so a
/// slow tenant (a slow key-custody backend) doesn't hold up the others.
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
) -> Vec<(crate::store::TenantId, Result<Vec<ScanResult>>)> {
    use futures_util::stream::{self, StreamExt};
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
                        SCAN_CALL_DEADLINE,
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
                                "scan took longer than {SCAN_CALL_DEADLINE:?}"
                            )),
                        )),
                    };
                    (tenant_id.clone(), result)
                },
                span,
            )
        })
        .buffered(SCAN_CONCURRENCY)
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
) -> Vec<(crate::store::TenantId, Result<Option<ScanResult>>)> {
    let tx = crate::daemon::ScanTx::of(tx);
    let inputs = [tx.input.clone()];
    let tenants: Vec<_> = tenants.iter().map(|tenant| (*tenant, 0)).collect();
    scan_txs_for_tenants(
        key_custody,
        std::slice::from_ref(&txid.to_string()),
        std::slice::from_ref(&tx),
        &inputs,
        &tenants,
    )
    .await
    .into_iter()
    .map(|(tenant_id, result)| (tenant_id, result.map(|mut found| found.pop())))
    .collect()
}

/// Persists a `ScanResult` against one tenant. Purely synchronous - no `.await`
/// anywhere in this function, so a `&Store` parameter here is never an issue.
/// Returns the set of order ids touched, so the caller knows which orders need
/// `Store::recompute_order_status`.
pub fn record_scan_match(
    store: &Store,
    tenant_id: &crate::store::TenantId,
    scan: &ScanResult,
    seen_at: i64,
    block_height: Option<u64>,
) -> Result<HashSet<crate::store::OrderId>> {
    let mut touched = HashSet::new();
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
        store.stage_partial_match(crate::store::StagedMatch {
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
        ScannerError::Store(crate::store::StoreError::Sqlite(
            rusqlite::Error::ToSqlConversionFailure(Box::new(e)),
        ))
    })
}

/// What one `check_vanished_mempool_payments` sweep concluded: orders
/// whose payments changed, and the reorg point is not part of it (this sweep
/// is not about the chain changing shape).
pub struct VanishedPoolReport {
    /// Orders whose payments changed and therefore need a status recompute.
    pub dirty_orders: Vec<crate::store::OrderId>,
    /// The subset of `dirty_orders` where a payment was voided on affirmative
    /// double-spend proof - the `order.double_spend_detected` webhook's trigger.
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
    locations: std::collections::HashMap<String, TxLocation>,
    /// For payments whose transaction is nowhere: whether a double spend is
    /// proven, by payment id.
    proven: std::collections::HashMap<i64, bool>,
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
    let mut spans: Vec<(i64, std::ops::Range<usize>)> = Vec::new();
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
    let mut dirty_orders = HashSet::new();
    let mut double_spent_orders = HashSet::new();
    let mut unresolved = Vec::new();

    for payment in unconfirmed {
        if mempool_txids.contains(&payment.txid) {
            continue; // still pending in the pool - nothing has been decided about it yet
        }
        let mut location = match hints.locations.get(&payment.txid) {
            Some(location) => *location,
            None => daemon.locate_transaction(&payment.txid).await?,
        };
        // Nowhere according to one node is not nowhere: every node is asked
        // before the key-image evidence can void it (see
        // `MoneroDaemonClient::locate_transaction_corroborated`).
        if location == TxLocation::NotFound {
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

/// Enqueues a webhook delivery for every enabled webhook on `order_id`'s tenant.
/// Internal orchestration helper - routes through `Store::get_order_tenant_id`
/// (unscoped by design, see its doc comment) since the scanner only has a bare
/// `order_id` at this point, not a tenant-authenticated request.
///
/// Wraps whatever event-specific fields the caller supplies in a common envelope
/// carrying an `event_id`, the `event` type, and a `created_at` timestamp. The
/// `event_id` is minted once per *event* here and baked into the stored payload, so
/// every retry of that delivery re-sends the identical id under the identical
/// signature: that is what lets a receiver tell "the same notification again, my ack
/// must have been lost" apart from "a genuine second transition to the same status",
/// which the previous `{order_id, status}` payload made indistinguishable. The
/// timestamp being inside the signed body (rather than only an unsigned header) is
/// what stops a captured delivery from being replayable against the merchant
/// indefinitely.
fn enqueue_webhook_event(
    store: &Store,
    order_id: &crate::store::OrderId,
    event_type: &str,
    fields: &[(&str, &str)],
    now: i64,
) -> Result<()> {
    // The caller has just written this order in the same transaction.
    let tenant_id = store
        .get_order_tenant_id(order_id)?
        .ok_or(crate::store::StoreError::NotFound)?;
    let webhooks: Vec<_> = store
        .list_webhooks(&tenant_id)?
        .into_iter()
        .filter(|w| w.enabled)
        .collect();
    if webhooks.is_empty() {
        return Ok(());
    }

    let mut envelope: serde_json::Map<String, serde_json::Value> = fields
        .iter()
        .map(|(name, value)| ((*name).to_string(), serde_json::Value::from(*value)))
        .collect();
    envelope.insert("event_id".into(), serde_json::json!(new_event_id()));
    envelope.insert("event".into(), serde_json::json!(event_type));
    envelope.insert("created_at".into(), serde_json::json!(now));
    let body = serde_json::Value::Object(envelope).to_string();

    for webhook in webhooks {
        store.enqueue_webhook_delivery(&webhook.id, order_id, event_type, &body, now)?;
    }
    Ok(())
}

fn new_event_id() -> String {
    format!("evt_{}", uuid::Uuid::new_v4().simple())
}

/// Recomputes an order's status and, on an actual transition, enqueues an
/// `order.<status>` webhook event for every one of its tenant's webhooks. This is
/// the *only* place status-transition webhooks are enqueued - see
/// `docs/DESIGN.md` §11 for why that's a transition-triggered event, never a
/// per-confirmation-count tick.
///
/// The status write and the enqueue it implies happen in one transaction. They are
/// not independently retryable: `recompute_order_status` decides "did anything
/// change" by comparing against the *stored* status, so the instant the new status
/// commits the transition stops being detectable. Enqueueing separately meant a
/// transient store error (or a crash) in the window between them didn't postpone the
/// merchant's `order.paid`, it destroyed it - permanently, for an order that really
/// is paid. Rolling the status back with the failed enqueue leaves the next tick to
/// redo both.
///
/// Public so `engine-test-support` can settle an order the same way a real
/// scan does (`TestEngineHandle::mark_order_paid`).
pub fn recompute_and_notify(
    store: &Store,
    order_id: &crate::store::OrderId,
    current_height: u64,
    now: i64,
) -> Result<()> {
    store.in_transaction(|store| recompute_and_notify_in_tx(store, order_id, current_height, now))
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
) -> Result<()> {
    let (old_status, new_status) = store.recompute_order_status(order_id, current_height, now)?;
    if old_status != new_status {
        enqueue_webhook_event(
            store,
            order_id,
            &format!("order.{new_status}"),
            &[
                ("order_id", order_id.as_str()),
                ("status", new_status.as_str()),
            ],
            now,
        )?;
    }
    Ok(())
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
    .await?;
    Ok(true)
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
) -> Result<()> {
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
) -> Result<()> {
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
    enqueue_webhook_event(
        store,
        order_id,
        "order.double_spend_detected",
        &[("order_id", order_id.as_str())],
        now,
    )
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
            .all(|p| p.voided_at.is_none())
        {
            store.clear_double_spend_flag(order_id)?;
        }
        recompute_and_notify_in_tx(store, order_id, current_height, now)?;
        enqueue_webhook_event(
            store,
            order_id,
            "order.double_spend_reversed",
            &[("order_id", order_id.as_str()), ("txid", txid)],
            now,
        )?;
        Ok(true)
    })
}

/// How far back the upkeep tier's void recheck (`work::upkeep`, `docs/DESIGN.md`
/// §7.7) looks for voided payments to recheck. Bounded deliberately: a void that turns out to have been a false
/// accusation is exactly as worth correcting a day later as a minute later - unlike
/// zero-conf detection, there is no latency requirement to trade away here - and an
/// old, long-settled void is not worth the cost of rechecking forever: if it were
/// wrong, the merchant and customer have long since moved on regardless.
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
    let (order_id, txid, output) = (
        payment.order_id.clone(),
        payment.txid.clone(),
        payment.output_index,
    );
    let restored = db
        .run(crate::store::db::Class::Scanner, move |s| {
            unvoid_as_false_positive(
                s,
                &order_id,
                &txid,
                output,
                block_height,
                current_height,
                now,
            )
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
) -> Result<std::collections::HashMap<i64, Vec<KeyImageStatus>>> {
    let mut images: Vec<String> = Vec::new();
    let mut spans: Vec<(i64, std::ops::Range<usize>)> = Vec::new();
    for payment in payments {
        if let Ok(own) = parse_payment_key_images(&payment.key_images_json) {
            spans.push((payment.id, images.len()..images.len() + own.len()));
            images.extend(own);
        }
    }
    if spans.len() < 2 {
        return Ok(Default::default());
    }
    let statuses = crate::work::bounded(daemon.is_key_image_spent_corroborated(&images)).await?;
    if statuses.len() != images.len() {
        return Ok(Default::default());
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
/// Never request more than this many blocks in one call, regardless of how
/// small `avg_bytes_per_block` has drifted (e.g. a long run of near-empty
/// blocks) - `payment.scan_chunk_memory_budget_mb` alone would technically
/// allow an enormous request in that case, and an older monerod ignoring
/// `get_blocks.bin`'s own `max_block_count` hint has no other backstop
/// against that.
pub(crate) const SCAN_CHUNK_MAX_BLOCKS: u64 = 500;
/// How fast the running average of bytes-per-block reacts to a real chunk's
/// own observed size - `0.3` weighs recent chunks heavily (so a genuine shift
/// in block size, e.g. catching up through a period of network congestion,
/// is reflected within a handful of chunks) without letting one anomalous
/// chunk (a single giant consolidation tx, or a run of empty blocks) swing
/// the next chunk's size wildly.
const SCAN_CHUNK_EWMA_ALPHA: f64 = 0.3;
/// The cold-start estimate for bytes-per-block, before any chunk in this tick
/// has actually been fetched - deliberately conservative (real average block
/// sizes on a healthy network are often smaller than this), so the very
/// first chunk of a catch-up walk undershoots `scan_chunk_memory_budget_mb`
/// rather than overshoots it. Self-correcting from the second chunk onward
/// regardless.
pub(crate) const SCAN_CHUNK_INITIAL_AVG_BYTES: f64 = 50_000.0;

/// How long one block request should take, at the node's measured transfer
/// rate (docs/engine_scaling.md section 2). This is the Blocks tier's
/// reserved share of a round, so 4 s today (40 % of the 10 s
/// [`crate::work::ROUND_BUDGET`]).
///
/// It is a time and not a size because the scheduler divides time, not
/// bytes. A round gives each tier a share of its seconds, and a tier always
/// runs at least one unit, which can't be stopped part-way through a node
/// request. One block request is therefore the smallest delay the Blocks
/// tier can cause the tiers after it: the mempool tier (zero-confirmation
/// payments), settlement, and upkeep. A request sized in bytes alone takes
/// milliseconds on a LAN node and minutes over Tor. Sized by the link's
/// rate, it fits the Blocks tier's share on any link.
///
/// It doesn't limit throughput. A round that ends with blocks left is
/// followed at once by the next, so a catch-up walk keeps the link about
/// as busy as it would be with larger requests. Larger requests would only
/// spread the fixed round trip over more bytes. At 4 s, a 1 s round trip
/// (the cold-start guess) is a fifth of a call, and a typical 100 ms is
/// 2.5 %.
///
/// On a fast link this limit rarely applies: the response cap (an eighth
/// of `payment.scan_chunk_memory_budget_mb`, 1 MB at the default 8 MB)
/// binds first. The link limit takes over below the cap divided by this
/// many seconds, about 2 Mbit/s at the default budget. Slow nodes, Tor
/// nodes and large budgets all fall below that.
///
/// A request's timeout is three times what the link says it needs, and
/// never under 15 s ([`crate::link::timeout_for`]). Three times this target
/// fits within that floor (checked below), so a request sized to the target
/// gets nearly four times as long as it should need. If the rate estimate
/// is out of date, the request runs slow but doesn't fail.
///
/// The target counts transfer time only, not the round trip or the node's
/// time to first byte for each block. A link-limited request can therefore
/// take somewhat longer than this. Its timeout
/// ([`crate::link::Link::timeout_for_blocks`]) counts every term.
///
/// Derived from the round so that changing [`crate::work::ROUND_BUDGET`]
/// or the tier shares can't leave it stale. [`next_page`] uses the same
/// share for a large block's pages.
pub(crate) const SCAN_CHUNK_TARGET_CALL_SECS: f64 = crate::work::Tier::Blocks.reserved_secs();

const _: () = assert!(
    SCAN_CHUNK_TARGET_CALL_SECS * crate::link::SAFETY <= crate::link::MIN_TIMEOUT.as_secs_f64(),
    "a request sized to the target call must keep the minimum timeout"
);

/// One response's share of the scan memory budget: the raw answer and its
/// parse copies are held at once, so each answer is kept to a fraction of
/// what the block cache may hold (docs/engine_scaling.md section 3).
pub(crate) const RESPONSE_SHARE_OF_BUDGET: u64 = 8;

/// The largest block response to ask for under a `budget_mb` scan budget:
/// an eighth of it, and never less than 256 kB, so a tiny budget still
/// fetches whole blocks.
pub(crate) fn response_cap_bytes(budget_mb: u32) -> u64 {
    (u64::from(budget_mb) * 1024 * 1024 / RESPONSE_SHARE_OF_BUDGET).max(256 * 1024)
}

pub use shared::scaling::{ChunkLimit, ChunkPlan};

/// Pure sizing decision, extracted so it's directly, cheaply unit-testable:
/// the smaller of what fits the response cap and what the link delivers in
/// a target call, at the running bytes-per-block average, within 1..=500
/// and the blocks that remain. A link not measured (`None`) doesn't limit.
pub(crate) fn next_scan_chunk(
    response_cap_bytes: u64,
    rate_bytes_per_sec: Option<f64>,
    avg_bytes_per_block: f64,
    remaining: u64,
) -> ChunkPlan {
    let avg = avg_bytes_per_block.max(1.0);
    let by_memory = (response_cap_bytes as f64 / avg).floor();
    let by_time = rate_bytes_per_sec.map_or(f64::INFINITY, |rate| {
        (rate * SCAN_CHUNK_TARGET_CALL_SECS / avg).floor()
    });
    let (wanted, mut limited_by) = if by_time < by_memory {
        (by_time, ChunkLimit::Link)
    } else {
        (by_memory, ChunkLimit::Memory)
    };
    let mut blocks = if wanted >= SCAN_CHUNK_MAX_BLOCKS as f64 {
        limited_by = ChunkLimit::Maximum;
        SCAN_CHUNK_MAX_BLOCKS
    } else {
        (wanted as u64).max(SCAN_CHUNK_MIN_BLOCKS)
    };
    if remaining < blocks {
        blocks = remaining.max(SCAN_CHUNK_MIN_BLOCKS).min(blocks);
        limited_by = ChunkLimit::Remaining;
    }
    ChunkPlan { blocks, limited_by }
}

/// A block that would take longer than this to fetch whole at its node's
/// measured rate is scanned a page of transactions at a time instead
/// (docs/engine_scaling.md section 4): well inside the two minutes after
/// which a block counts as slow.
pub(crate) const WHOLE_BLOCK_MAX_SECS: f64 = 30.0;

/// Most transactions on one page of a large block: what monerod's
/// restricted RPC (a public node's) gives in one `/get_transactions`
/// answer, so a page is one request.
pub(crate) const PAGE_MAX_TXS: u64 = 100;

/// Whether a block of `weight` bytes is scanned in pages rather than
/// fetched whole: it would overrun one response (the cap from the scan
/// memory budget), or take longer than [`WHOLE_BLOCK_MAX_SECS`] at the
/// link's measured rate. A block whose weight the node didn't give is
/// fetched whole, as before.
pub(crate) fn scan_in_pages(
    weight: Option<u64>,
    response_cap_bytes: u64,
    rate_bytes_per_sec: Option<f64>,
) -> bool {
    let Some(weight) = weight else {
        return false;
    };
    weight > response_cap_bytes
        || rate_bytes_per_sec
            .is_some_and(|rate| weight as f64 / rate.max(1.0) > WHOLE_BLOCK_MAX_SECS)
}

/// How many transactions the next page of a large block holds
/// (docs/engine_scaling.md section 4): the fewest of what fits the response
/// cap and what the link delivers in a target call, at `avg_tx_bytes` a
/// transaction, and what the scan gets through in `scan_slice_secs` when a
/// transaction costs `scan_secs_per_tx` (for every store scanned for);
/// within 1..=[`PAGE_MAX_TXS`] and the transactions that remain. A link not
/// measured or a scan cost not known yet doesn't limit.
pub(crate) fn next_page(
    response_cap_bytes: u64,
    rate_bytes_per_sec: Option<f64>,
    avg_tx_bytes: f64,
    scan_secs_per_tx: Option<f64>,
    scan_slice_secs: f64,
    remaining: u64,
) -> ChunkPlan {
    let avg = avg_tx_bytes.max(1.0);
    let mut wanted = (response_cap_bytes as f64 / avg).floor();
    let mut limited_by = ChunkLimit::Memory;
    if let Some(by_time) =
        rate_bytes_per_sec.map(|rate| (rate * SCAN_CHUNK_TARGET_CALL_SECS / avg).floor())
    {
        if by_time < wanted {
            (wanted, limited_by) = (by_time, ChunkLimit::Link);
        }
    }
    if let Some(by_cpu) = scan_secs_per_tx
        .filter(|secs| *secs > 0.0)
        .map(|secs| (scan_slice_secs / secs).floor())
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
/// many blocks (docs/engine_scaling.md section 2). Successes bring it back
/// down through [`update_avg_bytes_per_block`].
pub(crate) fn avg_after_failed_fetch(avg_bytes_per_block: f64) -> f64 {
    (avg_bytes_per_block * 2.0).min(1e12)
}

/// Pure EWMA update, same reasoning as `next_scan_chunk_size` above - `chunk_
/// bytes`/`block_count` are already known before this is called, so this is
/// just the averaging formula on its own, testable without any daemon or
/// store at all.
pub(crate) fn update_avg_bytes_per_block(
    avg_bytes_per_block: f64,
    chunk_bytes: usize,
    block_count: usize,
) -> f64 {
    let observed_avg = chunk_bytes as f64 / block_count as f64;
    SCAN_CHUNK_EWMA_ALPHA * observed_avg + (1.0 - SCAN_CHUNK_EWMA_ALPHA) * avg_bytes_per_block
}

/// One scan round for one network (docs/scanner_microtasks.md): reorg
/// detection and reconciliation, new and lagging blocks, the mempool, status
/// recomputes and upkeep, each a bounded unit with a share of the round's
/// time. `network` scopes everything to one chain: the `daemon` passed in must
/// be the client for that same network, and `tenants` are the tenants whose
/// keys are registered (only those on `network` are ever scanned).
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
#[allow(clippy::too_many_arguments)] // one round's genuinely independent inputs
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
    let db = crate::store::Db::over_shared(store.clone());
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
    };
    crate::work::run_round(state, &inputs, crate::work::ROUND_BUDGET)
        .await
        .into_result()
}

/// Registers the keys of every enabled tenant on `network` that has none
/// registered yet, from its sealed key material, and adds the handles to
/// `wallet_handles`. Registration at boot can fail (a key-custody backend
/// that wasn't up yet), and until a tenant's keys are registered its
/// payments can't be detected; this lets the scan loop keep retrying rather
/// than waiting for an API call to register them lazily. Returns how many
/// were registered.
///
/// Uses the same "first handle in wins" rule as
/// `http::resolve_wallet_handle`, which may be registering the same tenant
/// at the same time: the losing registration is removed again.
/// With `handled_epoch`, first asks the key-custody backend whether it still
/// holds the wallets registered with it (`KeyCustody::check_state`, task
/// 5.8). If it has lost them since `handled_epoch` (a sidecar that
/// restarted with empty memory), every handle in `wallet_handles` is
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
    let db = crate::store::Db::over_shared(store.clone());
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
                .entry(crate::network::network_str(network).to_string())
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
                if winner != handle {
                    let _ = key_custody.remove_wallet(handle).await;
                } else {
                    registered += 1;
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
pub(crate) mod tests {
    //! Scenario coverage checklist for the scanner.
    //!
    //! Reorgs, double-spends and hostile or broken nodes are rare in production and
    //! catastrophic when mishandled, so this module is organised around *classes of
    //! real-world chain and network condition* rather than around functions. The map
    //! below exists so a future maintainer can see at a glance which conditions are
    //! claimed to be covered - and, just as importantly, which limitations are
    //! deliberate. `docs/TESTING.md` §3 is the prose companion to this list.
    //!
    //! 1. **Reorgs, every depth.**
    //!    `a_reorg_is_detected_at_its_true_fork_point_at_every_depth_the_window_covers`
    //!    (1 block through the full `reorg_check_depth` window, one case per depth);
    //!    `a_reorg_deeper_than_the_window_is_reported_at_the_window_edge_and_leaves_older_payments_alone`
    //!    (the documented limitation, §DESIGN.md 3);
    //!    `a_settled_order_is_walked_back_when_a_reorg_deeper_than_confirmations_required_orphans_its_payment`
    //!    (the September 2025 mainnet shape: 18 blocks against 10 confirmations);
    //!    `a_reorg_whose_fork_is_below_every_recorded_block_rewinds_to_that_oldest_block` and
    //!    `a_reorg_back_to_the_oldest_recorded_block_does_not_look_like_a_scanner_that_never_ran`
    //!    (common ancestor at, and below, the edge of the recorded window);
    //!    `reorg_moving_a_tx_to_a_different_block_updates_height_without_voiding`,
    //!    `a_payment_a_reorg_dropped_to_the_mempool_can_still_be_voided_when_later_proven_double_spent`
    //!    (the three outcomes a reorg can have for a payment: remined, pooled, gone).
    //! 2. **Selfish-mining-shaped rapid chain splits.**
    //!    `rapidly_alternating_chain_tips_never_lose_or_double_count_a_payment` - the tip
    //!    trading places repeatedly, the same transaction moving in and out of the
    //!    chain, which is what a withholding pool looks like from here.
    //! 3. **Double-spends, every variant.**
    //!    `reorg_where_tx_vanishes_and_key_image_proven_spent_elsewhere_voids_and_flags_double_spend`
    //!    (the base case); `reorg_where_tx_vanishes_but_key_image_still_unspent_does_not_void`
    //!    (never void on ambiguity); `a_double_spend_mined_in_a_different_block_than_the_original_voids_it_once`;
    //!    `voiding_one_of_an_orders_two_payments_leaves_the_other_counting`;
    //!    `a_voided_payment_is_restored_when_its_transaction_returns_to_the_chain` and
    //!    `a_third_transaction_claiming_the_same_inputs_keeps_the_original_voided`
    //!    (un-voiding, and the chained case where a *third* transaction takes the inputs);
    //!    `a_zero_conf_order_double_spent_out_of_the_mempool_is_voided_with_no_reorg_involved`
    //!    (the reorg-free mempool double-spend - see `check_vanished_mempool_payments`);
    //!    `a_mempool_payment_that_merely_disappears_is_never_voided_on_that_evidence_alone`
    //!    (dropped/evicted transactions, which Monero produces without any attacker).
    //! 4. **Daemon disconnects and network failures.**
    //!    `a_node_failing_partway_through_the_block_range_resumes_from_that_exact_block`;
    //!    `a_failed_mempool_poll_skips_the_vanished_payment_sweep_rather_than_assuming_an_empty_pool`;
    //!    `a_reconciliation_that_fails_partway_leaves_the_reorg_still_detectable` and
    //!    `a_key_image_lookup_failing_mid_reconciliation_leaves_the_reorg_detectable`
    //!    (failures at each step of reconciliation);
    //!    `a_request_that_times_out_rather_than_failing_fast_is_still_just_a_failed_tick`;
    //!    `a_void_that_lands_before_the_node_fails_still_updates_the_order_it_belongs_to`
    //!    (a committed void and everything that makes it visible are one transaction -
    //!    see `void_and_notify`);
    //!    `a_block_whose_hash_cannot_be_read_stops_the_range_instead_of_leaving_a_gap`;
    //!    `first_run_bootstrap_tolerates_a_daemon_reporting_a_tip_it_cannot_yet_serve`.
    //!    Store-side failures at the same boundaries:
    //!    `a_store_failure_recording_a_block_match_leaves_that_block_unscanned_for_the_next_tick`,
    //!    `a_store_failure_marking_a_block_scanned_does_not_discard_the_ticks_mempool_matches`,
    //!    `a_status_change_whose_webhook_cannot_be_enqueued_is_rolled_back_rather_than_lost`.
    //! 5. **Dishonest or swapped daemons** (see `docs/DESIGN.md` §7.7 for which of
    //!    these are closed and which are accepted trust boundaries).
    //!    `swapping_to_a_daemon_serving_a_different_chain_reconciles_exactly_like_a_reorg`
    //!    (a node presenting a different history is handled by, and is indistinguishable
    //!    from, ordinary reorg detection - which is why no per-daemon sync state exists);
    //!    `a_daemon_that_omits_a_transaction_from_its_mempool_only_delays_detection`;
    //!    `a_transaction_a_daemon_invents_cannot_become_a_payment_unless_it_matches_a_wallet`;
    //!    `an_inflated_reported_height_inflates_confirmations_which_is_an_accepted_trust_boundary`;
    //!    `a_daemon_far_behind_the_recorded_high_water_mark_neither_rescans_nor_discards_its_window`.
    //!    The same scenarios composed through the real `daemon_fallback::FallbackDaemonClient`
    //!    rather than a hand-swapped daemon (see `docs/DESIGN.md` §7.7's "Fallback nodes widen
    //!    this trust boundary"):
    //!    `failing_over_through_a_real_fallback_client_to_a_node_serving_a_different_chain_reconciles_like_a_reorg`,
    //!    `failing_over_to_a_lagging_but_honest_fallback_neither_rewinds_nor_corrupts_the_window`,
    //!    `every_fallback_node_being_down_fails_the_tick_cleanly_without_corrupting_stored_state`,
    //!    and one genuinely new gap introduced by per-call failover (not merely inherited):
    //!    `a_node_that_dies_between_fetching_a_blocks_transactions_and_its_hash_can_pair_them_with_a_different_nodes_hash`.
    //! 6. **Many transactions per block.**
    //!    `several_transactions_in_one_block_paying_one_order_are_all_recorded_and_summed`;
    //!    `one_transactions_outputs_are_routed_to_whichever_order_owns_each_index`;
    //!    `one_tick_can_void_a_double_spent_payment_for_one_order_and_record_a_new_one_for_another`
    //!    (a payment and an unrelated void in one tick, across two orders sharing one
    //!    transaction - the cross-tenant case migration 0004 exists for).
    //! 7. **Everything else worth naming.**
    //!    `an_output_whose_amount_cannot_be_decrypted_is_skipped_not_recorded_as_zero` and
    //!    `an_amount_that_only_decrypts_on_a_later_tick_is_recorded_then_never_unrecorded`
    //!    (inconsistent amount recovery across ticks, both orderings);
    //!    `a_payment_completing_an_order_in_the_tick_its_deadline_passes_settles_rather_than_expiring`
    //!    (expiry versus payment within one tick, both sides of the boundary);
    //!    `a_chain_with_no_blocks_at_all_is_a_harmless_no_op_tick` and
    //!    `a_divergence_at_the_genesis_block_has_no_ancestor_to_re_anchor_to`
    //!    (degenerate chains: height 0, height 1, genesis divergence);
    //!    `the_scanned_block_window_stays_bounded_as_the_chain_grows`;
    //!    `a_tick_that_detects_a_reorg_never_announces_paid_from_the_chain_it_is_about_to_discard`
    //!    (intra-tick ordering); `the_transaction_variants_these_tests_are_built_from_are_genuinely_distinct`
    //!    (the fixtures the scenarios above are constructed from are what they claim).
    //!
    //! Deliberately *not* covered here, with reasons: clock skew between `now_unix()`
    //! and block timestamps (the scanner never reads a block timestamp - expiry is
    //! purely local wall-clock, so there is no interaction to test); two orders sharing
    //! one subaddress within a tenant (`UNIQUE(tenant_id, minor_index)` makes it
    //! unreachable, and store.rs's `concurrent_minor_index_allocation_never_duplicates`
    //! covers the allocation path that could otherwise reach it); proof-of-work,
    //! difficulty and timestamp validation of blocks (never performed - see
    //! §DESIGN.md 7.7).

    use super::*;

    /// A transaction's key images, hex, as a payment records them.
    fn key_images_of(tx: &Transaction) -> Vec<String> {
        crate::daemon::ScanTx::of(tx)
            .key_images
            .iter()
            .map(hex::encode)
            .collect()
    }

    #[test]
    fn stored_key_image_evidence_must_be_parseable_and_nonempty() {
        assert!(parse_payment_key_images("not json").is_err());
        assert!(parse_payment_key_images("[]").is_err());
        assert!(parse_payment_key_images("[\"image\"]").is_err());
        let image = "a".repeat(64);
        assert_eq!(
            parse_payment_key_images(&serde_json::json!([image.clone()]).to_string()).unwrap(),
            vec![image]
        );
    }
    use crate::daemon::fake::{tx_id_hex, FakeDaemonClient};
    use crate::daemon_fallback::{FallbackDaemonClient, FallbackNode};
    use crate::key_custody::{
        KeyCustodyError, MatchedOutput, Network, PlainKeyCustody, SubaddressIndex, WalletMaterial,
    };
    use crate::store::{NewOrder, NewTenant};
    use monero::consensus::encode::deserialize;
    use monero::{Address, PrivateKey, PublicKey};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Wraps a real `KeyCustody` and counts calls to `scan_tx_outputs` - the direct
    /// way to prove the active-watchlist optimization actually engages (§DESIGN.md
    /// §7.3), rather than trusting it by inspection. Delegates every other method
    /// unchanged.
    #[derive(Default)]
    struct CountingKeyCustody {
        inner: PlainKeyCustody,
        scan_calls: AtomicU64,
    }

    #[async_trait::async_trait]
    impl KeyCustody for CountingKeyCustody {
        async fn register_wallet(
            &self,
            material: WalletMaterial,
        ) -> std::result::Result<WalletHandle, KeyCustodyError> {
            self.inner.register_wallet(material).await
        }
        async fn remove_wallet(
            &self,
            handle: WalletHandle,
        ) -> std::result::Result<(), KeyCustodyError> {
            self.inner.remove_wallet(handle).await
        }
        async fn seal(
            &self,
            material: &WalletMaterial,
        ) -> std::result::Result<Vec<u8>, KeyCustodyError> {
            self.inner.seal(material).await
        }
        async fn unseal_and_register(
            &self,
            sealed: &[u8],
        ) -> std::result::Result<WalletHandle, KeyCustodyError> {
            self.inner.unseal_and_register(sealed).await
        }
        async fn derive_subaddress(
            &self,
            handle: WalletHandle,
            index: SubaddressIndex,
            network: Network,
        ) -> std::result::Result<Address, KeyCustodyError> {
            self.inner.derive_subaddress(handle, index, network).await
        }
        async fn scan_tx_outputs(
            &self,
            handle: WalletHandle,
            tx: &ScanInput,
            major_range: Range<u32>,
            minor_range: Range<u32>,
        ) -> std::result::Result<Vec<MatchedOutput>, KeyCustodyError> {
            self.scan_calls.fetch_add(1, Ordering::SeqCst);
            self.inner
                .scan_tx_outputs(handle, tx, major_range, minor_range)
                .await
        }
    }

    /// Wraps a `FakeDaemonClient` and fails exactly one call - `locate_transaction` -
    /// modelling the most ordinary thing that can go wrong halfway through reorg
    /// reconciliation: the node becomes briefly unreachable after the divergence has
    /// been detected but before the affected payments have been re-evaluated.
    struct DaemonFailingLocate {
        inner: FakeDaemonClient,
    }

    #[async_trait::async_trait]
    impl MoneroDaemonClient for DaemonFailingLocate {
        async fn get_height(&self) -> std::result::Result<u64, DaemonError> {
            self.inner.get_height().await
        }
        async fn get_block_hash(&self, height: u64) -> std::result::Result<String, DaemonError> {
            self.inner.get_block_hash(height).await
        }
        async fn get_chain_blocks(
            &self,
            start_height: u64,
            count: u64,
        ) -> std::result::Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
            self.inner.get_chain_blocks(start_height, count).await
        }
        async fn get_mempool_txids(&self) -> std::result::Result<Vec<String>, DaemonError> {
            self.inner.get_mempool_txids().await
        }
        async fn get_transactions_with_ids(
            &self,
            txids: &[String],
        ) -> std::result::Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
            self.inner.get_transactions_with_ids(txids).await
        }
        async fn locate_transaction(
            &self,
            _txid: &str,
        ) -> std::result::Result<TxLocation, DaemonError> {
            Err(DaemonError::Request(
                "simulated node failure mid-reconciliation".into(),
            ))
        }
        async fn is_key_image_spent(
            &self,
            key_images: &[String],
        ) -> std::result::Result<Vec<KeyImageStatus>, DaemonError> {
            self.inner.is_key_image_spent(key_images).await
        }
    }

    /// Wraps a `FakeDaemonClient` that can't serve exactly one height: asked for
    /// that block's hash, or for blocks starting there, it fails, and a run of
    /// blocks that reaches it stops short of it (a node may always send fewer
    /// blocks than it was asked for). Everything else works. Models a node that
    /// answers for some of what a tick asks and not the rest.
    struct DaemonFailingBlockHashAt {
        inner: FakeDaemonClient,
        failing_height: AtomicU64,
    }

    /// Sentinel for `failing_height` meaning "no height fails" - the wrapper is
    /// deliberately toggleable so one test can drive both the failing tick and the
    /// healthy tick that must recover from it.
    const NO_FAILING_HEIGHT: u64 = u64::MAX;

    #[async_trait::async_trait]
    impl MoneroDaemonClient for DaemonFailingBlockHashAt {
        async fn get_height(&self) -> std::result::Result<u64, DaemonError> {
            self.inner.get_height().await
        }
        async fn get_block_hash(&self, height: u64) -> std::result::Result<String, DaemonError> {
            if height == self.failing_height.load(Ordering::SeqCst) {
                return Err(DaemonError::Request(format!(
                    "simulated failure reading the hash of block {height}"
                )));
            }
            self.inner.get_block_hash(height).await
        }
        async fn get_chain_blocks(
            &self,
            start_height: u64,
            count: u64,
        ) -> std::result::Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
            let failing = self.failing_height.load(Ordering::SeqCst);
            if start_height == failing {
                return Err(DaemonError::Request(format!(
                    "simulated failure reading block {start_height}"
                )));
            }
            let below = failing.saturating_sub(start_height);
            self.inner
                .get_chain_blocks(
                    start_height,
                    if below == 0 { count } else { count.min(below) },
                )
                .await
        }
        async fn get_mempool_txids(&self) -> std::result::Result<Vec<String>, DaemonError> {
            self.inner.get_mempool_txids().await
        }
        async fn get_transactions_with_ids(
            &self,
            txids: &[String],
        ) -> std::result::Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
            self.inner.get_transactions_with_ids(txids).await
        }
        async fn locate_transaction(
            &self,
            txid: &str,
        ) -> std::result::Result<TxLocation, DaemonError> {
            self.inner.locate_transaction(txid).await
        }
        async fn is_key_image_spent(
            &self,
            key_images: &[String],
        ) -> std::result::Result<Vec<KeyImageStatus>, DaemonError> {
            self.inner.is_key_image_spent(key_images).await
        }
    }

    /// Which RPC a `DaemonFailingFrom` wrapper is watching. One method at a time,
    /// deliberately: every scenario below is about a *specific* call failing while
    /// its neighbours keep working, which is what distinguishes "the node blipped
    /// mid-tick" from "the node is down" (the latter needs no wrapper - a fake with
    /// no blocks in it already behaves that way).
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum DaemonCall {
        Height,
        /// Counts calls to `get_chain_blocks`, the block scan's one fetch for a
        /// run of blocks with their ids.
        ChainBlocks,
        /// Counts calls to `get_block_hash` - proves the deliberate scope limit
        /// (`docs/txid_lookup_and_scan_chunking_wbs.md` Part A's own "why the
        /// target moved" section): block-hash fetching for `scanned_blocks`
        /// stays one call per height regardless of how transaction-fetching is
        /// chunked.
        BlockHash,
        Mempool,
        Locate,
        KeyImageSpent,
    }

    /// Wraps a `FakeDaemonClient`, counting calls to one chosen RPC and failing them
    /// from the Nth (0-based) onwards. The generalisation of `DaemonFailingLocate`
    /// and `DaemonFailingBlockHashAt` above, which each hard-code one method: those
    /// stay as they are (their tests read better naming the specific failure), this
    /// one covers the mid-tick failures that need to land on a *particular call* of
    /// a repeated RPC - the second block of a range, say, rather than the first.
    ///
    /// Doubles as a call counter (`fail_from = NO_FAILING_CALL` never fails), which
    /// is how the tests below assert that the vanished-payment sweep costs no RPCs
    /// in the ordinary case rather than trusting that by inspection.
    ///
    /// Generic over the client it wraps so two of these can be stacked - one method
    /// failing while another is counted - which is what "prove the sweep was skipped
    /// *because* the poll failed" needs.
    struct DaemonFailingFrom<D: MoneroDaemonClient> {
        inner: D,
        method: DaemonCall,
        fail_from: AtomicU64,
        calls: AtomicU64,
        /// Milliseconds to wait before the failure surfaces - the difference between
        /// a connection refused instantly and a request that hangs until its client
        /// timeout expires (`RpcDaemonClient` sets 15s). Zero for everything that
        /// doesn't care.
        delay_ms: u64,
    }

    /// Sentinel for `fail_from`: count, never fail.
    const NO_FAILING_CALL: u64 = u64::MAX;

    impl<D: MoneroDaemonClient> DaemonFailingFrom<D> {
        fn counting(inner: D, method: DaemonCall) -> Self {
            Self {
                inner,
                method,
                fail_from: AtomicU64::new(NO_FAILING_CALL),
                calls: AtomicU64::new(0),
                delay_ms: 0,
            }
        }
        fn failing_from(inner: D, method: DaemonCall, nth: u64) -> Self {
            Self {
                inner,
                method,
                fail_from: AtomicU64::new(nth),
                calls: AtomicU64::new(0),
                delay_ms: 0,
            }
        }
        fn timing_out_from(inner: D, method: DaemonCall, nth: u64, delay_ms: u64) -> Self {
            Self {
                inner,
                method,
                fail_from: AtomicU64::new(nth),
                calls: AtomicU64::new(0),
                delay_ms,
            }
        }
        fn call_count(&self) -> u64 {
            self.calls.load(Ordering::SeqCst)
        }
        fn stop_failing(&self) {
            self.fail_from.store(NO_FAILING_CALL, Ordering::SeqCst);
        }
        async fn gate(&self, method: DaemonCall) -> std::result::Result<(), DaemonError> {
            if method != self.method {
                return Ok(());
            }
            let nth = self.calls.fetch_add(1, Ordering::SeqCst);
            if nth >= self.fail_from.load(Ordering::SeqCst) {
                if self.delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
                }
                return Err(DaemonError::Request(format!(
                    "simulated {method:?} failure on call {nth}"
                )));
            }
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl<D: MoneroDaemonClient> MoneroDaemonClient for DaemonFailingFrom<D> {
        /// Headers from the node's own headers, as a real node answers
        /// them: never through the (counted) block fetch.
        async fn get_chain_headers(
            &self,
            start_height: u64,
            count: u64,
        ) -> std::result::Result<Vec<crate::daemon::ChainHeader>, DaemonError> {
            self.inner.get_chain_headers(start_height, count).await
        }
        async fn get_height(&self) -> std::result::Result<u64, DaemonError> {
            self.gate(DaemonCall::Height).await?;
            self.inner.get_height().await
        }
        async fn get_block_hash(&self, height: u64) -> std::result::Result<String, DaemonError> {
            self.gate(DaemonCall::BlockHash).await?;
            self.inner.get_block_hash(height).await
        }
        async fn get_chain_blocks(
            &self,
            start_height: u64,
            count: u64,
        ) -> std::result::Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
            self.gate(DaemonCall::ChainBlocks).await?;
            self.inner.get_chain_blocks(start_height, count).await
        }
        /// The pool poll is what `DaemonCall::Mempool` gates; fetching the
        /// bodies of new txids isn't a second poll, so it passes through.
        async fn get_mempool_txids(&self) -> std::result::Result<Vec<String>, DaemonError> {
            self.gate(DaemonCall::Mempool).await?;
            self.inner.get_mempool_txids().await
        }
        async fn get_transactions_with_ids(
            &self,
            txids: &[String],
        ) -> std::result::Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
            self.inner.get_transactions_with_ids(txids).await
        }
        async fn locate_transaction(
            &self,
            txid: &str,
        ) -> std::result::Result<TxLocation, DaemonError> {
            self.gate(DaemonCall::Locate).await?;
            self.inner.locate_transaction(txid).await
        }
        async fn is_key_image_spent(
            &self,
            key_images: &[String],
        ) -> std::result::Result<Vec<KeyImageStatus>, DaemonError> {
            self.gate(DaemonCall::KeyImageSpent).await?;
            self.inner.is_key_image_spent(key_images).await
        }
    }

    /// The same real fixture transaction and view pair used in
    /// `src/key_custody/plain.rs`'s tests - it pays subaddress 0/1. Reused here so
    /// scanner-level tests exercise real crypto end to end, not a stub that assumes
    /// matching works.
    /// Scans a whole transaction under the id it hashes to.
    async fn scan_transaction(
        key_custody: &dyn KeyCustody,
        handle: WalletHandle,
        tx: &Transaction,
        minor_range: Range<u32>,
    ) -> Result<ScanResult> {
        scan_transaction_as(key_custody, handle, &tx_id_hex(tx), tx, minor_range).await
    }

    /// Scans a whole transaction for one store and records what it pays.
    #[allow(clippy::too_many_arguments)]
    async fn scan_transaction_for_tenant(
        store: &Store,
        key_custody: &dyn KeyCustody,
        handle: WalletHandle,
        tenant_id: &crate::store::TenantId,
        tx: &Transaction,
        minor_range: Range<u32>,
        seen_at: i64,
        block_height: Option<u64>,
    ) -> Result<HashSet<crate::store::OrderId>> {
        let scan = scan_transaction(key_custody, handle, tx, minor_range).await?;
        record_scan_match(store, tenant_id, &scan, seen_at, block_height)
    }

    pub(crate) struct ReconcileReport {
        /// Lowest height at which the canonical chain diverged from what was
        /// previously scanned, if any reorg was detected this call.
        pub reorg_detected_at: Option<u64>,
        /// Orders whose payments changed in a way that warrants a status recompute.
        pub dirty_orders: Vec<crate::store::OrderId>,
        /// The subset of `dirty_orders` where the change was specifically a proven
        /// double-spend (a payment voided because its key image was confirmed spent by
        /// a different transaction) - callers use this to enqueue the independent
        /// `order.double_spend_detected` webhook event (see `docs/DESIGN.md` §11),
        /// separate from whatever `order.<status>` event the recompute may also imply.
        pub double_spent_orders: Vec<crate::store::OrderId>,
    }

    /// Checks for a reorg within the last `reorg_check_depth` blocks and, if one is
    /// found (or one is already being reconciled), runs its reconciliation as far
    /// as it can go now: every affected payment re-examined, then the rewind to
    /// the common ancestor. Never voids a payment on ambiguous evidence
    /// (§DESIGN.md 7.5) - only when `is_key_image_spent` affirmatively proves a
    /// different transaction consumed the same inputs.
    ///
    /// The scheduler runs the same work in bounded units (`work::chain`); this
    /// drives it to completion in one call, for callers and tests that want
    /// "reconcile now". The job is durable: a failure partway (an unreachable
    /// node) returns the error and leaves the job, and the losing chain's
    /// hashes, for the next call to finish.
    ///
    /// Not called by the engine: its scheduler's chain tier does this work a
    /// bounded unit at a time. A test driver over that same code.
    pub(crate) async fn check_for_reorg_and_reconcile(
        store: &crate::store::SharedStore,
        daemon: &dyn MoneroDaemonClient,
        network: &str,
        reorg_check_depth: u64,
        now: i64,
    ) -> Result<ReconcileReport> {
        use crate::work::chain::{Chain, JobStep};
        let crate::daemon::ChainTip {
            height: tip,
            hash: tip_hash,
        } = daemon.get_tip().await?;
        let db = crate::store::Db::over_shared(store.clone());
        let parsed = crate::network::parse_network(network)
            .map_err(|e| ScannerError::Internal(e.to_string()))?;
        let chain = Chain::new(&db, daemon, parsed, reorg_check_depth, now).with_tip_hash(tip_hash);
        if let Some(fork) = chain.detect(tip).await? {
            chain.open(fork).await?;
        }
        let reorg_detected_at = store.lock().reorg_job(parsed)?.map(|job| job.fork_height);
        let mut dirty_orders = HashSet::new();
        let mut double_spent_orders = HashSet::new();
        let mut attempted = HashSet::new();
        let mut failure = None;
        loop {
            match chain
                .advance_job(
                    tip,
                    &mut attempted,
                    tokio::time::Instant::now() + crate::work::ROUND_BUDGET,
                )
                .await
            {
                Ok(Some(JobStep::Collected)) => {}
                Ok(Some(JobStep::Processed {
                    reconciled,
                    failure: page_failure,
                })) => {
                    dirty_orders.extend(reconciled.dirty_orders);
                    double_spent_orders.extend(reconciled.double_spent_orders);
                    if let Some(error) = page_failure {
                        failure.get_or_insert(error);
                    }
                }
                Ok(None | Some(JobStep::Waiting) | Some(JobStep::Rewound)) => break,
                Err(error) => {
                    failure.get_or_insert(error);
                    break;
                }
            }
        }
        // The notifying recompute: a reorg-driven transition (`paid` ->
        // `confirming` when a tx falls back to the mempool, say) is as
        // webhook-worthy as a forward-scan-driven one. While the job is still
        // open, the store holds back any new settlement.
        {
            let s = store.lock();
            for order_id in &dirty_orders {
                recompute_and_notify(&s, order_id, tip, now)?;
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(ReconcileReport {
            reorg_detected_at,
            dirty_orders: dirty_orders.into_iter().collect(),
            double_spent_orders: double_spent_orders.into_iter().collect(),
        })
    }

    pub(crate) fn fixture_tx() -> Transaction {
        let raw_tx = hex::decode(include_str!("../tests/fixtures/subaddress_tx.hex")).unwrap();
        deserialize(&raw_tx).unwrap()
    }

    fn fixture_view_key() -> [u8; 32] {
        PrivateKey::from_slice(
            &hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07")
                .unwrap(),
        )
        .unwrap()
        .to_bytes()
    }

    fn fixture_spend_pubkey() -> [u8; 32] {
        let secret_spend = PrivateKey::from_slice(
            &hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907")
                .unwrap(),
        )
        .unwrap();
        PublicKey::from_private_key(&secret_spend).to_bytes()
    }

    async fn setup() -> (Store, PlainKeyCustody, WalletHandle, String, String) {
        setup_with_confirmations_override(None).await
    }

    /// `setup()` with a per-order `confirmations_required_override`, for the
    /// scenarios where an order settles off a mempool sighting alone (native
    /// 0-conf: `Some(0)`) - the case a plain double-spend actually costs a merchant
    /// something, since they may have shipped against it.
    async fn setup_with_confirmations_override(
        confirmations_required_override: Option<u64>,
    ) -> (Store, PlainKeyCustody, WalletHandle, String, String) {
        let store = Store::open_in_memory().unwrap();
        let key_custody = PlainKeyCustody::default();
        let handle = key_custody
            .register_wallet(WalletMaterial::new(
                fixture_view_key(),
                fixture_spend_pubkey(),
            ))
            .await
            .unwrap();

        let created = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4fixture".into(),
                    network: "mainnet".into(),
                    confirmations_required: Some(10),
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();
        let tenant_id = created.tenant.id.clone();

        // The fixture tx pays subaddress 0/1 - derive that address for real via
        // KeyCustody and issue the order against it, exactly as production code
        // would (order creation happens to land on minor index 1 here since the
        // tenant starts at next_minor_index = 1).
        let index = store.allocate_minor_index(&tenant_id).unwrap();
        assert_eq!(
            index, 1,
            "fixture tx pays minor index 1 - keep this in sync with the order below"
        );
        let address = key_custody
            .derive_subaddress(
                handle,
                SubaddressIndex {
                    major: 0,
                    minor: index,
                },
                Network::Mainnet,
            )
            .await
            .unwrap();

        let order = store
            .create_order(NewOrder {
                idempotency_key: None,
                confirmations_required_override,
                tenant_id: tenant_id.clone(),
                merchant_order_id: None,
                minor_index: index,
                address: address.to_string(),
                xmr_amount_piconero: 1, // fixture tx amount is unknown ahead of time; use a trivially-satisfied amount
                description: None,
                created_at: 1000,
                // Genuinely in the future: every tick now recomputes every
                // non-terminal order, so a fixture expiry in the past (999_999_999 is
                // in 2001) would flip these orders to `expired` on the first tick.
                expires_at: crate::now_unix() + 3600,
            })
            .unwrap();

        (
            store,
            key_custody,
            handle,
            tenant_id.into_string(),
            order.id.into_string(),
        )
    }

    /// `setup()` with a caller-controlled `expires_at`, for the grace-period tests
    /// below - they need an order whose deadline sits at a specific real-wall-clock
    /// offset (recently past, or long past), which `setup()`'s own fixed
    /// `now_unix() + 3600` can't express.
    async fn setup_with_expiry(
        expires_at: i64,
    ) -> (Store, PlainKeyCustody, WalletHandle, String, String) {
        let store = Store::open_in_memory().unwrap();
        let key_custody = PlainKeyCustody::default();
        let handle = key_custody
            .register_wallet(WalletMaterial::new(
                fixture_view_key(),
                fixture_spend_pubkey(),
            ))
            .await
            .unwrap();

        let created = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4fixture".into(),
                    network: "mainnet".into(),
                    confirmations_required: Some(10),
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();
        let tenant_id = created.tenant.id.clone();

        let index = store.allocate_minor_index(&tenant_id).unwrap();
        assert_eq!(
            index, 1,
            "fixture tx pays minor index 1 - keep this in sync with the order below"
        );
        let address = key_custody
            .derive_subaddress(
                handle,
                SubaddressIndex {
                    major: 0,
                    minor: index,
                },
                Network::Mainnet,
            )
            .await
            .unwrap();

        let order = store
            .create_order(NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: tenant_id.clone(),
                merchant_order_id: None,
                minor_index: index,
                address: address.to_string(),
                xmr_amount_piconero: 1,
                description: None,
                created_at: expires_at - 300,
                expires_at,
            })
            .unwrap();

        (
            store,
            key_custody,
            handle,
            tenant_id.into_string(),
            order.id.into_string(),
        )
    }

    /// `docs/order_rescan_wbs.md` Phase 4 - the default grace period for
    /// recently-expired orders.
    #[tokio::test]
    async fn a_recently_expired_orders_late_payment_is_still_matched_within_its_grace_period() {
        let now = crate::now_unix();
        let (store, key_custody, handle, tenant_id, order_id) = setup_with_expiry(now - 300).await;

        // A prior tick already flipped this order to `Expired` - the exact state
        // the grace-period widening needs to matter at all (a still-`pending`
        // order is already covered by the base four-status clause regardless).
        let (_, status) = store
            .recompute_order_status(&shared::ids::OrderId::new(order_id.to_string()), 0, now)
            .unwrap();
        assert_eq!(
            status,
            crate::status::OrderStatus::Expired,
            "test setup must actually produce an expired order"
        );

        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.set_mempool(vec![fixture_tx()]);

        // A generous grace period - the order expired moments ago, well inside it.
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            3600,
        )
        .await
        .unwrap();

        let s = store.lock();
        assert_eq!(
            s.get_all_payments(&shared::ids::OrderId::new(order_id.to_string())).unwrap().len(),
            1,
            "a late payment within the grace period must still be matched by ordinary live scanning"
        );
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        // No native 0-conf threshold configured (`setup_with_expiry`), so a mempool-only
        // sighting correctly settles at `Unconfirmed`, not `Paid` - the real point
        // here is that the order came alive again at all (it must not still read
        // `Expired` with the payment silently uncounted).
        assert_eq!(
            order.status,
            crate::status::OrderStatus::Unconfirmed,
            "the order must reflect the late payment, not remain stuck at Expired"
        );
    }

    #[tokio::test]
    async fn a_payment_arriving_after_the_grace_period_has_elapsed_is_genuinely_not_matched() {
        let now = crate::now_unix();
        let (store, key_custody, handle, tenant_id, order_id) =
            setup_with_expiry(now - 10_000).await;

        let (_, status) = store
            .recompute_order_status(&shared::ids::OrderId::new(order_id.to_string()), 0, now)
            .unwrap();
        assert_eq!(
            status,
            crate::status::OrderStatus::Expired,
            "test setup must actually produce an expired order"
        );

        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.set_mempool(vec![fixture_tx()]);

        // A short grace period the order's 10,000-second-old expiry is well past -
        // proving the boundary is real, not just documented (exactly the gap the
        // manual rescan exists to close).
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            60,
        )
        .await
        .unwrap();

        let s = store.lock();
        assert_eq!(
            s.get_all_payments(&shared::ids::OrderId::new(order_id.to_string())).unwrap().len(),
            0,
            "a payment arriving after the grace period has elapsed must not be matched by ordinary live scanning"
        );
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            order.status,
            crate::status::OrderStatus::Expired,
            "must remain untouched"
        );
    }

    /// One transaction scanned for two stores. The store it pays gets the
    /// matches along with the transaction's id and key images; the store it
    /// doesn't pay gets `None`.
    #[tokio::test]
    async fn a_transaction_scanned_for_several_stores_is_described_only_to_the_one_it_pays() {
        let custody = PlainKeyCustody::default();
        let paid = custody
            .register_wallet(WalletMaterial::new(
                fixture_view_key(),
                fixture_spend_pubkey(),
            ))
            .await
            .unwrap();
        let (view, spend) = arbitrary_wallet_material(7);
        let unpaid = custody
            .register_wallet(WalletMaterial::new(view, spend))
            .await
            .unwrap();
        let tx = fixture_tx();
        let tenants = [
            (
                crate::store::TenantId::new("unpaid"),
                unpaid,
                ScanIndices::new([1]),
            ),
            (
                crate::store::TenantId::new("paid"),
                paid,
                ScanIndices::new([1]),
            ),
        ];

        let results = scan_for_tenants(
            &custody,
            &tx_id_hex(&tx),
            &tx,
            &tenants.iter().collect::<Vec<_>>(),
        )
        .await;

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0.as_str(), "unpaid");
        assert!(results[0].1.as_ref().unwrap().is_none());
        assert_eq!(results[1].0.as_str(), "paid");
        let scan = results[1].1.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(scan.matches.len(), 1);
        assert_eq!(scan.txid, tx_id_hex(&tx));
        assert_eq!(
            parse_payment_key_images(&scan.key_images_json).unwrap(),
            key_images_of(&tx)
        );
    }

    /// A run of transactions scanned for stores that are at different points
    /// in it. Each store is told only about the transactions it hadn't been
    /// scanned for, each under its own id.
    #[tokio::test]
    async fn a_run_of_transactions_is_scanned_for_each_store_from_where_it_had_got_to() {
        let custody = PlainKeyCustody::default();
        let wallet = custody
            .register_wallet(WalletMaterial::new(
                fixture_view_key(),
                fixture_spend_pubkey(),
            ))
            .await
            .unwrap();
        // Two payments with an unrelated transaction between them.
        let txs = [fixture_tx(), unrelated_tx(1), fixture_tx_variant(5)];
        let txids: Vec<String> = txs.iter().map(tx_id_hex).collect();
        let inputs: Vec<ScanInput> = txs.iter().map(ScanInput::of).collect();
        let store = |name: &str| {
            (
                crate::store::TenantId::new(name),
                wallet,
                ScanIndices::new([1]),
            )
        };
        let (fresh, resumed, finished) = (store("fresh"), store("resumed"), store("finished"));

        let kept: Vec<crate::daemon::ScanTx> = txs.iter().map(crate::daemon::ScanTx::of).collect();
        let results = scan_txs_for_tenants(
            &custody,
            &txids,
            &kept,
            &inputs,
            &[(&fresh, 0), (&resumed, 1), (&finished, 3)],
        )
        .await;

        let found: Vec<(String, Vec<String>)> = results
            .into_iter()
            .map(|(id, result)| {
                let txids = result.unwrap().into_iter().map(|scan| scan.txid).collect();
                (id.to_string(), txids)
            })
            .collect();
        assert_eq!(
            found,
            [
                (
                    "fresh".to_string(),
                    vec![tx_id_hex(&txs[0]), tx_id_hex(&txs[2])]
                ),
                ("resumed".to_string(), vec![tx_id_hex(&txs[2])]),
                ("finished".to_string(), vec![]),
            ]
        );
    }

    // -- Scanned block range (`docs/order_rescan_wbs.md` Phase 5.1) ---------

    /// A block interrupted partway keeps its matches staged, never as
    /// payments; they come back only for the same block (height and hash),
    /// once, at commit. A checkpoint for a changed block is dropped with its
    /// matches.
    #[tokio::test]
    async fn a_block_checkpoint_stages_matches_until_commit_and_a_changed_hash_drops_them() {
        let (store, custody, handle, tenant_id, order_id) = setup().await;
        let window = ScanIndices::new([1]);
        let scan = scan_transaction_in_window(
            &custody,
            handle,
            &tx_id_hex(&fixture_tx()),
            &fixture_tx(),
            &window,
        )
        .await
        .unwrap();
        assert!(!scan.matches.is_empty());
        let checkpoint = |hash: &str| crate::store::BlockCheckpoint {
            height: 10,
            hash: hash.into(),
            next_tx: 1,
        };
        let stage = |hash: &str| {
            store
                .in_transaction(|s| -> Result<()> {
                    s.save_block_checkpoint(
                        monero::Network::Mainnet,
                        &shared::ids::TenantId::new(tenant_id.to_string()),
                        &checkpoint(hash),
                    )?;
                    stage_block_match(
                        s,
                        monero::Network::Mainnet,
                        &shared::ids::TenantId::new(tenant_id.to_string()),
                        &scan,
                        1500,
                    )
                })
                .unwrap()
        };

        stage("old_hash");
        assert_eq!(
            store
                .block_checkpoint(
                    monero::Network::Mainnet,
                    &shared::ids::TenantId::new(tenant_id.to_string())
                )
                .unwrap(),
            Some(checkpoint("old_hash"))
        );
        assert!(
            store
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()
                .is_empty(),
            "an unfinished block must not announce payment"
        );
        assert!(
            store
                .take_staged_payments(
                    monero::Network::Mainnet,
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    "new_hash"
                )
                .unwrap()
                .is_empty(),
            "the old fork's matches are dropped"
        );
        assert_eq!(
            store
                .block_checkpoint(
                    monero::Network::Mainnet,
                    &shared::ids::TenantId::new(tenant_id.to_string())
                )
                .unwrap(),
            None
        );

        stage("old_hash");
        stage("new_hash");
        let staged = store
            .take_staged_payments(
                monero::Network::Mainnet,
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "new_hash",
            )
            .unwrap();
        assert_eq!(
            staged
                .iter()
                .map(|p| p.order_id.clone())
                .collect::<Vec<_>>(),
            vec![order_id.clone()]
        );
        assert!(
            store
                .take_staged_payments(
                    monero::Network::Mainnet,
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    "new_hash"
                )
                .unwrap()
                .is_empty(),
            "taken once"
        );
    }

    #[tokio::test]
    async fn a_fresh_orders_first_scanned_height_is_set_on_its_very_first_tick_not_backfilled_to_created_at(
    ) {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let daemon = FakeDaemonClient::new();
        for h in 1..=500 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        let store = store.into_shared();

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let order = store
            .lock()
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_ne!(
            order.first_scanned_height,
            Some(1000),
            "must never be backfilled to created_at's own value (1000)"
        );
        // Bootstrap seeds one block behind the tip (500) before the loop, so 500
        // is the first (and only, this tick) height actually reached.
        assert_eq!(order.first_scanned_height, Some(500));
        assert_eq!(order.last_scanned_height, Some(500));
    }

    #[tokio::test]
    async fn last_scanned_height_advances_tick_over_tick_then_freezes_once_the_order_leaves_scope()
    {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let daemon = FakeDaemonClient::new();
        for h in 1..=100 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        let store = store.into_shared();

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .get_order(
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &shared::ids::OrderId::new(order_id.to_string())
                )
                .unwrap()
                .unwrap()
                .last_scanned_height,
            Some(100)
        );

        for h in 101..=150 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        for _ in 0..7 {
            run_scan_tick(
                &store,
                &key_custody,
                &daemon,
                "mainnet",
                &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
                20,
                0,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            store
                .lock()
                .get_order(
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &shared::ids::OrderId::new(order_id.to_string())
                )
                .unwrap()
                .unwrap()
                .last_scanned_height,
            Some(150),
            "must have advanced with the chain while still in scope"
        );

        // Settle the order (a real payment, not a forced status write) - takes it
        // terminal and out of scope. `setup()`'s tenant requires 10 confirmations,
        // so the payment needs 9 more blocks on top of the one it's mined in
        // before the order actually reaches a terminal status.
        daemon.push_block("blk_settle", vec![fixture_tx()]); // height 151
        for h in 152..=160 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        for _ in 0..2 {
            run_scan_tick(
                &store,
                &key_custody,
                &daemon,
                "mainnet",
                &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
                20,
                0,
            )
            .await
            .unwrap();
        }
        let settled = store
            .lock()
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert!(
            matches!(
                settled.status,
                crate::status::OrderStatus::Paid | crate::status::OrderStatus::Overpaid
            ),
            "expected a terminal status after 10 confirmations, got {:?}",
            settled.status
        );
        assert_eq!(settled.last_scanned_height, Some(160));
        // A paid order stays in scope for the grace period after it closes
        // (D10); move its close time back so that period is over.
        store
            .lock()
            .execute_raw_for_test(&format!(
                "UPDATE orders SET closed_at_utc = 1 WHERE id = '{order_id}'"
            ))
            .unwrap();

        for h in 161..=200 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        let frozen = store
            .lock()
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            frozen.last_scanned_height,
            Some(160),
            "must freeze (not keep advancing, not reset) once the order is terminal and out of scope"
        );
    }

    #[tokio::test]
    async fn scanning_a_mempool_tx_records_a_payment_against_the_right_order() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();

        let touched = scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            touched,
            HashSet::from([crate::store::OrderId::new(order_id.clone())])
        );
        let payments = store
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(payments.len(), 1);
        assert!(payments[0].amount_piconero > 0);
        assert_eq!(payments[0].block_height, None); // mempool-only
    }

    #[tokio::test]
    async fn rescanning_the_same_mempool_tx_does_not_duplicate_the_payment() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();

        for _ in 0..3 {
            scan_transaction_for_tenant(
                &store,
                &key_custody,
                handle,
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &tx,
                0..3,
                1500,
                None,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            store
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn reorg_moving_a_tx_to_a_different_block_updates_height_without_voiding() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();

        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 50, "hash_50_v1")
            .unwrap();

        let daemon = FakeDaemonClient::new();
        // Fast-forward the fake daemon's internal height counter to 49 by pushing
        // filler blocks, then reorg from 50 with the tx moved to the new height 51.
        let mut last_height = 0;
        while last_height < 49 {
            last_height = daemon.push_block(&format!("filler_{last_height}"), vec![]);
        }
        daemon.reorg_from(
            50,
            vec![("hash_50_v2", vec![]), ("hash_51_v2", vec![tx.clone()])],
        );

        let store = store.into_shared();
        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
            .await
            .unwrap();
        assert_eq!(report.reorg_detected_at, Some(50));
        assert!(report
            .dirty_orders
            .contains(&shared::ids::OrderId::new(order_id.to_string())));
        assert!(report.double_spent_orders.is_empty());

        let payments = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(payments[0].block_height, Some(51));
        assert!(payments[0].voided_at.is_none());
    }

    #[tokio::test]
    async fn reorg_where_tx_vanishes_and_key_image_proven_spent_elsewhere_voids_and_flags_double_spend(
    ) {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let txid = tx_id_hex(&tx);
        let key_images = key_images_of(&tx);
        assert!(
            !key_images.is_empty(),
            "fixture tx must have at least one input to test key-image proof"
        );

        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 50, "hash_50_v1")
            .unwrap();

        let daemon = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = daemon.push_block(&format!("filler_{last_height}"), vec![]);
        }
        // The replacement chain does NOT include our tx at all - it vanished.
        daemon.reorg_from(50, vec![("hash_50_v2", vec![])]);
        // And its inputs are now proven spent by a different, unrelated transaction.
        for ki in &key_images {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }

        let store = store.into_shared();
        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
            .await
            .unwrap();
        assert_eq!(report.reorg_detected_at, Some(50));
        assert_eq!(report.dirty_orders, vec![order_id.clone()]);
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);

        let store = store.lock();
        let payments = store
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert!(payments[0].voided_at.is_some());
        let order = store
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert!(order.double_spend_detected_at.is_some());
        let _ = txid; // kept for readability of the scenario, not asserted on directly
    }

    #[tokio::test]
    async fn reorg_where_tx_vanishes_but_key_image_still_unspent_does_not_void() {
        // Safety property: never void on ambiguous evidence. A tx that vanished
        // from the chain but whose inputs are still unspent is most likely just
        // still propagating and will be re-caught on a later tick.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();

        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 50, "hash_50_v1")
            .unwrap();

        let daemon = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = daemon.push_block(&format!("filler_{last_height}"), vec![]);
        }
        daemon.reorg_from(50, vec![("hash_50_v2", vec![])]);
        // Deliberately do NOT mark the key images as spent - default is Unspent.

        let store = store.into_shared();
        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
            .await
            .unwrap();
        assert!(report.double_spent_orders.is_empty());

        let payments = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert!(
            payments[0].voided_at.is_none(),
            "must not void on ambiguous (unspent) evidence"
        );
    }

    #[tokio::test]
    async fn no_reorg_when_hashes_still_match_is_a_cheap_no_op() {
        let store = Store::open_in_memory().unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 50, "hash_50")
            .unwrap();
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 50 {
            let h = if last_height == 49 {
                "hash_50"
            } else {
                "filler"
            };
            last_height = daemon.push_block(h, vec![]);
        }

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
            .await
            .unwrap();
        assert_eq!(report.reorg_detected_at, None);
        assert!(report.dirty_orders.is_empty());
    }

    #[tokio::test]
    async fn run_scan_tick_matches_mempool_tx_recomputes_status_and_enqueues_a_webhook() {
        // End-to-end proof of the composed orchestration, not just its pieces: a
        // real transaction sitting in a fake daemon's mempool gets matched, the
        // owning order's status transitions (pending -> unconfirmed, since this is
        // a 0-conf-only match with a nonzero confirmation requirement), and
        // exactly one webhook delivery is enqueued for that transition.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let webhook = store
            .create_webhook(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]); // the fake starts at height 0 with no block there at all; give the first-run tip bootstrap something real to seed from
        daemon.set_mempool(vec![fixture_tx()]);

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(order.status, crate::status::OrderStatus::Unconfirmed);
        assert!(order.amount_received_piconero > 0);

        let due = s
            .due_webhook_deliveries_for_test(crate::now_unix() + 1, 10)
            .unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].webhook_id, webhook.id);
        assert_eq!(due[0].event_type, "order.unconfirmed");
    }

    /// Creates a tenant with one pending order against `key_custody`, returning
    /// `(tenant_id, handle, order_id)`. Shared by the watchlist tests below, which
    /// need to construct tenants against a `CountingKeyCustody` rather than
    /// `setup()`'s hardcoded `PlainKeyCustody`.
    async fn tenant_with_pending_order(
        store: &Store,
        key_custody: &dyn KeyCustody,
        view_key: [u8; 32],
        spend_pubkey: [u8; 32],
    ) -> (crate::store::TenantId, WalletHandle, crate::store::OrderId) {
        let handle = key_custody
            .register_wallet(WalletMaterial::new(view_key, spend_pubkey))
            .await
            .unwrap();
        let tenant = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4x".into(),
                    network: "mainnet".into(),
                    confirmations_required: Some(10),
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();
        let index = store.allocate_minor_index(&tenant.tenant.id).unwrap();
        let address = key_custody
            .derive_subaddress(
                handle,
                SubaddressIndex {
                    major: 0,
                    minor: index,
                },
                Network::Mainnet,
            )
            .await
            .unwrap();
        let order = store
            .create_order(NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: tenant.tenant.id.clone(),
                merchant_order_id: None,
                minor_index: index,
                address: address.to_string(),
                xmr_amount_piconero: 1,
                description: None,
                created_at: 1000,
                // Genuinely in the future: every tick now recomputes every
                // non-terminal order, so a fixture expiry in the past (999_999_999 is
                // in 2001) would flip these orders to `expired` on the first tick.
                expires_at: crate::now_unix() + 3600,
            })
            .unwrap();
        (
            shared::ids::TenantId::new(tenant.tenant.id.into_string()),
            handle,
            shared::ids::OrderId::new(order.id.into_string()),
        )
    }

    fn arbitrary_wallet_material(seed: u8) -> ([u8; 32], [u8; 32]) {
        let mut view_bytes = [seed; 32];
        view_bytes[31] &= 0x0f;
        let mut spend_seed = [seed.wrapping_add(1); 32];
        spend_seed[31] &= 0x0f;
        let secret_spend = PrivateKey::from_slice(&spend_seed).unwrap();
        (
            view_bytes,
            PublicKey::from_private_key(&secret_spend).to_bytes(),
        )
    }

    #[tokio::test]
    async fn run_scan_tick_never_calls_key_custody_for_a_tenant_with_no_pending_orders() {
        // Direct proof of docs/DESIGN.md §7.3's active-watchlist optimization,
        // using a call-counting spy rather than trusting it by inspection - see
        // docs/TESTING.md §4. Tenant A has a pending order and a real fixture view
        // pair (so a genuine match happens); tenant B is registered with
        // KeyCustody but has never had any order at all.
        let key_custody = CountingKeyCustody::default();
        let store = Store::open_in_memory().unwrap();

        let (tenant_a, handle_a, _order_a) = tenant_with_pending_order(
            &store,
            &key_custody,
            fixture_view_key(),
            fixture_spend_pubkey(),
        )
        .await;

        let (view_b, spend_b) = arbitrary_wallet_material(0x70);
        let handle_b = key_custody
            .register_wallet(WalletMaterial::new(view_b, spend_b))
            .await
            .unwrap();
        let tenant_b = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4b".into(),
                    network: "mainnet".into(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();

        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.set_mempool(vec![fixture_tx()]);

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(tenant_a, handle_a), (tenant_b.tenant.id, handle_b)],
            20,
            0,
        )
        .await
        .unwrap();

        assert_eq!(
            key_custody.scan_calls.load(Ordering::SeqCst),
            1,
            "exactly one scan call expected: tenant A (has a pending order) scanned once, tenant B (no orders ever) never scanned"
        );
    }

    #[tokio::test]
    async fn run_scan_tick_never_scans_a_tenant_on_a_different_network_even_if_included_in_the_input_list(
    ) {
        // Defense-in-depth for the multi-network design (§DESIGN.md §7): a
        // mainnet-network tick must not touch a pending stagenet tenant's data at
        // all, even though `active_tenants_page` already scopes by network - this
        // proves the *second*, independent filter (`t.network == network` in
        // `run_scan_tick` itself) actually engages, guarding against a caller (e.g.
        // a future refactor of `main.rs`'s scanner loop) accidentally passing an
        // unfiltered, all-networks tenant list into a single network's tick.
        let key_custody = CountingKeyCustody::default();
        let store = Store::open_in_memory().unwrap();

        let (mainnet_tenant, mainnet_handle, _order) = tenant_with_pending_order(
            &store,
            &key_custody,
            fixture_view_key(),
            fixture_spend_pubkey(),
        )
        .await;

        let (view_b, spend_b) = arbitrary_wallet_material(0x50);
        let stagenet_handle = key_custody
            .register_wallet(WalletMaterial::new(view_b, spend_b))
            .await
            .unwrap();
        let stagenet_tenant = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "5stagenet".into(),
                    network: "stagenet".into(),
                    confirmations_required: Some(10),
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();
        // A genuinely pending order on stagenet - if the network filter didn't
        // work, this tenant would be scanned too.
        store
            .create_order(NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: stagenet_tenant.tenant.id.clone(),
                merchant_order_id: None,
                minor_index: store
                    .allocate_minor_index(&stagenet_tenant.tenant.id)
                    .unwrap(),
                address: "5someaddress".into(),
                xmr_amount_piconero: 1,
                description: None,
                created_at: 1000,
                // Genuinely in the future: every tick now recomputes every
                // non-terminal order, so a fixture expiry in the past (999_999_999 is
                // in 2001) would flip these orders to `expired` on the first tick.
                expires_at: crate::now_unix() + 3600,
            })
            .unwrap();

        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.set_mempool(vec![fixture_tx()]);

        // Deliberately pass BOTH tenants into a "mainnet" tick, simulating
        // main.rs's scanner loop handing the *full* wallet_handles registry
        // (spanning every configured network) to a single network's call.
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[
                (mainnet_tenant, mainnet_handle),
                (stagenet_tenant.tenant.id, stagenet_handle),
            ],
            20,
            0,
        )
        .await
        .unwrap();

        assert_eq!(
            key_custody.scan_calls.load(Ordering::SeqCst),
            1,
            "only the mainnet tenant should be scanned during a \"mainnet\" tick, regardless of what else is in the input list"
        );
    }

    #[tokio::test]
    async fn run_scan_tick_stops_scanning_a_tenant_the_tick_after_it_fully_settles() {
        // Complements the store-level test of the same scenario
        // (`tenant_becomes_inactive_once_its_only_order_settles...` in store.rs) by
        // proving it at the scanner's actual call boundary: once an order reaches a
        // terminal status, the *next* tick must not invoke KeyCustody for that
        // tenant at all, even though the exact same mempool transaction is still
        // sitting there.
        let key_custody = CountingKeyCustody::default();
        let store = Store::open_in_memory().unwrap();
        let (tenant_id, _handle, order_id) = tenant_with_pending_order(
            &store,
            &key_custody,
            fixture_view_key(),
            fixture_spend_pubkey(),
        )
        .await;
        let handle = key_custody
            .register_wallet(WalletMaterial::new(
                fixture_view_key(),
                fixture_spend_pubkey(),
            ))
            .await
            .unwrap();
        // (Re-registering under the same key material is fine for this test - only
        // the handle identity matters for routing scan calls, not which handle
        // number KeyCustody happened to assign.)

        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.set_mempool(vec![fixture_tx()]);

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(tenant_id.clone(), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(key_custody.scan_calls.load(Ordering::SeqCst), 1);

        // Force the order to a terminal state the way "10 confirmations later"
        // naturally would: give the payment tick 1 already recorded a real block
        // height (rather than layering on a second payment, which would leave the
        // first one's 0-conf status dragging min-confirmations back to 0) and
        // recompute - the same public API the real block-scanning path uses.
        {
            let s = store.lock();
            let txid = tx_id_hex(&fixture_tx());
            s.update_payment_block_height(
                &shared::ids::OrderId::new(order_id.to_string()),
                &txid,
                1,
                Some(1),
            )
            .unwrap(); // output_index 1, per the fixture's known match (see key_custody::plain's tests)
            let (_, new_status) = s
                .recompute_order_status(&shared::ids::OrderId::new(order_id.to_string()), 100, 1600)
                .unwrap(); // 100 confirmations
            assert!(matches!(
                new_status,
                crate::status::OrderStatus::Paid | crate::status::OrderStatus::Overpaid
            ));
        }

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(tenant_id, handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            key_custody.scan_calls.load(Ordering::SeqCst),
            1,
            "tenant settled after tick 1 - tick 2 must not invoke KeyCustody for it again, even with the same mempool tx still present"
        );
    }

    #[tokio::test]
    async fn run_scan_tick_on_first_run_seeds_near_the_current_tip_instead_of_scanning_all_history()
    {
        // A fresh scanner (no scanned_blocks yet) must not try to replay the
        // entire chain. It seeds one block behind the reported tip (see the next
        // test for why) but then immediately scans forward through the real tip
        // too within the same tick, provided that block is actually fetchable -
        // the lagging-backend condition the seed-behind logic guards against is
        // the exception, not the common case.
        let (store, key_custody, handle, tenant_id, _order_id) = setup().await;
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        for i in 0..10 {
            daemon.push_block(&format!("h{i}"), vec![]);
        }

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(10)
        );
    }

    #[tokio::test]
    async fn first_run_bootstrap_tolerates_a_daemon_reporting_a_tip_it_cannot_yet_serve() {
        // Observed live against a real public node (see the comment on
        // `run_scan_tick`'s bootstrap branch): a public endpoint can be a small
        // pool of backend nodes a block apart, so `get_height` can briefly report
        // a tip that `get_block_hash` then rejects for that same height. This must
        // degrade to "try again next tick", never a hard error that kills the tick.
        let (store, key_custody, handle, tenant_id, _order_id) = setup().await;
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        for i in 0..10 {
            daemon.push_block(&format!("h{i}"), vec![]);
        }
        // Simulate the height-reporting backend being one block ahead of the
        // block-serving backend: report height 11, but no block 11 exists yet.
        daemon.advance_height_without_a_block();

        let result = run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await;
        assert!(
            result.is_ok(),
            "must not hard-fail the tick just because the tip block isn't fetchable yet"
        );
        // Falls back to seeding one behind the (unfetchable) reported tip, i.e.
        // height 10, which *does* exist.
        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(10)
        );

        // Once the block-serving backend catches up, a later tick proceeds
        // normally from where the fallback seeded it.
        daemon.push_block("h10_actual", vec![]);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(11)
        );
    }

    #[tokio::test]
    async fn an_output_whose_amount_cannot_be_decrypted_is_skipped_not_recorded_as_zero() {
        // Monero output-amount recovery can fail, which is why `MatchedOutput::
        // amount_piconero` is an `Option` at all. Recording the failure as a
        // 0-piconero payment is strictly worse than recording nothing: the row
        // contributes no money while still participating in `min_confirmations` and
        // `all_zero_conf` (see status.rs's
        // `a_zeroed_but_present_row_is_not_a_safe_substitute_for_exclusion`, which
        // documents exactly how that corrupts the derived status), and it can never
        // be removed afterwards, because voiding requires affirmative double-spend
        // proof that will never arrive for a perfectly valid output.
        let (store, _key_custody, _handle, tenant_id, order_id) = setup().await;

        let scan = ScanResult {
            matches: vec![MatchedOutput {
                output_index: 0,
                subaddress_index: SubaddressIndex { major: 0, minor: 1 }, // the order created by setup()
                amount_piconero: None,
            }],
            txid: "tx_with_an_undecryptable_output".into(),
            key_images_json: "[]".into(),
            output_keys: Default::default(),
        };

        let touched = record_scan_match(
            &store,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &scan,
            1500,
            Some(50),
        )
        .unwrap();
        assert!(
            touched.is_empty(),
            "an unmeasurable output must not even mark the order as needing a recompute"
        );
        assert!(
            store
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()
                .is_empty(),
            "nothing may be persisted for an output whose amount could not be recovered"
        );

        // The order is untouched, so a later tick that *can* decrypt the amount is
        // still free to record it properly.
        let order = store
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(order.status, crate::status::OrderStatus::Pending);
        assert_eq!(order.amount_received_piconero, 0);
    }

    #[tokio::test]
    async fn a_store_failure_recording_a_block_match_leaves_that_block_unscanned_for_the_next_tick()
    {
        // The highest-consequence silent failure in this file: block scanning used
        // to discard the error from `record_scan_match` (`if let Ok(..)`) and then
        // call `set_scanned_block` for that height regardless. Blocks are scanned
        // exactly once, so one transient store error meant the payment inside that
        // block was never seen by anything, ever, with nothing logged. The block must
        // instead stay unscanned so the next tick retries it - re-recording is a
        // no-op thanks to `UNIQUE(order_id, txid, output_index)`.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]); // height 1 - something for the tip bootstrap to seed from
        daemon.push_block("h2", vec![fixture_tx()]); // height 2 - carries the payment

        // Fail only *inserts* into order_payments, leaving every read working, so
        // this reproduces a write failure specifically rather than a broken database.
        store
            .lock()
            .execute_raw_for_test(
                "CREATE TRIGGER simulated_write_failure BEFORE INSERT ON order_payments
                 BEGIN SELECT RAISE(ABORT, 'simulated store failure'); END;",
            )
            .unwrap();

        let result = run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await;
        assert!(
            result.is_err(),
            "a storage failure is reported, not swallowed"
        );
        assert_eq!(
            store.lock().max_scanned_height(monero::Network::Mainnet).unwrap(),
            Some(1),
            "block 2's match failed to record - marking it scanned would lose that payment permanently"
        );
        assert!(store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .is_empty());

        // Once the store is healthy again, the very next tick re-covers the block it
        // deliberately left behind.
        store
            .lock()
            .execute_raw_for_test("DROP TRIGGER simulated_write_failure;")
            .unwrap();
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(2)
        );
        let payments = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].block_height, Some(2));
    }

    #[tokio::test]
    async fn an_order_reaches_paid_as_the_chain_advances_without_any_new_payment_arriving() {
        // The bug this protects against made the service unusable for its actual
        // purpose: `run_scan_tick` recomputed only the orders whose transactions it
        // matched *that tick*, but an order's confirmation count is derived from the
        // current chain height at recompute time and stored nowhere. So an order was
        // recomputed exactly once - at one confirmation - and then sat at
        // `confirming` forever no matter how deeply its payment was buried, with the
        // `order.paid` webhook the merchant is waiting on never firing.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await; // confirmations_required = 10
        let webhook = store
            .create_webhook(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![fixture_tx()]); // the payment lands at height 2

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        {
            let s = store.lock();
            let order = s
                .get_order(
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &shared::ids::OrderId::new(order_id.to_string()),
                )
                .unwrap()
                .unwrap();
            assert_eq!(
                order.status,
                crate::status::OrderStatus::Confirming,
                "one confirmation, ten required"
            );
        }

        // Nine more blocks, none of which contain anything for this order at all.
        for i in 3..=11 {
            daemon.push_block(&format!("h{i}"), vec![]);
        }
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            order.status,
            crate::status::OrderStatus::Overpaid, // the fixture tx pays more than this order's trivially-small expected amount
            "10 confirmations deep - nothing new matched, but the chain moved"
        );
        assert_eq!(order.confirmations, 10);

        // And the transition the merchant is actually waiting on was announced.
        let events: Vec<String> = s
            .due_webhook_deliveries_for_test(crate::now_unix() + 1, 10)
            .unwrap()
            .into_iter()
            .inspect(|d| assert_eq!(d.webhook_id, webhook.id))
            .map(|d| d.event_type)
            .collect();
        assert_eq!(
            events,
            vec!["order.confirming".to_string(), "order.overpaid".to_string()],
            "exactly one event per real transition, in order, with no repeats from the per-tick recompute"
        );
    }

    #[tokio::test]
    async fn an_unpaid_order_past_its_deadline_becomes_expired_on_a_tick_that_matches_nothing() {
        // The same root cause as the confirmations bug, on the other axis: expiry is
        // a function of wall-clock time, and an order that never receives a payment
        // is never in the `touched` set, so it was never recomputed and stayed
        // `pending` indefinitely.
        let (store, key_custody, handle, tenant_id, _order_id) = setup().await;
        let webhook = store
            .create_webhook(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();

        // A second order on the same tenant, already past its deadline and never paid.
        let stale = store
            .create_order(NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: shared::ids::TenantId::new(tenant_id.clone()),
                merchant_order_id: None,
                minor_index: store
                    .allocate_minor_index(&shared::ids::TenantId::new(tenant_id.to_string()))
                    .unwrap(),
                address: "sub_expired".into(),
                xmr_amount_piconero: 1_000_000,
                description: None,
                created_at: 1000,
                expires_at: crate::now_unix() - 1,
            })
            .unwrap();
        assert_eq!(stale.status, crate::status::OrderStatus::Pending);
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]); // an entirely uneventful tick: no mempool, no matches

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        assert_eq!(
            s.get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &stale.id
            )
            .unwrap()
            .unwrap()
            .status,
            crate::status::OrderStatus::Expired
        );
        let expired_events = s
            .due_webhook_deliveries_for_test(crate::now_unix() + 1, 10)
            .unwrap()
            .into_iter()
            .filter(|d| d.order_id == stale.id && d.webhook_id == webhook.id)
            .map(|d| d.event_type)
            .collect::<Vec<_>>();
        assert_eq!(
            expired_events,
            vec!["order.expired".to_string()],
            "queued for the store's own webhook"
        );
    }

    #[tokio::test]
    async fn a_reorg_driven_status_change_enqueues_a_webhook_just_like_a_forward_scan_one() {
        // Reorg reconciliation called the bare `Store::recompute_order_status`
        // instead of the notifying wrapper the normal payment path uses, and
        // `run_scan_tick` then threw `dirty_orders` away entirely - so an order
        // walking backwards from `paid` to `confirming` because its transaction was
        // remined shallower updated silently, and the merchant's only way to find
        // out was to poll.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let webhook = store
            .create_webhook(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();
        let tx = fixture_tx();

        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 50, "hash_50_v1")
            .unwrap();
        let (_, status) = store
            .recompute_order_status(&shared::ids::OrderId::new(order_id.to_string()), 100, 1600)
            .unwrap();
        assert_eq!(
            status,
            crate::status::OrderStatus::Overpaid,
            "51 confirmations deep before the reorg"
        );

        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = daemon.push_block(&format!("filler_{last_height}"), vec![]);
        }
        // The transaction survives the reorg but is remined at the new tip, so it is
        // suddenly one confirmation deep again instead of fifty-one.
        daemon.reorg_from(
            50,
            vec![("hash_50_v2", vec![]), ("hash_51_v2", vec![tx.clone()])],
        );

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
            .await
            .unwrap();
        assert_eq!(report.reorg_detected_at, Some(50));
        assert!(
            report.double_spent_orders.is_empty(),
            "a remined transaction is not a double-spend"
        );

        let s = store.lock();
        assert_eq!(
            s.get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string())
            )
            .unwrap()
            .unwrap()
            .status,
            crate::status::OrderStatus::Confirming
        );
        let due = s
            .due_webhook_deliveries_for_test(crate::now_unix() + 1, 10)
            .unwrap();
        assert_eq!(
            due.len(),
            1,
            "the paid -> confirming transition must be announced exactly once"
        );
        assert_eq!(due[0].event_type, "order.confirming");
        assert_eq!(due[0].webhook_id, webhook.id);
    }

    #[tokio::test]
    async fn a_reconciliation_that_fails_partway_leaves_the_reorg_still_detectable() {
        // The stored block hashes used to be corrected *inside* the detection loop,
        // before any payment had been looked at. Once that write landed, the stored
        // and actual chains agreed, so a later failure - or a crash - meant no future
        // tick could ever tell a reorg had happened, and the payments it affected
        // were stranded on a chain that no longer exists. Correcting the stored view
        // only after reconciliation succeeds makes a failed pass a pure retry.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 50, "hash_50_v1")
            .unwrap();
        let store = store.into_shared();

        let inner = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = inner.push_block(&format!("filler_{last_height}"), vec![]);
        }
        inner.reorg_from(
            50,
            vec![("hash_50_v2", vec![]), ("hash_51_v2", vec![tx.clone()])],
        );

        let failing = DaemonFailingLocate { inner };
        assert!(
            check_for_reorg_and_reconcile(&store, &failing, "mainnet", 20, 2000)
                .await
                .is_err(),
            "the node failure must surface, not be swallowed"
        );
        assert_eq!(
            store.lock().get_scanned_block_hash(monero::Network::Mainnet, 50).unwrap(),
            Some("hash_50_v1".to_string()),
            "the stored hash must still describe the old chain, or nothing will ever notice the reorg again"
        );
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()[0]
                .block_height,
            Some(50),
            "and the payment is still unreconciled, as the failed pass left it"
        );

        // The retry a healthy next tick performs now works, because the evidence the
        // detection depends on is still there.
        let report = check_for_reorg_and_reconcile(&store, &failing.inner, "mainnet", 20, 2100)
            .await
            .unwrap();
        assert_eq!(report.reorg_detected_at, Some(50));
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()[0]
                .block_height,
            Some(51)
        );
    }

    #[tokio::test]
    async fn after_a_reorg_the_replacement_blocks_are_forward_scanned_again() {
        // Reconciliation only ever re-examines payments it already knew about, so a
        // payment that exists *only* in the replacement chain - the transaction was
        // rebroadcast and mined into the fork that won - was never seen at all: the
        // scanner's high-water mark was still above those heights and blocks below it
        // are never revisited. Dropping the scanned-block rows at and above the reorg
        // point is what puts them back in range of the ordinary forward scan.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        // A scanner that has already worked its way up to height 51 on the old chain.
        for h in 40..=51 {
            store
                .set_scanned_block(monero::Network::Mainnet, h, &format!("old_{h}"))
                .unwrap();
        }
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = daemon.push_block(&format!("old_{}", last_height + 1), vec![]);
        }
        // The fork replaces 50 and 51, and the payment lives at height 50 - *below*
        // the high-water mark, so nothing would ever look there again on its own.
        daemon.reorg_from(50, vec![("new_50", vec![fixture_tx()]), ("new_51", vec![])]);

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        {
            let s = store.lock();
            assert_eq!(
                s.max_scanned_height(monero::Network::Mainnet).unwrap(),
                Some(49),
                "the high-water mark must fall back below the reorg point"
            );
            assert!(
                s.get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                    .unwrap()
                    .is_empty(),
                "nothing found yet - this tick only rewound"
            );
        }

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        assert_eq!(
            s.max_scanned_height(monero::Network::Mainnet).unwrap(),
            Some(51),
            "and forward scanning caught back up"
        );
        let payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(
            payments.len(),
            1,
            "the payment that exists only in the replacement chain was found"
        );
        assert_eq!(payments[0].block_height, Some(50));
    }

    #[tokio::test]
    async fn a_voided_payment_is_restored_when_its_transaction_returns_to_the_chain() {
        // Voiding is a conclusion drawn from a chain state that can itself change:
        // the replacement transaction that proved the double-spend can be reorged
        // out in turn, putting the original back on the canonical chain. Because
        // reconciliation once collected only payments that weren't voided, a voided
        // payment was invisible to every future reconciliation pass and the
        // merchant's genuinely-paid order stayed permanently short.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let key_images = key_images_of(&tx);

        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 50, "hash_50_v1")
            .unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = daemon.push_block(&format!("filler_{last_height}"), vec![]);
        }
        daemon.reorg_from(50, vec![("hash_50_v2", vec![])]); // the tx vanishes
        for ki in &key_images {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain); // ...and is proven double-spent
        }

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
            .await
            .unwrap();
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);
        assert!(store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()[0]
            .voided_at
            .is_some());

        // Now the attacker's replacement chain loses, and the original transaction is
        // back in a block. (The forward scan would have recorded the new hash for
        // height 50 between the two reconciliations; do that by hand here.)
        store
            .lock()
            .set_scanned_block(monero::Network::Mainnet, 50, "hash_50_v2")
            .unwrap();
        daemon.reorg_from(50, vec![("hash_50_v3", vec![tx.clone()])]);

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2100)
            .await
            .unwrap();
        assert_eq!(report.reorg_detected_at, Some(50));
        assert!(report
            .dirty_orders
            .contains(&shared::ids::OrderId::new(order_id.to_string())));

        let s = store.lock();
        let payment = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .remove(0);
        assert!(
            payment.voided_at.is_none(),
            "the payment is canonical again and must count towards the order"
        );
        assert_eq!(payment.block_height, Some(50));
        // Sticky by design: a double-spend attempt was genuinely observed on this
        // order, and that remains true regardless of how the chain settled.
        assert!(s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string())
            )
            .unwrap()
            .unwrap()
            .double_spend_detected_at
            .is_some());
    }

    #[tokio::test]
    async fn every_webhook_payload_carries_a_stable_event_id_and_a_timestamp() {
        // The payload used to be just `{order_id, status}`, which gives a receiver
        // nothing to dedupe on (a retry of a lost-ack delivery is byte-identical to a
        // genuine second transition to the same status) and nothing to bound a replay
        // with (a captured delivery stays valid forever). Both the id and the
        // timestamp live *inside* the signed body, not only in headers.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        store
            .create_webhook(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![fixture_tx()]);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let first_payload: serde_json::Value = {
            let s = store.lock();
            let due = s
                .due_webhook_deliveries_for_test(crate::now_unix() + 1, 10)
                .unwrap();
            assert_eq!(due.len(), 1);
            serde_json::from_str(&due[0].payload_json).unwrap()
        };
        assert_eq!(first_payload["order_id"], serde_json::json!(order_id));
        assert_eq!(first_payload["status"], serde_json::json!("confirming"));
        assert_eq!(
            first_payload["event"],
            serde_json::json!("order.confirming")
        );
        assert!(
            first_payload["event_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("evt_")),
            "every event needs an id a receiver can dedupe on: {first_payload}"
        );
        assert!(
            first_payload["created_at"].as_i64().is_some(),
            "and a timestamp to bound replays with"
        );

        // A retry of the same delivery re-sends the identical, identically-signed
        // body - the id must identify the *event*, not the attempt.
        {
            let s = store.lock();
            let due = s
                .due_webhook_deliveries_for_test(crate::now_unix() + 1, 10)
                .unwrap();
            s.schedule_webhook_retry(
                due[0].delivery_id,
                0,
                Some(500),
                Some("boom"),
                crate::now_unix(),
            )
            .unwrap();
            let retried = s
                .due_webhook_deliveries_for_test(crate::now_unix() + 1, 10)
                .unwrap();
            let retried_payload: serde_json::Value =
                serde_json::from_str(&retried[0].payload_json).unwrap();
            assert_eq!(retried_payload["event_id"], first_payload["event_id"]);
        }

        // A genuinely different transition gets a genuinely different id.
        for i in 3..=11 {
            daemon.push_block(&format!("h{i}"), vec![]);
        }
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        let s = store.lock();
        let paid = s
            .due_webhook_deliveries_for_test(crate::now_unix() + 1, 10)
            .unwrap()
            .into_iter()
            .find(|d| d.event_type == "order.overpaid")
            .expect("the settled transition must have been enqueued");
        let settled_payload: serde_json::Value = serde_json::from_str(&paid.payload_json).unwrap();
        assert_ne!(settled_payload["event_id"], first_payload["event_id"]);
    }

    #[tokio::test]
    async fn a_block_that_cannot_be_read_stops_the_range_instead_of_leaving_a_gap() {
        // The node serves the blocks below height H and not H itself. The range
        // must stop there: carrying on to the next height would leave a hole,
        // height H unrecorded and H+1 onwards recorded.
        //
        // A hole is worse than a stopping point, because reorg detection skips
        // heights it has no stored hash for. A later reorg genuinely starting at H is
        // then first noticed at H+1, so the reported reorg point is one block too
        // high - the payments actually orphaned at H are never re-evaluated (they go
        // on counting towards their order at a height that no longer exists) and
        // block H of the replacement chain is never rescanned.
        let (store, key_custody, handle, tenant_id, _order_id) = setup().await;
        store
            .set_scanned_block(monero::Network::Mainnet, 1, "h1")
            .unwrap();
        let store = store.into_shared();

        let inner = FakeDaemonClient::new();
        for i in 1..=5 {
            inner.push_block(&format!("h{i}"), vec![]);
        }
        let daemon = DaemonFailingBlockHashAt {
            inner,
            failing_height: AtomicU64::new(3),
        };

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        {
            let s = store.lock();
            assert_eq!(
                s.max_scanned_height(monero::Network::Mainnet).unwrap(),
                Some(2),
                "the range must stop at the height that could not be read, not step over it"
            );
            for h in 3..=5 {
                assert!(
                    s.get_scanned_block_hash(monero::Network::Mainnet, h)
                        .unwrap()
                        .is_none(),
                    "block {h} must not be recorded once the range was abandoned at 3"
                );
            }
        }

        // And the next healthy tick re-covers the whole abandoned range, leaving a
        // contiguous window with nothing missing from the middle of it.
        daemon
            .failing_height
            .store(NO_FAILING_HEIGHT, Ordering::SeqCst);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        assert_eq!(
            s.max_scanned_height(monero::Network::Mainnet).unwrap(),
            Some(5)
        );
        for h in 1..=5 {
            assert!(
                s.get_scanned_block_hash(monero::Network::Mainnet, h)
                    .unwrap()
                    .is_some(),
                "block {h} must be recorded"
            );
        }
    }

    #[tokio::test]
    async fn a_reorg_whose_common_ancestor_hash_cannot_be_read_leaves_the_scanned_window_intact() {
        // The same permanent loss as
        // `a_reorg_back_to_the_oldest_recorded_block_does_not_look_like_a_scanner_that_never_ran`,
        // reached the other way: the rewind's re-anchor step *does* run, but the one
        // RPC it depends on - reading the common ancestor's hash - fails transiently.
        // Treating that as "there is no ancestor" (which an `.ok()` did) still deletes
        // every row, still empties the table, and still makes the next tick read the
        // network as never-scanned and re-seed at the tip. Every replacement block
        // between the reorg point and the tip is skipped forever, taking any payment
        // that exists only in the winning chain with it.
        //
        // Nothing has to be deleted this tick. The stored hashes still describe the
        // losing chain, so leaving them alone means the next tick detects the same
        // reorg and redoes the whole step - the same idempotent retry
        // `a_reconciliation_that_fails_partway_leaves_the_reorg_still_detectable`
        // relies on.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        store
            .set_scanned_block(monero::Network::Mainnet, 50, "hash_50_v1")
            .unwrap();
        let store = store.into_shared();

        let inner = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = inner.push_block(&format!("filler_{last_height}"), vec![]);
        }
        // The fork replaces 50 and 51; the payment exists only in the new 50.
        inner.reorg_from(50, vec![("new_50", vec![fixture_tx()]), ("new_51", vec![])]);
        // 49 is the common ancestor the rewind must re-anchor to - and the one hash
        // this node will not serve.
        let daemon = DaemonFailingBlockHashAt {
            inner,
            failing_height: AtomicU64::new(49),
        };

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        {
            let s = store.lock();
            assert_eq!(
                s.get_scanned_block_hash(monero::Network::Mainnet, 50).unwrap().as_deref(),
                Some("hash_50_v1"),
                "with no anchor available, the losing chain's hash must stay put so the reorg stays detectable"
            );
            assert!(
                s.max_scanned_height(monero::Network::Mainnet).unwrap().is_some(),
                "the window must never be emptied by a rewind that cannot re-anchor - an empty window reads \
                 as 'never scanned' and re-seeds at the tip"
            );
        }

        // The node recovers; the very next tick completes the rewind it deferred.
        daemon
            .failing_height
            .store(NO_FAILING_HEIGHT, Ordering::SeqCst);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(49),
            "the rewind must now land on the common ancestor"
        );

        // ...and the tick after that forward-scans the replacement chain and finds the
        // payment that only ever existed there.
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        let s = store.lock();
        assert_eq!(
            s.max_scanned_height(monero::Network::Mainnet).unwrap(),
            Some(51)
        );
        let payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(
            payments.len(),
            1,
            "the payment that exists only in the replacement chain must still be found"
        );
        assert_eq!(payments[0].block_height, Some(50));
    }

    #[tokio::test]
    async fn a_store_failure_marking_a_block_scanned_does_not_discard_the_ticks_mempool_matches() {
        // Every failure inside the height loop is documented as a `break`, never a
        // `return`, for one specific reason: the mempool matches gathered earlier in
        // the same tick still need their status recompute and their webhooks, and
        // returning here throws both away. Writing the scanned-block row was the one
        // remaining step that still used `?`, so a transient store error there didn't
        // just skip the block - it silently swallowed a real, already-recorded
        // zero-conf payment's `order.unconfirmed` notification too.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        daemon.set_mempool(vec![fixture_tx()]); // the payment arrives zero-conf
        store
            .lock()
            .set_scanned_block(monero::Network::Mainnet, 1, "h1")
            .unwrap();

        let webhook = store
            .lock()
            .create_webhook(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();

        // Fail only writes to `scanned_blocks`, leaving every other table (and every
        // read) working - a write failure at exactly that one step, not a broken
        // database.
        store
            .lock()
            .execute_raw_for_test(
                "CREATE TRIGGER simulated_scanned_block_write_failure BEFORE INSERT ON scanned_blocks
                 BEGIN SELECT RAISE(ABORT, 'simulated store failure'); END;",
            )
            .unwrap();

        // The storage failure is reported, but it stops only block scanning:
        // every other kind of work in the round still runs.
        let result = run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await;
        assert!(
            result.is_err(),
            "a storage failure is reported, not swallowed"
        );

        {
            let s = store.lock();
            assert_eq!(
                s.max_scanned_height(monero::Network::Mainnet).unwrap(),
                Some(1),
                "block 2 must stay unscanned so the next tick retries it"
            );
            // The mempool half of the tick must have survived intact: the payment is
            // recorded, the status was recomputed off it, and the transition was
            // announced.
            let payments = s
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap();
            assert_eq!(payments.len(), 1);
            assert_eq!(payments[0].block_height, None, "still mempool-only");
            let order = s
                .get_order(
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &shared::ids::OrderId::new(order_id.to_string()),
                )
                .unwrap()
                .unwrap();
            assert_ne!(
                order.status,
                crate::status::OrderStatus::Pending,
                "the mempool match's status recompute must not have been discarded"
            );
            let due = s
                .due_webhook_deliveries_for_test(crate::now_unix() + 1, 10)
                .unwrap();
            assert!(
                due.iter()
                    .any(|d| d.webhook_id == webhook.id && d.event_type.starts_with("order.")),
                "the mempool match's status-transition webhook must not have been discarded"
            );
        }

        // And the block the tick deliberately left behind is covered once the store
        // is healthy again.
        store
            .lock()
            .execute_raw_for_test("DROP TRIGGER simulated_scanned_block_write_failure;")
            .unwrap();
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(2)
        );
    }

    #[tokio::test]
    async fn a_reorg_back_to_the_oldest_recorded_block_does_not_look_like_a_scanner_that_never_ran()
    {
        // `forget_scanned_blocks_at_or_above` walks the high-water mark back to
        // `reorg_point - 1` - but only while a row still exists at or below that
        // height. When the reorg point *is* the oldest block this network has a row
        // for (routine on a recently-started scanner, whose window begins at the tip
        // it bootstrapped from) the delete empties the table outright, and an empty
        // table is precisely what `run_scan_tick` reads as "never scanned this
        // network", which makes it re-seed at the current tip. Every replacement
        // block between the reorg point and the tip is then skipped forever - taking
        // with it any payment that exists only in the chain that won, which is the
        // exact loss dropping those rows was introduced to prevent.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        // A scanner whose entire history is one block: height 50, on the old chain.
        store
            .set_scanned_block(monero::Network::Mainnet, 50, "hash_50_v1")
            .unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = daemon.push_block(&format!("filler_{last_height}"), vec![]);
        }
        // The fork replaces 50 and 51, and the payment lives only in the new 50.
        daemon.reorg_from(50, vec![("new_50", vec![fixture_tx()]), ("new_51", vec![])]);

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store.lock().max_scanned_height(monero::Network::Mainnet).unwrap(),
            Some(49),
            "the rewind must leave the high-water mark at the common ancestor, not at nothing at all"
        );

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        assert_eq!(
            s.max_scanned_height(monero::Network::Mainnet).unwrap(),
            Some(51)
        );
        let payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(
            payments.len(),
            1,
            "the payment that exists only in the replacement chain was found"
        );
        assert_eq!(payments[0].block_height, Some(50));
    }

    #[tokio::test]
    async fn a_tick_that_detects_a_reorg_never_announces_paid_from_the_chain_it_is_about_to_discard(
    ) {
        // Ordering, not logic: reconciliation and the per-tick recompute sweep both
        // ran, but the sweep ran first, so on the one tick where a reorg is detected
        // every non-terminal order was evaluated against heights reconciliation was
        // about to invalidate. An order whose payment had just been orphaned could
        // therefore cross its confirmation threshold and fire `order.paid` seconds
        // before the same tick voided that payment and fired the retraction - and a
        // merchant who acted on `order.paid` has already shipped.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await; // confirmations_required = 10
        store
            .create_webhook(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();
        let tx = fixture_tx();
        let key_images = key_images_of(&tx);

        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 50, "hash_50_v1")
            .unwrap();
        let (_, status) = store
            .recompute_order_status(&shared::ids::OrderId::new(order_id.to_string()), 50, 1600)
            .unwrap();
        assert_eq!(
            status,
            crate::status::OrderStatus::Confirming,
            "one confirmation, ten required"
        );
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = daemon.push_block(&format!("filler_{last_height}"), vec![]);
        }
        // The replacement chain runs to height 60 and does not contain the
        // transaction at all - so counted against the *stale* height 50 the payment
        // now looks eleven confirmations deep, comfortably past the threshold, while
        // in reality it has been double-spent.
        let replacement: Vec<(&str, Vec<Transaction>)> = [
            "v2_50", "v2_51", "v2_52", "v2_53", "v2_54", "v2_55", "v2_56", "v2_57", "v2_58",
            "v2_59", "v2_60",
        ]
        .into_iter()
        .map(|h| (h, vec![]))
        .collect();
        daemon.reorg_from(50, replacement);
        for ki in &key_images {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            order.amount_received_piconero, 0,
            "the voided payment must not count towards the order"
        );
        assert!(order.double_spend_detected_at.is_some());

        let events: Vec<String> = s
            .due_webhook_deliveries_for_test(crate::now_unix() + 1, 20)
            .unwrap()
            .into_iter()
            .map(|d| d.event_type)
            .collect();
        assert!(
            !events.iter().any(|e| e == "order.paid" || e == "order.overpaid"),
            "no settlement may ever be announced from payment data the same tick is in the middle of retracting, got: {events:?}"
        );
    }

    #[tokio::test]
    async fn a_payment_a_reorg_dropped_to_the_mempool_can_still_be_voided_when_later_proven_double_spent(
    ) {
        // Reconciliation itself writes `block_height = NULL` when the daemon reports
        // a payment's transaction back in the pool - and the query that finds
        // payments to re-examine filtered on `block_height >= ?`, which SQL's
        // three-valued logic makes false for NULL. So the very act of handling one
        // reorg made the payment invisible to every reconciliation that followed: it
        // could be proven double-spent a hundred times over and would never be
        // voided, propping up the order's received total permanently.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let key_images = key_images_of(&tx);

        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 50, "hash_50_v1")
            .unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = daemon.push_block(&format!("filler_{last_height}"), vec![]);
        }
        // First reorg: the transaction falls out of its block and back into the pool.
        daemon.set_mempool(vec![tx.clone()]);
        daemon.reorg_from(50, vec![("hash_50_v2", vec![])]);

        check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
            .await
            .unwrap();
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()[0]
                .block_height,
            None,
            "reconciliation itself is what puts the row into the state that used to hide it"
        );

        // Later, the attacker's replacement confirms: the pooled transaction is
        // dropped for good and its inputs are provably consumed elsewhere.
        daemon.drop_from_mempool(&tx);
        for ki in &key_images {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        store
            .lock()
            .set_scanned_block(monero::Network::Mainnet, 50, "hash_50_v2")
            .unwrap();
        daemon.reorg_from(50, vec![("hash_50_v3", vec![])]);

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2100)
            .await
            .unwrap();
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);

        let s = store.lock();
        assert!(s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()[0]
            .voided_at
            .is_some());
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(order.amount_received_piconero, 0);
        assert!(order.double_spend_detected_at.is_some());
    }

    #[tokio::test]
    async fn a_status_change_whose_webhook_cannot_be_enqueued_is_rolled_back_rather_than_lost() {
        // The status write and the enqueue announcing it were two separate
        // autocommits, and `recompute_order_status` decides "did anything change" by
        // comparing against the *stored* status - so the moment the new status lands,
        // the transition stops being detectable. A failure in the window between them
        // therefore didn't delay the merchant's `order.confirming`, it destroyed it:
        // no later tick could ever re-derive that a transition had happened.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        store
            .create_webhook(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();
        // Fail only enqueues, leaving every other write working, so this reproduces
        // the specific window rather than a broken database.
        store
            .execute_raw_for_test(
                "CREATE TRIGGER simulated_enqueue_failure BEFORE INSERT ON webhook_deliveries
                 BEGIN SELECT RAISE(ABORT, 'simulated store failure'); END;",
            )
            .unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![fixture_tx()]);

        let result = run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await;
        assert!(
            result.is_err(),
            "the enqueue failure must surface, not be swallowed"
        );
        assert_eq!(
            store
                .lock()
                .get_order(
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &shared::ids::OrderId::new(order_id.to_string())
                )
                .unwrap()
                .unwrap()
                .status,
            crate::status::OrderStatus::Pending,
            "the status must not have advanced past a transition nothing will ever announce"
        );

        assert_eq!(
            store
                .lock()
                .pending_payment_recomputes_page(monero::Network::Mainnet, "", 10_000)
                .unwrap(),
            vec![order_id.clone()]
        );

        // Once the store is healthy again, the next tick performs both halves.
        store
            .lock()
            .execute_raw_for_test("DROP TRIGGER simulated_enqueue_failure;")
            .unwrap();
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        assert_eq!(
            s.get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string())
            )
            .unwrap()
            .unwrap()
            .status,
            crate::status::OrderStatus::Confirming
        );
        let due = s
            .due_webhook_deliveries_for_test(crate::now_unix() + 1, 10)
            .unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].event_type, "order.confirming");
        assert!(s
            .pending_payment_recomputes_page(monero::Network::Mainnet, "", 10_000)
            .unwrap()
            .is_empty());
    }

    // ---------------------------------------------------------------------
    // Scenario-coverage block: chain, network and adversarial conditions.
    // See the checklist at the top of this module for what maps to what.
    // ---------------------------------------------------------------------

    /// A second, distinct transaction paying the *same* subaddress with the *same*
    /// outputs and the *same* key images as `fixture_tx()`. Only a nonce in
    /// `extra` differs, which changes the transaction hash (`extra` is part of
    /// the prefix, and the prefix is hashed) without touching the transaction
    /// public key, the output keys, or the amount commitments that scanning
    /// and amount recovery depend on. (Not `unlock_time`: a locked output is
    /// not credited, see `a_time_locked_output_is_not_credited`.)
    ///
    /// This is what makes multi-transaction and double-spend scenarios testable
    /// against real crypto with a single fixture: two variants are two genuinely
    /// different txids that both match the fixture view pair, and - because they
    /// share their inputs - they are also a realistic *conflicting pair*, which is
    /// exactly the shape of a Monero double-spend (same key images, different
    /// transaction).
    fn fixture_tx_variant(nonce: u64) -> Transaction {
        let mut tx = fixture_tx();
        let mut extra = tx.prefix.extra.try_parse();
        extra
            .0
            .push(monero::blockdata::transaction::SubField::Nonce(
                nonce.to_le_bytes().to_vec(),
            ));
        tx.prefix.extra = extra.into();
        tx
    }

    /// Only an output that can be spent is a payment. The fixture, locked
    /// until a height (or, past the height threshold, a time) the sender
    /// chose, pays the order nothing until then - and the engine doesn't
    /// wait: it is not credited at all.
    #[tokio::test]
    async fn a_time_locked_output_is_not_credited() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tenant = shared::ids::TenantId::new(tenant_id.to_string());
        let order = shared::ids::OrderId::new(order_id.to_string());
        for lock in [1u64, 1_000_000, 1_700_000_000] {
            let mut locked = fixture_tx();
            locked.prefix.unlock_time = monero::VarInt(lock);
            let scan = scan_transaction(&key_custody, handle, &locked, 0..3)
                .await
                .unwrap();
            assert!(
                scan.matches.is_empty(),
                "unlock_time {lock}: the output pays the wallet, and is not credited"
            );
            scan_transaction_for_tenant(
                &store,
                &key_custody,
                handle,
                &tenant,
                &locked,
                0..3,
                1500,
                None,
            )
            .await
            .unwrap();
        }
        assert!(store.get_all_payments(&order).unwrap().is_empty());
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &tenant,
            &fixture_tx(),
            0..3,
            1500,
            None,
        )
        .await
        .unwrap();
        assert_eq!(store.get_all_payments(&order).unwrap().len(), 1);
    }

    /// Pushes `1..=height` onto a fresh fake chain, each block empty and hashed
    /// `{prefix}_{h}`, and records the same hashes into `store` for `from..=height`
    /// - i.e. a scanner that has already worked its way up this chain.
    fn chain_scanned_to(
        store: &Store,
        height: u64,
        recorded_from: u64,
        prefix: &str,
    ) -> FakeDaemonClient {
        let daemon = FakeDaemonClient::new();
        for h in 1..=height {
            daemon.push_block(&format!("{prefix}_{h}"), vec![]);
        }
        for h in recorded_from..=height {
            store
                .set_scanned_block(monero::Network::Mainnet, h, &format!("{prefix}_{h}"))
                .unwrap();
        }
        daemon
    }

    /// Replaces `fork..=tip` with a chain whose hashes are prefixed `new_`, keeping
    /// the (hash, txs) plumbing of `reorg_from`'s `&str` API out of every caller.
    fn reorg_to_new_chain(
        daemon: &FakeDaemonClient,
        fork: u64,
        tip: u64,
        tx_at: Option<(u64, Transaction)>,
    ) {
        reorg_to_chain(daemon, fork, tip, "new", tx_at)
    }

    /// `reorg_to_new_chain` with a caller-chosen hash prefix, for tests that switch
    /// between more than two chains and need each one's hashes to be distinct from
    /// every other's.
    fn reorg_to_chain(
        daemon: &FakeDaemonClient,
        fork: u64,
        tip: u64,
        prefix: &str,
        tx_at: Option<(u64, Transaction)>,
    ) {
        let hashes: Vec<String> = (fork..=tip).map(|h| format!("{prefix}_{h}")).collect();
        let blocks: Vec<(&str, Vec<Transaction>)> = hashes
            .iter()
            .enumerate()
            .map(|(i, hash)| {
                let height = fork + i as u64;
                let txs = match &tx_at {
                    Some((h, tx)) if *h == height => vec![tx.clone()],
                    _ => vec![],
                };
                (hash.as_str(), txs)
            })
            .collect();
        daemon.reorg_from(fork, blocks);
    }

    /// One reorg of `tip - fork + 1` blocks against a scanner that has recorded the
    /// previous 30 blocks, returning the height reconciliation reports it at.
    async fn reorg_point_for(fork: u64, tip: u64, reorg_check_depth: u64) -> Option<u64> {
        let store = Store::open_in_memory().unwrap();
        let daemon = chain_scanned_to(&store, tip, tip - 30, "old");
        reorg_to_new_chain(&daemon, fork, tip, None);
        let store = store.into_shared();
        check_for_reorg_and_reconcile(&store, &daemon, "mainnet", reorg_check_depth, 2000)
            .await
            .unwrap()
            .reorg_detected_at
    }

    #[tokio::test]
    async fn a_reorg_is_detected_at_its_true_fork_point_at_every_depth_the_window_covers() {
        // Reorg depth is not a parameter anything in the detection loop branches on,
        // but "not branching on it" is precisely the kind of claim that quietly stops
        // being true, and the real network has now produced reorgs (18 blocks, on
        // mainnet, in September 2025) deeper than this system's default
        // `confirmations_required`. Walking every depth the window covers - one block
        // through the window edge itself - is cheap and forecloses an off-by-one at
        // either end.
        //
        // With `reorg_check_depth = 20` the window is `tip-20..=tip`, i.e. 21
        // heights, so a 21-block reorg is the deepest one whose true fork point is
        // still inside it.
        let tip = 100;
        for depth in 1..=21u64 {
            let fork = tip - depth + 1;
            assert_eq!(
                reorg_point_for(fork, tip, 20).await,
                Some(fork),
                "a {depth}-block reorg (fork at {fork}) must be reported at its own fork point"
            );
        }
    }

    #[tokio::test]
    async fn a_reorg_deeper_than_the_window_is_reported_at_the_window_edge_and_leaves_older_payments_alone(
    ) {
        // The documented limitation (§DESIGN.md 3: "defending against reorgs deeper
        // than a configured window" is a non-goal), pinned down as executable
        // behavior rather than left as prose. What actually happens is worth being
        // precise about, because it is *not* "the reorg is missed": every stored hash
        // in the window mismatches, so the first one checked - the window edge - is
        // reported as the reorg point. That is a real detection, but at a height
        // above the true fork, so payments below the window keep counting towards
        // their orders at heights that no longer exist, and the replacement chain's
        // blocks below the window are never rescanned.
        //
        // The operational consequence, spelled out for whoever tunes this: the only
        // defense against a reorg deeper than `reorg_check_depth` is configuring
        // `reorg_check_depth` deeper than any reorg you intend to survive. The
        // September 2025 mainnet reorg was 18 blocks; the default is 20.
        let tip = 100;
        assert_eq!(
            reorg_point_for(79, tip, 20).await,
            Some(80),
            "a fork one block below the window is reported at the window edge, not at 79"
        );

        // And the payment orphaned below the window is left untouched by that pass.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let daemon = chain_scanned_to(&store, tip, tip - 30, "old");
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &fixture_tx(),
            0..3,
            1500,
            Some(79),
        )
        .await
        .unwrap();
        reorg_to_new_chain(&daemon, 79, tip, None);
        let store = store.into_shared();

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
            .await
            .unwrap();
        assert_eq!(report.reorg_detected_at, Some(80));
        let payments = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(
            payments[0].block_height,
            Some(79),
            "a payment below the window is not re-evaluated - this is the documented cost of the window, \
             and the reason `reorg_check_depth` must exceed the deepest reorg a deployment wants to survive"
        );
    }

    #[tokio::test]
    async fn a_settled_order_is_walked_back_when_a_reorg_deeper_than_confirmations_required_orphans_its_payment(
    ) {
        // The September 2025 shape, at this system's defaults: an 18-block reorg
        // against `confirmations_required = 10` reverts blocks the merchant was
        // already told were final. Nothing in the reorg path branches on
        // `confirmations_required` (10 and 11 blocks deep take exactly the same code
        // path), so what needs proving is the end-to-end consequence: an order that
        // reached a terminal, shipped-against status is walked back, and the merchant
        // is told, rather than the retraction being visible only to a poller.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await; // confirmations_required = 10
        store
            .create_webhook(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();
        let tx = fixture_tx();
        let daemon = chain_scanned_to(&store, 100, 70, "old");
        // Payment 18 blocks deep - comfortably "final" at ten confirmations.
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(83),
        )
        .await
        .unwrap();
        let (_, status) = store
            .recompute_order_status(&shared::ids::OrderId::new(order_id.to_string()), 100, 1600)
            .unwrap();
        assert_eq!(
            status,
            crate::status::OrderStatus::Overpaid,
            "18 confirmations deep before the reorg"
        );

        // An 18-block reorg that does not carry the transaction, and whose inputs are
        // proven consumed by something else.
        for ki in &key_images_of(&tx) {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        reorg_to_new_chain(&daemon, 83, 100, None);
        let store = store.into_shared();

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
            .await
            .unwrap();
        assert_eq!(report.reorg_detected_at, Some(83));
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);

        let s = store.lock();
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            order.amount_received_piconero, 0,
            "the orphaned payment must stop counting"
        );
        assert_eq!(order.status, crate::status::OrderStatus::Pending);
        assert!(order.double_spend_detected_at.is_some());
        let events: Vec<String> = s
            .due_webhook_deliveries_for_test(crate::now_unix() + 1, 10)
            .unwrap()
            .into_iter()
            .map(|d| d.event_type)
            .collect();
        assert!(
            events.contains(&"order.pending".to_string()),
            "a merchant told `overpaid` must be told when that stops being true: {events:?}"
        );
    }

    #[tokio::test]
    async fn a_reorg_whose_fork_is_below_every_recorded_block_rewinds_to_that_oldest_block() {
        // The boundary the round-2 fix
        // (`a_reorg_back_to_the_oldest_recorded_block_does_not_look_like_a_scanner_that_never_ran`)
        // sits next to, probed explicitly: there, the fork *is* the oldest recorded
        // block; here it is strictly below it, so the common ancestor falls outside
        // the recorded window entirely. Detection still fires at the oldest recorded
        // height (nothing lower has a hash to compare), the rewind re-anchors one
        // block below it, and the window is never emptied - the state that would
        // otherwise read as "this network has never been scanned" and re-seed at the
        // tip, skipping every replacement block in between.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        // A scanner whose entire history is heights 50..=52.
        let daemon = FakeDaemonClient::new();
        for h in 1..=52 {
            daemon.push_block(&format!("old_{h}"), vec![]);
        }
        for h in 50..=52 {
            store
                .set_scanned_block(monero::Network::Mainnet, h, &format!("old_{h}"))
                .unwrap();
        }
        // The fork is at 45 - five blocks below anything this scanner recorded - and
        // the payment exists only in the replacement chain.
        reorg_to_new_chain(&daemon, 45, 52, Some((51, fixture_tx())));
        let store = store.into_shared();

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(49),
            "the rewind must land just below the oldest recorded block, never on an empty window"
        );

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        let s = store.lock();
        assert_eq!(
            s.max_scanned_height(monero::Network::Mainnet).unwrap(),
            Some(52)
        );
        let payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(
            payments.len(),
            1,
            "the payment that exists only in the replacement chain must be found"
        );
        assert_eq!(payments[0].block_height, Some(51));
    }

    /// A transaction that *conflicts* with `fixture_tx()` - it spends the same
    /// inputs, so it carries the same key images - but pays somebody else: its
    /// outputs are stripped, so it matches no wallet in this test suite. This is the
    /// attacker's side of a Monero double-spend, which is always "same key images,
    /// different transaction" (there is no replace-by-fee to express it any other
    /// way).
    pub(crate) fn conflicting_tx(seed: u64) -> Transaction {
        let mut tx = fixture_tx_variant(seed);
        tx.prefix.outputs.clear();
        tx
    }

    /// What `fixture_tx()` pays, and to which subaddress: checked by
    /// `the_transaction_variants_these_tests_are_built_from_are_genuinely_distinct`.
    pub(crate) const FIXTURE_AMOUNT_PICONERO: u64 = 7_000_000_000;
    const FIXTURE_SUBADDRESS: SubaddressIndex = SubaddressIndex { major: 0, minor: 1 };

    /// A second payment to the same order that is *not* in conflict with the
    /// fixture: a sender of its own, so its own transaction key and its own
    /// one-time output key (two outputs sharing one is the "burning bug", and
    /// only one of them is ever credited - see
    /// `an_output_reusing_a_credited_one_time_key_is_not_credited_twice`), and
    /// different inputs, hence different key images. Pays the fixture's
    /// amount to the fixture's subaddress, in the clear (no RingCT part).
    fn independent_payment_tx(seed: u8) -> Transaction {
        use monero::blockdata::transaction::{ExtraField, SubField, TxOut, TxOutTarget};
        use monero::cryptonote::onetime_key::KeyGenerator;
        let view_pair = monero::ViewPair {
            view: PrivateKey::from_slice(&fixture_view_key()).unwrap(),
            spend: PublicKey::from_slice(&fixture_spend_pubkey()).unwrap(),
        };
        let (view, spend) =
            monero::cryptonote::subaddress::get_public_keys(&view_pair, FIXTURE_SUBADDRESS);
        // Deterministic per seed, not random: fixture data.
        let mut r_bytes = [seed; 32];
        r_bytes[31] &= 0x0f;
        let r = PrivateKey::from_slice(&r_bytes).unwrap();
        let sender = KeyGenerator::from_random(view, spend, r);
        let mut tx = fixture_tx_variant(0x1000 + seed as u64);
        for input in tx.prefix.inputs.iter_mut() {
            if let monero::blockdata::transaction::TxIn::ToKey { k_image, .. } = input {
                let mut bytes = k_image.image.to_bytes();
                bytes[0] ^= seed;
                k_image.image = monero::cryptonote::hash::Hash(bytes);
            }
        }
        // A subaddress payment's transaction key is r*D, not r*G.
        tx.prefix.extra = ExtraField(vec![SubField::TxPublicKey(r * &spend)]).into();
        tx.prefix.outputs = vec![TxOut {
            amount: monero::VarInt(FIXTURE_AMOUNT_PICONERO),
            target: TxOutTarget::ToKey {
                key: sender.one_time_key(0).to_bytes(),
            },
        }];
        tx.rct_signatures = monero::util::ringct::RctSig { sig: None, p: None };
        tx
    }

    /// `fixture_tx()` under another id: the same outputs, so the same
    /// one-time key, from other inputs. What a sender who reuses a
    /// transaction key produces, by mistake or on purpose ("burning bug"):
    /// only one of the two outputs can ever be spent.
    fn key_reusing_tx(seed: u8) -> Transaction {
        let mut tx = fixture_tx_variant(0x2000 + seed as u64);
        for input in tx.prefix.inputs.iter_mut() {
            if let monero::blockdata::transaction::TxIn::ToKey { k_image, .. } = input {
                let mut bytes = k_image.image.to_bytes();
                bytes[1] ^= seed;
                k_image.image = monero::cryptonote::hash::Hash(bytes);
            }
        }
        tx
    }

    #[tokio::test]
    async fn an_output_reusing_a_credited_one_time_key_is_not_credited_twice() {
        // Two outputs carrying one one-time key are one spendable output:
        // crediting both pays an order with money the merchant cannot have.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tenant = shared::ids::TenantId::new(tenant_id.to_string());
        let order = shared::ids::OrderId::new(order_id.to_string());
        let first = fixture_tx();
        let reused = key_reusing_tx(1);
        assert_ne!(tx_id_hex(&first), tx_id_hex(&reused));
        for tx in [&first, &reused, &first] {
            scan_transaction_for_tenant(
                &store,
                &key_custody,
                handle,
                &tenant,
                tx,
                0..3,
                1500,
                None,
            )
            .await
            .unwrap();
        }
        let payments = store.get_all_payments(&order).unwrap();
        assert_eq!(
            payments.len(),
            1,
            "the second output with the key is refused"
        );
        assert_eq!(payments[0].txid, tx_id_hex(&first));
        let scan = scan_transaction(&key_custody, handle, &first, 0..3)
            .await
            .unwrap();
        let (index, key) = scan.output_keys.iter().next().unwrap();
        assert_eq!(payments[0].output_index, *index as i64);
        assert_eq!(payments[0].output_key.as_deref(), Some(key.as_str()));

        // A genuinely independent second payment is credited.
        let independent = independent_payment_tx(2);
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &tenant,
            &independent,
            0..3,
            1500,
            None,
        )
        .await
        .unwrap();
        assert_eq!(store.get_all_payments(&order).unwrap().len(), 2);

        // Once the credited one is voided (its transaction lost a double
        // spend), the other output with the key is the one that can be spent.
        store
            .void_payment(&order, &tx_id_hex(&first), *index as i64, 1600)
            .unwrap();
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &tenant,
            &reused,
            0..3,
            1700,
            None,
        )
        .await
        .unwrap();
        let payments = store.get_all_payments(&order).unwrap();
        assert_eq!(payments.len(), 3);
        assert!(payments
            .iter()
            .any(|p| p.txid == tx_id_hex(&reused) && p.voided_at.is_none()));
    }

    #[tokio::test]
    async fn the_transaction_variants_these_tests_are_built_from_are_genuinely_distinct() {
        // Every multi-transaction and double-spend scenario below rests on these
        // three shapes actually being what they claim. If a future `monero-rs`
        // changed what the transaction hash covers, or which fields scanning reads,
        // the scenarios would still "pass" while quietly testing one transaction
        // against itself.
        let (_store, key_custody, handle, _tenant_id, _order_id) = setup().await;
        let base = fixture_tx();
        let variant = fixture_tx_variant(42);
        let conflicting = conflicting_tx(42);
        let independent = independent_payment_tx(3);

        assert_ne!(
            tx_id_hex(&base),
            tx_id_hex(&variant),
            "a different extra nonce must be a different txid"
        );
        assert_ne!(tx_id_hex(&base), tx_id_hex(&conflicting));
        assert_ne!(tx_id_hex(&base), tx_id_hex(&independent));

        assert_eq!(
            key_images_of(&base),
            key_images_of(&variant),
            "a variant spends the same inputs"
        );
        assert_eq!(
            key_images_of(&base),
            key_images_of(&conflicting),
            "a conflicting tx is one sharing key images"
        );
        assert_ne!(
            key_images_of(&base),
            key_images_of(&independent),
            "an independent payment spends other inputs"
        );

        let fixture_scan = scan_transaction(&key_custody, handle, &base, 0..3)
            .await
            .unwrap();
        assert_eq!(
            fixture_scan.matches[0].amount_piconero,
            Some(FIXTURE_AMOUNT_PICONERO)
        );
        assert_eq!(fixture_scan.matches[0].subaddress_index, FIXTURE_SUBADDRESS);
        let independent_scan = scan_transaction(&key_custody, handle, &independent, 0..3)
            .await
            .unwrap();
        assert_eq!(
            independent_scan.matches[0].amount_piconero,
            Some(FIXTURE_AMOUNT_PICONERO),
            "an independent payment pays the fixture's amount"
        );
        assert_ne!(
            fixture_scan.output_keys, independent_scan.output_keys,
            "an independent payment has its own one-time key"
        );

        for (label, tx, expected) in [
            ("the fixture itself", &base, 1),
            (
                "a nonce in extra must not break output matching",
                &variant,
                1,
            ),
            (
                "changing key images must not break output matching",
                &independent,
                1,
            ),
            (
                "the attacker's transaction pays somebody else",
                &conflicting,
                0,
            ),
        ] {
            let matches = scan_transaction(&key_custody, handle, tx, 0..3)
                .await
                .unwrap()
                .matches
                .len();
            assert_eq!(matches, expected, "{label}");
        }
    }

    #[tokio::test]
    async fn rapidly_alternating_chain_tips_never_lose_or_double_count_a_payment() {
        // The selfish-mining shape, from this scanner's point of view. A pool that
        // withholds blocks and publishes them in bursts (which is what Qubic was
        // doing against Monero through August-September 2025) does not present as one
        // clean reorg: it presents as the tip repeatedly changing its mind, with the
        // same transaction moving between blocks - or out of the chain and back -
        // several times in a row.
        //
        // The property that has to survive that is bookkeeping, not detection: one
        // payment row, counted once, at whatever height the chain most recently
        // agreed on, with no double-spend ever flagged (nothing was ever double-spent
        // here - the transaction stayed valid throughout, only its block kept
        // changing).
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let daemon = chain_scanned_to(&store, 51, 40, "c0");
        // The payment starts life mined at height 50 on the chain the scanner has
        // already recorded.
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        let store = store.into_shared();

        // Three rounds of the tip trading places: the withheld chain wins and the
        // transaction is nowhere, then the honest chain wins it back at a different
        // height, and so on. Two ticks per round because a rewind and the forward
        // rescan it enables are deliberately separate ticks (see
        // `after_a_reorg_the_replacement_blocks_are_forward_scanned_again`).
        for round in 0..3u64 {
            let withheld = format!("w{round}");
            let public = format!("p{round}");
            reorg_to_chain(&daemon, 50, 52 + round, &withheld, None); // the tx vanishes entirely
            for _ in 0..2 {
                run_scan_tick(
                    &store,
                    &key_custody,
                    &daemon,
                    "mainnet",
                    &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
                    20,
                    0,
                )
                .await
                .unwrap();
            }
            reorg_to_chain(
                &daemon,
                50,
                53 + round,
                &public,
                Some((51 + round, tx.clone())),
            ); // ...and comes back, deeper each time
            for _ in 0..2 {
                run_scan_tick(
                    &store,
                    &key_custody,
                    &daemon,
                    "mainnet",
                    &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
                    20,
                    0,
                )
                .await
                .unwrap();
            }

            let s = store.lock();
            let payments = s
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap();
            assert_eq!(
                payments.len(),
                1,
                "round {round}: one transaction is one payment row, however often it moves"
            );
            assert!(
                payments[0].voided_at.is_none(),
                "round {round}: a remined transaction is not a double-spend"
            );
            assert_eq!(
                payments[0].block_height,
                Some((51 + round) as i64),
                "round {round}: at the height the chain last agreed on"
            );
            let order = s
                .get_order(
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &shared::ids::OrderId::new(order_id.to_string()),
                )
                .unwrap()
                .unwrap();
            assert!(
                order.double_spend_detected_at.is_none(),
                "round {round}: nothing here was ever double-spent"
            );
            assert_eq!(
                order.amount_received_piconero, payments[0].amount_piconero,
                "round {round}: counted exactly once"
            );
        }
    }

    #[tokio::test]
    async fn a_double_spend_mined_in_a_different_block_than_the_original_voids_it_once() {
        // The variant the existing double-spend tests don't cover: the conflicting
        // transaction isn't just "somewhere in the chain", it is mined at a
        // *different height* than the original occupied, and the replacement chain is
        // longer than the one it replaced. Nothing about the reasoning changes -
        // `locate_transaction` says the original is nowhere, its key images say
        // something else spent them - but "the heights line up" is an assumption
        // worth not having.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let attacker = conflicting_tx(9);
        let daemon = chain_scanned_to(&store, 51, 40, "old");
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        for ki in &key_images_of(&tx) {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        // The replacement chain runs two blocks longer and carries the attacker's
        // transaction at 53, nowhere near where the original sat.
        reorg_to_new_chain(&daemon, 50, 53, Some((53, attacker)));
        let store = store.into_shared();

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
            .await
            .unwrap();
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);

        let s = store.lock();
        let payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(
            payments.len(),
            1,
            "the attacker's transaction pays nobody here - it must not become a payment row"
        );
        assert!(payments[0].voided_at.is_some());
        assert_eq!(
            s.get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string())
            )
            .unwrap()
            .unwrap()
            .amount_received_piconero,
            0
        );
    }

    #[tokio::test]
    async fn voiding_one_of_an_orders_two_payments_leaves_the_other_counting() {
        // §TESTING.md 3's "exact two-transaction scenario from design review", at the
        // scanner level rather than the status-function level: an order paid by two
        // independent transactions, one of which is double-spent. The surviving
        // payment must keep counting, the order's total must fall by exactly the
        // voided amount, and `double_spend_detected_at` must be stamped even though
        // the two facts are on different axes.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let first = fixture_tx();
        let second = independent_payment_tx(5);
        let daemon = chain_scanned_to(&store, 51, 40, "old");
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &first,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &second,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        // What the two payments are worth together, before either is voided.
        let total_before: u64 = store
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .iter()
            .map(|p| p.amount_piconero)
            .sum();

        // Only the first transaction is double-spent; the second is remined at 51.
        for ki in &key_images_of(&first) {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        reorg_to_new_chain(&daemon, 50, 51, Some((51, second.clone())));
        let store = store.into_shared();

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
            .await
            .unwrap();
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);

        let s = store.lock();
        let payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(payments.len(), 2);
        let (voided, kept): (Vec<_>, Vec<_>) = payments.iter().partition(|p| p.voided_at.is_some());
        assert_eq!(
            voided.len(),
            1,
            "exactly the double-spent transaction is written off"
        );
        assert_eq!(voided[0].txid, tx_id_hex(&first));
        assert_eq!(kept[0].txid, tx_id_hex(&second));
        assert_eq!(
            kept[0].block_height,
            Some(51),
            "the survivor follows the chain to its new height"
        );
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            order.amount_received_piconero, kept[0].amount_piconero,
            "only the survivor's amount counts"
        );
        assert_eq!(
            total_before - order.amount_received_piconero,
            voided[0].amount_piconero,
            "the total falls by exactly the voided amount"
        );
        assert!(
            order.double_spend_detected_at.is_some(),
            "the incident is recorded regardless of the resulting status"
        );
    }

    #[tokio::test]
    async fn a_third_transaction_claiming_the_same_inputs_keeps_the_original_voided() {
        // The un-voiding path, pushed one step further than
        // `a_voided_payment_is_restored_when_its_transaction_returns_to_the_chain`
        // takes it. There, the transaction that beat the original was itself reorged
        // out and the original came back. Here it is reorged out and a *third*
        // transaction - a different txid, still spending the same inputs - takes its
        // place. The original is still nowhere, its inputs are still provably
        // consumed by something else, and it must therefore stay voided: un-voiding
        // is conditioned on the original actually returning to the chain, not on the
        // particular replacement that displaced it going away.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let key_images = key_images_of(&tx);
        let daemon = chain_scanned_to(&store, 50, 40, "old");
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        for ki in &key_images {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        reorg_to_chain(&daemon, 50, 50, "b", Some((50, conflicting_tx(1)))); // the second transaction wins
        let store = store.into_shared();

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
            .await
            .unwrap();
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);
        assert!(store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()[0]
            .voided_at
            .is_some());

        // The second transaction's block loses in turn, but a third one - not the
        // original - takes the inputs.
        store
            .lock()
            .set_scanned_block(monero::Network::Mainnet, 50, "b_50")
            .unwrap();
        reorg_to_chain(&daemon, 50, 50, "c", Some((50, conflicting_tx(2))));

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2100)
            .await
            .unwrap();
        assert_eq!(report.reorg_detected_at, Some(50));

        let s = store.lock();
        let payment = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .remove(0);
        assert!(
            payment.voided_at.is_some(),
            "the original never returned to the chain - its replacement being replaced changes nothing"
        );
        assert_eq!(
            s.get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string())
            )
            .unwrap()
            .unwrap()
            .amount_received_piconero,
            0
        );
    }

    #[tokio::test]
    async fn a_zero_conf_order_double_spent_out_of_the_mempool_is_voided_with_no_reorg_involved() {
        // The gap this sweep was added to close, and the most consequential one in
        // this file: the textbook attack on a merchant watching the mempool involves
        // no reorg at all. Broadcast transaction A so the merchant's node sees it
        // (with `confirmations_required = 0` on this order, it reads `paid` -
        // native 0-conf, no confirmations needed at all - immediately), then get
        // transaction B, spending the same inputs, mined instead. A is never mined,
        // so no block hash the scanner recorded ever changes, so reorg
        // reconciliation - the only thing that ever re-examined an existing payment
        // - never runs. The payment sat at `block_height IS NULL` forever, counting
        // in full towards an order that was never paid, and no
        // `order.double_spend_detected` webhook ever fired.
        let (store, key_custody, handle, tenant_id, order_id) =
            setup_with_confirmations_override(Some(0)).await;
        store
            .create_webhook(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();
        let tx = fixture_tx();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.set_mempool(vec![tx.clone()]);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        {
            let s = store.lock();
            let order = s
                .get_order(
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &shared::ids::OrderId::new(order_id.to_string()),
                )
                .unwrap()
                .unwrap();
            assert_eq!(
                order.status,
                crate::status::OrderStatus::Overpaid,
                "settled off the mempool sighting alone"
            );
        }

        // The attack lands: A leaves the pool without ever being mined, and a
        // different transaction spending its inputs is confirmed.
        daemon.drop_from_mempool(&tx);
        daemon.push_block("h2", vec![conflicting_tx(11)]);
        for ki in &key_images_of(&tx) {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        let payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(payments.len(), 1);
        assert!(
            payments[0].voided_at.is_some(),
            "a proven double-spend must be written off with or without a reorg"
        );
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(order.amount_received_piconero, 0);
        assert_eq!(order.status, crate::status::OrderStatus::Pending);
        assert!(order.double_spend_detected_at.is_some());
        let events: Vec<String> = s
            .due_webhook_deliveries_for_test(crate::now_unix() + 1, 10)
            .unwrap()
            .into_iter()
            .map(|d| d.event_type)
            .collect();
        assert!(
            events.contains(&"order.double_spend_detected".to_string()),
            "the merchant who shipped against this needs telling: {events:?}"
        );
        assert!(
            events.contains(&"order.pending".to_string()),
            "and the status retraction is its own event: {events:?}"
        );
    }

    /// The chain `setup_with_one_voided_double_spend` builds, on a fresh node: a
    /// recheck asks a node that agrees about the blocks (so no reorg is
    /// detected) but whose key-image answers each test chooses.
    fn chain_replica() -> FakeDaemonClient {
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![conflicting_tx(11)]);
        daemon
    }

    /// Builds a store with one order whose single zero-conf payment has already been
    /// voided as a proven double-spend, via the exact same real path
    /// `a_zero_conf_order_double_spent_out_of_the_mempool_is_voided_with_no_reorg_involved`
    /// proves is reachable. For the void recheck's own tests below, which start
    /// from an already-voided payment and exercise only the *recheck*, not how
    /// it got voided in the first place.
    async fn setup_with_one_voided_double_spend() -> (crate::store::SharedStore, String, String) {
        let (store, key_custody, handle, tenant_id, order_id) =
            setup_with_confirmations_override(Some(0)).await;
        store
            .create_webhook(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();
        let tx = fixture_tx();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.set_mempool(vec![tx.clone()]);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        daemon.drop_from_mempool(&tx);
        daemon.push_block("h2", vec![conflicting_tx(11)]);
        for ki in &key_images_of(&tx) {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        assert!(
            store.lock().get_all_payments(&shared::ids::OrderId::new(order_id.to_string())).unwrap()[0].voided_at.is_some(),
            "test setup sanity check: the payment must actually be voided before these tests exercise the recheck"
        );
        (store, tenant_id, order_id)
    }

    /// One scheduler round with a void recheck pass due now (a pass starts at
    /// most every few minutes; the setup's own rounds started one). The
    /// recheck is the upkeep tier's; nothing else in the round touches a
    /// void.
    async fn round_with_void_recheck_due(
        store: &crate::store::SharedStore,
        daemon: &dyn MoneroDaemonClient,
    ) -> Result<()> {
        {
            let s = store.lock();
            s.set_scheduler_position::<crate::store::position::VoidRecheckPassStarted>(
                monero::Network::Mainnet,
                &i64::MIN,
            )
            .unwrap();
            s.set_scheduler_position::<crate::store::position::VoidRecheck>(
                monero::Network::Mainnet,
                &0,
            )
            .unwrap();
        }
        run_scan_tick(
            store,
            &PlainKeyCustody::default(),
            daemon,
            "mainnet",
            &[],
            20,
            0,
        )
        .await
    }

    fn voided(store: &crate::store::SharedStore, order_id: &str) -> Vec<bool> {
        store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .iter()
            .map(|p| p.voided_at.is_some())
            .collect()
    }

    #[tokio::test]
    async fn the_void_recheck_reverses_a_void_no_longer_supported_by_fresh_evidence() {
        let (store, tenant_id, order_id) = setup_with_one_voided_double_spend().await;

        // Every key image defaults to `Unspent` unless explicitly told otherwise, so
        // simply not calling `set_key_image_status` models the original accusation
        // no longer holding up.
        round_with_void_recheck_due(&store, &chain_replica())
            .await
            .unwrap();

        let s = store.lock();
        let payment = &s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()[0];
        assert!(payment.voided_at.is_none(), "the void should be reversed");
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert!(
            order.double_spend_detected_at.is_none(),
            "the only voided payment on the order was cleared - the sticky flag should clear too"
        );
        let events: Vec<String> = s
            .due_webhook_deliveries_for_test(crate::now_unix() + 1, 10)
            .unwrap()
            .into_iter()
            .map(|d| d.event_type)
            .collect();
        assert!(
            events.contains(&"order.double_spend_reversed".to_string()),
            "the merchant told about the original accusation deserves to be told it was wrong too: {events:?}"
        );
    }

    #[tokio::test]
    async fn malformed_or_empty_stored_key_images_never_restore_a_void() {
        for raw in ["not json", "[]"] {
            let (store, _tenant_id, order_id) = setup_with_one_voided_double_spend().await;
            store.lock().overwrite_payment_key_images_for_test(
                &shared::ids::OrderId::new(order_id.to_string()),
                raw,
            );
            round_with_void_recheck_due(&store, &chain_replica())
                .await
                .unwrap();
            assert_eq!(voided(&store, &order_id), vec![true]);
        }
    }

    #[tokio::test]
    async fn node_disagreement_never_restores_a_void() {
        let (store, _tenant_id, order_id) = setup_with_one_voided_double_spend().await;
        let payment = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()[0]
            .clone();
        let images: Vec<String> = serde_json::from_str(&payment.key_images_json).unwrap();
        let spent = chain_replica();
        let unspent = chain_replica();
        for image in &images {
            spent.set_key_image_status(image, KeyImageStatus::SpentInBlockchain);
            unspent.set_key_image_status(image, KeyImageStatus::Unspent);
        }
        let daemon = FallbackDaemonClient::new(vec![
            FallbackNode {
                label: "spent".to_string(),
                client: std::sync::Arc::new(spent),
            },
            FallbackNode {
                label: "unspent".to_string(),
                client: std::sync::Arc::new(unspent),
            },
        ]);
        round_with_void_recheck_due(&store, &daemon).await.unwrap();
        assert_eq!(voided(&store, &order_id), vec![true]);
    }

    #[tokio::test]
    async fn the_void_recheck_leaves_a_still_supported_void_alone() {
        let (store, tenant_id, order_id) = setup_with_one_voided_double_spend().await;
        let payment = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()[0]
            .clone();
        let key_images: Vec<String> = serde_json::from_str(&payment.key_images_json).unwrap();

        let recheck_daemon = chain_replica();
        for ki in &key_images {
            recheck_daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }

        round_with_void_recheck_due(&store, &recheck_daemon)
            .await
            .unwrap();
        assert_eq!(
            voided(&store, &order_id),
            vec![true],
            "a still-supported void must not be reversed"
        );
        assert!(store
            .lock()
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string())
            )
            .unwrap()
            .unwrap()
            .double_spend_detected_at
            .is_some());
    }

    #[tokio::test]
    async fn the_void_recheck_ignores_a_void_outside_the_recheck_window() {
        let (store, _tenant_id, order_id) = setup_with_one_voided_double_spend().await;
        let payment = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()[0]
            .clone();

        // Backdate the void to well outside the recheck window - fresh evidence would
        // clear it if only the recheck looked, but it's aged out.
        let old_timestamp = crate::now_unix() - DOUBLE_SPEND_RECHECK_WINDOW_SECS - 3600;
        {
            let s = store.lock();
            assert!(s
                .unvoid_payment(
                    &shared::ids::OrderId::new(order_id.to_string()),
                    &payment.txid,
                    payment.output_index
                )
                .unwrap());
            assert!(s
                .void_payment(
                    &shared::ids::OrderId::new(order_id.to_string()),
                    &payment.txid,
                    payment.output_index,
                    old_timestamp
                )
                .unwrap());
        }

        round_with_void_recheck_due(&store, &chain_replica())
            .await
            .unwrap(); // would say Unspent if asked
        assert_eq!(
            voided(&store, &order_id),
            vec![true],
            "a void outside the recheck window must not be touched"
        );
    }

    #[tokio::test]
    async fn the_void_recheck_keeps_the_flag_set_while_another_voided_payment_still_justifies_it() {
        // Two independent payments on one order, both voided - only one is a false
        // accusation. The order-level sticky flag must survive the correction of the
        // first, since the second remains a genuine, still-supported incident.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let first = fixture_tx();
        let second = independent_payment_tx(5);
        let now = crate::now_unix();
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &first,
            0..3,
            now,
            Some(50),
        )
        .await
        .unwrap();
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &second,
            0..3,
            now,
            Some(50),
        )
        .await
        .unwrap();
        // Void both by their *actual* recorded output_index - not assumed to be 0,
        // since that depends on which output of each fixture transaction matched.
        for payment in store
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
        {
            store
                .void_payment(
                    &shared::ids::OrderId::new(order_id.to_string()),
                    &payment.txid,
                    payment.output_index,
                    now,
                )
                .unwrap();
        }
        store
            .mark_double_spend_detected(&shared::ids::OrderId::new(order_id.to_string()), now)
            .unwrap();

        let store = store.into_shared();
        let recheck_daemon = FakeDaemonClient::new();
        for ki in &key_images_of(&second) {
            recheck_daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
            // still genuinely spent
        }
        // `first`'s key images default to Unspent - the accusation being corrected.

        round_with_void_recheck_due(&store, &recheck_daemon)
            .await
            .unwrap();

        let s = store.lock();
        let payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        let (voided, kept): (Vec<_>, Vec<_>) = payments.iter().partition(|p| p.voided_at.is_some());
        assert_eq!(voided.len(), 1, "the still-justified void must remain");
        assert_eq!(voided[0].txid, tx_id_hex(&second));
        assert_eq!(
            kept[0].txid,
            tx_id_hex(&first),
            "the false accusation is reversed"
        );
        assert!(
            s.get_order(&shared::ids::TenantId::new(tenant_id.to_string()), &shared::ids::OrderId::new(order_id.to_string())).unwrap().unwrap().double_spend_detected_at.is_some(),
            "the other voided payment still genuinely justifies the flag - it must not be cleared as a side effect"
        );
    }

    #[tokio::test]
    async fn the_void_recheck_does_nothing_while_the_chain_height_is_unreachable() {
        let (store, _tenant_id, order_id) = setup_with_one_voided_double_spend().await;

        let recheck_daemon = chain_replica();
        recheck_daemon.set_online(false); // every call, including get_height, now fails

        // A node that is down fails the round (it is retried next round);
        // nothing is decided about the void meanwhile.
        assert!(round_with_void_recheck_due(&store, &recheck_daemon)
            .await
            .is_err());
        assert_eq!(
            voided(&store, &order_id),
            vec![true],
            "a recheck that can't run must not touch anything"
        );
    }

    /// Wraps a `FakeDaemonClient`, failing exactly the call index in `fail_on_call`
    /// (0-based, counted across `is_key_image_spent` calls only) and delegating to
    /// `inner` for every other call - for proving a recheck that fails leaves its
    /// payment voided and is retried on the next pass.
    struct DaemonFailingOneKeyImageCall {
        inner: FakeDaemonClient,
        fail_on_call: u64,
        calls: AtomicU64,
    }

    impl DaemonFailingOneKeyImageCall {
        fn new(inner: FakeDaemonClient, fail_on_call: u64) -> Self {
            Self {
                inner,
                fail_on_call,
                calls: AtomicU64::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl MoneroDaemonClient for DaemonFailingOneKeyImageCall {
        async fn get_height(&self) -> std::result::Result<u64, DaemonError> {
            self.inner.get_height().await
        }
        async fn get_block_hash(&self, height: u64) -> std::result::Result<String, DaemonError> {
            self.inner.get_block_hash(height).await
        }
        async fn get_chain_blocks(
            &self,
            start_height: u64,
            count: u64,
        ) -> std::result::Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
            self.inner.get_chain_blocks(start_height, count).await
        }
        async fn get_mempool_txids(&self) -> std::result::Result<Vec<String>, DaemonError> {
            self.inner.get_mempool_txids().await
        }
        async fn get_transactions_with_ids(
            &self,
            txids: &[String],
        ) -> std::result::Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
            self.inner.get_transactions_with_ids(txids).await
        }
        async fn locate_transaction(
            &self,
            txid: &str,
        ) -> std::result::Result<TxLocation, DaemonError> {
            self.inner.locate_transaction(txid).await
        }
        async fn is_key_image_spent(
            &self,
            key_images: &[String],
        ) -> std::result::Result<Vec<KeyImageStatus>, DaemonError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == self.fail_on_call {
                return Err(DaemonError::Request(format!(
                    "simulated key-image lookup failure on call {n}"
                )));
            }
            self.inner.is_key_image_spent(key_images).await
        }
    }

    #[tokio::test]
    async fn a_failed_recheck_leaves_its_payment_voided_and_the_next_pass_retries_it() {
        let (_guard, logs) = crate::test_log::capture();
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let first = fixture_tx();
        let second = independent_payment_tx(5);
        let now = crate::now_unix();
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &first,
            0..3,
            now,
            Some(50),
        )
        .await
        .unwrap();
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &second,
            0..3,
            now,
            Some(50),
        )
        .await
        .unwrap();
        for payment in store
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
        {
            store
                .void_payment(
                    &shared::ids::OrderId::new(order_id.to_string()),
                    &payment.txid,
                    payment.output_index,
                    now,
                )
                .unwrap();
        }
        store
            .mark_double_spend_detected(&shared::ids::OrderId::new(order_id.to_string()), now)
            .unwrap();

        let store = store.into_shared();
        // Both payments' key images default to Unspent (would clear both if asked),
        // but the very first recheck call fails: a node that fails for one payment
        // would for the next, so the pass stops there, leaving both voided and
        // nothing half-done.
        let recheck_daemon = DaemonFailingOneKeyImageCall::new(FakeDaemonClient::new(), 0);
        round_with_void_recheck_due(&store, &recheck_daemon)
            .await
            .unwrap();
        assert_eq!(
            voided(&store, &order_id),
            vec![true, true],
            "the failed recheck restored nothing"
        );
        assert_eq!(
            logs.count("rechecking a voided payment failed"),
            1,
            "{}",
            logs.text()
        );

        // The next pass retries from the start and reverses both.
        round_with_void_recheck_due(&store, &recheck_daemon)
            .await
            .unwrap();
        assert_eq!(voided(&store, &order_id), vec![false, false]);
    }

    #[tokio::test]
    async fn a_fallback_daemon_that_disagrees_with_the_primary_prevents_the_wrongful_void_a_single_lying_node_would_cause(
    ) {
        // The prevention half of the fix for `is_key_image_spent`'s single-node trust
        // boundary (docs/DESIGN.md §7.7): the exact same attack shape as
        // `a_zero_conf_order_double_spent_out_of_the_mempool_is_voided_with_no_reorg_involved`
        // above, except the *accusation itself* is false - only one of two configured
        // nodes claims the key image is spent in the blockchain. Routed through a
        // real `FallbackDaemonClient` (not a bare `FakeDaemonClient`), this must NOT
        // void the payment - a single node's say-so is no longer enough once a second
        // one is configured to disagree with it.
        let (store, key_custody, handle, tenant_id, order_id) =
            setup_with_confirmations_override(Some(0)).await;
        store
            .create_webhook(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();
        let tx = fixture_tx();
        let store = store.into_shared();

        // Two nodes, kept in identical lockstep for every normal scan concern (same
        // blocks, same mempool) - the only difference between them is the key-image
        // answer, which is the one thing this test is isolating.
        let primary = FakeDaemonClient::new();
        let fallback = FakeDaemonClient::new();
        for daemon in [&primary, &fallback] {
            daemon.push_block("h1", vec![]);
            daemon.set_mempool(vec![tx.clone()]);
        }
        let client = FallbackDaemonClient::new(vec![
            FallbackNode {
                label: "primary".to_string(),
                client: std::sync::Arc::new(primary),
            },
            FallbackNode {
                label: "fallback".to_string(),
                client: std::sync::Arc::new(fallback),
            },
        ]);

        run_scan_tick(
            &store,
            &key_custody,
            &client,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store.lock().get_order(&shared::ids::TenantId::new(tenant_id.to_string()), &shared::ids::OrderId::new(order_id.to_string())).unwrap().unwrap().status,
            crate::status::OrderStatus::Overpaid,
            "settled off the mempool sighting alone, same as the bare-daemon version of this scenario"
        );

        // Re-fetch the two nodes back out of `client` is not possible (they were
        // moved in) - rebuild the same scenario's second act with two fresh, still
        // block/mempool-matched `FakeDaemonClient`s, one of which now (falsely)
        // claims the payment's key image was spent elsewhere.
        let primary = FakeDaemonClient::new();
        let fallback = FakeDaemonClient::new();
        for daemon in [&primary, &fallback] {
            daemon.push_block("h1", vec![]);
            // The transaction has vanished from both nodes' mempools, same as the
            // honest half of the real attack - nothing here tips off a reorg.
        }
        for ki in &key_images_of(&tx) {
            primary.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain); // the lie
            fallback.set_key_image_status(ki, KeyImageStatus::Unspent); // the truth
        }
        let client = FallbackDaemonClient::new(vec![
            FallbackNode {
                label: "primary".to_string(),
                client: std::sync::Arc::new(primary),
            },
            FallbackNode {
                label: "fallback".to_string(),
                client: std::sync::Arc::new(fallback),
            },
        ]);

        run_scan_tick(
            &store,
            &key_custody,
            &client,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        let payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(payments.len(), 1);
        assert!(
            payments[0].voided_at.is_none(),
            "one node's false accusation must not void the payment once a second, disagreeing node is configured"
        );
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert!(
            order.double_spend_detected_at.is_none(),
            "no incident occurred - nothing should be stamped"
        );
        let events: Vec<String> = s
            .due_webhook_deliveries_for_test(crate::now_unix() + 1, 10)
            .unwrap()
            .into_iter()
            .map(|d| d.event_type)
            .collect();
        assert!(
            !events.contains(&"order.double_spend_detected".to_string()),
            "no false double-spend webhook should ever be sent to the merchant: {events:?}"
        );
    }

    #[tokio::test]
    async fn a_fallback_daemon_still_voids_a_real_double_spend_every_node_agrees_on() {
        // The regression this fix must not cause: requiring corroboration must not
        // make genuine double-spend detection any less reliable when every
        // configured node honestly agrees, which is the overwhelmingly common case
        // even with a fallback configured.
        let (store, key_custody, handle, tenant_id, order_id) =
            setup_with_confirmations_override(Some(0)).await;
        let tx = fixture_tx();
        let store = store.into_shared();

        let primary = FakeDaemonClient::new();
        let fallback = FakeDaemonClient::new();
        for daemon in [&primary, &fallback] {
            daemon.push_block("h1", vec![]);
            daemon.set_mempool(vec![tx.clone()]);
        }
        let client = FallbackDaemonClient::new(vec![
            FallbackNode {
                label: "primary".to_string(),
                client: std::sync::Arc::new(primary),
            },
            FallbackNode {
                label: "fallback".to_string(),
                client: std::sync::Arc::new(fallback),
            },
        ]);
        run_scan_tick(
            &store,
            &key_custody,
            &client,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let primary = FakeDaemonClient::new();
        let fallback = FakeDaemonClient::new();
        for daemon in [&primary, &fallback] {
            daemon.push_block("h1", vec![]);
        }
        for ki in &key_images_of(&tx) {
            primary.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
            fallback.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        let client = FallbackDaemonClient::new(vec![
            FallbackNode {
                label: "primary".to_string(),
                client: std::sync::Arc::new(primary),
            },
            FallbackNode {
                label: "fallback".to_string(),
                client: std::sync::Arc::new(fallback),
            },
        ]);
        run_scan_tick(
            &store,
            &key_custody,
            &client,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        assert!(
            s.get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()[0]
                .voided_at
                .is_some(),
            "a genuine, unanimously-corroborated double-spend must still be voided"
        );
    }

    #[tokio::test]
    async fn a_mempool_payment_that_merely_disappears_is_never_voided_on_that_evidence_alone() {
        // The honest half of the same story, and the reason the sweep cannot simply
        // treat "gone from the pool" as "gone". Monero has no replace-by-fee, but a
        // transaction can still leave a pool without being mined - it can expire out
        // after `CRYPTONOTE_MEMPOOL_TX_LIVETIME` (three days), be dropped by a node
        // under pressure, or never have propagated past the node that first saw it.
        // The customer's money may be perfectly fine and the transaction may be
        // remined or rebroadcast at any point, so absence proves nothing and must
        // never void: the same evidence rule reorg reconciliation follows.
        let (store, key_custody, handle, tenant_id, order_id) =
            setup_with_confirmations_override(Some(0)).await;
        let tx = fixture_tx();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.set_mempool(vec![tx.clone()]);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        daemon.drop_from_mempool(&tx); // vanished, with nothing proven about its inputs
        daemon.push_block("h2", vec![]);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        {
            let s = store.lock();
            let payments = s
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap();
            assert!(
                payments[0].voided_at.is_none(),
                "an evicted or still-propagating transaction is not a double-spend"
            );
            assert_eq!(
                payments[0].block_height, None,
                "and it is left exactly as it was, re-checkable next tick"
            );
            assert!(s
                .get_order(
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &shared::ids::OrderId::new(order_id.to_string())
                )
                .unwrap()
                .unwrap()
                .double_spend_detected_at
                .is_none());
        }

        // And when it does come back and get mined, it is picked up as normal.
        daemon.push_block("h3", vec![tx.clone()]);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        let s = store.lock();
        assert_eq!(
            s.get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()[0]
                .block_height,
            Some(3)
        );
    }

    #[tokio::test]
    async fn the_vanished_payment_sweep_costs_nothing_while_a_transaction_is_where_it_should_be() {
        // The sweep runs on every tick of every network, so "cheap by construction"
        // has to be a property, not an aspiration: a payment still sitting in the
        // pool is answered by the snapshot the tick already fetched, and one mined
        // this tick has had its height written by the block scan before the sweep
        // looks. Neither costs an RPC. Only a genuinely vanished transaction does.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let store = store.into_shared();

        let inner = FakeDaemonClient::new();
        inner.push_block("h1", vec![]);
        inner.set_mempool(vec![tx.clone()]);
        let daemon = DaemonFailingFrom::counting(inner, DaemonCall::Locate);

        for _ in 0..3 {
            run_scan_tick(
                &store,
                &key_custody,
                &daemon,
                "mainnet",
                &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
                20,
                0,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            daemon.call_count(),
            0,
            "a transaction still in the pool must never be looked up"
        );

        // Mined, and out of the pool in the same tick - the ordinary lifecycle.
        daemon.inner.drop_from_mempool(&tx);
        daemon.inner.push_block("h2", vec![tx.clone()]);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()[0]
                .block_height,
            Some(2)
        );
        assert_eq!(
            daemon.call_count(),
            0,
            "the block scan anchors the payment before the sweep runs, so the ordinary path stays free"
        );

        // And it stays free once the payment is confirmed, tick after tick.
        for _ in 0..3 {
            run_scan_tick(
                &store,
                &key_custody,
                &daemon,
                "mainnet",
                &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
                20,
                0,
            )
            .await
            .unwrap();
        }
        assert_eq!(daemon.call_count(), 0);
    }

    #[tokio::test]
    async fn a_failed_mempool_poll_skips_the_vanished_payment_sweep_rather_than_assuming_an_empty_pool(
    ) {
        // The sweep's one dangerous input is the mempool snapshot: an empty set means
        // "every unconfirmed payment has vanished". A *failed* poll must therefore
        // skip it entirely rather than pass the empty set it has - not for
        // correctness of the void decision (the `locate_transaction` that follows
        // would still answer `InPool` and conclude nothing), but because it would
        // otherwise burn one RPC per pending payment, on every tick, for exactly as
        // long as the node is having trouble.
        let (store, key_custody, handle, tenant_id, _order_id) = setup().await;
        let store = store.into_shared();

        let tx = fixture_tx();
        let fake = FakeDaemonClient::new();
        fake.push_block("h1", vec![]);
        fake.set_mempool(vec![tx.clone()]);
        // First tick polls the pool successfully and records the payment; from the
        // second call onwards the poll fails. The outer wrapper counts the lookups
        // the sweep would make.
        let daemon = DaemonFailingFrom::counting(
            DaemonFailingFrom::failing_from(fake, DaemonCall::Mempool, 1),
            DaemonCall::Locate,
        );
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(_order_id.to_string()))
                .unwrap()
                .len(),
            1
        );

        let result = run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await;
        assert!(
            result.is_ok(),
            "a failed mempool poll must not fail the tick - the rest of it still has work to do"
        );
        assert_eq!(
            daemon.call_count(),
            0,
            "with no usable snapshot, the sweep must not run at all"
        );

        // The node recovers, and the transaction really is gone from the pool this
        // time - now the sweep does run, which is what makes the assertion above a
        // statement about the failed poll rather than about there being no work.
        daemon.inner.stop_failing();
        daemon.inner.inner.drop_from_mempool(&tx);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            daemon.call_count(),
            1,
            "one lookup for the one payment that has genuinely vanished"
        );
    }

    #[tokio::test]
    async fn a_node_failing_the_fetch_of_a_scan_chunk_retries_the_whole_chunk_next_tick() {
        // A tick is not atomic - it is a sequence of independent RPCs - so "the node
        // went away mid-tick" has as many shapes as there are calls in it. The
        // block-fetching path reads blocks in `get_chain_blocks` chunks
        // (`docs/txid_lookup_and_scan_chunking_wbs.md` Part A): one
        // `get_blocks.bin` call answers for a run of blocks or for none of them,
        // there is no partial response. So a failed fetch abandons the whole
        // chunk: the high-water mark doesn't move past wherever the chunk started
        // until a fetch succeeds, and the next tick retries the identical range -
        // no payment lost, none recorded twice.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        store
            .set_scanned_block(monero::Network::Mainnet, 1, "h1")
            .unwrap();
        let store = store.into_shared();

        let fake = FakeDaemonClient::new();
        fake.push_block("h1", vec![]);
        fake.push_block("h2", vec![]);
        fake.push_block("h3", vec![fixture_tx()]); // the payment is in the block that fails
        fake.push_block("h4", vec![]);
        // The whole 2..=4 range fits in one chunk (well under `SCAN_CHUNK_MAX_
        // BLOCKS`): the daemon fails while fetching the chunk that covers the
        // whole remaining range, before any of it is recorded.
        let daemon = DaemonFailingFrom::failing_from(fake, DaemonCall::ChainBlocks, 0);

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        {
            let s = store.lock();
            assert_eq!(
                s.max_scanned_height(monero::Network::Mainnet).unwrap(),
                Some(1),
                "the whole chunk failed, so the high-water mark stays exactly where it was before this tick"
            );
            assert!(s
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()
                .is_empty());
        }

        daemon.stop_failing();
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        assert_eq!(
            s.max_scanned_height(monero::Network::Mainnet).unwrap(),
            Some(4)
        );
        let payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(
            payments.len(),
            1,
            "exactly one payment - not lost by the failure, not duplicated by the retry"
        );
        assert_eq!(payments[0].block_height, Some(3));
    }

    #[tokio::test]
    async fn run_scan_tick_batches_a_wide_catchup_range_into_far_fewer_daemon_calls_than_blocks() {
        // The real-world case this whole mechanism exists for
        // (`docs/txid_lookup_and_scan_chunking_wbs.md` Part A): a process that was
        // down for a while faces a scan range spanning many blocks on its very next
        // tick. Before batching, that was one fetch per
        // block; now it should be a small number of `get_chain_blocks` calls
        // regardless of how wide the range is, as long as the blocks are small
        // enough to fit many per chunk under the default memory budget.
        //
        // Two separate runs, one per call counted: each half is a real,
        // independent claim.
        // A pre-existing high-water mark (height 1, same idiom the mid-chunk-
        // failure test above uses) is essential, not incidental: without it,
        // `run_scan_tick`'s own first-run bootstrap (`max_scanned_height` is
        // `None`) seeds one block behind the current tip and scans forward from
        // there - a genuinely fresh scanner never replays history - which would
        // collapse this "many blocks piled up" scenario down to a single-block
        // scan regardless of how many blocks were pushed, proving nothing.
        // `NEW_BLOCK_COUNT` blocks then arrive on top of that baseline (heights
        // 2..=NEW_BLOCK_COUNT+1) - the actual range this tick has to catch up on.
        const NEW_BLOCK_COUNT: u64 = 40;

        {
            let (store, key_custody, handle, tenant_id, _order_id) = setup().await;
            store
                .set_scanned_block(monero::Network::Mainnet, 1, "h1")
                .unwrap();
            let store = store.into_shared();
            let fake = FakeDaemonClient::new();
            fake.push_block("h1", vec![]); // the already-scanned baseline, mirrored into the daemon too
            for i in 0..NEW_BLOCK_COUNT {
                fake.push_block(&format!("h{}", i + 2), vec![]); // small, empty blocks - cheap to batch heavily
            }
            let daemon = DaemonFailingFrom::counting(fake, DaemonCall::ChainBlocks);

            for _ in 0..5 {
                run_scan_tick(
                    &store,
                    &key_custody,
                    &daemon,
                    "mainnet",
                    &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
                    20,
                    0,
                )
                .await
                .unwrap();
                if store
                    .lock()
                    .max_scanned_height(monero::Network::Mainnet)
                    .unwrap()
                    == Some(NEW_BLOCK_COUNT + 1)
                {
                    break;
                }
            }

            assert_eq!(
                store
                    .lock()
                    .max_scanned_height(monero::Network::Mainnet)
                    .unwrap(),
                Some(NEW_BLOCK_COUNT + 1),
                "the wide range must be fully scanned across bounded ticks"
            );
            assert!(
                daemon.call_count() < NEW_BLOCK_COUNT,
                "expected far fewer than {NEW_BLOCK_COUNT} get_chain_blocks calls for {NEW_BLOCK_COUNT} small \
                 blocks under the default memory budget, got {}",
                daemon.call_count()
            );
        }

        // Hash lookups (`get_block_hash`) don't grow with the blocks scanned
        // either, asserted here on a fresh scenario.
        {
            let (store, key_custody, handle, tenant_id, _order_id) = setup().await;
            store
                .set_scanned_block(monero::Network::Mainnet, 1, "h1")
                .unwrap();
            let store = store.into_shared();
            let fake = FakeDaemonClient::new();
            fake.push_block("h1", vec![]);
            for i in 0..NEW_BLOCK_COUNT {
                fake.push_block(&format!("h{}", i + 2), vec![]);
            }
            let daemon = DaemonFailingFrom::counting(fake, DaemonCall::BlockHash);

            let mut rounds = 0;
            for _ in 0..5 {
                rounds += 1;
                run_scan_tick(
                    &store,
                    &key_custody,
                    &daemon,
                    "mainnet",
                    &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
                    0,
                    0,
                )
                .await
                .unwrap();
                if store
                    .lock()
                    .max_scanned_height(monero::Network::Mainnet)
                    .unwrap()
                    == Some(NEW_BLOCK_COUNT + 1)
                {
                    break;
                }
            }

            // A block's id comes with its contents (`get_chain_blocks`), so the
            // scan makes no hash lookups of its own: the only ones are reorg
            // detection's, one per round when the chain agrees.
            assert_eq!(
                daemon.call_count(),
                rounds,
                "one detection lookup per round, none per block"
            );
        }
    }

    #[test]
    fn a_larger_average_block_asks_for_fewer_blocks() {
        let cap = 8 * 1024 * 1024;
        let small = next_scan_chunk(cap, None, 1_000.0, u64::MAX);
        let medium = next_scan_chunk(cap, None, 100_000.0, u64::MAX);
        let large = next_scan_chunk(cap, None, 10_000_000.0, u64::MAX);
        assert_eq!(
            small,
            ChunkPlan {
                blocks: SCAN_CHUNK_MAX_BLOCKS,
                limited_by: ChunkLimit::Maximum
            }
        );
        assert_eq!(
            medium,
            ChunkPlan {
                blocks: 83,
                limited_by: ChunkLimit::Memory
            }
        );
        assert_eq!(
            large,
            ChunkPlan {
                blocks: 1,
                limited_by: ChunkLimit::Memory
            },
            "never fewer than one"
        );
    }

    #[test]
    fn a_slow_link_asks_for_what_it_delivers_in_a_target_call() {
        let cap = 8 * 1024 * 1024;
        // 250 kB/s for 4 s at 50 kB a block: 20 blocks, where the cap
        // would allow 167.
        assert_eq!(
            next_scan_chunk(cap, Some(250_000.0), 50_000.0, u64::MAX),
            ChunkPlan {
                blocks: 20,
                limited_by: ChunkLimit::Link
            }
        );
        // A fast link doesn't lift the cap.
        assert_eq!(
            next_scan_chunk(cap, Some(1e9), 50_000.0, u64::MAX),
            ChunkPlan {
                blocks: 167,
                limited_by: ChunkLimit::Memory
            }
        );
        // A link too slow for one block in a target call still gets one.
        assert_eq!(
            next_scan_chunk(cap, Some(1_000.0), 50_000.0, u64::MAX).blocks,
            1
        );
    }

    #[test]
    fn a_chunk_stops_at_what_remains() {
        assert_eq!(
            next_scan_chunk(8 << 20, None, 1.0, 3),
            ChunkPlan {
                blocks: 3,
                limited_by: ChunkLimit::Remaining
            }
        );
        assert_eq!(next_scan_chunk(8 << 20, None, f64::MAX, u64::MAX).blocks, 1);
    }

    #[test]
    fn a_failed_fetch_halves_the_next_one() {
        let cap = 8 * 1024 * 1024;
        let mut avg = 50_000.0;
        let mut sizes = Vec::new();
        for _ in 0..10 {
            sizes.push(next_scan_chunk(cap, None, avg, u64::MAX).blocks);
            avg = avg_after_failed_fetch(avg);
        }
        assert_eq!(sizes, [167, 83, 41, 20, 10, 5, 2, 1, 1, 1]);
    }

    #[test]
    fn the_response_cap_is_an_eighth_of_the_budget_and_at_least_256_kb() {
        assert_eq!(response_cap_bytes(8), 1024 * 1024);
        assert_eq!(response_cap_bytes(1), 256 * 1024);
        assert_eq!(response_cap_bytes(4096), 512 * 1024 * 1024);
    }

    #[test]
    fn update_avg_bytes_per_block_weighs_recent_data_by_the_configured_alpha() {
        let after_one_big_chunk =
            update_avg_bytes_per_block(SCAN_CHUNK_INITIAL_AVG_BYTES, 1_000_000, 1);
        assert!(
            after_one_big_chunk > SCAN_CHUNK_INITIAL_AVG_BYTES,
            "a chunk far bigger than the cold-start guess must pull the average up, not leave it unchanged"
        );
        // A single all-zero (empty-block) chunk should pull the average down
        // but - by design, `SCAN_CHUNK_EWMA_ALPHA < 1.0` - not all the way to
        // zero in one step; a lone anomalous chunk shouldn't swing the very
        // next chunk's size wildly.
        let after_one_empty_chunk = update_avg_bytes_per_block(SCAN_CHUNK_INITIAL_AVG_BYTES, 0, 10);
        assert!(
            after_one_empty_chunk > 0.0 && after_one_empty_chunk < SCAN_CHUNK_INITIAL_AVG_BYTES
        );
    }

    // The paused clock lets the daemon's 20 ms "timeout" resolve without
    // real waiting.
    #[tokio::test(start_paused = true)]
    async fn a_request_that_times_out_rather_than_failing_fast_is_still_just_a_failed_tick() {
        // At this boundary a timeout and a refused connection are the same event -
        // `RpcDaemonClient` gives every request a 15-second client timeout and
        // surfaces whatever comes back as a `DaemonError` either way - but "the tick
        // that eventually errors leaves the same state as the tick that errors
        // immediately" is exactly the sort of thing that is true by construction
        // right up until some future retry or partial-progress logic makes it not.
        // The failure here resolves late (after an actual `.await` that yields)
        // rather than synchronously.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let store = store.into_shared();

        let fake = FakeDaemonClient::new();
        fake.push_block("h1", vec![]);
        fake.push_block("h2", vec![fixture_tx()]);
        let daemon = DaemonFailingFrom::timing_out_from(fake, DaemonCall::Height, 0, 20);

        let result = run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await;
        assert!(
            result.is_err(),
            "a tick that cannot learn the chain height has nothing to say about any order"
        );
        {
            let s = store.lock();
            assert_eq!(
                s.max_scanned_height(monero::Network::Mainnet).unwrap(),
                None,
                "nothing may be recorded as scanned"
            );
            assert_eq!(
                s.get_order(
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &shared::ids::OrderId::new(order_id.to_string())
                )
                .unwrap()
                .unwrap()
                .status,
                crate::status::OrderStatus::Pending,
                "and no order may have advanced on evidence the tick never got"
            );
        }

        daemon.stop_failing();
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        let s = store.lock();
        assert_eq!(
            s.get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()
                .len(),
            1,
            "the next tick recovers the payment exactly once"
        );
        assert_eq!(
            s.get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string())
            )
            .unwrap()
            .unwrap()
            .status,
            crate::status::OrderStatus::Confirming
        );
    }

    #[tokio::test]
    async fn a_key_image_lookup_failing_mid_reconciliation_leaves_the_reorg_detectable() {
        // The sibling of `a_reconciliation_that_fails_partway_leaves_the_reorg_still_detectable`,
        // one RPC further in: the divergence is found, the vanished transaction is
        // located (nowhere), and the node dies on the one call that would decide
        // whether that means "double-spent" or "still propagating". The stored hashes
        // must still describe the losing chain afterwards, or nothing will ever
        // re-derive that a reorg happened and the payment stays stranded.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let fake = chain_scanned_to(&store, 50, 40, "old");
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        for ki in &key_images_of(&tx) {
            fake.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        reorg_to_new_chain(&fake, 50, 51, None);
        let store = store.into_shared();
        let daemon = DaemonFailingFrom::failing_from(fake, DaemonCall::KeyImageSpent, 0);

        assert!(
            check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
                .await
                .is_err(),
            "the node failure must surface rather than be read as 'no proof of a double-spend'"
        );
        {
            let s = store.lock();
            assert_eq!(
                s.get_scanned_block_hash(monero::Network::Mainnet, 50)
                    .unwrap()
                    .as_deref(),
                Some("old_50")
            );
            assert!(
                s.get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                    .unwrap()[0]
                    .voided_at
                    .is_none(),
                "nothing may be voided on no evidence"
            );
        }

        daemon.stop_failing();
        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2100)
            .await
            .unwrap();
        assert_eq!(report.reorg_detected_at, Some(50));
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);
        assert!(store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()[0]
            .voided_at
            .is_some());
    }

    #[tokio::test]
    async fn swapping_to_a_daemon_serving_a_different_chain_reconciles_exactly_like_a_reorg() {
        // The concrete answer to "do we need to record where we synced up to *per
        // daemon*, so it can be redone if a daemon turns out to be dishonest?".
        //
        // This system configures exactly one daemon per network (`main.rs` builds a
        // `HashMap<Network, Arc<dyn MoneroDaemonClient>>`), so there is no pool of
        // daemon identities to key sync state by. What there *is* is a record of
        // which `(height, hash)` pairs this scanner has accepted - and that record is
        // checked against whatever daemon is answering *now*, with no notion of which
        // daemon supplied it originally. So pointing the scanner at a different node
        // serving a different history is not a case the reorg machinery fails to
        // cover: it is bit-for-bit the same case, and the same code re-derives,
        // re-locates and rescans exactly as it would for an honest reorg. That is the
        // property this test pins down - the same store, two entirely separate
        // daemon instances, no notification that anything changed.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();

        let honest = FakeDaemonClient::new();
        for h in 1..=49 {
            honest.push_block(&format!("a_{h}"), vec![]);
        }
        honest.push_block("a_50", vec![tx.clone()]);
        honest.push_block("a_51", vec![]);
        let store = store.into_shared();
        store
            .lock()
            .set_scanned_block(monero::Network::Mainnet, 49, "a_49")
            .unwrap();
        run_scan_tick(
            &store,
            &key_custody,
            &honest,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()[0]
                .block_height,
            Some(50)
        );

        // A different node entirely: same network, chain diverging at 48, and the
        // payment nowhere in it. Nothing tells the scanner the daemon changed.
        let other = FakeDaemonClient::new();
        for h in 1..=47 {
            other.push_block(&format!("a_{h}"), vec![]);
        }
        for h in 48..=55 {
            other.push_block(&format!("b_{h}"), vec![]);
        }

        run_scan_tick(
            &store,
            &key_custody,
            &other,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        {
            let s = store.lock();
            assert_eq!(
                s.max_scanned_height(monero::Network::Mainnet).unwrap(),
                Some(48),
                "the divergence is found and rewound, exactly as for a reorg: the first recorded height that \
                 differs is 49, so the scan restarts from 48, one below it - detection can only ever be as \
                 fine-grained as the window of hashes it kept"
            );
            let payment = &s
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()[0];
            assert!(
                payment.voided_at.is_none(),
                "the new node not having the transaction proves nothing about it - never void on that"
            );
        }

        // ...and the scanner then works forward over the new node's chain.
        run_scan_tick(
            &store,
            &key_custody,
            &other,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(55)
        );

        // Swapping back is symmetric: the *first* node is now the one presenting a
        // divergent history, and gets reconciled the same way.
        run_scan_tick(
            &store,
            &key_custody,
            &honest,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store.lock().max_scanned_height(monero::Network::Mainnet).unwrap(),
            Some(47),
            "no daemon is privileged - the stored chain is re-validated against whoever is answering"
        );
    }

    #[tokio::test]
    async fn failing_over_through_a_real_fallback_client_to_a_node_serving_a_different_chain_reconciles_like_a_reorg(
    ) {
        // The composed version of `swapping_to_a_daemon_serving_a_different_chain_
        // reconciles_exactly_like_a_reorg` above: that test proves the *scanner*
        // doesn't care which daemon answers, by handing it two raw `FakeDaemonClient`s
        // directly. It never exercises `daemon_fallback::FallbackDaemonClient` itself -
        // the actual code path production runs, which decides *when* to move to a
        // different node in the first place. This test proves the two compose
        // correctly: a real failover, triggered by the primary going unreachable
        // (not swapped by the test), landing on a fallback with a genuinely different,
        // divergent chain, still reconciles exactly like an ordinary reorg.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();

        let primary = std::sync::Arc::new(FakeDaemonClient::new());
        for h in 1..=49 {
            primary.push_block(&format!("a_{h}"), vec![]);
        }
        primary.push_block("a_50", vec![tx.clone()]);
        let store = store.into_shared();
        store
            .lock()
            .set_scanned_block(monero::Network::Mainnet, 49, "a_49")
            .unwrap();

        let fallback = std::sync::Arc::new(FakeDaemonClient::new());
        for h in 1..=47 {
            fallback.push_block(&format!("a_{h}"), vec![]);
        }
        for h in 48..=52 {
            fallback.push_block(&format!("b_{h}"), vec![]);
        }

        let client = FallbackDaemonClient::new(vec![
            FallbackNode {
                label: "primary".to_string(),
                client: primary.clone(),
            },
            FallbackNode {
                label: "fallback".to_string(),
                client: fallback.clone(),
            },
        ]);

        run_scan_tick(
            &store,
            &key_custody,
            &client,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()[0]
                .block_height,
            Some(50),
            "the primary is healthy and used first, exactly like a bare RpcDaemonClient would be"
        );

        // The primary goes unreachable - nothing tells `FallbackDaemonClient` to swap,
        // it discovers this itself on the next call and moves to the fallback, which
        // happens to disagree with recorded history from height 48 on.
        primary.set_online(false);
        run_scan_tick(
            &store,
            &key_custody,
            &client,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        {
            let s = store.lock();
            assert_eq!(
                s.max_scanned_height(monero::Network::Mainnet).unwrap(),
                Some(48),
                "failover to a genuinely diverging fallback reconciles exactly like the raw-swap test above"
            );
            assert!(
                s.get_all_payments(&shared::ids::OrderId::new(order_id.to_string())).unwrap()[0].voided_at.is_none(),
                "the fallback not having the transaction proves nothing about it - never void on that"
            );
        }

        run_scan_tick(
            &store,
            &key_custody,
            &client,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(52),
            "scanning continues forward on the fallback's own chain"
        );
    }

    #[tokio::test]
    async fn failing_over_to_a_lagging_but_honest_fallback_neither_rewinds_nor_corrupts_the_window()
    {
        // The much more likely real-world case than an actively diverging fallback:
        // a self-hoster's backup node is simply a bit behind (still catching up after
        // a restart, or just slower to relay), reporting the *same* history so far,
        // just less of it. `a_daemon_far_behind_the_recorded_high_water_mark_neither_
        // rescans_nor_discards_its_window` already proves the scanner's own handling
        // of this generically; this composes it with a real failover decision instead
        // of a hand-swapped daemon, since that's what production actually does.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();

        let primary = std::sync::Arc::new(FakeDaemonClient::new());
        for h in 1..=59 {
            primary.push_block(&format!("a_{h}"), vec![]);
        }
        primary.push_block("a_60", vec![tx.clone()]);
        let store = store.into_shared();
        store
            .lock()
            .set_scanned_block(monero::Network::Mainnet, 59, "a_59")
            .unwrap();

        run_scan_tick(
            &store,
            &key_custody,
            primary.as_ref(),
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(60)
        );

        // A fallback with the identical history, just not caught up yet.
        let fallback = std::sync::Arc::new(FakeDaemonClient::new());
        for h in 1..=40 {
            fallback.push_block(&format!("a_{h}"), vec![]);
        }

        let client = FallbackDaemonClient::new(vec![
            FallbackNode {
                label: "primary".to_string(),
                client: primary.clone(),
            },
            FallbackNode {
                label: "fallback".to_string(),
                client: fallback.clone(),
            },
        ]);

        primary.set_online(false);
        run_scan_tick(
            &store,
            &key_custody,
            &client,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        {
            let s = store.lock();
            assert_eq!(
                s.max_scanned_height(monero::Network::Mainnet).unwrap(),
                Some(60),
                "a lagging fallback must not rewind the window back to where it currently is"
            );
            assert!(
                s.get_all_payments(&shared::ids::OrderId::new(order_id.to_string())).unwrap()[0].voided_at.is_none(),
                "a lagging fallback not yet having the transaction proves nothing - never void on that"
            );
        }
    }

    #[tokio::test]
    async fn a_node_that_dies_as_a_block_is_read_has_its_hash_and_contents_come_from_the_same_fallback(
    ) {
        // `FallbackDaemonClient` fails over per call, and a block's id and its
        // transactions come in one call (`get_chain_blocks`): whichever node
        // answers it answers for both. So a primary that can't serve a block
        // never has its own contents paired with the fallback's hash for the
        // same height (the gap `docs/DESIGN.md` §7.7 used to accept, when the
        // two were separate calls).
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();

        let primary_inner = FakeDaemonClient::new();
        for h in 1..=50 {
            primary_inner.push_block(&format!("a_{h}"), vec![]);
        }
        // Height 51 exists only on the primary, and only the primary ever has this
        // transaction - the fallback's own block 51 is a different block entirely.
        primary_inner.push_block("a_51", vec![tx.clone()]);
        let store = store.into_shared();
        store
            .lock()
            .set_scanned_block(monero::Network::Mainnet, 50, "a_50")
            .unwrap();

        let fallback = std::sync::Arc::new(FakeDaemonClient::new());
        // The fallback shares block 50 (so its 51 extends the recorded chain)
        // and has its own, different 51.
        fallback.seed_block_at(50, "a_50", vec![]);
        fallback.seed_block_at(51, "b_51", vec![]);

        // The primary has block 51 (with the payment) and fails to serve it:
        // that call fails over to the fallback.
        let primary = std::sync::Arc::new(DaemonFailingBlockHashAt {
            inner: primary_inner,
            failing_height: AtomicU64::new(51),
        });
        let client = FallbackDaemonClient::new(vec![
            FallbackNode {
                label: "primary".to_string(),
                client: primary,
            },
            FallbackNode {
                label: "fallback".to_string(),
                client: fallback.clone(),
            },
        ]);

        run_scan_tick(
            &store,
            &key_custody,
            &client,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        assert_eq!(
            s.get_scanned_block_hash(monero::Network::Mainnet, 51)
                .unwrap(),
            Some("b_51".to_string()),
            "the recorded hash for height 51 came from the fallback, which served the block"
        );
        assert!(
            s.get_all_payments(&shared::ids::OrderId::new(order_id.to_string())).unwrap().is_empty(),
            "and so did the block's contents: the fallback's block 51 pays nobody, and the primary's block 51 (which \
             it can no longer vouch for) was never paired with the fallback's hash"
        );
    }

    #[tokio::test]
    async fn every_fallback_node_being_down_fails_the_tick_cleanly_without_corrupting_stored_state()
    {
        // Total node loss for a network (every configured node, primary and every
        // fallback, unreachable at once) must surface as an ordinary `Err` for that
        // tick - not a panic, and not a partially-written, inconsistent store state
        // that the next successful tick would have to somehow recover from. `main.rs`
        // already retries via its `supervise` wrapper; this proves there is nothing
        // for it to clean up.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();

        let primary = std::sync::Arc::new(FakeDaemonClient::new());
        primary.push_block("a_1", vec![tx.clone()]);
        let store = store.into_shared();
        run_scan_tick(
            &store,
            &key_custody,
            primary.as_ref(),
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        let payments_before: Vec<(String, Option<i64>)> = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .into_iter()
            .map(|p| (p.txid, p.block_height))
            .collect();
        let scanned_before = store
            .lock()
            .max_scanned_height(monero::Network::Mainnet)
            .unwrap();

        let fallback = std::sync::Arc::new(FakeDaemonClient::new());
        primary.set_online(false);
        fallback.set_online(false);
        let client = FallbackDaemonClient::new(vec![
            FallbackNode {
                label: "primary".to_string(),
                client: primary.clone(),
            },
            FallbackNode {
                label: "fallback".to_string(),
                client: fallback.clone(),
            },
        ]);

        let result = run_scan_tick(
            &store,
            &key_custody,
            &client,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await;
        assert!(
            result.is_err(),
            "every node down must surface as an error, not a silent no-op or a panic"
        );
        let payments_after: Vec<(String, Option<i64>)> = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .into_iter()
            .map(|p| (p.txid, p.block_height))
            .collect();
        assert_eq!(
            payments_after, payments_before,
            "a failed tick must not touch previously-recorded payments"
        );
        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            scanned_before,
            "a failed tick must not move the scanned watermark"
        );
    }

    #[tokio::test]
    async fn a_daemon_that_omits_a_transaction_from_its_mempool_only_delays_detection() {
        // The benign half of "a daemon lies about the pool". An omission cannot
        // falsify anything: the scanner records payments from what it is shown, so
        // being shown less means seeing a payment later (when it is mined), never
        // seeing a wrong one. Worth an executable statement because the *cost* of the
        // omission is a real, bounded thing a merchant may care about - zero-conf
        // detection is what is lost, nothing else.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        // The transaction is genuinely in flight, but this node's pool never shows it.
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        {
            let s = store.lock();
            assert!(
                s.get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                    .unwrap()
                    .is_empty(),
                "nothing to see - only zero-conf is lost"
            );
            assert_eq!(
                s.get_order(
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &shared::ids::OrderId::new(order_id.to_string())
                )
                .unwrap()
                .unwrap()
                .status,
                crate::status::OrderStatus::Pending
            );
        }

        daemon.push_block("h2", vec![tx]);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        let payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(
            payments.len(),
            1,
            "the payment is detected in full the moment it is mined"
        );
        assert_eq!(payments[0].block_height, Some(2));
        assert_eq!(
            s.get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string())
            )
            .unwrap()
            .unwrap()
            .status,
            crate::status::OrderStatus::Confirming
        );
    }

    #[tokio::test]
    async fn a_transaction_a_daemon_invents_cannot_become_a_payment_unless_it_matches_a_wallet() {
        // The other half: a daemon inserting transactions that are not really in the
        // pool (or blocks). This cannot manufacture a payment, and the reason is
        // structural rather than a check anywhere in this file - a payment row exists
        // only where `KeyCustody` matched an output against a tenant's own view key,
        // which a daemon does not have. A phantom transaction that does not match is
        // simply scanned and discarded; one that *does* match would have to be a real
        // transaction really paying that subaddress, which is not a lie.
        let key_custody = PlainKeyCustody::default();
        let store = Store::open_in_memory().unwrap();
        let (view, spend) = arbitrary_wallet_material(0x33); // a tenant this fixture does *not* pay
        let (tenant_id, handle, order_id) =
            tenant_with_pending_order(&store, &key_custody, view, spend).await;
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![fixture_tx()]);
        daemon.set_mempool(vec![fixture_tx_variant(77)]);

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(tenant_id.clone(), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        assert!(
            s.get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()
                .is_empty(),
            "no wallet matched, so no payment exists"
        );
        assert_eq!(
            s.get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string())
            )
            .unwrap()
            .unwrap()
            .status,
            crate::status::OrderStatus::Pending
        );
        assert_eq!(
            s.get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string())
            )
            .unwrap()
            .unwrap()
            .amount_received_piconero,
            0
        );
    }

    #[tokio::test]
    async fn an_inflated_reported_height_inflates_confirmations_which_is_an_accepted_trust_boundary(
    ) {
        // Recorded here as executable documentation of a boundary, not as a bug: the
        // confirmation count is `current_height - block_height + 1` off whatever
        // `get_height` returns, and nothing cross-checks that against the blocks the
        // scanner has actually seen. A daemon claiming a higher tip than exists
        // therefore ages payments faster than the chain does.
        //
        // This is not separately fixable, and the reason is worth stating: the
        // scanner validates no proof of work anywhere (by design - that is monerod's
        // job, §DESIGN.md 7.5), so a node willing to lie about its height can just as
        // cheaply serve a fabricated chain of block hashes to back the lie up.
        // Clamping confirmations to the scanner's own high-water mark would raise the
        // cost of the lie by nothing while making every honest post-reorg rewind
        // briefly under-count confirmations. The boundary is "the configured node is
        // trusted about the chain" - see docs/DESIGN.md §7.7.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await; // confirmations_required = 10
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![fixture_tx()]);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        {
            let s = store.lock();
            let order = s
                .get_order(
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &shared::ids::OrderId::new(order_id.to_string()),
                )
                .unwrap()
                .unwrap();
            assert_eq!(order.status, crate::status::OrderStatus::Confirming);
            assert_eq!(order.confirmations, 1, "one real block, one confirmation");
        }

        daemon.report_height(1_000); // the node asserts a tip it has no blocks for
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            order.confirmations, 999,
            "taken at face value - the documented trust boundary"
        );
        assert_eq!(order.status, crate::status::OrderStatus::Overpaid);
    }

    #[tokio::test]
    async fn a_daemon_far_behind_the_recorded_high_water_mark_neither_rescans_nor_discards_its_window(
    ) {
        // The opposite lie, which is usually not a lie at all: a node reporting a
        // height *below* what this scanner has already recorded. The overwhelmingly
        // common cause is honest - a node resyncing from scratch, or a freshly
        // provisioned one still catching up - and the correct response is to do
        // nothing at all and wait. Specifically it must not "rewind to the tip the
        // daemon has", which for a resyncing mainnet node would discard the entire
        // scanned window and re-seed hundreds of thousands of blocks in the past.
        //
        // Recovery needs no special case: once the node climbs back into the window,
        // any genuine divergence is caught by the ordinary hash comparison (see
        // `swapping_to_a_daemon_serving_a_different_chain_reconciles_exactly_like_a_reorg`).
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(95),
        )
        .await
        .unwrap();
        for h in 70..=100 {
            store
                .set_scanned_block(monero::Network::Mainnet, h, &format!("old_{h}"))
                .unwrap();
        }
        let (_, status) = store
            .recompute_order_status(&shared::ids::OrderId::new(order_id.to_string()), 100, 1600)
            .unwrap();
        assert_eq!(
            status,
            crate::status::OrderStatus::Confirming,
            "six confirmations, ten required"
        );
        let store = store.into_shared();

        // The node is at height 30, three quarters of the way through a resync.
        let daemon = FakeDaemonClient::new();
        for h in 1..=30 {
            daemon.push_block(&format!("old_{h}"), vec![]);
        }

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        assert_eq!(
            s.max_scanned_height(monero::Network::Mainnet).unwrap(),
            Some(100),
            "the window belongs to the chain, not to whichever node is currently answering"
        );
        assert_eq!(
            s.get_scanned_block_hash(monero::Network::Mainnet, 95)
                .unwrap()
                .as_deref(),
            Some("old_95")
        );
        let payment = &s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()[0];
        assert!(
            payment.voided_at.is_none(),
            "a node that hasn't got there yet proves nothing about a payment"
        );
        assert_eq!(payment.block_height, Some(95));
    }

    #[tokio::test]
    async fn several_transactions_in_one_block_paying_one_order_are_all_recorded_and_summed() {
        // A block is not "one transaction for us at most". A customer paying in two
        // instalments that happen to land together, a wallet splitting a payment, and
        // an accidental second payment to the same address all produce this, and the
        // arithmetic has to hold: three rows, three amounts, one total, one status
        // recompute.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let txs = vec![
            fixture_tx(),
            independent_payment_tx(1),
            independent_payment_tx(2),
        ];
        let per_tx_amount = {
            let scan = scan_transaction(&key_custody, handle, &txs[0], 0..3)
                .await
                .unwrap();
            scan.matches[0].amount_piconero.unwrap()
        };
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", txs);

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        let payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(
            payments.len(),
            3,
            "three distinct transactions are three payments, not one deduplicated row"
        );
        assert!(payments.iter().all(|p| p.block_height == Some(2)));
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(order.amount_received_piconero, per_tx_amount * 3);
        assert_eq!(
            order.status,
            crate::status::OrderStatus::Confirming,
            "one confirmation, ten required"
        );
    }

    #[tokio::test]
    async fn one_transactions_outputs_are_routed_to_whichever_order_owns_each_index() {
        // One transaction can pay several of a tenant's subaddresses at once (a
        // customer settling two invoices in one send is the obvious case). Routing is
        // per matched output, by minor index, and this is the step where a subtle
        // mistake would attribute a payment to the wrong customer's order - a
        // failure that shows up as one merchant's order marked paid by another's
        // money.
        //
        // Driven through `record_scan_match` with a constructed match set rather than
        // a real transaction: the fixture pays exactly one index, and manufacturing a
        // second real payment to a *different* index needs transaction construction
        // (which is what the stagenet end-to-end test exists for). The routing logic
        // being tested is entirely on this side of the crypto.
        let (store, _key_custody, _handle, tenant_id, first_order) = setup().await;
        let second_index = store
            .allocate_minor_index(&shared::ids::TenantId::new(tenant_id.to_string()))
            .unwrap();
        assert_eq!(second_index, 2);
        let second_order = store
            .create_order(NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: shared::ids::TenantId::new(tenant_id.clone()),
                merchant_order_id: None,
                minor_index: second_index,
                address: "sub_2".into(),
                xmr_amount_piconero: 1,
                description: None,
                created_at: 1000,
                expires_at: crate::now_unix() + 3600,
            })
            .unwrap();

        let scan = ScanResult {
            matches: vec![
                MatchedOutput {
                    output_index: 0,
                    subaddress_index: SubaddressIndex { major: 0, minor: 1 },
                    amount_piconero: Some(700),
                },
                MatchedOutput {
                    output_index: 1,
                    subaddress_index: SubaddressIndex { major: 0, minor: 2 },
                    amount_piconero: Some(900),
                },
                // An index no order was ever issued for - a tenant's own older
                // subaddress, say. Must be dropped, not attributed to anything.
                MatchedOutput {
                    output_index: 2,
                    subaddress_index: SubaddressIndex { major: 0, minor: 9 },
                    amount_piconero: Some(100),
                },
            ],
            txid: "tx_paying_two_orders".into(),
            key_images_json: "[]".into(),
            output_keys: Default::default(),
        };

        let touched = record_scan_match(
            &store,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &scan,
            1500,
            Some(50),
        )
        .unwrap();
        assert_eq!(
            touched,
            HashSet::from([
                crate::store::OrderId::new(first_order.clone()),
                second_order.id.clone()
            ])
        );

        let first = store
            .get_all_payments(&shared::ids::OrderId::new(first_order.to_string()))
            .unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].amount_piconero, 700,
            "each order gets its own output's amount, never the transaction total"
        );
        let second = store.get_all_payments(&second_order.id).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].amount_piconero, 900);
    }

    #[tokio::test]
    async fn one_tick_can_void_a_double_spent_payment_for_one_order_and_record_a_new_one_for_another(
    ) {
        // Two facts arriving in the same block, on the same tick, for different
        // orders: an older payment is orphaned and proven double-spent, while the
        // replacement chain carries a brand-new payment. Neither may contaminate the
        // other - and the shape also exercises the cross-tenant case migration 0004
        // exists for, since both tenants here are configured with the same view key
        // (a merchant running a second instance against one wallet), so one
        // transaction legitimately pays two different orders.
        let key_custody = PlainKeyCustody::default();
        let store = Store::open_in_memory().unwrap();
        let (tenant_a, handle_a, order_a) = tenant_with_pending_order(
            &store,
            &key_custody,
            fixture_view_key(),
            fixture_spend_pubkey(),
        )
        .await;
        let (tenant_b, handle_b, order_b) = tenant_with_pending_order(
            &store,
            &key_custody,
            fixture_view_key(),
            fixture_spend_pubkey(),
        )
        .await;

        let doomed = fixture_tx();
        let fresh = independent_payment_tx(6);
        let daemon = chain_scanned_to(&store, 51, 40, "old");
        // Only tenant A had a payment before the reorg.
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle_a,
            &shared::ids::TenantId::new(tenant_a.to_string()),
            &doomed,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        for ki in &key_images_of(&doomed) {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        reorg_to_new_chain(&daemon, 50, 51, Some((50, fresh.clone())));
        let store = store.into_shared();

        let tenants = [(tenant_a.clone(), handle_a), (tenant_b.clone(), handle_b)];
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap(); // detects, voids, rewinds
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap(); // rescans the replacement chain

        let s = store.lock();
        let a_payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_a.to_string()))
            .unwrap();
        assert_eq!(
            a_payments.len(),
            2,
            "the voided one and the new one both belong to A's audit trail"
        );
        let voided: Vec<_> = a_payments
            .iter()
            .filter(|p| p.voided_at.is_some())
            .collect();
        assert_eq!(voided.len(), 1);
        assert_eq!(voided[0].txid, tx_id_hex(&doomed));
        let order_a_row = s
            .get_order(
                &shared::ids::TenantId::new(tenant_a.to_string()),
                &shared::ids::OrderId::new(order_a.to_string()),
            )
            .unwrap()
            .unwrap();
        assert!(
            order_a_row.double_spend_detected_at.is_some(),
            "the incident stays recorded even though a later payment covered the order"
        );
        assert_eq!(
            order_a_row.amount_received_piconero,
            a_payments
                .iter()
                .find(|p| p.voided_at.is_none())
                .unwrap()
                .amount_piconero
        );

        let b_payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_b.to_string()))
            .unwrap();
        assert_eq!(
            b_payments.len(),
            1,
            "B gets its own row for the same output - the constraint is per order"
        );
        assert_eq!(b_payments[0].txid, tx_id_hex(&fresh));
        assert!(b_payments[0].voided_at.is_none());
        assert!(
            s.get_order(
                &shared::ids::TenantId::new(tenant_b.to_string()),
                &shared::ids::OrderId::new(order_b.to_string())
            )
            .unwrap()
            .unwrap()
            .double_spend_detected_at
            .is_none(),
            "another order's double-spend is not B's problem"
        );
    }

    #[tokio::test]
    async fn an_amount_that_only_decrypts_on_a_later_tick_is_recorded_then_never_unrecorded() {
        // Amount recovery returning `None` is not necessarily a permanent property of
        // an output - `MatchedOutput::amount_piconero` is an `Option` precisely
        // because the decryption can fail - so the two orderings both need defining.
        // Failing first and succeeding later must record the payment (the "skip, do
        // not write a zero" rule exists to keep that possible); succeeding first and
        // failing later must leave the recorded payment exactly as it is, since a
        // payment is only ever removed on affirmative double-spend proof.
        let (store, _key_custody, _handle, tenant_id, order_id) = setup().await;
        let undecryptable = ScanResult {
            matches: vec![MatchedOutput {
                output_index: 0,
                subaddress_index: SubaddressIndex { major: 0, minor: 1 },
                amount_piconero: None,
            }],
            txid: "tx_flaky_amount".into(),
            key_images_json: "[]".into(),
            output_keys: Default::default(),
        };
        let decrypted = ScanResult {
            matches: vec![MatchedOutput {
                output_index: 0,
                subaddress_index: SubaddressIndex { major: 0, minor: 1 },
                amount_piconero: Some(4_242),
            }],
            txid: "tx_flaky_amount".into(),
            key_images_json: "[]".into(),
            output_keys: Default::default(),
        };

        record_scan_match(
            &store,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &undecryptable,
            1500,
            Some(50),
        )
        .unwrap();
        assert!(store
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .is_empty());

        let touched = record_scan_match(
            &store,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &decrypted,
            1600,
            Some(50),
        )
        .unwrap();
        assert_eq!(
            touched,
            HashSet::from([crate::store::OrderId::new(order_id.clone())]),
            "the later success records it in full"
        );
        assert_eq!(
            store
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()[0]
                .amount_piconero,
            4_242
        );

        record_scan_match(
            &store,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &undecryptable,
            1700,
            Some(50),
        )
        .unwrap();
        let payments = store
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        assert_eq!(payments.len(), 1);
        assert_eq!(
            payments[0].amount_piconero, 4_242,
            "a later failure to decrypt must not disturb a recorded payment"
        );
        assert!(payments[0].voided_at.is_none());
    }

    #[tokio::test]
    async fn a_payment_completing_an_order_in_the_tick_its_deadline_passes_settles_rather_than_expiring(
    ) {
        // Expiry and payment are evaluated in one place, from one snapshot, so their
        // relative ordering within a tick is not a race - but it is worth an
        // executable statement, because "the customer paid at the last second" is
        // both common and the case where getting it wrong means keeping the money and
        // telling the customer their order expired. The status ladder only reaches
        // `expired` when the total falls *short*; a covered order is never expired,
        // however late it was covered.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        store
            .execute_raw_for_test(&format!(
                "UPDATE orders SET expires_at_utc = {} WHERE id = '{order_id}'",
                crate::now_unix() - 1
            ))
            .unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.set_mempool(vec![fixture_tx()]);

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();

        let s = store.lock();
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            order.status,
            crate::status::OrderStatus::Unconfirmed,
            "paid in full, if only just - the deadline governs orders that fall short, not covered ones"
        );
        assert!(order.amount_received_piconero > 0);

        // The other side of the same boundary: a *partial* payment past the deadline
        // does expire, and keeping both cases in one test is what stops a future
        // "fix" for either from quietly inverting the other.
        let partial = s
            .create_order(NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: shared::ids::TenantId::new(tenant_id.clone()),
                merchant_order_id: None,
                minor_index: s
                    .allocate_minor_index(&shared::ids::TenantId::new(tenant_id.to_string()))
                    .unwrap(),
                address: "sub_partial".into(),
                xmr_amount_piconero: 10_000_000,
                description: None,
                created_at: 1000,
                expires_at: crate::now_unix() - 1,
            })
            .unwrap();
        s.record_payment_match(&partial.id, "tx_partial", 0, 1, "[]", 1500, None, None)
            .unwrap();
        let (_, status) = s
            .recompute_order_status(&partial.id, 1, crate::now_unix())
            .unwrap();
        assert_eq!(status, crate::status::OrderStatus::Expired);
    }

    #[tokio::test]
    async fn a_chain_with_no_blocks_at_all_is_a_harmless_no_op_tick() {
        // The degenerate small-chain cases a fresh regtest or a just-initialised
        // private network actually produces: a node reporting height 0 with nothing
        // to serve. Everything here is unsigned arithmetic around a tip
        // (`height - reorg_check_depth`, `current_height - 1`), which is exactly the
        // shape that panics on underflow in debug builds if a `saturating_sub` is
        // ever dropped, and a payment gateway that panics on an empty chain fails
        // its first ever tick against a fresh node.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();

        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        {
            let s = store.lock();
            assert_eq!(
                s.max_scanned_height(monero::Network::Mainnet).unwrap(),
                None,
                "nothing to seed from, so nothing recorded"
            );
            assert!(s
                .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()
                .is_empty());
        }

        // One block exists: the whole chain is height 1, and the tick must still
        // behave (seeding one behind the tip lands on height 0, which does not exist
        // here, so it degrades to "try again next tick" rather than erroring).
        daemon.push_block("only_block", vec![fixture_tx()]);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert!(
            check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_divergence_at_the_genesis_block_has_no_ancestor_to_re_anchor_to() {
        // The one branch of the rewind that is otherwise unreachable: `reorg_point`
        // of 0 means the *genesis block* changed, so `reorg_point - 1` is not a
        // height at all. It cannot happen on any real chain - but the code has an
        // explicit branch for it, and an explicit branch with no test is a branch
        // nobody has ever run. The delete goes ahead with nothing to re-anchor to,
        // leaving an empty window that the next tick re-seeds from the tip.
        let store = Store::open_in_memory().unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 0, "genesis_v0")
            .unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 1, "block_1_v0")
            .unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.seed_block_at(0, "genesis_v1", vec![]);
        daemon.push_block("block_1_v1", vec![]);

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000)
            .await
            .unwrap();
        assert_eq!(report.reorg_detected_at, Some(0));
        assert_eq!(
            store.lock().max_scanned_height(monero::Network::Mainnet).unwrap(),
            None,
            "there is no ancestor to preserve below genesis, so the window is emptied and re-seeded next tick"
        );
    }

    #[tokio::test]
    async fn the_scanned_block_window_stays_bounded_as_the_chain_grows() {
        // §TESTING.md 3's resource item, and the reason `prune_scanned_blocks_below`
        // exists. Two minutes per block means ~260k rows a year of pure growth for a
        // table nothing ever reads more than `reorg_check_depth` blocks back into,
        // on hardware that is often a router with an SD card.
        //
        // What the retention must *not* do matters more than the size it saves: the
        // window can never be pruned to nothing (an empty window reads as "this
        // network was never scanned" and re-seeds at the tip, skipping everything in
        // between), and a reorg at the far edge of the configured depth must still
        // find both a stored hash to notice it and an ancestor to re-anchor to
        // afterwards.
        let (store, key_custody, handle, tenant_id, _order_id) = setup().await;
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);

        let depth = 5;
        for i in 2..=60 {
            daemon.push_block(&format!("h{i}"), vec![]);
            run_scan_tick(
                &store,
                &key_custody,
                &daemon,
                "mainnet",
                &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
                depth,
                0,
            )
            .await
            .unwrap();
        }

        {
            let s = store.lock();
            assert_eq!(
                s.max_scanned_height(monero::Network::Mainnet).unwrap(),
                Some(60)
            );
            let retained: u64 = (0..=60)
                .filter(|h| {
                    s.get_scanned_block_hash(monero::Network::Mainnet, *h)
                        .unwrap()
                        .is_some()
                })
                .count() as u64;
            assert!(
                retained > depth,
                "the window must always comfortably cover the configured reorg depth: {retained}"
            );
            assert!(
                retained <= depth * 4 + 2,
                "and must not grow with the chain: {retained} rows after 60 blocks"
            );
        }

        // A reorg at the deepest point the configured window claims to cover is still
        // both detectable and rewindable after all that pruning.
        // (Same tip, so the window is exactly `60-depth..=60` and the fork sits on
        // its lower edge - a reorg that also *grew* the chain would move the window
        // up with it, which is the separate limitation
        // `a_reorg_deeper_than_the_window_is_reported_at_the_window_edge_and_leaves_older_payments_alone`
        // covers.)
        reorg_to_new_chain(&daemon, 60 - depth, 60, None);
        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", depth, 2000)
            .await
            .unwrap();
        assert_eq!(report.reorg_detected_at, Some(60 - depth));
        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(60 - depth - 1),
            "the rewind still finds an ancestor to anchor on below the pruned window"
        );
    }

    #[tokio::test]
    async fn a_void_that_lands_before_the_node_fails_still_updates_the_order_it_belongs_to() {
        // The nastiest shape of "the node went away mid-tick", and the reason voiding
        // and the status recompute it implies are one transaction rather than two
        // steps a failure can land between.
        //
        // A void is a *committed* write. Everything that makes it visible - the
        // order's status and received total, the retraction webhook, the
        // double-spend event - used to happen only after the whole sweep finished,
        // so a node failure on a later payment discarded all of it. That is
        // unrecoverable rather than merely delayed: the voided row is excluded from
        // the next tick's sweep by construction (it is no longer an unconfirmed
        // payment), and an order sitting in a terminal status is excluded from the
        // per-tick recompute too. The merchant is left looking at `paid` for an
        // order whose money was double-spent, permanently, with no event ever sent.
        let (store, key_custody, handle, tenant_id, order_id) =
            setup_with_confirmations_override(Some(0)).await;
        store
            .create_webhook(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();
        let first = fixture_tx();
        let second = independent_payment_tx(8);
        let per_tx = {
            let scan = scan_transaction(&key_custody, handle, &first, 0..3)
                .await
                .unwrap();
            scan.matches[0].amount_piconero.unwrap()
        };
        // An order that needs *both* payments, so voiding either one has to be
        // visible in the order's status rather than being absorbed by the other.
        store
            .execute_raw_for_test(&format!(
                "UPDATE orders SET xmr_amount_piconero = {} WHERE id = '{order_id}'",
                per_tx * 2
            ))
            .unwrap();
        let store = store.into_shared();

        let fake = FakeDaemonClient::new();
        fake.push_block("h1", vec![]);
        fake.set_mempool(vec![first.clone(), second.clone()]);
        let daemon = DaemonFailingFrom::counting(fake, DaemonCall::Locate);
        run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await
        .unwrap();
        {
            let s = store.lock();
            let order = s
                .get_order(
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &shared::ids::OrderId::new(order_id.to_string()),
                )
                .unwrap()
                .unwrap();
            assert_eq!(
                order.status,
                crate::status::OrderStatus::Paid,
                "covered by both payments, trusted at zero-conf"
            );
        }

        // Both transactions are double-spent out of the pool at once, and the node
        // dies on the second lookup - after the first payment has already been voided.
        daemon.inner.drop_from_mempool(&first);
        daemon.inner.drop_from_mempool(&second);
        for ki in key_images_of(&first)
            .iter()
            .chain(key_images_of(&second).iter())
        {
            daemon
                .inner
                .set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        daemon.fail_from.store(1, Ordering::SeqCst);

        // A node failure is retried next round, not a failed round (only this
        // engine's own storage failures are); what matters is below.
        let result = run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
            20,
            0,
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            daemon.call_count(),
            2,
            "the second lookup failed, and the page stopped there"
        );

        let s = store.lock();
        let payments = s
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap();
        let voided: Vec<_> = payments.iter().filter(|p| p.voided_at.is_some()).collect();
        assert_eq!(
            voided.len(),
            1,
            "exactly one payment was resolved before the node failed"
        );
        let order = s
            .get_order(
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            order.amount_received_piconero, per_tx,
            "the order's total must reflect the void that actually committed"
        );
        assert_eq!(
            order.status,
            crate::status::OrderStatus::Partial,
            "an order whose money was voided must never be left reading `paid` because a later RPC failed"
        );
        assert!(order.double_spend_detected_at.is_some());
        let events: Vec<String> = s
            .due_webhook_deliveries_for_test(crate::now_unix() + 1, 20)
            .unwrap()
            .into_iter()
            .map(|d| d.event_type)
            .collect();
        assert!(
            events.contains(&"order.double_spend_detected".to_string()),
            "and the merchant is told: {events:?}"
        );
        assert!(
            events.contains(&"order.partial".to_string()),
            "including the retraction of `paid`: {events:?}"
        );
    }

    #[test]
    fn run_scan_ticks_future_is_send() {
        // Direct regression test for a real bug caught only when this code was
        // actually wired into `tokio::spawn` in `main.rs`, not by any of the
        // `#[tokio::test]`s above: `rusqlite::Connection` is `Send` but not `Sync`,
        // so a `&Store` held across an `.await` (as combining
        // `scan_transaction`+`record_scan_match` into one async function would
        // require) makes the containing future `!Send`. Every test above awaits
        // `run_scan_tick` directly inside its own task, which never requires
        // `Send` - only `tokio::spawn`'s `F: Send + 'static` bound does, and
        // nothing here exercises that. This compiles only if the future actually
        // is `Send`; it doesn't need to run to prove the property.
        fn assert_send<T: Send>(_: T) {}

        let store = Store::open_in_memory().unwrap().into_shared();
        let key_custody = PlainKeyCustody::default();
        let daemon = FakeDaemonClient::new();
        let tenants: Vec<(crate::store::TenantId, WalletHandle)> = vec![];
        assert_send(run_scan_tick(
            &store,
            &key_custody,
            &daemon,
            "mainnet",
            &tenants,
            20,
            0,
        ));
    }

    // -- Per-tenant scan cursors (admin_settings_v2.md task 5.0) --------------

    use crate::status::OrderStatus;

    /// A transaction that pays nobody: `fixture_tx` with its transaction keys
    /// replaced, so no wallet's derivation matches its outputs, and a txid of
    /// its own. Gap blocks need real transactions in them: a tenant is only
    /// found to be failing when there is something to scan for it.
    pub(crate) fn unrelated_tx(seed: u8) -> Transaction {
        let mut tx = fixture_tx();
        let mut key_bytes = [seed.wrapping_add(7); 32];
        key_bytes[31] &= 0x0f;
        let other = PublicKey::from_private_key(&PrivateKey::from_slice(&key_bytes).unwrap());
        let extra = tx.prefix.extra.try_parse();
        let replaced = monero::blockdata::transaction::ExtraField(
            extra
                .0
                .into_iter()
                .map(|field| match field {
                    monero::blockdata::transaction::SubField::TxPublicKey(_) => {
                        monero::blockdata::transaction::SubField::TxPublicKey(other)
                    }
                    monero::blockdata::transaction::SubField::AdditionalPublickKey(keys) => {
                        monero::blockdata::transaction::SubField::AdditionalPublickKey(
                            keys.iter().map(|_| other).collect(),
                        )
                    }
                    field => field,
                })
                .collect(),
        );
        tx.prefix.extra = replaced.into();
        tx
    }

    /// Wraps a `PlainKeyCustody` and fails `scan_tx_outputs` for any handle in
    /// `failing`, the way a key-custody backend that is down fails for the
    /// stores whose keys it holds, while other stores keep working.
    #[derive(Default)]
    pub(crate) struct FlakyKeyCustody {
        inner: PlainKeyCustody,
        failing: parking_lot::Mutex<HashSet<WalletHandle>>,
        /// Scan calls made for each handle, failed or not.
        pub(crate) attempts: parking_lot::Mutex<HashMap<WalletHandle, u32>>,
        /// Run once, at the next scan call: for changing the world mid-scan.
        on_next_scan: parking_lot::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }

    impl FlakyKeyCustody {
        pub(crate) fn on_next_scan(&self, hook: impl FnOnce() + Send + 'static) {
            *self.on_next_scan.lock() = Some(Box::new(hook));
        }
        pub(crate) fn fail(&self, handle: WalletHandle) {
            self.failing.lock().insert(handle);
        }
        pub(crate) fn recover(&self, handle: WalletHandle) {
            self.failing.lock().remove(&handle);
        }
    }

    #[async_trait::async_trait]
    impl KeyCustody for FlakyKeyCustody {
        async fn register_wallet(
            &self,
            material: WalletMaterial,
        ) -> std::result::Result<WalletHandle, KeyCustodyError> {
            self.inner.register_wallet(material).await
        }
        async fn remove_wallet(
            &self,
            handle: WalletHandle,
        ) -> std::result::Result<(), KeyCustodyError> {
            self.inner.remove_wallet(handle).await
        }
        async fn seal(
            &self,
            material: &WalletMaterial,
        ) -> std::result::Result<Vec<u8>, KeyCustodyError> {
            self.inner.seal(material).await
        }
        async fn unseal_and_register(
            &self,
            sealed: &[u8],
        ) -> std::result::Result<WalletHandle, KeyCustodyError> {
            self.inner.unseal_and_register(sealed).await
        }
        async fn derive_subaddress(
            &self,
            handle: WalletHandle,
            index: SubaddressIndex,
            network: Network,
        ) -> std::result::Result<Address, KeyCustodyError> {
            self.inner.derive_subaddress(handle, index, network).await
        }
        async fn scan_tx_outputs(
            &self,
            handle: WalletHandle,
            tx: &ScanInput,
            major_range: Range<u32>,
            minor_range: Range<u32>,
        ) -> std::result::Result<Vec<MatchedOutput>, KeyCustodyError> {
            *self.attempts.lock().entry(handle).or_default() += 1;
            let hook = self.on_next_scan.lock().take();
            if let Some(hook) = hook {
                hook();
            }
            if self.failing.lock().contains(&handle) {
                return Err(KeyCustodyError::BackendUnavailable(
                    "simulated backend outage".into(),
                ));
            }
            self.inner
                .scan_tx_outputs(handle, tx, major_range, minor_range)
                .await
        }
    }

    /// A tenant on the fixture wallet with one order at minor index 1, which
    /// `fixture_tx` pays. Several of these can share one store: each gets its
    /// own payment row from the same transaction.
    pub(crate) async fn fixture_tenant(
        store: &Store,
        key_custody: &dyn KeyCustody,
        expires_at: i64,
    ) -> (crate::store::TenantId, WalletHandle, crate::store::OrderId) {
        let handle = register_fixture_wallet(key_custody).await;
        let (tenant, order) = fixture_tenant_rows(store, expires_at);
        (tenant, handle, order)
    }

    /// [`fixture_tenant`] on a shared store, locked only for the
    /// synchronous writes: the custody call is awaited first, with no lock
    /// held.
    pub(crate) async fn fixture_tenant_shared(
        store: &crate::store::SharedStore,
        key_custody: &dyn KeyCustody,
        expires_at: i64,
    ) -> (crate::store::TenantId, WalletHandle, crate::store::OrderId) {
        let handle = register_fixture_wallet(key_custody).await;
        let (tenant, order) = fixture_tenant_rows(&store.lock(), expires_at);
        (tenant, handle, order)
    }

    async fn register_fixture_wallet(key_custody: &dyn KeyCustody) -> WalletHandle {
        key_custody
            .register_wallet(WalletMaterial::new(
                fixture_view_key(),
                fixture_spend_pubkey(),
            ))
            .await
            .unwrap()
    }

    fn fixture_tenant_rows(
        store: &Store,
        expires_at: i64,
    ) -> (crate::store::TenantId, crate::store::OrderId) {
        let tenant = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4fixture".into(),
                    network: "mainnet".into(),
                    confirmations_required: Some(10),
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();
        let index = store.allocate_minor_index(&tenant.tenant.id).unwrap();
        assert_eq!(index, 1);
        let order = store
            .create_order(NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: tenant.tenant.id.clone(),
                merchant_order_id: None,
                minor_index: index,
                address: "fixture".into(),
                xmr_amount_piconero: 1,
                description: None,
                created_at: 1000,
                expires_at,
            })
            .unwrap();
        (
            shared::ids::TenantId::new(tenant.tenant.id.into_string()),
            shared::ids::OrderId::new(order.id.into_string()),
        )
    }

    pub(crate) fn cursor_of(store: &crate::store::SharedStore, tenant_id: &str) -> Option<u64> {
        store
            .lock()
            .get_tenant_by_id(&shared::ids::TenantId::new(tenant_id.to_string()))
            .unwrap()
            .unwrap()
            .scanned_through_height
    }

    pub(crate) fn order_status(
        store: &crate::store::SharedStore,
        order_id: &crate::store::OrderId,
    ) -> OrderStatus {
        let s = store.lock();
        let tenant_id = s
            .get_order_tenant_id(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .unwrap();
        s.get_order(&tenant_id, &shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .unwrap()
            .status
    }

    #[tokio::test]
    async fn one_tenants_custody_failure_does_not_stall_the_network_and_it_catches_up_later() {
        let store = Store::open_in_memory().unwrap();
        let custody = FlakyKeyCustody::default();
        let far_future = crate::now_unix() + 3600;
        let (a, a_handle, a_order) = fixture_tenant(&store, &custody, far_future).await;
        let (b, b_handle, b_order) = fixture_tenant(&store, &custody, far_future).await;
        let store = store.into_shared();
        let tenants = [(a.clone(), a_handle), (b.clone(), b_handle)];

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]); // seed
        daemon.push_block("h2", vec![fixture_tx()]); // pays both tenants

        custody.fail(a_handle);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();

        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(2),
            "the network moved on"
        );
        assert_eq!(cursor_of(&store, b.as_str()), Some(2));
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(b_order.to_string()))
                .unwrap()
                .len(),
            1,
            "B was paid on time"
        );
        assert_eq!(
            cursor_of(&store, a.as_str()),
            Some(1),
            "A stays where its last good scan left it"
        );
        assert!(store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .lock()
                .lagging_tenants(monero::Network::Mainnet)
                .unwrap(),
            vec![(a.clone(), 1)]
        );

        // More blocks arrive while A is still down.
        daemon.push_block("h3", vec![unrelated_tx(101)]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(cursor_of(&store, b.as_str()), Some(3));
        assert_eq!(cursor_of(&store, a.as_str()), Some(1));

        custody.recover(a_handle);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();

        assert_eq!(cursor_of(&store, a.as_str()), Some(3), "caught up");
        let a_payments = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
            .unwrap();
        assert_eq!(
            a_payments.len(),
            1,
            "A's payment in the gap was found, once"
        );
        assert_eq!(a_payments[0].block_height, Some(2));
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(b_order.to_string()))
                .unwrap()
                .len(),
            1,
            "B's payment wasn't recorded twice"
        );
        assert!(store
            .lock()
            .lagging_tenants(monero::Network::Mainnet)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn a_lagging_tenants_order_does_not_expire_until_it_has_caught_up() {
        let store = Store::open_in_memory().unwrap();
        let custody = FlakyKeyCustody::default();
        // Already past its deadline, with no grace period: a caught-up tenant's
        // order would expire on the first tick.
        let (a, a_handle, a_order) = fixture_tenant(&store, &custody, crate::now_unix() - 10).await;
        let store = store.into_shared();
        let tenants = [(a.clone(), a_handle)];

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(
            order_status(
                &store,
                &shared::ids::OrderId::new(a_order.as_str().to_string())
            ),
            OrderStatus::Expired,
            "sanity: a caught-up tenant's order expires"
        );

        // A second tenant, expired the same way, whose backend is down while
        // the block with its payment goes by.
        let (c, c_handle, c_order) = {
            let handle = custody
                .register_wallet(WalletMaterial::new(
                    fixture_view_key(),
                    fixture_spend_pubkey(),
                ))
                .await
                .unwrap();
            let s = store.lock();
            let tenant = s
                .create_tenant(
                    NewTenant {
                        key_custody_backend: "plain".into(),
                        sealed_key_material: vec![],
                        primary_address: "4fixture".into(),
                        network: "mainnet".into(),
                        confirmations_required: Some(10),
                        order_expiry_seconds: None,
                    },
                    1000,
                )
                .unwrap();
            let index = s.allocate_minor_index(&tenant.tenant.id).unwrap();
            let order = s
                .create_order(NewOrder {
                    idempotency_key: None,
                    confirmations_required_override: None,
                    tenant_id: tenant.tenant.id.clone(),
                    merchant_order_id: None,
                    minor_index: index,
                    address: "fixture".into(),
                    xmr_amount_piconero: 1,
                    description: None,
                    created_at: 1000,
                    expires_at: crate::now_unix() + 3600,
                })
                .unwrap();
            (tenant.tenant.id, handle, order.id)
        };
        let tenants = [
            (a.clone(), a_handle),
            (
                shared::ids::TenantId::new(c.clone().into_string()),
                c_handle,
            ),
        ];
        custody.fail(c_handle);
        daemon.push_block("h2", vec![fixture_tx()]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(cursor_of(&store, c.as_str()), Some(1));

        // Its deadline passes while it is still behind.
        store
            .lock()
            .execute_raw_for_test(&format!(
                "UPDATE orders SET expires_at_utc = 1 WHERE id = '{c_order}'"
            ))
            .unwrap();
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(
            order_status(
                &store,
                &shared::ids::OrderId::new(c_order.as_str().to_string())
            ),
            OrderStatus::Pending,
            "no expiry while its blocks are unchecked"
        );
        let expired_events = store
            .lock()
            .due_webhook_deliveries_for_test(crate::now_unix() + 1, 100)
            .unwrap()
            .into_iter()
            .filter(|d| d.order_id == c_order && d.event_type == "order.expired")
            .count();
        assert_eq!(expired_events, 0);

        // Once it catches up, the payment in the gap is found, so it never expires.
        custody.recover(c_handle);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(cursor_of(&store, c.as_str()), Some(2));
        assert_eq!(store.lock().get_all_payments(&c_order).unwrap().len(), 1);
        assert_ne!(
            order_status(
                &store,
                &shared::ids::OrderId::new(c_order.as_str().to_string())
            ),
            OrderStatus::Expired
        );
    }

    #[tokio::test]
    async fn an_unpaid_lagging_tenants_order_expires_once_it_has_caught_up() {
        let store = Store::open_in_memory().unwrap();
        let custody = FlakyKeyCustody::default();
        let (a, a_handle, a_order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let store = store.into_shared();
        let tenants = [(a.clone(), a_handle)];
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();

        custody.fail(a_handle);
        daemon.push_block("h2", vec![unrelated_tx(102)]);
        store
            .lock()
            .execute_raw_for_test(&format!(
                "UPDATE orders SET expires_at_utc = 1 WHERE id = '{a_order}'"
            ))
            .unwrap();
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(
            order_status(
                &store,
                &shared::ids::OrderId::new(a_order.as_str().to_string())
            ),
            OrderStatus::Pending
        );

        custody.recover(a_handle);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(cursor_of(&store, a.as_str()), Some(2));
        assert_eq!(
            order_status(
                &store,
                &shared::ids::OrderId::new(a_order.as_str().to_string())
            ),
            OrderStatus::Expired,
            "nothing was paid, so it expires as normal"
        );
    }

    #[tokio::test]
    async fn a_tenant_without_registered_keys_is_held_back_and_caught_up_once_they_are() {
        let store = Store::open_in_memory().unwrap();
        let custody = PlainKeyCustody::default();
        let (a, a_handle, a_order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![fixture_tx()]);

        // A's keys aren't registered this time (it isn't in the tenant list).
        run_scan_tick(&store, &custody, &daemon, "mainnet", &[], 20, 0)
            .await
            .unwrap();
        run_scan_tick(&store, &custody, &daemon, "mainnet", &[], 20, 0)
            .await
            .unwrap();
        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(2)
        );
        assert_eq!(
            cursor_of(&store, a.as_str()),
            Some(1),
            "not moved past blocks nobody checked for it"
        );

        run_scan_tick(
            &store,
            &custody,
            &daemon,
            "mainnet",
            &[(a.clone(), a_handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(cursor_of(&store, a.as_str()), Some(2));
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn a_tenant_with_nothing_in_scope_is_never_reported_as_lagging() {
        let store = Store::open_in_memory().unwrap();
        let tenant = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4idle".into(),
                    network: "mainnet".into(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap()
            .tenant;
        assert_eq!(
            tenant.scanned_through_height, None,
            "network not seeded yet"
        );
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h1b", vec![]);
        run_scan_tick(
            &store,
            &PlainKeyCustody::default(),
            &daemon,
            "mainnet",
            &[],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            cursor_of(&store, tenant.id.as_str()),
            Some(2),
            "anchored when the network was seeded"
        );

        // Even from a cursor left far behind (say, by a bug or an old crash).
        store
            .lock()
            .execute_raw_for_test(&format!(
                "UPDATE tenants SET scanned_through_height = 0 WHERE id = '{}'",
                tenant.id
            ))
            .unwrap();
        daemon.push_block("h2", vec![unrelated_tx(103)]);
        run_scan_tick(
            &store,
            &PlainKeyCustody::default(),
            &daemon,
            "mainnet",
            &[],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(cursor_of(&store, tenant.id.as_str()), Some(3));
        assert!(store
            .lock()
            .lagging_tenants(monero::Network::Mainnet)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_new_tenant_starts_at_its_networks_scanned_height() {
        let store = Store::open_in_memory().unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 50, "h50")
            .unwrap();
        let new = |network: &str| {
            store
                .create_tenant(
                    NewTenant {
                        key_custody_backend: "plain".into(),
                        sealed_key_material: vec![],
                        primary_address: "4x".into(),
                        network: network.into(),
                        confirmations_required: None,
                        order_expiry_seconds: None,
                    },
                    1000,
                )
                .unwrap()
                .tenant
        };
        assert_eq!(
            new("mainnet").scanned_through_height,
            Some(50),
            "not 0: catch-up must not replay the chain"
        );
        assert_eq!(
            new("stagenet").scanned_through_height,
            None,
            "a network never scanned leaves it unset"
        );
    }

    #[tokio::test]
    async fn a_reorg_rewinds_cursors_to_below_the_reorg_point_and_the_replacement_block_is_scanned_for_everyone(
    ) {
        let store = Store::open_in_memory().unwrap();
        let custody = FlakyKeyCustody::default();
        let (a, a_handle, a_order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let (b, b_handle, b_order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let store = store.into_shared();
        let tenants = [(a.clone(), a_handle), (b.clone(), b_handle)];

        let daemon = FakeDaemonClient::new();
        for h in 1..=5 {
            daemon.push_block(&format!("old_{h}"), vec![unrelated_tx(h as u8)]);
        }
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(cursor_of(&store, a.as_str()), Some(5));

        // B falls behind at 5 before the reorg.
        custody.fail(b_handle);
        daemon.push_block("old_6", vec![unrelated_tx(104)]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(
            (cursor_of(&store, a.as_str()), cursor_of(&store, b.as_str())),
            (Some(6), Some(5))
        );

        // The fork replaces 5 and 6; the payment exists only at the new 5.
        daemon.reorg_from(
            5,
            vec![
                ("new_5", vec![fixture_tx()]),
                ("new_6", vec![unrelated_tx(66)]),
            ],
        );
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(4)
        );
        assert_eq!(
            cursor_of(&store, a.as_str()),
            Some(4),
            "clamped to reorg_point - 1, not reorg_point"
        );
        assert_eq!(cursor_of(&store, b.as_str()), Some(4));

        custody.recover(b_handle);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        for (tenant, order) in [(&a, &a_order), (&b, &b_order)] {
            assert_eq!(cursor_of(&store, tenant.as_str()), Some(6));
            let payments = store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(order.to_string()))
                .unwrap();
            assert_eq!(
                payments.len(),
                1,
                "the payment in the replacement block at the reorg point was found"
            );
            assert_eq!(payments[0].block_height, Some(5));
        }
    }

    #[tokio::test]
    async fn catch_up_stops_at_a_block_that_differs_from_the_one_scanned_earlier() {
        let store = Store::open_in_memory().unwrap();
        let custody = FlakyKeyCustody::default();
        let (a, a_handle, a_order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let (b, b_handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let store = store.into_shared();
        let tenants = [(a.clone(), a_handle), (b.clone(), b_handle)];
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(cursor_of(&store, a.as_str()), Some(2));

        custody.fail(a_handle);
        daemon.push_block("h3", vec![unrelated_tx(105)]);
        daemon.push_block("h4", vec![unrelated_tx(106)]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(cursor_of(&store, a.as_str()), Some(2));

        // The chain from 3 up changes before A catches up, and the payment is
        // now at 3. Catch-up for A must not record from blocks that differ from
        // what the network scan stored; the reorg check rewinds, and A is
        // caught up against the new chain.
        daemon.reorg_from(
            3,
            vec![
                ("new_3", vec![fixture_tx()]),
                ("new_4", vec![unrelated_tx(33)]),
            ],
        );
        custody.recover(a_handle);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        // Catch-up saw block 3's hash differ from the stored one and stopped
        // without moving A; the reorg check then rewound everyone to 2.
        assert_eq!(cursor_of(&store, a.as_str()), Some(2));
        assert_eq!(
            store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap(),
            Some(2)
        );
        for _ in 0..3 {
            run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
                .await
                .unwrap();
        }
        assert_eq!(cursor_of(&store, a.as_str()), Some(4));
        let payments = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
            .unwrap();
        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].block_height, Some(3));
    }

    #[tokio::test]
    async fn tenants_lagging_at_the_same_cursor_share_one_block_fetch() {
        let store = Store::open_in_memory().unwrap();
        let custody = FlakyKeyCustody::default();
        let mut tenants = vec![];
        for _ in 0..5 {
            let (id, handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
            tenants.push((id, handle));
        }
        let store = store.into_shared();
        let daemon = DaemonFailingFrom::counting(FakeDaemonClient::new(), DaemonCall::ChainBlocks);
        daemon.inner.push_block("h1", vec![]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();

        for (_, handle) in &tenants {
            custody.fail(*handle);
        }
        daemon.inner.push_block("h2", vec![fixture_tx()]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        for (_, handle) in &tenants {
            custody.recover(*handle);
        }

        // No new block: this tick's only range fetches are catch-up's.
        let before = daemon.call_count();
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(
            daemon.call_count() - before,
            1,
            "five tenants behind at the same block, one fetch"
        );
        for (id, _) in &tenants {
            assert_eq!(cursor_of(&store, id.as_str()), Some(2));
        }
    }

    #[tokio::test]
    async fn a_lagging_tenant_whose_keys_work_is_still_matched_in_the_mempool() {
        let store = Store::open_in_memory().unwrap();
        let custody = FlakyKeyCustody::default();
        let (a, a_handle, a_order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let store = store.into_shared();
        let tenants = [(a.clone(), a_handle)];
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        custody.fail(a_handle);
        daemon.push_block("h2", vec![unrelated_tx(107)]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(cursor_of(&store, a.as_str()), Some(1));

        // Put A a long way behind so one tick can't finish catching up, then
        // let its keys work again with a payment waiting in the pool.
        store
            .lock()
            .execute_raw_for_test(&format!(
                "UPDATE tenants SET scanned_through_height = 0 WHERE id = '{a}'"
            ))
            .unwrap();
        custody.recover(a_handle);
        daemon.set_mempool(vec![fixture_tx()]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        let payments = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
            .unwrap();
        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].block_height, None, "seen in the pool, 0-conf");
    }

    /// Random outages, recoveries, blocks and reorgs for several tenants.
    /// Whatever happens, once every backend is healthy again and enough ticks
    /// have run, every tenant has exactly the payment it would have had with no
    /// failures, at the right height, and its order isn't expired.
    #[tokio::test]
    async fn random_outages_and_reorgs_never_lose_or_duplicate_a_payment() {
        for seed in 1..=12u64 {
            let mut rng = seed;
            let mut next = move |n: u64| {
                rng = rng
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (rng >> 33) % n
            };
            let store = Store::open_in_memory().unwrap();
            let custody = FlakyKeyCustody::default();
            let mut tenants = vec![];
            let mut orders = vec![];
            for _ in 0..4 {
                let (id, handle, order) =
                    fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
                tenants.push((id, handle));
                orders.push(order);
            }
            let store = store.into_shared();
            let daemon = FakeDaemonClient::new();
            daemon.push_block("b1", vec![]);
            let mut tip = 1u64;
            let mut payment_height: Option<u64> = None;
            let mut fork = 0u32;

            for step in 0..30 {
                for (_, handle) in &tenants {
                    if next(4) == 0 {
                        custody.fail(*handle);
                    } else if next(2) == 0 {
                        custody.recover(*handle);
                    }
                }
                match next(6) {
                    0..=2 => {
                        let pay = payment_height.is_none() && next(3) == 0;
                        tip = daemon.push_block(
                            &format!("f{fork}_b{}", tip + 1),
                            if pay {
                                vec![fixture_tx()]
                            } else {
                                vec![unrelated_tx(tip as u8 + 50)]
                            },
                        );
                        if pay {
                            payment_height = Some(tip);
                        }
                    }
                    3 if tip > 3 => {
                        // Replace the last one or two blocks, moving the payment
                        // into the first replacement block if it was among them.
                        let from = tip - next(2);
                        fork += 1;
                        let moved = payment_height.is_some_and(|h| h >= from);
                        let blocks: Vec<(String, Vec<Transaction>)> = (from..=tip)
                            .map(|h| {
                                (
                                    format!("f{fork}_b{h}"),
                                    if moved && h == from {
                                        vec![fixture_tx()]
                                    } else {
                                        vec![unrelated_tx((h as u8).wrapping_add(fork as u8 * 17))]
                                    },
                                )
                            })
                            .collect();
                        daemon.reorg_from(
                            from,
                            blocks
                                .iter()
                                .map(|(hash, txs)| (hash.as_str(), txs.clone()))
                                .collect(),
                        );
                        if moved {
                            payment_height = Some(from);
                        }
                    }
                    _ => {}
                }
                let _ = step;
                run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
                    .await
                    .unwrap();
            }

            for (_, handle) in &tenants {
                custody.recover(*handle);
            }
            for _ in 0..10 {
                run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
                    .await
                    .unwrap();
            }

            let high_water = store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap();
            for ((tenant_id, _), order_id) in tenants.iter().zip(&orders) {
                assert_eq!(
                    cursor_of(&store, tenant_id.as_str()),
                    high_water,
                    "seed {seed}: tenant caught up"
                );
                let payments: Vec<_> = store
                    .lock()
                    .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                    .unwrap()
                    .into_iter()
                    .filter(|p| p.voided_at.is_none())
                    .collect();
                match payment_height {
                    Some(h) => {
                        assert_eq!(payments.len(), 1, "seed {seed}: exactly one payment");
                        assert_eq!(
                            payments[0].block_height,
                            Some(h as i64),
                            "seed {seed}: at the height it's really at"
                        );
                    }
                    None => assert!(payments.is_empty(), "seed {seed}"),
                }
                assert_ne!(
                    order_status(
                        &store,
                        &shared::ids::OrderId::new(order_id.as_str().to_string())
                    ),
                    OrderStatus::Expired,
                    "seed {seed}"
                );
            }
        }
    }

    // -- Fair, concurrent scanning within a tick (task 7.4) -------------------

    /// Fixture-only backend with controlled async delays. Compute real matches
    /// during registration, before any deadlines exist: paused-time tests must
    /// not wait on CPU slots held by other tests' independent runtimes.
    #[derive(Default)]
    pub(crate) struct SlowKeyCustody {
        inner: PlainKeyCustody,
        pub(crate) delays: parking_lot::Mutex<HashMap<WalletHandle, Duration>>,
        matches: parking_lot::Mutex<HashMap<WalletHandle, Vec<MatchedOutput>>>,
    }

    #[async_trait::async_trait]
    impl KeyCustody for SlowKeyCustody {
        async fn register_wallet(
            &self,
            material: WalletMaterial,
        ) -> std::result::Result<WalletHandle, KeyCustodyError> {
            let handle = self.inner.register_wallet(material).await?;
            let matches = self
                .inner
                .scan_tx_outputs(handle, &ScanInput::of(&fixture_tx()), 0..1, 1..2)
                .await?;
            self.matches.lock().insert(handle, matches);
            Ok(handle)
        }
        async fn remove_wallet(
            &self,
            handle: WalletHandle,
        ) -> std::result::Result<(), KeyCustodyError> {
            self.inner.remove_wallet(handle).await
        }
        async fn seal(
            &self,
            material: &WalletMaterial,
        ) -> std::result::Result<Vec<u8>, KeyCustodyError> {
            self.inner.seal(material).await
        }
        async fn unseal_and_register(
            &self,
            sealed: &[u8],
        ) -> std::result::Result<WalletHandle, KeyCustodyError> {
            self.inner.unseal_and_register(sealed).await
        }
        async fn derive_subaddress(
            &self,
            handle: WalletHandle,
            index: SubaddressIndex,
            network: Network,
        ) -> std::result::Result<Address, KeyCustodyError> {
            self.inner.derive_subaddress(handle, index, network).await
        }
        async fn scan_tx_outputs(
            &self,
            handle: WalletHandle,
            tx: &ScanInput,
            major_range: Range<u32>,
            minor_range: Range<u32>,
        ) -> std::result::Result<Vec<MatchedOutput>, KeyCustodyError> {
            let delay = self.delays.lock().get(&handle).copied();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            assert_eq!(
                *tx,
                ScanInput::of(&fixture_tx()),
                "this backend only scans the fixture transaction"
            );
            let matches = self.matches.lock();
            let matches = matches.get(&handle).ok_or(KeyCustodyError::UnknownWallet)?;
            Ok(matches
                .iter()
                .copied()
                .filter(|m| {
                    major_range.contains(&m.subaddress_index.major)
                        && minor_range.contains(&m.subaddress_index.minor)
                })
                .collect())
        }
    }

    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn a_tenant_whose_backend_answers_slowly_is_left_behind_and_holds_nobody_up() {
        let store = Store::open_in_memory().unwrap();
        let custody = SlowKeyCustody::default();
        let (a, a_handle, a_order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let (b, b_handle, b_order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let store = store.into_shared();
        let tenants = [(a.clone(), a_handle), (b.clone(), b_handle)];
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();

        custody
            .delays
            .lock()
            .insert(a_handle, Duration::from_secs(10 * 60));
        daemon.push_block("h3", vec![fixture_tx()]);
        let started = tokio::time::Instant::now();
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert!(
            started.elapsed() < SCAN_CALL_DEADLINE * 3,
            "bounded by the per-call deadline: {:?}",
            started.elapsed()
        );
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(b_order.to_string()))
                .unwrap()
                .len(),
            1,
            "B was paid on time"
        );
        assert_eq!(cursor_of(&store, a.as_str()), Some(2), "A left behind");

        custody.delays.lock().clear();
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
                .unwrap()
                .len(),
            1,
            "and caught up once it answers normally"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_tenants_mempool_scans_do_not_consume_every_tick_before_blocks() {
        let store = Store::open_in_memory().unwrap();
        let custody = SlowKeyCustody::default();
        let (tenant, handle, order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let store = store.into_shared();
        let tenants = [(tenant, handle)];
        let daemon = FakeDaemonClient::new();
        let memory = crate::work::ScanState::default();
        daemon.push_block("b1", vec![]);
        daemon.push_block("b2", vec![]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        custody
            .delays
            .lock()
            .insert(handle, Duration::from_secs(600));
        daemon.set_mempool((0..13).map(unrelated_tx).collect());
        let deadline = crate::loops::tick_deadline(Duration::from_secs(1));
        for height in 3..=4 {
            daemon.push_block(&format!("b{height}"), vec![]);
            let result = tokio::time::timeout(
                deadline,
                run_scan_tick_with(
                    &memory, &store, &custody, &daemon, "mainnet", &tenants, 20, 0, 64,
                ),
            )
            .await;
            assert!(
                result.is_ok(),
                "retrying the same failed tenant must not starve block scanning"
            );
            result.unwrap().unwrap();
            assert_eq!(
                store
                    .lock()
                    .max_scanned_height(monero::Network::Mainnet)
                    .unwrap(),
                Some(height)
            );
        }
        custody.delays.lock().clear();
        daemon.set_mempool(vec![fixture_tx()]);
        run_scan_tick_with(
            &memory, &store, &custody, &daemon, "mainnet", &tenants, 20, 0, 64,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(order.to_string()))
                .unwrap()
                .len(),
            1,
            "the tenant is retried after recovery"
        );
    }

    // Real time, not a paused clock: the scans run on the blocking pool, one
    // per core, and between two of them a paused clock jumps ahead to the
    // next timer, which on a one- or two-core machine is the scan deadline.
    #[tokio::test]
    async fn tenants_are_scanned_concurrently_not_one_after_another() {
        let store = Store::open_in_memory().unwrap();
        let custody = SlowKeyCustody::default();
        let mut tenants = vec![];
        for _ in 0..20 {
            let (id, handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
            custody.delays.lock().insert(handle, Duration::from_secs(1));
            tenants.push((id, handle));
        }
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();

        daemon.push_block("h3", vec![fixture_tx()]);
        let started = tokio::time::Instant::now();
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        // 20 tenants x 1s each: about 1s at once (plus the mempool pass),
        // not 20s in a row. The bound is half the serial time, so a loaded
        // machine's slowness can't fail it, only scanning one by one can.
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "took {:?}",
            started.elapsed()
        );
        for (id, _) in &tenants {
            assert_eq!(cursor_of(&store, id.as_str()), Some(3));
        }
    }

    // -- Fixes from the independent review of the per-tenant cursors ---------

    #[tokio::test]
    async fn a_late_payment_in_the_grace_period_is_found_even_if_the_grace_period_ends_during_the_gap(
    ) {
        let store = Store::open_in_memory().unwrap();
        let custody = FlakyKeyCustody::default();
        let now = crate::now_unix();
        // Expired 50s ago, inside a one-hour grace period.
        let (a, a_handle, a_order) = fixture_tenant(&store, &custody, now - 50).await;
        let store = store.into_shared();
        let tenants = [(a.clone(), a_handle)];
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        daemon.set_block_timestamp(2, (now - 100) as u64);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 3600)
            .await
            .unwrap();
        assert_eq!(
            order_status(
                &store,
                &shared::ids::OrderId::new(a_order.as_str().to_string())
            ),
            OrderStatus::Expired
        );

        // Backend down while the late payment is mined.
        custody.fail(a_handle);
        daemon.push_block("h3", vec![fixture_tx()]);
        daemon.set_block_timestamp(3, (now - 20) as u64);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 3600)
            .await
            .unwrap();
        assert_eq!(cursor_of(&store, a.as_str()), Some(2));

        // The grace period is over by now (modelled by a grace of 0), so the
        // order is no longer in scope today. It was in scope during the gap,
        // though, so the tenant must still be caught up rather than skipped.
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(
            cursor_of(&store, a.as_str()),
            Some(2),
            "not moved past the block with the payment"
        );
        custody.recover(a_handle);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
            .await
            .unwrap();
        assert_eq!(cursor_of(&store, a.as_str()), Some(3));
        let payments = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
            .unwrap();
        assert_eq!(payments.len(), 1, "the late payment was recorded");
        assert_eq!(payments[0].block_height, Some(3));
    }

    #[tokio::test]
    async fn a_disabled_tenants_orders_still_expire_and_it_never_counts_as_lagging() {
        let store = Store::open_in_memory().unwrap();
        let custody = PlainKeyCustody::default();
        let (a, _a_handle, a_order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &[], 20, 0)
            .await
            .unwrap();

        store
            .lock()
            .disable_tenant(
                &shared::ids::TenantId::new(a.to_string()),
                crate::now_unix(),
            )
            .unwrap();
        store
            .lock()
            .execute_raw_for_test(&format!(
                "UPDATE orders SET expires_at_utc = 1 WHERE id = '{a_order}'"
            ))
            .unwrap();
        daemon.push_block("h3", vec![unrelated_tx(90)]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &[], 20, 0)
            .await
            .unwrap();
        run_scan_tick(&store, &custody, &daemon, "mainnet", &[], 20, 0)
            .await
            .unwrap();
        assert_eq!(
            order_status(
                &store,
                &shared::ids::OrderId::new(a_order.as_str().to_string())
            ),
            OrderStatus::Expired
        );
        assert_eq!(cursor_of(&store, a.as_str()), Some(3));
        assert!(store
            .lock()
            .lagging_tenants(monero::Network::Mainnet)
            .unwrap()
            .is_empty());
    }

    /// Only a tenant with nothing that could have been paid moves without a
    /// scan, and "nothing in scope" is decided inside the transaction: a tenant
    /// with an open order stays at its cursor (the block scan moves it, with a
    /// `ScannedBlock`), as does one on another cursor or network.
    #[test]
    fn only_tenants_with_nothing_in_scope_move_without_a_scan() {
        let store = Store::open_in_memory().unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 10, "h10")
            .unwrap();
        let tenant = |network: &str, cursor: i64, with_order: bool| {
            let id = store
                .create_tenant(
                    NewTenant {
                        key_custody_backend: "plain".into(),
                        sealed_key_material: vec![],
                        primary_address: format!("4{}", uuid::Uuid::new_v4().simple()),
                        network: network.into(),
                        confirmations_required: None,
                        order_expiry_seconds: None,
                    },
                    100,
                )
                .unwrap()
                .tenant
                .id;
            store
                .execute_raw_for_test(&format!(
                    "UPDATE tenants SET scanned_through_height = {cursor} WHERE id = '{id}'"
                ))
                .unwrap();
            if with_order {
                let index = store.allocate_minor_index(&id).unwrap();
                store
                    .create_order(NewOrder {
                        idempotency_key: None,
                        confirmations_required_override: None,
                        tenant_id: id.clone(),
                        merchant_order_id: None,
                        minor_index: index,
                        address: format!("addr-{}", uuid::Uuid::new_v4().simple()),
                        xmr_amount_piconero: 1,
                        description: None,
                        created_at: 100,
                        expires_at: 10_000,
                    })
                    .unwrap();
            }
            id
        };
        let idle = tenant("mainnet", 10, false);
        let active = tenant("mainnet", 10, true);
        let elsewhere = tenant("mainnet", 9, false);
        let other_network = tenant("stagenet", 10, false);

        assert_eq!(
            store
                .advance_idle_cursors(monero::Network::Mainnet, 10, 11, 500, 0)
                .unwrap(),
            1
        );
        let cursor = |id: &str| {
            store
                .get_tenant_by_id(&shared::ids::TenantId::new(id.to_string()))
                .unwrap()
                .unwrap()
                .scanned_through_height
        };
        assert_eq!(cursor(idle.as_str()), Some(11));
        assert_eq!(
            cursor(active.as_str()),
            Some(10),
            "block 11 was never checked against its order"
        );
        assert_eq!(cursor(elsewhere.as_str()), Some(9));
        assert_eq!(cursor(other_network.as_str()), Some(10));
    }

    #[tokio::test]
    async fn a_transaction_that_can_pay_nobody_is_no_match_not_a_failure() {
        let mut tx = fixture_tx();
        tx.prefix.extra = monero::blockdata::transaction::RawExtraField(vec![]);
        let custody = PlainKeyCustody::default();
        let handle = custody
            .register_wallet(WalletMaterial::new(
                fixture_view_key(),
                fixture_spend_pubkey(),
            ))
            .await
            .unwrap();
        let matches = custody
            .scan_tx_outputs(handle, &ScanInput::of(&tx), 0..1, 0..10)
            .await
            .unwrap();
        assert!(matches.is_empty());
    }

    #[tokio::test]
    async fn keys_that_failed_to_register_are_registered_by_the_retry() {
        let store = Store::open_in_memory().unwrap();
        let custody = PlainKeyCustody::default();
        let material = WalletMaterial::new(fixture_view_key(), fixture_spend_pubkey());
        let sealed = custody.seal(&material).await.unwrap();
        let tenant = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: sealed,
                    primary_address: "4x".into(),
                    network: "mainnet".into(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1,
            )
            .unwrap()
            .tenant;
        let store = store.into_shared();
        let handles = parking_lot::RwLock::new(HashMap::new());
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &custody,
                &handles,
                None,
                monero::Network::Stagenet
            )
            .await,
            0,
            "other networks untouched"
        );
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &custody,
                &handles,
                None,
                monero::Network::Mainnet
            )
            .await,
            1
        );
        assert!(handles.read().contains_key(&tenant.id));
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &custody,
                &handles,
                None,
                monero::Network::Mainnet
            )
            .await,
            0,
            "nothing left to do"
        );
    }

    #[tokio::test]
    async fn catch_up_refuses_a_block_whose_hash_differs_even_when_the_reorg_check_cannot_see_it() {
        let store = Store::open_in_memory().unwrap();
        let custody = FlakyKeyCustody::default();
        let (a, a_handle, a_order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let store = store.into_shared();
        let tenants = [(a.clone(), a_handle)];
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 1, 0)
            .await
            .unwrap();

        custody.fail(a_handle);
        for h in 3..=6 {
            daemon.push_block(&format!("h{h}"), vec![unrelated_tx(h as u8 + 120)]);
        }
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 1, 0)
            .await
            .unwrap();
        assert_eq!(cursor_of(&store, a.as_str()), Some(2));

        // Block 3 now answers with another hash and a payment, below a
        // reorg-check window of 1 block, so only catch-up can notice.
        daemon.seed_block_at(3, "other_3", vec![fixture_tx()]);
        custody.recover(a_handle);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 1, 0)
            .await
            .unwrap();
        assert_eq!(
            cursor_of(&store, a.as_str()),
            Some(2),
            "stopped at the block that differs"
        );
        assert!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
                .unwrap()
                .is_empty(),
            "nothing recorded from it"
        );
    }

    // -- Crash safety (task 7.11) ----------------------------------------------

    /// A daemon that yields to the executor before every call, so a tick has
    /// an await point wherever it talks to the node.
    struct YieldingDaemon(FakeDaemonClient);

    #[async_trait::async_trait]
    impl MoneroDaemonClient for YieldingDaemon {
        async fn get_height(&self) -> std::result::Result<u64, DaemonError> {
            tokio::task::yield_now().await;
            self.0.get_height().await
        }
        async fn get_block_hash(&self, height: u64) -> std::result::Result<String, DaemonError> {
            tokio::task::yield_now().await;
            self.0.get_block_hash(height).await
        }
        async fn get_chain_blocks(
            &self,
            start_height: u64,
            count: u64,
        ) -> std::result::Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
            tokio::task::yield_now().await;
            self.0.get_chain_blocks(start_height, count).await
        }
        async fn get_mempool_txids(&self) -> std::result::Result<Vec<String>, DaemonError> {
            tokio::task::yield_now().await;
            self.0.get_mempool_txids().await
        }
        async fn get_transactions_with_ids(
            &self,
            txids: &[String],
        ) -> std::result::Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
            tokio::task::yield_now().await;
            self.0.get_transactions_with_ids(txids).await
        }
        async fn locate_transaction(
            &self,
            txid: &str,
        ) -> std::result::Result<TxLocation, DaemonError> {
            tokio::task::yield_now().await;
            self.0.locate_transaction(txid).await
        }
        async fn is_key_image_spent(
            &self,
            key_images: &[String],
        ) -> std::result::Result<Vec<KeyImageStatus>, DaemonError> {
            tokio::task::yield_now().await;
            self.0.is_key_image_spent(key_images).await
        }
    }

    /// Polls `future` at most `polls` times, then drops it where it stands:
    /// the in-process equivalent of the engine being killed at that point.
    /// Everything written before the cut stays written; nothing after it
    /// happens.
    async fn run_until_killed<F: std::future::Future>(
        future: F,
        polls: usize,
    ) -> Option<F::Output> {
        let mut future = Box::pin(future);
        let mut left = polls;
        std::future::poll_fn(move |cx| {
            if left == 0 {
                return std::task::Poll::Ready(None);
            }
            left -= 1;
            future.as_mut().poll(cx).map(Some)
        })
        .await
    }

    #[tokio::test]
    async fn cancellation_after_a_closed_orders_payment_is_written_does_not_lose_its_status_or_webhook(
    ) {
        use std::future::Future;
        let store = Store::open_in_memory().unwrap();
        let custody = PlainKeyCustody::default();
        let now = crate::now_unix();
        let (tenant, handle, order) = fixture_tenant(&store, &custody, now + 3600).await;
        store
            .execute_raw_for_test(&format!(
                "UPDATE orders SET confirmations_required_override = 0 WHERE id = '{order}'"
            ))
            .unwrap();
        store
            .create_webhook(
                &shared::ids::TenantId::new(tenant.to_string()),
                "https://shop.example/hook",
                "{}",
                "whsec",
                now,
            )
            .unwrap();
        store
            .record_payment_match(
                &shared::ids::OrderId::new(order.to_string()),
                "original",
                0,
                1,
                "[]",
                now,
                Some(1),
                None,
            )
            .unwrap();
        recompute_and_notify(
            &store,
            &shared::ids::OrderId::new(order.to_string()),
            2,
            now,
        )
        .unwrap();
        let store = store.into_shared();
        assert_eq!(
            order_status(
                &store,
                &shared::ids::OrderId::new(order.as_str().to_string())
            ),
            OrderStatus::Paid
        );
        let tenants = [(tenant, handle)];
        let daemon = YieldingDaemon(FakeDaemonClient::new());
        daemon.0.push_block("b1", vec![]);
        daemon.0.push_block("b2", vec![]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 3600)
            .await
            .unwrap();
        daemon.0.push_block("b3", vec![fixture_tx()]);
        {
            // Every database job an await point, as with the worker: the
            // tick can be cut between writing the payment and settling it.
            let db = crate::store::Db::over_shared_yielding(store.clone());
            let state = crate::work::ScanState::default();
            let inputs = crate::work::RoundInputs {
                db: &db,
                custody: &custody,
                daemon: &daemon,
                network: monero::Network::Mainnet,
                tenants: &tenants,
                reorg_check_depth: 20,
                grace_period_seconds: 3600,
                scan_chunk_memory_budget_mb: crate::engine_settings::EngineSettings::defaults()
                    .scan
                    .load()
                    .scan_chunk_memory_budget_mb,
            };
            let mut tick = Box::pin(crate::work::run_round(
                &state,
                &inputs,
                crate::work::ROUND_BUDGET,
            ));
            std::future::poll_fn(|cx| {
                assert!(tick.as_mut().poll(cx).is_pending());
                if store
                    .lock()
                    .get_all_payments(&shared::ids::OrderId::new(order.to_string()))
                    .unwrap()
                    .len()
                    == 2
                {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            })
            .await;
            // This tick's volatile `touched` set disappears here.
        }
        // Let the closed order fall outside the scan window before recovery.
        // Its already-persisted payment still needs a status update.
        store
            .lock()
            .execute_raw_for_test(&format!(
                "UPDATE orders SET closed_at_utc = 1 WHERE id = '{order}'"
            ))
            .unwrap();
        for _ in 0..2 {
            run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
                .await
                .unwrap();
        }
        assert_eq!(
            order_status(
                &store,
                &shared::ids::OrderId::new(order.as_str().to_string())
            ),
            OrderStatus::Overpaid
        );
        let events = store
            .lock()
            .due_webhook_deliveries_for_test(i64::MAX / 2, 100)
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|d| d.order_id == order && d.event_type == "order.overpaid")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn killing_the_engine_at_any_point_in_a_tick_never_loses_or_duplicates_a_payment_or_its_webhook(
    ) {
        for seed in 1..=12u64 {
            let mut rng = seed;
            let mut next = move |n: u64| {
                rng = rng
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (rng >> 33) % n
            };
            let store = Store::open_in_memory().unwrap();
            let custody = PlainKeyCustody::default();
            let mut tenants = vec![];
            let mut orders = vec![];
            for _ in 0..3 {
                let (id, handle, order) =
                    fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
                store
                    .create_webhook(
                        &shared::ids::TenantId::new(id.to_string()),
                        "https://shop.example/hook",
                        "{}",
                        "whsec",
                        1,
                    )
                    .unwrap();
                tenants.push((id, handle));
                orders.push(order);
            }
            let store = store.into_shared();
            let daemon = YieldingDaemon(FakeDaemonClient::new());
            daemon.0.push_block("b1", vec![]);
            daemon.0.push_block("b2", vec![]);
            let mut paid_at = None;

            for step in 0..20u64 {
                if paid_at.is_none() && next(4) == 0 {
                    if next(2) == 0 {
                        daemon.0.set_mempool(vec![fixture_tx()]);
                    }
                    let h = daemon
                        .0
                        .push_block(&format!("b{}", step + 3), vec![fixture_tx()]);
                    daemon.0.set_mempool(vec![]);
                    paid_at = Some(h);
                } else {
                    daemon.0.push_block(
                        &format!("b{}", step + 3),
                        vec![unrelated_tx(step as u8 + 60)],
                    );
                }
                let polls = 1 + next(60) as usize;
                // Only a tick that was cut off (`None`) is tolerated: one
                // that ran to the end must have succeeded, or a failure on
                // the interrupted path would go unseen.
                if let Some(result) = run_until_killed(
                    run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0),
                    polls,
                )
                .await
                {
                    result.unwrap_or_else(|e| panic!("seed {seed} step {step}: {e}"));
                }
            }
            for _ in 0..5 {
                run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 0)
                    .await
                    .unwrap();
            }

            let high_water = store
                .lock()
                .max_scanned_height(monero::Network::Mainnet)
                .unwrap();
            let events = store
                .lock()
                .due_webhook_deliveries_for_test(i64::MAX / 2, 10_000)
                .unwrap();
            for ((tenant_id, _), order_id) in tenants.iter().zip(&orders) {
                assert_eq!(
                    cursor_of(&store, tenant_id.as_str()),
                    high_water,
                    "seed {seed}"
                );
                let payments = store
                    .lock()
                    .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                    .unwrap();
                match paid_at {
                    Some(h) => {
                        assert_eq!(payments.len(), 1, "seed {seed}: exactly one payment");
                        assert_eq!(payments[0].block_height, Some(h as i64), "seed {seed}");
                        let status = order_status(
                            &store,
                            &shared::ids::OrderId::new(order_id.as_str().to_string()),
                        );
                        assert!(
                            events.iter().any(|d| &d.order_id == order_id && d.event_type == format!("order.{status}")),
                            "seed {seed}: the webhook for the order's current status ({status}) was enqueued"
                        );
                    }
                    None => assert!(payments.is_empty(), "seed {seed}"),
                }
            }
        }
    }

    // -- Scan window (task 7.3, decision D10) ----------------------------------

    #[tokio::test]
    async fn a_payment_to_a_recently_closed_order_is_seen_but_one_closed_before_the_grace_period_is_left_to_lookup(
    ) {
        for (closed_ago, grace, expect_scanned) in [(100, 3600, true), (7200, 3600, false)] {
            let store = Store::open_in_memory().unwrap();
            let custody = PlainKeyCustody::default();
            let (a, a_handle, a_order) =
                fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
            // Paid (by some earlier payment) and closed `closed_ago` seconds ago.
            store
                .execute_raw_for_test(&format!(
                    "UPDATE orders SET status = 'paid', closed_at_utc = {} WHERE id = '{a_order}'",
                    crate::now_unix() - closed_ago
                ))
                .unwrap();
            // Another open order keeps the store active either way.
            let index = store
                .allocate_minor_index(&shared::ids::TenantId::new(a.to_string()))
                .unwrap();
            store
                .create_order(NewOrder {
                    idempotency_key: None,
                    confirmations_required_override: None,
                    tenant_id: a.clone(),
                    merchant_order_id: None,
                    minor_index: index,
                    address: "other".into(),
                    xmr_amount_piconero: 1,
                    description: None,
                    created_at: 1000,
                    expires_at: crate::now_unix() + 3600,
                })
                .unwrap();
            let store = store.into_shared();
            let daemon = FakeDaemonClient::new();
            daemon.push_block("h1", vec![]);
            daemon.push_block("h2", vec![]);
            run_scan_tick(
                &store,
                &custody,
                &daemon,
                "mainnet",
                &[(a.clone(), a_handle)],
                20,
                grace,
            )
            .await
            .unwrap();
            daemon.push_block("h3", vec![fixture_tx()]);
            run_scan_tick(
                &store,
                &custody,
                &daemon,
                "mainnet",
                &[(a.clone(), a_handle)],
                20,
                grace,
            )
            .await
            .unwrap();

            let payments = store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
                .unwrap();
            assert_eq!(
                payments.len() == 1,
                expect_scanned,
                "closed {closed_ago}s ago, grace {grace}s"
            );
            if !expect_scanned {
                // The merchant's payment lookup scans every index the store has
                // ever issued, and records it.
                let next = store
                    .lock()
                    .get_tenant_by_id(&shared::ids::TenantId::new(a.to_string()))
                    .unwrap()
                    .unwrap()
                    .next_minor_index;
                let scan = scan_transaction(&custody, a_handle, &fixture_tx(), 0..next)
                    .await
                    .unwrap();
                record_scan_match(
                    &store.lock(),
                    &shared::ids::TenantId::new(a.to_string()),
                    &scan,
                    crate::now_unix(),
                    Some(3),
                )
                .unwrap();
                assert_eq!(
                    store
                        .lock()
                        .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
                        .unwrap()
                        .len(),
                    1
                );
            }
        }
    }

    #[tokio::test]
    async fn catch_up_after_a_gap_longer_than_the_grace_period_still_finds_a_payment_to_an_order_that_closed_since(
    ) {
        let store = Store::open_in_memory().unwrap();
        let custody = FlakyKeyCustody::default();
        let now = crate::now_unix();
        let (a, a_handle, a_order) = fixture_tenant(&store, &custody, now + 3600).await;
        let store = store.into_shared();
        let tenants = [(a.clone(), a_handle)];
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        daemon.set_block_timestamp(2, (now - 1000) as u64);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 60)
            .await
            .unwrap();

        custody.fail(a_handle);
        daemon.push_block("h3", vec![fixture_tx()]);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 60)
            .await
            .unwrap();
        assert_eq!(cursor_of(&store, a.as_str()), Some(2));
        // While it was behind, the order closed (say it was cancelled and
        // expired) 500s ago, well past a 60s grace period by now.
        store
            .lock()
            .execute_raw_for_test(&format!(
                "UPDATE orders SET status = 'expired', closed_at_utc = {} WHERE id = '{a_order}'",
                now - 500
            ))
            .unwrap();

        custody.recover(a_handle);
        run_scan_tick(&store, &custody, &daemon, "mainnet", &tenants, 20, 60)
            .await
            .unwrap();
        assert_eq!(cursor_of(&store, a.as_str()), Some(3));
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
                .unwrap()
                .len(),
            1,
            "it was open when the gap began"
        );
    }

    #[tokio::test]
    async fn a_store_with_over_a_million_orders_but_few_open_is_scanned_normally() {
        let store = Store::open_in_memory().unwrap();
        let custody = PlainKeyCustody::default();
        let (a, a_handle, a_order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        // Its index counter is far past the old 1M-entry table limit.
        store
            .execute_raw_for_test(&format!(
                "UPDATE tenants SET next_minor_index = 1200000 WHERE id = '{a}'"
            ))
            .unwrap();
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        run_scan_tick(
            &store,
            &custody,
            &daemon,
            "mainnet",
            &[(a.clone(), a_handle)],
            20,
            0,
        )
        .await
        .unwrap();
        daemon.push_block("h3", vec![fixture_tx()]);
        run_scan_tick(
            &store,
            &custody,
            &daemon,
            "mainnet",
            &[(a.clone(), a_handle)],
            20,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            cursor_of(&store, a.as_str()),
            Some(3),
            "not left behind by a scan failure"
        );
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
                .unwrap()
                .len(),
            1
        );
        let a = shared::ids::TenantId::new(a.to_string());
        assert_eq!(
            store
                .lock()
                .scan_windows(std::slice::from_ref(&a), crate::now_unix(), 0)
                .unwrap()
                .get(&a),
            Some(&vec![1]),
            "the window is the open orders"
        );
    }

    // -- Mempool memory (task 7.3) ---------------------------------------------

    #[tokio::test]
    async fn an_unchanged_mempool_is_neither_fetched_nor_scanned_again() {
        let store = Store::open_in_memory().unwrap();
        let custody = CountingKeyCustody {
            inner: PlainKeyCustody::default(),
            scan_calls: AtomicU64::new(0),
        };
        let (a, a_handle, a_order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        daemon.set_mempool(vec![fixture_tx(), unrelated_tx(1), unrelated_tx(2)]);
        let memory = crate::work::ScanState::default();
        let tenants = [(a.clone(), a_handle)];

        run_scan_tick_with(
            &memory, &store, &custody, &daemon, "mainnet", &tenants, 20, 0, 8,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
                .unwrap()
                .len(),
            1
        );
        let after_first = custody.scan_calls.load(Ordering::SeqCst);
        assert_eq!(after_first, 3);

        run_scan_tick_with(
            &memory, &store, &custody, &daemon, "mainnet", &tenants, 20, 0, 8,
        )
        .await
        .unwrap();
        assert_eq!(
            custody.scan_calls.load(Ordering::SeqCst),
            after_first,
            "nothing new, nothing scanned"
        );

        // One new transaction: scanned once.
        daemon.set_mempool(vec![
            fixture_tx(),
            unrelated_tx(1),
            unrelated_tx(2),
            unrelated_tx(3),
        ]);
        run_scan_tick_with(
            &memory, &store, &custody, &daemon, "mainnet", &tenants, 20, 0, 8,
        )
        .await
        .unwrap();
        assert_eq!(custody.scan_calls.load(Ordering::SeqCst), after_first + 1);
    }

    #[tokio::test]
    async fn a_failed_mempool_scan_is_retried_next_tick() {
        let store = Store::open_in_memory().unwrap();
        let custody = FlakyKeyCustody::default();
        let (a, a_handle, a_order) =
            fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        daemon.set_mempool(vec![fixture_tx()]);
        let memory = crate::work::ScanState::default();
        let tenants = [(a.clone(), a_handle)];

        custody.fail(a_handle);
        run_scan_tick_with(
            &memory, &store, &custody, &daemon, "mainnet", &tenants, 20, 0, 8,
        )
        .await
        .unwrap();
        assert!(store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
            .unwrap()
            .is_empty());

        custody.recover(a_handle);
        run_scan_tick_with(
            &memory, &store, &custody, &daemon, "mainnet", &tenants, 20, 0, 8,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .lock()
                .get_all_payments(&shared::ids::OrderId::new(a_order.to_string()))
                .unwrap()
                .len(),
            1,
            "not remembered as scanned after failing"
        );
    }

    /// What the node client fetches: a pruned transaction, with its id
    /// alongside. It scans to the same match, amount and key images as the
    /// whole one, under the same id.
    #[tokio::test]
    async fn a_pruned_transaction_scans_to_the_same_result_as_the_whole_one() {
        let (_store, key_custody, handle, _tenant_id, _order_id) = setup().await;
        let whole = fixture_tx();
        let mut blob = monero::consensus::encode::serialize(&whole.prefix);
        blob.extend(monero::consensus::encode::serialize(
            whole.rct_signatures.sig.as_ref().unwrap(),
        ));
        let pruned = shared::monero_tx::decode_pruned(&blob).unwrap();
        let txid = tx_id_hex(&whole);

        let from_whole = scan_transaction(&key_custody, handle, &whole, 0..3)
            .await
            .unwrap();
        let from_pruned = scan_transaction_as(&key_custody, handle, &txid, &pruned, 0..3)
            .await
            .unwrap();
        assert_eq!(from_whole.matches.len(), 1);
        assert!(from_whole.matches[0].amount_piconero.unwrap() > 0);
        assert_eq!(from_pruned.matches, from_whole.matches);
        assert_eq!(from_pruned.txid, from_whole.txid);
        assert_eq!(from_pruned.key_images_json, from_whole.key_images_json);
    }

    #[tokio::test]
    async fn a_store_whose_window_changed_has_the_pool_scanned_again_for_it() {
        let store = Store::open_in_memory().unwrap();
        let custody = CountingKeyCustody {
            inner: PlainKeyCustody::default(),
            scan_calls: AtomicU64::new(0),
        };
        let (a, a_handle, _) = fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        daemon.set_mempool(vec![unrelated_tx(1)]);
        let memory = crate::work::ScanState::default();
        let tenants = [(a.clone(), a_handle)];
        run_scan_tick_with(
            &memory, &store, &custody, &daemon, "mainnet", &tenants, 20, 0, 8,
        )
        .await
        .unwrap();
        let before = custody.scan_calls.load(Ordering::SeqCst);

        // A new order: the window changes, so the pool is checked for it.
        {
            let s = store.lock();
            let index = s
                .allocate_minor_index(&shared::ids::TenantId::new(a.to_string()))
                .unwrap();
            s.create_order(NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: a.clone(),
                merchant_order_id: None,
                minor_index: index,
                address: "new".into(),
                xmr_amount_piconero: 1,
                description: None,
                created_at: 1000,
                expires_at: crate::now_unix() + 3600,
            })
            .unwrap();
        }
        run_scan_tick_with(
            &memory, &store, &custody, &daemon, "mainnet", &tenants, 20, 0, 8,
        )
        .await
        .unwrap();
        assert_eq!(custody.scan_calls.load(Ordering::SeqCst), before + 1);
    }

    // -- Re-registering after the key-custody backend lost its wallets (5.8) ---

    #[derive(Default)]
    struct ForgetfulKeyCustody {
        inner: PlainKeyCustody,
        epoch: AtomicU64,
    }

    #[async_trait::async_trait]
    impl KeyCustody for ForgetfulKeyCustody {
        async fn register_wallet(
            &self,
            material: WalletMaterial,
        ) -> std::result::Result<WalletHandle, KeyCustodyError> {
            self.inner.register_wallet(material).await
        }
        async fn remove_wallet(
            &self,
            handle: WalletHandle,
        ) -> std::result::Result<(), KeyCustodyError> {
            self.inner.remove_wallet(handle).await
        }
        async fn seal(
            &self,
            material: &WalletMaterial,
        ) -> std::result::Result<Vec<u8>, KeyCustodyError> {
            self.inner.seal(material).await
        }
        async fn unseal_and_register(
            &self,
            sealed: &[u8],
        ) -> std::result::Result<WalletHandle, KeyCustodyError> {
            self.inner.unseal_and_register(sealed).await
        }
        async fn derive_subaddress(
            &self,
            handle: WalletHandle,
            index: SubaddressIndex,
            network: Network,
        ) -> std::result::Result<Address, KeyCustodyError> {
            self.inner.derive_subaddress(handle, index, network).await
        }
        async fn scan_tx_outputs(
            &self,
            handle: WalletHandle,
            tx: &ScanInput,
            major_range: Range<u32>,
            minor_range: Range<u32>,
        ) -> std::result::Result<Vec<MatchedOutput>, KeyCustodyError> {
            self.inner
                .scan_tx_outputs(handle, tx, major_range, minor_range)
                .await
        }
        async fn check_state(&self) -> std::result::Result<u64, KeyCustodyError> {
            Ok(self.epoch.load(Ordering::SeqCst))
        }
    }

    #[tokio::test]
    async fn when_the_backend_loses_its_wallets_every_tenant_is_registered_again_once() {
        let store = Store::open_in_memory().unwrap();
        let custody = ForgetfulKeyCustody::default();
        let sealed = custody
            .seal(&WalletMaterial::new(
                fixture_view_key(),
                fixture_spend_pubkey(),
            ))
            .await
            .unwrap();
        let mut ids = vec![];
        for _ in 0..3 {
            ids.push(
                store
                    .create_tenant(
                        NewTenant {
                            key_custody_backend: "socket".into(),
                            sealed_key_material: sealed.clone(),
                            primary_address: "4x".into(),
                            network: "mainnet".into(),
                            confirmations_required: None,
                            order_expiry_seconds: None,
                        },
                        1,
                    )
                    .unwrap()
                    .tenant
                    .id,
            );
        }
        let store = store.into_shared();
        let handles = parking_lot::RwLock::new(HashMap::new());
        let handled = AtomicU64::new(0);
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &custody,
                &handles,
                Some(&handled),
                monero::Network::Mainnet
            )
            .await,
            3
        );
        let before: Vec<WalletHandle> = ids.iter().map(|id| handles.read()[id]).collect();
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &custody,
                &handles,
                Some(&handled),
                monero::Network::Mainnet
            )
            .await,
            0
        );

        // The backend restarts and loses everything.
        custody.epoch.store(1, Ordering::SeqCst);
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &custody,
                &handles,
                Some(&handled),
                monero::Network::Mainnet
            )
            .await,
            3
        );
        for (id, old) in ids.iter().zip(before) {
            assert_ne!(handles.read()[id], old, "a fresh handle");
        }
        // A second network's loop noticing the same epoch doesn't clear again.
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &custody,
                &handles,
                Some(&handled),
                monero::Network::Stagenet
            )
            .await,
            0
        );
        assert_eq!(handles.read().len(), 3);
    }

    #[tokio::test]
    async fn stores_on_a_disabled_backend_are_left_alone_and_come_back_when_it_is_enabled_again() {
        use crate::key_custody::{CustodyRouter, PlainKeyCustody};
        use std::sync::Arc;
        let plain: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let socket: Arc<dyn KeyCustody> = Arc::new(ForgetfulKeyCustody::default());
        let both = HashMap::from([
            ("plain".to_string(), plain.clone()),
            ("socket".to_string(), socket.clone()),
        ]);
        let router = CustodyRouter::new(both.clone(), "plain");
        let material = WalletMaterial::new(fixture_view_key(), fixture_spend_pubkey());
        let store = Store::open_in_memory().unwrap();
        let mut ids = HashMap::new();
        for backend in ["plain", "socket"] {
            let sealed = router.seal_in(backend, &material).await.unwrap();
            let tenant = store
                .create_tenant(
                    NewTenant {
                        key_custody_backend: backend.into(),
                        sealed_key_material: sealed,
                        primary_address: "4x".into(),
                        network: "mainnet".into(),
                        confirmations_required: None,
                        order_expiry_seconds: None,
                    },
                    1,
                )
                .unwrap()
                .tenant;
            ids.insert(backend, tenant.id);
        }
        let store = store.into_shared();
        let handles = parking_lot::RwLock::new(HashMap::new());
        let handled = AtomicU64::new(0);
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &router,
                &handles,
                Some(&handled),
                monero::Network::Mainnet
            )
            .await,
            2
        );
        let plain_handle = handles.read()[&ids["plain"]];

        // The socket backend is turned off: its store drops out, quietly,
        // and the other store keeps its handle.
        router.replace(
            HashMap::from([("plain".to_string(), plain.clone())]),
            "plain",
        );
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &router,
                &handles,
                Some(&handled),
                monero::Network::Mainnet
            )
            .await,
            0
        );
        assert!(!handles.read().contains_key(&ids["socket"]));
        assert_eq!(handles.read()[&ids["plain"]], plain_handle);

        // Turned back on: the store is registered again from its sealed keys.
        router.replace(both, "plain");
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &router,
                &handles,
                Some(&handled),
                monero::Network::Mainnet
            )
            .await,
            1
        );
        let socket_handle = handles.read()[&ids["socket"]];
        assert!(router
            .derive_subaddress(socket_handle, SubaddressIndex::default(), Network::Mainnet)
            .await
            .is_ok());
        assert_eq!(
            handles.read()[&ids["plain"]],
            plain_handle,
            "never disturbed"
        );
    }

    #[tokio::test]
    async fn a_store_whose_backend_lost_it_or_was_replaced_is_registered_again_by_the_scan_loop_alone(
    ) {
        use crate::key_custody::{CustodyRouter, PlainKeyCustody};
        use std::sync::Arc;
        let plain: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let socket: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let router = CustodyRouter::new(
            HashMap::from([
                ("plain".to_string(), plain.clone()),
                ("socket".to_string(), socket.clone()),
            ]),
            "plain",
        );
        let material = WalletMaterial::new(fixture_view_key(), fixture_spend_pubkey());
        let store = Store::open_in_memory().unwrap();
        let tenant = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "socket".into(),
                    sealed_key_material: router.seal_in("socket", &material).await.unwrap(),
                    primary_address: "4x".into(),
                    network: "mainnet".into(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1,
            )
            .unwrap()
            .tenant;
        let store = store.into_shared();
        let handles = parking_lot::RwLock::new(HashMap::new());
        let handled = AtomicU64::new(0);
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &router,
                &handles,
                Some(&handled),
                monero::Network::Mainnet
            )
            .await,
            1
        );

        // The backend loses the store without saying so (no epoch change):
        // the next scan call finds out, and the next tick registers it again.
        let handle = handles.read()[&tenant.id];
        socket.remove_wallet(handle).await.unwrap();
        let tx = unrelated_tx(1);
        let window = ScanIndices::range(0..1);
        assert!(
            scan_transaction_in_window(&router, handle, &tx_id_hex(&tx), &tx, &window)
                .await
                .is_err()
        );
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &router,
                &handles,
                Some(&handled),
                monero::Network::Mainnet
            )
            .await,
            1
        );
        let handle = handles.read()[&tenant.id];
        assert!(
            scan_transaction_in_window(&router, handle, &tx_id_hex(&tx), &tx, &window)
                .await
                .is_ok()
        );

        // The socket backend is pointed at another server (a new instance
        // under the same name): the store is registered there on the next tick.
        let new_socket: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        router.replace(
            HashMap::from([
                ("plain".to_string(), plain),
                ("socket".to_string(), new_socket.clone()),
            ]),
            "plain",
        );
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &router,
                &handles,
                Some(&handled),
                monero::Network::Mainnet
            )
            .await,
            1
        );
        let handle = handles.read()[&tenant.id];
        assert!(new_socket
            .derive_subaddress(handle, SubaddressIndex::default(), Network::Mainnet)
            .await
            .is_ok());
        assert!(
            scan_transaction_in_window(&router, handle, &tx_id_hex(&tx), &tx, &window)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn one_networks_registration_pass_leaves_another_networks_lost_handles_for_that_network()
    {
        use crate::key_custody::{CustodyRouter, PlainKeyCustody};
        use std::sync::Arc;
        let plain: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let socket: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let router = CustodyRouter::new(
            HashMap::from([
                ("plain".to_string(), plain.clone()),
                ("socket".to_string(), socket),
            ]),
            "plain",
        );
        let material = WalletMaterial::new(fixture_view_key(), fixture_spend_pubkey());
        let store = Store::open_in_memory().unwrap();
        let mut ids = HashMap::new();
        for (network, backend) in [("mainnet", "plain"), ("stagenet", "socket")] {
            let tenant = store
                .create_tenant(
                    NewTenant {
                        key_custody_backend: backend.into(),
                        sealed_key_material: router.seal_in(backend, &material).await.unwrap(),
                        primary_address: "4x".into(),
                        network: network.into(),
                        confirmations_required: None,
                        order_expiry_seconds: None,
                    },
                    1,
                )
                .unwrap()
                .tenant;
            ids.insert(network, tenant.id);
        }
        let store = store.into_shared();
        let handles = parking_lot::RwLock::new(HashMap::new());
        for network in ["mainnet", "stagenet"] {
            assert_eq!(
                register_missing_wallets_checking_state(
                    &store,
                    &router,
                    &handles,
                    None,
                    shared::network::parse_network(network).unwrap()
                )
                .await,
                1
            );
        }

        // The stagenet store's backend is replaced; mainnet's loop runs first.
        router.replace(
            HashMap::from([
                ("plain".to_string(), plain),
                (
                    "socket".to_string(),
                    Arc::new(PlainKeyCustody::default()) as Arc<dyn KeyCustody>,
                ),
            ]),
            "plain",
        );
        let pass = register_missing_wallets_reporting(
            &crate::store::Db::over_shared(store.clone()),
            &router,
            &handles,
            None,
            monero::Network::Mainnet,
        )
        .await;
        assert_eq!(
            pass,
            Registration {
                registered: 0,
                failed: 0
            }
        );
        let stale = handles.read()[&ids["stagenet"]];
        assert!(
            !router.handle_is_live(stale),
            "still there, so stagenet's loop sees it lost and retries soon"
        );
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &router,
                &handles,
                None,
                monero::Network::Stagenet
            )
            .await,
            1
        );
        assert!(router.handle_is_live(handles.read()[&ids["stagenet"]]));
    }

    // -- Scale (task 7.12). Ignored by default: run with
    // `cargo test -p engine --release --lib scale_ -- --ignored --nocapture`.

    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn scale_many_stores_a_busy_mempool_and_a_block_with_payments_for_all_of_them() {
        let stores: usize = std::env::var("SCALE_STORES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);
        let pool_size: u8 = 200;
        let store = Store::open_in_memory().unwrap();
        let custody = PlainKeyCustody::default();
        let mut tenants = vec![];
        let mut orders = vec![];
        for _ in 0..stores {
            let (id, handle, order) =
                fixture_tenant(&store, &custody, crate::now_unix() + 3600).await;
            tenants.push((id, handle));
            orders.push(order);
        }
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        daemon.set_mempool((0..pool_size).map(unrelated_tx).collect());
        let memory = crate::work::ScanState::default();

        let started = std::time::Instant::now();
        run_scan_tick_with(
            &memory, &store, &custody, &daemon, "mainnet", &tenants, 20, 0, 8,
        )
        .await
        .unwrap();
        let cold = started.elapsed();

        let started = std::time::Instant::now();
        run_scan_tick_with(
            &memory, &store, &custody, &daemon, "mainnet", &tenants, 20, 0, 8,
        )
        .await
        .unwrap();
        let warm = started.elapsed();

        daemon.push_block("h3", vec![fixture_tx(), unrelated_tx(250)]);
        let started = std::time::Instant::now();
        run_scan_tick_with(
            &memory, &store, &custody, &daemon, "mainnet", &tenants, 20, 0, 8,
        )
        .await
        .unwrap();
        let block = started.elapsed();

        let paid = orders
            .iter()
            .filter(|o| store.lock().get_all_payments(o).unwrap().len() == 1)
            .count();
        println!(
            "scale: {stores} stores, {pool_size}-tx mempool: cold tick {cold:?}, warm tick {warm:?}, \
             block with a payment for every store {block:?}, {paid}/{stores} paid"
        );
        assert_eq!(
            paid, stores,
            "every store's payment detected in the tick after its block"
        );
        assert!(
            warm < Duration::from_secs(2),
            "an unchanged pool costs almost nothing: {warm:?}"
        );
    }

    // -- Remaining edges: coinbase inputs, undecryptable amounts, races --------

    /// A coinbase input spends nothing, so it has no key image.
    #[test]
    fn a_coinbase_input_has_no_key_image() {
        let mut tx = fixture_tx();
        tx.prefix.inputs = vec![monero::blockdata::transaction::TxIn::Gen {
            height: monero::VarInt(5),
        }];
        assert!(key_images_of(&tx).is_empty());
    }

    /// An output that matched but whose amount couldn't be decrypted is
    /// neither recorded nor staged (a zero row would be worse than none),
    /// and the skip is logged.
    #[tokio::test]
    async fn an_undecryptable_amount_is_neither_recorded_nor_staged() {
        let (_guard, logs) = crate::test_log::capture();
        let (store, _custody, _handle, tenant_id, order_id) = setup().await;
        let scan = ScanResult {
            matches: vec![MatchedOutput {
                output_index: 0,
                subaddress_index: SubaddressIndex { major: 0, minor: 1 },
                amount_piconero: None,
            }],
            txid: "ab".repeat(32),
            key_images_json: "[]".into(),
            output_keys: Default::default(),
        };
        assert!(record_scan_match(
            &store,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &scan,
            100,
            Some(5)
        )
        .unwrap()
        .is_empty());
        stage_block_match(
            &store,
            monero::Network::Mainnet,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &scan,
            100,
        )
        .unwrap();
        assert!(store
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .is_empty());
        assert!(store
            .take_staged_payments(
                monero::Network::Mainnet,
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "any"
            )
            .unwrap()
            .is_empty());
        assert_eq!(
            logs.count("its amount could not be decrypted"),
            2,
            "{}",
            logs.text()
        );
    }

    /// Every SQL statement of the "reconcile now" entry point, failed in
    /// turn: it reports the failure, and the next call finishes the job.
    #[tokio::test]
    async fn every_sql_failure_in_reconcile_now_is_recovered_from() {
        let mut faults = 0;
        for fault in 0.. {
            let (store, key_custody, handle, tenant_id, order_id) = setup().await;
            let store = store.into_shared();
            let daemon = FakeDaemonClient::new();
            daemon.push_block("h1", vec![]);
            daemon.push_block("h2", vec![fixture_tx()]);
            run_scan_tick(
                &store,
                &key_custody,
                &daemon,
                "mainnet",
                &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
                20,
                0,
            )
            .await
            .unwrap();
            run_scan_tick(
                &store,
                &key_custody,
                &daemon,
                "mainnet",
                &[(shared::ids::TenantId::new(tenant_id.clone()), handle)],
                20,
                0,
            )
            .await
            .unwrap();
            daemon.reorg_from(2, vec![("x2", vec![]), ("x3", vec![fixture_tx()])]);
            let seen = store.lock().fail_nth_access(Some(fault));
            let first =
                check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, crate::now_unix())
                    .await;
            store.lock().fail_nth_access(None);
            if seen.load(Ordering::Relaxed) <= fault {
                first.unwrap();
                break;
            }
            faults += 1;
            if first.is_err() {
                check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, crate::now_unix())
                    .await
                    .unwrap();
            }
            assert!(
                store
                    .lock()
                    .reorg_job(monero::Network::Mainnet)
                    .unwrap()
                    .is_none(),
                "fault {fault}"
            );
            assert_eq!(
                store
                    .lock()
                    .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
                    .unwrap()[0]
                    .block_height,
                Some(3),
                "fault {fault}"
            );
        }
        assert!(faults > 5, "reached {faults}");
    }

    /// A node that un-voids the payment (as the reorg path might) while the
    /// void recheck is asking about its key images: the recheck finds nothing
    /// left to restore and tells nobody twice.
    struct UnvoidsWhileAsked {
        inner: FakeDaemonClient,
        store: crate::store::SharedStore,
        order_id: String,
    }

    #[async_trait::async_trait]
    impl MoneroDaemonClient for UnvoidsWhileAsked {
        async fn get_height(&self) -> std::result::Result<u64, DaemonError> {
            self.inner.get_height().await
        }
        async fn get_block_hash(&self, height: u64) -> std::result::Result<String, DaemonError> {
            self.inner.get_block_hash(height).await
        }
        async fn get_chain_blocks(
            &self,
            start_height: u64,
            count: u64,
        ) -> std::result::Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
            self.inner.get_chain_blocks(start_height, count).await
        }
        async fn get_mempool_txids(&self) -> std::result::Result<Vec<String>, DaemonError> {
            self.inner.get_mempool_txids().await
        }
        async fn get_transactions_with_ids(
            &self,
            txids: &[String],
        ) -> std::result::Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
            self.inner.get_transactions_with_ids(txids).await
        }
        async fn locate_transaction(
            &self,
            txid: &str,
        ) -> std::result::Result<TxLocation, DaemonError> {
            self.inner.locate_transaction(txid).await
        }
        async fn is_key_image_spent(
            &self,
            key_images: &[String],
        ) -> std::result::Result<Vec<KeyImageStatus>, DaemonError> {
            {
                let s = self.store.lock();
                for payment in s
                    .get_all_payments(&shared::ids::OrderId::new(self.order_id.to_string()))
                    .unwrap()
                {
                    s.unvoid_payment(
                        &shared::ids::OrderId::new(self.order_id.to_string()),
                        &payment.txid,
                        payment.output_index,
                    )
                    .unwrap();
                }
            }
            self.inner.is_key_image_spent(key_images).await
        }
    }

    #[tokio::test]
    async fn a_void_restored_while_rechecked_is_not_restored_twice() {
        let (store, _tenant_id, order_id) = setup_with_one_voided_double_spend().await;
        let payment = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()[0]
            .clone();
        let daemon = UnvoidsWhileAsked {
            inner: chain_replica(),
            store: store.clone(),
            order_id: order_id.clone(),
        };
        let db = crate::store::Db::over_shared(store.clone());
        let restored = recheck_voided_payment(
            &db,
            &daemon,
            monero::Network::Mainnet,
            &payment,
            10,
            crate::now_unix(),
            None,
        )
        .await
        .unwrap();
        assert!(!restored, "already restored by the time it was applied");
        let events: Vec<String> = store
            .lock()
            .due_webhook_deliveries_for_test(i64::MAX / 2, 10)
            .unwrap()
            .into_iter()
            .map(|d| d.event_type)
            .collect();
        assert!(
            !events.contains(&"order.double_spend_reversed".to_string()),
            "{events:?}"
        );
    }

    /// A void from reconciliation (the transaction was in a block that lost a
    /// reorg) clears the height it was recorded at: a void that is later
    /// reversed comes back unconfirmed, followed by the vanished-payment
    /// check, not counting confirmations from a block that is on no chain.
    #[tokio::test]
    async fn a_voided_payment_keeps_no_height_from_the_block_it_lost() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let txid = tx_id_hex(&tx);
        let order = shared::ids::OrderId::new(order_id.to_string());
        scan_transaction_for_tenant(
            &store,
            &key_custody,
            handle,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &tx,
            0..3,
            1500,
            Some(50),
        )
        .await
        .unwrap();
        let payment = store.get_all_payments(&order).unwrap()[0].clone();
        assert_eq!(payment.block_height, Some(50));
        store
            .in_transaction(|s| {
                void_and_notify_in_tx(s, &order, &txid, payment.output_index, 60, 1600)
            })
            .unwrap();
        let voided = &store.get_all_payments(&order).unwrap()[0];
        assert!(voided.voided_at.is_some());
        assert_eq!(
            voided.block_height, None,
            "in no block: voided on that evidence"
        );

        // Reversed on fresh evidence (every key image unspent, the transaction
        // nowhere): unconfirmed, so the vanished-payment check follows it.
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        let db = crate::store::Db::over_shared(store.into_shared());
        let restored = recheck_voided_payment(
            &db,
            &daemon,
            monero::Network::Mainnet,
            voided,
            60,
            crate::now_unix(),
            None,
        )
        .await
        .unwrap();
        assert!(restored);
        let payment = db
            .run(crate::store::db::Class::Scanner, {
                let order = order.clone();
                move |s| Ok::<_, ScannerError>(s.get_all_payments(&order)?[0].clone())
            })
            .await
            .unwrap();
        assert_eq!((payment.voided_at, payment.block_height), (None, None));
    }

    /// A void was a false accusation and the accused transaction is mined
    /// after all: its own inputs are now spent (by it), so only the
    /// transaction's whereabouts can clear it. Restored, at its block.
    #[tokio::test]
    async fn a_void_whose_transaction_is_mined_after_all_is_restored_at_its_block() {
        let (store, _tenant_id, order_id) = setup_with_one_voided_double_spend().await;
        let order = shared::ids::OrderId::new(order_id.to_string());
        let payment = store.lock().get_all_payments(&order).unwrap()[0].clone();
        let daemon = chain_replica();
        daemon.push_block("h3", vec![fixture_tx()]);
        // Spent, by the transaction itself: the key-image test alone would
        // leave the payment voided forever.
        for ki in &key_images_of(&fixture_tx()) {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        let db = crate::store::Db::over_shared(store.clone());
        let restored = recheck_voided_payment(
            &db,
            &daemon,
            monero::Network::Mainnet,
            &payment,
            10,
            crate::now_unix(),
            None,
        )
        .await
        .unwrap();
        assert!(restored);
        let payment = &store.lock().get_all_payments(&order).unwrap()[0];
        assert_eq!((payment.voided_at, payment.block_height), (None, Some(3)));
        let events: Vec<String> = store
            .lock()
            .due_webhook_deliveries_for_test(i64::MAX / 2, 10)
            .unwrap()
            .into_iter()
            .map(|d| d.event_type)
            .collect();
        assert!(
            events.contains(&"order.double_spend_reversed".to_string()),
            "{events:?}"
        );
    }

    // -- Key registration's failure paths -------------------------------------

    /// How a [`ScriptedCustody`] call behaves.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Behaviour {
        Answer,
        Fail,
        Hang,
    }

    /// Plain key custody whose state check and registration a test scripts;
    /// `on_register` runs before each registration answers.
    struct ScriptedCustody {
        inner: PlainKeyCustody,
        check: Behaviour,
        register: Behaviour,
        on_register: Box<dyn Fn(&str) + Send + Sync>,
    }

    impl ScriptedCustody {
        fn new(check: Behaviour, register: Behaviour) -> Self {
            Self {
                inner: PlainKeyCustody::default(),
                check,
                register,
                on_register: Box::new(|_| {}),
            }
        }

        async fn behave(behaviour: Behaviour) -> std::result::Result<(), KeyCustodyError> {
            match behaviour {
                Behaviour::Answer => Ok(()),
                Behaviour::Fail => Err(KeyCustodyError::BackendUnavailable(
                    "scripted failure".into(),
                )),
                Behaviour::Hang => std::future::pending().await,
            }
        }
    }

    #[async_trait::async_trait]
    impl KeyCustody for ScriptedCustody {
        async fn register_wallet(
            &self,
            material: WalletMaterial,
        ) -> std::result::Result<WalletHandle, KeyCustodyError> {
            self.inner.register_wallet(material).await
        }
        async fn remove_wallet(
            &self,
            handle: WalletHandle,
        ) -> std::result::Result<(), KeyCustodyError> {
            self.inner.remove_wallet(handle).await
        }
        async fn seal(
            &self,
            material: &WalletMaterial,
        ) -> std::result::Result<Vec<u8>, KeyCustodyError> {
            self.inner.seal(material).await
        }
        async fn unseal_and_register(
            &self,
            sealed: &[u8],
        ) -> std::result::Result<WalletHandle, KeyCustodyError> {
            self.inner.unseal_and_register(sealed).await
        }
        async fn unseal_and_register_in_idempotent(
            &self,
            _backend: &str,
            sealed: &[u8],
            registration_id: &str,
        ) -> std::result::Result<WalletHandle, KeyCustodyError> {
            Self::behave(self.register).await?;
            (self.on_register)(registration_id);
            self.inner.unseal_and_register(sealed).await
        }
        async fn derive_subaddress(
            &self,
            handle: WalletHandle,
            index: SubaddressIndex,
            network: Network,
        ) -> std::result::Result<Address, KeyCustodyError> {
            self.inner.derive_subaddress(handle, index, network).await
        }
        async fn scan_tx_outputs(
            &self,
            handle: WalletHandle,
            tx: &ScanInput,
            major_range: Range<u32>,
            minor_range: Range<u32>,
        ) -> std::result::Result<Vec<MatchedOutput>, KeyCustodyError> {
            self.inner
                .scan_tx_outputs(handle, tx, major_range, minor_range)
                .await
        }
        async fn check_state(&self) -> std::result::Result<u64, KeyCustodyError> {
            Self::behave(self.check).await?;
            Ok(0)
        }
    }

    /// `count` stores on mainnet with sealed keys `custody` can open.
    async fn stores_with_keys(
        custody: &dyn KeyCustody,
        count: usize,
    ) -> (crate::store::SharedStore, Vec<crate::store::TenantId>) {
        let store = Store::open_in_memory().unwrap();
        let sealed = custody
            .seal(&WalletMaterial::new(
                fixture_view_key(),
                fixture_spend_pubkey(),
            ))
            .await
            .unwrap();
        let ids = (0..count)
            .map(|_| {
                store
                    .create_tenant(
                        NewTenant {
                            key_custody_backend: "plain".into(),
                            sealed_key_material: sealed.clone(),
                            primary_address: "4x".into(),
                            network: "mainnet".into(),
                            confirmations_required: None,
                            order_expiry_seconds: None,
                        },
                        1,
                    )
                    .unwrap()
                    .tenant
                    .id
            })
            .collect();
        (store.into_shared(), ids)
    }

    /// A state check that fails or never answers is logged and passed over:
    /// the stores are still registered.
    #[tokio::test(start_paused = true)]
    async fn registration_carries_on_when_the_state_check_fails_or_hangs() {
        for check in [Behaviour::Fail, Behaviour::Hang] {
            let (_guard, logs) = crate::test_log::capture();
            let custody = ScriptedCustody::new(check, Behaviour::Answer);
            let (store, _) = stores_with_keys(&custody, 2).await;
            let handles = parking_lot::RwLock::new(HashMap::new());
            let epoch = AtomicU64::new(0);
            let db = crate::store::Db::over_shared(store.clone());
            let pass = register_missing_wallets_reporting(
                &db,
                &custody,
                &handles,
                Some(&epoch),
                monero::Network::Mainnet,
            )
            .await;
            assert_eq!(
                pass,
                Registration {
                    registered: 2,
                    failed: 0
                }
            );
            let message = if check == Behaviour::Fail {
                "state failed"
            } else {
                "state exceeded"
            };
            assert_eq!(logs.count(message), 1, "{}", logs.text());
        }
    }

    /// Stores that can't be listed are one failure, retried soon.
    #[tokio::test]
    async fn registration_that_cannot_list_stores_is_retried() {
        let (_guard, logs) = crate::test_log::capture();
        let custody = ScriptedCustody::new(Behaviour::Answer, Behaviour::Answer);
        let (store, _) = stores_with_keys(&custody, 1).await;
        let handles = parking_lot::RwLock::new(HashMap::new());
        let db = crate::store::Db::over_shared(store.clone());
        store.lock().fail_nth_access(Some(0));
        let pass = register_missing_wallets_reporting(
            &db,
            &custody,
            &handles,
            None,
            monero::Network::Mainnet,
        )
        .await;
        store.lock().fail_nth_access(None);
        assert_eq!(
            pass,
            Registration {
                registered: 0,
                failed: 1
            }
        );
        assert_eq!(
            logs.count("listing stores to register their keys failed"),
            1,
            "{}",
            logs.text()
        );
    }

    /// Registrations that fail or hang are counted and logged once for the
    /// pass (the first failure shown); a pass that runs out of time leaves
    /// the rest for the next.
    #[tokio::test(start_paused = true)]
    async fn failed_registrations_are_counted_and_logged_once() {
        registrations_fail(Behaviour::Fail).await;
    }

    #[tokio::test(start_paused = true)]
    async fn hung_registrations_are_counted_and_the_pass_is_bounded() {
        registrations_fail(Behaviour::Hang).await;
    }

    async fn registrations_fail(register: Behaviour) {
        {
            let (_guard, logs) = crate::test_log::capture();
            let custody = ScriptedCustody::new(Behaviour::Answer, register);
            let (store, _) = stores_with_keys(&custody, 3).await;
            let handles = parking_lot::RwLock::new(HashMap::new());
            let db = crate::store::Db::over_shared(store.clone());
            let started = tokio::time::Instant::now();
            let pass = register_missing_wallets_reporting(
                &db,
                &custody,
                &handles,
                None,
                monero::Network::Mainnet,
            )
            .await;
            assert_eq!(
                pass,
                Registration {
                    registered: 0,
                    failed: 3
                }
            );
            assert_eq!(
                logs.count("registering the keys of stores failed"),
                1,
                "{}",
                logs.text()
            );
            if register == Behaviour::Hang {
                assert_eq!(
                    started.elapsed(),
                    Duration::from_secs(20),
                    "two calls to their deadline, then the pass's end"
                );
            }
        }
    }

    /// A store registered by someone else (an API call) while this pass was
    /// registering it keeps the first handle; this pass's is removed again.
    #[tokio::test]
    async fn a_registration_that_loses_the_race_is_removed() {
        let mut custody = ScriptedCustody::new(Behaviour::Answer, Behaviour::Answer);
        let (store, ids) = stores_with_keys(&custody, 1).await;
        let handles = std::sync::Arc::new(parking_lot::RwLock::new(HashMap::new()));
        let sealed = store
            .lock()
            .get_tenant_by_id(&shared::ids::TenantId::new(ids[0].to_string()))
            .unwrap()
            .unwrap()
            .sealed_key_material;
        let earlier = custody.inner.unseal_and_register(&sealed).await.unwrap();
        let racing = handles.clone();
        custody.on_register = Box::new(move |id| {
            racing
                .write()
                .insert(crate::store::TenantId::new(id), earlier);
        });
        let db = crate::store::Db::over_shared(store.clone());
        let pass = register_missing_wallets_reporting(
            &db,
            &custody,
            &handles,
            None,
            monero::Network::Mainnet,
        )
        .await;
        assert_eq!(
            pass,
            Registration {
                registered: 0,
                failed: 0
            }
        );
        assert_eq!(handles.read()[&ids[0]], earlier, "the first handle in wins");
    }

    /// A match against an index no order has is nothing to stage; a staging
    /// write that fails is an error the caller's transaction rolls back.
    #[tokio::test]
    async fn staging_skips_unknown_indices_and_reports_write_failures() {
        let (store, _custody, _handle, tenant_id, order_id) = setup().await;
        let matched = |minor| ScanResult {
            matches: vec![MatchedOutput {
                output_index: 0,
                subaddress_index: SubaddressIndex { major: 0, minor },
                amount_piconero: Some(5),
            }],
            txid: "cd".repeat(32),
            key_images_json: "[]".into(),
            output_keys: Default::default(),
        };
        stage_block_match(
            &store,
            monero::Network::Mainnet,
            &shared::ids::TenantId::new(tenant_id.to_string()),
            &matched(99),
            100,
        )
        .unwrap();
        let checkpoint = crate::store::BlockCheckpoint {
            height: 7,
            hash: "h7".into(),
            next_tx: 1,
        };
        store
            .save_block_checkpoint(
                monero::Network::Mainnet,
                &shared::ids::TenantId::new(tenant_id.to_string()),
                &checkpoint,
            )
            .unwrap();
        assert!(
            store
                .take_staged_payments(
                    monero::Network::Mainnet,
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    "h7"
                )
                .unwrap()
                .is_empty(),
            "nothing staged for index 99"
        );
        let mut failed = 0;
        for fault in 0.. {
            store
                .save_block_checkpoint(
                    monero::Network::Mainnet,
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &checkpoint,
                )
                .unwrap();
            let seen = store.fail_nth_access(Some(fault));
            let result = store.in_transaction(|s| {
                stage_block_match(
                    s,
                    monero::Network::Mainnet,
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &matched(1),
                    100,
                )
            });
            store.fail_nth_access(None);
            if seen.load(Ordering::Relaxed) <= fault {
                result.unwrap();
                break;
            }
            assert!(result.is_err());
            failed += 1;
            assert!(
                store
                    .take_staged_payments(
                        monero::Network::Mainnet,
                        &shared::ids::TenantId::new(tenant_id.to_string()),
                        "h7"
                    )
                    .unwrap()
                    .is_empty(),
                "fault {fault}: rolled back"
            );
        }
        assert!(failed >= 2);
        let staged = store
            .take_staged_payments(
                monero::Network::Mainnet,
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "h7",
            )
            .unwrap();
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].order_id, shared::ids::OrderId::new(order_id));
    }

    /// A node that answers for fewer key images than it was asked about has
    /// said nothing conclusive: the void stays.
    struct ShortAnswers(FakeDaemonClient);

    #[async_trait::async_trait]
    impl MoneroDaemonClient for ShortAnswers {
        async fn get_height(&self) -> std::result::Result<u64, DaemonError> {
            self.0.get_height().await
        }
        async fn get_block_hash(&self, height: u64) -> std::result::Result<String, DaemonError> {
            self.0.get_block_hash(height).await
        }
        async fn get_chain_blocks(
            &self,
            start_height: u64,
            count: u64,
        ) -> std::result::Result<Vec<crate::daemon::ChainBlock>, DaemonError> {
            self.0.get_chain_blocks(start_height, count).await
        }
        async fn get_mempool_txids(&self) -> std::result::Result<Vec<String>, DaemonError> {
            self.0.get_mempool_txids().await
        }
        async fn get_transactions_with_ids(
            &self,
            txids: &[String],
        ) -> std::result::Result<Vec<crate::daemon::FetchedTx>, DaemonError> {
            self.0.get_transactions_with_ids(txids).await
        }
        async fn locate_transaction(
            &self,
            txid: &str,
        ) -> std::result::Result<TxLocation, DaemonError> {
            self.0.locate_transaction(txid).await
        }
        async fn is_key_image_spent(
            &self,
            _: &[String],
        ) -> std::result::Result<Vec<KeyImageStatus>, DaemonError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn short_key_image_answers_leave_a_void_alone() {
        let (store, _tenant_id, order_id) = setup_with_one_voided_double_spend().await;
        let payment = store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()[0]
            .clone();
        let db = crate::store::Db::over_shared(store.clone());
        let restored = recheck_voided_payment(
            &db,
            &ShortAnswers(chain_replica()),
            monero::Network::Mainnet,
            &payment,
            10,
            crate::now_unix(),
            None,
        )
        .await
        .unwrap();
        assert!(!restored);
        assert!(store
            .lock()
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()[0]
            .voided_at
            .is_some());
    }

    /// With several backends, one that lost its wallets costs only its own
    /// stores their handles: the others keep theirs.
    #[tokio::test]
    async fn a_backend_losing_its_wallets_behind_a_router_costs_only_its_stores() {
        use crate::key_custody::CustodyRouter;
        use std::sync::Arc;
        let plain: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let forgetful = Arc::new(ForgetfulKeyCustody::default());
        let socket: Arc<dyn KeyCustody> = forgetful.clone();
        let router = CustodyRouter::new(
            HashMap::from([("plain".to_string(), plain), ("socket".to_string(), socket)]),
            "plain",
        );
        let material = WalletMaterial::new(fixture_view_key(), fixture_spend_pubkey());
        let store = Store::open_in_memory().unwrap();
        let mut ids = HashMap::new();
        for backend in ["plain", "socket"] {
            let tenant = store
                .create_tenant(
                    NewTenant {
                        key_custody_backend: backend.into(),
                        sealed_key_material: router.seal_in(backend, &material).await.unwrap(),
                        primary_address: "4x".into(),
                        network: "mainnet".into(),
                        confirmations_required: None,
                        order_expiry_seconds: None,
                    },
                    1,
                )
                .unwrap()
                .tenant;
            ids.insert(backend, tenant.id);
        }
        let store = store.into_shared();
        let handles = parking_lot::RwLock::new(HashMap::new());
        let handled = AtomicU64::new(0);
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &router,
                &handles,
                Some(&handled),
                monero::Network::Mainnet
            )
            .await,
            2
        );
        let (plain_handle, socket_handle) = (
            handles.read()[&ids["plain"]],
            handles.read()[&ids["socket"]],
        );

        forgetful.epoch.store(1, Ordering::SeqCst);
        assert_eq!(
            register_missing_wallets_checking_state(
                &store,
                &router,
                &handles,
                Some(&handled),
                monero::Network::Mainnet
            )
            .await,
            1
        );
        assert_eq!(handles.read()[&ids["plain"]], plain_handle, "untouched");
        assert_ne!(
            handles.read()[&ids["socket"]],
            socket_handle,
            "registered again"
        );
        assert_eq!(handled.load(Ordering::SeqCst), 1);
    }

    /// The test and tool entry points refuse a network name they don't know.
    #[tokio::test]
    async fn the_entry_points_refuse_an_unknown_network() {
        let store = Store::open_in_memory().unwrap().into_shared();
        let daemon = FakeDaemonClient::new();
        let custody = PlainKeyCustody::default();
        assert!(matches!(
            run_scan_tick(&store, &custody, &daemon, "moonnet", &[], 20, 0).await,
            Err(ScannerError::Internal(_))
        ));
        assert!(matches!(
            check_for_reorg_and_reconcile(&store, &daemon, "moonnet", 20, 0).await,
            Err(ScannerError::Internal(_))
        ));
    }
}

#[cfg(test)]
mod paging_tests {
    use super::*;

    /// A block goes to pages when it would overrun one answer, or take the
    /// link over 30 seconds; one whose weight isn't known is fetched whole.
    #[test]
    fn a_block_is_paged_when_too_large_for_an_answer_or_the_link() {
        let cap = 32_000_000;
        assert!(!scan_in_pages(None, cap, Some(1.0)));
        assert!(!scan_in_pages(Some(300_000), cap, None));
        assert!(scan_in_pages(Some(200_000_000), cap, None), "over the cap");
        assert!(
            scan_in_pages(Some(4_000_000), cap, Some(100_000.0)),
            "40 s at 100 kB/s"
        );
        assert!(
            !scan_in_pages(Some(2_000_000), cap, Some(100_000.0)),
            "20 s"
        );
    }

    /// A page is the fewest of what fits the cap, what the link sends in a
    /// target call and what the scan gets through in the round's share,
    /// from 1 to 100 transactions.
    #[test]
    fn a_page_is_sized_by_memory_link_and_cpu() {
        let page = |cap, rate, avg, cpu, remaining| next_page(cap, rate, avg, cpu, 4.0, remaining);
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
            page(32_000_000, Some(10_000.0), 2_000.0, None, 10_000),
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
