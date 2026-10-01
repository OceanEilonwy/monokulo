//! Every runtime-configurable monokulo setting the admin settings page
//! exposes (`http/admin_settings.rs`), resolved via the same
//! `env > database > default` precedence as the engine's, declared once
//! with the `live-settings` library (admin_settings_v2.md part 1): each
//! setting's type, range, description and example live in its declaration,
//! and saving through the registry applies the change to the running
//! process (part 3).
//!
//! **`MONOKULO_ENCRYPTION_KEY` is deliberately not here.** Every other
//! setting below can be changed at any time with no lasting consequence
//! beyond "the new value takes effect on the next read" - this one can't:
//! it's the AES-256-GCM key every `store_connections.tenant_secret_token_encrypted`
//! row was encrypted with (`crate::crypto`), so rotating it live (or even
//! exposing its current value on a settings page) would either corrupt
//! every already-encrypted secret token or leak the key that protects them.
//! It stays exactly what it always was: a required environment variable,
//! read once at boot (`main.rs::encryption_key_from_env`), with no database
//! fallback and no admin-page field.

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use live_settings::{
    choice_value, settings, AnySetting, FieldError, HttpUrl, Registry, Section, Setting,
    SettingValue, Snapshot, Warning,
};

use crate::abuse::{AbuseConfig, AbuseProtection, TrustedProxies};
use crate::db::{Database, Db};
use crate::engine_client::EngineClient;
use crate::exchange_rate_config::{ExchangeRateConfig, ExchangeRateProviders};

choice_value! {
    /// Who may sign up.
    pub enum SignupMode { Public = "public", InviteOnly = "invite_only" }
}

fn check_public_url(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Ok(())
    } else {
        validate_public_url(value).map(|_| ())
    }
}

fn check_trusted_proxies(value: &str) -> Result<(), String> {
    TrustedProxies::parse(value)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn check_onion_listener(value: &str) -> Result<(), String> {
    crate::abuse::proxy_protocol::validate_onion_listener(value).map(|_| ())
}

settings! {
    SIGNUP_MODE: SignupMode {
        key: "signup.mode",
        env: "MONOKULO_SIGNUP_MODE",
        default: SignupMode::InviteOnly,
        description: "Who can create an account: public (anyone) or invite_only (only people with an invite link from the admin).",
        example: "invite_only",
    },
    EXCHANGE_RATE_COINGECKO_ENABLED: bool {
        key: "exchange_rate.coingecko_enabled",
        env: "MONOKULO_EXCHANGE_RATE_COINGECKO_ENABLED",
        default: true,
        description: "Whether stores can price orders in fiat currencies using Coingecko's rates. Off, only XMR prices work.",
        example: "true",
    },
    EXCHANGE_RATE_COINGECKO_BASE_URL: HttpUrl {
        key: "exchange_rate.coingecko_base_url",
        env: "MONOKULO_EXCHANGE_RATE_COINGECKO_BASE_URL",
        default: live_settings::parsed_default("https://api.coingecko.com"),
        description: "Where Coingecko's API is reached. Change it only to use a proxy or mirror.",
        example: "https://api.coingecko.com",
    },
    EXCHANGE_RATE_COINMARKETCAP_ENABLED: bool {
        key: "exchange_rate.coinmarketcap_enabled",
        env: "MONOKULO_EXCHANGE_RATE_COINMARKETCAP_ENABLED",
        default: true,
        description: "Whether stores can price orders in fiat currencies using CoinMarketCap's rates. Each store still chooses whether to use it, and in what order.",
        example: "true",
    },
    EXCHANGE_RATE_COINMARKETCAP_BASE_URL: HttpUrl {
        key: "exchange_rate.coinmarketcap_base_url",
        env: "MONOKULO_EXCHANGE_RATE_COINMARKETCAP_BASE_URL",
        default: live_settings::parsed_default("https://pro-api.coinmarketcap.com/public-api"),
        description: "Where CoinMarketCap's keyless API is reached. Change it only to use a proxy or mirror.",
        example: "https://pro-api.coinmarketcap.com/public-api",
    },
    EXCHANGE_RATE_HAVENO_ENABLED: bool {
        key: "exchange_rate.haveno_enabled",
        env: "MONOKULO_EXCHANGE_RATE_HAVENO_ENABLED",
        default: false,
        description: "Whether stores can price orders using the RetoSwap (Haveno) order book, read through haveno.markets. It is a thin peer-to-peer market, so it prices only currencies with both buyers and sellers listed right now.",
        example: "false",
    },
    EXCHANGE_RATE_HAVENO_BASE_URL: HttpUrl {
        key: "exchange_rate.haveno_base_url",
        env: "MONOKULO_EXCHANGE_RATE_HAVENO_BASE_URL",
        default: live_settings::parsed_default("https://haveno.markets"),
        description: "Where the haveno.markets API is reached. Change it only to use a proxy or mirror.",
        example: "https://haveno.markets",
    },
    EXCHANGE_RATE_CACHE_SECONDS: u64 {
        key: "exchange_rate.cache_seconds",
        env: "MONOKULO_EXCHANGE_RATE_CACHE_SECONDS",
        default: 30,
        check: range(0, 86_400),
        description: "Seconds a fetched exchange rate is reused before asking its provider again.",
        example: "30",
    },
    HTTP_CACHE_MAX_MB: u64 {
        key: "http_cache.max_mb",
        env: "MONOKULO_HTTP_CACHE_MAX_MB",
        default: 16,
        check: range(1, 4096),
        description: "Megabytes of memory for monokulo's cache of engine responses.",
        example: "16",
    },
    DATABASE_READ_CONNECTIONS: usize {
        key: "database.read_connections",
        env: "MONOKULO_DATABASE_READ_CONNECTIONS",
        default: shared::sqlite::DEFAULT_READ_CONNECTIONS,
        check: range(1, 64),
        description: "Read-only connections monokulo opens to its database, each on its own thread. Reads run side by side, so more help up to the number of CPU cores; each keeps its own cache of about 2 MB. Takes effect after a restart.",
        example: "4",
        applies: Restart,
    },
    RATE_LIMIT_PER_STORE_KEY_PER_MIN: u32 {
        key: "rate_limit.per_store_key_per_min",
        env: "MONOKULO_RATE_LIMIT_PER_STORE_KEY_PER_MIN",
        default: 600,
        check: range(1, 10_000_000),
        description: "Requests a minute a shop's server may make with its store's secret key (for example the WooCommerce plugin creating orders). These are never challenged.",
        example: "600",
    },
    PUBLIC_URL: String {
        key: "public_url",
        env: "MONOKULO_PUBLIC_URL",
        default: String::new(),
        check: |v: &String| check_public_url(v),
        description: "This instance's public address, e.g. https://pay.example.com or an http://....onion address. Plugins such as WooCommerce are given it when they connect, and send customers to its checkout. Plugins can't connect until it is set.",
        example: "https://pay.example.com",
    },
    ABUSE_TRUSTED_PROXIES: String {
        key: "abuse.trusted_proxies",
        env: "MONOKULO_ABUSE_TRUSTED_PROXIES",
        default: String::new(),
        check: |v: &String| check_trusted_proxies(v),
        description: "Addresses and CIDR ranges of reverse proxies in front of this instance, comma-separated. A request from one of these is identified by the last address in its X-Forwarded-For header that isn't a trusted proxy. Leave empty if clients connect directly.",
        example: "127.0.0.1, 10.0.0.0/8",
    },
    ABUSE_ONION_LISTENER: String {
        key: "abuse.onion_listener",
        env: "MONOKULO_ABUSE_ONION_LISTENER",
        default: String::new(),
        check: |v: &String| check_onion_listener(v),
        description: "A loopback address:port for tor's onion service to connect to, with HiddenServiceExportCircuitID haproxy set in torrc, so each Tor circuit is its own client. Empty turns it off. Only loopback is accepted.",
        example: "127.0.0.1:8082",
    },
    ABUSE_STREAM_CAP: usize {
        key: "abuse.stream_cap",
        env: "MONOKULO_ABUSE_STREAM_CAP",
        default: 16,
        check: range(1, 100_000),
        description: "Live-update streams one client may hold open at once to one store.",
        example: "16",
    },
    ABUSE_SOFT_PER_MIN: u32 {
        key: "abuse.soft_per_min",
        env: "MONOKULO_ABUSE_SOFT_PER_MIN",
        default: 60,
        check: range(1, 10_000_000),
        description: "Requests a minute one visitor (a Tor circuit, or an address) may make to the checkout and public pages before being asked to solve a short challenge. Signed-in merchants and shops using their secret key are never challenged.",
        example: "60",
    },
    ABUSE_HARD_PER_MIN: u32 {
        key: "abuse.hard_per_min",
        env: "MONOKULO_ABUSE_HARD_PER_MIN",
        default: 300,
        check: range(1, 10_000_000),
        description: "Requests a minute past which a visitor is refused outright (429) until the minute is up. Must be above the soft limit.",
        example: "300",
    },
    ABUSE_SIGNED_IN_PER_MIN: u32 {
        key: "abuse.signed_in_per_min",
        env: "MONOKULO_ABUSE_SIGNED_IN_PER_MIN",
        default: 600,
        check: range(1, 10_000_000),
        description: "Requests a minute a signed-in merchant may make (dashboard, POS). Never challenged.",
        example: "600",
    },
    ABUSE_CLIENT_LOGS_PER_MIN: u32 {
        key: "abuse.client_logs_per_min",
        env: "MONOKULO_ABUSE_CLIENT_LOGS_PER_MIN",
        default: 30,
        check: range(1, 100_000),
        description: "Log reports a minute one client may send (browser problem reports, POS session timelines, WooCommerce plugin errors). Past it, reports are dropped with a 429 - never a challenge - and the client's other requests are unaffected.",
        example: "30",
    },
    ABUSE_CHALLENGE_BITS: u32 {
        key: "abuse.challenge_bits",
        env: "MONOKULO_ABUSE_CHALLENGE_BITS",
        default: 16,
        check: range(8, 24),
        description: "How hard the challenge is, in leading zero bits of a SHA-256 hash. Each extra bit doubles the work; 16 takes a phone about a second.",
        example: "16",
    },
    ABUSE_UNDER_ATTACK: bool {
        key: "abuse.under_attack",
        env: "MONOKULO_ABUSE_UNDER_ATTACK",
        default: false,
        description: "When true, every visitor who isn't signed in must pass a challenge before using the checkout or public pages (live updates are not affected). A pass lasts 10 minutes.",
        example: "false",
    },
    LOGGING_LEVEL: String {
        key: "logging.level",
        env: "MONOKULO_LOG",
        default: telemetry::DEFAULT_LEVEL.to_string(),
        check: telemetry::check_level,
        description: "Which log lines monokulo writes: a level (error, warn, info, debug, trace), optionally followed by target=level pairs for parts of monokulo.",
        example: "info,monokulo::http=debug",
    },
    LOGGING_DEV_MODE_UNTIL: u64 {
        key: "logging.dev_mode_until",
        env: "MONOKULO_LOGGING_DEV_MODE_UNTIL",
        default: 0,
        check: range(0, i64::MAX),
        description: "Development logging: until this time monokulo logs at debug level, then goes back to the level above by itself. Secrets and addresses stay hidden either way.",
    },
    LOGGING_RETENTION_DAYS: u64 {
        key: "logging.retention_days",
        env: "MONOKULO_LOGGING_RETENTION_DAYS",
        default: telemetry::store::DEFAULT_RETENTION_DAYS,
        check: range(1, 365),
        description: "Days monokulo's log store keeps lines for the Logs page. Older lines are deleted once a minute.",
        example: "14",
    },
    LOGGING_MAX_MB: u64 {
        key: "logging.max_mb",
        env: "MONOKULO_LOGGING_MAX_MB",
        default: telemetry::store::DEFAULT_MAX_MB,
        check: range(10, 100_000),
        description: "Most megabytes monokulo's log store may use. Past it, the oldest lines are deleted first.",
        example: "500",
    },
    LOGGING_OTLP_ENDPOINT: String {
        key: "logging.otlp_endpoint",
        env: "MONOKULO_LOGGING_OTLP_ENDPOINT",
        default: String::new(),
        check: telemetry::otlp::check_endpoint,
        description: "An OpenTelemetry collector (OTLP over HTTP) to send monokulo's log lines and spans to as well, such as a Collector, Grafana, Seq or the Aspire Dashboard. Leave empty to keep them here only. They are redacted the same way either way.",
        example: "http://127.0.0.1:4318",
    },
    LOGGING_OTLP_HEADERS: live_settings::Secret {
        key: "logging.otlp_headers",
        env: "MONOKULO_LOGGING_OTLP_HEADERS",
        default: live_settings::Secret::default(),
        check: telemetry::otlp::check_headers,
        description: "Headers the collector needs, such as an API key, as name=value pairs separated by commas.",
    },
}

/// Log level and development mode (structured_logging.md task 1.3), applied
/// to the process-wide subscriber by `telemetry::LogReloadable`.
#[derive(Debug, Clone, PartialEq)]
pub struct LoggingConfig(pub telemetry::LogConfig);

impl AsRef<telemetry::LogConfig> for LoggingConfig {
    fn as_ref(&self) -> &telemetry::LogConfig {
        &self.0
    }
}

impl Section for LoggingConfig {
    const NAME: &'static str = "logging";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &LOGGING_LEVEL,
            &LOGGING_DEV_MODE_UNTIL,
            &LOGGING_RETENTION_DAYS,
            &LOGGING_MAX_MB,
            &LOGGING_OTLP_ENDPOINT,
            &LOGGING_OTLP_HEADERS,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(LoggingConfig(telemetry::LogConfig {
            level: snapshot.get(&LOGGING_LEVEL).trim().to_string(),
            dev_mode_until: snapshot.get(&LOGGING_DEV_MODE_UNTIL),
            retention_days: snapshot.get(&LOGGING_RETENTION_DAYS),
            max_mb: snapshot.get(&LOGGING_MAX_MB),
            otlp_endpoint: snapshot.get(&LOGGING_OTLP_ENDPOINT).trim().to_string(),
            otlp_headers: snapshot.get(&LOGGING_OTLP_HEADERS).expose().to_string(),
        }))
    }
}

/// A setting's effective value for a per-request read: environment, else
/// saved, else default. A value that doesn't parse (possible only from an
/// environment variable or a hand-edited row; the admin page refuses them)
/// gives the default, as the registry resolves it, so the admin page shows
/// the value requests use.
pub fn get<T: SettingValue>(db: &Db, setting: &Setting<T>) -> T {
    if let Some(raw) = shared::settings::env_value(setting.env_var) {
        if !raw.trim().is_empty() {
            match setting.parse(&raw) {
                Ok(value) => return value,
                Err(e) => {
                    tracing::warn!(setting = setting.key, env = setting.env_var, error = %e, "settings: the environment variable's value is invalid; using the default");
                    return setting.default_value();
                }
            }
        }
    }
    if let Some(raw) = db.get_setting(setting.key).ok().flatten() {
        match setting.parse(&raw) {
            Ok(value) => return value,
            Err(e) => {
                tracing::warn!(setting = setting.key, error = %e, "settings: the saved value is invalid; using the default")
            }
        }
    }
    setting.default_value()
}

/// Checks a `public_url` value: this instance's external base URL, the one
/// address plugins and customers use (clearnet or `.onion`). It must be an
/// absolute `http`/`https` URL with a host and nothing after it but an
/// optional `/` - no path, query, fragment or login - since callers append
/// paths like `/pay/{pk}/orders` to it. Returns it without the trailing
/// `/`. The empty string is not valid here; an empty setting just means
/// "not set" ([`public_url`]).
pub fn validate_public_url(value: &str) -> Result<String, String> {
    let value = value.trim();
    let problem = "Enter this instance's public address, like https://pay.example.com or http://abc...xyz.onion, with no path after it.";
    let parsed = url::Url::parse(value).map_err(|_| problem.to_string())?;
    let ok = matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str().is_some_and(|host| !host.is_empty())
        && parsed.path() == "/"
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.username().is_empty()
        && parsed.password().is_none();
    if !ok {
        return Err(problem.to_string());
    }
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

/// This instance's public base URL (no trailing `/`), or `None` while it
/// isn't set.
pub fn public_url(db: &Db) -> Option<String> {
    let value = get(db, &PUBLIC_URL);
    if value.trim().is_empty() {
        return None;
    }
    validate_public_url(&value).ok()
}

/// This instance's current signup mode.
pub fn signup_mode(db: &Db) -> SignupMode {
    get(db, &SIGNUP_MODE)
}

/// The environment variable naming the engine's address.
pub const ENGINE_URL_ENV: &str = "MONOKULO_ENGINE_URL";
/// The engine's address when [`ENGINE_URL_ENV`] isn't set: an engine on the
/// same machine with its default `server.bind`.
pub const DEFAULT_ENGINE_URL: &str = "http://127.0.0.1:8443";
/// The environment variable holding the engine's admin token (the engine's
/// `SCANNER_ADMIN_TOKEN`).
pub const ENGINE_TOKEN_ENV: &str = "MONOKULO_SCANNER_ADMIN_TOKEN";

/// How monokulo reaches the engine: its address and admin token, read from
/// the environment once at start and never from the admin page, which
/// shows both locked. The token is required, since the engine refuses
/// every request without it.
#[derive(Debug, Clone)]
pub struct EngineEnv {
    pub url: String,
    pub token: shared::auth::RawToken,
}

impl EngineEnv {
    pub fn from_env(env: &live_settings::Env) -> Result<Self, String> {
        let url = match env.get(ENGINE_URL_ENV) {
            Some(raw) => <live_settings::HttpUrl as live_settings::SettingValue>::parse(&raw)
                .map_err(|e| format!("{ENGINE_URL_ENV} is not usable: {e}"))?
                .as_str()
                .to_string(),
            None => DEFAULT_ENGINE_URL.to_string(),
        };
        let token =
            shared::auth::engine_token_from_env(ENGINE_TOKEN_ENV, env.get(ENGINE_TOKEN_ENV))?;
        Ok(EngineEnv { url, token })
    }
}

/// The engine connection's one setting: the size of the cache of its
/// responses (task 3.2).
#[derive(Debug, Clone, PartialEq)]
pub struct EngineConnection {
    pub http_cache_bytes: u64,
}

impl Section for EngineConnection {
    const NAME: &'static str = "engine connection";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&HTTP_CACHE_MAX_MB]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(EngineConnection {
            http_cache_bytes: snapshot.get(&HTTP_CACHE_MAX_MB) * 1024 * 1024,
        })
    }
}

/// Read per request with [`get`]; grouped so each setting belongs to a
/// section. Nothing holds it live.
#[derive(Debug, Clone, PartialEq)]
pub struct PerRequest {
    pub signup_mode: SignupMode,
    pub public_url: String,
}

/// Read once at start: how many read connections the database opens
/// (`db::Database`).
#[derive(Debug, Clone, PartialEq)]
pub struct DatabaseConfig {
    pub read_connections: usize,
}

impl Section for DatabaseConfig {
    const NAME: &'static str = "database";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&DATABASE_READ_CONNECTIONS]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(DatabaseConfig {
            read_connections: snapshot.get(&DATABASE_READ_CONNECTIONS),
        })
    }
}

impl Section for PerRequest {
    const NAME: &'static str = "per request";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&SIGNUP_MODE, &PUBLIC_URL]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(PerRequest {
            signup_mode: snapshot.get(&SIGNUP_MODE),
            public_url: snapshot.get(&PUBLIC_URL),
        })
    }
}

impl Section for ExchangeRateConfig {
    const NAME: &'static str = "exchange rates";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &EXCHANGE_RATE_COINGECKO_ENABLED,
            &EXCHANGE_RATE_COINGECKO_BASE_URL,
            &EXCHANGE_RATE_COINMARKETCAP_ENABLED,
            &EXCHANGE_RATE_COINMARKETCAP_BASE_URL,
            &EXCHANGE_RATE_HAVENO_ENABLED,
            &EXCHANGE_RATE_HAVENO_BASE_URL,
            &EXCHANGE_RATE_CACHE_SECONDS,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(ExchangeRateConfig {
            coingecko_enabled: snapshot.get(&EXCHANGE_RATE_COINGECKO_ENABLED),
            coingecko_base_url: snapshot
                .get(&EXCHANGE_RATE_COINGECKO_BASE_URL)
                .as_str()
                .to_string(),
            coinmarketcap_enabled: snapshot.get(&EXCHANGE_RATE_COINMARKETCAP_ENABLED),
            coinmarketcap_base_url: snapshot
                .get(&EXCHANGE_RATE_COINMARKETCAP_BASE_URL)
                .as_str()
                .to_string(),
            haveno_enabled: snapshot.get(&EXCHANGE_RATE_HAVENO_ENABLED),
            haveno_base_url: snapshot
                .get(&EXCHANGE_RATE_HAVENO_BASE_URL)
                .as_str()
                .to_string(),
            cache_seconds: snapshot.get(&EXCHANGE_RATE_CACHE_SECONDS),
        })
    }
}

impl Section for AbuseConfig {
    const NAME: &'static str = "abuse protection";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &ABUSE_TRUSTED_PROXIES,
            &ABUSE_STREAM_CAP,
            &ABUSE_SOFT_PER_MIN,
            &ABUSE_HARD_PER_MIN,
            &ABUSE_SIGNED_IN_PER_MIN,
            &ABUSE_CLIENT_LOGS_PER_MIN,
            &ABUSE_CHALLENGE_BITS,
            &ABUSE_UNDER_ATTACK,
            &RATE_LIMIT_PER_STORE_KEY_PER_MIN,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        let soft = snapshot.get(&ABUSE_SOFT_PER_MIN);
        let hard = snapshot.get(&ABUSE_HARD_PER_MIN);
        if hard <= soft {
            return Err(vec![FieldError::new(
                ABUSE_HARD_PER_MIN.key,
                format!("abuse.hard_per_min ({hard}) must be above abuse.soft_per_min ({soft})."),
            )]);
        }
        Ok(AbuseConfig {
            trusted_proxies: TrustedProxies::parse(&snapshot.get(&ABUSE_TRUSTED_PROXIES))
                .unwrap_or_default(),
            soft_per_min: soft,
            hard_per_min: hard,
            signed_in_per_min: snapshot.get(&ABUSE_SIGNED_IN_PER_MIN),
            client_logs_per_min: snapshot.get(&ABUSE_CLIENT_LOGS_PER_MIN),
            per_store_key_per_min: snapshot.get(&RATE_LIMIT_PER_STORE_KEY_PER_MIN),
            stream_cap: snapshot.get(&ABUSE_STREAM_CAP),
            challenge_bits: snapshot.get(&ABUSE_CHALLENGE_BITS),
            under_attack: snapshot.get(&ABUSE_UNDER_ATTACK),
        })
    }
}

/// The onion listener's address (task 3.4); `None` when off.
#[derive(Debug, Clone, PartialEq)]
pub struct OnionListenerConfig {
    pub address: Option<SocketAddr>,
}

impl Section for OnionListenerConfig {
    const NAME: &'static str = "onion listener";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&ABUSE_ONION_LISTENER]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        crate::abuse::proxy_protocol::validate_onion_listener(&snapshot.get(&ABUSE_ONION_LISTENER))
            .map(|address| OnionListenerConfig { address })
            .map_err(|e| vec![FieldError::new(ABUSE_ONION_LISTENER.key, e)])
    }
}

/// monokulo's settings store, over its own `settings` table, through the
/// same reading and writing connections as everything else (so a save
/// queues behind other writes on the writer's thread, never on a Tokio
/// worker).
pub struct DbSettings(pub Database);

#[live_settings::async_trait]
impl live_settings::SettingsStore for DbSettings {
    async fn read_all(
        &self,
    ) -> Result<std::collections::HashMap<String, String>, live_settings::StoreError> {
        self.0
            .read(|db| db.list_settings())
            .await
            .map_err(live_settings::StoreError::new)
    }

    async fn write_all(
        &self,
        changes: Vec<(&'static str, Option<String>)>,
    ) -> Result<(), live_settings::StoreError> {
        self.0
            .write(move |db| db.write_settings(&changes))
            .await
            .map_err(live_settings::StoreError::new)
    }
}

/// Gives the engine client a saved cache size (task 3.2).
pub struct EngineConnectionReloadable {
    pub engine_client: EngineClient,
}

#[live_settings::async_trait]
impl live_settings::Reloadable for EngineConnectionReloadable {
    type Config = EngineConnection;
    type Prepared = EngineConnection;

    async fn prepare(
        &self,
        new: &EngineConnection,
        _old: &EngineConnection,
    ) -> Result<(EngineConnection, Vec<Warning>), FieldError> {
        Ok((new.clone(), Vec::new()))
    }

    async fn install(&self, connection: EngineConnection) {
        self.engine_client
            .set_cache_limit(connection.http_cache_bytes);
    }

    fn boot_policy(&self) -> live_settings::BootPolicy {
        live_settings::BootPolicy::StartDegraded
    }
}

/// Applies saved exchange-rate settings (task 3.3).
pub struct ExchangeRatesReloadable {
    pub providers: Arc<ExchangeRateProviders>,
}

#[live_settings::async_trait]
impl live_settings::Reloadable for ExchangeRatesReloadable {
    type Config = ExchangeRateConfig;
    type Prepared = ExchangeRateConfig;

    async fn prepare(
        &self,
        new: &ExchangeRateConfig,
        _old: &ExchangeRateConfig,
    ) -> Result<(ExchangeRateConfig, Vec<Warning>), FieldError> {
        Ok((new.clone(), Vec::new()))
    }

    async fn install(&self, config: ExchangeRateConfig) {
        self.providers.reconfigure(&config);
    }

    fn boot_policy(&self) -> live_settings::BootPolicy {
        live_settings::BootPolicy::StartDegraded
    }
}

/// Applies saved abuse-protection settings.
pub struct AbuseReloadable {
    pub abuse: Arc<AbuseProtection>,
}

#[live_settings::async_trait]
impl live_settings::Reloadable for AbuseReloadable {
    type Config = AbuseConfig;
    type Prepared = AbuseConfig;

    async fn prepare(
        &self,
        new: &AbuseConfig,
        _old: &AbuseConfig,
    ) -> Result<(AbuseConfig, Vec<Warning>), FieldError> {
        Ok((new.clone(), Vec::new()))
    }

    async fn install(&self, config: AbuseConfig) {
        self.abuse.reload(config);
    }

    fn boot_policy(&self) -> live_settings::BootPolicy {
        live_settings::BootPolicy::StartDegraded
    }
}

/// Starts, moves or stops the onion listener when its address is saved
/// (task 3.4). A new address is bound before anything is stored, so one
/// that can't be bound refuses the save. The old listener stops accepting
/// at once; its open connections finish on their own. At start-up the
/// router doesn't exist yet when settings are applied, so a listener bound
/// then waits until `router_ready`.
#[derive(Clone, Default)]
pub struct OnionReloadable {
    inner: Arc<OnionInner>,
}

#[derive(Default)]
struct OnionInner {
    router: OnceLock<axum::Router>,
    running: parking_lot::Mutex<Option<tokio::sync::watch::Sender<bool>>>,
    waiting: parking_lot::Mutex<Option<crate::abuse::proxy_protocol::OnionListener>>,
}

impl OnionReloadable {
    /// Gives the listener the router to serve, starting one that was
    /// waiting for it.
    pub fn router_ready(&self, router: axum::Router) {
        let _ = self.inner.router.set(router);
        let waiting = self.inner.waiting.lock().take();
        if let Some(listener) = waiting {
            self.start(listener);
        }
    }

    fn start(&self, listener: crate::abuse::proxy_protocol::OnionListener) {
        let Some(router) = self.inner.router.get().cloned() else {
            *self.inner.waiting.lock() = Some(listener);
            return;
        };
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let address = listener.bound_address();
        tokio::spawn(async move {
            let serve = axum::serve(
                listener,
                router
                    .into_make_service_with_connect_info::<crate::abuse::proxy_protocol::OnionPeer>(
                    ),
            )
            .with_graceful_shutdown(async move {
                let _ = stopped.wait_for(|s| *s).await;
            });
            if let Err(e) = serve.await {
                tracing::error!(server.address = %address, error = %e, "onion listener stopped");
            }
        });
        tracing::info!(server.address = %address, "onion listener (PROXY protocol, for tor) started");
        *self.inner.running.lock() = Some(stop);
    }
}

/// What saving the onion listener's address does.
pub enum OnionChange {
    /// Same address as before: leave the listener as it is.
    Keep,
    /// Cleared: stop it.
    Off,
    /// A new address, already bound.
    Start(crate::abuse::proxy_protocol::OnionListener),
}

#[live_settings::async_trait]
impl live_settings::Reloadable for OnionReloadable {
    type Config = OnionListenerConfig;
    type Prepared = OnionChange;

    async fn prepare(
        &self,
        new: &OnionListenerConfig,
        old: &OnionListenerConfig,
    ) -> Result<(OnionChange, Vec<Warning>), FieldError> {
        let active = self.inner.running.lock().is_some() || self.inner.waiting.lock().is_some();
        if new == old && (active || new.address.is_none()) {
            return Ok((OnionChange::Keep, Vec::new()));
        }
        match new.address {
            None => Ok((OnionChange::Off, Vec::new())),
            Some(address) => {
                // Moving back to an address this listener just left: the old
                // one lets go of it moments after it's stopped.
                let mut attempts = 0;
                loop {
                    match crate::abuse::proxy_protocol::OnionListener::bind(address).await {
                        Ok(listener) => return Ok((OnionChange::Start(listener), Vec::new())),
                        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && attempts < 20 => {
                            attempts += 1;
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        Err(e) => {
                            return Err(FieldError::new(
                                ABUSE_ONION_LISTENER.key,
                                format!("Can't listen on {address}: {e}."),
                            ))
                        }
                    }
                }
            }
        }
    }

    async fn install(&self, change: OnionChange) {
        match change {
            OnionChange::Keep => {}
            OnionChange::Off => self.stop(),
            OnionChange::Start(listener) => {
                self.stop();
                self.start(listener);
            }
        }
    }

    fn boot_policy(&self) -> live_settings::BootPolicy {
        live_settings::BootPolicy::Exit
    }
}

impl OnionReloadable {
    fn stop(&self) {
        if let Some(stop) = self.inner.running.lock().take() {
            let _ = stop.send(true);
        }
        self.inner.waiting.lock().take();
    }
}

/// monokulo's settings: the registry that saves and describes them, and the
/// live sections the process reads.
pub struct MonokuloSettings {
    pub registry: Option<Registry>,
}

impl MonokuloSettings {
    /// No registry, for tests that don't use the admin settings page.
    pub fn defaults() -> Arc<Self> {
        Arc::new(MonokuloSettings { registry: None })
    }

    /// Loads every setting and applies it to the given runtime pieces;
    /// later saves through the registry apply the same way.
    pub async fn load(
        db: Database,
        engine_client: EngineClient,
        exchange_rates: Arc<ExchangeRateProviders>,
        abuse: Arc<AbuseProtection>,
        onion: Option<OnionReloadable>,
        env: live_settings::Env,
    ) -> Result<Arc<Self>, String> {
        let mut builder = Registry::builder_with_env(Arc::new(DbSettings(db)), ALL, env).await;
        builder.reloadable(EngineConnectionReloadable { engine_client });
        builder.reloadable(ExchangeRatesReloadable {
            providers: exchange_rates,
        });
        builder.reloadable(AbuseReloadable { abuse });
        match onion {
            Some(onion) => {
                builder.reloadable(onion);
            }
            None => {
                builder.section::<OnionListenerConfig>();
            }
        }
        // Its settings are read per request with `get` (they have no
        // runtime state to rebuild); the section only groups them.
        builder.section::<PerRequest>();
        // Read once at start, before the registry exists (`main.rs`).
        builder.section::<DatabaseConfig>();
        builder.reloadable(telemetry::LogReloadable::<LoggingConfig>::default());
        let registry = builder.build().map_err(|e| e.to_string())?;
        let report = registry.boot().await.map_err(|e| e.to_string())?;
        for warning in &report.warnings {
            tracing::warn!(
                setting = warning.key.as_deref(),
                "settings: {}",
                warning.message
            );
        }
        for (section, error) in &report.degraded {
            tracing::warn!(section = %section, error = %error, "settings: could not be applied at start, carrying on without it");
        }
        Ok(Arc::new(MonokuloSettings {
            registry: Some(registry),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_public_url_must_be_an_http_base_url_with_nothing_after_it() {
        for (input, expected) in [
            ("https://pay.example.com", "https://pay.example.com"),
            ("https://pay.example.com/", "https://pay.example.com"),
            (
                "  http://pay.example.com:8081/ ",
                "http://pay.example.com:8081",
            ),
            (
                "http://abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz234.onion",
                "http://abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz234.onion",
            ),
        ] {
            assert_eq!(
                validate_public_url(input).as_deref(),
                Ok(expected),
                "{input}"
            );
        }
        for bad in [
            "",
            "pay.example.com",
            "ftp://pay.example.com",
            "https://pay.example.com/monokulo",
            "https://pay.example.com/?a=b",
            "https://pay.example.com/#top",
            "https://user:pass@pay.example.com",
            "https://",
        ] {
            assert!(
                validate_public_url(bad).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn public_url_is_none_until_set_and_then_normalized() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(public_url(&db), None);
        db.set_setting(PUBLIC_URL.key, "https://pay.example.com/")
            .unwrap();
        assert_eq!(public_url(&db).as_deref(), Some("https://pay.example.com"));
        db.set_setting(PUBLIC_URL.key, "not a url").unwrap();
        assert_eq!(public_url(&db), None);
    }

    #[test]
    fn a_saved_setting_is_read_back_over_the_default_and_an_env_var_wins() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(get(&db, &EXCHANGE_RATE_CACHE_SECONDS), 30);
        db.set_setting(EXCHANGE_RATE_CACHE_SECONDS.key, "3")
            .unwrap();
        assert_eq!(get(&db, &EXCHANGE_RATE_CACHE_SECONDS), 3);
        let _env = shared::settings::test_env::set(EXCHANGE_RATE_CACHE_SECONDS.env_var, Some("9"));
        assert_eq!(get(&db, &EXCHANGE_RATE_CACHE_SECONDS), 9);
    }

    /// An environment value that doesn't parse gives the default, as the
    /// registry (and so the admin page) resolves it, not the saved value.
    #[test]
    fn an_invalid_env_var_gives_the_default_as_the_registry_does() {
        let db = Db::open_in_memory().unwrap();
        db.set_setting(EXCHANGE_RATE_CACHE_SECONDS.key, "3")
            .unwrap();
        let _env =
            shared::settings::test_env::set(EXCHANGE_RATE_CACHE_SECONDS.env_var, Some("lots"));
        assert_eq!(get(&db, &EXCHANGE_RATE_CACHE_SECONDS), 30);
        let resolved = live_settings::read_sync_with_env::<ExchangeRateConfig>(
            db.list_settings().map_err(live_settings::StoreError::new),
            &live_settings::Env::fixed([(EXCHANGE_RATE_CACHE_SECONDS.env_var, "lots")]),
        );
        assert_eq!(resolved.cache_seconds, 30);
    }

    /// With nothing saved and nothing in the environment: Coingecko and
    /// CoinMarketCap on at their public, keyless addresses, so a fresh
    /// instance can price fiat orders; Haveno off, since it prices from a
    /// thin order book and an operator opts in; rates reused for half a
    /// minute.
    #[test]
    fn exchange_rates_default_to_the_keyless_public_providers() {
        let config = ExchangeRateConfig::from_snapshot(&Snapshot::defaults())
            .expect("the defaults are valid");
        assert_eq!(
            config,
            ExchangeRateConfig {
                coingecko_enabled: true,
                coingecko_base_url: "https://api.coingecko.com".to_string(),
                coinmarketcap_enabled: true,
                coinmarketcap_base_url: "https://pro-api.coinmarketcap.com/public-api".to_string(),
                haveno_enabled: false,
                haveno_base_url: "https://haveno.markets".to_string(),
                cache_seconds: 30,
            }
        );
    }

    #[test]
    fn the_engine_connection_comes_from_the_environment_and_needs_a_token() {
        let env = |pairs: &[(&str, &str)]| {
            live_settings::Env::fixed(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<Vec<_>>(),
            )
        };
        let token = "t".repeat(shared::auth::MIN_ENGINE_TOKEN_LEN);

        let defaults = EngineEnv::from_env(&env(&[(ENGINE_TOKEN_ENV, &token)])).unwrap();
        assert_eq!(defaults.url, DEFAULT_ENGINE_URL);
        assert_eq!(defaults.token.expose(), token);

        let set = EngineEnv::from_env(&env(&[
            (ENGINE_URL_ENV, "http://engine:8443/"),
            (ENGINE_TOKEN_ENV, &token),
        ]))
        .unwrap();
        assert_eq!(
            set.url, "http://engine:8443",
            "no trailing slash to double up"
        );

        let missing = EngineEnv::from_env(&env(&[])).unwrap_err();
        assert!(
            missing.starts_with("MONOKULO_SCANNER_ADMIN_TOKEN is not set"),
            "{missing}"
        );
        let short = EngineEnv::from_env(&env(&[(ENGINE_TOKEN_ENV, "short")])).unwrap_err();
        assert!(short.contains("shorter than"), "{short}");
        let bad_url = EngineEnv::from_env(&env(&[
            (ENGINE_URL_ENV, "engine:8443"),
            (ENGINE_TOKEN_ENV, &token),
        ]))
        .unwrap_err();
        assert!(
            bad_url.starts_with("MONOKULO_ENGINE_URL is not usable"),
            "{bad_url}"
        );
    }

    #[test]
    fn every_section_builds_from_the_defaults_and_every_setting_is_in_one() {
        let snapshot = Snapshot::defaults();
        assert!(EngineConnection::from_snapshot(&snapshot).is_ok());
        assert!(PerRequest::from_snapshot(&snapshot).is_ok());
        assert!(ExchangeRateConfig::from_snapshot(&snapshot).is_ok());
        assert!(AbuseConfig::from_snapshot(&snapshot).is_ok());
        assert!(OnionListenerConfig::from_snapshot(&snapshot).is_ok());
        assert!(LoggingConfig::from_snapshot(&snapshot).is_ok());
        assert!(DatabaseConfig::from_snapshot(&snapshot).is_ok());
        let covered: usize = [
            EngineConnection::keys().len(),
            PerRequest::keys().len(),
            ExchangeRateConfig::keys().len(),
            AbuseConfig::keys().len(),
            OnionListenerConfig::keys().len(),
            LoggingConfig::keys().len(),
            DatabaseConfig::keys().len(),
        ]
        .iter()
        .sum();
        assert_eq!(covered, ALL.len());
    }

    async fn loaded(
        onion: Option<OnionReloadable>,
    ) -> (
        Arc<MonokuloSettings>,
        EngineClient,
        Arc<ExchangeRateProviders>,
        Arc<AbuseProtection>,
    ) {
        let db = Database::inline(Db::open_in_memory().unwrap().into_shared());
        let engine = EngineClient::for_tests("http://127.0.0.1:1");
        let rates = Arc::new(ExchangeRateProviders::xmr_only());
        let abuse: Arc<AbuseProtection> = Default::default();
        let settings = MonokuloSettings::load(
            db,
            engine.clone(),
            rates.clone(),
            abuse.clone(),
            onion,
            live_settings::Env::fixed(Vec::<(String, String)>::new()),
        )
        .await
        .unwrap();
        (settings, engine, rates, abuse)
    }

    fn change(key: &str, value: &str) -> live_settings::Changes {
        vec![(key.to_string(), Some(value.to_string()))]
    }

    #[tokio::test]
    async fn saved_settings_reach_the_engine_client_exchange_rates_and_abuse_protection() {
        let (settings, engine, rates, abuse) = loaded(None).await;
        let registry = settings.registry.as_ref().unwrap();
        // Loading applied the saved (here: default) settings.
        assert_eq!(
            rates.available_providers(),
            vec!["coingecko", "coinmarketcap"],
            "haveno is off until an admin turns it on"
        );
        registry
            .save(change("exchange_rate.haveno_enabled", "true"))
            .await
            .unwrap();
        assert_eq!(
            rates.available_providers(),
            vec!["coingecko", "coinmarketcap", "haveno"]
        );

        assert!(
            registry
                .save(change("engine.url", "http://127.0.0.1:2"))
                .await
                .is_err(),
            "the engine's address comes from the environment, not a save"
        );
        assert_eq!(engine.base_url(), "http://127.0.0.1:1");

        registry
            .save(change("exchange_rate.coingecko_enabled", "false"))
            .await
            .unwrap();
        assert_eq!(rates.available_providers(), vec!["coinmarketcap", "haveno"]);
        registry
            .save(change("exchange_rate.haveno_enabled", "false"))
            .await
            .unwrap();
        registry
            .save(change("exchange_rate.coinmarketcap_enabled", "false"))
            .await
            .unwrap();
        assert!(rates.available_providers().is_empty());

        registry
            .save(change("abuse.soft_per_min", "7"))
            .await
            .unwrap();
        assert_eq!(abuse.config().soft_per_min, 7);

        let refused = registry.save(change("abuse.hard_per_min", "5")).await;
        assert!(refused.is_err(), "hard must stay above soft");
        assert_eq!(abuse.config().hard_per_min, 300, "nothing applied");
    }

    #[tokio::test]
    async fn the_onion_listener_starts_moves_and_stops_when_saved() {
        let onion = OnionReloadable::default();
        let (settings, ..) = loaded(Some(onion.clone())).await;
        onion.router_ready(axum::Router::new().route("/", axum::routing::get(|| async { "ok" })));
        let registry = settings.registry.as_ref().unwrap();

        let free = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        registry
            .save(change("abuse.onion_listener", &free.to_string()))
            .await
            .unwrap();
        // The save binds before it returns (`prepare`): a connection is
        // queued by the kernel whether or not it has been accepted yet.
        assert!(
            tokio::net::TcpStream::connect(free).await.is_ok(),
            "listening straight away"
        );

        // Moved away and straight back: the first address is free again at
        // once, with no connection needed to let go of it.
        let other = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        registry
            .save(change("abuse.onion_listener", &other.to_string()))
            .await
            .unwrap();
        registry
            .save(change("abuse.onion_listener", &free.to_string()))
            .await
            .expect("moving back to the first address");
        assert!(
            tokio::net::TcpStream::connect(free).await.is_ok(),
            "listening on the first address again"
        );

        registry
            .save(change("abuse.onion_listener", ""))
            .await
            .unwrap();
        // The old listener goes as its task is stopped: waited for, with a
        // deadline that bounds only a hung stop.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut closed = false;
        while std::time::Instant::now() < deadline {
            if tokio::net::TcpStream::connect(free).await.is_err() {
                closed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(closed, "cleared: no longer listening");

        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let refused = registry
            .save(change(
                "abuse.onion_listener",
                &taken.local_addr().unwrap().to_string(),
            ))
            .await;
        assert!(
            refused.is_err(),
            "an address that can't be bound refuses the save"
        );
        assert_eq!(
            registry
                .describe()
                .iter()
                .find(|v| v.key == "abuse.onion_listener")
                .unwrap()
                .value,
            ""
        );
    }
}
