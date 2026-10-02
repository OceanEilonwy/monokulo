//! Zero-confirmation payments, detected as soon as they reach the pool.
//!
//! Two paths share what is remembered here:
//! - [`fast_pass`] runs on its own every fraction of a second
//!   (`loops::run_fast_mempool_loop`). It looks only at transactions it
//!   hasn't seen, scans each against every store with something in scope,
//!   and records, recomputes and wakes webhook delivery for what they pay,
//!   in one go. A per-pass scan budget keeps a flood of new transactions
//!   from taking the CPU: what doesn't fit is left to the rotation.
//! - The round's mempool tier ([`step`]) is the safety net: new transactions
//!   first, then a rotating slice of the pool against a rotating page of
//!   stores, rescanning a transaction for a store whose scan window changed.
//!
//! What is remembered (task 7.3), so a transaction sitting in the pool is
//! fetched once and scanned once per store, not every second:
//! - the bodies of transactions still in the pool;
//! - which (transaction, store, scan window) were scanned *successfully*.
//!   A failed scan isn't remembered, so it is retried.
//!
//! Entries go when their transaction leaves the pool.
//!
//! The pool is only looked at while there is something to look for: a store
//! with an order in scope, or (for the round's tier, whose poll the
//! vanished-payment check reads) a payment not yet in a block. Otherwise no
//! request is made at all, and what was remembered is dropped. And bodies are
//! only fetched when there is a store to scan them for.
//!
//! A round that will look at the pool asks for it with the chain's tip, at
//! its start ([`watching`], `run_round`): one request for both.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use monero::Transaction;
use tokio::time::Instant;

use crate::key_custody::{ScanIndices, WalletHandle};
use crate::scanner::{record_scan_match, scan_for_tenants, ScannerError};
use crate::store::db::Class;

use super::{bounded, Progress, Round, RoundInputs, ScanState, Wait};

/// Most mempool transaction bodies remembered, by count and by serialized
/// size; beyond either (a spam wave of many, or of large, transactions)
/// new ones are scanned but not kept.
const MAX_BODIES: usize = 20_000;
const MAX_BODY_BYTES: usize = 128 * 1024 * 1024;
/// Transactions the round's tier takes per round.
const TXS_PER_ROUND: usize = 64;
/// Stores one transaction is scanned for per round by the rotation.
const TENANTS_PER_TX: usize = 32;
/// Stores whose scan windows the round's tier loads per round.
const TENANT_PAGE: usize = 256;
/// Most (transaction, store) scans one fast pass does. At a few hundred
/// microseconds each, a pass stays well under its interval.
const FAST_SCANS_PER_PASS: usize = 4096;
/// Most new transactions one fast pass fetches.
const FAST_TXS_PER_PASS: usize = 256;
/// How long the fast path trusts the windows it loaded. A window that
/// changed meanwhile is caught by the rotation, which rescans a
/// transaction for a store whose window changed.
const WINDOWS_TTL: Duration = Duration::from_secs(1);
/// Longest one store's scan of a pool transaction may take: the mempool
/// tier's share of a round. The fast path scans with the same deadline, so
/// a store it gives up on is given no less time by the round's rotation.
const SCAN_DEADLINE: Duration = super::Tier::Mempool.reserved();

type TenantWindow = (crate::store::TenantId, WalletHandle, ScanIndices);

/// Each store with something in scope and its subaddress window.
type Windows = Arc<Vec<(crate::store::TenantId, Vec<u32>)>>;

#[derive(Default)]
pub(crate) struct MempoolState {
    inner: parking_lot::Mutex<Remembered>,
    next_tx_offset: AtomicUsize,
    next_tenant_offset: AtomicUsize,
    tenant_page_after: parking_lot::Mutex<String>,
    /// Every store's window as of recently, for the fast path.
    windows: parking_lot::Mutex<Option<(Instant, Windows)>>,
    /// The chain height the last round saw, for the fast path's recomputes.
    pub(crate) last_tip: AtomicU64,
}

/// Mempool bodies kept between rounds, with their serialized size.
#[derive(Default)]
struct Bodies {
    by_txid: HashMap<String, (Arc<Transaction>, usize)>,
    bytes: usize,
}

impl Bodies {
    fn get(&self, txid: &str) -> Option<&Arc<Transaction>> {
        self.by_txid.get(txid).map(|(tx, _)| tx)
    }

    fn contains_key(&self, txid: &str) -> bool {
        self.by_txid.contains_key(txid)
    }

    fn retain(&mut self, mut keep: impl FnMut(&String) -> bool) {
        let mut freed = 0;
        self.by_txid.retain(|txid, (_, size)| {
            let kept = keep(txid);
            if !kept {
                freed += *size;
            }
            kept
        });
        self.bytes -= freed;
    }

    /// Keeps a fetched body for later rounds, up to `max_count` bodies and
    /// `max_bytes` serialized bytes: past either (a pool far bigger than
    /// any real one) bodies are fetched each time instead, and memory stays
    /// bounded.
    fn remember(&mut self, txid: &str, tx: &Arc<Transaction>, max_count: usize, max_bytes: usize) {
        if self.by_txid.len() >= max_count || self.by_txid.contains_key(txid) {
            return;
        }
        let size = monero::consensus::encode::serialize(tx.as_ref()).len();
        if self.bytes + size > max_bytes {
            return;
        }
        self.bytes += size;
        self.by_txid.insert(txid.to_string(), (tx.clone(), size));
    }
}

#[derive(Default)]
struct Remembered {
    bodies: Bodies,
    /// txid -> store -> the window generation it was scanned with.
    scanned: HashMap<String, HashMap<crate::store::TenantId, u64>>,
}

impl MempoolState {
    /// Forgets what left the pool.
    fn retain_pool(&self, in_pool: &HashSet<String>) {
        let mut remembered = self.inner.lock();
        remembered.bodies.retain(|txid| in_pool.contains(txid));
        remembered.scanned.retain(|txid, _| in_pool.contains(txid));
    }

    /// Drops everything remembered: the pool isn't being watched.
    fn forget(&self) {
        let mut remembered = self.inner.lock();
        remembered.bodies = Bodies::default();
        remembered.scanned = HashMap::new();
    }

    /// Whether any store has been scanned for this transaction yet.
    fn is_new(&self, txid: &str) -> bool {
        !self.inner.lock().scanned.contains_key(txid)
    }

    fn mark_scanned(&self, txid: &str, tenant_id: &crate::store::TenantId, generation: u64) {
        self.inner
            .lock()
            .scanned
            .entry(txid.to_string())
            .or_default()
            .insert(tenant_id.clone(), generation);
    }

    /// The stores in `tenants` not yet scanned for `txid` with their current
    /// window.
    fn due<'a>(
        &self,
        txid: &str,
        tenants: &'a [TenantWindow],
        failed: &HashSet<crate::store::TenantId>,
    ) -> Vec<&'a TenantWindow> {
        let remembered = self.inner.lock();
        let done = remembered.scanned.get(txid);
        tenants
            .iter()
            .filter(|(id, _, _)| !failed.contains(id))
            .filter(|(id, _, window)| done.and_then(|d| d.get(id)) != Some(&window.generation()))
            .collect()
    }
}

pub(crate) struct MempoolRound {
    done: bool,
    /// Whether there is anything to look for in the pool ([`watching`]),
    /// as decided when the round started.
    watching: Option<Result<bool, ScannerError>>,
    /// The pool's transaction ids as asked for with the round's tip, when
    /// they were.
    polled: Option<crate::daemon::PoolAnswer>,
}

impl MempoolRound {
    pub(super) fn starting(
        watching: Result<bool, ScannerError>,
        polled: Option<crate::daemon::PoolAnswer>,
    ) -> Self {
        Self {
            done: false,
            watching: Some(watching),
            polled,
        }
    }
}

/// Whether there is anything to look for in the pool: a store with an
/// order in scope, or a payment waiting for a block.
pub(super) async fn watching(inputs: &RoundInputs<'_>, now: i64) -> Result<bool, ScannerError> {
    let (network, grace) = (inputs.network, inputs.grace_period_seconds);
    inputs
        .db
        .run(Class::Scanner, move |s| -> Result<bool, ScannerError> {
            Ok(!s
                .active_tenants_page(network, now, grace, "", 1)?
                .is_empty()
                || !s.unconfirmed_payments_page(network, 0, 1)?.is_empty())
        })
        .await
}

/// The round's mempool tier: one unit per round.
pub(super) async fn step(round: &mut Round<'_>, until: Instant) -> Progress {
    if round.mempool.done {
        return Progress::Idle;
    }
    round.mempool.done = true;
    let network = round.network();
    let state = &round.state.mempool;
    // Nothing to look for in the pool: no store has an order in scope and
    // no payment is waiting for a block. The node isn't asked.
    match round.mempool.watching.take() {
        Some(Ok(true)) => {}
        Some(Ok(false)) | None => {
            state.forget();
            return Progress::Idle;
        }
        Some(Err(error)) => return Progress::Failed(error),
    }
    // The pool was asked for with the round's tip.
    let answer = match round.mempool.polled.take() {
        Some(answer) => answer.map_err(ScannerError::from),
        None => bounded(round.inputs.daemon.get_mempool_txids()).await,
    };
    let Some(pool_txids) = readable(round.inputs, answer) else {
        return Progress::Blocked(Wait::MempoolUnreadable);
    };
    let in_pool: HashSet<String> = pool_txids.iter().cloned().collect();
    state.retain_pool(&in_pool);
    round.pool_txids = Some(in_pool);
    if pool_txids.is_empty() {
        return Progress::Advanced;
    }
    let tenants = match tenant_page(round).await {
        Ok(tenants) => tenants,
        Err(error) => return Progress::Failed(error),
    };
    // Nobody to scan for (the poll was for the vanished-payment check, or
    // no store's keys are registered): no bodies are fetched.
    if tenants.is_empty() {
        return Progress::Advanced;
    }
    let state = &round.state.mempool;
    let selected = select(state, pool_txids);
    let (pool, fetch_failed) = bodies(state, round.inputs, &selected).await;
    // A tenant that fails is retried next round, not once per transaction:
    // one unresponsive backend mustn't spend the round on deadlines.
    let mut failed: HashSet<crate::store::TenantId> = HashSet::new();
    let mut attempted = 0;
    for (txid, tx) in &pool {
        if attempted > 0 && Instant::now() >= until {
            break;
        }
        attempted += 1;
        let mut due = state.due(txid, &tenants, &failed);
        if due.is_empty() {
            continue;
        }
        let offset = state
            .next_tenant_offset
            .fetch_add(TENANTS_PER_TX, Ordering::Relaxed)
            % due.len();
        due.rotate_left(offset);
        due.truncate(TENANTS_PER_TX);
        let outcome = scan_and_record(round.state, round.inputs, tx, txid, &due, None).await;
        failed.extend(outcome.failed);
        if let Some(error) = outcome.store_error {
            tracing::warn!(network = crate::network::network_str(network), error = %error, "recording a mempool match failed (retried next round)");
        }
    }
    // Advance by the work actually attempted, so a slow first transaction
    // isn't revisited forever when the time allowance stops the slice early.
    // A slice whose bodies couldn't be fetched is tried again next round.
    let advance = match (attempted, fetch_failed) {
        (0, true) => 0,
        (0, false) => selected.len(),
        (attempted, _) => attempted,
    };
    state.next_tx_offset.fetch_add(advance, Ordering::Relaxed);
    Progress::Advanced
}

/// What a fast pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FastReport {
    /// New transactions scanned.
    pub scanned: usize,
    /// Orders with a new payment.
    pub paid_orders: usize,
    /// New transactions left for the rotation (over the scan budget).
    pub deferred: usize,
}

/// Scans the pool's new transactions against every store with something in
/// scope, and records and settles what they pay, straight away (see the
/// module doc). Returns `None` if the pool couldn't be read.
pub async fn fast_pass(state: &ScanState, inputs: &RoundInputs<'_>) -> Option<FastReport> {
    let mut report = FastReport::default();
    // Who to scan for comes first: with no store to scan for, the pool
    // isn't asked about at all (several times a second, otherwise).
    let tenants = match all_windows(state, inputs).await {
        Ok(tenants) => tenants,
        Err(error) => {
            tracing::warn!(network = ?inputs.network, error = %error, "loading scan windows for the mempool failed (retried)");
            return Some(report);
        }
    };
    if tenants.is_empty() {
        return Some(report);
    }
    let pool_txids = readable(inputs, bounded(inputs.daemon.get_mempool_txids()).await)?;
    let mempool = &state.mempool;
    let in_pool: HashSet<String> = pool_txids.iter().cloned().collect();
    mempool.retain_pool(&in_pool);
    let mut new: Vec<String> = pool_txids
        .into_iter()
        .filter(|txid| mempool.is_new(txid))
        .collect();
    if new.is_empty() {
        return Some(report);
    }
    // As many new transactions as the scan budget covers for every store.
    let fits = (FAST_SCANS_PER_PASS / tenants.len()).clamp(1, FAST_TXS_PER_PASS);
    new.sort_unstable();
    report.deferred = new.len().saturating_sub(fits);
    new.truncate(fits);
    let (pool, _) = bodies(mempool, inputs, &new).await;
    let tip = match mempool.last_tip.load(Ordering::Relaxed) {
        0 => bounded(inputs.daemon.get_height()).await.ok(),
        tip => Some(tip),
    };
    let mut failed = HashSet::new();
    for (txid, tx) in &pool {
        let due = mempool.due(txid, &tenants, &failed);
        let outcome = scan_and_record(state, inputs, tx, txid, &due, tip).await;
        report.scanned += 1;
        report.paid_orders += outcome.touched;
        failed.extend(outcome.failed);
    }
    if report.paid_orders > 0 {
        state.wake_webhooks();
    }
    Some(report)
}

/// What scanning one transaction for some stores did.
#[derive(Default)]
struct ScanOutcome {
    touched: usize,
    failed: Vec<crate::store::TenantId>,
    store_error: Option<ScannerError>,
}

/// Scans `tx` for `due` and records each match. With a `tip`, the orders it
/// pays are recomputed in the same database job (status, webhook), so a
/// payment is settled as soon as it is seen; without, their recompute
/// obligations are left to the settlement tier. A scan that found nothing
/// costs no database job at all. A store whose scan failed is backed off.
async fn scan_and_record(
    state: &ScanState,
    inputs: &RoundInputs<'_>,
    tx: &Transaction,
    txid: &str,
    due: &[&TenantWindow],
    tip: Option<u64>,
) -> ScanOutcome {
    let mut outcome = ScanOutcome::default();
    if due.is_empty() {
        return outcome;
    }
    let generations: HashMap<&str, u64> = due
        .iter()
        .map(|(id, _, w)| (id.as_str(), w.generation()))
        .collect();
    for (tenant_id, result) in scan_for_tenants(inputs.custody, txid, tx, due, SCAN_DEADLINE).await
    {
        let scan = match result {
            Ok(scan) => scan,
            Err(error) => {
                shared::throttled!(format!("mempool-scan:{tenant_id}"), warn, store.id = %tenant_id, network = ?inputs.network,
                    error = %error, "scanning a mempool transaction failed");
                state.backoff.failed(&tenant_id);
                outcome.failed.push(tenant_id);
                continue;
            }
        };
        let generation = generations
            .get(tenant_id.as_str())
            .copied()
            .unwrap_or_default();
        let Some(scan) = scan else {
            state.mempool.mark_scanned(txid, &tenant_id, generation);
            continue;
        };
        let (id, now) = (tenant_id.clone(), crate::now_unix());
        let recorded = inputs
            .db
            .run(Class::Scanner, move |s| {
                s.in_transaction(|s| -> Result<usize, ScannerError> {
                    let touched = record_scan_match(s, &id, &scan, now, None)?;
                    if let Some(tip) = tip {
                        for order_id in &touched {
                            crate::scanner::recompute_and_notify_in_tx(s, order_id, tip, now)?;
                        }
                    }
                    Ok(touched.len())
                })
            })
            .await;
        match recorded {
            Ok(touched) => {
                outcome.touched += touched;
                state.mempool.mark_scanned(txid, &tenant_id, generation);
            }
            Err(error) => {
                outcome.store_error.get_or_insert(error);
            }
        }
    }
    outcome
}

/// The pool's transaction ids, or `None` (logged) if the node couldn't say:
/// never "we couldn't look" read as "the pool is empty".
fn readable(
    inputs: &RoundInputs<'_>,
    answer: Result<Vec<String>, ScannerError>,
) -> Option<Vec<String>> {
    match answer {
        Ok(txids) => Some(txids),
        Err(error) => {
            shared::throttled!(format!("mempool-poll:{:?}", inputs.network), warn, network = ?inputs.network, error = %error,
                "polling the mempool failed - no zero-conf detection until it answers");
            None
        }
    }
}

/// The round's slice of the pool: transactions no store has been scanned
/// for yet come first, then a rotating slice of the rest.
fn select(state: &MempoolState, pool_txids: Vec<String>) -> Vec<String> {
    let (mut new, mut seen): (Vec<String>, Vec<String>) =
        pool_txids.into_iter().partition(|txid| state.is_new(txid));
    new.sort_unstable();
    seen.sort_unstable();
    if !seen.is_empty() {
        let offset = state.next_tx_offset.load(Ordering::Relaxed) % seen.len();
        seen.rotate_left(offset);
    }
    new.extend(seen);
    new.truncate(TXS_PER_ROUND);
    new
}

/// The bodies of `txids`, fetching those not remembered in one call.
/// Returns the bodies found (each with its id: a body may be pruned, and
/// then doesn't hash to it), in order, and whether the fetch failed.
async fn bodies(
    state: &MempoolState,
    inputs: &RoundInputs<'_>,
    txids: &[String],
) -> (Vec<(String, Arc<Transaction>)>, bool) {
    let missing: Vec<String> = {
        let remembered = state.inner.lock();
        txids
            .iter()
            .filter(|txid| !remembered.bodies.contains_key(txid))
            .cloned()
            .collect()
    };
    let mut fetched: HashMap<String, Arc<Transaction>> = HashMap::new();
    let mut fetch_failed = false;
    if !missing.is_empty() {
        match bounded(inputs.daemon.get_transactions_with_ids(&missing)).await {
            Ok(txs) => {
                let mut remembered = state.inner.lock();
                for crate::daemon::FetchedTx { txid, tx } in txs {
                    let tx = Arc::new(tx);
                    remembered
                        .bodies
                        .remember(&txid, &tx, MAX_BODIES, MAX_BODY_BYTES);
                    fetched.insert(txid, tx);
                }
            }
            Err(error) => {
                fetch_failed = true;
                tracing::warn!(network = ?inputs.network, transactions = missing.len(), error = %error,
                    "fetching new mempool transactions failed (retried)");
            }
        }
    }
    let remembered = state.inner.lock();
    let pool = txids
        .iter()
        .filter_map(|txid| {
            remembered
                .bodies
                .get(txid)
                .or_else(|| fetched.get(txid))
                .map(|tx| (txid.clone(), tx.clone()))
        })
        .collect();
    (pool, fetch_failed)
}

/// The next page of stores with something in scope, with their scan windows
/// as of now, for the round's rotation. Wraps round to the first page after
/// the last.
async fn tenant_page(round: &Round<'_>) -> Result<Vec<TenantWindow>, ScannerError> {
    let (grace, now) = (round.inputs.grace_period_seconds, round.now);
    let after = round.state.mempool.tenant_page_after.lock().clone();
    let (page, next_after) = round
        .db(move |s, network| -> Result<_, ScannerError> {
            let mut page: Vec<crate::store::TenantId> = s
                .active_tenants_page(network, now, grace, &after, TENANT_PAGE)?
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            let full = page.len() == TENANT_PAGE;
            let next_after = if full {
                page.last().map(|id| id.to_string()).unwrap_or_default()
            } else {
                String::new()
            };
            if !full && !after.is_empty() {
                // Wrap round: fill the page from the start.
                page.extend(
                    s.active_tenants_page(network, now, grace, "", TENANT_PAGE - page.len())?
                        .into_iter()
                        .map(|(id, _)| id),
                );
            }
            page.sort_unstable();
            page.dedup();
            let mut windows = s.scan_windows(&page, now, grace)?;
            Ok((
                page.into_iter()
                    .filter_map(|id| windows.remove(&id).map(|w| (id, w)))
                    .collect::<Vec<_>>(),
                next_after,
            ))
        })
        .await?;
    *round.state.mempool.tenant_page_after.lock() = next_after;
    Ok(with_handles(round.state, &round.handles, &page))
}

/// Every store with something in scope and its window, reloaded at most
/// once per `WINDOWS_TTL`, for the fast path.
async fn all_windows(
    state: &ScanState,
    inputs: &RoundInputs<'_>,
) -> Result<Vec<TenantWindow>, ScannerError> {
    let cached = state
        .mempool
        .windows
        .lock()
        .as_ref()
        .filter(|(at, _)| at.elapsed() < WINDOWS_TTL)
        .map(|(_, w)| w.clone());
    let windows = match cached {
        Some(windows) => windows,
        None => {
            let (network, grace, now) = (
                inputs.network,
                inputs.grace_period_seconds,
                crate::now_unix(),
            );
            let loaded = inputs
                .db
                .run(
                    Class::Scanner,
                    move |s| -> Result<Vec<(crate::store::TenantId, Vec<u32>)>, ScannerError> {
                        let mut ids = Vec::new();
                        let mut after = String::new();
                        loop {
                            let page =
                                s.active_tenants_page(network, now, grace, &after, TENANT_PAGE)?;
                            let full = page.len() == TENANT_PAGE;
                            after = page
                                .last()
                                .map(|(id, _)| id.to_string())
                                .unwrap_or_default();
                            ids.extend(page.into_iter().map(|(id, _)| id));
                            if !full {
                                break;
                            }
                        }
                        let mut windows = s.scan_windows(&ids, now, grace)?;
                        Ok(ids
                            .into_iter()
                            .filter_map(|id| windows.remove(&id).map(|w| (id, w)))
                            .collect())
                    },
                )
                .await?;
            let loaded = Arc::new(loaded);
            *state.mempool.windows.lock() = Some((Instant::now(), loaded.clone()));
            loaded
        }
    };
    let handles: HashMap<&str, WalletHandle> = inputs
        .tenants
        .iter()
        .map(|(id, h)| (id.as_str(), *h))
        .collect();
    Ok(with_handles(state, &handles, &windows))
}

/// The stores that can be scanned now: keys registered and not waiting out
/// a retry delay. (Every window listed has something in it.)
fn with_handles(
    state: &ScanState,
    handles: &HashMap<&str, WalletHandle>,
    windows: &[(crate::store::TenantId, Vec<u32>)],
) -> Vec<TenantWindow> {
    windows
        .iter()
        .filter(|(id, _)| !state.backoff.is_waiting(id))
        .filter_map(|(id, window)| {
            handles
                .get(id.as_str())
                .map(|h| (id.clone(), *h, ScanIndices::new(window.clone())))
        })
        .collect()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// Past either cap, count or bytes, bodies are used but not kept; what
    /// leaves the pool gives its bytes back.
    #[test]
    fn remembered_bodies_stop_at_the_count_or_byte_cap() {
        let tx = Arc::new(crate::scanner::tests::fixture_tx());
        let size = monero::consensus::encode::serialize(tx.as_ref()).len();
        let mut bodies = Bodies::default();
        bodies.remember("a", &tx, 2, usize::MAX);
        bodies.remember("b", &tx, 2, usize::MAX);
        bodies.remember("c", &tx, 2, usize::MAX);
        let mut kept: Vec<&String> = bodies.by_txid.keys().collect();
        kept.sort();
        assert_eq!(kept, ["a", "b"]);

        let mut bodies = Bodies::default();
        bodies.remember("a", &tx, 100, size * 2);
        bodies.remember("b", &tx, 100, size * 2);
        bodies.remember("c", &tx, 100, size * 2);
        assert_eq!(bodies.by_txid.len(), 2, "the byte cap holds");
        assert_eq!(bodies.bytes, size * 2);
        bodies.retain(|txid| txid != "a");
        assert_eq!(bodies.bytes, size);
        bodies.remember("c", &tx, 100, size * 2);
        assert!(bodies.contains_key("c"), "freed bytes are reused");
    }

    /// New transactions (no store scanned for them yet) come before the
    /// rotation, which then rotates through the rest.
    #[test]
    fn new_transactions_come_first_then_the_rotation() {
        let state = MempoolState::default();
        state.mark_scanned("b", &shared::ids::TenantId::new("t"), 1);
        state.mark_scanned("c", &shared::ids::TenantId::new("t"), 1);
        state.next_tx_offset.store(1, Ordering::Relaxed);
        let selected = select(&state, vec!["c".into(), "b".into(), "z".into(), "a".into()]);
        assert_eq!(selected, vec!["a", "z", "c", "b"]);
    }
}
