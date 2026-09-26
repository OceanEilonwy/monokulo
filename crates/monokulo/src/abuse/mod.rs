//! Abuse protection that works for Tor and clearnet visitors alike
//! (`docs/workpacks/engine-boundary/README.md` step 9, `docs/ABUSE_PROTECTION.md`).
//!
//! - [`identity`]: who a request is from (Tor circuit, clearnet address
//!   behind trusted proxies, signed-in user, shop's secret key).
//! - [`proxy_protocol`]: the loopback onion listener that learns the Tor
//!   circuit from tor's PROXY header.
//! - [`limiter`]: soft and hard limits per client over a rolling minute.
//! - [`challenge`]: the signed proof-of-work (or, without JavaScript, wait)
//!   challenge a client past its soft limit must pass.
//! - [`streams`]: the cap on open live-update streams per (client, store).
//! - [`stats`]: challenge counts for operators.
//!
//! Everything here is in memory and per process: nothing is written to the
//! database and no cookie is ever set.

pub mod challenge;
pub mod identity;
pub mod limiter;
pub mod proxy_protocol;
pub mod stats;
pub mod streams;

use std::sync::{Arc, RwLock};

pub use identity::{ClientIdentity, TrustedProxies};
pub use limiter::{Limits, Tier};

use crate::db::Db;
use crate::settings;

/// The abuse-protection settings in force, read from the settings table
/// (`crate::settings`) at startup and again whenever the admin settings
/// page saves ([`AbuseProtection::reload`]).
#[derive(Clone, Debug, PartialEq)]
pub struct AbuseConfig {
    pub trusted_proxies: TrustedProxies,
    /// Requests a minute an anonymous client may make before a challenge.
    pub soft_per_min: u32,
    /// Requests a minute past which an anonymous client is refused.
    pub hard_per_min: u32,
    /// Requests a minute for a signed-in merchant (never challenged).
    pub signed_in_per_min: u32,
    /// Requests a minute a shop's server may make with its store's key.
    pub per_store_key_per_min: u32,
    /// Open live-update streams per (client, store).
    pub stream_cap: usize,
    /// Leading zero bits the proof-of-work needs.
    pub challenge_bits: u32,
    /// Challenge every anonymous visitor on pages and the JSON API.
    pub under_attack: bool,
}

impl Default for AbuseConfig {
    fn default() -> Self {
        AbuseConfig {
            trusted_proxies: TrustedProxies::default(),
            soft_per_min: parse_default(&settings::ABUSE_SOFT_PER_MIN),
            hard_per_min: parse_default(&settings::ABUSE_HARD_PER_MIN),
            signed_in_per_min: parse_default(&settings::ABUSE_SIGNED_IN_PER_MIN),
            per_store_key_per_min: parse_default(&settings::RATE_LIMIT_PER_STORE_KEY_PER_MIN),
            stream_cap: parse_default(&settings::ABUSE_STREAM_CAP),
            challenge_bits: parse_default(&settings::ABUSE_CHALLENGE_BITS),
            under_attack: parse_default(&settings::ABUSE_UNDER_ATTACK),
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
            soft_per_min: settings::get(db, &settings::ABUSE_SOFT_PER_MIN),
            hard_per_min: settings::get(db, &settings::ABUSE_HARD_PER_MIN),
            signed_in_per_min: settings::get(db, &settings::ABUSE_SIGNED_IN_PER_MIN),
            per_store_key_per_min: settings::get(db, &settings::RATE_LIMIT_PER_STORE_KEY_PER_MIN),
            stream_cap: settings::get(db, &settings::ABUSE_STREAM_CAP),
            challenge_bits: settings::get::<u32>(db, &settings::ABUSE_CHALLENGE_BITS).clamp(1, 32),
            under_attack: settings::get(db, &settings::ABUSE_UNDER_ATTACK),
        }
    }

    /// The limits that apply to `client`. Signed-in merchants and store keys
    /// get one limit each (soft = hard), so they are never challenged, only
    /// refused past it.
    pub fn limits_for(&self, client: &ClientIdentity) -> Limits {
        match client {
            ClientIdentity::User(_) => Limits { soft_per_min: self.signed_in_per_min, hard_per_min: self.signed_in_per_min },
            ClientIdentity::Store(_) => Limits { soft_per_min: self.per_store_key_per_min, hard_per_min: self.per_store_key_per_min },
            _ => Limits { soft_per_min: self.soft_per_min, hard_per_min: self.hard_per_min },
        }
    }
}

/// All abuse-protection state for one monokulo process, shared by every
/// request (`AppState::abuse`).
pub struct AbuseProtection {
    config: RwLock<AbuseConfig>,
    pub limiter: limiter::TieredLimiter<ClientIdentity>,
    pub challenges: challenge::Challenges,
    pub streams: Arc<streams::StreamLimiter>,
    pub stats: stats::ChallengeStats,
}

impl Default for AbuseProtection {
    fn default() -> Self {
        AbuseProtection::new(AbuseConfig::default())
    }
}

impl AbuseProtection {
    pub fn new(config: AbuseConfig) -> Self {
        AbuseProtection {
            limiter: limiter::TieredLimiter::default(),
            challenges: challenge::Challenges::default(),
            streams: Arc::new(streams::StreamLimiter::new(config.stream_cap)),
            stats: stats::ChallengeStats::default(),
            config: RwLock::new(config),
        }
    }

    pub fn config(&self) -> AbuseConfig {
        self.config.read().unwrap().clone()
    }

    /// Applies new settings without a restart (the onion listener's address
    /// is the one exception: it is bound at startup).
    pub fn reload(&self, config: AbuseConfig) {
        self.streams.set_max(config.stream_cap);
        *self.config.write().unwrap() = config;
    }

    /// Counts one request from `client` and says what it may do.
    /// `challengeable` is false for requests that can't be challenged
    /// (streams, form posts): those are never forced past soft by
    /// under-attack mode.
    pub fn check(&self, client: &ClientIdentity, challengeable: bool, now: i64) -> Tier {
        let config = self.config();
        let force_soft = challengeable && config.under_attack && !client.is_authenticated();
        self.limiter.check(client, config.limits_for(client), force_soft, now)
    }
}
