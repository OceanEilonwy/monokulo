//! The `KeyCustody` boundary, from the `key-custody` crate.
//!
//! Each store's keys live in one backend of its own choosing; [`CustodyRouter`]
//! holds the enabled ones and routes each call to the backend that issued its
//! handle (`docs/DESIGN.md` §6.4). Re-exported here so the engine names them
//! as its own.

pub use ::key_custody::{
    remove_wallet_logged, router, size_scan_slots, wallet_matches_address, CustodyRouter,
    KeyCustody, KeyCustodyError, MatchedOutput, Network, PlainKeyCustody, ScanIndices, ScanInput,
    SubaddressIndex, TxMatches, WalletHandle, WalletMaterial,
};
