//! The `KeyCustody` boundary, from the `key-custody` crate.
//!
//! Each store's keys live in one backend of its own choosing; [`CustodyRouter`]
//! holds the enabled ones and routes each call to the backend that issued its
//! handle (`docs/DESIGN.md` §6.4). Re-exported here so the engine names them
//! as its own. [`SnpSlot`] is how the engine starts the `snp` backend, in a
//! build with the `snp` feature; without it there is none, and enabling
//! `snp` is refused (`engine_settings::CustodyConfig`).

use std::ops::Range;
#[cfg(not(feature = "snp"))]
use std::sync::Arc;

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
