//! Key custody: where a store's private view key lives, and the boundary
//! everything else talks to it through.
//!
//! Everything on the caller's side of [`KeyCustody`] — the HTTP API, the chain
//! scanner, the tenant registry — deals only in [`WalletHandle`]s. Nobody
//! outside a `KeyCustody` implementation ever holds a private view key in their
//! own stack frame or struct. That's the whole point of drawing the line here:
//! it's the one seam an implementation slots behind to change *where* key
//! material physically lives and how it is protected at rest, without touching
//! the scanner, the tenant model, or the API layer at all.
//!
//! Custody is chosen per store: the engine runs several backends at once
//! behind a [`CustodyRouter`], which is itself a `KeyCustody`. The `*_in`
//! methods name the backend for a new registration; everything else follows
//! the handle.
//!
//! Backends:
//! - [`PlainKeyCustody`] (`plain`) keeps view pairs in ordinary process
//!   memory, and seals them as plain bytes. That suits a self-hosted,
//!   single-tenant deployment, where a host-level attacker already owns the
//!   one wallet on the box regardless of what this backend does.
//! - [`snp::SnpKeyCustody`] (`snp`) is for an engine running inside an AMD
//!   SEV-SNP confidential VM: memory is encrypted against the host by the
//!   hardware, keys at rest are sealed to the engine image's launch
//!   measurement, and merchants send their keys encrypted to the backend
//!   itself ([`transport`]).
//!
//! See the design notes in the project README for why SGX specifically is a
//! poor fit for this workload (secret-scalar EC multiplication is exactly what
//! its published side-channel attacks target) and why VM-based isolation is
//! preferred.
//!
//! Features: `backends` (default) is everything above. Without it, only
//! [`transport`] is built: what `key-custody-cli` and the browser need to
//! check a backend and encrypt keys to it. `wasm` adds the exports the
//! browser calls (built for `wasm32-unknown-unknown` by monokulo's build
//! script).

// Test modules are left out of coverage reports (`cargo +nightly llvm-cov`).
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

#[cfg(feature = "backends")]
mod custody;
#[cfg(feature = "backends")]
mod outputs;
#[cfg(feature = "backends")]
mod plain;
#[cfg(all(test, feature = "backends"))]
mod property_support;
#[cfg(feature = "backends")]
pub mod router;
#[cfg(feature = "snp")]
pub mod snp;
#[cfg(all(test, feature = "backends"))]
mod test_log;
pub mod transport;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod wasm;

#[cfg(feature = "backends")]
pub use custody::*;
#[cfg(feature = "backends")]
pub use plain::{size_scan_slots, PlainKeyCustody};
#[cfg(feature = "backends")]
pub use router::CustodyRouter;
