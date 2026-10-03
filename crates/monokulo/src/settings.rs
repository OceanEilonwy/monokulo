//! Every monokulo setting, declared once with the `live-settings` library
//! (admin_settings_v2.md part 1): each setting's type, range, description
//! and example live in its declaration. Configuration comes from the options
//! file (`monokulo.toml`), which the admin settings page
//! (`http/admin_settings.rs`) saves to, or from its command-line option,
//! which wins and locks it on the page. Saving or reloading through the
//! registry applies the change to the running process (part 3).
//!
//! Two runtime switches, `abuse.under_attack` and `logging.dev_mode_until`,
//! are kept in the database instead: they are flipped from the admin page
//! while running, not configured. Secrets come from the environment only
//! (the process list and the options file can be read by others): the
//! engine token, the collector's headers, and `crypto.encryption_key`, the
//! AES-256-GCM key every `store_connections.tenant_secret_token_encrypted`
//! row is encrypted with (`crate::crypto`). Keeping it in the database it
//! protects, or changing it while running, would leak it or make every
//! stored token unreadable.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use live_settings::{
    choice_value, settings, AnySetting, BindAddr, FieldError, HttpUrl, Registry, Secret, Section,
    Snapshot, Warning,
};

use crate::abuse::{AbuseConfig, AbuseProtection, TrustedProxies};
use crate::db::Database;
use crate::engine_client::EngineClient;
use crate::exchange_rate_config::{ExchangeRateConfig, ExchangeRateProviders};

choice_value! {
    /// Who may sign up.
    pub enum SignupMode { Public = "public", InviteOnly = "invite_only" }
}

choice_value! {
    /// Where the engine runs (docs/engine_as_library.md).
    pub enum EngineMode { Embedded = "embedded", Remote = "remote" }
}

/// The table of monokulo's options file that holds an embedded engine's
/// settings: `[engine.payment]` holds its `payment.…`.
pub const ENGINE_TABLE: &str = "engine";

/// Why `[engine.*]` tables are refused with a remote engine.
pub const ENGINE_TABLE_HINT: &str = "the engine's own settings go here only when it runs inside monokulo (engine.mode = \"embedded\"); a remote engine keeps them in its own options file";

/// The engine's mode from the settings read at start, checked against the
/// settings that only make sense for the other mode, so none is set and
/// silently ignored: a remote engine needs its URL's token, and an embedded
/// one takes neither its URL nor a token (monokulo makes its own).
pub fn engine_mode(early: &Snapshot, env: &live_settings::Env) -> Result<EngineMode, String> {
    let mode = early.get(&ENGINE_MODE);
    match mode {
        EngineMode::Remote => match env.get(ENGINE_TOKEN.env_var) {
            None => {
                return Err(format!(
                    "{} must be set: engine.mode is remote, and the engine refuses any request without its token (its ENGINE_TOKEN).",
                    ENGINE_TOKEN.env_var
                ))
            }
            Some(token) => shared::auth::check_engine_token(&token)
                .map_err(|e| format!("{}: {e}", ENGINE_TOKEN.env_var))?,
        },
        EngineMode::Embedded => {
            let mut given = Vec::new();
            if early.source(&ENGINE_URL) != live_settings::SettingSource::Default {
                given.push("engine.url".to_string());
            }
            if env.get(ENGINE_TOKEN.env_var).is_some() {
                given.push(ENGINE_TOKEN.env_var.to_string());
            }
            if !given.is_empty() {
                return Err(format!(
                    "{} only apply to a remote engine, but the engine runs inside monokulo (engine.mode is embedded). Remove them, or set engine.mode = \"remote\".",
                    given.join(" and ")
                ));
            }
        }
    }
    Ok(mode)
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

/// 64 hex characters: the 32 bytes of an AES-256-GCM key.
fn check_encryption_key(value: &Secret) -> Result<(), String> {
    encryption_key_bytes(value.expose()).map(|_| ())
}

/// The 32 bytes `value` (64 hex characters) spells.
pub fn encryption_key_bytes(value: &str) -> Result<[u8; 32], String> {
    let problem = "Enter 64 hex characters (32 bytes); generate them with `openssl rand -hex 32`.";
    let bytes = hex::decode(value.trim()).map_err(|_| problem.to_string())?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| problem.to_string())
}

settings! {
    DATABASE_PATH: PathBuf {
        key: "database.path",
        default: live_settings::paths::data_file("monokulo.db").unwrap_or_else(|| PathBuf::from("monokulo.db")),
        description: "monokulo's database file, by default ~/.local/share/monokulo/monokulo.db (or monokulo.db in the working directory if that can't be used). Its log store is kept beside it.",
        example: "/var/lib/monokulo/monokulo.db",
        applies: Restart,
        editable: false,
    },
    SERVER_BIND: BindAddr {
        key: "server.bind",
        default: live_settings::parsed_default("127.0.0.1:8081"),
        description: "The address and port monokulo listens on.",
        example: "127.0.0.1:8081",
        applies: Restart,
    },
    CRYPTO_ENCRYPTION_KEY: Secret {
        key: "crypto.encryption_key",
        env: "MONOKULO_ENCRYPTION_KEY",
        default: Secret::default(),
        check: check_encryption_key,
        description: "The key the engine's secret token for each store is encrypted with in monokulo's database: 64 hex characters, from `openssl rand -hex 32`. Required, given at start only, and never changed once stores are connected: their tokens could no longer be read.",
        applies: Restart,
        sources: [Env],
        required: true,
    },
    ENGINE_MODE: EngineMode {
        key: "engine.mode",
        default: EngineMode::Embedded,
        description: "Where the engine runs. embedded: inside monokulo, one process, its settings in this file's [engine.*] tables. remote: a separate monokulo-engine at engine.url, reached with the engine token (MONOKULO_ENGINE_TOKEN).",
        example: "embedded",
        applies: Restart,
    },
    ENGINE_URL: HttpUrl {
        key: "engine.url",
        default: live_settings::parsed_default("http://127.0.0.1:8443"),
        description: "With engine.mode = remote: where monokulo reaches the engine, its server.bind as a URL. Refused when the engine is embedded.",
        example: "http://127.0.0.1:8443",
        applies: Restart,
    },
    ENGINE_TOKEN: Secret {
        key: "engine.token",
        env: "MONOKULO_ENGINE_TOKEN",
        default: Secret::default(),
        // Unset is valid: only a remote engine needs it (`engine_mode`).
        check: |token: &Secret| if token.expose().is_empty() { Ok(()) } else { shared::auth::check_engine_token(token.expose()) },
        description: "With engine.mode = remote: the engine token, sent with every request to the engine, which refuses anything without it (the engine's ENGINE_TOKEN). At least 32 characters, given at start only. Required for a remote engine, refused for an embedded one, which monokulo gives a token of its own each time it starts.",
        applies: Restart,
        sources: [Env],
    },
    LOGGING_FORMAT: telemetry::LogFormat {
        key: "logging.format",
        default: telemetry::LogFormat::Auto,
        description: "How log lines are written to the console: json, pretty, or auto (pretty at a terminal, JSON everywhere else).",
        example: "json",
        applies: Restart,
    },
    SIGNUP_MODE: SignupMode {
        key: "signup.mode",
        default: SignupMode::InviteOnly,
        description: "Who can create an account: public (anyone) or invite_only (only people with an invite link from the admin).",
        example: "invite_only",
    },
    EXCHANGE_RATE_COINGECKO_ENABLED: bool {
        key: "exchange_rate.coingecko_enabled",
        default: true,
        description: "Whether stores can price orders in fiat currencies using Coingecko's rates. Off, only XMR prices work.",
        example: "true",
    },
    EXCHANGE_RATE_COINGECKO_BASE_URL: HttpUrl {
        key: "exchange_rate.coingecko_base_url",
        default: live_settings::parsed_default("https://api.coingecko.com"),
        description: "Where Coingecko's API is reached. Change it only to use a proxy or mirror.",
        example: "https://api.coingecko.com",
    },
    EXCHANGE_RATE_COINMARKETCAP_ENABLED: bool {
        key: "exchange_rate.coinmarketcap_enabled",
        default: true,
        description: "Whether stores can price orders in fiat currencies using CoinMarketCap's rates. Each store still chooses whether to use it, and in what order.",
        example: "true",
    },
    EXCHANGE_RATE_COINMARKETCAP_BASE_URL: HttpUrl {
        key: "exchange_rate.coinmarketcap_base_url",
        default: live_settings::parsed_default("https://pro-api.coinmarketcap.com/public-api"),
        description: "Where CoinMarketCap's keyless API is reached. Change it only to use a proxy or mirror.",
        example: "https://pro-api.coinmarketcap.com/public-api",
    },
    EXCHANGE_RATE_HAVENO_ENABLED: bool {
        key: "exchange_rate.haveno_enabled",
        default: false,
        description: "Whether stores can price orders using the RetoSwap (Haveno) order book, read through haveno.markets. It is a thin peer-to-peer market, so it prices only currencies with both buyers and sellers listed right now.",
        example: "false",
    },
    EXCHANGE_RATE_HAVENO_BASE_URL: HttpUrl {
        key: "exchange_rate.haveno_base_url",
        default: live_settings::parsed_default("https://haveno.markets"),
        description: "Where the haveno.markets API is reached. Change it only to use a proxy or mirror.",
        example: "https://haveno.markets",
    },
    EXCHANGE_RATE_CACHE_SECONDS: u64 {
        key: "exchange_rate.cache_seconds",
        default: 30,
        check: range(0, 86_400),
        description: "Seconds a fetched exchange rate is reused before asking its provider again.",
        example: "30",
    },
    HTTP_CACHE_MAX_MB: u64 {
        key: "http_cache.max_mb",
        default: 16,
        check: range(1, 4096),
        description: "Megabytes of memory for monokulo's cache of engine responses.",
        example: "16",
    },
    DATABASE_READ_CONNECTIONS: usize {
        key: "database.read_connections",
        default: shared::sqlite::DEFAULT_READ_CONNECTIONS,
        check: range(1, 64),
        description: "Read-only connections monokulo opens to its database, each on its own thread. Reads run side by side, so more help up to the number of CPU cores; each keeps its own cache of about 2 MB.",
        example: "4",
        applies: Restart,
    },
    RATE_LIMIT_PER_STORE_KEY_PER_MIN: u32 {
        key: "rate_limit.per_store_key_per_min",
        default: 600,
        check: range(1, 10_000_000),
        description: "Requests a minute a shop's server may make with its store's secret key (for example the WooCommerce plugin creating orders). These are never challenged.",
        example: "600",
    },
    KEY_CUSTODY_CLI_DOWNLOAD_URL: String {
        key: "key_custody.cli_download_url",
        default: concat!(env!("CARGO_PKG_REPOSITORY"), "/releases/download/v{version}/{file}").to_owned(),
        check: |v: &String| check_cli_url(v, "{file}"),
        description: "Where merchants download key-custody-cli, the tool that encrypts their keys for an SEV-SNP engine without a browser. {version} is this monokulo's version and {file} the release file for their computer (key-custody-cli-{version}-{target}.tar.gz, or .zip for Windows). Change it only if you publish your own builds.",
        example: "https://github.com/OceanEilonwy/monokulo/releases/download/v{version}/{file}",
        applies: Restart,
        editable: false,
    },
    KEY_CUSTODY_CLI_SOURCE_URL: String {
        key: "key_custody.cli_source_url",
        default: concat!(env!("CARGO_PKG_REPOSITORY"), "/tree/{ref}/crates/key-custody-cli").to_owned(),
        check: |v: &String| check_cli_url(v, ""),
        description: "Where merchants read key-custody-cli's source. {ref} is this build's release tag (v{version}), or its commit for a build that isn't a release.",
        example: "https://github.com/OceanEilonwy/monokulo/tree/{ref}/crates/key-custody-cli",
        applies: Restart,
        editable: false,
    },
    KEY_CUSTODY_SNP_ENTRY_ID_KEY: Option<String> {
        key: "key_custody.snp_entry_id_key",
        default: None,
        check: check_id_key_digest,
        description: "The SHA-384 digest (96 hex characters) of the ID key an engine image must be signed with before this site's key entry forms encrypt merchants' keys to it. Leave empty for the official monokulo releases. Set here, not taken from the engine, so whoever runs the engine's machine can't loosen it; the status page shows an alert when it differs from the engine's key_custody.snp_trusted_id_key.",
        example: "",
    },
    KEY_CUSTODY_SNP_ENTRY_MIN_GUEST_SVN: u32 {
        key: "key_custody.snp_entry_min_guest_svn",
        default: 0,
        description: "The lowest engine image security version (the ID block's guest SVN) this site's key entry forms encrypt keys to. Keep it equal to the engine's key_custody.snp_min_guest_svn; the status page shows an alert when they differ.",
        example: "1",
    },
    KEY_CUSTODY_SNP_ENTRY_MIN_TCB: Option<String> {
        key: "key_custody.snp_entry_min_tcb",
        default: None,
        check: check_tcb_floor,
        description: "The lowest firmware this site's key entry forms encrypt keys to, as the security patch levels bootloader,tee,snp,microcode of the attested TCB; empty checks only this release's own floor, which always applies. Keep it equal to the engine's key_custody.snp_min_tcb; the status page shows an alert when they differ.",
        example: "10,0,23,213",
    },
    KEY_CUSTODY_SNP_ENTRY_REQUIRED: bool {
        key: "key_custody.snp_entry_required",
        default: false,
        description: "Whether every store's keys must go to the engine's SEV-SNP backend, encrypted in the merchant's browser or with key-custody-cli. On, this site never shows a form for keys in the clear and never sends typed keys to the engine, even if the engine says it has no SEV-SNP backend: key entry is then unavailable, and the status page shows an alert.",
        example: "true",
    },
    PUBLIC_URL: String {
        key: "public_url",
        default: String::new(),
        check: |v: &String| check_public_url(v),
        description: "This instance's public address, e.g. https://pay.example.com or an http://....onion address. Plugins such as WooCommerce are given it when they connect, and send customers to its checkout. Plugins can't connect until it is set.",
        example: "https://pay.example.com",
    },
    ABUSE_TRUSTED_PROXIES: String {
        key: "abuse.trusted_proxies",
        default: String::new(),
        check: |v: &String| check_trusted_proxies(v),
        description: "Addresses and CIDR ranges of reverse proxies in front of this instance, comma-separated. A request from one of these is identified by the last address in its X-Forwarded-For header that isn't a trusted proxy. Leave empty if clients connect directly.",
        example: "127.0.0.1, 10.0.0.0/8",
    },
    ABUSE_ONION_LISTENER: String {
        key: "abuse.onion_listener",
        default: String::new(),
        check: |v: &String| check_onion_listener(v),
        description: "A loopback address:port for tor's onion service to connect to, with HiddenServiceExportCircuitID haproxy set in torrc, so each Tor circuit is its own client. Empty turns it off. Only loopback is accepted.",
        example: "127.0.0.1:8082",
    },
    ABUSE_STREAM_CAP: usize {
        key: "abuse.stream_cap",
        default: 16,
        check: range(1, 100_000),
        description: "Live-update streams one client may hold open at once to one store.",
        example: "16",
    },
    ABUSE_SOFT_PER_MIN: u32 {
        key: "abuse.soft_per_min",
        default: 60,
        check: range(1, 10_000_000),
        description: "Requests a minute one visitor (a Tor circuit, or an address) may make to the checkout and public pages before being asked to solve a short challenge. Signed-in merchants and shops using their secret key are never challenged.",
        example: "60",
    },
    ABUSE_HARD_PER_MIN: u32 {
        key: "abuse.hard_per_min",
        default: 300,
        check: range(1, 10_000_000),
        description: "Requests a minute past which a visitor is refused outright (429) until the minute is up. Must be above the soft limit.",
        example: "300",
    },
    ABUSE_SIGNED_IN_PER_MIN: u32 {
        key: "abuse.signed_in_per_min",
        default: 600,
        check: range(1, 10_000_000),
        description: "Requests a minute a signed-in merchant may make (dashboard, POS). Never challenged.",
        example: "600",
    },
    ABUSE_CLIENT_LOGS_PER_MIN: u32 {
        key: "abuse.client_logs_per_min",
        default: 30,
        check: range(1, 100_000),
        description: "Log reports a minute one client may send (browser problem reports, POS session timelines, WooCommerce plugin errors). Past it, reports are dropped with a 429 - never a challenge - and the client's other requests are unaffected.",
        example: "30",
    },
    ABUSE_CHALLENGE_BITS: u32 {
        key: "abuse.challenge_bits",
        default: 16,
        check: range(8, 24),
        description: "How hard the challenge is, in leading zero bits of a SHA-256 hash. Each extra bit doubles the work; 16 takes a phone about a second.",
        example: "16",
    },
    ABUSE_UNDER_ATTACK: bool {
        key: "abuse.under_attack",
        default: false,
        description: "When true, every visitor who isn't signed in must pass a challenge before using the checkout or public pages (live updates are not affected). A pass lasts 10 minutes.",
        example: "false",
        sources: [Database],
    },
    LOGGING_LEVEL: String {
        key: "logging.level",
        default: telemetry::DEFAULT_LEVEL.to_string(),
        check: telemetry::check_level,
        description: "Which log lines monokulo writes: a level (error, warn, info, debug, trace), optionally followed by target=level pairs for parts of monokulo.",
        example: "info,monokulo::http=debug",
    },
    LOGGING_DEV_MODE_UNTIL: u64 {
        key: "logging.dev_mode_until",
        default: 0,
        check: range(0, i64::MAX),
        description: "Development logging: until this time monokulo logs at debug level, then goes back to the level above by itself. Secrets and addresses stay hidden either way.",
        sources: [Database],
    },
    LOGGING_RETENTION_DAYS: u64 {
        key: "logging.retention_days",
        default: telemetry::store::DEFAULT_RETENTION_DAYS,
        check: range(1, 365),
        description: "Days monokulo's log store keeps lines for the Logs page. Older lines are deleted once a minute.",
        example: "14",
    },
    LOGGING_MAX_MB: u64 {
        key: "logging.max_mb",
        default: telemetry::store::DEFAULT_MAX_MB,
        check: range(10, 100_000),
        description: "Most megabytes monokulo's log store may use. Past it, the oldest lines are deleted first.",
        example: "500",
    },
    LOGGING_OTLP_ENDPOINT: String {
        key: "logging.otlp_endpoint",
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
        sources: [Env],
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

/// Checks a `public_url` value: this instance's external base URL, the one
/// address plugins and customers use (clearnet or `.onion`). It must be an
/// absolute `http`/`https` URL with a host and nothing after it but an
/// optional `/` - no path, query, fragment or login - since callers append
/// paths like `/pay/{pk}/orders` to it. Returns it without the trailing
/// `/`. The empty string is not valid here; an empty setting just means
/// "not set" ([`MonokuloSettings::public_url`]).
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

/// What monokulo needs before its database opens, or must never keep in it,
/// read in `main`: where the database is, the log format, and the secrets.
#[derive(Debug, Clone, PartialEq)]
pub struct BootConfig {
    pub database_path: PathBuf,
    pub encryption_key: Secret,
    pub engine_token: Secret,
    pub log_format: telemetry::LogFormat,
}

impl Section for BootConfig {
    const NAME: &'static str = "boot";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &DATABASE_PATH,
            &CRYPTO_ENCRYPTION_KEY,
            &ENGINE_TOKEN,
            &LOGGING_FORMAT,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(BootConfig {
            database_path: snapshot.get(&DATABASE_PATH),
            encryption_key: snapshot.get(&CRYPTO_ENCRYPTION_KEY),
            engine_token: snapshot.get(&ENGINE_TOKEN),
            log_format: snapshot.get(&LOGGING_FORMAT),
        })
    }
}

/// Where monokulo listens and where it reaches the engine: read once at
/// start, so both apply on restart.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub engine_mode: EngineMode,
    pub engine_url: HttpUrl,
}

impl Section for ServerConfig {
    const NAME: &'static str = "server";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&SERVER_BIND, &ENGINE_MODE, &ENGINE_URL]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(ServerConfig {
            bind: snapshot.get(&SERVER_BIND).0,
            engine_mode: snapshot.get(&ENGINE_MODE),
            engine_url: snapshot.get(&ENGINE_URL),
        })
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

/// Read on each request that needs them ([`MonokuloSettings::signup_mode`],
/// [`MonokuloSettings::public_url`]); a save or reload applies at once.
#[derive(Debug, Clone, PartialEq)]
pub struct PerRequest {
    pub signup_mode: SignupMode,
    pub public_url: String,
}

/// Where merchants get key-custody-cli (`http::key_entry`): URL templates,
/// read once at start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliLinks {
    pub download: String,
    pub source: String,
}

impl Section for CliLinks {
    const NAME: &'static str = "key custody cli";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&KEY_CUSTODY_CLI_DOWNLOAD_URL, &KEY_CUSTODY_CLI_SOURCE_URL]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(CliLinks {
            download: snapshot.get(&KEY_CUSTODY_CLI_DOWNLOAD_URL),
            source: snapshot.get(&KEY_CUSTODY_CLI_SOURCE_URL),
        })
    }
}

/// Which engine images this site's key entry forms encrypt merchants' keys
/// to, and whether every store's keys must go there (`http::key_entry`):
/// monokulo's own, never the engine's. A save is checked against the
/// engine's first (`http::admin_settings`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnpEntryPolicy {
    /// `None` when no ID key is set and this build has no official one:
    /// encrypted key entry is then unavailable.
    pub trust: Option<key_custody::transport::TrustPolicy>,
    pub required: bool,
}

impl SnpEntryPolicy {
    /// Whether `trust` names the official ID key this build carries, which
    /// key-custody-cli and the browser's checker trust without being told.
    pub fn is_official(&self) -> bool {
        self.trust.is_some_and(|trust| {
            key_custody::transport::official_id_key_digest() == Some(trust.id_key_digest)
        })
    }
}

impl Section for SnpEntryPolicy {
    const NAME: &'static str = "key custody snp entry";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &KEY_CUSTODY_SNP_ENTRY_ID_KEY,
            &KEY_CUSTODY_SNP_ENTRY_MIN_GUEST_SVN,
            &KEY_CUSTODY_SNP_ENTRY_MIN_TCB,
            &KEY_CUSTODY_SNP_ENTRY_REQUIRED,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        SnpEntryPolicy::from_values(
            snapshot.get(&KEY_CUSTODY_SNP_ENTRY_ID_KEY).as_deref(),
            snapshot.get(&KEY_CUSTODY_SNP_ENTRY_MIN_GUEST_SVN),
            snapshot.get(&KEY_CUSTODY_SNP_ENTRY_MIN_TCB).as_deref(),
            snapshot.get(&KEY_CUSTODY_SNP_ENTRY_REQUIRED),
        )
        .map_err(|e| vec![e])
    }
}

impl SnpEntryPolicy {
    /// The policy the four `key_custody.snp_entry_*` settings make: no ID
    /// key set is the official one, when this build has one.
    pub fn from_values(
        id_key: Option<&str>,
        min_guest_svn: u32,
        min_tcb: Option<&str>,
        required: bool,
    ) -> Result<Self, FieldError> {
        use key_custody::transport::{
            official_id_key_digest, parse_id_key_digest, TcbFloor, TrustPolicy,
        };
        let digest =
            match id_key {
                Some(text) => Some(parse_id_key_digest(text).map_err(|e| {
                    FieldError::new(KEY_CUSTODY_SNP_ENTRY_ID_KEY.key, e.to_string())
                })?),
                None => official_id_key_digest(),
            };
        let min_tcb = TcbFloor::parse(min_tcb.unwrap_or_default())
            .map_err(|e| FieldError::new(KEY_CUSTODY_SNP_ENTRY_MIN_TCB.key, e.to_string()))?;
        if required && digest.is_none() {
            return Err(FieldError::new(
                KEY_CUSTODY_SNP_ENTRY_ID_KEY.key,
                "key_custody.snp_entry_required is on, but this build has no official engine ID key: set this to the digest of the key your engine image is signed with",
            ));
        }
        Ok(SnpEntryPolicy {
            trust: digest.map(|id_key_digest| TrustPolicy {
                id_key_digest,
                min_guest_svn,
                min_tcb,
            }),
            required,
        })
    }
}

/// An ID key digest setting: empty, or 96 hex characters.
#[expect(
    clippy::ref_option,
    reason = "the settings macro passes a setting's value by reference"
)]
fn check_id_key_digest(digest: &Option<String>) -> Result<(), String> {
    digest.as_deref().map_or(Ok(()), |text| {
        key_custody::transport::parse_id_key_digest(text)
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
}

/// A TCB floor setting: empty, or four numbers.
#[expect(
    clippy::ref_option,
    reason = "the settings macro passes a setting's value by reference"
)]
fn check_tcb_floor(floor: &Option<String>) -> Result<(), String> {
    floor.as_deref().map_or(Ok(()), |text| {
        key_custody::transport::TcbFloor::parse(text)
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
}

/// A key-custody-cli link template: an http(s) address, with `needs` in it
/// when given.
fn check_cli_url(value: &str, needs: &str) -> Result<(), String> {
    if !(value.starts_with("https://") || value.starts_with("http://")) {
        return Err("Must be an http:// or https:// address.".to_owned());
    }
    if !needs.is_empty() && !value.contains(needs) {
        return Err(format!(
            "Must contain {needs}, where the file for the merchant's computer goes."
        ));
    }
    Ok(())
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
            &HTTP_CACHE_MAX_MB,
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
            http_cache_bytes: snapshot.get(&HTTP_CACHE_MAX_MB) * 1024 * 1024,
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

/// monokulo's store for the runtime switches (`sources: [Database]`), over
/// its own `settings` table, through the
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
    /// Where monokulo listens, read once at start.
    pub server: live_settings::Live<ServerConfig>,
    /// Who may sign up and this instance's public address.
    pub per_request: live_settings::Live<PerRequest>,
    /// Where merchants get key-custody-cli, read once at start.
    pub cli_links: live_settings::Live<CliLinks>,
    /// What the key entry forms check an SEV-SNP engine against: at start
    /// and after every save.
    pub snp_entry: live_settings::Live<SnpEntryPolicy>,
}

impl MonokuloSettings {
    /// No registry and every default, for tests that don't use the admin
    /// settings page.
    pub fn defaults() -> Arc<Self> {
        Self::fixed(PerRequest {
            signup_mode: SIGNUP_MODE.default_value(),
            public_url: PUBLIC_URL.default_value(),
        })
    }

    /// No registry, with the given signup mode and public address: for
    /// tests.
    pub fn fixed(per_request: PerRequest) -> Arc<Self> {
        Self::fixed_with_snp_entry(
            per_request,
            SnpEntryPolicy {
                trust: key_custody::transport::official_id_key_digest().map(|id_key_digest| {
                    key_custody::transport::TrustPolicy {
                        id_key_digest,
                        min_guest_svn: 0,
                        min_tcb: key_custody::transport::TcbFloor::default(),
                    }
                }),
                required: false,
            },
        )
    }

    /// [`Self::fixed`], with the given SEV-SNP key entry policy: for tests.
    pub fn fixed_with_snp_entry(per_request: PerRequest, snp_entry: SnpEntryPolicy) -> Arc<Self> {
        Arc::new(MonokuloSettings {
            registry: None,
            server: live_settings::Live::new(ServerConfig {
                bind: SERVER_BIND.default_value().0,
                engine_mode: ENGINE_MODE.default_value(),
                engine_url: ENGINE_URL.default_value(),
            }),
            per_request: live_settings::Live::new(per_request),
            cli_links: live_settings::Live::new(CliLinks {
                download: KEY_CUSTODY_CLI_DOWNLOAD_URL.default_value(),
                source: KEY_CUSTODY_CLI_SOURCE_URL.default_value(),
            }),
            snp_entry: live_settings::Live::new(snp_entry),
        })
    }

    /// This instance's current signup mode.
    pub fn signup_mode(&self) -> SignupMode {
        self.per_request.load().signup_mode
    }

    /// This instance's public base URL (no trailing `/`), or `None` while
    /// it isn't set.
    pub fn public_url(&self) -> Option<String> {
        let value = &self.per_request.load().public_url;
        if value.trim().is_empty() {
            return None;
        }
        validate_public_url(value).ok()
    }

    /// Loads every setting, from the options file, the database and `env`,
    /// and applies it to the given runtime pieces; later saves and reloads
    /// through the registry apply the same way. Anything invalid, or a
    /// missing secret, stops it with every problem named.
    pub async fn load(
        db: Database,
        engine_client: EngineClient,
        exchange_rates: Arc<ExchangeRateProviders>,
        abuse: Arc<AbuseProtection>,
        onion: Option<OnionReloadable>,
        env: live_settings::Env,
        options: live_settings::OptionsFile,
    ) -> Result<Arc<Self>, String> {
        // The options file holds the configuration and the database the
        // runtime switches, each key in its own place.
        let layered = live_settings::LayeredStore::new(options, Arc::new(DbSettings(db)), ALL);
        let mut builder = Registry::builder_with_env(Arc::new(layered), ALL, env).await;
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
        // Read per request: nothing to rebuild when they change.
        let per_request = builder.section::<PerRequest>();
        let cli_links = builder.section::<CliLinks>();
        let snp_entry = builder.section::<SnpEntryPolicy>();
        // Read once at start, before the registry exists (`main.rs`).
        builder.section::<DatabaseConfig>();
        builder.section::<BootConfig>();
        let server = builder.section::<ServerConfig>();
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
            server,
            per_request,
            cli_links,
            snp_entry,
        }))
    }
}

/// The two secrets monokulo can't start without, for tests.
#[cfg(test)]
pub(crate) fn test_secrets() -> live_settings::Env {
    live_settings::Env::fixed([
        (CRYPTO_ENCRYPTION_KEY.env_var.to_string(), "07".repeat(32)),
        (
            ENGINE_TOKEN.env_var.to_string(),
            "t".repeat(shared::auth::MIN_ENGINE_TOKEN_LEN),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

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
        let with = |public_url: &str| {
            MonokuloSettings::fixed(PerRequest {
                signup_mode: SignupMode::InviteOnly,
                public_url: public_url.to_string(),
            })
            .public_url()
        };
        assert_eq!(MonokuloSettings::defaults().public_url(), None);
        assert_eq!(
            with("https://pay.example.com/").as_deref(),
            Some("https://pay.example.com")
        );
        assert_eq!(with("not a url"), None);
    }

    /// A value given on the command line wins over the options file's, as
    /// the registry (and so the admin page) resolves it.
    #[test]
    fn the_command_line_wins_over_the_options_file() {
        let file: std::collections::HashMap<String, String> =
            [(EXCHANGE_RATE_CACHE_SECONDS.key.to_string(), "3".to_string())].into();
        let none = live_settings::Env::fixed(Vec::<(String, String)>::new());
        let resolved =
            live_settings::read_sync_with_env::<ExchangeRateConfig>(Ok(file.clone()), &none);
        assert_eq!(resolved.cache_seconds, 3);
        assert_eq!(resolved.http_cache_bytes, 16 * 1024 * 1024);
        let given =
            none.with_cli([(EXCHANGE_RATE_CACHE_SECONDS.key.to_string(), "9".to_string())].into());
        let resolved = live_settings::read_sync_with_env::<ExchangeRateConfig>(Ok(file), &given);
        assert_eq!(resolved.cache_seconds, 9);
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
                http_cache_bytes: 16 * 1024 * 1024,
            }
        );
    }

    /// What has to be known at start: the engine's address (defaulting to
    /// one on this machine, or given on the command line), and the engine
    /// token and encryption key, from the environment, without which
    /// monokulo doesn't start.
    #[test]
    fn the_start_up_settings_come_from_outside_and_the_key_is_required() {
        let env = |pairs: &[(&str, &str)]| {
            live_settings::Env::fixed(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<Vec<_>>(),
            )
        };
        let token = "t".repeat(shared::auth::MIN_ENGINE_TOKEN_LEN);
        let none = env(&[]);
        assert_eq!(
            ENGINE_URL.require(&none).unwrap().as_str(),
            "http://127.0.0.1:8443"
        );
        let flagged = none
            .clone()
            .with_cli([("engine.url".to_string(), "http://engine:8443/".to_string())].into());
        assert_eq!(
            ENGINE_URL.require(&flagged).unwrap().as_str(),
            "http://engine:8443",
            "no trailing slash to double up"
        );

        assert_eq!(
            ENGINE_TOKEN
                .require(&env(&[("MONOKULO_ENGINE_TOKEN", &token)]))
                .unwrap()
                .expose(),
            token
        );
        let short = engine_mode(
            &Snapshot::new(
                [("engine.mode".to_string(), "remote".to_string())].into(),
                env(&[("MONOKULO_ENGINE_TOKEN", "short")]),
            ),
            &env(&[("MONOKULO_ENGINE_TOKEN", "short")]),
        )
        .unwrap_err();
        assert!(short.contains("at least 32 characters"), "{short}");

        let key = "ab".repeat(32);
        assert!(CRYPTO_ENCRYPTION_KEY
            .require(&env(&[("MONOKULO_ENCRYPTION_KEY", &key)]))
            .is_ok());
        assert!(CRYPTO_ENCRYPTION_KEY.require(&none).is_err());
        let not_hex = CRYPTO_ENCRYPTION_KEY
            .require(&env(&[("MONOKULO_ENCRYPTION_KEY", "zz")]))
            .unwrap_err();
        assert!(not_hex.contains("64 hex characters"), "{not_hex}");
        assert_eq!(encryption_key_bytes(&key).unwrap(), [0xab; 32]);
    }

    /// The engine's mode decides what else must, and mustn't, be given: a
    /// remote engine needs its token; an embedded one (the default) takes
    /// neither a URL nor a token, which would otherwise be silently unused.
    #[test]
    fn the_engine_mode_decides_whether_its_url_and_token_are_wanted() {
        let token = "t".repeat(shared::auth::MIN_ENGINE_TOKEN_LEN);
        let with_token =
            live_settings::Env::fixed([(ENGINE_TOKEN.env_var.to_string(), token.clone())]);
        let none = live_settings::Env::fixed(Vec::<(String, String)>::new());
        let file = |pairs: &[(&str, &str)]| -> std::collections::HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let mode = |stored: std::collections::HashMap<String, String>, env: &live_settings::Env| {
            engine_mode(&Snapshot::new(stored, env.clone()), env)
        };

        assert_eq!(mode(file(&[]), &none), Ok(EngineMode::Embedded));
        let refused = mode(file(&[]), &with_token).unwrap_err();
        assert!(
            refused.starts_with("MONOKULO_ENGINE_TOKEN only apply to a remote engine"),
            "{refused}"
        );
        let refused = mode(file(&[("engine.url", "http://engine:8443")]), &none).unwrap_err();
        assert!(refused.starts_with("engine.url only apply"), "{refused}");

        let remote = file(&[
            ("engine.mode", "remote"),
            ("engine.url", "http://engine:8443"),
        ]);
        assert_eq!(mode(remote.clone(), &with_token), Ok(EngineMode::Remote));
        let missing = mode(remote, &none).unwrap_err();
        assert!(
            missing.starts_with("MONOKULO_ENGINE_TOKEN must be set: engine.mode is remote"),
            "{missing}"
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
        assert!(BootConfig::from_snapshot(&snapshot).is_ok());
        assert!(ServerConfig::from_snapshot(&snapshot).is_ok());
        assert!(DatabaseConfig::from_snapshot(&snapshot).is_ok());
        // http_cache.max_mb is read by two (the engine client and the
        // exchange-rate providers).
        let covered: std::collections::BTreeSet<&str> = [
            EngineConnection::keys(),
            PerRequest::keys(),
            CliLinks::keys(),
            SnpEntryPolicy::keys(),
            ExchangeRateConfig::keys(),
            AbuseConfig::keys(),
            OnionListenerConfig::keys(),
            LoggingConfig::keys(),
            DatabaseConfig::keys(),
            BootConfig::keys(),
            ServerConfig::keys(),
        ]
        .iter()
        .flat_map(|keys| keys.iter().map(|setting| setting.key()))
        .collect();
        assert_eq!(covered.len(), ALL.len());
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
            test_secrets(),
            live_settings::OptionsFile::in_memory(""),
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
        let (settings, _engine, rates, abuse) = loaded(None).await;
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

        // The engine's address is saved for the next start, not applied to
        // the running client.
        let saved = registry
            .save(change("engine.url", "http://127.0.0.1:2"))
            .await
            .unwrap();
        assert_eq!(saved.restart_required, ["engine.url"]);

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
