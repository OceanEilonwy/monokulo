//! Independent ownership and cache-budget models for generated event histories.
use super::{MempoolState, ScanClaims};
use crate::key_custody::{ScanIndices, WalletHandle};
use crate::store::TenantId;
use monero::Transaction;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::sync::Arc;

type Key = (String, TenantId);
struct Owner<'a> {
    guard: ScanClaims<'a>,
    txid: String,
    pending: BTreeMap<TenantId, u64>,
}

/// Exercise actual reservations and cache operations without I/O or randomness.
pub(super) fn explore(data: &[u8]) {
    let state = MempoolState::default();
    let mut owners: Vec<Option<Owner<'_>>> = std::iter::repeat_with(|| None).take(8).collect();
    let mut completed: BTreeMap<Key, u64> = BTreeMap::new();
    let mut occupied: BTreeMap<Key, usize> = BTreeMap::new();
    let mut cached: BTreeMap<String, usize> = BTreeMap::new();
    let tx = Arc::new(Transaction::default());
    let handles: [WalletHandle; 8] = std::array::from_fn(|_| WalletHandle::generate());
    let header = |i| data.get(i).copied().unwrap_or(0);
    let max_count = usize::from(header(0) % 17);
    let max_bytes = usize::from(u16::from_le_bytes([header(1), header(2)]));
    for &[action, owner, transaction, mask, window, value] in data
        .get(3..)
        .unwrap_or_default()
        .as_chunks::<6>()
        .0
        .iter()
        .take(682)
    {
        let slot = usize::from(owner % 8);
        let txid = format!("transaction-{:02}", transaction % 8);
        let tenant = TenantId::new(format!("tenant-{}", value % 8));
        match action % 7 {
            0 => {
                release(&mut owners[slot], &mut occupied);
                let windows: Vec<_> = (0..8u8)
                    .filter(|i| mask & (1 << i) != 0)
                    .map(|i| {
                        // Different order and duplicates describe the same set.
                        let indices = [u32::from(window), u32::from(i), u32::from(window)];
                        (
                            TenantId::new(format!("tenant-{i}")),
                            handles[usize::from(i)],
                            ScanIndices::new(indices),
                        )
                    })
                    .collect();
                let mut pending = BTreeMap::new();
                for (id, _, indices) in &windows {
                    let key = (txid.clone(), id.clone());
                    if !occupied.contains_key(&key)
                        && completed.get(&key) != Some(&indices.generation())
                    {
                        pending.insert(id.clone(), indices.generation());
                        occupied.insert(key, slot);
                    }
                }
                let mut due: Vec<_> = windows.iter().collect();
                if value & 128 != 0 {
                    due.extend(windows.iter()); // duplicated input must not create two owners
                }
                let (guard, claimed) = state.claim(&txid, &due);
                let actual: Vec<_> = claimed.iter().map(|w| w.0.clone()).collect();
                assert_eq!(
                    actual,
                    pending.keys().cloned().collect::<Vec<_>>(),
                    "reservation admission differs from independent ownership model"
                );
                owners[slot] = Some(Owner {
                    guard,
                    txid: txid.clone(),
                    pending,
                });
            }
            1 => {
                if let Some(owner) = &mut owners[slot] {
                    let generation = owner.pending.get(&tenant).copied().unwrap_or(u64::MAX);
                    owner.guard.complete(&tenant, generation);
                    if owner.pending.remove(&tenant).is_some() {
                        let key = (owner.txid.clone(), tenant);
                        occupied.remove(&key);
                        completed.insert(key, generation);
                    }
                }
            }
            2 => release(&mut owners[slot], &mut occupied),
            3 => {
                state.forget();
                completed.clear();
                cached.clear();
            }
            4 => {
                let pool: HashSet<_> = (0..8u8)
                    .filter(|i| mask & (1 << i) != 0)
                    .map(|i| format!("transaction-{i:02}"))
                    .collect();
                state.retain_pool(&pool);
                completed.retain(|(t, _), _| pool.contains(t));
                cached.retain(|t, _| pool.contains(t));
            }
            5 => {
                let size = if value == u8::MAX {
                    usize::MAX
                } else {
                    usize::from(u16::from_le_bytes([window, value]))
                };
                let total: u128 = cached.values().map(|&n| n as u128).sum();
                if !cached.contains_key(&txid)
                    && cached.len() < max_count
                    && total + size as u128 <= max_bytes as u128
                {
                    cached.insert(txid.clone(), size);
                }
                state
                    .inner
                    .lock()
                    .bodies
                    .remember(&txid, &tx, size, max_count, max_bytes);
            }
            _ => {
                let windows: Vec<_> = (0..8u8)
                    .map(|i| {
                        (
                            TenantId::new(format!("tenant-{i}")),
                            handles[usize::from(i)],
                            ScanIndices::new([u32::from(window), u32::from(i)]),
                        )
                    })
                    .collect();
                let failed: HashSet<_> = windows
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| mask & (1 << i) != 0)
                    .map(|(_, w)| w.0.clone())
                    .collect();
                let expected: Vec<_> = windows
                    .iter()
                    .filter(|w| {
                        !failed.contains(&w.0)
                            && completed.get(&(txid.clone(), w.0.clone()))
                                != Some(&w.2.generation())
                    })
                    .map(|w| w.0.clone())
                    .collect();
                assert_eq!(
                    state
                        .due(&txid, &windows, &failed)
                        .iter()
                        .map(|w| w.0.clone())
                        .collect::<Vec<_>>(),
                    expected,
                    "due selection omitted retryable work or included failed/completed work"
                );
            }
        }
        // Compare entire state, not just cardinality: wrong-owner removals and
        // wrong-tenant completions can otherwise pass unnoticed.
        {
            let inner = state.inner.lock();
            let actual: BTreeMap<_, _> = inner
                .scanned
                .iter()
                .flat_map(|(t, tenants)| {
                    tenants
                        .iter()
                        .map(move |(id, &generation)| ((t.clone(), id.clone()), generation))
                })
                .collect();
            assert_eq!(
                actual, completed,
                "completed scan identities or windows differ"
            );
            let active: std::collections::BTreeSet<_> = inner.in_flight.iter().cloned().collect();
            assert_eq!(
                active,
                occupied.keys().cloned().collect(),
                "reservation ownership was lost or duplicated"
            );
            let bodies: BTreeMap<_, _> = inner
                .bodies
                .by_txid
                .iter()
                .map(|(id, (_, n))| (id.clone(), *n))
                .collect();
            assert_eq!(
                bodies, cached,
                "cached bodies differ from independent budget model"
            );
            assert_eq!(
                inner.bodies.bytes as u128,
                cached.values().map(|&n| n as u128).sum::<u128>(),
                "cache byte accounting differs from wide reference sum"
            );
        }
        let ids: std::collections::BTreeSet<_> = completed.keys().map(|(t, _)| t.clone()).collect();
        assert_eq!(
            state.is_new(&txid),
            !ids.contains(&txid),
            "newness differs from completed transaction set"
        );
        let listed: Vec<_> = ids
            .iter()
            .take(usize::from(value))
            .map(|id| shared::activity::short_id(id))
            .collect();
        assert_eq!(
            state.remembered(usize::from(value)),
            (ids.len(), listed),
            "remembered snapshot count/order/limit differs"
        );
    }
    drop(owners);
    assert!(
        state.inner.lock().in_flight.is_empty(),
        "dropping owners leaked reservations"
    );
}

fn release(owner: &mut Option<Owner<'_>>, occupied: &mut BTreeMap<Key, usize>) {
    if let Some(owner) = owner.take() {
        for id in owner.pending.keys() {
            occupied.remove(&(owner.txid.clone(), id.clone()));
        }
        drop(owner);
    }
}
