//! The `KeyCustody` boundary, from the `key-custody` crate.
//!
//! Each store's keys live in one backend of its own choosing; [`CustodyRouter`]
//! holds the enabled ones and routes each call to the backend that issued its
//! handle. Re-exported here so the engine names them
//! as its own. [`SnpSlot`] is how the engine starts the `snp` backend, in a
//! build with the `snp` feature; without it there is none, and enabling
//! `snp` is refused (`engine_settings::CustodyConfig`).

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use crate::store::TenantId;

#[cfg(feature = "snp")]
pub use ::key_custody::snp;
#[cfg(not(feature = "snp"))]
use ::key_custody::transport::TrustPolicy;
pub use ::key_custody::{
    remove_wallet_logged, router, size_scan_slots, transport, wallet_matches_address,
    CustodyRouter, KeyCustody, KeyCustodyError, MatchedOutput, Network, PlainKeyCustody,
    ScanIndices, ScanInput, SubaddressIndex, TxMatches, WalletHandle, WalletMaterial,
};

#[cfg(feature = "snp")]
mod snp_slot;
#[cfg(feature = "snp")]
pub use snp_slot::*;

/// What an engine built without the `snp` feature says when asked for it.
pub const SNP_NOT_BUILT: &str =
    "needs an engine built with the `snp` feature (cargo build --features snp); this one wasn't";

/// Without the `snp` feature there is no `snp` backend: this type has no
/// values, so `Custody::snp` is always `None`.
#[cfg(not(feature = "snp"))]
pub struct SnpSlot(std::convert::Infallible);

#[cfg(not(feature = "snp"))]
impl SnpSlot {
    pub fn trust(&self) -> Option<TrustPolicy> {
        match self.0 {}
    }

    pub fn backend(&self) -> Option<Arc<dyn KeyCustody>> {
        match self.0 {}
    }

    pub fn check(&self) -> Result<(), String> {
        match self.0 {}
    }

    pub fn start(&self) -> Result<Arc<dyn KeyCustody>, String> {
        match self.0 {}
    }
}

/// Stands in for a backend that couldn't start: every call says why, so its
/// stores are reported unavailable with the reason, and the other backends
/// carry on.
pub struct Unstarted(pub String);

impl Unstarted {
    fn error(&self) -> KeyCustodyError {
        KeyCustodyError::BackendUnavailable(self.0.clone())
    }
}

#[async_trait::async_trait]
impl KeyCustody for Unstarted {
    async fn register_wallet(
        &self,
        _material: WalletMaterial,
    ) -> Result<WalletHandle, KeyCustodyError> {
        Err(self.error())
    }
    async fn remove_wallet(&self, _handle: WalletHandle) -> Result<(), KeyCustodyError> {
        Err(KeyCustodyError::UnknownWallet)
    }
    async fn seal(&self, _material: &WalletMaterial) -> Result<Vec<u8>, KeyCustodyError> {
        Err(self.error())
    }
    async fn unseal_and_register(&self, _sealed: &[u8]) -> Result<WalletHandle, KeyCustodyError> {
        Err(self.error())
    }
    async fn derive_subaddress(
        &self,
        _handle: WalletHandle,
        _index: SubaddressIndex,
        _network: Network,
    ) -> Result<monero::Address, KeyCustodyError> {
        Err(self.error())
    }
    async fn scan_tx_outputs(
        &self,
        _handle: WalletHandle,
        _tx: &ScanInput,
        _major_range: Range<u32>,
        _minor_range: Range<u32>,
    ) -> Result<Vec<MatchedOutput>, KeyCustodyError> {
        Err(self.error())
    }
    fn unavailable(&self) -> Option<String> {
        Some(self.0.clone())
    }
    fn takes_raw_keys(&self) -> bool {
        false
    }
    async fn key_bundle(
        &self,
        _action: transport::Action,
        _store: Option<&str>,
    ) -> Result<transport::Bundle, KeyCustodyError> {
        Err(self.error())
    }
}

/// A tenant row's keys as this process has them registered.
///
/// The backend's handle, and the wallet they are the keys of. What is
/// derived or scanned with the handle belongs to that wallet, so whether it
/// still belongs to the row is a comparison of wallets, never of clocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveWallet {
    pub wallet_id: String,
    pub handle: WalletHandle,
}

#[cfg(test)]
impl LiveWallet {
    /// `handle` as the keys of the wallet `tenant_id` takes payments into
    /// now: for tests that register a store's keys themselves.
    pub fn of_store(
        store: &crate::store::Store,
        tenant_id: &TenantId,
        handle: WalletHandle,
    ) -> Self {
        Self {
            wallet_id: store
                .wallet_of(tenant_id)
                .unwrap()
                .expect("the store exists"),
            handle,
        }
    }
}

/// Each registered tenant row's [`LiveWallet`], and a lock per row.
///
/// Whatever reads a row's wallet and then acts on it holds the row's lock
/// throughout: shared to use the wallet (registering the row's keys,
/// creating an order), exclusive to change it (changing the store's
/// wallet, moving its keys, deleting the row). So a wallet read under the
/// lock is still the row's when the act lands, and no handle is put back
/// for a row that was deleted or moved on meanwhile. The handle map itself
/// is only ever held for a moment, never across an `.await`.
#[derive(Default)]
pub struct WalletHandles {
    handles: parking_lot::RwLock<HashMap<TenantId, LiveWallet>>,
    rows: parking_lot::Mutex<HashMap<TenantId, Arc<tokio::sync::RwLock<()>>>>,
}

impl From<HashMap<TenantId, LiveWallet>> for WalletHandles {
    fn from(handles: HashMap<TenantId, LiveWallet>) -> Self {
        Self {
            handles: parking_lot::RwLock::new(handles),
            rows: parking_lot::Mutex::default(),
        }
    }
}

impl WalletHandles {
    pub fn read(&self) -> parking_lot::RwLockReadGuard<'_, HashMap<TenantId, LiveWallet>> {
        self.handles.read()
    }

    pub fn write(&self) -> parking_lot::RwLockWriteGuard<'_, HashMap<TenantId, LiveWallet>> {
        self.handles.write()
    }

    /// The row's lock, shared: its wallet stays the one read under it.
    pub async fn using(&self, id: &TenantId) -> tokio::sync::OwnedRwLockReadGuard<()> {
        self.row(id).read_owned().await
    }

    /// The row's lock, exclusive: for changing its wallet or keys, or
    /// deleting it.
    pub async fn changing(&self, id: &TenantId) -> tokio::sync::OwnedRwLockWriteGuard<()> {
        self.row(id).write_owned().await
    }

    /// The lock for `id`. Locks nobody holds are dropped on the way, so
    /// there are only as many as rows in use.
    fn row(&self, id: &TenantId) -> Arc<tokio::sync::RwLock<()>> {
        let mut rows = self.rows.lock();
        rows.retain(|row, lock| row == id || Arc::strong_count(lock) > 1);
        Arc::clone(rows.entry(id.clone()).or_default())
    }
}
