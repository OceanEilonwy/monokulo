//! Abuse protection that works for Tor and clearnet visitors alike
//! (`docs/workpacks/engine-boundary/README.md` step 9, `docs/ABUSE_PROTECTION.md`).
//!
//! - [`identity`]: who a request is from (Tor circuit, clearnet address
//!   behind trusted proxies, signed-in user, shop's secret key).
//! - [`proxy_protocol`]: the loopback onion listener that learns the Tor
//!   circuit from tor's PROXY header.
//! - [`streams`]: the cap on open live-update streams per (client, store).
//!
//! Everything here is in memory and per process: nothing is written to the
//! database and no cookie is ever set.

pub mod identity;
pub mod proxy_protocol;
pub mod streams;

use std::sync::{Arc, RwLock};

pub use identity::{ClientIdentity, TrustedProxies};

use crate::db::Db;
use crate::settings;

/// The abuse-protection settings in force, read from the settings table
/// (`crate::settings`) at startup and again whenever the admin settings
/// page saves ([`AbuseProtection::reload`]).
#[derive(Clone, Debug, PartialEq)]
pub struct AbuseConfig {
    pub trusted_proxies: TrustedProxies,
    /// Requests a minute one anonymous client may make to the public routes.
    pub per_client_per_min: u32,
    /// Requests a minute a shop's server may make with its store's key.
    pub per_store_key_per_min: u32,
    /// Open live-update streams per (client, store).
    pub stream_cap: usize,
}

impl Default for AbuseConfig {
    fn default() -> Self {
        AbuseConfig {
            trusted_proxies: TrustedProxies::default(),
            per_client_per_min: parse_default(&settings::RATE_LIMIT_PER_IP_PER_MIN),
            per_store_key_per_min: parse_default(&settings::RATE_LIMIT_PER_STORE_KEY_PER_MIN),
            stream_cap: parse_default(&settings::ABUSE_STREAM_CAP),
        }
    }
}

fn parse_default<T: std::str::FromStr>(setting: &settings::ScalarSetting) -> T {
    setting.default.parse().unwrap_or_else(|_| panic!("{}'s default doesn't parse", setting.key))
}

impl AbuseConfig {
    /// The current settings. A stored value that doesn't parse (possible only
    /// through an environment variable; the admin page refuses them) falls
    /// back to the default with a log line, like every other setting.
    pub fn from_settings(db: &Db) -> Self {
        let trusted_raw: String = settings::get(db, &settings::ABUSE_TRUSTED_PROXIES);
        let trusted_proxies = TrustedProxies::parse(&trusted_raw).unwrap_or_else(|e| {
            eprintln!("settings: abuse.trusted_proxies is invalid ({e}); trusting no proxy");
            TrustedProxies::default()
        });
        AbuseConfig {
            trusted_proxies,
            per_client_per_min: settings::get(db, &settings::RATE_LIMIT_PER_IP_PER_MIN),
            per_store_key_per_min: settings::get(db, &settings::RATE_LIMIT_PER_STORE_KEY_PER_MIN),
            stream_cap: settings::get(db, &settings::ABUSE_STREAM_CAP),
        }
    }
}

/// All abuse-protection state for one monokulo process, shared by every
/// request (`AppState::abuse`).
pub struct AbuseProtection {
    config: RwLock<AbuseConfig>,
    per_client: RwLock<Arc<shared::rate_limit::RateLimiter<ClientIdentity>>>,
    per_store_key: RwLock<Arc<shared::rate_limit::RateLimiter<ClientIdentity>>>,
    pub streams: Arc<streams::StreamLimiter>,
}

impl Default for AbuseProtection {
    fn default() -> Self {
        AbuseProtection::new(AbuseConfig::default())
    }
}

impl AbuseProtection {
    pub fn new(config: AbuseConfig) -> Self {
        AbuseProtection {
            per_client: RwLock::new(Arc::new(shared::rate_limit::RateLimiter::new(config.per_client_per_min))),
            per_store_key: RwLock::new(Arc::new(shared::rate_limit::RateLimiter::new(config.per_store_key_per_min))),
            streams: Arc::new(streams::StreamLimiter::new(config.stream_cap)),
            config: RwLock::new(config),
        }
    }

    pub fn config(&self) -> AbuseConfig {
        self.config.read().unwrap().clone()
    }

    /// Applies new settings without a restart (the onion listener's address
    /// is the one exception: it is bound at startup).
    pub fn reload(&self, config: AbuseConfig) {
        let old = self.config();
        if old.per_client_per_min != config.per_client_per_min {
            *self.per_client.write().unwrap() = Arc::new(shared::rate_limit::RateLimiter::new(config.per_client_per_min));
        }
        if old.per_store_key_per_min != config.per_store_key_per_min {
            *self.per_store_key.write().unwrap() = Arc::new(shared::rate_limit::RateLimiter::new(config.per_store_key_per_min));
        }
        self.streams.set_max(config.stream_cap);
        *self.config.write().unwrap() = config;
    }

    /// Spends one request of `client`'s budget; `false` when it's used up.
    /// A store's key has its own budget; everything else shares the
    /// per-client one.
    pub fn check(&self, client: &ClientIdentity, now: i64) -> bool {
        let limiter = match client {
            ClientIdentity::Store(_) => self.per_store_key.read().unwrap().clone(),
            _ => self.per_client.read().unwrap().clone(),
        };
        limiter.check(client.clone(), now)
    }
}
