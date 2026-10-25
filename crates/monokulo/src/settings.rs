//! Every monokulo setting, declared once with the `live-settings` library
//! (admin_settings_v2.md part 1): each setting's type, range, description
//! and example live in its declaration. Configuration comes from the options
//! file (`monokulo.toml`), which the admin settings page
//! (`http/admin_settings.rs`) saves to, or from its command-line option,
//! which wins and locks it on the page. Saving or reloading through the
//! registry applies the change to the running process (part 3).
//!
//! Three runtime switches, `abuse.under_attack`, `logging.dev_mode_until`
//! and `admin.query_enabled`, are kept in the database instead: they are
//! flipped from the admin page while running, not configured. Secrets come from the environment only
//! (the process list and the options file can be read by others): the
//! engine token, the collector's headers, and `crypto.encryption_key`, the
//! AES-256-GCM key every `store_connections.tenant_secret_token_encrypted`
//! row is encrypted with (`crate::crypto`). Keeping it in the database it
//! protects, or changing it while running, would leak it or make every
//! stored token unreadable.
//!
//! One secret is a deliberate exception, agreed with the instance's owner:
//! the CoinMarketCap API key (`exchange_rate.coinmarketcap_api_key`) may be
//! set from the admin settings page as well as from its environment
//! variable (`sources: [Database, Env]`). The page only asks for it, never
//! shows it back (it says whether one is set, with Replace and Remove);
//! [`DbSettings`] keeps it in its own `encrypted_settings` table, encrypted
//! with `crypto.encryption_key` and bound to its key, so neither the
//! database file nor the Query page holds it in the clear. Its variable,
//! when set, still wins and locks it on the page. It applies on a save or a
//! reload, like the other live settings. Every other secret stays in the
//! environment only.

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
    /// How visitors on Tor reach this instance, beside its web address.
    pub enum TorMode { Inside = "inside", External = "external", Off = "off" }
}

choice_value! {
    /// Where the engine runs.
    pub enum EngineMode { Embedded = "embedded", Remote = "remote" }
}

/// The table of monokulo's options file that holds an embedded engine's
/// settings: `[engine.payment]` holds its `payment.…`.
pub const ENGINE_TABLE: &str = "engine";

/// Settings renamed since an earlier release, old key and new. An options
/// file still holding an old key stops monokulo, naming the new key.
pub const RENAMED: &[(&str, &str)] = &[("public_url", "public_uri")];

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

fn check_public_uri(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Ok(())
    } else {
        validate_public_uri(value).map(|_| ())
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

fn check_onion_uri(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Ok(())
    } else {
        crate::onion::validate_onion_uri(value).map(|_| ())
    }
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
    ENGINE_STATUS_CACHE_MS: u64 {
        key: "engine.status_cache_ms",
        default: 10_000,
        check: range(1_000, 300_000),
        description: "Milliseconds a status fetched from the engine is reused before a page asks it again. Every page's status dot, the status page and the admin pages share it, so a burst of page loads costs the engine one request, not one each; under a second that shield is gone.",
        example: "10000",
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
    EXCHANGE_RATE_COINMARKETCAP_API_KEY: Secret {
        key: "exchange_rate.coinmarketcap_api_key",
        env: "MONOKULO_EXCHANGE_RATE_COINMARKETCAP_API_KEY",
        default: Secret::default(),
        // Unset is valid: CoinMarketCap is then asked without a key.
        check: |key: &Secret| if key.expose().is_empty() { Ok(()) } else { shared::coinmarketcap::check_api_key_format(key.expose()) },
        description: "Your CoinMarketCap API key, from the CoinMarketCap developer dashboard. With one, rates come from CoinMarketCap's keyed API under your plan's limits; without one, from its keyless API, which is limited per address and meant for trying it out. Kept encrypted, and never shown again once saved.",
        sources: [Database, Env],
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
        default: sqlite::DEFAULT_READ_CONNECTIONS,
        check: range(1, 64),
        description: "Read-only connections monokulo opens to its database, each on its own thread. Reads run side by side, so more help up to the number of CPU cores; each keeps its own cache of about 2 MB.",
        example: "4",
        applies: Restart,
    },
    RATE_LIMIT_PER_STORE_KEY_PER_MIN: u32 {
        key: "rate_limit.per_store_key_per_min",
        default: 600,
        check: range(1, 10_000_000),
        description: "Requests a minute a shop's server may make with its store's secret key (for example a shop plugin creating orders). These are never challenged.",
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
    KEY_CUSTODY_SNP_BUNDLES_PER_USER: usize {
        key: "key_custody.snp_bundles_per_user",
        default: 20,
        check: range(1, 10_000),
        description: "How many SEV-SNP key entry forms one account may have open at once. Each form holds a single-use challenge from the engine; past this, opening another expires that account's oldest, never anyone else's.",
        example: "20",
    },
    KEY_CUSTODY_SNP_BUNDLES_PER_USER_PER_MIN: u32 {
        key: "key_custody.snp_bundles_per_user_per_min",
        default: 30,
        check: range(1, 10_000),
        description: "How many SEV-SNP key entry forms one account may open per minute. Past this, the form shows an alert asking them to wait a minute, and no challenge is taken from the engine.",
        example: "30",
    },
    PUBLIC_URI: String {
        key: "public_uri",
        default: String::new(),
        check: |v: &String| check_public_uri(v),
        description: "This instance's web address, e.g. https://www.monokulo.com. Connect codes for apps and the plugin connect flow use it, and payment links shared from the dashboard; visitors on Tor get the onion address instead (onion_uri). Plugins and apps can't connect until it is set. Behind a reverse proxy that serves this instance under a path (and strips it), include the path, e.g. https://example.com/monokulo: every URL the pages and the API give starts with it.",
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
        description: "With tor.mode = external: a loopback address:port for your tor's onion service to connect to, with HiddenServiceExportCircuitID haproxy set in torrc, so each Tor circuit is its own client. Only loopback is accepted. Tor inside monokulo needs no port: leave it empty.",
        example: "127.0.0.1:8082",
    },
    TOR_MODE: TorMode {
        key: "tor.mode",
        default: TorMode::Off,
        description: "How visitors on Tor reach this instance, beside its web address. inside: monokulo runs Tor itself and makes an onion address, nothing else to install. external: a tor you run sends its onion service to abuse.onion_listener, and onion_uri is its address. off: the web address only.",
        example: "inside",
    },
    TOR_STATE_DIR: PathBuf {
        key: "tor.state_dir",
        default: live_settings::paths::data_file("tor").unwrap_or_else(|| PathBuf::from("tor")),
        description: "With tor.mode = inside: the folder Tor keeps the onion address's key and its directory cache in, by default ~/.local/share/monokulo/tor. Back it up with the secrets: without the key, the onion address changes.",
        example: "/var/lib/monokulo/tor",
    },
    TOR_HIDDEN_SERVICE_DIR: String {
        key: "tor.hidden_service_dir",
        default: String::new(),
        description: "With tor.mode = inside: keep the onion address an external tor served. A copy of its HiddenServiceDir (the folder holding hs_ed25519_secret_key), owned and readable only by monokulo's user. Its key is used instead of one Tor would make; empty makes a new address.",
        example: "/var/lib/monokulo/tor-hidden-service",
    },
    ONION_URI: String {
        key: "onion_uri",
        default: String::new(),
        check: |v: &String| check_onion_uri(v),
        description: "With tor.mode = external: this instance's onion address, from the hostname file in your tor's HiddenServiceDir. Visitors on Tor get links to it. With Tor inside monokulo, Tor makes it: leave it empty.",
        example: "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion",
    },
    TOR_ONION_LOCATION: bool {
        key: "tor.onion_location",
        default: true,
        description: "Tell Tor Browser visitors on the web address that the onion address exists (the Onion-Location header), so it offers to switch to it.",
        example: "true",
        sources: [Database],
    },
    WEBHOOKS_ALLOW_PRIVATE_URLS: bool {
        key: "webhooks.allow_private_urls",
        default: false,
        description: "Whether webhooks may be sent to private or loopback addresses. Only for testing against your own network.",
        example: "false",
    },
    WEBHOOKS_DELIVERY_TIMEOUT_MS: u64 {
        key: "webhooks.delivery_timeout_ms",
        default: 5000,
        check: range(100, 300_000),
        description: "Milliseconds a store's webhook endpoint has to answer before the attempt counts as failed.",
        example: "5000",
    },
    WEBHOOKS_MAX_ATTEMPTS: u32 {
        key: "webhooks.max_attempts",
        default: 8,
        check: range(1, 64),
        description: "Attempts per webhook delivery before giving up, with the wait doubling from 1 minute up to 64 minutes between them.",
        example: "8",
    },
    WEBHOOKS_KEEP_DELIVERED_DAYS: u64 {
        key: "webhooks.keep_delivered_days",
        default: 30,
        check: range(1, 3650),
        description: "Days a webhook delivery that arrived stays in the store's list of deliveries before it is deleted.",
        example: "30",
    },
    WEBHOOKS_KEEP_GIVEN_UP_DAYS: u64 {
        key: "webhooks.keep_given_up_days",
        default: 90,
        check: range(1, 3650),
        description: "Days a webhook delivery that was given up on stays in the store's list (where it can still be sent again) before it is deleted.",
        example: "90",
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
        description: "Log reports a minute one client may send (browser problem reports, POS session timelines, shop plugin errors). Past it, reports are dropped with a 429 - never a challenge - and the client's other requests are unaffected.",
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
        description: "When on, every visitor who isn't signed in must pass a challenge before using the checkout or public pages (live updates are not affected). A pass lasts 10 minutes.",
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
    ADMIN_QUERY_ENABLED: bool {
        key: "admin.query_enabled",
        default: true,
        description: "The Query page: admins can run read-only SQL against monokulo's and the engine's databases. Secret columns (password hashes, tokens, keys) always read as hidden, and every query is written to the Logs page.",
        example: "true",
        sources: [Database],
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

/// Checks a `public_uri` value: this instance's web address, the one
/// plugins and web visitors use (an onion address is `onion_uri`). It must be an
/// absolute `http`/`https` URL with a host, and may have a path: the prefix
/// a reverse proxy serves this instance under (`https://shop.example/monokulo`,
/// the proxy stripping it before passing a request on), which the API's
/// links start with (`base::for_request`). No query, fragment or
/// login, since callers append paths like `/checkout/{order_id}` to it.
/// Returns it without the trailing `/`. The empty string is not valid here;
/// an empty setting just means "not set" ([`MonokuloSettings::public_uri`]).
pub fn validate_public_uri(value: &str) -> Result<String, String> {
    let value = value.trim();
    let problem =
        "Enter this instance's web address, like https://pay.example.com, or https://example.com/monokulo when a proxy serves it under a path.";
    let parsed = url::Url::parse(value).map_err(|_| problem.to_string())?;
    if parsed.host_str().is_some_and(crate::onion::is_onion_host) {
        return Err("That's an onion address: put it in onion_uri (with tor.mode = external), and set this to the web address or leave it empty.".to_string());
    }
    let ok = matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str().is_some_and(|host| !host.is_empty())
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

/// Where monokulo listens, where it reaches the engine and how long it
/// trusts the engine's status (`http::status_page`): read once at start, so
/// all apply on restart.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub engine_mode: EngineMode,
    pub engine_url: HttpUrl,
    pub status_cache: Duration,
}

impl Section for ServerConfig {
    const NAME: &'static str = "server";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &SERVER_BIND,
            &ENGINE_MODE,
            &ENGINE_URL,
            &ENGINE_STATUS_CACHE_MS,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(ServerConfig {
            bind: snapshot.get(&SERVER_BIND).0,
            engine_mode: snapshot.get(&ENGINE_MODE),
            engine_url: snapshot.get(&ENGINE_URL),
            status_cache: Duration::from_millis(snapshot.get(&ENGINE_STATUS_CACHE_MS)),
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
/// [`MonokuloSettings::public_uri`]); a save or reload applies at once.
#[derive(Debug, Clone, PartialEq)]
pub struct PerRequest {
    pub signup_mode: SignupMode,
    pub public_uri: String,
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

/// How many SEV-SNP key entry forms one account may hold and open
/// (`http::key_entry`), so one account can't expire everyone else's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnpBundleLimits {
    pub per_user: usize,
    pub per_user_per_min: u32,
}

impl Default for SnpBundleLimits {
    fn default() -> Self {
        SnpBundleLimits {
            per_user: KEY_CUSTODY_SNP_BUNDLES_PER_USER.default_value(),
            per_user_per_min: KEY_CUSTODY_SNP_BUNDLES_PER_USER_PER_MIN.default_value(),
        }
    }
}

impl Section for SnpBundleLimits {
    const NAME: &'static str = "sev-snp key entry limits";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &KEY_CUSTODY_SNP_BUNDLES_PER_USER,
            &KEY_CUSTODY_SNP_BUNDLES_PER_USER_PER_MIN,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(SnpBundleLimits {
            per_user: snapshot.get(&KEY_CUSTODY_SNP_BUNDLES_PER_USER),
            per_user_per_min: snapshot.get(&KEY_CUSTODY_SNP_BUNDLES_PER_USER_PER_MIN),
        })
    }
}

/// How webhooks are delivered (`crate::webhooks::delivery`), read before
/// each batch, so a save applies to the next attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookConfig {
    pub allow_private_urls: bool,
    pub delivery_timeout: std::time::Duration,
    pub max_attempts: u32,
    /// How long a delivered delivery is kept, in seconds.
    pub keep_delivered_secs: i64,
    /// How long a given-up delivery is kept, in seconds.
    pub keep_given_up_secs: i64,
}

/// Days as seconds, for the keep settings.
fn days(days: u64) -> i64 {
    i64::try_from(days.saturating_mul(86_400)).unwrap_or(i64::MAX)
}

impl Default for WebhookConfig {
    fn default() -> Self {
        WebhookConfig {
            allow_private_urls: WEBHOOKS_ALLOW_PRIVATE_URLS.default_value(),
            delivery_timeout: std::time::Duration::from_millis(
                WEBHOOKS_DELIVERY_TIMEOUT_MS.default_value(),
            ),
            max_attempts: WEBHOOKS_MAX_ATTEMPTS.default_value(),
            keep_delivered_secs: days(WEBHOOKS_KEEP_DELIVERED_DAYS.default_value()),
            keep_given_up_secs: days(WEBHOOKS_KEEP_GIVEN_UP_DAYS.default_value()),
        }
    }
}

impl Section for WebhookConfig {
    const NAME: &'static str = "webhooks";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &WEBHOOKS_ALLOW_PRIVATE_URLS,
            &WEBHOOKS_DELIVERY_TIMEOUT_MS,
            &WEBHOOKS_MAX_ATTEMPTS,
            &WEBHOOKS_KEEP_DELIVERED_DAYS,
            &WEBHOOKS_KEEP_GIVEN_UP_DAYS,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(WebhookConfig {
            allow_private_urls: snapshot.get(&WEBHOOKS_ALLOW_PRIVATE_URLS),
            delivery_timeout: std::time::Duration::from_millis(
                snapshot.get(&WEBHOOKS_DELIVERY_TIMEOUT_MS),
            ),
            max_attempts: snapshot.get(&WEBHOOKS_MAX_ATTEMPTS),
            keep_delivered_secs: days(snapshot.get(&WEBHOOKS_KEEP_DELIVERED_DAYS)),
            keep_given_up_secs: days(snapshot.get(&WEBHOOKS_KEEP_GIVEN_UP_DAYS)),
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

/// The admin tools that can be switched off while running: read on each
/// request ([`MonokuloSettings::query_enabled`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminTools {
    pub query_enabled: bool,
}

impl Section for AdminTools {
    const NAME: &'static str = "admin tools";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&ADMIN_QUERY_ENABLED]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(AdminTools {
            query_enabled: snapshot.get(&ADMIN_QUERY_ENABLED),
        })
    }
}

impl Section for PerRequest {
    const NAME: &'static str = "per request";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&SIGNUP_MODE, &PUBLIC_URI]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(PerRequest {
            signup_mode: snapshot.get(&SIGNUP_MODE),
            public_uri: snapshot.get(&PUBLIC_URI),
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
            &EXCHANGE_RATE_COINMARKETCAP_API_KEY,
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
            coinmarketcap_api_key: snapshot.get(&EXCHANGE_RATE_COINMARKETCAP_API_KEY),
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

/// How visitors on Tor reach this instance (`crate::tor`, `crate::network`):
/// started, moved and stopped when saved, and read per request for the
/// onion address and the Onion-Location switch.
#[derive(Debug, Clone, PartialEq)]
pub struct TorConfig {
    pub mode: TorMode,
    /// With Tor inside monokulo: where it keeps its keys and cache.
    pub state_dir: PathBuf,
    /// With Tor inside monokulo: an external tor's HiddenServiceDir whose
    /// key (and so onion address) to keep.
    pub hidden_service_dir: Option<PathBuf>,
    /// With an external tor: its onion address (`http://….onion`).
    pub onion_uri: Option<String>,
    pub onion_location: bool,
}

impl TorConfig {
    /// Every setting at its default: Tor off.
    pub fn defaults() -> Self {
        TorConfig {
            mode: TOR_MODE.default_value(),
            state_dir: TOR_STATE_DIR.default_value(),
            hidden_service_dir: None,
            onion_uri: None,
            onion_location: TOR_ONION_LOCATION.default_value(),
        }
    }
}

impl Section for TorConfig {
    const NAME: &'static str = "tor";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &TOR_MODE,
            &TOR_STATE_DIR,
            &TOR_HIDDEN_SERVICE_DIR,
            &ONION_URI,
            &ABUSE_ONION_LISTENER,
            &TOR_ONION_LOCATION,
        ]
    }
    /// Refuses a setting that means nothing in the chosen mode, so nothing
    /// is set and silently ignored: an external tor needs its port and its
    /// address, and Tor inside monokulo takes neither.
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        let mode = snapshot.get(&TOR_MODE);
        let onion_uri = snapshot.get(&ONION_URI);
        let onion_uri = (!onion_uri.trim().is_empty())
            .then(|| crate::onion::validate_onion_uri(&onion_uri))
            .transpose()
            .map_err(|e| vec![FieldError::new(ONION_URI.key, e)])?;
        let listener = !snapshot.get(&ABUSE_ONION_LISTENER).trim().is_empty();
        let hidden_service_dir = snapshot.get(&TOR_HIDDEN_SERVICE_DIR);
        let hidden_service_dir = (!hidden_service_dir.trim().is_empty())
            .then(|| PathBuf::from(hidden_service_dir.trim()));
        let mut errors = Vec::new();
        let mut refuse = |key: &str, why: &str| errors.push(FieldError::new(key, why.to_string()));
        match mode {
            TorMode::Inside => {
                if !crate::tor::BUILT_IN {
                    refuse(TOR_MODE.key, "This monokulo was built without Tor inside it (its tor feature). Run tor yourself and set tor.mode = external.");
                }
                if onion_uri.is_some() {
                    refuse(ONION_URI.key, "Tor inside monokulo makes the onion address itself: leave onion_uri empty.");
                }
                if listener {
                    refuse(
                        ABUSE_ONION_LISTENER.key,
                        "Tor inside monokulo needs no port: leave abuse.onion_listener empty.",
                    );
                }
            }
            TorMode::External => {
                if onion_uri.is_none() {
                    refuse(ONION_URI.key, "With an external tor, set onion_uri to its onion address (the hostname file in its HiddenServiceDir), so visitors on Tor get links to it.");
                }
                if !listener {
                    refuse(ABUSE_ONION_LISTENER.key, "With an external tor, set abuse.onion_listener to the port its HiddenServicePort points at, e.g. 127.0.0.1:8082.");
                }
            }
            TorMode::Off => {
                if onion_uri.is_some() {
                    refuse(ONION_URI.key, "Tor is off (tor.mode = off): set tor.mode = external to use this onion address, or leave onion_uri empty.");
                }
                if listener {
                    refuse(ABUSE_ONION_LISTENER.key, "Tor is off (tor.mode = off): set tor.mode = external to serve your tor here, or leave abuse.onion_listener empty.");
                }
            }
        }
        if hidden_service_dir.is_some() && mode != TorMode::Inside {
            refuse(TOR_HIDDEN_SERVICE_DIR.key, "Only Tor inside monokulo (tor.mode = inside) uses a HiddenServiceDir's key: leave tor.hidden_service_dir empty.");
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(TorConfig {
            mode,
            state_dir: snapshot.get(&TOR_STATE_DIR),
            hidden_service_dir,
            onion_uri,
            onion_location: snapshot.get(&TOR_ONION_LOCATION),
        })
    }
}

/// monokulo's store for the runtime switches (`sources: [Database]`), over
/// its own `settings` table, through the
/// same reading and writing connections as everything else (so a save
/// queues behind other writes on the writer's thread, never on a Tokio
/// worker).
///
/// A secret the admin page may set (`sources: [Database, Env]`: only the
/// CoinMarketCap API key) goes to the `encrypted_settings` table instead,
/// encrypted with `crypto.encryption_key` and bound to its own key
/// (`crypto::Binding::Setting`), and comes back decrypted. One that can't
/// be decrypted (the encryption key was changed) is left out, as if unset,
/// and logged, naming the setting only.
pub struct DbSettings {
    db: Database,
    key: crate::crypto::AtRestKey,
}

impl DbSettings {
    pub fn new(db: Database, key: crate::crypto::AtRestKey) -> Self {
        DbSettings { db, key }
    }
}

/// Whether `key` is a secret kept encrypted by [`DbSettings`].
pub fn is_stored_secret(key: &str) -> bool {
    ALL.iter().any(|setting| {
        setting.key() == key
            && setting.kind() == live_settings::SettingKind::Secret
            && setting.sources().database
    })
}

#[live_settings::async_trait]
impl live_settings::SettingsStore for DbSettings {
    async fn read_all(
        &self,
    ) -> Result<std::collections::HashMap<String, String>, live_settings::StoreError> {
        let (mut plain, encrypted) = self
            .db
            .read(|db| {
                Ok::<_, crate::db::DbError>((db.list_settings()?, db.list_encrypted_settings()?))
            })
            .await
            .map_err(live_settings::StoreError::new)?;
        for (key, value) in encrypted {
            if !is_stored_secret(&key) {
                continue;
            }
            match crate::crypto::decrypt(&self.key, crate::crypto::Binding::Setting(&key), &value) {
                Ok(secret) => {
                    plain.insert(key, secret);
                }
                Err(error) => tracing::error!(
                    setting = %key,
                    %error,
                    "a saved secret setting can't be decrypted with this encryption key, so it is treated as unset: enter it again on the admin settings page"
                ),
            }
        }
        Ok(plain)
    }

    async fn write_all(
        &self,
        changes: Vec<(&'static str, Option<String>)>,
    ) -> Result<(), live_settings::StoreError> {
        let mut plain = Vec::new();
        let mut encrypted = Vec::new();
        for (key, value) in changes {
            if is_stored_secret(key) {
                let value = value.map(|secret| {
                    crate::crypto::encrypt(&self.key, crate::crypto::Binding::Setting(key), &secret)
                });
                encrypted.push((key, value));
            } else {
                plain.push((key, value));
            }
        }
        self.db
            .write(move |db| db.write_settings_and_secrets(&plain, &encrypted))
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
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let address = listener.bound_address();
        tokio::spawn(async move {
            if let Err(e) = crate::tor::serve(listener, router, stopped).await {
                tracing::error!(server.address = %address, error = %e, "onion listener stopped");
            }
        });
        tracing::info!(server.address = %address, "onion listener (PROXY protocol, for tor) started");
        *self.inner.running.lock() = Some(stop);
    }
}

/// What serves the onion address, started, moved and stopped by its
/// settings: the listener an external tor connects to, and Tor inside
/// monokulo.
pub struct OnionServing {
    pub listener: OnionReloadable,
    pub tor: crate::tor::Tor,
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
    /// How many key entry forms one account may hold and open.
    pub snp_bundle_limits: live_settings::Live<SnpBundleLimits>,
    /// How webhooks are delivered: read before each batch.
    pub webhooks: live_settings::Live<WebhookConfig>,
    /// Which admin tools are on: read per request.
    pub admin_tools: live_settings::Live<AdminTools>,
    /// How visitors on Tor reach this instance: read per request.
    pub tor: live_settings::Live<TorConfig>,
}

impl MonokuloSettings {
    /// No registry and every default, for tests that don't use the admin
    /// settings page.
    pub fn defaults() -> Arc<Self> {
        Self::fixed(PerRequest {
            signup_mode: SIGNUP_MODE.default_value(),
            public_uri: PUBLIC_URI.default_value(),
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
                status_cache: Duration::from_millis(ENGINE_STATUS_CACHE_MS.default_value()),
            }),
            per_request: live_settings::Live::new(per_request),
            cli_links: live_settings::Live::new(CliLinks {
                download: KEY_CUSTODY_CLI_DOWNLOAD_URL.default_value(),
                source: KEY_CUSTODY_CLI_SOURCE_URL.default_value(),
            }),
            snp_entry: live_settings::Live::new(snp_entry),
            snp_bundle_limits: live_settings::Live::new(SnpBundleLimits::default()),
            webhooks: live_settings::Live::new(WebhookConfig::default()),
            admin_tools: live_settings::Live::new(AdminTools {
                query_enabled: ADMIN_QUERY_ENABLED.default_value(),
            }),
            tor: live_settings::Live::new(TorConfig::defaults()),
        })
    }

    /// These settings with `webhooks` in place of their webhook settings:
    /// for tests that deliver to an endpoint on this machine.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_webhooks(&self, webhooks: WebhookConfig) -> Arc<Self> {
        Arc::new(MonokuloSettings {
            registry: None,
            server: self.server.clone(),
            per_request: self.per_request.clone(),
            cli_links: self.cli_links.clone(),
            snp_entry: self.snp_entry.clone(),
            snp_bundle_limits: self.snp_bundle_limits.clone(),
            webhooks: live_settings::Live::new(webhooks),
            admin_tools: self.admin_tools.clone(),
            tor: self.tor.clone(),
        })
    }

    /// These settings with `tor` in place of their Tor settings: for tests
    /// of what visitors on Tor get.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_tor(&self, tor: TorConfig) -> Arc<Self> {
        Arc::new(MonokuloSettings {
            registry: None,
            server: self.server.clone(),
            per_request: self.per_request.clone(),
            cli_links: self.cli_links.clone(),
            snp_entry: self.snp_entry.clone(),
            snp_bundle_limits: self.snp_bundle_limits.clone(),
            webhooks: self.webhooks.clone(),
            admin_tools: self.admin_tools.clone(),
            tor: live_settings::Live::new(tor),
        })
    }

    /// Whether the admin Query page is on.
    pub fn query_enabled(&self) -> bool {
        self.admin_tools.load().query_enabled
    }

    /// This instance's current signup mode.
    pub fn signup_mode(&self) -> SignupMode {
        self.per_request.load().signup_mode
    }

    /// This instance's public base URL (no trailing `/`), or `None` while
    /// it isn't set.
    pub fn public_uri(&self) -> Option<String> {
        let value = &self.per_request.load().public_uri;
        if value.trim().is_empty() {
            return None;
        }
        validate_public_uri(value).ok()
    }

    /// Loads every setting, from the options file, the database and `env`,
    /// and applies it to the given runtime pieces; later saves and reloads
    /// through the registry apply the same way. Anything invalid, or a
    /// missing secret, stops it with every problem named.
    pub async fn load(
        store: DbSettings,
        engine_client: EngineClient,
        exchange_rates: Arc<ExchangeRateProviders>,
        abuse: Arc<AbuseProtection>,
        onion: Option<OnionServing>,
        env: live_settings::Env,
        options: live_settings::OptionsFile,
    ) -> Result<Arc<Self>, String> {
        // The options file holds the configuration and the database the
        // runtime switches, each key in its own place.
        let layered = live_settings::LayeredStore::new(options, Arc::new(store), ALL);
        let mut builder = Registry::builder_with_env(Arc::new(layered), ALL, env).await;
        builder.reloadable(EngineConnectionReloadable { engine_client });
        builder.reloadable(ExchangeRatesReloadable {
            providers: exchange_rates,
        });
        builder.reloadable(AbuseReloadable { abuse });
        // The onion listener and Tor inside monokulo start and stop with
        // their settings; the rest of the Tor settings are read per request.
        let tor = match onion {
            Some(OnionServing { listener, tor }) => {
                builder.reloadable(listener);
                builder.reloadable(crate::tor::TorReloadable::new(tor))
            }
            None => {
                builder.section::<OnionListenerConfig>();
                builder.section::<TorConfig>()
            }
        };
        // Read per request: nothing to rebuild when they change.
        let per_request = builder.section::<PerRequest>();
        let cli_links = builder.section::<CliLinks>();
        let snp_entry = builder.section::<SnpEntryPolicy>();
        let snp_bundle_limits = builder.section::<SnpBundleLimits>();
        let webhooks = builder.section::<WebhookConfig>();
        let admin_tools = builder.section::<AdminTools>();
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
            snp_bundle_limits,
            webhooks,
            admin_tools,
            tor,
        }))
    }
}

/// The two secrets monokulo can't start without, for tests.
#[cfg(any(test, feature = "test-support"))]
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

    /// The CoinMarketCap API key, the one secret the admin page may set:
    /// stored encrypted, bound to its key, in its own table (the Query page
    /// and its export hide the column), read back decrypted; removed with
    /// its row; and, under another encryption key, treated as unset rather
    /// than stopping anything.
    #[tokio::test]
    async fn a_secret_the_page_sets_is_stored_encrypted_and_read_back_decrypted() {
        use live_settings::SettingsStore;
        const KEY: &str = "b54bcf4d-1bca-4e8e-9a24-22ff2c3d462c";
        let api_key = EXCHANGE_RATE_COINMARKETCAP_API_KEY.key;
        assert!(is_stored_secret(api_key));
        assert!(!is_stored_secret(CRYPTO_ENCRYPTION_KEY.key), "env only");
        assert!(!is_stored_secret(ABUSE_UNDER_ATTACK.key), "not a secret");

        let shared_db = Db::open_in_memory().unwrap().into_shared();
        let db = Database::inline(shared_db.clone());
        let store = DbSettings::new(db.clone(), crate::http::TEST_ENCRYPTION_KEY);
        store
            .write_all(vec![
                (api_key, Some(KEY.to_string())),
                (ABUSE_UNDER_ATTACK.key, Some("true".to_string())),
            ])
            .await
            .unwrap();

        let raw = shared_db.lock().list_encrypted_settings().unwrap();
        let stored = raw.get(api_key).expect("kept in encrypted_settings");
        assert!(!stored.contains(KEY), "the raw value isn't the key");
        assert_eq!(
            crate::crypto::decrypt(
                &crate::http::TEST_ENCRYPTION_KEY,
                crate::crypto::Binding::Setting(api_key),
                stored
            )
            .unwrap(),
            KEY
        );
        assert_eq!(shared_db.lock().get_setting(api_key).unwrap(), None);
        let read = store.read_all().await.unwrap();
        assert_eq!(read.get(api_key).map(String::as_str), Some(KEY));
        assert_eq!(
            read.get(ABUSE_UNDER_ATTACK.key).map(String::as_str),
            Some("true")
        );

        // The Query page (and its export) reads the column as hidden.
        let result = shared_db
            .lock()
            .admin_query(
                "SELECT key, value_encrypted FROM encrypted_settings",
                sqlite::query::Limits::EXPORT,
            )
            .unwrap();
        assert_eq!(
            result.hidden,
            vec!["encrypted_settings.value_encrypted".to_string()]
        );
        assert!(!format!("{:?}", result.rows).contains(stored.as_str()));

        // Under another key it can't be read: unset, and nothing else lost.
        let other = DbSettings::new(db.clone(), crate::crypto::AtRestKey::new([8; 32]));
        let read = other.read_all().await.unwrap();
        assert_eq!(read.get(api_key), None);
        assert_eq!(
            read.get(ABUSE_UNDER_ATTACK.key).map(String::as_str),
            Some("true")
        );

        // Copied under another setting's key, it doesn't decrypt there.
        assert!(crate::crypto::decrypt(
            &crate::http::TEST_ENCRYPTION_KEY,
            crate::crypto::Binding::Setting("exchange_rate.other"),
            stored
        )
        .is_err());

        store.write_all(vec![(api_key, None)]).await.unwrap();
        assert!(shared_db
            .lock()
            .list_encrypted_settings()
            .unwrap()
            .is_empty());
        assert_eq!(store.read_all().await.unwrap().get(api_key), None);
    }

    #[test]
    fn a_public_uri_is_an_http_base_url_maybe_with_a_path() {
        for (input, expected) in [
            ("https://pay.example.com", "https://pay.example.com"),
            ("https://pay.example.com/", "https://pay.example.com"),
            (
                "  http://pay.example.com:8081/ ",
                "http://pay.example.com:8081",
            ),
            // Served by a reverse proxy under a path.
            (
                "https://shop.example/monokulo",
                "https://shop.example/monokulo",
            ),
            (
                "https://shop.example/monokulo/",
                "https://shop.example/monokulo",
            ),
            ("https://shop.example/a/b", "https://shop.example/a/b"),
        ] {
            assert_eq!(
                validate_public_uri(input).as_deref(),
                Ok(expected),
                "{input}"
            );
        }
        for bad in [
            "",
            "pay.example.com",
            "ftp://pay.example.com",
            "https://pay.example.com/?a=b",
            "https://pay.example.com/#top",
            "https://user:pass@pay.example.com",
            "https://",
            // The onion address is `onion_uri`'s.
            "http://abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz234.onion",
        ] {
            assert!(
                validate_public_uri(bad).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn public_uri_is_none_until_set_and_then_normalized() {
        let with = |public_uri: &str| {
            MonokuloSettings::fixed(PerRequest {
                signup_mode: SignupMode::InviteOnly,
                public_uri: public_uri.to_string(),
            })
            .public_uri()
        };
        assert_eq!(MonokuloSettings::defaults().public_uri(), None);
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

    /// The browser tests trust the engine's status for a second
    /// (e2e/browser/real-stack.js), so this pins what an instance runs
    /// without the setting: ten seconds. Under a second is refused, and a
    /// refused value stops monokulo at start.
    #[test]
    fn the_engine_status_is_trusted_for_ten_seconds_unless_set_and_never_under_one() {
        assert_eq!(ENGINE_STATUS_CACHE_MS.default_value(), 10_000);
        assert_eq!(
            ServerConfig::from_snapshot(&Snapshot::defaults())
                .unwrap()
                .status_cache,
            Duration::from_secs(10)
        );
        assert_eq!(ENGINE_STATUS_CACHE_MS.parse("1000"), Ok(1_000));
        assert!(ENGINE_STATUS_CACHE_MS.parse("999").is_err());
        assert!(ENGINE_STATUS_CACHE_MS.parse("0").is_err());
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
                coinmarketcap_api_key: Secret::default(),
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
        assert!(AdminTools::from_snapshot(&snapshot).unwrap().query_enabled);
        assert_eq!(
            WebhookConfig::from_snapshot(&snapshot).unwrap(),
            WebhookConfig::default()
        );
        // http_cache.max_mb is read by two (the engine client and the
        // exchange-rate providers).
        let covered: std::collections::BTreeSet<&str> = [
            EngineConnection::keys(),
            PerRequest::keys(),
            CliLinks::keys(),
            SnpEntryPolicy::keys(),
            SnpBundleLimits::keys(),
            ExchangeRateConfig::keys(),
            AbuseConfig::keys(),
            OnionListenerConfig::keys(),
            TorConfig::keys(),
            LoggingConfig::keys(),
            DatabaseConfig::keys(),
            BootConfig::keys(),
            ServerConfig::keys(),
            WebhookConfig::keys(),
            AdminTools::keys(),
        ]
        .iter()
        .flat_map(|keys| keys.iter().map(|setting| setting.key()))
        .collect();
        assert_eq!(covered.len(), ALL.len());
    }

    /// How long webhook deliveries are kept comes from the options file;
    /// a value out of range stops monokulo from starting, naming it.
    #[tokio::test]
    async fn the_webhook_delivery_keep_periods_load_and_bad_ones_stop_monokulo() {
        let load = |options: &str| {
            let options = options.to_string();
            async move {
                MonokuloSettings::load(
                    DbSettings::new(
                        Database::inline(Db::open_in_memory().unwrap().into_shared()),
                        crate::http::TEST_ENCRYPTION_KEY,
                    ),
                    EngineClient::for_tests("http://127.0.0.1:1"),
                    Arc::new(ExchangeRateProviders::xmr_only()),
                    Default::default(),
                    None,
                    test_secrets(),
                    live_settings::OptionsFile::in_memory(&options),
                )
                .await
            }
        };
        let settings = load("[webhooks]\nkeep_delivered_days = 7\nkeep_given_up_days = 45\n")
            .await
            .unwrap();
        let webhooks = settings.webhooks.load();
        assert_eq!(
            (webhooks.keep_delivered_secs, webhooks.keep_given_up_secs),
            (7 * 86_400, 45 * 86_400)
        );
        for bad in [
            "[webhooks]\nkeep_delivered_days = 0\n",
            "[webhooks]\nkeep_given_up_days = 5000\n",
        ] {
            let refused = match load(bad).await {
                Err(refused) => refused,
                Ok(_) => panic!("{bad} was accepted"),
            };
            assert!(refused.contains("webhooks.keep_"), "{refused}");
        }
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
            DbSettings::new(db, crate::http::TEST_ENCRYPTION_KEY),
            engine.clone(),
            rates.clone(),
            abuse.clone(),
            onion.map(|listener| OnionServing {
                listener,
                tor: crate::tor::Tor::default(),
            }),
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

    /// An external tor serving its onion service to `listener`: the mode,
    /// its port and its address, saved together.
    fn external_tor(listener: &str) -> live_settings::Changes {
        [
            ("tor.mode", "external"),
            ("abuse.onion_listener", listener),
            (
                "onion_uri",
                "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion",
            ),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), Some(value.to_string())))
        .collect()
    }

    /// Tor turned off, with the external tor's port and address cleared.
    fn tor_off() -> live_settings::Changes {
        ["tor.mode", "abuse.onion_listener", "onion_uri"]
            .into_iter()
            .map(|key| {
                (
                    key.to_string(),
                    Some(if key == "tor.mode" { "off" } else { "" }.to_string()),
                )
            })
            .collect()
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
            .save(external_tor(&free.to_string()))
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
            .save(external_tor(&other.to_string()))
            .await
            .unwrap();
        registry
            .save(external_tor(&free.to_string()))
            .await
            .expect("moving back to the first address");
        assert!(
            tokio::net::TcpStream::connect(free).await.is_ok(),
            "listening on the first address again"
        );

        registry.save(tor_off()).await.unwrap();
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
            .save(external_tor(&taken.local_addr().unwrap().to_string()))
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
