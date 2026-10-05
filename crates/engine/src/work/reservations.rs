//! Pure mempool scan ownership. The executor keeps each lease until success or Drop.
#![cfg_attr(
    not(any(test, feature = "fuzzing")),
    expect(
        unreachable_pub,
        reason = "publicly re-exported only by the exploration build"
    )
)]
use crate::store::TenantId;
use std::collections::{HashMap, HashSet};

/// Completed windows and live owners are independent of transaction-body caching.
#[derive(Default)]
pub struct Reservations {
    pub(crate) scanned: HashMap<String, HashMap<TenantId, u64>>,
    pub(crate) in_flight: HashSet<(String, TenantId)>,
}
/// One admitted scan, moved exactly once into completion or release.
pub struct Reservation {
    txid: String,
    tenant: TenantId,
    generation: u64,
}
impl Reservation {
    pub fn generation(&self) -> u64 {
        self.generation
    }
}
impl Reservations {
    pub fn claim(&mut self, txid: &str, tenant: &TenantId, generation: u64) -> Option<Reservation> {
        if self.completed(txid, tenant) == Some(generation) {
            return None;
        }
        let key = (txid.to_owned(), tenant.clone());
        if !self.in_flight.insert(key) {
            return None;
        }
        Some(Reservation {
            txid: txid.to_owned(),
            tenant: tenant.clone(),
            generation,
        })
    }
    pub fn complete(&mut self, reservation: Reservation) {
        self.in_flight
            .remove(&(reservation.txid.clone(), reservation.tenant.clone()));
        self.scanned
            .entry(reservation.txid)
            .or_default()
            .insert(reservation.tenant, reservation.generation);
    }
    pub fn release(&mut self, reservation: Reservation) {
        self.in_flight
            .remove(&(reservation.txid, reservation.tenant));
    }
    pub fn completed(&self, txid: &str, tenant: &TenantId) -> Option<u64> {
        self.scanned.get(txid).and_then(|v| v.get(tenant)).copied()
    }
    #[cfg(any(test, feature = "fuzzing"))]
    pub fn pending(&self) -> usize {
        self.in_flight.len()
    }
    /// Forgetting cached success never releases outstanding owners.
    #[cfg(any(test, feature = "fuzzing"))]
    pub fn forget_completed(&mut self) {
        self.scanned.clear();
    }
}
