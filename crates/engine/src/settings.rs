//! What remains of the engine's first settings module: the shape of a saved
//! Monero node (`monero_node.<network>`, a JSON value) and the check that
//! the engine listens only privately.
//!
//! Every setting itself is declared, read and saved through
//! `engine_settings` (`admin_settings_v2.md` parts 1 and 2).

use serde::{Deserialize, Serialize};

/// One configured Monero node - the primary for a network, or one of its
/// fallbacks.
///
/// Same shape `config.rs`'s own (now-removed) `MoneroNodeConfig`/
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
    pub fallbacks: Vec<Self>,
    /// The node's ZMQ publisher (monerod's `--zmq-pub`), `tcp://host:port`
    /// or `ipc:///path`: a block or pool transaction it announces wakes the
    /// scan at once instead of at the next poll (`docs/monero_zmq.md`). Needs
    /// an engine built with the `zmq` feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zmq_pub: Option<String>,
}

fn default_true() -> bool {
    true
}

/// The `check` for a node setting: a fallback's own `fallbacks` would be
/// accepted by the shape and never tried (only the top-level list is), so a
/// non-empty one is refused rather than saved and ignored.
///
/// An empty one is what a form sends for "none".
pub fn check_node(node: &Option<live_settings::Json<MoneroNodeSetting>>) -> Result<(), String> {
    let Some(node) = node else { return Ok(()) };
    for fallback in &node.0.fallbacks {
        if !fallback.fallbacks.is_empty() {
            return Err(format!(
                "fallback node {}:{} has fallbacks of its own; only the main node's fallbacks are tried, so list every fallback there",
                fallback.host, fallback.port
            ));
        }
    }
    for each in std::iter::once(&node.0).chain(&node.0.fallbacks) {
        if let Some(zmq_pub) = &each.zmq_pub {
            check_zmq_pub(zmq_pub)
                .map_err(|e| format!("node {}:{}: zmq_pub {e}", each.host, each.port))?;
        }
    }
    Ok(())
}

/// A `zmq_pub` this engine can subscribe to: monerod's own forms,
/// `tcp://host:port` and `ipc:///path`.
#[cfg(feature = "zmq")]
fn check_zmq_pub(zmq_pub: &str) -> Result<(), String> {
    let usage = "must be tcp://host:port or ipc:///path, as given to monerod's --zmq-pub";
    if let Some(path) = zmq_pub.strip_prefix("ipc://") {
        return if path.is_empty() {
            Err(usage.to_owned())
        } else {
            Ok(())
        };
    }
    let Some(address) = zmq_pub.strip_prefix("tcp://") else {
        return Err(usage.to_owned());
    };
    let (host, port) = address.rsplit_once(':').ok_or(usage)?;
    if host.is_empty() || port.parse::<u16>().is_err() {
        return Err(usage.to_owned());
    }
    Ok(())
}

#[cfg(not(feature = "zmq"))]
fn check_zmq_pub(_zmq_pub: &str) -> Result<(), String> {
    Err("needs an engine built with the `zmq` feature (on by default); this one was built without it".to_owned())
}

/// Whether an address the engine is listening on can only be reached from
/// this machine or a private network.
///
/// That is loopback, RFC 1918 (`10/8`, `172.16/12`, `192.168/16`), IPv6
/// unique-local (`fc00::/7`) or link-local (`169.254/16`, `fe80::/10`).
///
/// The engine is private: monokulo is the only thing meant to talk to it
/// (see `docs/DESIGN.md`'s monokulo boundary section), so `main.rs` warns
/// loudly at boot when this returns `false`.
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
    use live_settings::Json;

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
            host: "primary.example".to_owned(),
            port: 18081,
            ssl: false,
            accept_self_signed_certs: true,
            fallbacks: vec![MoneroNodeSetting {
                host: "backup.example".to_owned(),
                port: 18081,
                ssl: true,
                accept_self_signed_certs: false,
                fallbacks: vec![],
                zmq_pub: None,
            }],
            zmq_pub: Some("tcp://127.0.0.1:18083".to_owned()),
        };
        let json = serde_json::to_string(&node).unwrap();
        assert_eq!(
            serde_json::from_str::<MoneroNodeSetting>(&json).unwrap(),
            node
        );
        let minimal: MoneroNodeSetting = serde_json::from_str(r#"{"host":"n","port":1}"#).unwrap();
        assert!(
            !minimal.ssl
                && minimal.accept_self_signed_certs
                && minimal.fallbacks.is_empty()
                && minimal.zmq_pub.is_none(),
            "the documented defaults"
        );
        assert!(
            !serde_json::to_string(&minimal).unwrap().contains("zmq_pub"),
            "a node without a publisher saves as it did before"
        );
    }

    fn with_zmq(primary: Option<&str>, fallback: Option<&str>) -> Json<MoneroNodeSetting> {
        let node = |zmq_pub: Option<&str>| MoneroNodeSetting {
            host: "node.example".to_owned(),
            port: 18081,
            ssl: false,
            accept_self_signed_certs: true,
            fallbacks: vec![],
            zmq_pub: zmq_pub.map(str::to_owned),
        };
        let mut primary = node(primary);
        primary.fallbacks.push(node(fallback));
        Json(primary)
    }

    #[test]
    fn a_node_without_a_publisher_needs_no_zmq_support() {
        assert_eq!(check_node(&Some(with_zmq(None, None))), Ok(()));
    }

    #[cfg(feature = "zmq")]
    #[test]
    fn a_publisher_must_be_a_tcp_or_ipc_address() {
        for good in [
            "tcp://127.0.0.1:18083",
            "tcp://[::1]:18083",
            "tcp://monerod:18083",
            "ipc:///run/monerod/zmq.sock",
        ] {
            assert_eq!(
                check_node(&Some(with_zmq(Some(good), None))),
                Ok(()),
                "{good}"
            );
            assert_eq!(
                check_node(&Some(with_zmq(None, Some(good)))),
                Ok(()),
                "{good}"
            );
        }
        for bad in [
            "",
            "127.0.0.1:18083",
            "http://127.0.0.1:18083",
            "tcp://127.0.0.1",
            "tcp://:18083",
            "tcp://host:port",
            "ipc://",
            "inproc://x",
        ] {
            let error = check_node(&Some(with_zmq(Some(bad), None))).unwrap_err();
            assert!(error.contains("zmq_pub"), "{bad}: {error}");
        }
    }
}
