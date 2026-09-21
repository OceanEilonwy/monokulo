//! Chain scanning: matching transactions against active tenants, and reorg /
//! double-spend detection. See `docs/DESIGN.md` §7.
//!
//! Deliberately built against the `MoneroDaemonClient` trait rather than a live
//! node, and against `Store` directly rather than the (not-yet-built) writer-actor
//! wrapper - the correctness of the reconciliation logic doesn't depend on which
//! thread runs it, only on doing the right thing with what the daemon reports.

use std::collections::HashSet;
use std::ops::Range;

use monero::cryptonote::hash::Hashable;
use monero::Transaction;

use crate::daemon::{DaemonError, KeyImageStatus, MoneroDaemonClient, TxLocation};
use crate::key_custody::{KeyCustody, KeyCustodyError, WalletHandle};
use crate::store::{SharedStore, Store, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum ScannerError {
    #[error(transparent)]
    Daemon(#[from] DaemonError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    KeyCustody(#[from] KeyCustodyError),
}

type Result<T> = std::result::Result<T, ScannerError>;

pub fn tx_id_hex(tx: &Transaction) -> String {
    hex::encode(tx.hash().to_bytes())
}

fn key_images_of(tx: &Transaction) -> Vec<String> {
    tx.prefix
        .inputs
        .iter()
        .filter_map(|input| match input {
            monero::blockdata::transaction::TxIn::ToKey { k_image, .. } => {
                Some(hex::encode(k_image.image.to_bytes()))
            }
            monero::blockdata::transaction::TxIn::Gen { .. } => None,
        })
        .collect()
}

/// The result of scanning one transaction against one wallet - pure `KeyCustody`
/// output, no `Store` involved. Deliberately separate from persisting it (see
/// `record_scan_match`): `rusqlite::Connection` is `Send` but not `Sync`, so a
/// `&Store` reference held across an `.await` (as combining these two steps into
/// one async function would require, since `KeyCustody::scan_tx_outputs` awaits)
/// makes the containing future `!Send` - fine for a future only ever `.await`ed
/// directly inside another task (every test in this file does that), but fatal the
/// moment anything containing it is handed to `tokio::spawn`, which requires the
/// whole future to be `Send + 'static`. `scanner::run_scan_tick` is spawned in
/// production (`main.rs`), so this split isn't a style preference - the combined
/// version simply cannot be spawned. Caught by the compiler, not a test, the first
/// time this code was actually wired into a spawned task rather than awaited
/// directly in a test.
pub struct ScanResult {
    pub matches: Vec<crate::key_custody::MatchedOutput>,
    pub txid: String,
    pub key_images_json: String,
}

pub async fn scan_transaction(
    key_custody: &dyn KeyCustody,
    handle: WalletHandle,
    tx: &Transaction,
    minor_range: Range<u32>,
) -> Result<ScanResult> {
    let matches = key_custody.scan_tx_outputs(handle, tx, 0..1, minor_range).await?;
    Ok(ScanResult {
        matches,
        txid: tx_id_hex(tx),
        key_images_json: serde_json::to_string(&key_images_of(tx)).unwrap(),
    })
}

/// Persists a `ScanResult` against one tenant. Purely synchronous - no `.await`
/// anywhere in this function, so a `&Store` parameter here is never an issue.
/// Returns the set of order ids touched, so the caller knows which orders need
/// `Store::recompute_order_status`.
pub fn record_scan_match(
    store: &Store,
    tenant_id: &str,
    scan: &ScanResult,
    seen_at: i64,
    block_height: Option<u64>,
) -> Result<HashSet<String>> {
    let mut touched = HashSet::new();
    for m in &scan.matches {
        let Some(order) = store.find_order_by_minor_index(tenant_id, m.subaddress_index.minor)? else {
            continue; // a match against an index with no order row is not this scanner's problem to solve
        };
        // An output whose amount couldn't be decrypted is skipped, not recorded as
        // zero. A present-but-zero row is strictly worse than no row: it contributes
        // nothing to the received total while still dragging `min_confirmations` and
        // `all_zero_conf` around in the status derivation (see status.rs's
        // `a_zeroed_but_present_row_is_not_a_safe_substitute_for_exclusion`), and it
        // can never be cleaned up afterwards - voiding requires affirmative
        // double-spend proof, which will never arrive for a perfectly valid output.
        // Skipping leaves the tick free to record it properly once the amount is
        // recoverable.
        let Some(amount) = m.amount_piconero else {
            eprintln!(
                "scanner: output {} of tx {} matched order {} but its amount could not be decrypted - not recording it",
                m.output_index, scan.txid, order.id
            );
            continue;
        };
        store.record_payment_match(
            &order.id,
            &scan.txid,
            m.output_index as i64,
            amount,
            &scan.key_images_json,
            seen_at,
            block_height.map(|h| h as i64),
        )?;
        touched.insert(order.id);
    }
    Ok(touched)
}

/// Convenience wrapper combining `scan_transaction` + `record_scan_match`, for
/// callers that `.await` it directly within their own task rather than spawning it
/// (every test in this file does exactly that, so the `!Send` issue never bites
/// them). `run_scan_tick` (spawned in production) deliberately does not use this;
/// it calls the two halves separately instead.
#[allow(clippy::too_many_arguments)] // one scan call's genuinely-independent inputs; a params struct would just move the ceremony, not remove it
pub async fn scan_transaction_for_tenant(
    store: &Store,
    key_custody: &dyn KeyCustody,
    handle: WalletHandle,
    tenant_id: &str,
    tx: &Transaction,
    minor_range: Range<u32>,
    seen_at: i64,
    block_height: Option<u64>,
) -> Result<HashSet<String>> {
    let scan = scan_transaction(key_custody, handle, tx, minor_range).await?;
    record_scan_match(store, tenant_id, &scan, seen_at, block_height)
}

pub struct ReconcileReport {
    /// Lowest height at which the canonical chain diverged from what was
    /// previously scanned, if any reorg was detected this call.
    pub reorg_detected_at: Option<u64>,
    /// Orders whose payments changed in a way that warrants a status recompute.
    pub dirty_orders: Vec<String>,
    /// The subset of `dirty_orders` where the change was specifically a proven
    /// double-spend (a payment voided because its key image was confirmed spent by
    /// a different transaction) - callers use this to enqueue the independent
    /// `order.double_spend_detected` webhook event (see `docs/DESIGN.md` §11),
    /// separate from whatever `order.<status>` event the recompute may also imply.
    pub double_spent_orders: Vec<String>,
}

/// Checks for a reorg within the last `reorg_check_depth` blocks and, if one is
/// found, re-evaluates every payment whose recorded `block_height` falls in the
/// affected range. Never voids a payment on ambiguous evidence (§DESIGN.md 7.5) -
/// only when `is_key_image_spent` affirmatively proves a different transaction
/// consumed the same inputs. Safe to call unconditionally on every scan tick: with
/// no reorg, this is just a handful of hash comparisons against the stored window.
///
/// Takes `&SharedStore`, locking only around each brief synchronous call and never
/// across a daemon RPC `.await` - this function can make many sequential network
/// calls (a hash check per block in the window, then a `locate_transaction` and
/// possibly an `is_key_image_spent` per affected payment), so holding the store
/// lock for its whole duration would block every other request touching the store
/// for as long as all of that network I/O takes. Same reasoning as
/// `webhook_delivery::run_delivery_tick`.
pub async fn check_for_reorg_and_reconcile(
    store: &crate::store::SharedStore,
    daemon: &dyn MoneroDaemonClient,
    network: &str,
    reorg_check_depth: u64,
    now: i64,
) -> Result<ReconcileReport> {
    let height = daemon.get_height().await?;
    let window_start = height.saturating_sub(reorg_check_depth);

    // Detection is read-only against the store: the corrected hashes are *not*
    // written back here. Writing them mid-detection (as this loop used to) makes the
    // reorg undetectable to any later attempt - stored and actual hashes now agree -
    // while the payments it affects have not been reconciled yet. A crash, or any
    // `?` further down (a `locate_transaction` against a briefly-unreachable node is
    // enough), then left those payments permanently stranded on the old chain with
    // nothing to tell the next tick anything had happened.
    let mut reorg_point: Option<u64> = None;
    for h in window_start..=height {
        let stored_hash = store.lock().unwrap().get_scanned_block_hash(network, h)?;
        if let Some(stored_hash) = stored_hash {
            let actual_hash = daemon.get_block_hash(h).await?;
            if actual_hash != stored_hash {
                reorg_point = Some(h);
                break; // the first divergence is the reorg point; everything above it is re-scanned wholesale
            }
        }
    }

    let mut dirty_orders = HashSet::new();
    let mut double_spent_orders = HashSet::new();

    if let Some(reorg_point) = reorg_point {
        let affected = store.lock().unwrap().find_payments_at_or_after_height(network, reorg_point)?;
        for payment in affected {
            match daemon.locate_transaction(&payment.txid).await? {
                TxLocation::InBlock(new_height) => {
                    store.lock().unwrap().update_payment_block_height(
                        &payment.order_id,
                        &payment.txid,
                        payment.output_index,
                        Some(new_height as i64),
                    )?;
                    dirty_orders.insert(payment.order_id.clone());
                }
                TxLocation::InPool => {
                    store.lock().unwrap().update_payment_block_height(
                        &payment.order_id,
                        &payment.txid,
                        payment.output_index,
                        None,
                    )?;
                    dirty_orders.insert(payment.order_id.clone());
                }
                TxLocation::NotFound => {
                    if void_if_double_spend_proven(store, daemon, &payment, height, now).await? {
                        dirty_orders.insert(payment.order_id.clone());
                        double_spent_orders.insert(payment.order_id.clone());
                    }
                }
            }
        }

        // The reverse direction: a payment an earlier pass voided as a proven
        // double-spend, whose transaction has since come back to the canonical chain
        // because the *replacement* was itself reorged out. Voiding is a conclusion
        // drawn from a chain state that can change, so it can't be treated as final;
        // without this, a merchant's genuinely-paid order stays permanently short by
        // the voided amount. `double_spend_detected_at` stays set regardless - it
        // records that an incident occurred, not that it is currently in effect.
        let previously_voided = store.lock().unwrap().find_voided_payments_at_or_after_height(network, reorg_point)?;
        for payment in previously_voided {
            if let TxLocation::InBlock(new_height) = daemon.locate_transaction(&payment.txid).await? {
                let s = store.lock().unwrap();
                if s.unvoid_payment(&payment.order_id, &payment.txid, payment.output_index)? {
                    s.update_payment_block_height(
                        &payment.order_id,
                        &payment.txid,
                        payment.output_index,
                        Some(new_height as i64),
                    )?;
                    dirty_orders.insert(payment.order_id.clone());
                }
            }
        }
    }

    {
        let s = store.lock().unwrap();
        // The notifying recompute, not the bare one: a reorg-driven transition
        // (`paid` -> `confirming` when a tx falls back to the mempool, say) is
        // exactly as webhook-worthy as a forward-scan-driven one, and a merchant
        // discovering by polling that an order silently stopped being paid is the
        // worst possible way to learn about it.
        for order_id in &dirty_orders {
            recompute_and_notify(&s, order_id, height, now)?;
        }
    }

    // Only now that every affected payment has been re-evaluated is it safe to let
    // the stored view of the chain catch up. Dropping the rows at and above the
    // reorg point (rather than overwriting them with the new hashes) does double
    // duty: the next detection pass sees nothing to re-reconcile, *and*
    // `max_scanned_height` falls back to `reorg_point - 1` so the next tick's
    // forward scan re-covers `reorg_point..tip` against the replacement chain,
    // picking up any payment that exists only there. Should this tick die before
    // reaching this line, nothing has been written, and the next tick simply detects
    // the same reorg again and redoes the work - reconciliation is idempotent.
    if let Some(reorg_point) = reorg_point {
        // ...with one thing to be careful about: "walks back to `reorg_point - 1`"
        // is only true while a row still exists at or below that height. When the
        // reorg point is the *lowest* block this network has a row for - routine on a
        // recently-started scanner, whose window begins at the tip it bootstrapped
        // from - the delete empties the table outright, and an empty table is exactly
        // what `run_scan_tick` reads as "this network has never been scanned", which
        // makes it re-seed at the current tip. Every replacement block between the
        // reorg point and the tip would then be skipped forever, taking any payment
        // that exists only in the winning chain with it - the precise failure
        // dropping these rows was introduced to prevent.
        //
        // Re-anchoring the common ancestor fixes the high-water mark in place. Its
        // hash is fetched before the lock is taken, since nothing may `.await` while
        // holding the store mutex; one extra RPC per detected reorg is not worth
        // conditionalising.
        //
        // The anchor is fetched *before* anything is deleted, and a failure to fetch
        // it abandons the delete entirely rather than proceeding without it. Treating
        // a failed lookup as "no anchor" (which an `.ok()` here quietly did) reaches
        // precisely the outcome the paragraph above exists to prevent, just via a
        // transient RPC error instead of a missing row: the rows are gone, the table
        // can now be empty, and the next tick reads that as "never scanned" and
        // re-seeds at the tip - skipping every replacement block between the reorg
        // point and the tip, permanently and silently. Leaving the stored hashes
        // untouched instead costs nothing: they still describe the losing chain, so
        // the next tick detects the same reorg and redoes this whole step, exactly
        // as it does when reconciliation itself fails partway.
        let anchor = reorg_point.checked_sub(1);
        let anchor_hash = match anchor {
            Some(h) => match daemon.get_block_hash(h).await {
                Ok(hash) => Some(hash),
                Err(e) => {
                    eprintln!(
                        "reorg at {reorg_point} on {network}: could not read the hash of the common ancestor \
                         at {h} ({e}) - leaving the scanned-block window untouched so the next tick re-detects \
                         this reorg, rather than dropping rows this tick can no longer re-anchor"
                    );
                    return Ok(ReconcileReport {
                        reorg_detected_at: Some(reorg_point),
                        dirty_orders: dirty_orders.into_iter().collect(),
                        double_spent_orders: double_spent_orders.into_iter().collect(),
                    });
                }
            },
            // A divergence at height 0 means the genesis block changed, which cannot
            // happen on any real chain. There is no ancestor to anchor to and nothing
            // to preserve, so the delete goes ahead and the next tick re-seeds.
            None => None,
        };
        let s = store.lock().unwrap();
        s.forget_scanned_blocks_at_or_above(network, reorg_point)?;
        if s.max_scanned_height(network)?.is_none() {
            if let (Some(h), Some(hash)) = (anchor, anchor_hash) {
                s.set_scanned_block(network, h, &hash)?;
            }
        }
    }

    Ok(ReconcileReport {
        reorg_detected_at: reorg_point,
        dirty_orders: dirty_orders.into_iter().collect(),
        double_spent_orders: double_spent_orders.into_iter().collect(),
    })
}

/// What one `check_vanished_mempool_payments` sweep concluded. Same two lists as
/// `ReconcileReport`, minus the reorg point (this sweep is not about the chain
/// changing shape).
pub struct VanishedPoolReport {
    /// Orders whose payments changed and therefore need a status recompute.
    pub dirty_orders: Vec<String>,
    /// The subset of `dirty_orders` where a payment was voided on affirmative
    /// double-spend proof - the `order.double_spend_detected` webhook's trigger.
    pub double_spent_orders: Vec<String>,
}

/// Re-examines every payment that is still mempool-only (`block_height IS NULL`)
/// and whose transaction is no longer in the mempool snapshot this tick polled.
///
/// This exists because reorg reconciliation cannot cover the most ordinary
/// double-spend there is. `check_for_reorg_and_reconcile` only re-examines existing
/// payments when a *stored block hash stops matching*, which is a reorg and nothing
/// else. But the textbook attack on a merchant watching the mempool involves no
/// reorg at all: broadcast transaction A to the merchant's node (the order is
/// matched at zero confirmations, and under a `zero_conf_max_piconero` ceiling
/// immediately reads as `paid`), then get transaction B, spending the same inputs,
/// mined instead. A is never mined, so no block the scanner recorded ever changes,
/// so nothing ever looked at that payment again: it sat at `block_height IS NULL`
/// forever, counting in full towards an order the customer never actually paid, with
/// no `order.double_spend_detected` webhook ever fired. The same blind spot swallows
/// the honest version of the story - a transaction that is dropped or evicted from
/// the pool (Monero has no replace-by-fee, but a transaction can still expire out of
/// the pool after `CRYPTONOTE_MEMPOOL_TX_LIVETIME`, or simply never propagate) -
/// which is why the "gone but not proven double-spent" case is deliberately left
/// re-checkable rather than resolved.
///
/// Cheap by construction: a payment whose transaction is still in the pool costs
/// nothing (the snapshot the tick already fetched answers it), and one that has just
/// been mined normally has its height set by this same tick's block scan before this
/// runs, so it isn't in the query's result set either. Only a genuinely vanished
/// transaction costs an RPC, and the evidence rule is identical to
/// `check_for_reorg_and_reconcile`'s: never void on absence, only on an affirmative
/// `SpentInBlockchain` for one of the payment's own key images.
///
/// `mempool_txids` must be a snapshot of an *actually successful* poll. A failed
/// poll must skip this sweep entirely rather than pass an empty set, which would
/// read as "every unconfirmed payment has vanished" and burn one RPC per payment
/// re-establishing that they hadn't.
pub async fn check_vanished_mempool_payments(
    store: &crate::store::SharedStore,
    daemon: &dyn MoneroDaemonClient,
    network: &str,
    mempool_txids: &HashSet<String>,
    current_height: u64,
    now: i64,
) -> Result<VanishedPoolReport> {
    let unconfirmed = store.lock().unwrap().find_unconfirmed_payments(network)?;
    let mut dirty_orders = HashSet::new();
    let mut double_spent_orders = HashSet::new();

    for payment in unconfirmed {
        if mempool_txids.contains(&payment.txid) {
            continue; // still pending in the pool - nothing has been decided about it yet
        }
        match daemon.locate_transaction(&payment.txid).await? {
            // Mined after all: the pool snapshot was taken before the block arrived,
            // or the block scan stopped short of that height this tick. Recording the
            // height here is the same write the block scan would have made, and
            // costs the payment nothing if the block scan gets there first.
            TxLocation::InBlock(new_height) => {
                store.lock().unwrap().update_payment_block_height(
                    &payment.order_id,
                    &payment.txid,
                    payment.output_index,
                    Some(new_height as i64),
                )?;
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
                if void_if_double_spend_proven(store, daemon, &payment, current_height, now).await? {
                    dirty_orders.insert(payment.order_id.clone());
                    double_spent_orders.insert(payment.order_id.clone());
                }
            }
        }
    }

    Ok(VanishedPoolReport {
        dirty_orders: dirty_orders.into_iter().collect(),
        double_spent_orders: double_spent_orders.into_iter().collect(),
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
/// which the previous `{payment_id, status}` payload made indistinguishable. The
/// timestamp being inside the signed body (rather than only an unsigned header) is
/// what stops a captured delivery from being replayable against the merchant
/// indefinitely.
fn enqueue_webhook_event(store: &Store, order_id: &str, event_type: &str, payload: &serde_json::Value, now: i64) -> Result<()> {
    let Some(tenant_id) = store.get_order_tenant_id(order_id)? else { return Ok(()) };
    let webhooks: Vec<_> = store.list_webhooks(&tenant_id)?.into_iter().filter(|w| w.enabled).collect();
    if webhooks.is_empty() {
        return Ok(());
    }

    let mut envelope = payload.clone();
    let fields = envelope
        .as_object_mut()
        .expect("every webhook event payload in this module is constructed as a JSON object");
    fields.insert("event_id".into(), serde_json::json!(new_event_id()));
    fields.insert("event".into(), serde_json::json!(event_type));
    fields.insert("created_at".into(), serde_json::json!(now));
    let body = envelope.to_string();

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
pub(crate) fn recompute_and_notify(store: &Store, order_id: &str, current_height: u64, now: i64) -> Result<()> {
    store.in_transaction(|store| recompute_and_notify_in_tx(store, order_id, current_height, now))
}

/// The body of `recompute_and_notify`, minus the transaction, for callers that are
/// already inside one - `Store::in_transaction` uses `unchecked_transaction`, so
/// nesting it would fail at SQLite's "cannot start a transaction within a
/// transaction" rather than composing.
fn recompute_and_notify_in_tx(store: &Store, order_id: &str, current_height: u64, now: i64) -> Result<()> {
    let (old_status, new_status) = store.recompute_order_status(order_id, current_height, now)?;
    if old_status != new_status {
        let payload = serde_json::json!({ "payment_id": order_id, "status": new_status.as_str() });
        enqueue_webhook_event(store, order_id, &format!("order.{new_status}"), &payload, now)?;
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
async fn void_if_double_spend_proven(
    store: &crate::store::SharedStore,
    daemon: &dyn MoneroDaemonClient,
    payment: &crate::store::OrderPaymentRow,
    current_height: u64,
    now: i64,
) -> Result<bool> {
    let key_images: Vec<String> = serde_json::from_str(&payment.key_images_json).unwrap_or_default();
    // Corroborated, not the bare call: this is the one place a false accusation
    // permanently voids real money, so a `daemon` that knows about more than one
    // node (`daemon_fallback::FallbackDaemonClient`) cross-checks them here rather
    // than trusting whichever single one happened to answer - see
    // `MoneroDaemonClient::is_key_image_spent_corroborated`'s doc comment.
    let statuses = daemon.is_key_image_spent_corroborated(&key_images).await?;
    if !statuses.contains(&KeyImageStatus::SpentInBlockchain) {
        // Still ambiguous (still propagating, or a re-check will catch it next tick)
        // - never void on this evidence alone.
        return Ok(false);
    }
    let s = store.lock().unwrap();
    void_and_notify(&s, &payment.order_id, &payment.txid, payment.output_index, current_height, now)?;
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
    order_id: &str,
    txid: &str,
    output_index: i64,
    current_height: u64,
    now: i64,
) -> Result<()> {
    store.in_transaction(|store| {
        store.void_payment(order_id, txid, output_index, now)?;
        store.mark_double_spend_detected(order_id, now)?;
        recompute_and_notify_in_tx(store, order_id, current_height, now)?;
        // One event per voided payment row (docs/DESIGN.md §11), independent of
        // whatever status transition the recompute above may also have announced.
        enqueue_webhook_event(
            store,
            order_id,
            "order.double_spend_detected",
            &serde_json::json!({ "payment_id": order_id }),
            now,
        )
    })
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
fn unvoid_as_false_positive(
    store: &Store,
    order_id: &str,
    txid: &str,
    output_index: i64,
    current_height: u64,
    now: i64,
) -> Result<bool> {
    store.in_transaction(|store| {
        if !store.unvoid_payment(order_id, txid, output_index)? {
            return Ok(false);
        }
        if store.get_all_payments(order_id)?.iter().all(|p| p.voided_at.is_none()) {
            store.clear_double_spend_flag(order_id)?;
        }
        recompute_and_notify_in_tx(store, order_id, current_height, now)?;
        enqueue_webhook_event(
            store,
            order_id,
            "order.double_spend_reversed",
            &serde_json::json!({ "payment_id": order_id, "txid": txid }),
            now,
        )?;
        Ok(true)
    })
}

/// How far back [`revalidate_recent_double_spend_voids`] looks for voided payments
/// to recheck. Bounded deliberately: a void that turns out to have been a false
/// accusation is exactly as worth correcting a day later as a minute later - unlike
/// zero-conf detection, there is no latency requirement to trade away here - and an
/// old, long-settled void is not worth the cost of rechecking forever: if it were
/// wrong, the merchant and customer have long since moved on regardless.
pub const DOUBLE_SPEND_RECHECK_WINDOW_SECS: i64 = 48 * 3600;

/// Re-examines every payment on `network` voided within the last
/// [`DOUBLE_SPEND_RECHECK_WINDOW_SECS`] and reverses the void
/// (`unvoid_as_false_positive`) if a fresh call to
/// [`MoneroDaemonClient::is_key_image_spent_corroborated`] no longer supports the
/// original accusation.
///
/// This is the *only* other path that can ever reverse a void, alongside
/// `check_for_reorg_and_reconcile`'s own reverse check - and that one only runs when
/// a reorg (a block-hash mismatch) is *also* independently detected, which a lying
/// `is_key_image_spent` answer alone would never trigger, since it has nothing to do
/// with block hashes. Without this sweep, a false accusation from an uncorroborated
/// single answer would never be revisited by anything ever again (`docs/DESIGN.md`
/// §7.7).
///
/// Deliberately its own, much slower background loop (see `main.rs`), never called
/// from `run_scan_tick`'s tight per-second loop: double-spend voids are healthily
/// rare, so the cost of this sweep is proportional to how many voids happened
/// recently, not to how often it runs - checking every few minutes instead of every
/// second loses nothing a merchant would notice (see the constant above) and keeps
/// the extra per-payment RPCs entirely off the hot scanning path.
///
/// A failure rechecking one payment's key images is logged and skipped, leaving that
/// payment voided for the next sweep to retry - the same per-item resilience shape
/// as every other loop in this file - but a failure to read the chain height at all
/// (needed for the status recompute a reversal implies) aborts the sweep for this
/// network this round, since nothing here can proceed without one.
pub async fn revalidate_recent_double_spend_voids(
    store: &crate::store::SharedStore,
    daemon: &dyn MoneroDaemonClient,
    network: &str,
    now: i64,
) -> Result<Vec<String>> {
    let current_height = daemon.get_height().await?;
    let candidates = store.lock().unwrap().find_payments_voided_since(network, now - DOUBLE_SPEND_RECHECK_WINDOW_SECS)?;

    let mut recovered_orders = Vec::new();
    for payment in candidates {
        let key_images: Vec<String> = serde_json::from_str(&payment.key_images_json).unwrap_or_default();
        let statuses = match daemon.is_key_image_spent_corroborated(&key_images).await {
            Ok(statuses) => statuses,
            Err(e) => {
                eprintln!(
                    "double-spend revalidation: rechecking order {}'s voided payment on {network} failed - \
                     leaving it voided, will retry next sweep: {e}",
                    payment.order_id
                );
                continue;
            }
        };
        if !statuses.contains(&KeyImageStatus::SpentInBlockchain) {
            let s = store.lock().unwrap();
            if unvoid_as_false_positive(&s, &payment.order_id, &payment.txid, payment.output_index, current_height, now)? {
                recovered_orders.push(payment.order_id.clone());
            }
        }
    }
    Ok(recovered_orders)
}

// ---------------------------------------------------------------------------
// Order rescans (`docs/order_rescan_wbs.md` Phase 1) - a bounded, one-order
// historical rescan, separate from the live per-network scanner above.
// ---------------------------------------------------------------------------

/// Blocks subtracted from a rescan's computed start height, as a safety margin
/// against two independent sources of slop, both one-sided (only ever pushing the
/// real start *earlier* than requested, never later):
///
/// 1. [`crate::daemon::MoneroDaemonClient::find_height_at_or_before`]'s best-effort
///    binary search landing slightly late because of Monero's non-strictly-
///    monotonic block timestamps (see that method's own doc comment).
/// 2. The advanced-mode date inputs' own inherent timezone ambiguity - a plain
///    `<input type="date">` carries no timezone at all, so a merchant's typed date
///    is interpreted as UTC midnight server-side (monokulo labels the fields
///    as UTC precisely because of this), which can be up to ~12h off whatever the
///    merchant actually meant in their own local time.
///
/// Roughly 24 hours at Monero's ~2-minute block time - comfortably absorbs either
/// source alone, or both at once, and is still negligible next to the rescan
/// ranges this feature targets (days, not hours).
///
/// Deliberately one-sided: only ever applied to the *start* height. The *end* side
/// (near the tip) gets no equivalent buffer - see `rescan_order`'s own doc comment
/// for why the existing reorg/double-spend reconciliation pass already covers that
/// case without one.
pub const RESCAN_START_HEIGHT_CUSHION_BLOCKS: u64 = 720;

/// Applies [`RESCAN_START_HEIGHT_CUSHION_BLOCKS`] to a timestamp-derived height,
/// saturating at genesis rather than underflowing.
pub fn rescan_start_height(target_height: u64) -> u64 {
    target_height.saturating_sub(RESCAN_START_HEIGHT_CUSHION_BLOCKS)
}

/// How often [`run_rescan_job`] persists `order_rescans.current_height` while
/// walking a block range - not on every single block, since a real write per block
/// would be wasteful for a rescan that might cover tens of thousands of them
/// (WBS 1.3). The final height is always persisted regardless of this interval,
/// via `Store::complete_rescan`/`fail_rescan`.
pub const RESCAN_PROGRESS_PERSIST_INTERVAL_BLOCKS: u64 = 50;

/// Rescans one tenant's one `minor_index` across `from_height..=to_height`, then
/// does one final pass over the current mempool - the bounded, one-order historical
/// rescan primitive (WBS 1.1/1.4). Reuses the exact same `scan_transaction`/
/// `record_scan_match` primitives the live scanner (`run_scan_tick`) already calls,
/// narrowed to this one `minor_index` via the `Range<u32>` both already take - not a
/// rewrite, a different caller of the same building blocks.
///
/// Unlike `run_scan_tick`, a failure here propagates with `?` rather than being
/// logged and treated as "retry next tick": this is a one-shot bounded job, not a
/// perpetual loop, so `run_rescan_job` (its caller) is what decides what a failure
/// means - it marks the job's row `failed`, a terminal state the merchant sees and
/// can retry from, rather than silently stalling forever at an unscanned height the
/// way a live scanner tick would.
///
/// Calls `daemon.get_height()` once at the very end, after both the block walk and
/// the mempool check, purely to give `recompute_and_notify` a current tip to derive
/// confirmation counts against - `to_height` itself is not a safe substitute (it's
/// the rescan's own fixed target, potentially already behind the real tip by the
/// time a long rescan finishes).
///
/// Deliberately scans all the way to the literal tip with no held-back buffer on the
/// end side, unlike the start side's `RESCAN_START_HEIGHT_CUSHION_BLOCKS`: the
/// existing reorg/double-spend reconciliation pass (`check_for_reorg_and_reconcile`,
/// above, backed by `Store::find_payments_at_or_after_height`) has **no order-status
/// filter at all** - it re-examines every recorded, non-voided payment within
/// `reorg_check_depth` blocks of the tip regardless of whether that payment's order
/// is terminal, already proven for a terminal order by
/// `a_settled_order_is_walked_back_when_a_reorg_deeper_than_confirmations_required_orphans_its_payment`
/// below. A payment this rescan records - even one that immediately flips the order
/// to `Paid` - inherits that same ongoing protection automatically. A manual
/// tip-side buffer here would only create a blind spot that mechanism doesn't need.
/// How many times a single rescan step (one block's transactions, the final
/// mempool pass, the closing tip lookup) is retried before the whole job gives up
/// - a transient daemon hiccup mid-rescan (a dropped connection, a momentary
/// non-200) no longer kills an otherwise-healthy job outright. Deliberately
/// small and fast, not a substitute for `FallbackDaemonClient`'s own node-level
/// failover (already engaged underneath this - a real node switch on a single
/// call's failure) or `RpcDaemonClient`'s own 15s per-call timeout
/// (`src/daemon_rpc.rs`) - this is the next layer up, for the case where every
/// configured node briefly agrees on failure (a shared upstream blip, a
/// reorg-in-progress hiccup) rather than one node being individually down.
const RESCAN_STEP_MAX_ATTEMPTS: u32 = 3;
const RESCAN_STEP_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// How many blocks [`rescan_order`] asks [`MoneroDaemonClient::get_blocks_range`]
/// for in one daemon round trip. A rescan can span tens of thousands of blocks
/// (a default lookback window is weeks); fetching them one at a time (the
/// original shape) meant one HTTP round trip per block, competing for the same
/// public node's request capacity as the live scanner for as long as the
/// rescan ran (see `AppState::rescan_daemons`'s own doc comment for the
/// contention bug that motivated this). `RpcDaemonClient::get_blocks_range`
/// batches this into monerod's own `get_blocks.bin`, one round trip per chunk
/// instead of per block. 100 is a conservative middle ground: real Monero
/// blocks vary widely in size (near-empty to several hundred KB under load),
/// and monerod only started honoring `get_blocks.bin`'s own `max_block_count`
/// field in v0.18.4.3 - an older node ignoring it and returning more than
/// asked is still handled correctly (the caller advances by however many
/// blocks actually came back, not by this constant), so this number trades
/// off round-trip count against a single response's size on a well-behaved
/// node, not correctness either way.
const RESCAN_CHUNK_BLOCKS: u64 = 100;

/// Retries `f` up to [`RESCAN_STEP_MAX_ATTEMPTS`] times, a short fixed delay
/// apart, before giving up with its last error - the one bounded-retry point
/// every daemon call inside [`rescan_order`] goes through.
async fn retry_rescan_step<T, F, Fut>(mut f: F) -> std::result::Result<T, DaemonError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, DaemonError>>,
{
    let mut last_err = None;
    for attempt in 1..=RESCAN_STEP_MAX_ATTEMPTS {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt < RESCAN_STEP_MAX_ATTEMPTS {
                    eprintln!(
                        "rescan: step failed (attempt {attempt}/{RESCAN_STEP_MAX_ATTEMPTS}), retrying in \
                         {RESCAN_STEP_RETRY_DELAY:?}: {e}"
                    );
                    tokio::time::sleep(RESCAN_STEP_RETRY_DELAY).await;
                }
                last_err = Some(e);
            }
        }
    }
    // Always `Some` - the loop only exits without an early `return` after every
    // one of `RESCAN_STEP_MAX_ATTEMPTS` iterations has taken the `Err` arm.
    Err(last_err.expect("loop always records an error before exiting without returning"))
}

#[allow(clippy::too_many_arguments)]
pub async fn rescan_order(
    store: &SharedStore,
    key_custody: &dyn KeyCustody,
    daemon: &dyn MoneroDaemonClient,
    tenant_id: &str,
    handle: WalletHandle,
    minor_index: u32,
    from_height: u64,
    to_height: u64,
    now: i64,
    mut on_progress: impl FnMut(u64),
) -> Result<()> {
    let minor_range = minor_index..(minor_index + 1);
    let mut touched: HashSet<String> = HashSet::new();

    // Chunked, not per-block (see `RESCAN_CHUNK_BLOCKS`'s own doc comment for
    // why): `daemon.get_blocks_range` may return fewer blocks than asked (an
    // older node ignoring `max_block_count`, or simply running short of the
    // requested range) - the loop advances by however many actually came
    // back, `chunk.len()`, never by the requested chunk size, so a
    // permissive/older node can't desync progress from what was truly
    // recorded. A chunk of zero blocks is the one case that can't mean
    // anything but a stuck daemon (the requested range is always within
    // `to_height`, itself resolved against a real chain tip no earlier than
    // trigger time), so that's a hard error rather than a silent infinite
    // loop.
    let mut height = from_height;
    while height <= to_height {
        let remaining = to_height - height + 1;
        let chunk_size = remaining.min(RESCAN_CHUNK_BLOCKS);
        let chunk = retry_rescan_step(|| daemon.get_blocks_range(height, chunk_size)).await?;
        if chunk.is_empty() {
            return Err(DaemonError::Request(format!(
                "get_blocks_range returned zero blocks for a chunk starting at height {height} \
                 (asked for {chunk_size}) - refusing to loop forever without progress"
            ))
            .into());
        }
        for (offset, block_txs) in chunk.iter().enumerate() {
            let height = height + offset as u64;
            for tx in block_txs {
                let scan = scan_transaction(key_custody, handle, tx, minor_range.clone()).await?;
                let s = store.lock().unwrap();
                touched.extend(record_scan_match(&s, tenant_id, &scan, now, Some(height))?);
            }
            on_progress(height);
        }
        height += chunk.len() as u64;
    }

    // WBS 1.4: catches a payment that was broadcast but not yet mined by the time
    // the historical walk reaches the tip - a real "network delay" case, not an
    // error condition. Recorded at zero confirmations, same as the live scanner
    // would record it; nothing here waits for it to confirm (decision 6).
    for tx in &retry_rescan_step(|| daemon.get_mempool_transactions()).await? {
        let scan = scan_transaction(key_custody, handle, tx, minor_range.clone()).await?;
        let s = store.lock().unwrap();
        touched.extend(record_scan_match(&s, tenant_id, &scan, now, None)?);
    }

    let tip = retry_rescan_step(|| daemon.get_height()).await?;
    for order_id in &touched {
        let s = store.lock().unwrap();
        recompute_and_notify(&s, order_id, tip, now)?;
    }

    Ok(())
}

/// Runs rescan job `rescan_id` to completion (or failure), persisting
/// `order_rescans.current_height` periodically as it walks and marking the row
/// `completed`/`failed` when done (WBS 1.3). Resumes from the row's own
/// `current_height` - not `from_height` - so calling this again for a job that was
/// already partway through (e.g. after a server restart; see
/// `Store::list_running_rescans`) picks up where it left off rather than redoing
/// already-scanned blocks. Re-scanning `current_height` itself is intentional, not
/// an off-by-one: it guarantees at-least-once coverage of whatever block was
/// mid-flight when the process stopped, relying on `record_scan_match`'s existing
/// idempotency (`UNIQUE(order_id, txid, output_index)`) rather than inventing new
/// idempotency logic here.
///
/// Awaitable directly - every test below does exactly that. [`spawn_rescan_job`] is
/// the fire-and-forget production wrapper that additionally catches a panic here and
/// marks the row `failed` rather than letting it vanish silently.
pub async fn run_rescan_job(
    store: &SharedStore,
    key_custody: &dyn KeyCustody,
    daemon: &dyn MoneroDaemonClient,
    handle: WalletHandle,
    rescan_id: &str,
) {
    let job = match store.lock().unwrap().get_rescan(rescan_id) {
        Ok(Some(job)) => job,
        Ok(None) => {
            eprintln!("rescan {rescan_id}: row vanished before the job could run - nothing to do");
            return;
        }
        Err(e) => {
            eprintln!("rescan {rescan_id}: failed to load its own row - cannot start: {e}");
            return;
        }
    };

    let to_height = job.to_height;
    let mut last_persisted = job.current_height;
    let on_progress = |height: u64| {
        if height == to_height || height.saturating_sub(last_persisted) >= RESCAN_PROGRESS_PERSIST_INTERVAL_BLOCKS {
            let now = crate::now_unix();
            if let Err(e) = store.lock().unwrap().update_rescan_progress(rescan_id, height, now) {
                eprintln!("rescan {rescan_id}: failed to persist progress at height {height}: {e}");
            }
            // `docs/order_rescan_wbs.md` Phase 5.1 - same throttled cadence as the
            // progress write above, so this bookkeeping genuinely survives a
            // restart-and-resume rather than only updating on a clean finish.
            // Deliberately `job.from_height` (the row's own *original*, immutable
            // value) here, never `height` or the resume point `rescan_order` was
            // actually called with - a resumed job must not narrow
            // `first_scanned_height` back down to wherever it merely resumed from.
            if let Err(e) = store.lock().unwrap().bump_scanned_range_for_order(&job.order_id, job.from_height, height) {
                eprintln!("rescan {rescan_id}: failed to bump order {}'s scanned range: {e}", job.order_id);
            }
            last_persisted = height;
        }
    };

    let now = crate::now_unix();
    let result = rescan_order(
        store,
        key_custody,
        daemon,
        &job.tenant_id,
        handle,
        job.minor_index,
        job.current_height,
        job.to_height,
        now,
        on_progress,
    )
    .await;

    let now = crate::now_unix();
    match result {
        Ok(()) => {
            if let Err(e) = store.lock().unwrap().complete_rescan(rescan_id, now) {
                eprintln!("rescan {rescan_id}: failed to mark completed: {e}");
            }
        }
        Err(e) => {
            eprintln!("rescan {rescan_id} failed: {e}");
            if let Err(e2) = store.lock().unwrap().fail_rescan(rescan_id, &e.to_string(), now) {
                eprintln!("rescan {rescan_id}: failed to mark failed after a genuine error: {e2}");
            }
        }
    }
}

/// The real, fire-and-forget production shape for [`run_rescan_job`] - a genuinely
/// new spawn shape for this codebase (WBS 1.3): `shared::supervise::supervise` is
/// loop-only and wraps a closure that never returns, but a rescan job is bounded and
/// must run once to completion, not forever.
///
/// The double `tokio::spawn` (an outer task supervising an inner one) is what makes
/// a panic inside the job catchable without pulling in a separate `catch_unwind`
/// dependency: a panic propagating through the inner `JoinHandle` surfaces as an
/// `Err` the outer task can react to - marking the row `failed` - rather than
/// silently killing whatever spawned it. `run_rescan_job` itself already handles an
/// ordinary `Err` result from `rescan_order` (marking the row `failed` from inside),
/// so this outer layer only ever needs to handle the panic case.
pub fn spawn_rescan_job(
    store: SharedStore,
    key_custody: std::sync::Arc<dyn KeyCustody>,
    daemon: std::sync::Arc<dyn MoneroDaemonClient>,
    handle: WalletHandle,
    rescan_id: String,
) {
    let outer_store = store.clone();
    let outer_rescan_id = rescan_id.clone();
    tokio::spawn(async move {
        let inner = tokio::spawn(async move {
            run_rescan_job(&store, key_custody.as_ref(), daemon.as_ref(), handle, &rescan_id).await;
        });
        if let Err(join_err) = inner.await {
            eprintln!("rescan {outer_rescan_id} background task panicked: {join_err} - marking it failed");
            let now = crate::now_unix();
            if let Err(e) =
                outer_store.lock().unwrap().fail_rescan(&outer_rescan_id, &format!("panicked: {join_err}"), now)
            {
                eprintln!("rescan {outer_rescan_id}: failed to mark failed after a panic: {e}");
            }
        }
    });
}

/// One full scan tick for one network: mempool, any new confirmed blocks, then a
/// reorg check - composing the primitives above into what a production scanner
/// loop actually runs on an interval. `network` scopes everything to one chain: the
/// `daemon` passed in must be the client for that same network, `tenants` should be
/// pre-filtered (or filters itself further, see below) to that network's tenants,
/// and every `scanned_blocks`/reorg call is keyed by it. A multi-network instance
/// (§DESIGN.md §7) calls this once per configured network per round, each with its
/// own daemon and its own independent block-height bookkeeping - mixing them under
/// one call would compare block hashes across unrelated chains.
///
/// Takes `&SharedStore` and re-locks it around each `scan_transaction_for_tenant`
/// call rather than for the whole tick - but unlike `webhook_delivery::run_delivery_tick`,
/// it does *not* avoid holding the lock across `scan_transaction_for_tenant`'s
/// internal `.await` on `KeyCustody::scan_tx_outputs`. That await is CPU-bound
/// in-process work (elliptic-curve scalar multiplication), not network I/O, so the
/// hold time is microseconds rather than however long a merchant's webhook
/// endpoint takes to respond - a materially different, and currently accepted,
/// tradeoff. Worth revisiting with `spawn_blocking` if profiling ever shows
/// contention with HTTP handlers.
///
/// On first run (no `scanned_blocks` history at all), seeds at the current chain
/// tip rather than replaying the entire chain from genesis - this is a payment
/// gateway watching for new incoming payments, not a block explorer backfilling
/// history.
/// Never request fewer than this many blocks in one `get_blocks_range` call,
/// regardless of how large `avg_bytes_per_block` has drifted - a pathological
/// (e.g. cold-start-too-low) estimate must not compute a chunk size of `0`
/// and stall the catch-up walk forever.
const SCAN_CHUNK_MIN_BLOCKS: u64 = 1;
/// Never request more than this many blocks in one call, regardless of how
/// small `avg_bytes_per_block` has drifted (e.g. a long run of near-empty
/// blocks) - `payment.scan_chunk_memory_budget_mb` alone would technically
/// allow an enormous request in that case, and an older monerod ignoring
/// `get_blocks.bin`'s own `max_block_count` hint (see `get_blocks_range`'s
/// own doc comment) has no other backstop against that.
const SCAN_CHUNK_MAX_BLOCKS: u64 = 500;
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
const SCAN_CHUNK_INITIAL_AVG_BYTES: f64 = 50_000.0;

/// Pure sizing decision, extracted from `run_scan_tick`'s own loop specifically
/// so it's directly, cheaply unit-testable - the real behavior lives entirely
/// in arithmetic over three numbers, and proving "a larger average yields a
/// smaller chunk" shouldn't require driving real transactions through real
/// crypto to observe.
fn next_scan_chunk_size(budget_bytes: u64, avg_bytes_per_block: f64, remaining: u64) -> u64 {
    let by_budget = ((budget_bytes as f64) / avg_bytes_per_block).floor() as u64;
    by_budget.clamp(SCAN_CHUNK_MIN_BLOCKS, SCAN_CHUNK_MAX_BLOCKS).min(remaining)
}

/// Pure EWMA update, same reasoning as `next_scan_chunk_size` above - `chunk_
/// bytes`/`block_count` are already known before this is called, so this is
/// just the averaging formula on its own, testable without any daemon or
/// store at all.
fn update_avg_bytes_per_block(avg_bytes_per_block: f64, chunk_bytes: usize, block_count: usize) -> f64 {
    let observed_avg = chunk_bytes as f64 / block_count as f64;
    SCAN_CHUNK_EWMA_ALPHA * observed_avg + (1.0 - SCAN_CHUNK_EWMA_ALPHA) * avg_bytes_per_block
}

pub async fn run_scan_tick(
    store: &crate::store::SharedStore,
    key_custody: &dyn KeyCustody,
    daemon: &dyn MoneroDaemonClient,
    network: &str,
    tenants: &[(String, WalletHandle)],
    reorg_check_depth: u64,
    expired_order_grace_period_seconds: i64,
) -> Result<()> {
    let now = crate::now_unix();

    // The active watchlist (docs/DESIGN.md §7.3): only tenants with at least one
    // order still capable of receiving a *new* detected payment are worth the
    // scalar-multiplication cost of scanning. `active_tenant_ids` is a fresh query
    // against the one mutex-serialized Store, not a separately-maintained cache -
    // see its doc comment in `store.rs` for why that's a deliberate choice, not a
    // missed optimization: an incrementally add/removed cache has a real TOCTOU
    // race (a tenant's last order can settle and a brand-new order can arrive for
    // the same tenant in the wrong order relative to an out-of-band "remove"
    // decision, wrongly dropping a tenant with a genuinely pending order), and
    // paying for a fresh indexed query every tick is negligible next to the EC
    // math it avoids. Taking both queries (`active_tenant_ids` and each
    // `get_tenant_by_id`) under one lock hold keeps them mutually consistent -
    // no writer can slip in between deciding a tenant is active and reading its
    // current `next_minor_index`.
    //
    // Also filters by `t.network == network` even though `active_tenant_ids`
    // already scopes by network at the query level - a defensive second check
    // against whatever `tenants` the caller happened to pass in (e.g. a boot-time
    // snapshot spanning every configured network), so a mismatched daemon can never
    // be handed a tenant that belongs to a different chain.
    let ranges: Vec<(String, WalletHandle, Range<u32>)> = {
        let s = store.lock().unwrap();
        let active_ids: HashSet<String> =
            s.active_tenant_ids(network, now, expired_order_grace_period_seconds)?.into_iter().collect();
        tenants
            .iter()
            .filter(|(tenant_id, _)| active_ids.contains(tenant_id))
            .filter_map(|(tenant_id, handle)| {
                let t = s.get_tenant_by_id(tenant_id).ok().flatten()?;
                (t.network == network).then_some((tenant_id.clone(), *handle, 0..t.next_minor_index))
            })
            .collect()
    };

    let mut touched: HashSet<String> = HashSet::new();

    // A failed mempool poll is survivable (the next tick re-polls a second later) but
    // must not be *silent*: if it keeps failing, zero-conf detection is simply off for
    // this network, and an operator whose orders never leave `pending` before a block
    // arrives has nothing anywhere to tell them why.
    let mempool = match daemon.get_mempool_transactions().await {
        Ok(txs) => Some(txs),
        Err(e) => {
            eprintln!("polling the mempool on {network} failed - no zero-conf detection this tick: {e}");
            None
        }
    };
    // The txids of an *actually successful* poll, kept for the vanished-payment
    // sweep below - `None` (a failed poll) makes that sweep skip entirely rather
    // than mistake "we didn't look" for "the pool is empty".
    let mut mempool_txids: Option<HashSet<String>> = None;
    if let Some(mempool_txs) = mempool {
        mempool_txids = Some(mempool_txs.iter().map(tx_id_hex).collect());
        for tx in &mempool_txs {
            for (tenant_id, handle, minor_range) in &ranges {
                // Compute (async, no Store - see ScanResult's doc comment) then
                // persist (sync, no .await) as two separate steps, never a single
                // await-spanning call holding a &Store.
                //
                // A failure here is genuinely recoverable by doing nothing: the
                // mempool is re-polled roughly every second, so the same transaction
                // comes back around next tick. It still gets logged rather than
                // silently swallowed - a persistently failing scan or store call
                // that never surfaces anywhere is indistinguishable from "no
                // payments are arriving".
                match scan_transaction(key_custody, *handle, tx, minor_range.clone()).await {
                    Ok(scan) => {
                        let s = store.lock().unwrap();
                        match record_scan_match(&s, tenant_id, &scan, now, None) {
                            Ok(order_ids) => touched.extend(order_ids),
                            Err(e) => eprintln!(
                                "recording a mempool match for tenant {tenant_id} on {network} failed (will retry next tick): {e}"
                            ),
                        }
                    }
                    Err(e) => eprintln!("scanning a mempool tx for tenant {tenant_id} on {network} failed: {e}"),
                }
            }
        }
    }

    // A failure anywhere in here means "skip block scanning this tick, try again
    // next time" - it must never discard the mempool-scan results already
    // gathered above by short-circuiting the whole function. An earlier version of
    // this function did exactly that (`return Ok(())` from inside this block),
    // which silently dropped a real mempool match's status recompute + webhook
    // whenever the tip-seeding bootstrap below hit the lagging-backend condition
    // it exists to handle - caught only by a test asserting the mempool match's
    // *end-to-end* effect (status + webhook), not just that scanning didn't error.
    let current_height = daemon.get_height().await?;
    let last_scanned = store.lock().unwrap().max_scanned_height(network)?;
    let scan_range = match last_scanned {
        Some(h) => Some((h + 1, current_height)),
        None => {
            // Seeds one block behind the reported tip as a small safety margin
            // against a daemon momentarily reporting a height it can't yet serve a
            // block for (e.g. genuine replication lag across a pool of backend
            // nodes behind a public endpoint). This margin was originally added to
            // work around what turned out to be a different, deterministic bug -
            // `RpcDaemonClient::get_height` misreading monerod's block-count
            // convention as the tip height itself, now fixed at its source in
            // `daemon_rpc.rs`. Kept anyway as cheap, genuine defense for the
            // scenario it actually describes, now that a real bug isn't hiding
            // behind it.
            let seed_height = current_height.saturating_sub(1);
            match daemon.get_block_hash(seed_height).await {
                Ok(hash) => {
                    store.lock().unwrap().set_scanned_block(network, seed_height, &hash)?;
                    Some((seed_height + 1, current_height))
                }
                Err(_) => None,
            }
        }
    };

    // Unlike the mempool loop above, a failure here is *not* self-healing: a block is
    // scanned exactly once, and `set_scanned_block` moves the high-water mark past it
    // whether or not anything in it was successfully recorded. So a single transient
    // store error while recording a match used to lose that payment permanently and
    // without a trace - the block was marked scanned regardless, and nothing ever
    // looked at it again. Any failure at a height therefore abandons the rest of the
    // block range *without* marking that height scanned, leaving the next tick to
    // retry it from the same place; re-recording an already-recorded match is a no-op
    // thanks to `UNIQUE(order_id, txid, output_index)`.
    //
    // Deliberately a `break` rather than an early `return Err(..)`: the mempool
    // matches already gathered above still need their status recompute and webhooks,
    // which returning here would discard (a bug this function has had once before -
    // see the comment above `current_height`).
    if let Some((scan_from, scan_to)) = scan_range {
        // Fetches transactions in `get_blocks_range` chunks sized against
        // `payment.scan_chunk_memory_budget_mb` (read fresh from the store each
        // tick - a live, no-restart-needed knob, same as every other setting a
        // handler reads via `settings::get` at the point it's used) rather than
        // one `get_block_transactions` call per height - the catch-up walk after
        // real downtime can span thousands of blocks, and each one used to cost
        // its own daemon round trip. `avg_bytes_per_block` is a per-tick-local
        // EWMA seeded from `SCAN_CHUNK_INITIAL_AVG_BYTES`, updated from each
        // chunk's own real transaction sizes as it goes - see the constants'
        // own doc comments above for the reasoning. Deliberately *not*
        // batching `get_block_hash` below - see `docs/txid_lookup_and_scan_
        // chunking_wbs.md`'s "scope limit" for why real block-hash computation
        // stays out of this change entirely.
        let scan_chunk_memory_budget_mb: u32 =
            crate::settings::get(&store.lock().unwrap(), &crate::settings::PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB);
        let budget_bytes = (scan_chunk_memory_budget_mb as u64).saturating_mul(1024 * 1024);
        let mut avg_bytes_per_block = SCAN_CHUNK_INITIAL_AVG_BYTES;

        let mut height = scan_from;
        'heights: while height <= scan_to {
            let remaining = scan_to - height + 1;
            let chunk_size = next_scan_chunk_size(budget_bytes, avg_bytes_per_block, remaining);

            // Same danger as the old per-block `get_block_transactions` failure
            // this replaces: silence here is not necessarily transient (a
            // pruned node with no blob for this range, or an undecodable
            // transaction, fails identically forever), so it's logged loudly,
            // and the range is abandoned here rather than marking anything
            // scanned - the next tick retries from the same place.
            let chunk = match daemon.get_blocks_range(height, chunk_size).await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "fetching blocks {height}..+{chunk_size} on {network} failed - leaving block {height} \
                         unscanned so the next tick retries it. If this repeats at the same height, the \
                         scanner is stuck there and no payment on {network} is being detected: {e}"
                    );
                    break 'heights;
                }
            };
            if chunk.is_empty() {
                eprintln!(
                    "get_blocks_range returned zero blocks for a chunk starting at height {height} on \
                     {network} (asked for {chunk_size}) - leaving it unscanned so the next tick retries it"
                );
                break 'heights;
            }

            // `get_blocks_range` may return fewer than `chunk_size` (an older
            // node ignoring monerod's own `max_block_count` hint, or simply
            // running short of the requested range) - update the running
            // average and advance by however many blocks actually came back,
            // never by `chunk_size` itself, so an under-delivering node can't
            // desync progress from what was truly recorded.
            let chunk_bytes: usize =
                chunk.iter().flatten().map(|tx| monero::consensus::encode::serialize(tx).len()).sum();
            avg_bytes_per_block = update_avg_bytes_per_block(avg_bytes_per_block, chunk_bytes, chunk.len());

            for (offset, block_txs) in chunk.iter().enumerate() {
                let height = height + offset as u64;
                for tx in block_txs {
                    for (tenant_id, handle, minor_range) in &ranges {
                        let scan = match scan_transaction(key_custody, *handle, tx, minor_range.clone()).await {
                            Ok(scan) => scan,
                            Err(e) => {
                                eprintln!(
                                    "scanning a tx in block {height} on {network} for tenant {tenant_id} failed - \
                                     leaving block {height} unscanned so the next tick retries it: {e}"
                                );
                                break 'heights;
                            }
                        };
                        let recorded = {
                            let s = store.lock().unwrap();
                            record_scan_match(&s, tenant_id, &scan, now, Some(height))
                        };
                        match recorded {
                            Ok(order_ids) => touched.extend(order_ids),
                            Err(e) => {
                                eprintln!(
                                    "recording a match in block {height} on {network} for tenant {tenant_id} \
                                     failed - leaving block {height} unscanned so the next tick retries it: {e}"
                                );
                                break 'heights;
                            }
                        }
                    }
                }
                // Failing to read the hash of a block that was otherwise scanned
                // fine is the same "abandon the range here" situation as any
                // other failure at this height, and for a sharper reason than it
                // looks: continuing would leave a *hole* - height H unrecorded
                // while H+1 onwards are - and `check_for_reorg_and_reconcile`
                // skips heights it has no stored hash for. A later reorg
                // starting at H is then first noticed at H+1, so the reorg point
                // is reported one block too high: the payments actually
                // orphaned at H are never re-evaluated (they keep counting
                // towards their order at a height that no longer exists) and
                // block H of the replacement chain is never rescanned. Stopping
                // here instead simply re-scans H next tick, which is a no-op
                // for anything already recorded. Still one call per height,
                // unbatched - see this block's own opening comment.
                match daemon.get_block_hash(height).await {
                    Ok(hash) => {
                        let written = store.lock().unwrap().set_scanned_block(network, height, &hash);
                        if let Err(e) = written {
                            eprintln!(
                                "recording block {height} on {network} as scanned failed - leaving it unscanned \
                                 so the next tick retries it: {e}"
                            );
                            break 'heights;
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "reading the hash of block {height} on {network} failed - leaving block {height} \
                             unscanned so the next tick retries it rather than leaving a gap in the reorg window: {e}"
                        );
                        break 'heights;
                    }
                }
            }

            height += chunk.len() as u64;
        }
    }

    // `docs/order_rescan_wbs.md` Phase 5.1: bumps every currently-in-scope order's
    // scanned-range bookkeeping to whatever height is now confirmed-scanned on this
    // network - once per active tenant, not a per-order loop. Runs unconditionally
    // every tick, not only when this tick's own block-scanning pass made progress:
    // `max_scanned_height` still reflects genuine prior coverage on a tick where no
    // new block happened to arrive, which is exactly what lets a brand-new order
    // get its `first_scanned_height` set on its own very first eligible tick
    // (`ranges` is a fresh query every tick) rather than only on a tick that
    // happens to also process a new block. `None` (nothing has ever been scanned on
    // this network at all yet) is skipped entirely, correctly leaving every order's
    // range still `NULL`.
    let scanned_through = store.lock().unwrap().max_scanned_height(network).ok().flatten();
    if let Some(scanned_through) = scanned_through {
        for (tenant_id, _, _) in &ranges {
            if let Err(e) = store.lock().unwrap().bump_scanned_heights_for_tenant(
                tenant_id,
                scanned_through,
                now,
                expired_order_grace_period_seconds,
            ) {
                eprintln!(
                    "failed to bump scanned-range bookkeeping for tenant {tenant_id} on {network} - its \
                     orders' displayed scan range may lag until a later tick succeeds: {e}"
                );
            }
        }
    }

    // Reconciliation runs *before* the recompute sweep below, not after it. Both
    // orderings recompute the same orders; only this one recomputes them from payment
    // rows the chain still agrees with. Sweeping first means that on the one tick
    // where a reorg is detected, every non-terminal order is evaluated against
    // heights reconciliation is about to invalidate - so an order whose payment was
    // just orphaned can cross its confirmation threshold and fire `order.paid`
    // moments before the same tick voids that payment and fires the retraction. A
    // merchant acting on `order.paid` ships goods; "we sent it, then took it back a
    // second later" is not a recoverable webhook. Reconciling first costs nothing:
    // with no reorg this is a handful of hash comparisons, and the orders it marks
    // dirty simply recompute to the same value again in the sweep, which enqueues
    // nothing when nothing changed.
    let report = check_for_reorg_and_reconcile(store, daemon, network, reorg_check_depth, now).await;

    // The blind spot reorg detection structurally cannot cover: a zero-conf payment
    // whose transaction quietly leaves the pool without ever being mined, because a
    // conflicting transaction won instead. No stored block hash changes in that
    // story, so nothing above would ever look at that payment again. Runs after the
    // block scan deliberately - a transaction mined this tick already has its height
    // recorded by then and is not a candidate at all - and, like reconciliation,
    // before the recompute sweep, so a voided payment can never be announced as a
    // settlement moments before it is retracted.
    let vanished = match &mempool_txids {
        Some(txids) => check_vanished_mempool_payments(store, daemon, network, txids, current_height, now).await,
        None => Ok(VanishedPoolReport { dirty_orders: vec![], double_spent_orders: vec![] }),
    };
    if let Ok(report) = &vanished {
        // Unioned into `touched` rather than recomputed here: an order whose only
        // payment was just voided may well be terminal (`paid` off the zero-conf
        // ceiling is exactly the case this sweep exists for), so it is absent from
        // the non-terminal set the sweep below iterates and would otherwise never be
        // recomputed at all.
        touched.extend(report.dirty_orders.iter().cloned());
    }

    // Every non-terminal order on this network is recomputed, not only the orders
    // whose transactions were matched this tick. An order's status depends on the
    // current chain height (confirmations are derived, never stored per payment) and
    // on wall-clock time (expiry), so an order with entirely unchanged payments still
    // changes status as the chain grows. Recomputing only the `touched` set meant an
    // order was recomputed exactly once - at one confirmation - and then stayed
    // `confirming` forever no matter how deeply buried its payment became, and an
    // unpaid order never became `expired` at all. `touched` is unioned in rather than
    // replaced because a just-matched order may already be terminal (and so absent
    // from the non-terminal set) yet still need its amounts refreshed. The set union
    // is also what keeps this from double-firing webhooks: each order is recomputed
    // at most once per tick, and `recompute_and_notify` only enqueues on an actual
    // status change.
    //
    // Deliberately still runs when reconciliation above failed (its result is only
    // unwrapped afterwards): a node that went unreachable partway through
    // reconciliation leaves a pure retry for the next tick, and holding every order's
    // expiry and confirmation growth hostage to it would turn a transient node blip
    // into orders that silently stop advancing.
    let to_recompute: HashSet<String> = store
        .lock()
        .unwrap()
        .non_terminal_order_ids(network, now, expired_order_grace_period_seconds)?
        .into_iter()
        .chain(touched.iter().cloned())
        .collect();
    for order_id in &to_recompute {
        let s = store.lock().unwrap();
        recompute_and_notify(&s, order_id, current_height, now)?;
    }

    // Both reports' `double_spent_orders` are informational here: the
    // `order.double_spend_detected` event for each voided payment was already
    // enqueued by `void_and_notify`, in the same transaction as the void itself, so
    // that a failure later in the same pass can neither lose the event nor leave the
    // order's status describing money that has been written off. Unwrapped only now,
    // after the recompute sweep above, so a node that died mid-reconciliation still
    // leaves every other order's confirmations and expiry advancing.
    let _ = report?;
    let _ = vanished?;

    // Nothing reads a scanned-block row from further back than the reorg window, so
    // keeping every row this service has ever written is pure growth - a block every
    // two minutes, forever, on hardware whose storage is often an SD card. The
    // retention is measured from *this scanner's own* high-water mark rather than the
    // daemon's reported height, so a node briefly claiming an absurd tip cannot talk
    // the scanner into deleting the window it needs, and it is deliberately several
    // times `reorg_check_depth`: the only thing that has to survive is enough history
    // for the deepest reorg the window claims to handle, plus room for a rewind to
    // find its common ancestor below that.
    //
    // Last in the tick, and non-fatal: this is housekeeping, and a tick that detected
    // a payment must not be reported as failed because a delete didn't land.
    {
        let s = store.lock().unwrap();
        if let Ok(Some(high_water)) = s.max_scanned_height(network) {
            let keep_from = high_water.saturating_sub(reorg_check_depth.saturating_mul(4));
            if let Err(e) = s.prune_scanned_blocks_below(network, keep_from) {
                eprintln!("pruning scanned blocks below {keep_from} on {network} failed (harmless, retried next tick): {e}");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
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
    //!    `a_zero_conf_payment_double_spent_out_of_the_mempool_is_voided_with_no_reorg_involved`
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
    use crate::daemon::fake::FakeDaemonClient;
    use crate::daemon_fallback::{FallbackDaemonClient, FallbackNode};
    use crate::key_custody::{KeyCustodyError, MatchedOutput, Network, PlainKeyCustody, SubaddressIndex, WalletMaterial};
    use crate::store::{NewOrder, NewOrderRescan, NewTenant, RescanMode};
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
        async fn register_wallet(&self, material: WalletMaterial) -> std::result::Result<WalletHandle, KeyCustodyError> {
            self.inner.register_wallet(material).await
        }
        async fn remove_wallet(&self, handle: WalletHandle) -> std::result::Result<(), KeyCustodyError> {
            self.inner.remove_wallet(handle).await
        }
        async fn seal(&self, material: &WalletMaterial) -> std::result::Result<Vec<u8>, KeyCustodyError> {
            self.inner.seal(material).await
        }
        async fn unseal_and_register(&self, sealed: &[u8]) -> std::result::Result<WalletHandle, KeyCustodyError> {
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
            tx: &Transaction,
            major_range: Range<u32>,
            minor_range: Range<u32>,
        ) -> std::result::Result<Vec<MatchedOutput>, KeyCustodyError> {
            self.scan_calls.fetch_add(1, Ordering::SeqCst);
            self.inner.scan_tx_outputs(handle, tx, major_range, minor_range).await
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
        async fn get_block_timestamp(&self, height: u64) -> std::result::Result<u64, DaemonError> {
            self.inner.get_block_timestamp(height).await
        }
        async fn get_block_transactions(&self, height: u64) -> std::result::Result<Vec<Transaction>, DaemonError> {
            self.inner.get_block_transactions(height).await
        }
        async fn get_mempool_transactions(&self) -> std::result::Result<Vec<Transaction>, DaemonError> {
            self.inner.get_mempool_transactions().await
        }
        async fn locate_transaction(&self, _txid: &str) -> std::result::Result<TxLocation, DaemonError> {
            Err(DaemonError::Request("simulated node failure mid-reconciliation".into()))
        }
        async fn get_transaction(&self, txid: &str) -> std::result::Result<Transaction, DaemonError> {
            self.inner.get_transaction(txid).await
        }
        async fn is_key_image_spent(&self, key_images: &[String]) -> std::result::Result<Vec<KeyImageStatus>, DaemonError> {
            self.inner.is_key_image_spent(key_images).await
        }
    }

    /// Wraps a `FakeDaemonClient` and fails `get_block_hash` for exactly one height,
    /// leaving everything else - including `get_block_transactions` for that same
    /// height - working. Models one RPC in a tick's sequence failing where its
    /// immediate neighbours succeed, which is the only way a *gap* (rather than a
    /// clean stopping point) can appear in the scanned-block window.
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
                return Err(DaemonError::Request(format!("simulated failure reading the hash of block {height}")));
            }
            self.inner.get_block_hash(height).await
        }
        async fn get_block_timestamp(&self, height: u64) -> std::result::Result<u64, DaemonError> {
            self.inner.get_block_timestamp(height).await
        }
        async fn get_block_transactions(&self, height: u64) -> std::result::Result<Vec<Transaction>, DaemonError> {
            self.inner.get_block_transactions(height).await
        }
        async fn get_mempool_transactions(&self) -> std::result::Result<Vec<Transaction>, DaemonError> {
            self.inner.get_mempool_transactions().await
        }
        async fn locate_transaction(&self, txid: &str) -> std::result::Result<TxLocation, DaemonError> {
            self.inner.locate_transaction(txid).await
        }
        async fn get_transaction(&self, txid: &str) -> std::result::Result<Transaction, DaemonError> {
            self.inner.get_transaction(txid).await
        }
        async fn is_key_image_spent(&self, key_images: &[String]) -> std::result::Result<Vec<KeyImageStatus>, DaemonError> {
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
        BlockTransactions,
        /// Counts calls to `get_blocks_range` itself, distinct from
        /// `BlockTransactions` - `DaemonFailingFrom` overrides `get_blocks_range`
        /// to gate/count it directly rather than falling through to the trait's
        /// own default (which would decompose it into per-height
        /// `BlockTransactions` calls, making the two indistinguishable). This is
        /// what lets a test assert "the chunked scan loop issued N real batched
        /// calls," not just "N blocks were eventually fetched somehow."
        BlocksRange,
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
            Self { inner, method, fail_from: AtomicU64::new(NO_FAILING_CALL), calls: AtomicU64::new(0), delay_ms: 0 }
        }
        fn failing_from(inner: D, method: DaemonCall, nth: u64) -> Self {
            Self { inner, method, fail_from: AtomicU64::new(nth), calls: AtomicU64::new(0), delay_ms: 0 }
        }
        fn timing_out_from(inner: D, method: DaemonCall, nth: u64, delay_ms: u64) -> Self {
            Self { inner, method, fail_from: AtomicU64::new(nth), calls: AtomicU64::new(0), delay_ms }
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
                return Err(DaemonError::Request(format!("simulated {method:?} failure on call {nth}")));
            }
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl<D: MoneroDaemonClient> MoneroDaemonClient for DaemonFailingFrom<D> {
        async fn get_height(&self) -> std::result::Result<u64, DaemonError> {
            self.gate(DaemonCall::Height).await?;
            self.inner.get_height().await
        }
        async fn get_block_hash(&self, height: u64) -> std::result::Result<String, DaemonError> {
            self.gate(DaemonCall::BlockHash).await?;
            self.inner.get_block_hash(height).await
        }
        async fn get_block_timestamp(&self, height: u64) -> std::result::Result<u64, DaemonError> {
            self.inner.get_block_timestamp(height).await
        }
        async fn get_block_transactions(&self, height: u64) -> std::result::Result<Vec<Transaction>, DaemonError> {
            self.gate(DaemonCall::BlockTransactions).await?;
            self.inner.get_block_transactions(height).await
        }
        async fn get_blocks_range(&self, start_height: u64, count: u64) -> std::result::Result<Vec<Vec<Transaction>>, DaemonError> {
            self.gate(DaemonCall::BlocksRange).await?;
            // Deliberately *not* `self.inner.get_blocks_range(...)`: that would call
            // `inner`'s own `get_block_transactions` directly for each height,
            // bypassing this wrapper's `BlockTransactions` gate entirely (silently
            // breaking every test that injects a failure at a specific
            // `BlockTransactions` call number). Looping through `self.
            // get_block_transactions` instead - the trait's own default body,
            // copied here rather than inherited, so it stays wrapped - keeps both
            // gates independently meaningful: a `BlocksRange`-gated test sees one
            // count per top-level call this wrapper receives, a
            // `BlockTransactions`-gated test still sees one count per height
            // regardless of how many blocks one `get_blocks_range` call covers.
            let mut out = Vec::new();
            for height in start_height..start_height.saturating_add(count) {
                out.push(self.get_block_transactions(height).await?);
            }
            Ok(out)
        }
        async fn get_mempool_transactions(&self) -> std::result::Result<Vec<Transaction>, DaemonError> {
            self.gate(DaemonCall::Mempool).await?;
            self.inner.get_mempool_transactions().await
        }
        async fn locate_transaction(&self, txid: &str) -> std::result::Result<TxLocation, DaemonError> {
            self.gate(DaemonCall::Locate).await?;
            self.inner.locate_transaction(txid).await
        }
        async fn get_transaction(&self, txid: &str) -> std::result::Result<Transaction, DaemonError> {
            self.inner.get_transaction(txid).await
        }
        async fn is_key_image_spent(&self, key_images: &[String]) -> std::result::Result<Vec<KeyImageStatus>, DaemonError> {
            self.gate(DaemonCall::KeyImageSpent).await?;
            self.inner.is_key_image_spent(key_images).await
        }
    }

    /// The same real fixture transaction and view pair used in
    /// `src/key_custody/plain.rs`'s tests - it pays subaddress 0/1. Reused here so
    /// scanner-level tests exercise real crypto end to end, not a stub that assumes
    /// matching works.
    fn fixture_tx() -> Transaction {
        let raw_tx = hex::decode(include_str!("../tests/fixtures/subaddress_tx.hex")).unwrap();
        deserialize(&raw_tx).unwrap()
    }

    fn fixture_view_key() -> [u8; 32] {
        PrivateKey::from_slice(
            &hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07").unwrap(),
        )
        .unwrap()
        .to_bytes()
    }

    fn fixture_spend_pubkey() -> [u8; 32] {
        let secret_spend = PrivateKey::from_slice(
            &hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907").unwrap(),
        )
        .unwrap();
        PublicKey::from_private_key(&secret_spend).to_bytes()
    }

    async fn setup() -> (Store, PlainKeyCustody, WalletHandle, String, String) {
        setup_with_zero_conf_ceiling(None).await
    }

    /// `setup()` with a merchant-configured zero-conf trust ceiling, for the
    /// scenarios where an order settles off a mempool sighting alone - the case a
    /// plain double-spend actually costs a merchant something, since they may have
    /// shipped against it.
    async fn setup_with_zero_conf_ceiling(
        zero_conf_max_piconero: Option<u64>,
    ) -> (Store, PlainKeyCustody, WalletHandle, String, String) {
        let store = Store::open_in_memory().unwrap();
        let key_custody = PlainKeyCustody::default();
        let handle = key_custody
            .register_wallet(WalletMaterial::new(fixture_view_key(), fixture_spend_pubkey()))
            .await
            .unwrap();

        let created = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4fixture".into(),
                    network: "mainnet".into(),
                    allowed_origins: vec![],
                    confirmations_required: Some(10),
                    zero_conf_max_piconero,
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
        assert_eq!(index, 1, "fixture tx pays minor index 1 - keep this in sync with the order below");
        let address = key_custody
            .derive_subaddress(handle, SubaddressIndex { major: 0, minor: index }, Network::Mainnet)
            .await
            .unwrap();

        let order = store
            .create_order(NewOrder {
                confirmations_required_override: None,
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

        (store, key_custody, handle, tenant_id, order.id)
    }

    /// `setup()` with a caller-controlled `expires_at`, for the grace-period tests
    /// below - they need an order whose deadline sits at a specific real-wall-clock
    /// offset (recently past, or long past), which `setup()`'s own fixed
    /// `now_unix() + 3600` can't express.
    async fn setup_with_expiry(expires_at: i64) -> (Store, PlainKeyCustody, WalletHandle, String, String) {
        let store = Store::open_in_memory().unwrap();
        let key_custody = PlainKeyCustody::default();
        let handle =
            key_custody.register_wallet(WalletMaterial::new(fixture_view_key(), fixture_spend_pubkey())).await.unwrap();

        let created = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4fixture".into(),
                    network: "mainnet".into(),
                    allowed_origins: vec![],
                    confirmations_required: Some(10),
                    zero_conf_max_piconero: None,
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();
        let tenant_id = created.tenant.id.clone();

        let index = store.allocate_minor_index(&tenant_id).unwrap();
        assert_eq!(index, 1, "fixture tx pays minor index 1 - keep this in sync with the order below");
        let address =
            key_custody.derive_subaddress(handle, SubaddressIndex { major: 0, minor: index }, Network::Mainnet).await.unwrap();

        let order = store
            .create_order(NewOrder {
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

        (store, key_custody, handle, tenant_id, order.id)
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
        let (_, status) = store.recompute_order_status(&order_id, 0, now).unwrap();
        assert_eq!(status, crate::status::OrderStatus::Expired, "test setup must actually produce an expired order");

        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.set_mempool(vec![fixture_tx()]);

        // A generous grace period - the order expired moments ago, well inside it.
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 3600).await.unwrap();

        let s = store.lock().unwrap();
        assert_eq!(
            s.get_all_payments(&order_id).unwrap().len(),
            1,
            "a late payment within the grace period must still be matched by ordinary live scanning"
        );
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
        // No zero-conf ceiling configured (`setup_with_expiry`), so a mempool-only
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
        let (store, key_custody, handle, tenant_id, order_id) = setup_with_expiry(now - 10_000).await;

        let (_, status) = store.recompute_order_status(&order_id, 0, now).unwrap();
        assert_eq!(status, crate::status::OrderStatus::Expired, "test setup must actually produce an expired order");

        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.set_mempool(vec![fixture_tx()]);

        // A short grace period the order's 10,000-second-old expiry is well past -
        // proving the boundary is real, not just documented (exactly the gap the
        // manual rescan exists to close).
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 60).await.unwrap();

        let s = store.lock().unwrap();
        assert_eq!(
            s.get_all_payments(&order_id).unwrap().len(),
            0,
            "a payment arriving after the grace period has elapsed must not be matched by ordinary live scanning"
        );
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(order.status, crate::status::OrderStatus::Expired, "must remain untouched");
    }

    // -- Scanned block range (`docs/order_rescan_wbs.md` Phase 5.1) ---------

    #[tokio::test]
    async fn a_fresh_orders_first_scanned_height_is_set_on_its_very_first_tick_not_backfilled_to_created_at() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let daemon = FakeDaemonClient::new();
        for h in 1..=500 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        let store = store.into_shared();

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let order = store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap();
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
    async fn last_scanned_height_advances_tick_over_tick_then_freezes_once_the_order_leaves_scope() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let daemon = FakeDaemonClient::new();
        for h in 1..=100 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        let store = store.into_shared();

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(
            store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap().last_scanned_height,
            Some(100)
        );

        for h in 101..=150 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(
            store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap().last_scanned_height,
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
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        let settled = store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert!(
            matches!(settled.status, crate::status::OrderStatus::Paid | crate::status::OrderStatus::Overpaid),
            "expected a terminal status after 10 confirmations, got {:?}",
            settled.status
        );
        assert_eq!(settled.last_scanned_height, Some(160));

        for h in 161..=200 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        let frozen = store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(
            frozen.last_scanned_height,
            Some(160),
            "must freeze (not keep advancing, not reset) once the order is terminal and out of scope"
        );
    }

    #[tokio::test]
    async fn a_single_rescan_extends_both_bounds_beyond_what_live_scanning_alone_reached() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let daemon = FakeDaemonClient::new();
        for h in 1..=100 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        let store = store.into_shared();
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        let before = store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!((before.first_scanned_height, before.last_scanned_height), (Some(100), Some(100)));

        for h in 101..=200 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        let job = store
            .lock()
            .unwrap()
            .trigger_rescan(
                NewOrderRescan {
                    order_id: order_id.clone(),
                    tenant_id: tenant_id.clone(),
                    minor_index: 1,
                    mode: RescanMode::Advanced,
                    from_height: 10,
                    to_height: 200,
                },
                crate::now_unix(),
            )
            .unwrap()
            .into_job();
        run_rescan_job(&store, &key_custody, &daemon, handle, &job.id).await;

        let after = store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(after.first_scanned_height, Some(10), "must extend earlier than live scanning alone ever reached");
        assert_eq!(after.last_scanned_height, Some(200), "must extend later than live scanning alone ever reached");
    }

    #[tokio::test]
    async fn two_sequential_rescans_each_further_out_accumulate_rather_than_overwrite() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let daemon = FakeDaemonClient::new();
        for h in 1..=300 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        let store = store.into_shared();

        let job1 = store
            .lock()
            .unwrap()
            .trigger_rescan(
                NewOrderRescan {
                    order_id: order_id.clone(),
                    tenant_id: tenant_id.clone(),
                    minor_index: 1,
                    mode: RescanMode::Advanced,
                    from_height: 100,
                    to_height: 150,
                },
                crate::now_unix(),
            )
            .unwrap()
            .into_job();
        run_rescan_job(&store, &key_custody, &daemon, handle, &job1.id).await;
        let after1 = store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!((after1.first_scanned_height, after1.last_scanned_height), (Some(100), Some(150)));

        // A second rescan, further out on *both* sides - must not overwrite the
        // first's own bounds, only extend past them.
        let job2 = store
            .lock()
            .unwrap()
            .trigger_rescan(
                NewOrderRescan {
                    order_id: order_id.clone(),
                    tenant_id: tenant_id.clone(),
                    minor_index: 1,
                    mode: RescanMode::Advanced,
                    from_height: 50,
                    to_height: 250,
                },
                crate::now_unix(),
            )
            .unwrap()
            .into_job();
        assert_ne!(job2.id, job1.id, "the first job must have completed and freed the one-per-tenant slot");
        run_rescan_job(&store, &key_custody, &daemon, handle, &job2.id).await;
        let after2 = store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(after2.first_scanned_height, Some(50), "must extend further out, not overwrite or narrow");
        assert_eq!(after2.last_scanned_height, Some(250), "must extend further out, not overwrite or narrow");
    }

    #[tokio::test]
    async fn a_resumed_rescan_still_extends_the_scanned_range_from_the_jobs_original_from_height() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let daemon = FakeDaemonClient::new();
        for h in 1..=200 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        let store = store.into_shared();

        let job = store
            .lock()
            .unwrap()
            .trigger_rescan(
                NewOrderRescan {
                    order_id: order_id.clone(),
                    tenant_id: tenant_id.clone(),
                    minor_index: 1,
                    mode: RescanMode::Advanced,
                    from_height: 10,
                    to_height: 200,
                },
                crate::now_unix(),
            )
            .unwrap()
            .into_job();
        // Simulate a prior partial run that had already persisted progress to
        // height 100 before the process died - the exact state a restart resumes
        // from. The bug this test would catch: a design that (wrongly) used the
        // resume point (100) instead of the job's own immutable `from_height`
        // (10) as the "first" bound, silently narrowing the displayed range.
        store.lock().unwrap().update_rescan_progress(&job.id, 100, crate::now_unix()).unwrap();

        run_rescan_job(&store, &key_custody, &daemon, handle, &job.id).await;

        let order = store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(order.first_scanned_height, Some(10), "must reflect the job's original from_height, not the resume point");
        assert_eq!(order.last_scanned_height, Some(200));
    }

    #[tokio::test]
    async fn an_order_in_its_grace_window_with_a_simultaneous_rescan_has_its_range_advanced_correctly_by_both() {
        let now = crate::now_unix();
        let (store, key_custody, handle, tenant_id, order_id) = setup_with_expiry(now - 60).await;
        let (_, status) = store.recompute_order_status(&order_id, 0, now).unwrap();
        assert_eq!(status, crate::status::OrderStatus::Expired, "test setup must actually produce an expired order");

        let daemon = FakeDaemonClient::new();
        for h in 1..=100 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        let store = store.into_shared();

        // Ordinary live scanning, widened by a generous grace window, bumps the
        // range first.
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 3600).await.unwrap();
        assert_eq!(
            store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap().last_scanned_height,
            Some(100)
        );

        for h in 101..=150 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        let job = store
            .lock()
            .unwrap()
            .trigger_rescan(
                NewOrderRescan {
                    order_id: order_id.clone(),
                    tenant_id: tenant_id.clone(),
                    minor_index: 1,
                    mode: RescanMode::Advanced,
                    from_height: 1,
                    to_height: 150,
                },
                now,
            )
            .unwrap()
            .into_job();
        run_rescan_job(&store, &key_custody, &daemon, handle, &job.id).await;

        for h in 151..=200 {
            daemon.push_block(&format!("blk_{h}"), vec![]);
        }
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 3600).await.unwrap();

        let order = store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(order.first_scanned_height, Some(1), "the rescan's earlier start must be preserved");
        assert_eq!(
            order.last_scanned_height,
            Some(200),
            "live scanning must still be able to advance the range further after the rescan finished"
        );
    }

    // -- Order rescans (`docs/order_rescan_wbs.md` Phase 1) -----------------

    #[test]
    fn rescan_start_height_subtracts_the_cushion_and_saturates_at_genesis() {
        assert_eq!(rescan_start_height(1000), 1000 - RESCAN_START_HEIGHT_CUSHION_BLOCKS);
        assert_eq!(rescan_start_height(0), 0, "must saturate rather than underflow near genesis");
        assert_eq!(
            rescan_start_height(RESCAN_START_HEIGHT_CUSHION_BLOCKS - 1),
            0,
            "a target inside the cushion window of genesis still saturates to 0, not a negative height"
        );
    }

    /// Wraps `FakeDaemonClient`, recording every height `get_block_transactions` was
    /// called for - the direct way to prove a resumed rescan job actually starts
    /// walking from its persisted `current_height` rather than `from_height` again.
    struct HeightRecordingDaemon {
        inner: FakeDaemonClient,
        heights_seen: std::sync::Mutex<Vec<u64>>,
    }

    impl HeightRecordingDaemon {
        fn new(inner: FakeDaemonClient) -> Self {
            Self { inner, heights_seen: std::sync::Mutex::new(Vec::new()) }
        }
    }

    #[async_trait::async_trait]
    impl MoneroDaemonClient for HeightRecordingDaemon {
        async fn get_height(&self) -> std::result::Result<u64, DaemonError> {
            self.inner.get_height().await
        }
        async fn get_block_hash(&self, height: u64) -> std::result::Result<String, DaemonError> {
            self.inner.get_block_hash(height).await
        }
        async fn get_block_timestamp(&self, height: u64) -> std::result::Result<u64, DaemonError> {
            self.inner.get_block_timestamp(height).await
        }
        async fn get_block_transactions(&self, height: u64) -> std::result::Result<Vec<Transaction>, DaemonError> {
            self.heights_seen.lock().unwrap().push(height);
            self.inner.get_block_transactions(height).await
        }
        async fn get_mempool_transactions(&self) -> std::result::Result<Vec<Transaction>, DaemonError> {
            self.inner.get_mempool_transactions().await
        }
        async fn locate_transaction(&self, txid: &str) -> std::result::Result<TxLocation, DaemonError> {
            self.inner.locate_transaction(txid).await
        }
        async fn get_transaction(&self, txid: &str) -> std::result::Result<Transaction, DaemonError> {
            self.inner.get_transaction(txid).await
        }
        async fn is_key_image_spent(&self, key_images: &[String]) -> std::result::Result<Vec<KeyImageStatus>, DaemonError> {
            self.inner.is_key_image_spent(key_images).await
        }
    }

    /// Wraps `FakeDaemonClient`, failing the first `fail_count` calls to
    /// `get_block_transactions` (regardless of which height) with a real
    /// `DaemonError`, then delegating normally forever after - models a transient
    /// hiccup every configured fallback node briefly agrees on (a shared upstream
    /// blip), the case `RESCAN_STEP_MAX_ATTEMPTS` retries exist for.
    struct FlakyBlockTransactionsDaemon {
        inner: FakeDaemonClient,
        remaining_failures: std::sync::atomic::AtomicU32,
    }

    impl FlakyBlockTransactionsDaemon {
        fn new(inner: FakeDaemonClient, fail_count: u32) -> Self {
            Self { inner, remaining_failures: std::sync::atomic::AtomicU32::new(fail_count) }
        }
    }

    #[async_trait::async_trait]
    impl MoneroDaemonClient for FlakyBlockTransactionsDaemon {
        async fn get_height(&self) -> std::result::Result<u64, DaemonError> {
            self.inner.get_height().await
        }
        async fn get_block_hash(&self, height: u64) -> std::result::Result<String, DaemonError> {
            self.inner.get_block_hash(height).await
        }
        async fn get_block_timestamp(&self, height: u64) -> std::result::Result<u64, DaemonError> {
            self.inner.get_block_timestamp(height).await
        }
        async fn get_block_transactions(&self, height: u64) -> std::result::Result<Vec<Transaction>, DaemonError> {
            if self.remaining_failures.fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |n| if n > 0 { Some(n - 1) } else { None },
            ).is_ok()
            {
                return Err(DaemonError::Request("simulated transient failure".to_string()));
            }
            self.inner.get_block_transactions(height).await
        }
        async fn get_mempool_transactions(&self) -> std::result::Result<Vec<Transaction>, DaemonError> {
            self.inner.get_mempool_transactions().await
        }
        async fn locate_transaction(&self, txid: &str) -> std::result::Result<TxLocation, DaemonError> {
            self.inner.locate_transaction(txid).await
        }
        async fn get_transaction(&self, txid: &str) -> std::result::Result<Transaction, DaemonError> {
            self.inner.get_transaction(txid).await
        }
        async fn is_key_image_spent(&self, key_images: &[String]) -> std::result::Result<Vec<KeyImageStatus>, DaemonError> {
            self.inner.is_key_image_spent(key_images).await
        }
    }

    #[tokio::test]
    async fn a_rescan_survives_fewer_transient_failures_than_the_retry_budget() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let inner = FakeDaemonClient::new();
        for h in 1..=100 {
            let txs = if h == 50 { vec![tx.clone()] } else { vec![] };
            inner.push_block(&format!("blk_{h}"), txs);
        }
        // One fewer failure than the retry budget - the step must still succeed.
        let daemon = FlakyBlockTransactionsDaemon::new(inner, RESCAN_STEP_MAX_ATTEMPTS - 1);
        let store = store.into_shared();

        rescan_order(&store, &key_custody, &daemon, &tenant_id, handle, 1, 1, 100, 2000, |_| {}).await.unwrap();

        let payments = store.lock().unwrap().get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 1, "a transient failure within the retry budget must not lose the payment");
        assert_eq!(payments[0].block_height, Some(50));
    }

    #[tokio::test]
    async fn a_rescan_still_fails_once_failures_exceed_the_retry_budget() {
        let (store, key_custody, handle, tenant_id, _order_id) = setup().await;
        let inner = FakeDaemonClient::new();
        for h in 1..=100 {
            inner.push_block(&format!("blk_{h}"), vec![]);
        }
        // Persistently failing (well past the retry budget) must still surface as a
        // real error, not be silently swallowed or retried forever.
        let daemon = FlakyBlockTransactionsDaemon::new(inner, RESCAN_STEP_MAX_ATTEMPTS * 10);
        let store = store.into_shared();

        let err = rescan_order(&store, &key_custody, &daemon, &tenant_id, handle, 1, 1, 100, 2000, |_| {}).await;
        assert!(err.is_err(), "must genuinely fail once the retry budget is exhausted, not hang or succeed");
    }

    /// Builds a chain of `height` filler blocks with `tx` mined at `tx_at`, and
    /// records every one of those blocks as already-scanned in `store` (as they
    /// realistically would be by the live scanner, which keeps running against this
    /// same network the whole time an order sits `expired`) - so a later reorg test
    /// against this chain has something to compare against.
    fn chain_with_tx_at(store: &Store, height: u64, tx_at: u64, tx: &Transaction, prefix: &str) -> FakeDaemonClient {
        let daemon = FakeDaemonClient::new();
        for h in 1..=height {
            let txs = if h == tx_at { vec![tx.clone()] } else { vec![] };
            daemon.push_block(&format!("{prefix}_{h}"), txs);
        }
        for h in 1..=height {
            store.set_scanned_block("mainnet", h, &format!("{prefix}_{h}")).unwrap();
        }
        daemon
    }

    #[tokio::test]
    async fn rescan_order_walks_a_historical_range_and_finds_a_late_payment() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let daemon = chain_with_tx_at(&store, 105, 60, &tx, "hist");
        let store = store.into_shared();

        rescan_order(&store, &key_custody, &daemon, &tenant_id, handle, 1, 1, 105, 2000, |_| {})
            .await
            .unwrap();

        let s = store.lock().unwrap();
        let payments = s.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].block_height, Some(60));
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert!(
            matches!(order.status, crate::status::OrderStatus::Paid | crate::status::OrderStatus::Overpaid),
            "a rescan-found payment must recompute the order's status, not just record the payment row: {:?}",
            order.status
        );
    }

    /// WBS 1.4: a payment broadcast but not yet mined by the time the historical
    /// walk reaches `to_height` must still be found via the final mempool pass, at
    /// zero confirmations - not silently missed because it was never in a block.
    #[tokio::test]
    async fn rescan_order_finds_a_still_unconfirmed_payment_via_the_final_mempool_check() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let daemon = FakeDaemonClient::new();
        for h in 1..=10 {
            daemon.push_block(&format!("filler_{h}"), vec![]);
        }
        daemon.set_mempool(vec![tx]);
        let store = store.into_shared();

        rescan_order(&store, &key_custody, &daemon, &tenant_id, handle, 1, 1, 10, 2000, |_| {})
            .await
            .unwrap();

        let s = store.lock().unwrap();
        let payments = s.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].block_height, None, "found via the mempool, not a block");
    }

    /// The one genuinely new piece of behavior this whole feature adds (WBS 1.2):
    /// a rescan job that was already partway through when the process stopped must
    /// resume from its own persisted `current_height`, not restart from
    /// `from_height` - re-walking already-scanned blocks would be wasted work at
    /// best and, for a long rescan, could make a restart-heavy deployment never
    /// converge.
    #[tokio::test]
    async fn a_resumed_rescan_continues_from_its_persisted_current_height_not_from_height() {
        let (store, key_custody, handle, tenant_id, _order_id) = setup().await;
        let now = 2000;

        let job = store
            .trigger_rescan(
                NewOrderRescan {
                    order_id: _order_id.clone(),
                    tenant_id: tenant_id.clone(),
                    minor_index: 1,
                    mode: RescanMode::Simple,
                    from_height: 1,
                    to_height: 110,
                },
                now,
            )
            .unwrap()
            .into_job();
        // Simulate the process having already made it to height 50 (persisted) before
        // it died mid-job - exactly the state a restart finds a still-`running` row in.
        store.update_rescan_progress(&job.id, 50, now).unwrap();

        let inner = FakeDaemonClient::new();
        for h in 1..=110 {
            inner.push_block(&format!("blk_{h}"), vec![]);
        }
        let daemon = HeightRecordingDaemon::new(inner);
        let store = store.into_shared();

        run_rescan_job(&store, &key_custody, &daemon, handle, &job.id).await;

        let heights_seen = daemon.heights_seen.lock().unwrap();
        assert_eq!(
            heights_seen.iter().min().copied(),
            Some(50),
            "must resume at the persisted current_height, not redo from_height=1"
        );
        assert_eq!(
            heights_seen.iter().max().copied(),
            Some(110),
            "must still reach the same original to_height"
        );
        drop(heights_seen);

        let resumed = store.lock().unwrap().get_rescan(&job.id).unwrap().unwrap();
        assert_eq!(resumed.status, crate::store::RescanStatus::Completed);
        assert_eq!(resumed.current_height, 110);
    }

    /// A second trigger while one rescan is already `running` for a tenant must not
    /// start a competing job - it hands back the existing row instead (WBS 1.2
    /// decision 4's one-job-per-tenant guardrail).
    #[tokio::test]
    async fn triggering_a_second_rescan_while_one_is_running_returns_the_existing_job() {
        let (store, _key_custody, _handle, tenant_id, order_id) = setup().await;
        let first = store
            .trigger_rescan(
                NewOrderRescan {
                    order_id: order_id.clone(),
                    tenant_id: tenant_id.clone(),
                    minor_index: 1,
                    mode: RescanMode::Simple,
                    from_height: 1,
                    to_height: 100,
                },
                1000,
            )
            .unwrap();
        assert!(matches!(first, crate::store::TriggerRescanOutcome::Started(_)));
        let first = first.into_job();

        let second = store
            .trigger_rescan(
                NewOrderRescan {
                    order_id,
                    tenant_id,
                    minor_index: 1,
                    mode: RescanMode::Advanced,
                    from_height: 5,
                    to_height: 200,
                },
                1001,
            )
            .unwrap();
        assert!(
            matches!(second, crate::store::TriggerRescanOutcome::AlreadyRunning(_)),
            "a caller that spawns on `Started` but not `AlreadyRunning` must be able to tell these apart"
        );
        let second = second.into_job();

        assert_eq!(second.id, first.id, "must return the existing running row, not start a new one");
        assert_eq!(second.mode, RescanMode::Simple, "unchanged - the second trigger's request never took effect");
    }

    /// Proves the reliance described in `rescan_order`'s own doc comment actually
    /// holds, not just reasoned about: a payment a rescan records - even one that
    /// immediately settles the order - still gets walked back by the ordinary reorg
    /// path if the chain that carried it is later orphaned. This is *why* the
    /// rescan itself scans all the way to the tip with no held-back buffer.
    #[tokio::test]
    async fn a_rescan_found_payment_is_still_walked_back_by_a_later_reorg() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let daemon = chain_with_tx_at(&store, 100, 80, &tx, "old");
        let store = store.into_shared();

        rescan_order(&store, &key_custody, &daemon, &tenant_id, handle, 1, 1, 100, 2000, |_| {}).await.unwrap();
        {
            let s = store.lock().unwrap();
            let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
            assert!(
                matches!(order.status, crate::status::OrderStatus::Paid | crate::status::OrderStatus::Overpaid),
                "must have settled off the rescan-found payment before the reorg: {:?}",
                order.status
            );
        }

        // The chain that carried the payment is now orphaned: it never mined that
        // transaction, and its inputs are proven spent by something else.
        for ki in &key_images_of(&tx) {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        reorg_to_new_chain(&daemon, 80, 100, None);

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 3000).await.unwrap();
        assert_eq!(report.reorg_detected_at, Some(80));
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);

        let s = store.lock().unwrap();
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(order.amount_received_piconero, 0, "the rescan-found payment must stop counting once orphaned");
        assert_eq!(order.status, crate::status::OrderStatus::Pending);
        assert!(order.double_spend_detected_at.is_some());
    }

    #[tokio::test]
    async fn scanning_a_mempool_tx_records_a_payment_against_the_right_order() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();

        let touched = scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, None)
            .await
            .unwrap();

        assert_eq!(touched, HashSet::from([order_id.clone()]));
        let payments = store.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 1);
        assert!(payments[0].amount_piconero > 0);
        assert_eq!(payments[0].block_height, None); // mempool-only
    }

    #[tokio::test]
    async fn rescanning_the_same_mempool_tx_does_not_duplicate_the_payment() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();

        for _ in 0..3 {
            scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, None)
                .await
                .unwrap();
        }
        assert_eq!(store.get_all_payments(&order_id).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn reorg_moving_a_tx_to_a_different_block_updates_height_without_voiding() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();

        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, Some(50))
            .await
            .unwrap();
        store.set_scanned_block("mainnet", 50, "hash_50_v1").unwrap();

        let daemon = FakeDaemonClient::new();
        // Fast-forward the fake daemon's internal height counter to 49 by pushing
        // filler blocks, then reorg from 50 with the tx moved to the new height 51.
        let mut last_height = 0;
        while last_height < 49 {
            last_height = daemon.push_block(&format!("filler_{last_height}"), vec![]);
        }
        daemon.reorg_from(50, vec![("hash_50_v2", vec![]), ("hash_51_v2", vec![tx.clone()])]);

        let store = store.into_shared();
        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.unwrap();
        assert_eq!(report.reorg_detected_at, Some(50));
        assert!(report.dirty_orders.contains(&order_id));
        assert!(report.double_spent_orders.is_empty());

        let payments = store.lock().unwrap().get_all_payments(&order_id).unwrap();
        assert_eq!(payments[0].block_height, Some(51));
        assert!(payments[0].voided_at.is_none());
    }

    #[tokio::test]
    async fn reorg_where_tx_vanishes_and_key_image_proven_spent_elsewhere_voids_and_flags_double_spend() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let txid = tx_id_hex(&tx);
        let key_images = key_images_of(&tx);
        assert!(!key_images.is_empty(), "fixture tx must have at least one input to test key-image proof");

        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, Some(50))
            .await
            .unwrap();
        store.set_scanned_block("mainnet", 50, "hash_50_v1").unwrap();

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
        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.unwrap();
        assert_eq!(report.reorg_detected_at, Some(50));
        assert_eq!(report.dirty_orders, vec![order_id.clone()]);
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);

        let store = store.lock().unwrap();
        let payments = store.get_all_payments(&order_id).unwrap();
        assert!(payments[0].voided_at.is_some());
        let order = store.get_order(&tenant_id, &order_id).unwrap().unwrap();
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

        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, Some(50))
            .await
            .unwrap();
        store.set_scanned_block("mainnet", 50, "hash_50_v1").unwrap();

        let daemon = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = daemon.push_block(&format!("filler_{last_height}"), vec![]);
        }
        daemon.reorg_from(50, vec![("hash_50_v2", vec![])]);
        // Deliberately do NOT mark the key images as spent - default is Unspent.

        let store = store.into_shared();
        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.unwrap();
        assert!(report.double_spent_orders.is_empty());

        let payments = store.lock().unwrap().get_all_payments(&order_id).unwrap();
        assert!(payments[0].voided_at.is_none(), "must not void on ambiguous (unspent) evidence");
    }

    #[tokio::test]
    async fn no_reorg_when_hashes_still_match_is_a_cheap_no_op() {
        let store = Store::open_in_memory().unwrap();
        store.set_scanned_block("mainnet", 50, "hash_50").unwrap();
        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 50 {
            let h = if last_height == 49 { "hash_50" } else { "filler" };
            last_height = daemon.push_block(h, vec![]);
        }

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.unwrap();
        assert_eq!(report.reorg_detected_at, None);
        assert!(report.dirty_orders.is_empty());
    }

    #[tokio::test]
    async fn run_scan_tick_matches_mempool_tx_recomputes_status_and_enqueues_a_webhook() {
        // End-to-end proof of the composed orchestration, not just its pieces: a
        // real transaction sitting in a fake daemon's mempool gets matched, the
        // owning order's status transitions (pending -> unconfirmed, since this is
        // a 0-conf-only match with no zero-conf trust ceiling configured), and
        // exactly one webhook delivery is enqueued for that transition.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let webhook = store.create_webhook(&tenant_id, "https://merchant.example/hook", "{}", "whsec_x", 1000).unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]); // the fake starts at height 0 with no block there at all; give the first-run tip bootstrap something real to seed from
        daemon.set_mempool(vec![fixture_tx()]);

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(order.status, crate::status::OrderStatus::Unconfirmed);
        assert!(order.amount_received_piconero > 0);

        let due = s.due_webhook_deliveries(crate::now_unix() + 1, 10).unwrap();
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
    ) -> (String, WalletHandle, String) {
        let handle = key_custody.register_wallet(WalletMaterial::new(view_key, spend_pubkey)).await.unwrap();
        let tenant = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4x".into(),
                    network: "mainnet".into(),
                    allowed_origins: vec![],
                    confirmations_required: Some(10),
                    zero_conf_max_piconero: None,
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();
        let index = store.allocate_minor_index(&tenant.tenant.id).unwrap();
        let address = key_custody
            .derive_subaddress(handle, SubaddressIndex { major: 0, minor: index }, Network::Mainnet)
            .await
            .unwrap();
        let order = store
            .create_order(NewOrder {
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
        (tenant.tenant.id, handle, order.id)
    }

    fn arbitrary_wallet_material(seed: u8) -> ([u8; 32], [u8; 32]) {
        let mut view_bytes = [seed; 32];
        view_bytes[31] &= 0x0f;
        let mut spend_seed = [seed.wrapping_add(1); 32];
        spend_seed[31] &= 0x0f;
        let secret_spend = PrivateKey::from_slice(&spend_seed).unwrap();
        (view_bytes, PublicKey::from_private_key(&secret_spend).to_bytes())
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

        let (tenant_a, handle_a, _order_a) =
            tenant_with_pending_order(&store, &key_custody, fixture_view_key(), fixture_spend_pubkey()).await;

        let (view_b, spend_b) = arbitrary_wallet_material(0x70);
        let handle_b = key_custody.register_wallet(WalletMaterial::new(view_b, spend_b)).await.unwrap();
        let tenant_b = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4b".into(),
                    network: "mainnet".into(),
                    allowed_origins: vec![],
                    confirmations_required: None,
                    zero_conf_max_piconero: None,
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();

        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.set_mempool(vec![fixture_tx()]);

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_a, handle_a), (tenant_b.tenant.id, handle_b)], 20, 0)
            .await
            .unwrap();

        assert_eq!(
            key_custody.scan_calls.load(Ordering::SeqCst),
            1,
            "exactly one scan call expected: tenant A (has a pending order) scanned once, tenant B (no orders ever) never scanned"
        );
    }

    #[tokio::test]
    async fn run_scan_tick_never_scans_a_tenant_on_a_different_network_even_if_included_in_the_input_list() {
        // Defense-in-depth for the multi-network design (§DESIGN.md §7): a
        // mainnet-network tick must not touch a pending stagenet tenant's data at
        // all, even though `active_tenant_ids` already scopes by network - this
        // proves the *second*, independent filter (`t.network == network` in
        // `run_scan_tick` itself) actually engages, guarding against a caller (e.g.
        // a future refactor of `main.rs`'s scanner loop) accidentally passing an
        // unfiltered, all-networks tenant list into a single network's tick.
        let key_custody = CountingKeyCustody::default();
        let store = Store::open_in_memory().unwrap();

        let (mainnet_tenant, mainnet_handle, _order) =
            tenant_with_pending_order(&store, &key_custody, fixture_view_key(), fixture_spend_pubkey()).await;

        let (view_b, spend_b) = arbitrary_wallet_material(0x50);
        let stagenet_handle = key_custody.register_wallet(WalletMaterial::new(view_b, spend_b)).await.unwrap();
        let stagenet_tenant = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "5stagenet".into(),
                    network: "stagenet".into(),
                    allowed_origins: vec![],
                    confirmations_required: Some(10),
                    zero_conf_max_piconero: None,
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();
        // A genuinely pending order on stagenet - if the network filter didn't
        // work, this tenant would be scanned too.
        store
            .create_order(NewOrder {
                confirmations_required_override: None,
                tenant_id: stagenet_tenant.tenant.id.clone(),
                merchant_order_id: None,
                minor_index: store.allocate_minor_index(&stagenet_tenant.tenant.id).unwrap(),
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
            &[(mainnet_tenant, mainnet_handle), (stagenet_tenant.tenant.id, stagenet_handle)],
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
        let (tenant_id, _handle, order_id) =
            tenant_with_pending_order(&store, &key_custody, fixture_view_key(), fixture_spend_pubkey()).await;
        let handle = key_custody
            .register_wallet(WalletMaterial::new(fixture_view_key(), fixture_spend_pubkey()))
            .await
            .unwrap();
        // (Re-registering under the same key material is fine for this test - only
        // the handle identity matters for routing scan calls, not which handle
        // number KeyCustody happened to assign.)

        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.set_mempool(vec![fixture_tx()]);

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(key_custody.scan_calls.load(Ordering::SeqCst), 1);

        // Force the order to a terminal state the way "10 confirmations later"
        // naturally would: give the payment tick 1 already recorded a real block
        // height (rather than layering on a second payment, which would leave the
        // first one's 0-conf status dragging min-confirmations back to 0) and
        // recompute - the same public API the real block-scanning path uses.
        {
            let s = store.lock().unwrap();
            let txid = tx_id_hex(&fixture_tx());
            s.update_payment_block_height(&order_id, &txid, 1, Some(1)).unwrap(); // output_index 1, per the fixture's known match (see key_custody::plain's tests)
            let (_, new_status) = s.recompute_order_status(&order_id, 100, 1600).unwrap(); // 100 confirmations
            assert!(matches!(new_status, crate::status::OrderStatus::Paid | crate::status::OrderStatus::Overpaid));
        }

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id, handle)], 20, 0).await.unwrap();
        assert_eq!(
            key_custody.scan_calls.load(Ordering::SeqCst),
            1,
            "tenant settled after tick 1 - tick 2 must not invoke KeyCustody for it again, even with the same mempool tx still present"
        );
    }

    #[tokio::test]
    async fn run_scan_tick_on_first_run_seeds_near_the_current_tip_instead_of_scanning_all_history() {
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

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id, handle)], 20, 0).await.unwrap();

        assert_eq!(store.lock().unwrap().max_scanned_height("mainnet").unwrap(), Some(10));
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

        let result = run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await;
        assert!(result.is_ok(), "must not hard-fail the tick just because the tip block isn't fetchable yet");
        // Falls back to seeding one behind the (unfetchable) reported tip, i.e.
        // height 10, which *does* exist.
        assert_eq!(store.lock().unwrap().max_scanned_height("mainnet").unwrap(), Some(10));

        // Once the block-serving backend catches up, a later tick proceeds
        // normally from where the fallback seeded it.
        daemon.push_block("h10_actual", vec![]);
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id, handle)], 20, 0).await.unwrap();
        assert_eq!(store.lock().unwrap().max_scanned_height("mainnet").unwrap(), Some(11));
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
        };

        let touched = record_scan_match(&store, &tenant_id, &scan, 1500, Some(50)).unwrap();
        assert!(touched.is_empty(), "an unmeasurable output must not even mark the order as needing a recompute");
        assert!(
            store.get_all_payments(&order_id).unwrap().is_empty(),
            "nothing may be persisted for an output whose amount could not be recovered"
        );

        // The order is untouched, so a later tick that *can* decrypt the amount is
        // still free to record it properly.
        let order = store.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(order.status, crate::status::OrderStatus::Pending);
        assert_eq!(order.amount_received_piconero, 0);
    }

    #[tokio::test]
    async fn a_store_failure_recording_a_block_match_leaves_that_block_unscanned_for_the_next_tick() {
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
            .unwrap()
            .execute_raw_for_test(
                "CREATE TRIGGER simulated_write_failure BEFORE INSERT ON order_payments
                 BEGIN SELECT RAISE(ABORT, 'simulated store failure'); END;",
            )
            .unwrap();

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(
            store.lock().unwrap().max_scanned_height("mainnet").unwrap(),
            Some(1),
            "block 2's match failed to record - marking it scanned would lose that payment permanently"
        );
        assert!(store.lock().unwrap().get_all_payments(&order_id).unwrap().is_empty());

        // Once the store is healthy again, the very next tick re-covers the block it
        // deliberately left behind.
        store.lock().unwrap().execute_raw_for_test("DROP TRIGGER simulated_write_failure;").unwrap();
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id, handle)], 20, 0).await.unwrap();

        assert_eq!(store.lock().unwrap().max_scanned_height("mainnet").unwrap(), Some(2));
        let payments = store.lock().unwrap().get_all_payments(&order_id).unwrap();
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
        let webhook = store.create_webhook(&tenant_id, "https://merchant.example/hook", "{}", "whsec_x", 1000).unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![fixture_tx()]); // the payment lands at height 2

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        {
            let s = store.lock().unwrap();
            let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
            assert_eq!(order.status, crate::status::OrderStatus::Confirming, "one confirmation, ten required");
        }

        // Nine more blocks, none of which contain anything for this order at all.
        for i in 3..=11 {
            daemon.push_block(&format!("h{i}"), vec![]);
        }
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(
            order.status,
            crate::status::OrderStatus::Overpaid, // the fixture tx pays more than this order's trivially-small expected amount
            "10 confirmations deep - nothing new matched, but the chain moved"
        );
        assert_eq!(order.confirmations, 10);

        // And the transition the merchant is actually waiting on was announced.
        let events: Vec<String> = s
            .due_webhook_deliveries(crate::now_unix() + 1, 10)
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
        let webhook = store.create_webhook(&tenant_id, "https://merchant.example/hook", "{}", "whsec_x", 1000).unwrap();

        // A second order on the same tenant, already past its deadline and never paid.
        let stale = store
            .create_order(NewOrder {
                confirmations_required_override: None,
                tenant_id: tenant_id.clone(),
                merchant_order_id: None,
                minor_index: store.allocate_minor_index(&tenant_id).unwrap(),
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

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        assert_eq!(
            s.get_order(&tenant_id, &stale.id).unwrap().unwrap().status,
            crate::status::OrderStatus::Expired
        );
        let expired_events = s
            .due_webhook_deliveries(crate::now_unix() + 1, 10)
            .unwrap()
            .into_iter()
            .filter(|d| d.order_id == stale.id)
            .map(|d| d.event_type)
            .collect::<Vec<_>>();
        assert_eq!(expired_events, vec!["order.expired".to_string()]);
        let _ = webhook;
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
        let webhook = store.create_webhook(&tenant_id, "https://merchant.example/hook", "{}", "whsec_x", 1000).unwrap();
        let tx = fixture_tx();

        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, Some(50))
            .await
            .unwrap();
        store.set_scanned_block("mainnet", 50, "hash_50_v1").unwrap();
        let (_, status) = store.recompute_order_status(&order_id, 100, 1600).unwrap();
        assert_eq!(status, crate::status::OrderStatus::Overpaid, "51 confirmations deep before the reorg");

        let store = store.into_shared();
        let daemon = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = daemon.push_block(&format!("filler_{last_height}"), vec![]);
        }
        // The transaction survives the reorg but is remined at the new tip, so it is
        // suddenly one confirmation deep again instead of fifty-one.
        daemon.reorg_from(50, vec![("hash_50_v2", vec![]), ("hash_51_v2", vec![tx.clone()])]);

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.unwrap();
        assert_eq!(report.reorg_detected_at, Some(50));
        assert!(report.double_spent_orders.is_empty(), "a remined transaction is not a double-spend");

        let s = store.lock().unwrap();
        assert_eq!(
            s.get_order(&tenant_id, &order_id).unwrap().unwrap().status,
            crate::status::OrderStatus::Confirming
        );
        let due = s.due_webhook_deliveries(crate::now_unix() + 1, 10).unwrap();
        assert_eq!(due.len(), 1, "the paid -> confirming transition must be announced exactly once");
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
        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, Some(50))
            .await
            .unwrap();
        store.set_scanned_block("mainnet", 50, "hash_50_v1").unwrap();
        let store = store.into_shared();

        let inner = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = inner.push_block(&format!("filler_{last_height}"), vec![]);
        }
        inner.reorg_from(50, vec![("hash_50_v2", vec![]), ("hash_51_v2", vec![tx.clone()])]);

        let failing = DaemonFailingLocate { inner };
        assert!(
            check_for_reorg_and_reconcile(&store, &failing, "mainnet", 20, 2000).await.is_err(),
            "the node failure must surface, not be swallowed"
        );
        assert_eq!(
            store.lock().unwrap().get_scanned_block_hash("mainnet", 50).unwrap(),
            Some("hash_50_v1".to_string()),
            "the stored hash must still describe the old chain, or nothing will ever notice the reorg again"
        );
        assert_eq!(
            store.lock().unwrap().get_all_payments(&order_id).unwrap()[0].block_height,
            Some(50),
            "and the payment is still unreconciled, as the failed pass left it"
        );

        // The retry a healthy next tick performs now works, because the evidence the
        // detection depends on is still there.
        let report = check_for_reorg_and_reconcile(&store, &failing.inner, "mainnet", 20, 2100).await.unwrap();
        assert_eq!(report.reorg_detected_at, Some(50));
        assert_eq!(store.lock().unwrap().get_all_payments(&order_id).unwrap()[0].block_height, Some(51));
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
            store.set_scanned_block("mainnet", h, &format!("old_{h}")).unwrap();
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

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        {
            let s = store.lock().unwrap();
            assert_eq!(
                s.max_scanned_height("mainnet").unwrap(),
                Some(49),
                "the high-water mark must fall back below the reorg point"
            );
            assert!(s.get_all_payments(&order_id).unwrap().is_empty(), "nothing found yet - this tick only rewound");
        }

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        assert_eq!(s.max_scanned_height("mainnet").unwrap(), Some(51), "and forward scanning caught back up");
        let payments = s.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 1, "the payment that exists only in the replacement chain was found");
        assert_eq!(payments[0].block_height, Some(50));
    }

    #[tokio::test]
    async fn a_voided_payment_is_restored_when_its_transaction_returns_to_the_chain() {
        // Voiding is a conclusion drawn from a chain state that can itself change:
        // the replacement transaction that proved the double-spend can be reorged
        // out in turn, putting the original back on the canonical chain. Because
        // `find_payments_at_or_after_height` filters voided rows out, a voided
        // payment was invisible to every future reconciliation pass and the
        // merchant's genuinely-paid order stayed permanently short.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let tx = fixture_tx();
        let key_images = key_images_of(&tx);

        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, Some(50))
            .await
            .unwrap();
        store.set_scanned_block("mainnet", 50, "hash_50_v1").unwrap();
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

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.unwrap();
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);
        assert!(store.lock().unwrap().get_all_payments(&order_id).unwrap()[0].voided_at.is_some());

        // Now the attacker's replacement chain loses, and the original transaction is
        // back in a block. (The forward scan would have recorded the new hash for
        // height 50 between the two reconciliations; do that by hand here.)
        store.lock().unwrap().set_scanned_block("mainnet", 50, "hash_50_v2").unwrap();
        daemon.reorg_from(50, vec![("hash_50_v3", vec![tx.clone()])]);

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2100).await.unwrap();
        assert_eq!(report.reorg_detected_at, Some(50));
        assert!(report.dirty_orders.contains(&order_id));

        let s = store.lock().unwrap();
        let payment = s.get_all_payments(&order_id).unwrap().remove(0);
        assert!(payment.voided_at.is_none(), "the payment is canonical again and must count towards the order");
        assert_eq!(payment.block_height, Some(50));
        // Sticky by design: a double-spend attempt was genuinely observed on this
        // order, and that remains true regardless of how the chain settled.
        assert!(s.get_order(&tenant_id, &order_id).unwrap().unwrap().double_spend_detected_at.is_some());
    }

    #[tokio::test]
    async fn every_webhook_payload_carries_a_stable_event_id_and_a_timestamp() {
        // The payload used to be just `{payment_id, status}`, which gives a receiver
        // nothing to dedupe on (a retry of a lost-ack delivery is byte-identical to a
        // genuine second transition to the same status) and nothing to bound a replay
        // with (a captured delivery stays valid forever). Both the id and the
        // timestamp live *inside* the signed body, not only in headers.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        store.create_webhook(&tenant_id, "https://merchant.example/hook", "{}", "whsec_x", 1000).unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![fixture_tx()]);
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let first_payload: serde_json::Value = {
            let s = store.lock().unwrap();
            let due = s.due_webhook_deliveries(crate::now_unix() + 1, 10).unwrap();
            assert_eq!(due.len(), 1);
            serde_json::from_str(&due[0].payload_json).unwrap()
        };
        assert_eq!(first_payload["payment_id"], serde_json::json!(order_id));
        assert_eq!(first_payload["status"], serde_json::json!("confirming"));
        assert_eq!(first_payload["event"], serde_json::json!("order.confirming"));
        assert!(
            first_payload["event_id"].as_str().is_some_and(|id| id.starts_with("evt_")),
            "every event needs an id a receiver can dedupe on: {first_payload}"
        );
        assert!(first_payload["created_at"].as_i64().is_some(), "and a timestamp to bound replays with");

        // A retry of the same delivery re-sends the identical, identically-signed
        // body - the id must identify the *event*, not the attempt.
        {
            let s = store.lock().unwrap();
            let due = s.due_webhook_deliveries(crate::now_unix() + 1, 10).unwrap();
            s.schedule_webhook_retry(due[0].delivery_id, 0, Some(500), Some("boom"), crate::now_unix()).unwrap();
            let retried = s.due_webhook_deliveries(crate::now_unix() + 1, 10).unwrap();
            let retried_payload: serde_json::Value = serde_json::from_str(&retried[0].payload_json).unwrap();
            assert_eq!(retried_payload["event_id"], first_payload["event_id"]);
        }

        // A genuinely different transition gets a genuinely different id.
        for i in 3..=11 {
            daemon.push_block(&format!("h{i}"), vec![]);
        }
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id, handle)], 20, 0).await.unwrap();
        let s = store.lock().unwrap();
        let paid = s
            .due_webhook_deliveries(crate::now_unix() + 1, 10)
            .unwrap()
            .into_iter()
            .find(|d| d.event_type == "order.overpaid")
            .expect("the settled transition must have been enqueued");
        let settled_payload: serde_json::Value = serde_json::from_str(&paid.payload_json).unwrap();
        assert_ne!(settled_payload["event_id"], first_payload["event_id"]);
    }

    #[tokio::test]
    async fn a_block_whose_hash_cannot_be_read_stops_the_range_instead_of_leaving_a_gap() {
        // Every other failure inside the height loop abandons the range without
        // marking the height scanned; reading the block's *hash* was the one step
        // whose failure was silently ignored, and it is the step that decides whether
        // the height is recorded at all. Skipping it while carrying on to the next
        // height leaves a hole: height H unrecorded, H+1 onwards recorded.
        //
        // A hole is worse than a stopping point, because reorg detection skips
        // heights it has no stored hash for. A later reorg genuinely starting at H is
        // then first noticed at H+1, so the reported reorg point is one block too
        // high - the payments actually orphaned at H are never re-evaluated (they go
        // on counting towards their order at a height that no longer exists) and
        // block H of the replacement chain is never rescanned.
        let (store, key_custody, handle, tenant_id, _order_id) = setup().await;
        store.set_scanned_block("mainnet", 1, "h1").unwrap();
        let store = store.into_shared();

        let inner = FakeDaemonClient::new();
        for i in 1..=5 {
            inner.push_block(&format!("h{i}"), vec![]);
        }
        let daemon = DaemonFailingBlockHashAt { inner, failing_height: AtomicU64::new(3) };

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        {
            let s = store.lock().unwrap();
            assert_eq!(
                s.max_scanned_height("mainnet").unwrap(),
                Some(2),
                "the range must stop at the height whose hash could not be read, not step over it"
            );
            for h in 3..=5 {
                assert!(
                    s.get_scanned_block_hash("mainnet", h).unwrap().is_none(),
                    "block {h} must not be recorded once the range was abandoned at 3"
                );
            }
        }

        // And the next healthy tick re-covers the whole abandoned range, leaving a
        // contiguous window with nothing missing from the middle of it.
        daemon.failing_height.store(NO_FAILING_HEIGHT, Ordering::SeqCst);
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id, handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        assert_eq!(s.max_scanned_height("mainnet").unwrap(), Some(5));
        for h in 1..=5 {
            assert!(s.get_scanned_block_hash("mainnet", h).unwrap().is_some(), "block {h} must be recorded");
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
        store.set_scanned_block("mainnet", 50, "hash_50_v1").unwrap();
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
        let daemon = DaemonFailingBlockHashAt { inner, failing_height: AtomicU64::new(49) };

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        {
            let s = store.lock().unwrap();
            assert_eq!(
                s.get_scanned_block_hash("mainnet", 50).unwrap().as_deref(),
                Some("hash_50_v1"),
                "with no anchor available, the losing chain's hash must stay put so the reorg stays detectable"
            );
            assert!(
                s.max_scanned_height("mainnet").unwrap().is_some(),
                "the window must never be emptied by a rewind that cannot re-anchor - an empty window reads \
                 as 'never scanned' and re-seeds at the tip"
            );
        }

        // The node recovers; the very next tick completes the rewind it deferred.
        daemon.failing_height.store(NO_FAILING_HEIGHT, Ordering::SeqCst);
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(
            store.lock().unwrap().max_scanned_height("mainnet").unwrap(),
            Some(49),
            "the rewind must now land on the common ancestor"
        );

        // ...and the tick after that forward-scans the replacement chain and finds the
        // payment that only ever existed there.
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id, handle)], 20, 0).await.unwrap();
        let s = store.lock().unwrap();
        assert_eq!(s.max_scanned_height("mainnet").unwrap(), Some(51));
        let payments = s.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 1, "the payment that exists only in the replacement chain must still be found");
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
        store.lock().unwrap().set_scanned_block("mainnet", 1, "h1").unwrap();

        let webhook = store
            .lock()
            .unwrap()
            .create_webhook(&tenant_id, "https://merchant.example/hook", "{}", "whsec_x", 1000)
            .unwrap();

        // Fail only writes to `scanned_blocks`, leaving every other table (and every
        // read) working - a write failure at exactly that one step, not a broken
        // database.
        store
            .lock()
            .unwrap()
            .execute_raw_for_test(
                "CREATE TRIGGER simulated_scanned_block_write_failure BEFORE INSERT ON scanned_blocks
                 BEGIN SELECT RAISE(ABORT, 'simulated store failure'); END;",
            )
            .unwrap();

        let result = run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await;
        assert!(result.is_ok(), "a failure marking a block scanned must abandon the range, not the whole tick");

        {
            let s = store.lock().unwrap();
            assert_eq!(
                s.max_scanned_height("mainnet").unwrap(),
                Some(1),
                "block 2 must stay unscanned so the next tick retries it"
            );
            // The mempool half of the tick must have survived intact: the payment is
            // recorded, the status was recomputed off it, and the transition was
            // announced.
            let payments = s.get_all_payments(&order_id).unwrap();
            assert_eq!(payments.len(), 1);
            assert_eq!(payments[0].block_height, None, "still mempool-only");
            let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
            assert_ne!(
                order.status,
                crate::status::OrderStatus::Pending,
                "the mempool match's status recompute must not have been discarded"
            );
            let due = s.due_webhook_deliveries(crate::now_unix() + 1, 10).unwrap();
            assert!(
                due.iter().any(|d| d.webhook_id == webhook.id && d.event_type.starts_with("order.")),
                "the mempool match's status-transition webhook must not have been discarded"
            );
        }

        // And the block the tick deliberately left behind is covered once the store
        // is healthy again.
        store
            .lock()
            .unwrap()
            .execute_raw_for_test("DROP TRIGGER simulated_scanned_block_write_failure;")
            .unwrap();
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id, handle)], 20, 0).await.unwrap();
        assert_eq!(store.lock().unwrap().max_scanned_height("mainnet").unwrap(), Some(2));
    }

    #[tokio::test]
    async fn a_reorg_back_to_the_oldest_recorded_block_does_not_look_like_a_scanner_that_never_ran() {
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
        store.set_scanned_block("mainnet", 50, "hash_50_v1").unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = daemon.push_block(&format!("filler_{last_height}"), vec![]);
        }
        // The fork replaces 50 and 51, and the payment lives only in the new 50.
        daemon.reorg_from(50, vec![("new_50", vec![fixture_tx()]), ("new_51", vec![])]);

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(
            store.lock().unwrap().max_scanned_height("mainnet").unwrap(),
            Some(49),
            "the rewind must leave the high-water mark at the common ancestor, not at nothing at all"
        );

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id, handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        assert_eq!(s.max_scanned_height("mainnet").unwrap(), Some(51));
        let payments = s.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 1, "the payment that exists only in the replacement chain was found");
        assert_eq!(payments[0].block_height, Some(50));
    }

    #[tokio::test]
    async fn a_tick_that_detects_a_reorg_never_announces_paid_from_the_chain_it_is_about_to_discard() {
        // Ordering, not logic: reconciliation and the per-tick recompute sweep both
        // ran, but the sweep ran first, so on the one tick where a reorg is detected
        // every non-terminal order was evaluated against heights reconciliation was
        // about to invalidate. An order whose payment had just been orphaned could
        // therefore cross its confirmation threshold and fire `order.paid` seconds
        // before the same tick voided that payment and fired the retraction - and a
        // merchant who acted on `order.paid` has already shipped.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await; // confirmations_required = 10
        store.create_webhook(&tenant_id, "https://merchant.example/hook", "{}", "whsec_x", 1000).unwrap();
        let tx = fixture_tx();
        let key_images = key_images_of(&tx);

        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, Some(50))
            .await
            .unwrap();
        store.set_scanned_block("mainnet", 50, "hash_50_v1").unwrap();
        let (_, status) = store.recompute_order_status(&order_id, 50, 1600).unwrap();
        assert_eq!(status, crate::status::OrderStatus::Confirming, "one confirmation, ten required");
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
        let replacement: Vec<(&str, Vec<Transaction>)> =
            ["v2_50", "v2_51", "v2_52", "v2_53", "v2_54", "v2_55", "v2_56", "v2_57", "v2_58", "v2_59", "v2_60"]
                .into_iter()
                .map(|h| (h, vec![]))
                .collect();
        daemon.reorg_from(50, replacement);
        for ki in &key_images {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(order.amount_received_piconero, 0, "the voided payment must not count towards the order");
        assert!(order.double_spend_detected_at.is_some());

        let events: Vec<String> =
            s.due_webhook_deliveries(crate::now_unix() + 1, 20).unwrap().into_iter().map(|d| d.event_type).collect();
        assert!(
            !events.iter().any(|e| e == "order.paid" || e == "order.overpaid"),
            "no settlement may ever be announced from payment data the same tick is in the middle of retracting, got: {events:?}"
        );
    }

    #[tokio::test]
    async fn a_payment_a_reorg_dropped_to_the_mempool_can_still_be_voided_when_later_proven_double_spent() {
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

        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, Some(50))
            .await
            .unwrap();
        store.set_scanned_block("mainnet", 50, "hash_50_v1").unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        let mut last_height = 0;
        while last_height < 49 {
            last_height = daemon.push_block(&format!("filler_{last_height}"), vec![]);
        }
        // First reorg: the transaction falls out of its block and back into the pool.
        daemon.set_mempool(vec![tx.clone()]);
        daemon.reorg_from(50, vec![("hash_50_v2", vec![])]);

        check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.unwrap();
        assert_eq!(
            store.lock().unwrap().get_all_payments(&order_id).unwrap()[0].block_height,
            None,
            "reconciliation itself is what puts the row into the state that used to hide it"
        );

        // Later, the attacker's replacement confirms: the pooled transaction is
        // dropped for good and its inputs are provably consumed elsewhere.
        daemon.drop_from_mempool(&tx);
        for ki in &key_images {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        store.lock().unwrap().set_scanned_block("mainnet", 50, "hash_50_v2").unwrap();
        daemon.reorg_from(50, vec![("hash_50_v3", vec![])]);

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2100).await.unwrap();
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);

        let s = store.lock().unwrap();
        assert!(s.get_all_payments(&order_id).unwrap()[0].voided_at.is_some());
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
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
        store.create_webhook(&tenant_id, "https://merchant.example/hook", "{}", "whsec_x", 1000).unwrap();
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

        let result = run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await;
        assert!(result.is_err(), "the enqueue failure must surface, not be swallowed");
        assert_eq!(
            store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap().status,
            crate::status::OrderStatus::Pending,
            "the status must not have advanced past a transition nothing will ever announce"
        );

        // Once the store is healthy again, the next tick performs both halves.
        store.lock().unwrap().execute_raw_for_test("DROP TRIGGER simulated_enqueue_failure;").unwrap();
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        assert_eq!(
            s.get_order(&tenant_id, &order_id).unwrap().unwrap().status,
            crate::status::OrderStatus::Confirming
        );
        let due = s.due_webhook_deliveries(crate::now_unix() + 1, 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].event_type, "order.confirming");
    }

    // ---------------------------------------------------------------------
    // Scenario-coverage block: chain, network and adversarial conditions.
    // See the checklist at the top of this module for what maps to what.
    // ---------------------------------------------------------------------

    /// A second, distinct transaction paying the *same* subaddress with the *same*
    /// outputs and the *same* key images as `fixture_tx()`. Only `unlock_time`
    /// differs, which changes the transaction hash (it is part of the prefix, and
    /// the prefix is hashed) without touching the transaction public key, the
    /// output keys, or the amount commitments that scanning and amount recovery
    /// depend on.
    ///
    /// This is what makes multi-transaction and double-spend scenarios testable
    /// against real crypto with a single fixture: two variants are two genuinely
    /// different txids that both match the fixture view pair, and - because they
    /// share their inputs - they are also a realistic *conflicting pair*, which is
    /// exactly the shape of a Monero double-spend (same key images, different
    /// transaction).
    fn fixture_tx_variant(unlock_time: u64) -> Transaction {
        let mut tx = fixture_tx();
        tx.prefix.unlock_time = monero::VarInt(unlock_time);
        tx
    }

    /// Pushes `1..=height` onto a fresh fake chain, each block empty and hashed
    /// `{prefix}_{h}`, and records the same hashes into `store` for `from..=height`
    /// - i.e. a scanner that has already worked its way up this chain.
    fn chain_scanned_to(store: &Store, height: u64, recorded_from: u64, prefix: &str) -> FakeDaemonClient {
        let daemon = FakeDaemonClient::new();
        for h in 1..=height {
            daemon.push_block(&format!("{prefix}_{h}"), vec![]);
        }
        for h in recorded_from..=height {
            store.set_scanned_block("mainnet", h, &format!("{prefix}_{h}")).unwrap();
        }
        daemon
    }

    /// Replaces `fork..=tip` with a chain whose hashes are prefixed `new_`, keeping
    /// the (hash, txs) plumbing of `reorg_from`'s `&str` API out of every caller.
    fn reorg_to_new_chain(daemon: &FakeDaemonClient, fork: u64, tip: u64, tx_at: Option<(u64, Transaction)>) {
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
    async fn a_reorg_deeper_than_the_window_is_reported_at_the_window_edge_and_leaves_older_payments_alone() {
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
        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &fixture_tx(), 0..3, 1500, Some(79))
            .await
            .unwrap();
        reorg_to_new_chain(&daemon, 79, tip, None);
        let store = store.into_shared();

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.unwrap();
        assert_eq!(report.reorg_detected_at, Some(80));
        let payments = store.lock().unwrap().get_all_payments(&order_id).unwrap();
        assert_eq!(
            payments[0].block_height,
            Some(79),
            "a payment below the window is not re-evaluated - this is the documented cost of the window, \
             and the reason `reorg_check_depth` must exceed the deepest reorg a deployment wants to survive"
        );
    }

    #[tokio::test]
    async fn a_settled_order_is_walked_back_when_a_reorg_deeper_than_confirmations_required_orphans_its_payment() {
        // The September 2025 shape, at this system's defaults: an 18-block reorg
        // against `confirmations_required = 10` reverts blocks the merchant was
        // already told were final. Nothing in the reorg path branches on
        // `confirmations_required` (10 and 11 blocks deep take exactly the same code
        // path), so what needs proving is the end-to-end consequence: an order that
        // reached a terminal, shipped-against status is walked back, and the merchant
        // is told, rather than the retraction being visible only to a poller.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await; // confirmations_required = 10
        store.create_webhook(&tenant_id, "https://merchant.example/hook", "{}", "whsec_x", 1000).unwrap();
        let tx = fixture_tx();
        let daemon = chain_scanned_to(&store, 100, 70, "old");
        // Payment 18 blocks deep - comfortably "final" at ten confirmations.
        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, Some(83))
            .await
            .unwrap();
        let (_, status) = store.recompute_order_status(&order_id, 100, 1600).unwrap();
        assert_eq!(status, crate::status::OrderStatus::Overpaid, "18 confirmations deep before the reorg");

        // An 18-block reorg that does not carry the transaction, and whose inputs are
        // proven consumed by something else.
        for ki in &key_images_of(&tx) {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        reorg_to_new_chain(&daemon, 83, 100, None);
        let store = store.into_shared();

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.unwrap();
        assert_eq!(report.reorg_detected_at, Some(83));
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);

        let s = store.lock().unwrap();
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(order.amount_received_piconero, 0, "the orphaned payment must stop counting");
        assert_eq!(order.status, crate::status::OrderStatus::Pending);
        assert!(order.double_spend_detected_at.is_some());
        let events: Vec<String> =
            s.due_webhook_deliveries(crate::now_unix() + 1, 10).unwrap().into_iter().map(|d| d.event_type).collect();
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
            store.set_scanned_block("mainnet", h, &format!("old_{h}")).unwrap();
        }
        // The fork is at 45 - five blocks below anything this scanner recorded - and
        // the payment exists only in the replacement chain.
        reorg_to_new_chain(&daemon, 45, 52, Some((51, fixture_tx())));
        let store = store.into_shared();

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(
            store.lock().unwrap().max_scanned_height("mainnet").unwrap(),
            Some(49),
            "the rewind must land just below the oldest recorded block, never on an empty window"
        );

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id, handle)], 20, 0).await.unwrap();
        let s = store.lock().unwrap();
        assert_eq!(s.max_scanned_height("mainnet").unwrap(), Some(52));
        let payments = s.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 1, "the payment that exists only in the replacement chain must be found");
        assert_eq!(payments[0].block_height, Some(51));
    }

    /// A transaction that *conflicts* with `fixture_tx()` - it spends the same
    /// inputs, so it carries the same key images - but pays somebody else: its
    /// outputs are stripped, so it matches no wallet in this test suite. This is the
    /// attacker's side of a Monero double-spend, which is always "same key images,
    /// different transaction" (there is no replace-by-fee to express it any other
    /// way).
    fn conflicting_tx(seed: u64) -> Transaction {
        let mut tx = fixture_tx_variant(seed);
        tx.prefix.outputs.clear();
        tx
    }

    /// A second payment to the same order that is *not* in conflict with the
    /// fixture: same outputs (so it still matches, and still pays the order), but
    /// different inputs, hence different key images. Two transactions paying one
    /// order have to look like this - two transactions sharing key images could
    /// never both be valid.
    fn independent_payment_tx(seed: u8) -> Transaction {
        let mut tx = fixture_tx_variant(0x1000 + seed as u64);
        for input in tx.prefix.inputs.iter_mut() {
            if let monero::blockdata::transaction::TxIn::ToKey { k_image, .. } = input {
                let mut bytes = k_image.image.to_bytes();
                bytes[0] ^= seed;
                k_image.image = monero::cryptonote::hash::Hash(bytes);
            }
        }
        tx
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

        assert_ne!(tx_id_hex(&base), tx_id_hex(&variant), "a different unlock_time must be a different txid");
        assert_ne!(tx_id_hex(&base), tx_id_hex(&conflicting));
        assert_ne!(tx_id_hex(&base), tx_id_hex(&independent));

        assert_eq!(key_images_of(&base), key_images_of(&variant), "a variant spends the same inputs");
        assert_eq!(key_images_of(&base), key_images_of(&conflicting), "a conflicting tx is one sharing key images");
        assert_ne!(key_images_of(&base), key_images_of(&independent), "an independent payment spends other inputs");

        for (label, tx, expected) in [
            ("the fixture itself", &base, 1),
            ("changing unlock_time must not break output matching", &variant, 1),
            ("changing key images must not break output matching", &independent, 1),
            ("the attacker's transaction pays somebody else", &conflicting, 0),
        ] {
            let matches = scan_transaction(&key_custody, handle, tx, 0..3).await.unwrap().matches.len();
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
        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, Some(50))
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
                run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0)
                    .await
                    .unwrap();
            }
            reorg_to_chain(&daemon, 50, 53 + round, &public, Some((51 + round, tx.clone()))); // ...and comes back, deeper each time
            for _ in 0..2 {
                run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0)
                    .await
                    .unwrap();
            }

            let s = store.lock().unwrap();
            let payments = s.get_all_payments(&order_id).unwrap();
            assert_eq!(payments.len(), 1, "round {round}: one transaction is one payment row, however often it moves");
            assert!(payments[0].voided_at.is_none(), "round {round}: a remined transaction is not a double-spend");
            assert_eq!(
                payments[0].block_height,
                Some((51 + round) as i64),
                "round {round}: at the height the chain last agreed on"
            );
            let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
            assert!(order.double_spend_detected_at.is_none(), "round {round}: nothing here was ever double-spent");
            assert_eq!(order.amount_received_piconero, payments[0].amount_piconero, "round {round}: counted exactly once");
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
        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, Some(50))
            .await
            .unwrap();
        for ki in &key_images_of(&tx) {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        // The replacement chain runs two blocks longer and carries the attacker's
        // transaction at 53, nowhere near where the original sat.
        reorg_to_new_chain(&daemon, 50, 53, Some((53, attacker)));
        let store = store.into_shared();

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.unwrap();
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);

        let s = store.lock().unwrap();
        let payments = s.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 1, "the attacker's transaction pays nobody here - it must not become a payment row");
        assert!(payments[0].voided_at.is_some());
        assert_eq!(s.get_order(&tenant_id, &order_id).unwrap().unwrap().amount_received_piconero, 0);
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
        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &first, 0..3, 1500, Some(50))
            .await
            .unwrap();
        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &second, 0..3, 1500, Some(50))
            .await
            .unwrap();
        let total_before = store.get_order(&tenant_id, &order_id).unwrap().unwrap();
        let _ = total_before;

        // Only the first transaction is double-spent; the second is remined at 51.
        for ki in &key_images_of(&first) {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        reorg_to_new_chain(&daemon, 50, 51, Some((51, second.clone())));
        let store = store.into_shared();

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.unwrap();
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);

        let s = store.lock().unwrap();
        let payments = s.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 2);
        let (voided, kept): (Vec<_>, Vec<_>) = payments.iter().partition(|p| p.voided_at.is_some());
        assert_eq!(voided.len(), 1, "exactly the double-spent transaction is written off");
        assert_eq!(voided[0].txid, tx_id_hex(&first));
        assert_eq!(kept[0].txid, tx_id_hex(&second));
        assert_eq!(kept[0].block_height, Some(51), "the survivor follows the chain to its new height");
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(order.amount_received_piconero, kept[0].amount_piconero, "only the survivor's amount counts");
        assert!(order.double_spend_detected_at.is_some(), "the incident is recorded regardless of the resulting status");
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
        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, Some(50))
            .await
            .unwrap();
        for ki in &key_images {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        reorg_to_chain(&daemon, 50, 50, "b", Some((50, conflicting_tx(1)))); // the second transaction wins
        let store = store.into_shared();

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.unwrap();
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);
        assert!(store.lock().unwrap().get_all_payments(&order_id).unwrap()[0].voided_at.is_some());

        // The second transaction's block loses in turn, but a third one - not the
        // original - takes the inputs.
        store.lock().unwrap().set_scanned_block("mainnet", 50, "b_50").unwrap();
        reorg_to_chain(&daemon, 50, 50, "c", Some((50, conflicting_tx(2))));

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2100).await.unwrap();
        assert_eq!(report.reorg_detected_at, Some(50));

        let s = store.lock().unwrap();
        let payment = s.get_all_payments(&order_id).unwrap().remove(0);
        assert!(
            payment.voided_at.is_some(),
            "the original never returned to the chain - its replacement being replaced changes nothing"
        );
        assert_eq!(s.get_order(&tenant_id, &order_id).unwrap().unwrap().amount_received_piconero, 0);
    }

    #[tokio::test]
    async fn a_zero_conf_payment_double_spent_out_of_the_mempool_is_voided_with_no_reorg_involved() {
        // The gap this sweep was added to close, and the most consequential one in
        // this file: the textbook attack on a merchant watching the mempool involves
        // no reorg at all. Broadcast transaction A so the merchant's node sees it
        // (with a zero-conf ceiling configured, the order reads `paid` immediately -
        // that is what the setting is for), then get transaction B, spending the same
        // inputs, mined instead. A is never mined, so no block hash the scanner
        // recorded ever changes, so reorg reconciliation - the only thing that ever
        // re-examined an existing payment - never runs. The payment sat at
        // `block_height IS NULL` forever, counting in full towards an order that was
        // never paid, and no `order.double_spend_detected` webhook ever fired.
        let (store, key_custody, handle, tenant_id, order_id) = setup_with_zero_conf_ceiling(Some(u64::MAX)).await;
        store.create_webhook(&tenant_id, "https://merchant.example/hook", "{}", "whsec_x", 1000).unwrap();
        let tx = fixture_tx();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.set_mempool(vec![tx.clone()]);
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        {
            let s = store.lock().unwrap();
            let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
            assert_eq!(order.status, crate::status::OrderStatus::Overpaid, "settled off the mempool sighting alone");
        }

        // The attack lands: A leaves the pool without ever being mined, and a
        // different transaction spending its inputs is confirmed.
        daemon.drop_from_mempool(&tx);
        daemon.push_block("h2", vec![conflicting_tx(11)]);
        for ki in &key_images_of(&tx) {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        let payments = s.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 1);
        assert!(payments[0].voided_at.is_some(), "a proven double-spend must be written off with or without a reorg");
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(order.amount_received_piconero, 0);
        assert_eq!(order.status, crate::status::OrderStatus::Pending);
        assert!(order.double_spend_detected_at.is_some());
        let events: Vec<String> =
            s.due_webhook_deliveries(crate::now_unix() + 1, 10).unwrap().into_iter().map(|d| d.event_type).collect();
        assert!(
            events.contains(&"order.double_spend_detected".to_string()),
            "the merchant who shipped against this needs telling: {events:?}"
        );
        assert!(events.contains(&"order.pending".to_string()), "and the status retraction is its own event: {events:?}");
    }

    /// Builds a store with one order whose single zero-conf payment has already been
    /// voided as a proven double-spend, via the exact same real path
    /// `a_zero_conf_payment_double_spent_out_of_the_mempool_is_voided_with_no_reorg_involved`
    /// proves is reachable. For `revalidate_recent_double_spend_voids`'s own tests
    /// below, which start from an already-voided payment and exercise only the
    /// *recheck*, not how it got voided in the first place.
    async fn setup_with_one_voided_double_spend() -> (crate::store::SharedStore, String, String) {
        let (store, key_custody, handle, tenant_id, order_id) = setup_with_zero_conf_ceiling(Some(u64::MAX)).await;
        store.create_webhook(&tenant_id, "https://merchant.example/hook", "{}", "whsec_x", 1000).unwrap();
        let tx = fixture_tx();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.set_mempool(vec![tx.clone()]);
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        daemon.drop_from_mempool(&tx);
        daemon.push_block("h2", vec![conflicting_tx(11)]);
        for ki in &key_images_of(&tx) {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        assert!(
            store.lock().unwrap().get_all_payments(&order_id).unwrap()[0].voided_at.is_some(),
            "test setup sanity check: the payment must actually be voided before these tests exercise the recheck"
        );
        (store, tenant_id, order_id)
    }

    #[tokio::test]
    async fn revalidate_recent_double_spend_voids_reverses_a_void_no_longer_supported_by_fresh_evidence() {
        let (store, tenant_id, order_id) = setup_with_one_voided_double_spend().await;

        // A fresh daemon for the recheck - every key image defaults to `Unspent`
        // unless explicitly told otherwise, so simply not calling
        // `set_key_image_status` models the original accusation no longer holding up.
        let recheck_daemon = FakeDaemonClient::new();

        let recovered =
            revalidate_recent_double_spend_voids(&store, &recheck_daemon, "mainnet", crate::now_unix()).await.unwrap();
        assert_eq!(recovered, vec![order_id.clone()]);

        let s = store.lock().unwrap();
        let payment = &s.get_all_payments(&order_id).unwrap()[0];
        assert!(payment.voided_at.is_none(), "the void should be reversed");
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert!(
            order.double_spend_detected_at.is_none(),
            "the only voided payment on the order was cleared - the sticky flag should clear too"
        );
        let events: Vec<String> =
            s.due_webhook_deliveries(crate::now_unix() + 1, 10).unwrap().into_iter().map(|d| d.event_type).collect();
        assert!(
            events.contains(&"order.double_spend_reversed".to_string()),
            "the merchant told about the original accusation deserves to be told it was wrong too: {events:?}"
        );
    }

    #[tokio::test]
    async fn revalidate_recent_double_spend_voids_leaves_a_still_supported_void_alone() {
        let (store, tenant_id, order_id) = setup_with_one_voided_double_spend().await;
        let payment = store.lock().unwrap().get_all_payments(&order_id).unwrap()[0].clone();
        let key_images: Vec<String> = serde_json::from_str(&payment.key_images_json).unwrap();

        let recheck_daemon = FakeDaemonClient::new();
        for ki in &key_images {
            recheck_daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }

        let recovered =
            revalidate_recent_double_spend_voids(&store, &recheck_daemon, "mainnet", crate::now_unix()).await.unwrap();
        assert!(recovered.is_empty(), "a still-supported void must not be reversed");
        assert!(store.lock().unwrap().get_all_payments(&order_id).unwrap()[0].voided_at.is_some());
        assert!(store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap().double_spend_detected_at.is_some());
    }

    #[tokio::test]
    async fn revalidate_recent_double_spend_voids_ignores_a_void_outside_the_recheck_window() {
        let (store, _tenant_id, order_id) = setup_with_one_voided_double_spend().await;
        let payment = store.lock().unwrap().get_all_payments(&order_id).unwrap()[0].clone();

        // Backdate the void to well outside the recheck window - fresh evidence would
        // clear it if only the sweep looked, but it's aged out.
        let old_timestamp = crate::now_unix() - DOUBLE_SPEND_RECHECK_WINDOW_SECS - 3600;
        {
            let s = store.lock().unwrap();
            assert!(s.unvoid_payment(&order_id, &payment.txid, payment.output_index).unwrap());
            assert!(s.void_payment(&order_id, &payment.txid, payment.output_index, old_timestamp).unwrap());
        }

        let recheck_daemon = FakeDaemonClient::new(); // would say Unspent if asked
        let recovered =
            revalidate_recent_double_spend_voids(&store, &recheck_daemon, "mainnet", crate::now_unix()).await.unwrap();
        assert!(recovered.is_empty(), "a void outside the recheck window must not be touched");
        assert!(store.lock().unwrap().get_all_payments(&order_id).unwrap()[0].voided_at.is_some());
    }

    #[tokio::test]
    async fn revalidate_recent_double_spend_voids_keeps_the_flag_set_while_another_voided_payment_still_justifies_it() {
        // Two independent payments on one order, both voided - only one is a false
        // accusation. The order-level sticky flag must survive the correction of the
        // first, since the second remains a genuine, still-supported incident.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let first = fixture_tx();
        let second = independent_payment_tx(5);
        let now = crate::now_unix();
        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &first, 0..3, now, Some(50)).await.unwrap();
        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &second, 0..3, now, Some(50)).await.unwrap();
        // Void both by their *actual* recorded output_index - not assumed to be 0,
        // since that depends on which output of each fixture transaction matched.
        for payment in store.get_all_payments(&order_id).unwrap() {
            store.void_payment(&order_id, &payment.txid, payment.output_index, now).unwrap();
        }
        store.mark_double_spend_detected(&order_id, now).unwrap();

        let store = store.into_shared();
        let recheck_daemon = FakeDaemonClient::new();
        for ki in &key_images_of(&second) {
            recheck_daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain); // still genuinely spent
        }
        // `first`'s key images default to Unspent - the accusation being corrected.

        let recovered = revalidate_recent_double_spend_voids(&store, &recheck_daemon, "mainnet", now).await.unwrap();
        assert_eq!(recovered, vec![order_id.clone()]);

        let s = store.lock().unwrap();
        let payments = s.get_all_payments(&order_id).unwrap();
        let (voided, kept): (Vec<_>, Vec<_>) = payments.iter().partition(|p| p.voided_at.is_some());
        assert_eq!(voided.len(), 1, "the still-justified void must remain");
        assert_eq!(voided[0].txid, tx_id_hex(&second));
        assert_eq!(kept[0].txid, tx_id_hex(&first), "the false accusation is reversed");
        assert!(
            s.get_order(&tenant_id, &order_id).unwrap().unwrap().double_spend_detected_at.is_some(),
            "the other voided payment still genuinely justifies the flag - it must not be cleared as a side effect"
        );
    }

    #[tokio::test]
    async fn revalidate_recent_double_spend_voids_aborts_the_sweep_cleanly_if_the_chain_height_is_unreachable() {
        let (store, _tenant_id, _order_id) = setup_with_one_voided_double_spend().await;
        let payment_before = store.lock().unwrap().get_all_payments(&_order_id).unwrap();

        let recheck_daemon = FakeDaemonClient::new();
        recheck_daemon.set_online(false); // every call, including get_height, now fails

        let result = revalidate_recent_double_spend_voids(&store, &recheck_daemon, "mainnet", crate::now_unix()).await;
        assert!(result.is_err(), "an unreachable node must fail the sweep, not silently do nothing");
        let payment_after = store.lock().unwrap().get_all_payments(&_order_id).unwrap();
        assert_eq!(
            payment_after.iter().map(|p| p.voided_at).collect::<Vec<_>>(),
            payment_before.iter().map(|p| p.voided_at).collect::<Vec<_>>(),
            "a failed sweep must not touch anything"
        );
    }

    /// Wraps a `FakeDaemonClient`, failing exactly the call index in `fail_on_call`
    /// (0-based, counted across `is_key_image_spent` calls only) and delegating to
    /// `inner` for every other call - for proving a sweep that hits one payment's
    /// recheck failure still processes the rest of the batch, rather than aborting
    /// on the first error.
    struct DaemonFailingOneKeyImageCall {
        inner: FakeDaemonClient,
        fail_on_call: u64,
        calls: AtomicU64,
    }

    impl DaemonFailingOneKeyImageCall {
        fn new(inner: FakeDaemonClient, fail_on_call: u64) -> Self {
            Self { inner, fail_on_call, calls: AtomicU64::new(0) }
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
        async fn get_block_timestamp(&self, height: u64) -> std::result::Result<u64, DaemonError> {
            self.inner.get_block_timestamp(height).await
        }
        async fn get_block_transactions(&self, height: u64) -> std::result::Result<Vec<Transaction>, DaemonError> {
            self.inner.get_block_transactions(height).await
        }
        async fn get_mempool_transactions(&self) -> std::result::Result<Vec<Transaction>, DaemonError> {
            self.inner.get_mempool_transactions().await
        }
        async fn locate_transaction(&self, txid: &str) -> std::result::Result<TxLocation, DaemonError> {
            self.inner.locate_transaction(txid).await
        }
        async fn get_transaction(&self, txid: &str) -> std::result::Result<Transaction, DaemonError> {
            self.inner.get_transaction(txid).await
        }
        async fn is_key_image_spent(&self, key_images: &[String]) -> std::result::Result<Vec<KeyImageStatus>, DaemonError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == self.fail_on_call {
                return Err(DaemonError::Request(format!("simulated key-image lookup failure on call {n}")));
            }
            self.inner.is_key_image_spent(key_images).await
        }
    }

    #[tokio::test]
    async fn revalidate_recent_double_spend_voids_skips_a_failed_recheck_but_still_processes_the_rest_of_the_batch() {
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        let first = fixture_tx();
        let second = independent_payment_tx(5);
        let now = crate::now_unix();
        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &first, 0..3, now, Some(50)).await.unwrap();
        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &second, 0..3, now, Some(50)).await.unwrap();
        for payment in store.get_all_payments(&order_id).unwrap() {
            store.void_payment(&order_id, &payment.txid, payment.output_index, now).unwrap();
        }
        store.mark_double_spend_detected(&order_id, now).unwrap();

        let store = store.into_shared();
        // Both payments' key images default to Unspent (would clear both if asked),
        // but the very first recheck call fails - it must not stop the second.
        let recheck_daemon = DaemonFailingOneKeyImageCall::new(FakeDaemonClient::new(), 0);

        let recovered = revalidate_recent_double_spend_voids(&store, &recheck_daemon, "mainnet", now).await.unwrap();
        assert_eq!(recovered.len(), 1, "exactly one of the two payments' rechecks succeeded");

        let s = store.lock().unwrap();
        let payments = s.get_all_payments(&order_id).unwrap();
        let (voided, kept): (Vec<_>, Vec<_>) = payments.iter().partition(|p| p.voided_at.is_some());
        assert_eq!(voided.len(), 1, "the payment whose recheck failed must be left voided, not lost or corrupted");
        assert_eq!(kept.len(), 1, "the payment whose recheck succeeded must be reversed");
    }

    #[tokio::test]
    async fn a_fallback_daemon_that_disagrees_with_the_primary_prevents_the_wrongful_void_a_single_lying_node_would_cause() {
        // The prevention half of the fix for `is_key_image_spent`'s single-node trust
        // boundary (docs/DESIGN.md §7.7): the exact same attack shape as
        // `a_zero_conf_payment_double_spent_out_of_the_mempool_is_voided_with_no_reorg_involved`
        // above, except the *accusation itself* is false - only one of two configured
        // nodes claims the key image is spent in the blockchain. Routed through a
        // real `FallbackDaemonClient` (not a bare `FakeDaemonClient`), this must NOT
        // void the payment - a single node's say-so is no longer enough once a second
        // one is configured to disagree with it.
        let (store, key_custody, handle, tenant_id, order_id) = setup_with_zero_conf_ceiling(Some(u64::MAX)).await;
        store.create_webhook(&tenant_id, "https://merchant.example/hook", "{}", "whsec_x", 1000).unwrap();
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
            FallbackNode { label: "primary".to_string(), client: std::sync::Arc::new(primary) },
            FallbackNode { label: "fallback".to_string(), client: std::sync::Arc::new(fallback) },
        ]);

        run_scan_tick(&store, &key_custody, &client, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(
            store.lock().unwrap().get_order(&tenant_id, &order_id).unwrap().unwrap().status,
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
            FallbackNode { label: "primary".to_string(), client: std::sync::Arc::new(primary) },
            FallbackNode { label: "fallback".to_string(), client: std::sync::Arc::new(fallback) },
        ]);

        run_scan_tick(&store, &key_custody, &client, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        let payments = s.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 1);
        assert!(
            payments[0].voided_at.is_none(),
            "one node's false accusation must not void the payment once a second, disagreeing node is configured"
        );
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert!(order.double_spend_detected_at.is_none(), "no incident occurred - nothing should be stamped");
        let events: Vec<String> =
            s.due_webhook_deliveries(crate::now_unix() + 1, 10).unwrap().into_iter().map(|d| d.event_type).collect();
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
        let (store, key_custody, handle, tenant_id, order_id) = setup_with_zero_conf_ceiling(Some(u64::MAX)).await;
        let tx = fixture_tx();
        let store = store.into_shared();

        let primary = FakeDaemonClient::new();
        let fallback = FakeDaemonClient::new();
        for daemon in [&primary, &fallback] {
            daemon.push_block("h1", vec![]);
            daemon.set_mempool(vec![tx.clone()]);
        }
        let client = FallbackDaemonClient::new(vec![
            FallbackNode { label: "primary".to_string(), client: std::sync::Arc::new(primary) },
            FallbackNode { label: "fallback".to_string(), client: std::sync::Arc::new(fallback) },
        ]);
        run_scan_tick(&store, &key_custody, &client, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

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
            FallbackNode { label: "primary".to_string(), client: std::sync::Arc::new(primary) },
            FallbackNode { label: "fallback".to_string(), client: std::sync::Arc::new(fallback) },
        ]);
        run_scan_tick(&store, &key_custody, &client, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        assert!(
            s.get_all_payments(&order_id).unwrap()[0].voided_at.is_some(),
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
        let (store, key_custody, handle, tenant_id, order_id) = setup_with_zero_conf_ceiling(Some(u64::MAX)).await;
        let tx = fixture_tx();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.set_mempool(vec![tx.clone()]);
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        daemon.drop_from_mempool(&tx); // vanished, with nothing proven about its inputs
        daemon.push_block("h2", vec![]);
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        {
            let s = store.lock().unwrap();
            let payments = s.get_all_payments(&order_id).unwrap();
            assert!(payments[0].voided_at.is_none(), "an evicted or still-propagating transaction is not a double-spend");
            assert_eq!(payments[0].block_height, None, "and it is left exactly as it was, re-checkable next tick");
            assert!(s.get_order(&tenant_id, &order_id).unwrap().unwrap().double_spend_detected_at.is_none());
        }

        // And when it does come back and get mined, it is picked up as normal.
        daemon.push_block("h3", vec![tx.clone()]);
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        let s = store.lock().unwrap();
        assert_eq!(s.get_all_payments(&order_id).unwrap()[0].block_height, Some(3));
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
            run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        }
        assert_eq!(daemon.call_count(), 0, "a transaction still in the pool must never be looked up");

        // Mined, and out of the pool in the same tick - the ordinary lifecycle.
        daemon.inner.drop_from_mempool(&tx);
        daemon.inner.push_block("h2", vec![tx.clone()]);
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(store.lock().unwrap().get_all_payments(&order_id).unwrap()[0].block_height, Some(2));
        assert_eq!(
            daemon.call_count(),
            0,
            "the block scan anchors the payment before the sweep runs, so the ordinary path stays free"
        );

        // And it stays free once the payment is confirmed, tick after tick.
        for _ in 0..3 {
            run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        }
        assert_eq!(daemon.call_count(), 0);
    }

    #[tokio::test]
    async fn a_failed_mempool_poll_skips_the_vanished_payment_sweep_rather_than_assuming_an_empty_pool() {
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
        let daemon =
            DaemonFailingFrom::counting(DaemonFailingFrom::failing_from(fake, DaemonCall::Mempool, 1), DaemonCall::Locate);
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(store.lock().unwrap().get_all_payments(&_order_id).unwrap().len(), 1);

        let result = run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await;
        assert!(result.is_ok(), "a failed mempool poll must not fail the tick - the rest of it still has work to do");
        assert_eq!(daemon.call_count(), 0, "with no usable snapshot, the sweep must not run at all");

        // The node recovers, and the transaction really is gone from the pool this
        // time - now the sweep does run, which is what makes the assertion above a
        // statement about the failed poll rather than about there being no work.
        daemon.inner.stop_failing();
        daemon.inner.inner.drop_from_mempool(&tx);
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(daemon.call_count(), 1, "one lookup for the one payment that has genuinely vanished");
    }

    #[tokio::test]
    async fn a_node_failing_partway_through_a_scan_chunk_retries_the_whole_chunk_next_tick() {
        // A tick is not atomic - it is a sequence of independent RPCs - so "the node
        // went away mid-tick" has as many shapes as there are calls in it. Since the
        // block-fetching path batches into `get_blocks_range` chunks (`RESCAN_
        // CHUNK_BLOCKS`'s successor, `docs/txid_lookup_and_scan_chunking_wbs.md`
        // Part A), a failure *within* a chunk abandons the whole chunk, not just the
        // one block that failed - there is no partial-response concept for a real
        // `get_blocks.bin` HTTP call to recover mid-flight the way the old
        // one-block-at-a-time loop could. This is the accepted, real trade-off of
        // batching, not a regression: the high-water mark simply doesn't move past
        // wherever the chunk started until a whole chunk succeeds, and the next tick
        // retries the identical range - no payment lost, none recorded twice, just a
        // coarser (and, in the default-daemon test-double case only, more repeated)
        // unit of retry than before.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        store.set_scanned_block("mainnet", 1, "h1").unwrap();
        let store = store.into_shared();

        let fake = FakeDaemonClient::new();
        fake.push_block("h1", vec![]);
        fake.push_block("h2", vec![]);
        fake.push_block("h3", vec![fixture_tx()]); // the payment is in the block that fails
        fake.push_block("h4", vec![]);
        // The whole 2..=4 range fits in one chunk (well under `SCAN_CHUNK_MAX_
        // BLOCKS`), so this is really "the daemon fails while fetching the chunk
        // that covers the whole remaining range" - the second `BlockTransactions`
        // call the default `get_blocks_range` implementation makes internally
        // (height 2 succeeds as call 0, height 3 fails as call 1), which fails the
        // entire chunk before any of it is recorded.
        let daemon = DaemonFailingFrom::failing_from(fake, DaemonCall::BlockTransactions, 1);

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        {
            let s = store.lock().unwrap();
            assert_eq!(
                s.max_scanned_height("mainnet").unwrap(),
                Some(1),
                "the whole chunk failed, so the high-water mark stays exactly where it was before this tick"
            );
            assert!(s.get_all_payments(&order_id).unwrap().is_empty());
        }

        daemon.stop_failing();
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id, handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        assert_eq!(s.max_scanned_height("mainnet").unwrap(), Some(4));
        let payments = s.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 1, "exactly one payment - not lost by the failure, not duplicated by the retry");
        assert_eq!(payments[0].block_height, Some(3));
    }

    #[tokio::test]
    async fn run_scan_tick_batches_a_wide_catchup_range_into_far_fewer_daemon_calls_than_blocks() {
        // The real-world case this whole mechanism exists for
        // (`docs/txid_lookup_and_scan_chunking_wbs.md` Part A): a process that was
        // down for a while faces a scan range spanning many blocks on its very next
        // tick. Before batching, that was one `get_block_transactions` call per
        // block; now it should be a small number of `get_blocks_range` calls
        // regardless of how wide the range is, as long as the blocks are small
        // enough to fit many per chunk under the default memory budget.
        //
        // Two separate runs, not one nested wrapper counting both call types at
        // once: `DaemonFailingFrom::get_blocks_range`'s own override always
        // decomposes into per-height `get_block_transactions` calls on `self`
        // (needed so a *different* wrapper gating `BlockTransactions` still sees
        // every sub-call - see that override's own doc comment), which means an
        // outer wrapper's `get_blocks_range` never actually reaches an inner
        // wrapper's own `get_blocks_range` counter. Not a limitation that matters
        // here - each half is a real, independent claim anyway.
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
            store.set_scanned_block("mainnet", 1, "h1").unwrap();
            let store = store.into_shared();
            let fake = FakeDaemonClient::new();
            fake.push_block("h1", vec![]); // the already-scanned baseline, mirrored into the daemon too
            for i in 0..NEW_BLOCK_COUNT {
                fake.push_block(&format!("h{}", i + 2), vec![]); // small, empty blocks - cheap to batch heavily
            }
            let daemon = DaemonFailingFrom::counting(fake, DaemonCall::BlocksRange);

            run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id, handle)], 20, 0).await.unwrap();

            assert_eq!(
                store.lock().unwrap().max_scanned_height("mainnet").unwrap(),
                Some(NEW_BLOCK_COUNT + 1),
                "the whole wide range must still be fully scanned in one tick, batching or not"
            );
            assert!(
                daemon.call_count() < NEW_BLOCK_COUNT,
                "expected far fewer than {NEW_BLOCK_COUNT} get_blocks_range calls for {NEW_BLOCK_COUNT} small \
                 blocks under the default memory budget, got {}",
                daemon.call_count()
            );
        }

        // Block-hash fetching (`get_block_hash`, for `scanned_blocks`) is
        // deliberately *not* part of this batching (see `SCAN_CHUNK_MIN_BLOCKS`'s
        // own "scope limit" doc comment) - still exactly one call per new block,
        // asserted here on a fresh scenario so that scope limit is a tested
        // guarantee, not just a comment. `reorg_check_depth` is `0` here
        // specifically (every other test in this file conventionally passes
        // `20`): `check_for_reorg_and_reconcile` - unconditionally called at the
        // end of every tick, unrelated to this change - also calls
        // `get_block_hash` to re-verify already-recorded blocks within that
        // depth of the tip, which would otherwise inflate this count with a
        // second, genuinely unrelated mechanism's own calls.
        {
            let (store, key_custody, handle, tenant_id, _order_id) = setup().await;
            store.set_scanned_block("mainnet", 1, "h1").unwrap();
            let store = store.into_shared();
            let fake = FakeDaemonClient::new();
            fake.push_block("h1", vec![]);
            for i in 0..NEW_BLOCK_COUNT {
                fake.push_block(&format!("h{}", i + 2), vec![]);
            }
            let daemon = DaemonFailingFrom::counting(fake, DaemonCall::BlockHash);

            run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id, handle)], 0, 0).await.unwrap();

            assert_eq!(
                daemon.call_count(),
                // One more than the number of newly-arrived blocks: even with
                // `reorg_check_depth = 0`, `check_for_reorg_and_reconcile`
                // (unconditionally called at the end of every tick, unrelated to
                // this change) still re-checks the current tip's own hash -
                // `>= tip - 0` includes the tip itself. A real, pre-existing
                // extra call, not batching leaking through.
                NEW_BLOCK_COUNT + 1,
                "get_block_hash must be called once per newly-arrived block (plus reconciliation's own tip check) \
                 - it is deliberately never batched, and the already-scanned baseline block must not be re-fetched"
            );
        }
    }

    #[test]
    fn next_scan_chunk_size_shrinks_as_the_observed_average_grows() {
        // The actual claim behind "dynamic, memory-budget-based chunk sizing"
        // (`docs/txid_lookup_and_scan_chunking_wbs.md` Part A), proven directly
        // against the pure sizing function rather than by driving real
        // transactions through real crypto to observe it indirectly - a fast,
        // precise unit test where an end-to-end one would need thousands of
        // scanned outputs just to move the needle.
        let budget_bytes = 8 * 1024 * 1024;
        let small_block_chunk = next_scan_chunk_size(budget_bytes, 1_000.0, u64::MAX);
        let medium_block_chunk = next_scan_chunk_size(budget_bytes, 100_000.0, u64::MAX);
        let large_block_chunk = next_scan_chunk_size(budget_bytes, 10_000_000.0, u64::MAX);
        assert!(
            small_block_chunk > medium_block_chunk,
            "1KB-average blocks ({small_block_chunk}) should fit far more per chunk than 100KB-average ones \
             ({medium_block_chunk})"
        );
        assert!(
            medium_block_chunk > large_block_chunk,
            "100KB-average blocks ({medium_block_chunk}) should still fit more per chunk than 10MB-average ones \
             ({large_block_chunk})"
        );
    }

    #[test]
    fn next_scan_chunk_size_respects_its_own_bounds() {
        let budget_bytes = 8 * 1024 * 1024;
        // An average so small it would otherwise compute a chunk far larger
        // than `SCAN_CHUNK_MAX_BLOCKS` - the backstop `RESCAN_CHUNK_MAX_BLOCKS`'s
        // own doc comment names (an older node ignoring `max_block_count`).
        assert_eq!(next_scan_chunk_size(budget_bytes, 1.0, u64::MAX), SCAN_CHUNK_MAX_BLOCKS);
        // An average so large it would otherwise compute a chunk of `0`, which
        // must never happen (it would stall the catch-up walk forever).
        assert_eq!(next_scan_chunk_size(budget_bytes, f64::MAX, u64::MAX), SCAN_CHUNK_MIN_BLOCKS);
        // Never larger than what's actually left to scan, regardless of budget.
        assert_eq!(next_scan_chunk_size(budget_bytes, 1.0, 3), 3);
    }

    #[test]
    fn update_avg_bytes_per_block_weighs_recent_data_by_the_configured_alpha() {
        let after_one_big_chunk = update_avg_bytes_per_block(SCAN_CHUNK_INITIAL_AVG_BYTES, 1_000_000, 1);
        assert!(
            after_one_big_chunk > SCAN_CHUNK_INITIAL_AVG_BYTES,
            "a chunk far bigger than the cold-start guess must pull the average up, not leave it unchanged"
        );
        // A single all-zero (empty-block) chunk should pull the average down
        // but - by design, `SCAN_CHUNK_EWMA_ALPHA < 1.0` - not all the way to
        // zero in one step; a lone anomalous chunk shouldn't swing the very
        // next chunk's size wildly.
        let after_one_empty_chunk = update_avg_bytes_per_block(SCAN_CHUNK_INITIAL_AVG_BYTES, 0, 10);
        assert!(after_one_empty_chunk > 0.0 && after_one_empty_chunk < SCAN_CHUNK_INITIAL_AVG_BYTES);
    }

    #[tokio::test]
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

        let result = run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await;
        assert!(result.is_err(), "a tick that cannot learn the chain height has nothing to say about any order");
        {
            let s = store.lock().unwrap();
            assert_eq!(s.max_scanned_height("mainnet").unwrap(), None, "nothing may be recorded as scanned");
            assert_eq!(
                s.get_order(&tenant_id, &order_id).unwrap().unwrap().status,
                crate::status::OrderStatus::Pending,
                "and no order may have advanced on evidence the tick never got"
            );
        }

        daemon.stop_failing();
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        let s = store.lock().unwrap();
        assert_eq!(s.get_all_payments(&order_id).unwrap().len(), 1, "the next tick recovers the payment exactly once");
        assert_eq!(s.get_order(&tenant_id, &order_id).unwrap().unwrap().status, crate::status::OrderStatus::Confirming);
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
        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, Some(50))
            .await
            .unwrap();
        for ki in &key_images_of(&tx) {
            fake.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        reorg_to_new_chain(&fake, 50, 51, None);
        let store = store.into_shared();
        let daemon = DaemonFailingFrom::failing_from(fake, DaemonCall::KeyImageSpent, 0);

        assert!(
            check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.is_err(),
            "the node failure must surface rather than be read as 'no proof of a double-spend'"
        );
        {
            let s = store.lock().unwrap();
            assert_eq!(s.get_scanned_block_hash("mainnet", 50).unwrap().as_deref(), Some("old_50"));
            assert!(s.get_all_payments(&order_id).unwrap()[0].voided_at.is_none(), "nothing may be voided on no evidence");
        }

        daemon.stop_failing();
        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2100).await.unwrap();
        assert_eq!(report.reorg_detected_at, Some(50));
        assert_eq!(report.double_spent_orders, vec![order_id.clone()]);
        assert!(store.lock().unwrap().get_all_payments(&order_id).unwrap()[0].voided_at.is_some());
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
        store.lock().unwrap().set_scanned_block("mainnet", 49, "a_49").unwrap();
        run_scan_tick(&store, &key_custody, &honest, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(store.lock().unwrap().get_all_payments(&order_id).unwrap()[0].block_height, Some(50));

        // A different node entirely: same network, chain diverging at 48, and the
        // payment nowhere in it. Nothing tells the scanner the daemon changed.
        let other = FakeDaemonClient::new();
        for h in 1..=47 {
            other.push_block(&format!("a_{h}"), vec![]);
        }
        for h in 48..=55 {
            other.push_block(&format!("b_{h}"), vec![]);
        }

        run_scan_tick(&store, &key_custody, &other, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        {
            let s = store.lock().unwrap();
            assert_eq!(
                s.max_scanned_height("mainnet").unwrap(),
                Some(48),
                "the divergence is found and rewound, exactly as for a reorg - at 49 rather than the true fork at \
                 48 because 48 is a height this scanner never recorded a hash for, and detection can only ever be \
                 as fine-grained as the window it kept"
            );
            let payment = &s.get_all_payments(&order_id).unwrap()[0];
            assert!(
                payment.voided_at.is_none(),
                "the new node not having the transaction proves nothing about it - never void on that"
            );
        }

        // ...and the scanner then works forward over the new node's chain.
        run_scan_tick(&store, &key_custody, &other, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(store.lock().unwrap().max_scanned_height("mainnet").unwrap(), Some(55));

        // Swapping back is symmetric: the *first* node is now the one presenting a
        // divergent history, and gets reconciled the same way.
        run_scan_tick(&store, &key_custody, &honest, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(
            store.lock().unwrap().max_scanned_height("mainnet").unwrap(),
            Some(47),
            "no daemon is privileged - the stored chain is re-validated against whoever is answering"
        );
    }

    #[tokio::test]
    async fn failing_over_through_a_real_fallback_client_to_a_node_serving_a_different_chain_reconciles_like_a_reorg() {
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
        store.lock().unwrap().set_scanned_block("mainnet", 49, "a_49").unwrap();

        let fallback = std::sync::Arc::new(FakeDaemonClient::new());
        for h in 1..=47 {
            fallback.push_block(&format!("a_{h}"), vec![]);
        }
        for h in 48..=52 {
            fallback.push_block(&format!("b_{h}"), vec![]);
        }

        let client = FallbackDaemonClient::new(vec![
            FallbackNode { label: "primary".to_string(), client: primary.clone() },
            FallbackNode { label: "fallback".to_string(), client: fallback.clone() },
        ]);

        run_scan_tick(&store, &key_custody, &client, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(
            store.lock().unwrap().get_all_payments(&order_id).unwrap()[0].block_height,
            Some(50),
            "the primary is healthy and used first, exactly like a bare RpcDaemonClient would be"
        );

        // The primary goes unreachable - nothing tells `FallbackDaemonClient` to swap,
        // it discovers this itself on the next call and moves to the fallback, which
        // happens to disagree with recorded history from height 48 on.
        primary.set_online(false);
        run_scan_tick(&store, &key_custody, &client, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        {
            let s = store.lock().unwrap();
            assert_eq!(
                s.max_scanned_height("mainnet").unwrap(),
                Some(48),
                "failover to a genuinely diverging fallback reconciles exactly like the raw-swap test above"
            );
            assert!(
                s.get_all_payments(&order_id).unwrap()[0].voided_at.is_none(),
                "the fallback not having the transaction proves nothing about it - never void on that"
            );
        }

        run_scan_tick(&store, &key_custody, &client, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(
            store.lock().unwrap().max_scanned_height("mainnet").unwrap(),
            Some(52),
            "scanning continues forward on the fallback's own chain"
        );
    }

    #[tokio::test]
    async fn failing_over_to_a_lagging_but_honest_fallback_neither_rewinds_nor_corrupts_the_window() {
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
        store.lock().unwrap().set_scanned_block("mainnet", 59, "a_59").unwrap();

        run_scan_tick(&store, &key_custody, primary.as_ref(), "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert_eq!(store.lock().unwrap().max_scanned_height("mainnet").unwrap(), Some(60));

        // A fallback with the identical history, just not caught up yet.
        let fallback = std::sync::Arc::new(FakeDaemonClient::new());
        for h in 1..=40 {
            fallback.push_block(&format!("a_{h}"), vec![]);
        }

        let client = FallbackDaemonClient::new(vec![
            FallbackNode { label: "primary".to_string(), client: primary.clone() },
            FallbackNode { label: "fallback".to_string(), client: fallback.clone() },
        ]);

        primary.set_online(false);
        run_scan_tick(&store, &key_custody, &client, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        {
            let s = store.lock().unwrap();
            assert_eq!(
                s.max_scanned_height("mainnet").unwrap(),
                Some(60),
                "a lagging fallback must not rewind the window back to where it currently is"
            );
            assert!(
                s.get_all_payments(&order_id).unwrap()[0].voided_at.is_none(),
                "a lagging fallback not yet having the transaction proves nothing - never void on that"
            );
        }
    }

    #[tokio::test]
    async fn a_node_that_dies_between_fetching_a_blocks_transactions_and_its_hash_can_pair_them_with_a_different_nodes_hash(
    ) {
        // A real, narrow correctness gap `FallbackDaemonClient` introduces rather
        // than merely inherits: `run_scan_tick` fetches a block's transactions and
        // its hash as two *separate* daemon calls (see the long comment above
        // `daemon.get_block_hash(height)` in `run_scan_tick` on why - a hole would
        // otherwise be left in the reorg window). Per-call failover means those two
        // calls for the *same height* are not guaranteed to come from the same node:
        // if the first call succeeds against the primary and the primary dies before
        // the second, the second is transparently served by the fallback instead -
        // pairing one node's transactions with a different node's hash for what is
        // recorded as a single scanned block.
        //
        // This is a sharper version of a risk already accepted for a single node
        // (see the "genuine replication lag across a pool of backend nodes behind a
        // public endpoint" comment on `run_scan_tick`'s bootstrap branch, and
        // `docs/DESIGN.md` §7.7's now-updated note on fallback nodes): there,
        // inconsistency is bounded by how out-of-sync one public endpoint's own
        // backends are. Here, it is bounded only by how different two *independently
        // operated* nodes' chains are allowed to be, which for a fallback added
        // specifically to survive a primary that has gone badly wrong (not just
        // "slightly behind") could be a lot. This test exists to pin the actual
        // behavior down precisely rather than leave it as an unverified worry - see
        // `docs/DESIGN.md` §7.7 for the accepted-tradeoff writeup this backs.
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
        store.lock().unwrap().set_scanned_block("mainnet", 50, "a_50").unwrap();

        let fallback = std::sync::Arc::new(FakeDaemonClient::new());
        fallback.seed_block_at(51, "b_51", vec![]);

        // `get_block_transactions(51)` succeeds normally against the primary; only
        // its `get_block_hash(51)` call - for that same height, moments later - is
        // made to fail, which is exactly what fails over to the fallback for that one
        // call alone.
        let primary =
            std::sync::Arc::new(DaemonFailingBlockHashAt { inner: primary_inner, failing_height: AtomicU64::new(51) });
        let client = FallbackDaemonClient::new(vec![
            FallbackNode { label: "primary".to_string(), client: primary },
            FallbackNode { label: "fallback".to_string(), client: fallback.clone() },
        ]);

        run_scan_tick(&store, &key_custody, &client, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        assert_eq!(
            s.get_scanned_block_hash("mainnet", 51).unwrap(),
            Some("b_51".to_string()),
            "the recorded hash for height 51 came from the fallback, whose get_block_hash call is what failed over"
        );
        assert_eq!(
            s.get_all_payments(&order_id).unwrap()[0].block_height,
            Some(51),
            "but the payment recorded at height 51 came from the primary's block content, fetched moments earlier - \
             this is the actual inconsistency: the stored (height, hash) pair for 51 does not correspond to any \
             single node's real block 51, and there is no detection for this today"
        );
    }

    #[tokio::test]
    async fn every_fallback_node_being_down_fails_the_tick_cleanly_without_corrupting_stored_state() {
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
        run_scan_tick(&store, &key_custody, primary.as_ref(), "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        let payments_before: Vec<(String, Option<i64>)> = store
            .lock()
            .unwrap()
            .get_all_payments(&order_id)
            .unwrap()
            .into_iter()
            .map(|p| (p.txid, p.block_height))
            .collect();
        let scanned_before = store.lock().unwrap().max_scanned_height("mainnet").unwrap();

        let fallback = std::sync::Arc::new(FakeDaemonClient::new());
        primary.set_online(false);
        fallback.set_online(false);
        let client = FallbackDaemonClient::new(vec![
            FallbackNode { label: "primary".to_string(), client: primary.clone() },
            FallbackNode { label: "fallback".to_string(), client: fallback.clone() },
        ]);

        let result = run_scan_tick(&store, &key_custody, &client, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await;
        assert!(result.is_err(), "every node down must surface as an error, not a silent no-op or a panic");
        let payments_after: Vec<(String, Option<i64>)> = store
            .lock()
            .unwrap()
            .get_all_payments(&order_id)
            .unwrap()
            .into_iter()
            .map(|p| (p.txid, p.block_height))
            .collect();
        assert_eq!(payments_after, payments_before, "a failed tick must not touch previously-recorded payments");
        assert_eq!(
            store.lock().unwrap().max_scanned_height("mainnet").unwrap(),
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
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        {
            let s = store.lock().unwrap();
            assert!(s.get_all_payments(&order_id).unwrap().is_empty(), "nothing to see - only zero-conf is lost");
            assert_eq!(s.get_order(&tenant_id, &order_id).unwrap().unwrap().status, crate::status::OrderStatus::Pending);
        }

        daemon.push_block("h2", vec![tx]);
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        let payments = s.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 1, "the payment is detected in full the moment it is mined");
        assert_eq!(payments[0].block_height, Some(2));
        assert_eq!(s.get_order(&tenant_id, &order_id).unwrap().unwrap().status, crate::status::OrderStatus::Confirming);
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
        let (tenant_id, handle, order_id) = tenant_with_pending_order(&store, &key_custody, view, spend).await;
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![fixture_tx()]);
        daemon.set_mempool(vec![fixture_tx_variant(77)]);

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        assert!(s.get_all_payments(&order_id).unwrap().is_empty(), "no wallet matched, so no payment exists");
        assert_eq!(s.get_order(&tenant_id, &order_id).unwrap().unwrap().status, crate::status::OrderStatus::Pending);
        assert_eq!(s.get_order(&tenant_id, &order_id).unwrap().unwrap().amount_received_piconero, 0);
    }

    #[tokio::test]
    async fn an_inflated_reported_height_inflates_confirmations_which_is_an_accepted_trust_boundary() {
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
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        {
            let s = store.lock().unwrap();
            let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
            assert_eq!(order.status, crate::status::OrderStatus::Confirming);
            assert_eq!(order.confirmations, 1, "one real block, one confirmation");
        }

        daemon.report_height(1_000); // the node asserts a tip it has no blocks for
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(order.confirmations, 999, "taken at face value - the documented trust boundary");
        assert_eq!(order.status, crate::status::OrderStatus::Overpaid);
    }

    #[tokio::test]
    async fn a_daemon_far_behind_the_recorded_high_water_mark_neither_rescans_nor_discards_its_window() {
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
        scan_transaction_for_tenant(&store, &key_custody, handle, &tenant_id, &tx, 0..3, 1500, Some(95))
            .await
            .unwrap();
        for h in 70..=100 {
            store.set_scanned_block("mainnet", h, &format!("old_{h}")).unwrap();
        }
        let (_, status) = store.recompute_order_status(&order_id, 100, 1600).unwrap();
        assert_eq!(status, crate::status::OrderStatus::Confirming, "six confirmations, ten required");
        let store = store.into_shared();

        // The node is at height 30, three quarters of the way through a resync.
        let daemon = FakeDaemonClient::new();
        for h in 1..=30 {
            daemon.push_block(&format!("old_{h}"), vec![]);
        }

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        assert_eq!(
            s.max_scanned_height("mainnet").unwrap(),
            Some(100),
            "the window belongs to the chain, not to whichever node is currently answering"
        );
        assert_eq!(s.get_scanned_block_hash("mainnet", 95).unwrap().as_deref(), Some("old_95"));
        let payment = &s.get_all_payments(&order_id).unwrap()[0];
        assert!(payment.voided_at.is_none(), "a node that hasn't got there yet proves nothing about a payment");
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
        let txs = vec![fixture_tx(), independent_payment_tx(1), independent_payment_tx(2)];
        let per_tx_amount = {
            let scan = scan_transaction(&key_custody, handle, &txs[0], 0..3).await.unwrap();
            scan.matches[0].amount_piconero.unwrap()
        };
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", txs);

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        let payments = s.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 3, "three distinct transactions are three payments, not one deduplicated row");
        assert!(payments.iter().all(|p| p.block_height == Some(2)));
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(order.amount_received_piconero, per_tx_amount * 3);
        assert_eq!(order.status, crate::status::OrderStatus::Confirming, "one confirmation, ten required");
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
        let second_index = store.allocate_minor_index(&tenant_id).unwrap();
        assert_eq!(second_index, 2);
        let second_order = store
            .create_order(NewOrder {
                confirmations_required_override: None,
                tenant_id: tenant_id.clone(),
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
                MatchedOutput { output_index: 0, subaddress_index: SubaddressIndex { major: 0, minor: 1 }, amount_piconero: Some(700) },
                MatchedOutput { output_index: 1, subaddress_index: SubaddressIndex { major: 0, minor: 2 }, amount_piconero: Some(900) },
                // An index no order was ever issued for - a tenant's own older
                // subaddress, say. Must be dropped, not attributed to anything.
                MatchedOutput { output_index: 2, subaddress_index: SubaddressIndex { major: 0, minor: 9 }, amount_piconero: Some(100) },
            ],
            txid: "tx_paying_two_orders".into(),
            key_images_json: "[]".into(),
        };

        let touched = record_scan_match(&store, &tenant_id, &scan, 1500, Some(50)).unwrap();
        assert_eq!(touched, HashSet::from([first_order.clone(), second_order.id.clone()]));

        let first = store.get_all_payments(&first_order).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].amount_piconero, 700, "each order gets its own output's amount, never the transaction total");
        let second = store.get_all_payments(&second_order.id).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].amount_piconero, 900);
    }

    #[tokio::test]
    async fn one_tick_can_void_a_double_spent_payment_for_one_order_and_record_a_new_one_for_another() {
        // Two facts arriving in the same block, on the same tick, for different
        // orders: an older payment is orphaned and proven double-spent, while the
        // replacement chain carries a brand-new payment. Neither may contaminate the
        // other - and the shape also exercises the cross-tenant case migration 0004
        // exists for, since both tenants here are configured with the same view key
        // (a merchant running a second instance against one wallet), so one
        // transaction legitimately pays two different orders.
        let key_custody = PlainKeyCustody::default();
        let store = Store::open_in_memory().unwrap();
        let (tenant_a, handle_a, order_a) =
            tenant_with_pending_order(&store, &key_custody, fixture_view_key(), fixture_spend_pubkey()).await;
        let (tenant_b, handle_b, order_b) =
            tenant_with_pending_order(&store, &key_custody, fixture_view_key(), fixture_spend_pubkey()).await;

        let doomed = fixture_tx();
        let fresh = independent_payment_tx(6);
        let daemon = chain_scanned_to(&store, 51, 40, "old");
        // Only tenant A had a payment before the reorg.
        scan_transaction_for_tenant(&store, &key_custody, handle_a, &tenant_a, &doomed, 0..3, 1500, Some(50))
            .await
            .unwrap();
        for ki in &key_images_of(&doomed) {
            daemon.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        reorg_to_new_chain(&daemon, 50, 51, Some((50, fresh.clone())));
        let store = store.into_shared();

        let tenants = [(tenant_a.clone(), handle_a), (tenant_b.clone(), handle_b)];
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &tenants, 20, 0).await.unwrap(); // detects, voids, rewinds
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &tenants, 20, 0).await.unwrap(); // rescans the replacement chain

        let s = store.lock().unwrap();
        let a_payments = s.get_all_payments(&order_a).unwrap();
        assert_eq!(a_payments.len(), 2, "the voided one and the new one both belong to A's audit trail");
        let voided: Vec<_> = a_payments.iter().filter(|p| p.voided_at.is_some()).collect();
        assert_eq!(voided.len(), 1);
        assert_eq!(voided[0].txid, tx_id_hex(&doomed));
        let order_a_row = s.get_order(&tenant_a, &order_a).unwrap().unwrap();
        assert!(
            order_a_row.double_spend_detected_at.is_some(),
            "the incident stays recorded even though a later payment covered the order"
        );
        assert_eq!(order_a_row.amount_received_piconero, a_payments.iter().find(|p| p.voided_at.is_none()).unwrap().amount_piconero);

        let b_payments = s.get_all_payments(&order_b).unwrap();
        assert_eq!(b_payments.len(), 1, "B gets its own row for the same output - the constraint is per order");
        assert_eq!(b_payments[0].txid, tx_id_hex(&fresh));
        assert!(b_payments[0].voided_at.is_none());
        assert!(
            s.get_order(&tenant_b, &order_b).unwrap().unwrap().double_spend_detected_at.is_none(),
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
        };
        let decrypted = ScanResult {
            matches: vec![MatchedOutput {
                output_index: 0,
                subaddress_index: SubaddressIndex { major: 0, minor: 1 },
                amount_piconero: Some(4_242),
            }],
            txid: "tx_flaky_amount".into(),
            key_images_json: "[]".into(),
        };

        record_scan_match(&store, &tenant_id, &undecryptable, 1500, Some(50)).unwrap();
        assert!(store.get_all_payments(&order_id).unwrap().is_empty());

        let touched = record_scan_match(&store, &tenant_id, &decrypted, 1600, Some(50)).unwrap();
        assert_eq!(touched, HashSet::from([order_id.clone()]), "the later success records it in full");
        assert_eq!(store.get_all_payments(&order_id).unwrap()[0].amount_piconero, 4_242);

        record_scan_match(&store, &tenant_id, &undecryptable, 1700, Some(50)).unwrap();
        let payments = store.get_all_payments(&order_id).unwrap();
        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].amount_piconero, 4_242, "a later failure to decrypt must not disturb a recorded payment");
        assert!(payments[0].voided_at.is_none());
    }

    #[tokio::test]
    async fn a_payment_completing_an_order_in_the_tick_its_deadline_passes_settles_rather_than_expiring() {
        // Expiry and payment are evaluated in one place, from one snapshot, so their
        // relative ordering within a tick is not a race - but it is worth an
        // executable statement, because "the customer paid at the last second" is
        // both common and the case where getting it wrong means keeping the money and
        // telling the customer their order expired. The status ladder only reaches
        // `expired` when the total falls *short*; a covered order is never expired,
        // however late it was covered.
        let (store, key_custody, handle, tenant_id, order_id) = setup().await;
        store
            .execute_raw_for_test(&format!("UPDATE orders SET expires_at_utc = {} WHERE id = '{order_id}'", crate::now_unix() - 1))
            .unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.set_mempool(vec![fixture_tx()]);

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();

        let s = store.lock().unwrap();
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
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
                confirmations_required_override: None,
                tenant_id: tenant_id.clone(),
                merchant_order_id: None,
                minor_index: s.allocate_minor_index(&tenant_id).unwrap(),
                address: "sub_partial".into(),
                xmr_amount_piconero: 10_000_000,
                description: None,
                created_at: 1000,
                expires_at: crate::now_unix() - 1,
            })
            .unwrap();
        s.record_payment_match(&partial.id, "tx_partial", 0, 1, "[]", 1500, None).unwrap();
        let (_, status) = s.recompute_order_status(&partial.id, 1, crate::now_unix()).unwrap();
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

        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        {
            let s = store.lock().unwrap();
            assert_eq!(s.max_scanned_height("mainnet").unwrap(), None, "nothing to seed from, so nothing recorded");
            assert!(s.get_all_payments(&order_id).unwrap().is_empty());
        }

        // One block exists: the whole chain is height 1, and the tick must still
        // behave (seeding one behind the tip lands on height 0, which does not exist
        // here, so it degrades to "try again next tick" rather than erroring).
        daemon.push_block("only_block", vec![fixture_tx()]);
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        assert!(check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.is_ok());
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
        store.set_scanned_block("mainnet", 0, "genesis_v0").unwrap();
        store.set_scanned_block("mainnet", 1, "block_1_v0").unwrap();
        let store = store.into_shared();

        let daemon = FakeDaemonClient::new();
        daemon.seed_block_at(0, "genesis_v1", vec![]);
        daemon.push_block("block_1_v1", vec![]);

        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", 20, 2000).await.unwrap();
        assert_eq!(report.reorg_detected_at, Some(0));
        assert_eq!(
            store.lock().unwrap().max_scanned_height("mainnet").unwrap(),
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
            run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], depth, 0).await.unwrap();
        }

        {
            let s = store.lock().unwrap();
            assert_eq!(s.max_scanned_height("mainnet").unwrap(), Some(60));
            let retained: u64 = (0..=60).filter(|h| s.get_scanned_block_hash("mainnet", *h).unwrap().is_some()).count() as u64;
            assert!(retained > depth, "the window must always comfortably cover the configured reorg depth: {retained}");
            assert!(retained <= depth * 4 + 2, "and must not grow with the chain: {retained} rows after 60 blocks");
        }

        // A reorg at the deepest point the configured window claims to cover is still
        // both detectable and rewindable after all that pruning.
        // (Same tip, so the window is exactly `60-depth..=60` and the fork sits on
        // its lower edge - a reorg that also *grew* the chain would move the window
        // up with it, which is the separate limitation
        // `a_reorg_deeper_than_the_window_is_reported_at_the_window_edge_and_leaves_older_payments_alone`
        // covers.)
        reorg_to_new_chain(&daemon, 60 - depth, 60, None);
        let report = check_for_reorg_and_reconcile(&store, &daemon, "mainnet", depth, 2000).await.unwrap();
        assert_eq!(report.reorg_detected_at, Some(60 - depth));
        assert_eq!(
            store.lock().unwrap().max_scanned_height("mainnet").unwrap(),
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
        let (store, key_custody, handle, tenant_id, order_id) = setup_with_zero_conf_ceiling(Some(u64::MAX)).await;
        store.create_webhook(&tenant_id, "https://merchant.example/hook", "{}", "whsec_x", 1000).unwrap();
        let first = fixture_tx();
        let second = independent_payment_tx(8);
        let per_tx = {
            let scan = scan_transaction(&key_custody, handle, &first, 0..3).await.unwrap();
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
        run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await.unwrap();
        {
            let s = store.lock().unwrap();
            let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
            assert_eq!(order.status, crate::status::OrderStatus::Paid, "covered by both payments, trusted at zero-conf");
        }

        // Both transactions are double-spent out of the pool at once, and the node
        // dies on the second lookup - after the first payment has already been voided.
        daemon.inner.drop_from_mempool(&first);
        daemon.inner.drop_from_mempool(&second);
        for ki in key_images_of(&first).iter().chain(key_images_of(&second).iter()) {
            daemon.inner.set_key_image_status(ki, KeyImageStatus::SpentInBlockchain);
        }
        daemon.fail_from.store(1, Ordering::SeqCst);

        let result = run_scan_tick(&store, &key_custody, &daemon, "mainnet", &[(tenant_id.clone(), handle)], 20, 0).await;
        assert!(result.is_err(), "the node failure must surface");

        let s = store.lock().unwrap();
        let payments = s.get_all_payments(&order_id).unwrap();
        let voided: Vec<_> = payments.iter().filter(|p| p.voided_at.is_some()).collect();
        assert_eq!(voided.len(), 1, "exactly one payment was resolved before the node failed");
        let order = s.get_order(&tenant_id, &order_id).unwrap().unwrap();
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
        let events: Vec<String> =
            s.due_webhook_deliveries(crate::now_unix() + 1, 20).unwrap().into_iter().map(|d| d.event_type).collect();
        assert!(events.contains(&"order.double_spend_detected".to_string()), "and the merchant is told: {events:?}");
        assert!(events.contains(&"order.partial".to_string()), "including the retraction of `paid`: {events:?}");
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
        let tenants: Vec<(String, WalletHandle)> = vec![];
        assert_send(run_scan_tick(&store, &key_custody, &daemon, "mainnet", &tenants, 20, 0));
    }
}
