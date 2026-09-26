//! Every runtime-configurable setting this instance exposes - resolved at boot
//! (`main.rs`) and over the instance-admin HTTP API
//! (`http::instance_admin`) via the same `env > database > default`
//! precedence (`shared::settings`). Replaces the former `config.rs`'s
//! TOML-file-only model entirely: there is no config file any more, so every
//! setting that used to live in one now has a fixed key here instead, with
//! the exact same default value the TOML schema's own `Default` impls used.
//!
//! `[wallet]` (the former TOML section bootstrapping a single self-hosted
//! tenant at first boot) is deliberately *not* here - it was always a
//! one-time provisioning action, not an ongoing setting something should
//! keep reading on every boot, and now lives in `cli.rs` as an explicit
//! `--bootstrap-wallet` command instead (see that module's own doc comment).
//!
//! `monero_node.<network>` is the one setting stored as a JSON object rather
//! than a plain scalar - it's genuinely structured (host/port/TLS flags plus
//! an ordered fallback list), and flattening it into a dozen separate flat
//! keys per network would buy nothing a JSON blob doesn't already give an
//! admin page just as directly (one text field per network, not the finer
//! per-field granularity every scalar setting below gets, but still fully
//! inspectable and editable - "every setting exposed", not "every setting
//! exposed with identical UI").

use serde::{Deserialize, Serialize};

use shared::settings::{resolve_parsed, resolve_raw, SettingSource};

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

/// Reads and parses `monero_node.<network>` - `None` if that network has
/// never been configured (a real, valid state: an instance can watch only
/// mainnet, only a testnet for development, or any other subset). A stored
/// value that fails to parse as JSON is treated the same as absent (falls
/// back to unconfigured) with a warning on stderr, same "a bad stored value
/// shouldn't be able to block a boot" policy `shared::settings::
/// resolve_parsed` already applies to every scalar setting below - this
/// can't reuse that function directly since JSON, not `FromStr`, is this
/// value's real parse format.
pub fn monero_node_setting(store: &crate::store::Store, network: &str) -> Option<MoneroNodeSetting> {
    let key = format!("monero_node.{network}");
    let raw = store.get_setting(&key).ok().flatten()?;
    match serde_json::from_str(&raw) {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            eprintln!("settings: stored value for {key} is not valid JSON ({e}), treating {network} as unconfigured");
            None
        }
    }
}

pub fn set_monero_node_setting(
    store: &crate::store::Store,
    network: &str,
    value: &MoneroNodeSetting,
) -> Result<(), crate::store::StoreError> {
    let key = format!("monero_node.{network}");
    let raw = serde_json::to_string(value).expect("MoneroNodeSetting always serializes");
    store.set_setting(&key, &raw)
}

/// One scalar setting's identity - a stable key (used as the `settings` table
/// row's own primary key, and the instance-admin HTTP API's own field name),
/// the environment variable that overrides it, and the code default applied
/// when neither is set. Declared once so the HTTP listing
/// (`http::instance_admin::list_settings`) and each typed getter below read
/// off the exact same key/env-var/default triple - nothing here can drift
/// out of sync with what a caller actually resolves against.
pub struct ScalarSetting {
    pub key: &'static str,
    pub env_var: &'static str,
    pub default: &'static str,
}

macro_rules! scalar_settings {
    ($($name:ident => { key: $key:expr, env: $env:expr, default: $default:expr }),+ $(,)?) => {
        $(pub const $name: ScalarSetting = ScalarSetting { key: $key, env_var: $env, default: $default };)+
        /// Every scalar setting (i.e. everything except `monero_node.<network>`,
        /// which is JSON-shaped and listed separately) - see this module's own
        /// doc comment.
        pub const ALL_SCALAR: &[ScalarSetting] = &[$($name),+];
    };
}

scalar_settings! {
    KEY_CUSTODY_BACKEND => { key: "key_custody.backend", env: "SCANNER_KEY_CUSTODY_BACKEND", default: "plain" },
    KEY_CUSTODY_SOCKET_PATH => { key: "key_custody.socket_path", env: "SCANNER_KEY_CUSTODY_SOCKET_PATH", default: "" },
    PAYMENT_CONFIRMATIONS_REQUIRED => { key: "payment.confirmations_required", env: "SCANNER_PAYMENT_CONFIRMATIONS_REQUIRED", default: "10" },
    PAYMENT_ORDER_EXPIRY_MINUTES => { key: "payment.order_expiry_minutes", env: "SCANNER_PAYMENT_ORDER_EXPIRY_MINUTES", default: "30" },
    PAYMENT_REORG_CHECK_DEPTH => { key: "payment.reorg_check_depth", env: "SCANNER_PAYMENT_REORG_CHECK_DEPTH", default: "20" },
    PAYMENT_MEMPOOL_POLL_INTERVAL_MS => { key: "payment.mempool_poll_interval_ms", env: "SCANNER_PAYMENT_MEMPOOL_POLL_INTERVAL_MS", default: "1000" },
    PAYMENT_EXPIRED_ORDER_GRACE_PERIOD_MINUTES => { key: "payment.expired_order_grace_period_minutes", env: "SCANNER_PAYMENT_EXPIRED_ORDER_GRACE_PERIOD_MINUTES", default: "360" },
    PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB => { key: "payment.scan_chunk_memory_budget_mb", env: "SCANNER_PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB", default: "8" },
    SERVER_BIND => { key: "server.bind", env: "SCANNER_SERVER_BIND", default: "127.0.0.1:8443" },
    SERVER_WORKER_THREADS => { key: "server.worker_threads", env: "SCANNER_SERVER_WORKER_THREADS", default: "2" },
    SERVER_RATE_LIMIT_PER_TOKEN_PER_MIN => { key: "server.rate_limit_per_token_per_min", env: "SCANNER_SERVER_RATE_LIMIT_PER_TOKEN_PER_MIN", default: "120" },
    SERVER_MAX_BODY_BYTES => { key: "server.max_body_bytes", env: "SCANNER_SERVER_MAX_BODY_BYTES", default: "8192" },
    WEBHOOKS_ALLOW_PRIVATE_URLS => { key: "webhooks.allow_private_urls", env: "SCANNER_WEBHOOKS_ALLOW_PRIVATE_URLS", default: "false" },
    WEBHOOKS_DELIVERY_TIMEOUT_MS => { key: "webhooks.delivery_timeout_ms", env: "SCANNER_WEBHOOKS_DELIVERY_TIMEOUT_MS", default: "5000" },
    WEBHOOKS_MAX_ATTEMPTS => { key: "webhooks.max_attempts", env: "SCANNER_WEBHOOKS_MAX_ATTEMPTS", default: "8" },
}

/// The three fixed networks `monero_node.<network>` can be configured for -
/// used by both the instance-admin HTTP listing and `main.rs`'s own boot
/// sequence so neither can enumerate a different set than the other.
pub const NETWORKS: &[&str] = &["mainnet", "stagenet", "testnet"];

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

pub fn get<T: std::str::FromStr>(store: &crate::store::Store, setting: &ScalarSetting) -> T {
    let db_value = store.get_setting(setting.key).ok().flatten();
    resolve_parsed(setting.env_var, db_value.as_deref(), setting.default.parse().unwrap_or_else(|_| {
        panic!("{}'s own hardcoded default {:?} does not parse as the type it's requested as - a bug in this module, not a runtime input", setting.key, setting.default)
    }))
}

pub fn get_raw(store: &crate::store::Store, setting: &ScalarSetting) -> (String, SettingSource) {
    let db_value = store.get_setting(setting.key).ok().flatten();
    resolve_raw(setting.env_var, db_value.as_deref(), setting.default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn every_scalar_settings_own_default_parses_as_the_type_boot_code_actually_requests_it_as() {
        // `get::<T>` panics if a setting's own hardcoded `default` string doesn't
        // parse as whatever `T` a caller asks for - this pins that every default
        // above is at least self-consistent as a number/bool, independent of
        // which concrete type `main.rs` happens to request it as.
        let store = Store::open_in_memory().unwrap();
        let _: String = get(&store, &KEY_CUSTODY_BACKEND);
        let _: u64 = get(&store, &PAYMENT_CONFIRMATIONS_REQUIRED);
        let _: i64 = get(&store, &PAYMENT_ORDER_EXPIRY_MINUTES);
        let _: u64 = get(&store, &PAYMENT_REORG_CHECK_DEPTH);
        let _: u64 = get(&store, &PAYMENT_MEMPOOL_POLL_INTERVAL_MS);
        let _: i64 = get(&store, &PAYMENT_EXPIRED_ORDER_GRACE_PERIOD_MINUTES);
        let _: u32 = get(&store, &PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB);
        let _: String = get(&store, &SERVER_BIND);
        let _: usize = get(&store, &SERVER_WORKER_THREADS);
        let _: u32 = get(&store, &SERVER_RATE_LIMIT_PER_TOKEN_PER_MIN);
        let _: usize = get(&store, &SERVER_MAX_BODY_BYTES);
        let _: bool = get(&store, &WEBHOOKS_ALLOW_PRIVATE_URLS);
        let _: u64 = get(&store, &WEBHOOKS_DELIVERY_TIMEOUT_MS);
        let _: u32 = get(&store, &WEBHOOKS_MAX_ATTEMPTS);
    }

    #[test]
    fn the_default_bind_address_is_loopback_only() {
        let addr: std::net::SocketAddr = SERVER_BIND.default.parse().unwrap();
        assert!(addr.ip().is_loopback(), "the engine must not listen publicly by default");
        assert!(is_private_bind_address(addr.ip()));
    }

    #[test]
    fn bind_addresses_are_classified_as_private_or_public() {
        let private = [
            "127.0.0.1", "127.8.9.10", "::1", "10.0.0.1", "172.16.0.1", "172.31.255.255",
            "192.168.1.1", "169.254.10.10", "fc00::1", "fd12:3456::1", "fe80::1", "::ffff:10.1.2.3",
            "::ffff:127.0.0.1",
        ];
        for ip in private {
            assert!(is_private_bind_address(ip.parse().unwrap()), "{ip} should count as private");
        }
        let public = [
            "0.0.0.0", "::", "8.8.8.8", "172.32.0.1", "192.169.0.1", "100.64.0.1", "2001:db8::1",
            "2a00:1450::1", "fec0::1", "::ffff:8.8.8.8",
        ];
        for ip in public {
            assert!(!is_private_bind_address(ip.parse().unwrap()), "{ip} should count as public");
        }
    }

    #[test]
    fn a_saved_scalar_setting_is_read_back_over_the_default() {
        let store = Store::open_in_memory().unwrap();
        store.set_setting(PAYMENT_CONFIRMATIONS_REQUIRED.key, "3").unwrap();
        let value: u64 = get(&store, &PAYMENT_CONFIRMATIONS_REQUIRED);
        assert_eq!(value, 3);
    }

    #[test]
    fn an_env_var_overrides_a_saved_setting() {
        let store = Store::open_in_memory().unwrap();
        store.set_setting(PAYMENT_CONFIRMATIONS_REQUIRED.key, "3").unwrap();
        let _env = shared::settings::test_env::set(PAYMENT_CONFIRMATIONS_REQUIRED.env_var, Some("9"));
        let value: u64 = get(&store, &PAYMENT_CONFIRMATIONS_REQUIRED);
        assert_eq!(value, 9);
    }

    #[test]
    fn monero_node_setting_round_trips_through_json_including_fallbacks() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(monero_node_setting(&store, "mainnet"), None, "an unconfigured network reads as None, not a default node");

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
        set_monero_node_setting(&store, "mainnet", &node).unwrap();
        let read_back = monero_node_setting(&store, "mainnet").unwrap();
        assert_eq!(read_back, node);
        assert_eq!(monero_node_setting(&store, "stagenet"), None, "setting mainnet must not affect other networks");
    }

    #[test]
    fn a_malformed_stored_monero_node_value_reads_as_unconfigured_rather_than_panicking() {
        let store = Store::open_in_memory().unwrap();
        store.set_setting("monero_node.mainnet", "not valid json").unwrap();
        assert_eq!(monero_node_setting(&store, "mainnet"), None);
    }
}
