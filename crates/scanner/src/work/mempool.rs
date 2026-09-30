//! The mempool tier: zero-confirmation payments.
//!
//! What it remembers between rounds (task 7.3), so a transaction sitting in
//! the pool is fetched once and scanned once per store, not every second:
//! - the bodies of transactions still in the pool;
//! - which (transaction, store, scan window) were scanned *successfully*.
//!   A failed scan isn't remembered, so it is retried; a store whose window
//!   changed is scanned again.
//!
//! Entries go when their transaction leaves the pool. Each round scans a
//! rotating slice of the pool against a rotating page of stores, so a huge
//! pool or tenant list costs a bounded amount per round.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use monero::Transaction;
use tokio::time::Instant;

use crate::key_custody::{ScanIndices, WalletHandle};
use crate::scanner::{record_scan_match, scan_for_tenants, tx_id_hex};

use super::{bounded, Progress, Round};

/// Most mempool transaction bodies remembered; beyond this (a spam wave)
/// new ones are scanned but not kept.
const MAX_BODIES: usize = 20_000;
const TXS_PER_ROUND: usize = 64;
const TENANTS_PER_TX: usize = 32;
/// Stores whose scan windows are loaded per round.
const TENANT_PAGE: usize = 256;

#[derive(Default)]
pub(crate) struct MempoolState {
    inner: parking_lot::Mutex<Remembered>,
    next_tx_offset: AtomicUsize,
    next_tenant_offset: AtomicUsize,
    tenant_page_after: parking_lot::Mutex<String>,
}

#[derive(Default)]
struct Remembered {
    bodies: HashMap<String, Arc<Transaction>>,
    scanned: HashMap<String, HashMap<String, u64>>,
}

#[derive(Default)]
pub(crate) struct MempoolRound {
    done: bool,
}

pub(super) async fn step(round: &mut Round<'_>, until: Instant) -> Progress {
    if round.mempool.done {
        return Progress::Idle;
    }
    round.mempool.done = true;
    let network = round.network().to_string();
    let state = &round.state.mempool;

    // A failed poll is survivable, but must not be silent: if it keeps
    // failing, zero-conf detection is off for this network.
    let pool_txids = match bounded(round.inputs.daemon.get_mempool_txids()).await {
        Ok(txids) => txids,
        Err(error) => {
            shared::throttled!(format!("mempool-poll:{network}"), warn, network = %network, error = %error,
                "polling the mempool failed - no zero-conf detection this round");
            return Progress::Blocked("the mempool couldn't be read");
        }
    };
    let in_pool: HashSet<String> = pool_txids.iter().cloned().collect();
    let mut selected = pool_txids;
    selected.sort_unstable();
    if !selected.is_empty() {
        let offset = state.next_tx_offset.load(Ordering::Relaxed) % selected.len();
        selected.rotate_left(offset);
        selected.truncate(TXS_PER_ROUND);
    }
    let missing: Vec<String> = {
        let mut remembered = state.inner.lock();
        remembered.bodies.retain(|txid, _| in_pool.contains(txid));
        remembered.scanned.retain(|txid, _| in_pool.contains(txid));
        selected.iter().filter(|txid| !remembered.bodies.contains_key(*txid)).cloned().collect()
    };
    let mut fetched: HashMap<String, Arc<Transaction>> = HashMap::new();
    if !missing.is_empty() {
        match bounded(round.inputs.daemon.get_transactions(&missing)).await {
            Ok(txs) => {
                let mut remembered = state.inner.lock();
                for tx in txs {
                    let tx = Arc::new(tx);
                    let txid = tx_id_hex(&tx);
                    if remembered.bodies.len() < MAX_BODIES {
                        remembered.bodies.insert(txid.clone(), tx.clone());
                    }
                    fetched.insert(txid, tx);
                }
            }
            Err(error) => tracing::warn!(network = %network, transactions = missing.len(), error = %error,
                "fetching new mempool transactions failed (retried next round)"),
        }
    }
    let pool: Vec<Arc<Transaction>> = {
        let remembered = state.inner.lock();
        selected.iter().filter_map(|txid| remembered.bodies.get(txid).or_else(|| fetched.get(txid)).cloned()).collect()
    };
    round.pool_txids = Some(in_pool);

    let tenants = match tenant_page(round).await {
        Ok(tenants) => tenants,
        Err(error) => return Progress::Failed(error),
    };
    let state = &round.state.mempool;
    // A tenant that fails is retried next round, not once per transaction:
    // one unresponsive backend mustn't spend the round on deadlines.
    let mut failed: HashSet<String> = HashSet::new();
    let mut attempted = 0;
    for tx in &pool {
        if attempted > 0 && Instant::now() >= until {
            break;
        }
        attempted += 1;
        let txid = tx_id_hex(tx);
        let mut due: Vec<&(String, WalletHandle, ScanIndices)> = {
            let remembered = state.inner.lock();
            let done = remembered.scanned.get(&txid);
            tenants
                .iter()
                .filter(|(id, _, _)| !failed.contains(id))
                .filter(|(id, _, window)| done.and_then(|d| d.get(id)) != Some(&window.generation()))
                .collect()
        };
        if due.is_empty() {
            continue;
        }
        let offset = state.next_tenant_offset.fetch_add(TENANTS_PER_TX, Ordering::Relaxed) % due.len();
        due.rotate_left(offset);
        due.truncate(TENANTS_PER_TX);
        let generations: HashMap<String, u64> = due.iter().map(|(id, _, w)| (id.clone(), w.generation())).collect();
        for (tenant_id, result) in scan_for_tenants(round.inputs.custody, tx, &due).await {
            match result {
                Ok(scan) => {
                    let (id, now) = (tenant_id.clone(), round.now);
                    let recorded = round.db(move |s, _| record_scan_match(s, &id, &scan, now, None)).await;
                    match recorded {
                        Ok(_) => {
                            if let Some(generation) = generations.get(&tenant_id) {
                                state.inner.lock().scanned.entry(txid.clone()).or_default().insert(tenant_id, *generation);
                            }
                        }
                        Err(error) => tracing::warn!(store.id = %tenant_id, network = %network, error = %error,
                            "recording a mempool match failed (retried next round)"),
                    }
                }
                Err(error) => {
                    shared::throttled!(format!("mempool-scan:{tenant_id}"), warn, store.id = %tenant_id, network = %network,
                        error = %error, "scanning a mempool transaction failed");
                    round.state.backoff.failed(&tenant_id);
                    failed.insert(tenant_id);
                }
            }
        }
    }
    // Advance by the work actually attempted, so a slow first transaction
    // isn't revisited forever when the time allowance stops the slice early.
    state.next_tx_offset.fetch_add(if attempted == 0 { selected.len() } else { attempted }, Ordering::Relaxed);
    Progress::Advanced
}

/// The next page of stores with something in scope, with their scan windows
/// as of now. Wraps round to the first page after the last.
async fn tenant_page(round: &Round<'_>) -> Result<Vec<(String, WalletHandle, ScanIndices)>, crate::scanner::ScannerError> {
    let (grace, now) = (round.inputs.grace_period_seconds, round.now);
    let after = round.state.mempool.tenant_page_after.lock().clone();
    let (page, next_after) = round
        .db(move |s, network| {
            let mut page = s.active_tenants_page(network, now, grace, &after, TENANT_PAGE)?;
            let full = page.len() == TENANT_PAGE;
            let next_after = if full { page.last().map(|(id, _)| id.clone()).unwrap_or_default() } else { String::new() };
            if !full && !after.is_empty() {
                // Wrap round: fill the page from the start.
                page.extend(s.active_tenants_page(network, now, grace, "", TENANT_PAGE - page.len())?);
            }
            let mut seen = HashSet::new();
            let mut windows = Vec::with_capacity(page.len());
            for (tenant_id, _) in page {
                if seen.insert(tenant_id.clone()) {
                    let window = s.scan_window(&tenant_id, now, grace)?;
                    windows.push((tenant_id, window));
                }
            }
            Ok((windows, next_after))
        })
        .await?;
    *round.state.mempool.tenant_page_after.lock() = next_after;
    let mut tenants = Vec::with_capacity(page.len());
    for (tenant_id, window) in page {
        if window.is_empty() || round.state.backoff.is_waiting(&tenant_id) {
            continue;
        }
        let Some(handle) = round.handles.get(tenant_id.as_str()) else { continue };
        tenants.push((tenant_id, *handle, ScanIndices::new(window)));
    }
    Ok(tenants)
}
