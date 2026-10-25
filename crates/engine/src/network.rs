//! Shared helpers for `monero::Network`.
//!
//! Used wherever a network name crosses a text boundary: the config file
//! (`[monero_node.<network>]`), the admin API (`CreateTenantRequest.network`),
//! and the `tenants.network` column.
//!
//! Shared with monokulo (`engine_protocol::network`); re-exported here so the
//! engine names it as its own.

pub use engine_protocol::network::{network_str, parse_network, UnknownNetwork};
