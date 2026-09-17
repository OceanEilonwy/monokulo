//! Shared helpers for `monero::Network`, used wherever a network name crosses a
//! text boundary: the config file (`[monero_node.<network>]`), the admin API
//! (`CreateTenantRequest.network`), and the `tenants.network` column.
//!
//! Moved to `shared::network` as of WBS 2.1.3 (see `shared::key_custody`'s module
//! doc comment for the full reasoning behind that step - this module rode along
//! for the same reason: `key-custody-service`'s wire DTOs need the identical
//! mainnet/stagenet/testnet mapping this crate already uses everywhere else, and
//! `key-custody-service` can no longer depend on `scanner` to reach it).
//! This module is now just a re-export, so every existing
//! `crate::network::{parse_network, network_str}` call in this crate keeps
//! compiling unchanged.

pub use shared::network::{network_str, parse_network, UnknownNetwork};
