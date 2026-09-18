//! The `KeyCustody` boundary.
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

mod plain;
pub use plain::PlainKeyCustody;

pub use shared::key_custody::{
    KeyCustody, KeyCustodyError, MatchedOutput, Network, SubaddressIndex, WalletHandle,
    WalletMaterial,
};
