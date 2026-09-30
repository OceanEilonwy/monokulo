//! What remains of the engine's first settings module: the shape of a saved
//! Monero node (`monero_node.<network>`, a JSON value) and the check that
//! the engine listens only privately. Every setting itself is declared,
//! read and saved through `engine_settings` (admin_settings_v2.md parts 1
//! and 2).

use serde::{Deserialize, Serialize};

/// One configured Monero node - the primary for a network, or one of its
/// fallbacks. Same shape `config.rs`'s own (now-removed) `MoneroNodeConfig`/
/// `MoneroFallbackNodeConfig` had, unified into one type (a fallback never
/// nested another `fallbacks` list either way) and given `Serialize` as well
/// as `Deserialize` - this now round-trips through a JSON column value and
/// the instance-admin HTTP API's own request/response bodies, not just a
/// one-way TOML parse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoneroNodeSetting {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub ssl: bool,
    #[serde(default = "default_true")]
    pub accept_self_signed_certs: bool,
    #[serde(default)]
    pub fallbacks: Vec<MoneroNodeSetting>,
}

fn default_true() -> bool {
    true
}

/// Whether an address the engine is listening on can only be reached from
/// this machine or a private network - loopback, RFC 1918 (`10/8`,
/// `172.16/12`, `192.168/16`), IPv6 unique-local (`fc00::/7`) or link-local
/// (`169.254/16`, `fe80::/10`). The engine is private: monokulo is the only
/// thing meant to talk to it (see `docs/DESIGN.md`'s monokulo boundary
/// section), so `main.rs` warns loudly at boot when this returns `false`.
///
/// The unspecified addresses (`0.0.0.0`, `::`) count as *not* private: they
/// listen on every interface, including a public one if the host has one.
/// An IPv4-mapped IPv6 address is judged by the IPv4 address it carries.
pub fn is_private_bind_address(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                // fc00::/7, unique local
                || (first & 0xfe00) == 0xfc00
                // fe80::/10, link-local
                || (first & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn the_default_bind_address_is_loopback_only() {
        let addr = crate::engine_settings::EngineSettings::defaults()
            .runtime
            .load()
            .bind;
        assert!(
            addr.ip().is_loopback(),
            "the engine must not listen publicly by default"
        );
        assert!(is_private_bind_address(addr.ip()));
    }

    #[test]
    fn bind_addresses_are_classified_as_private_or_public() {
        let private = [
            "127.0.0.1",
            "127.8.9.10",
            "::1",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.10.10",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "::ffff:10.1.2.3",
            "::ffff:127.0.0.1",
        ];
        for ip in private {
            assert!(
                is_private_bind_address(ip.parse().unwrap()),
                "{ip} should count as private"
            );
        }
        let public = [
            "0.0.0.0",
            "::",
            "8.8.8.8",
            "172.32.0.1",
            "192.169.0.1",
            "100.64.0.1",
            "2001:db8::1",
            "2a00:1450::1",
            "fec0::1",
            "::ffff:8.8.8.8",
        ];
        for ip in public {
            assert!(
                !is_private_bind_address(ip.parse().unwrap()),
                "{ip} should count as public"
            );
        }
    }

    #[test]
    fn a_saved_node_round_trips_through_json_including_fallbacks() {
        let node = MoneroNodeSetting {
            host: "primary.example".to_string(),
            port: 18081,
            ssl: false,
            accept_self_signed_certs: true,
            fallbacks: vec![MoneroNodeSetting {
                host: "backup.example".to_string(),
                port: 18081,
                ssl: true,
                accept_self_signed_certs: false,
                fallbacks: vec![],
            }],
        };
        let json = serde_json::to_string(&node).unwrap();
        assert_eq!(
            serde_json::from_str::<MoneroNodeSetting>(&json).unwrap(),
            node
        );
        let minimal: MoneroNodeSetting = serde_json::from_str(r#"{"host":"n","port":1}"#).unwrap();
        assert!(
            !minimal.ssl && minimal.accept_self_signed_certs && minimal.fallbacks.is_empty(),
            "the documented defaults"
        );
    }
}
