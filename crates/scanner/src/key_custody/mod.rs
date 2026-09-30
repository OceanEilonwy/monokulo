//! The `KeyCustody` boundary.
//!
//! Each store's keys live in one backend of its own choosing (`plain` or
//! `socket`); [`CustodyRouter`] holds the enabled ones and routes each call to
//! the backend that issued its handle (`router.rs`, `docs/DESIGN.md` §6.4).
//!
//! [`PlainKeyCustody`] is the only *in-process* implementation, and stays defined
//! here - it keeps view pairs in ordinary process memory with no encryption and no
//! isolation from the host process, appropriate for a self-hosted, single-tenant
//! deployment where a host-level attacker already owns the one wallet on the box
//! regardless of what this backend does.
//!
//! The `KeyCustody` trait itself, and every type that crosses it (`WalletHandle`,
//! `WalletMaterial`, `KeyCustodyError`, `MatchedOutput`), moved to
//! `shared::key_custody` as of WBS 2.1.3 - this module now just re-exports them, so
//! every existing `scanner::key_custody::{KeyCustody, WalletHandle, ...}`
//! import elsewhere in this crate keeps compiling completely unchanged (a `pub use`
//! re-export is the same type, not a wrapper). **Read `shared::key_custody`'s own
//! module doc comment before assuming this was a style choice** - it's a structural
//! fix for a real `cyclic package dependency` Cargo error that only appears once
//! `main.rs` needs to depend on `key-custody-service` (for the socket-based
//! `SocketKeyCustody`) while `key-custody-service` needs these same types (for its
//! wire DTOs), and the doc comment there explains why the trait had to leave
//! `scanner` for that to resolve rather than the other way around.
//!
//! A multi-tenant, hosted deployment can implement this trait against a TEE (AWS
//! Nitro Enclaves, AMD SEV-SNP) instead of holding key material in-process at all,
//! so that compromising the host process yields scan requests and results but never
//! the keys that answered them - `key_custody_service::client::SocketKeyCustody`
//! (WBS 2.1.2/2.1.3) is the first step in that direction, forwarding every call to
//! a separate process over a Unix socket rather than a TEE, but drawing exactly the
//! same boundary. See the design notes in the project README for why SGX
//! specifically is a poor fit for this workload (secret-scalar EC multiplication is
//! exactly what its published side-channel attacks target) and why VM-based
//! isolation is preferred.

mod outputs;
mod plain;
pub mod router;
pub use router::CustodyRouter;

/// Whether `material` is the wallet `address` belongs to, on `network`:
/// the public spend key must match, and so must the public view key derived
/// from the private view key. Compares keys, not strings, so any valid
/// spelling of the address works. `Err` if the address doesn't parse or the
/// keys are malformed.
pub fn wallet_matches_address(
    material: &WalletMaterial,
    address: &str,
    network: Network,
) -> Result<bool, String> {
    let address: monero::Address = address
        .parse()
        .map_err(|_| format!("{address:?} is not a Monero address"))?;
    let pair = material.to_view_pair().map_err(|e| e.to_string())?;
    Ok(address.network == network
        && address.public_spend == pair.spend
        && address.public_view == monero::PublicKey::from_private_key(&pair.view))
}
pub use plain::PlainKeyCustody;

pub use shared::key_custody::{
    KeyCustody, KeyCustodyError, MatchedOutput, Network, ScanIndices, SubaddressIndex,
    WalletHandle, WalletMaterial,
};
