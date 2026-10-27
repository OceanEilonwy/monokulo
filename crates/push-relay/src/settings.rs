//! Every relay setting, declared once with the `live-settings` library and
//! registered by both binaries: `monokulo-push-relay` reads them from
//! `relay.toml` and `--…` options, monokulo from the `[relay.*]` tables of
//! its own options file and `--relay-…` options, by the same keys (the
//! embedded engine's pattern). Secrets (the APNs key, the FCM service
//! account) come from the environment only, in both.
//!
//! The relay reads its settings once, at start: nothing here reloads.
//! Anything invalid stops the process ([`load`]), every problem named.
//!
//! Every key has two parts (`server.listen`, never `listen`): inside
//! monokulo a value directly in `[relay]` is monokulo's own
//! (`relay.enabled`), and only the tables under it are the relay's.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use abuse::TrustedProxies;
use live_settings::{
    settings, AnySetting, BindAddr, Env, FieldError, Json, MemoryStore, Registry, Secret, Section,
    SettingKind, SettingValue, Snapshot,
};
use push::jwt::Es256Key;
use push::{Endpoint, Origin};
use serde::{Deserialize, Serialize};

use crate::fcm::ServiceAccount;
use crate::handles::{AppName, Lifetimes};

settings! {
    SERVER_LISTEN: BindAddr {
        key: "server.listen",
        default: live_settings::parsed_default("0.0.0.0:8444"),
        description: "The address and port the relay listens on. Apps register here and senders push here, so it is meant to be reachable: put it behind the https address server.public_uri names.",
        example: "0.0.0.0:8444",
        applies: Restart,
    },
    SERVER_PUBLIC_URI: Option<PublicUri> {
        key: "server.public_uri",
        default: None,
        description: "The https address apps and senders reach this relay at, such as https://relay.example.org: every endpoint it hands out is under it, and a push is accepted only when its VAPID token is signed for it. Required to run the relay. An origin alone (no path), https unless it is this machine.",
        example: "https://relay.example.org",
        applies: Restart,
    },
    SERVER_TRUSTED_PROXIES: String {
        key: "server.trusted_proxies",
        default: String::new(),
        check: |value: &String| check_trusted_proxies(value),
        description: "Addresses or CIDR ranges of the reverse proxies in front of the relay, separated by commas. A request from one of them is counted against the address its X-Forwarded-For names, not the proxy's. Leave empty when nothing is in front.",
        example: "127.0.0.1, 10.0.0.0/8",
        applies: Restart,
    },
    SERVER_UPSTREAM_TIMEOUT_MS: u64 {
        key: "server.upstream_timeout_ms",
        default: 10_000,
        check: range(100, 120_000),
        description: "Milliseconds the relay waits for APNs or FCM to take a push (and for FCM's token endpoint) before answering the sender 503, to try again later.",
        example: "10000",
        applies: Restart,
    },
    STORE_PATH: PathBuf {
        key: "store.path",
        default: live_settings::paths::data_file("push-relay.db").unwrap_or_else(|| PathBuf::from("push-relay.db")),
        description: "The relay's database file, ~/.local/share/monokulo/push-relay.db unless set (or push-relay.db in the working directory if that can't be used). It holds the handles alone, never a push, and is the same file whether the relay runs inside monokulo or on its own.",
        example: "/var/lib/monokulo/push-relay.db",
        applies: Restart,
        editable: false,
    },
    HANDLES_TTL_DAYS: u32 {
        key: "handles.ttl_days",
        default: 90,
        check: range(1, 3650),
        description: "Days a handle lasts after its last registration or the last push APNs or FCM took for it, once APNs or FCM has taken one. After that it is forgotten and its endpoint answers 404, so a sender drops it.",
        example: "90",
        applies: Restart,
    },
    HANDLES_UNUSED_TTL_DAYS: u32 {
        key: "handles.unused_ttl_days",
        default: 7,
        check: range(1, 3650),
        description: "Days a handle no push has reached yet lasts after its registration: at most handles.ttl_days. Registering needs no credential, so this is how long a made-up token's handle is kept; an app registers again whenever it starts, which pushes it back.",
        example: "7",
        applies: Restart,
    },
    HANDLES_APPS: Json<Apps> {
        key: "handles.apps",
        default: Json(Apps::default()),
        description: "The apps devices may register for, by the name they register with (lowercase letters, digits and hyphens), each with its iOS bundle ID (apns_topic) for APNs and its Firebase project ID (fcm_project_id) for FCM, either or both. In the options file, a table per app.",
        example: r#"{"monokular":{"apns_topic":"com.example.monokular","fcm_project_id":"monokular-12345"}}"#,
        applies: Restart,
    },
    LIMITS_PER_SENDER_PER_MINUTE: u32 {
        key: "limits.per_sender_per_minute",
        default: 600,
        check: range(1, 1_000_000),
        description: "Pushes a minute each sender (a VAPID key) may hand on, to all its handles together. A sender is a whole monokulo, every store on it, or a whole Bell, every merchant's tills: size this for the busiest you serve (for a hosted monokulo, its stores' push.per_store_per_minute together at their busiest). Pushes a handle's own limit refuses don't count.",
        example: "600",
        applies: Restart,
    },
    LIMITS_PER_HANDLE_PER_MINUTE: u32 {
        key: "limits.per_handle_per_minute",
        default: 60,
        check: range(1, 1_000_000),
        description: "Pushes a minute to any one handle. A sale is at most five pushes, so this only stops a sender gone wrong from draining a device's battery.",
        example: "60",
        applies: Restart,
    },
    LIMITS_REGISTRATIONS_PER_MINUTE: u32 {
        key: "limits.registrations_per_minute",
        default: 30,
        check: range(1, 1_000_000),
        description: "Registrations a minute from any one address (IPv6 by its /64).",
        example: "30",
        applies: Restart,
    },
    APNS_ENDPOINT: ServiceUrl {
        key: "apns.endpoint",
        default: live_settings::parsed_default("https://api.push.apple.com"),
        description: "Where APNs is: https://api.push.apple.com, or https://api.sandbox.push.apple.com for apps built for development.",
        example: "https://api.sandbox.push.apple.com",
        applies: Restart,
    },
    APNS_TEAM_ID: Option<AppleId> {
        key: "apns.team_id",
        default: None,
        description: "The Apple Developer team ID the APNs key belongs to: 10 letters and digits, from the membership page. Required with the key.",
        example: "ABCDE12345",
        applies: Restart,
    },
    APNS_KEY_ID: Option<AppleId> {
        key: "apns.key_id",
        default: None,
        description: "The APNs key's ID: 10 letters and digits, the <id> of its AuthKey_<id>.p8. Required with the key.",
        example: "FGHIJ67890",
        applies: Restart,
    },
    APNS_KEY: Secret {
        key: "apns.key",
        env: "MONOKULO_RELAY_APNS_KEY",
        default: Secret::default(),
        check: check_apns_key,
        description: "The APNs signing key: the whole AuthKey_<id>.p8 file Apple gave you, as text. Without it the relay hands nothing to APNs and refuses iOS registrations.",
        applies: Restart,
        sources: [Env],
    },
    FCM_ENDPOINT: ServiceUrl {
        key: "fcm.endpoint",
        default: live_settings::parsed_default("https://fcm.googleapis.com"),
        description: "Where Firebase Cloud Messaging's HTTP v1 API is.",
        example: "https://fcm.googleapis.com",
        applies: Restart,
    },
    FCM_CREDENTIALS: Secret {
        key: "fcm.credentials",
        env: "MONOKULO_RELAY_FCM_CREDENTIALS",
        default: Secret::default(),
        check: check_fcm_credentials,
        description: "A Firebase service account allowed to send messages: the whole JSON key file Google gave you, as text. Without it the relay hands nothing to FCM and refuses Android registrations.",
        applies: Restart,
        sources: [Env],
    },
    LOGGING_LEVEL: String {
        key: "logging.level",
        default: telemetry::DEFAULT_LEVEL.to_owned(),
        check: telemetry::check_level,
        description: "Which log lines the relay writes: a level (error, warn, info, debug, trace), optionally followed by target=level pairs. No line carries a token, an endpoint or a push.",
        example: "info,push_relay=debug",
        applies: Restart,
    },
    LOGGING_FORMAT: telemetry::LogFormat {
        key: "logging.format",
        default: telemetry::LogFormat::Auto,
        description: "How log lines are written to the console: json, pretty, or auto (pretty at a terminal, JSON everywhere else).",
        example: "json",
        applies: Restart,
    },
}

/// Settings that only mean something for `monokulo-push-relay`: its own
/// process's logging. Inside monokulo the relay logs through monokulo's
/// logger, so these aren't among its `--relay-…` options or `[relay.*]`
/// keys, and given there they stop monokulo as unknown.
pub const STANDALONE_ONLY: &[&'static dyn AnySetting] = &[&LOGGING_LEVEL, &LOGGING_FORMAT];

/// Every setting a relay inside monokulo has: [`ALL`] less
/// [`STANDALONE_ONLY`]. What monokulo offers as `--relay-…` and reads from
/// `[relay.*]`.
pub fn embedded_settings() -> Vec<&'static dyn AnySetting> {
    ALL.iter()
        .copied()
        .filter(|setting| {
            !STANDALONE_ONLY
                .iter()
                .any(|standalone| standalone.key() == setting.key())
        })
        .collect()
}

/// Where the relay runs, which decides the settings it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// `monokulo-push-relay`: every setting.
    Standalone,
    /// Inside monokulo: [`embedded_settings`].
    Embedded,
}

/// Why the settings can't be used: every problem, one per line.
#[derive(Debug, thiserror::Error)]
#[error("the push relay's settings: {0}")]
pub struct LoadError(String);

/// The relay's settings from `stored` (what its options file holds, by
/// key, already checked line by line) and `env` (the command line and the
/// environment). Every value is checked again where it came from, then
/// together ([`RelayConfig`]'s rules); anything wrong is the error, never
/// a default in its place.
pub async fn load(
    stored: HashMap<String, String>,
    env: Env,
    host: Host,
) -> Result<RelayConfig, LoadError> {
    let declared = match host {
        Host::Standalone => ALL.to_vec(),
        Host::Embedded => embedded_settings(),
    };
    let store = Arc::new(MemoryStore::with(stored));
    let mut builder = Registry::builder_with_env(store, &declared, env).await;
    let relay = builder.section::<RelayConfig>();
    if host == Host::Standalone {
        // Read before the registry (logging starts first); registered so
        // a bad value there stops the relay like any other.
        builder.section::<LoggingConfig>();
    }
    builder
        .build()
        .map_err(|error| LoadError(error.to_string()))?;
    Ok((*relay.load()).clone())
}

/// The settings the relay runs on, checked together.
#[derive(Clone, PartialEq)]
pub struct RelayConfig {
    pub listen: SocketAddr,
    /// Required to start ([`crate::Relay::start`]), not to load: inside
    /// monokulo a relay that isn't enabled needs none.
    pub public_uri: Option<PublicUri>,
    pub trusted_proxies: TrustedProxies,
    /// How long APNs, FCM and FCM's token endpoint get to answer.
    pub upstream_timeout: Duration,
    pub store_path: PathBuf,
    pub handle_lifetimes: Lifetimes,
    pub apps: Apps,
    pub limits: LimitsConfig,
    /// Set when the APNs key is.
    pub apns: Option<ApnsConfig>,
    /// Set when the FCM service account is.
    pub fcm: Option<FcmConfig>,
}

/// The three limits, a minute each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitsConfig {
    pub per_sender: u32,
    pub per_handle: u32,
    pub registrations: u32,
}

/// What the relay needs to hand pushes to APNs.
#[derive(Clone, PartialEq)]
pub struct ApnsConfig {
    pub endpoint: ServiceUrl,
    pub team_id: AppleId,
    pub key_id: AppleId,
    pub key: ApnsKey,
}

/// What the relay needs to hand pushes to FCM.
#[derive(Clone, PartialEq)]
pub struct FcmConfig {
    pub endpoint: ServiceUrl,
    pub account: ServiceAccount,
}

/// Names the settings, never the keys.
impl fmt::Debug for RelayConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayConfig")
            .field("listen", &self.listen)
            .field("public_uri", &self.public_uri)
            .field("store_path", &self.store_path)
            .field("handle_lifetimes", &self.handle_lifetimes)
            .field("apps", &self.apps)
            .field("limits", &self.limits)
            .field("apns", &self.apns.as_ref().map(|apns| &apns.team_id))
            .field("fcm", &self.fcm.is_some())
            .finish_non_exhaustive()
    }
}

impl Section for RelayConfig {
    const NAME: &'static str = "push relay";

    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &SERVER_LISTEN,
            &SERVER_PUBLIC_URI,
            &SERVER_TRUSTED_PROXIES,
            &SERVER_UPSTREAM_TIMEOUT_MS,
            &STORE_PATH,
            &HANDLES_TTL_DAYS,
            &HANDLES_UNUSED_TTL_DAYS,
            &HANDLES_APPS,
            &LIMITS_PER_SENDER_PER_MINUTE,
            &LIMITS_PER_HANDLE_PER_MINUTE,
            &LIMITS_REGISTRATIONS_PER_MINUTE,
            &APNS_ENDPOINT,
            &APNS_TEAM_ID,
            &APNS_KEY_ID,
            &APNS_KEY,
            &FCM_ENDPOINT,
            &FCM_CREDENTIALS,
        ]
    }

    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        let mut errors = Vec::new();
        let apns = apns(snapshot, &mut errors);
        let fcm = fcm(snapshot, &mut errors);
        let apps = snapshot.get(&HANDLES_APPS).0;
        for (name, app) in apps.iter() {
            if app.apns_topic.is_some() && apns.is_none() {
                errors.push(FieldError::new(
                    HANDLES_APPS.key,
                    format!(
                        "{name} has an apns_topic, but {} isn't set: without the APNs key nothing reaches APNs.",
                        APNS_KEY.env_var
                    ),
                ));
            }
            if app.fcm_project_id.is_some() && fcm.is_none() {
                errors.push(FieldError::new(
                    HANDLES_APPS.key,
                    format!(
                        "{name} has an fcm_project_id, but {} isn't set: without a service account nothing reaches FCM.",
                        FCM_CREDENTIALS.env_var
                    ),
                ));
            }
        }
        let (ttl_days, unused_ttl_days) = (
            snapshot.get(&HANDLES_TTL_DAYS),
            snapshot.get(&HANDLES_UNUSED_TTL_DAYS),
        );
        if unused_ttl_days > ttl_days {
            errors.push(FieldError::new(
                HANDLES_UNUSED_TTL_DAYS.key,
                format!(
                    "is {unused_ttl_days}, more than {} ({ttl_days}): a handle no push has reached can't outlast one that has.",
                    HANDLES_TTL_DAYS.key
                ),
            ));
        }
        // Checked by the setting itself.
        let trusted_proxies =
            TrustedProxies::parse(&snapshot.get(&SERVER_TRUSTED_PROXIES)).unwrap_or_default();
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(RelayConfig {
            listen: snapshot.get(&SERVER_LISTEN).0,
            public_uri: snapshot.get(&SERVER_PUBLIC_URI),
            trusted_proxies,
            upstream_timeout: Duration::from_millis(snapshot.get(&SERVER_UPSTREAM_TIMEOUT_MS)),
            store_path: snapshot.get(&STORE_PATH),
            handle_lifetimes: Lifetimes {
                unused: days(unused_ttl_days),
                delivered: days(ttl_days),
            },
            apps,
            limits: LimitsConfig {
                per_sender: snapshot.get(&LIMITS_PER_SENDER_PER_MINUTE),
                per_handle: snapshot.get(&LIMITS_PER_HANDLE_PER_MINUTE),
                registrations: snapshot.get(&LIMITS_REGISTRATIONS_PER_MINUTE),
            },
            apns,
            fcm,
        })
    }
}

/// APNs, when its key is set: the key needs both ids, and the ids mean
/// nothing without the key.
fn apns(snapshot: &Snapshot, errors: &mut Vec<FieldError>) -> Option<ApnsConfig> {
    let key = snapshot.get(&APNS_KEY);
    let team_id = snapshot.get(&APNS_TEAM_ID);
    let key_id = snapshot.get(&APNS_KEY_ID);
    if key.is_empty() {
        for (setting, given) in [
            (APNS_TEAM_ID.key, team_id.is_some()),
            (APNS_KEY_ID.key, key_id.is_some()),
        ] {
            if given {
                errors.push(FieldError::new(
                    setting,
                    format!(
                        "is set, but {} isn't: set the key too, or leave the APNs settings out.",
                        APNS_KEY.env_var
                    ),
                ));
            }
        }
        return None;
    }
    let missing = |setting: &str| {
        FieldError::new(
            setting,
            format!(
                "is required with {}: the key's tokens name it.",
                APNS_KEY.env_var
            ),
        )
    };
    let (Some(team_id), Some(key_id)) = (team_id.clone(), key_id.clone()) else {
        if team_id.is_none() {
            errors.push(missing(APNS_TEAM_ID.key));
        }
        if key_id.is_none() {
            errors.push(missing(APNS_KEY_ID.key));
        }
        return None;
    };
    match ApnsKey::parse(&key) {
        Ok(key) => Some(ApnsConfig {
            endpoint: snapshot.get(&APNS_ENDPOINT),
            team_id,
            key_id,
            key,
        }),
        Err(message) => {
            errors.push(FieldError::new(APNS_KEY.key, message));
            None
        }
    }
}

/// FCM, when its service account is set.
fn fcm(snapshot: &Snapshot, errors: &mut Vec<FieldError>) -> Option<FcmConfig> {
    let credentials = snapshot.get(&FCM_CREDENTIALS);
    if credentials.is_empty() {
        return None;
    }
    match ServiceAccount::parse(credentials.expose()) {
        Ok(account) => Some(FcmConfig {
            endpoint: snapshot.get(&FCM_ENDPOINT),
            account,
        }),
        Err(error) => {
            errors.push(FieldError::new(FCM_CREDENTIALS.key, error.to_string()));
            None
        }
    }
}

fn days(days: u32) -> Duration {
    Duration::from_secs(u64::from(days) * 86_400)
}

/// `monokulo-push-relay`'s own logging, read before anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggingConfig {
    pub level: String,
    pub format: telemetry::LogFormat,
}

impl Section for LoggingConfig {
    const NAME: &'static str = "logging";

    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&LOGGING_LEVEL, &LOGGING_FORMAT]
    }

    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(LoggingConfig {
            level: snapshot.get(&LOGGING_LEVEL),
            format: snapshot.get(&LOGGING_FORMAT),
        })
    }
}

fn check_trusted_proxies(value: &str) -> Result<(), String> {
    TrustedProxies::parse(value).map(|_| ())
}

fn check_apns_key(value: &Secret) -> Result<(), String> {
    if value.is_empty() {
        return Ok(());
    }
    ApnsKey::parse(value).map(|_| ())
}

fn check_fcm_credentials(value: &Secret) -> Result<(), String> {
    if value.is_empty() {
        return Ok(());
    }
    ServiceAccount::parse(value.expose())
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// An address the relay is reached at or reaches: `https`, or `http` to
/// this machine (`127.0.0.0/8`, `[::1]`, `localhost`, where a test runs a
/// stand-in), never an onion address or with a user name in it. A push
/// endpoint's rule ([`push::Endpoint`]): APNs and FCM are reached with a
/// bearer token, and the relay's own address is where pushes are signed
/// for, so none of them is ever plain `http` across a network.
#[derive(Clone, PartialEq, Eq)]
pub struct ServiceUrl(Endpoint);

impl ServiceUrl {
    pub fn url(&self) -> &url::Url {
        self.0.as_url()
    }

    /// `path` (starting with `/`) under this address, without doubling
    /// the slash a bare host has.
    pub fn join(&self, path: &str) -> String {
        format!("{}{path}", self.0.as_str().trim_end_matches('/'))
    }
}

impl fmt::Debug for ServiceUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ServiceUrl({})", self.0.as_str())
    }
}

impl SettingValue for ServiceUrl {
    fn parse(raw: &str) -> Result<Self, String> {
        Endpoint::parse(raw.trim())
            .map(ServiceUrl)
            .map_err(|error| {
                format!("Enter an https address, like https://example.com: this one is {error}.")
            })
    }

    fn render(&self) -> String {
        self.0.as_str().to_owned()
    }

    fn kind() -> SettingKind {
        SettingKind::Url
    }
}

/// This relay's own address, `server.public_uri`: a [`ServiceUrl`] that is
/// an origin alone. Pushes are signed for its origin (the VAPID `aud`),
/// and its endpoints are `{origin}/v1/push/{handle}`, so a path in it
/// would mean nothing (a reverse proxy serving the relay under a path
/// would need it stripped, and the signature would still be for the
/// origin).
#[derive(Clone, PartialEq, Eq)]
pub struct PublicUri {
    origin: Origin,
}

impl PublicUri {
    /// The audience a push to this relay must be signed for.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }
}

impl fmt::Debug for PublicUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicUri({})", self.origin)
    }
}

impl SettingValue for PublicUri {
    fn parse(raw: &str) -> Result<Self, String> {
        let url = ServiceUrl::parse(raw)?;
        let url = url.url();
        let origin = Origin::of(url).map_err(|error| error.to_string())?;
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err(format!(
                "Enter the relay's address alone, like {origin}: no path, the relay's routes are at its root."
            ));
        }
        Ok(PublicUri { origin })
    }

    fn render(&self) -> String {
        self.origin.as_str().to_owned()
    }

    fn kind() -> SettingKind {
        SettingKind::Url
    }
}

/// An Apple Developer team or key ID: 10 capital letters and digits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppleId(String);

impl AppleId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AppleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl SettingValue for AppleId {
    fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.len() == 10
            && raw
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            Ok(AppleId(raw.to_owned()))
        } else {
            Err("Enter the 10 capital letters and digits Apple shows, like ABCDE12345.".to_owned())
        }
    }

    fn render(&self) -> String {
        self.0.clone()
    }

    fn kind() -> SettingKind {
        SettingKind::Text
    }
}

/// The APNs signing key, read once. Compared by its PEM text, which is
/// what the setting holds.
#[derive(Clone)]
pub struct ApnsKey {
    pem: Secret,
    key: Arc<Es256Key>,
}

impl ApnsKey {
    fn parse(pem: &Secret) -> Result<Self, String> {
        let key = Es256Key::from_pkcs8_pem(pem.expose()).map_err(|_| {
            "Give the whole AuthKey_<id>.p8 file, from -----BEGIN PRIVATE KEY----- to -----END PRIVATE KEY-----: this isn't a P-256 key in that form.".to_owned()
        })?;
        Ok(ApnsKey {
            pem: pem.clone(),
            key: Arc::new(key),
        })
    }

    pub(crate) fn signer(&self) -> &Es256Key {
        &self.key
    }
}

impl PartialEq for ApnsKey {
    fn eq(&self, other: &Self) -> bool {
        self.pem == other.pem
    }
}

/// The apps devices may register for (`handles.apps`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Apps(BTreeMap<AppName, AppConfig>);

impl Apps {
    pub fn get(&self, name: &AppName) -> Option<&AppConfig> {
        self.0.get(name)
    }

    fn iter(&self) -> impl Iterator<Item = (&AppName, &AppConfig)> {
        self.0.iter()
    }
}

/// Where one app's pushes go: its iOS bundle ID for APNs, its Firebase
/// project for FCM. A field it doesn't know stops the relay rather than
/// being ignored, so a misspelt one isn't a platform silently missing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apns_topic: Option<BundleId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fcm_project_id: Option<ProjectId>,
}

/// An iOS bundle ID (the APNs topic): reverse-DNS, letters, digits,
/// hyphens and dots, at most 155 characters, as Apple allows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BundleId(String);

impl BundleId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BundleId {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let fits = (1..=155).contains(&text.len())
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
            && text.split('.').all(|part| !part.is_empty());
        if fits {
            Ok(BundleId(text))
        } else {
            Err(format!(
                "{text:?} isn't a bundle ID (letters, digits, hyphens and dots, like com.example.app)"
            ))
        }
    }
}

impl From<BundleId> for String {
    fn from(id: BundleId) -> Self {
        id.0
    }
}

/// A Firebase (Google Cloud) project ID: 6 to 30 lowercase letters, digits
/// and hyphens, starting with a letter and not ending with a hyphen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectId(String);

impl ProjectId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ProjectId {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let bytes = text.as_bytes();
        let fits = (6..=30).contains(&bytes.len())
            && bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes.last().is_some_and(|byte| *byte != b'-')
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');
        if fits {
            Ok(ProjectId(text))
        } else {
            Err(format!(
                "{text:?} isn't a Firebase project ID (6 to 30 lowercase letters, digits and hyphens, starting with a letter)"
            ))
        }
    }
}

impl From<ProjectId> for String {
    fn from(id: ProjectId) -> Self {
        id.0
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The APNs key the tests sign with (`tests/fixtures`): a throwaway
    /// P-256 key in the form of Apple's `AuthKey_<id>.p8`.
    pub(crate) const APNS_KEY_PEM: &str = include_str!("../tests/fixtures/apns-test.p8");

    /// A throwaway service account in the form Google hands out, around
    /// the RSA key in `tests/fixtures`, getting its tokens at `token_uri`.
    /// Made here rather than kept as a file: a file that looks like a
    /// real service account is refused by the repository's secret
    /// scanning.
    pub(crate) fn fcm_account(token_uri: &str) -> String {
        serde_json::json!({
            "type": "service_account",
            "project_id": "monokular-test",
            "private_key_id": "0123456789abcdef0123456789abcdef01234567",
            "private_key": include_str!("../tests/fixtures/fcm-test-key.pem"),
            "client_email": "relay@monokular-test.iam.gserviceaccount.com",
            "client_id": "100000000000000000000",
            "token_uri": token_uri,
        })
        .to_string()
    }

    /// Google's token endpoint, as a real service account names it.
    pub(crate) const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

    /// The settings from `values` (as the options file holds them) and
    /// `vars` (the environment), as a process loads them.
    pub(crate) async fn load_with(
        values: &[(&str, &str)],
        vars: &[(&str, &str)],
        host: Host,
    ) -> Result<RelayConfig, LoadError> {
        let stored = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        load(stored, Env::fixed(vars.iter().copied()), host).await
    }

    pub(crate) fn apns_config() -> ApnsConfig {
        ApnsConfig {
            endpoint: ServiceUrl::parse("http://127.0.0.1:1").unwrap(),
            team_id: AppleId::parse("TEAM123456").unwrap(),
            key_id: AppleId::parse("KEY1234567").unwrap(),
            key: ApnsKey::parse(&Secret::new(APNS_KEY_PEM)).unwrap(),
        }
    }

    /// FCM, with its token endpoint at an address that can't be reached.
    pub(crate) fn fcm_config() -> FcmConfig {
        let token_uri = format!("http://{}/token", service_runtime::unreachable::address());
        FcmConfig {
            endpoint: ServiceUrl::parse("http://127.0.0.1:1").unwrap(),
            account: ServiceAccount::parse(&fcm_account(&token_uri)).unwrap(),
        }
    }

    /// Options file values, environment variables, and what the error
    /// says.
    type Case<'a> = (&'a [(&'a str, &'a str)], &'a [(&'a str, &'a str)], &'a str);

    const APPS: &str =
        r#"{"monokular":{"apns_topic":"com.example.monokular","fcm_project_id":"monokular-test"}}"#;

    #[tokio::test]
    async fn the_defaults_load_and_run_nothing_upstream() {
        let config = load_with(&[], &[], Host::Standalone).await.unwrap();
        assert_eq!(config.listen, "0.0.0.0:8444".parse().unwrap());
        assert_eq!(config.public_uri, None);
        assert_eq!(
            config.handle_lifetimes,
            Lifetimes {
                unused: Duration::from_secs(7 * 86_400),
                delivered: Duration::from_secs(90 * 86_400),
            }
        );
        assert_eq!(
            config.limits,
            LimitsConfig {
                per_sender: 600,
                per_handle: 60,
                registrations: 30
            }
        );
        assert!(config.apns.is_none() && config.fcm.is_none());
        assert!(
            config.store_path.ends_with("push-relay.db"),
            "{:?}",
            config.store_path
        );
    }

    #[tokio::test]
    async fn apps_and_their_keys_load_together() {
        let config = load_with(
            &[
                ("server.public_uri", "https://relay.example.org"),
                ("handles.apps", APPS),
                ("apns.team_id", "TEAM123456"),
                ("apns.key_id", "KEY1234567"),
            ],
            &[
                ("MONOKULO_RELAY_APNS_KEY", APNS_KEY_PEM),
                (
                    "MONOKULO_RELAY_FCM_CREDENTIALS",
                    &fcm_account(GOOGLE_TOKEN_URI),
                ),
            ],
            Host::Embedded,
        )
        .await
        .unwrap();
        assert_eq!(
            config.public_uri.unwrap().origin().as_str(),
            "https://relay.example.org"
        );
        let app = config
            .apps
            .get(&AppName::try_from("monokular".to_owned()).unwrap())
            .unwrap();
        assert_eq!(
            app.apns_topic.as_ref().unwrap().as_str(),
            "com.example.monokular"
        );
        assert_eq!(
            app.fcm_project_id.as_ref().unwrap().as_str(),
            "monokular-test"
        );
        let apns = config.apns.unwrap();
        assert_eq!(apns.team_id.as_str(), "TEAM123456");
        assert_eq!(apns.endpoint.url().as_str(), "https://api.push.apple.com/");
        assert!(config.fcm.is_some());
    }

    /// `handles.apps` as the docs and `--init` write it, a TOML table per
    /// app, read into the apps with their bundle and project.
    #[tokio::test]
    async fn apps_written_as_a_toml_table_are_read() {
        let file = live_settings::OptionsFile::in_memory(
            "[server]\npublic_uri = \"https://relay.example.org\"\n\n\
             [apns]\nteam_id = \"TEAM123456\"\nkey_id = \"KEY1234567\"\n\n\
             [handles.apps.monokular]\n\
             apns_topic = \"com.example.monokular\"\n\
             fcm_project_id = \"monokular-test\"\n",
        );
        let stored = file.read(ALL).unwrap();
        let config = load(
            stored,
            Env::fixed([
                ("MONOKULO_RELAY_APNS_KEY", APNS_KEY_PEM),
                (
                    "MONOKULO_RELAY_FCM_CREDENTIALS",
                    &*fcm_account(GOOGLE_TOKEN_URI),
                ),
            ]),
            Host::Standalone,
        )
        .await
        .unwrap();
        let app = config
            .apps
            .get(&AppName::try_from("monokular".to_owned()).unwrap())
            .unwrap();
        assert_eq!(
            app.apns_topic.as_ref().map(BundleId::as_str),
            Some("com.example.monokular")
        );
        assert_eq!(
            app.fcm_project_id.as_ref().map(ProjectId::as_str),
            Some("monokular-test")
        );
    }

    /// Each rule that ties settings together stops the relay with the
    /// setting named.
    #[tokio::test]
    async fn settings_that_need_each_other_are_refused_alone() {
        let key = [("MONOKULO_RELAY_APNS_KEY", APNS_KEY_PEM)];
        let cases: &[Case] = &[
            (
                &[("apns.team_id", "TEAM123456")],
                &key,
                "apns.key_id: is required",
            ),
            (
                &[("apns.key_id", "KEY1234567")],
                &key,
                "apns.team_id: is required",
            ),
            (
                &[("apns.team_id", "TEAM123456")],
                &[],
                "apns.team_id: is set, but MONOKULO_RELAY_APNS_KEY isn't",
            ),
            (
                &[("handles.apps", APPS)],
                &[],
                "monokular has an apns_topic, but MONOKULO_RELAY_APNS_KEY isn't set",
            ),
            (
                &[("handles.apps", APPS)],
                &[],
                "monokular has an fcm_project_id, but MONOKULO_RELAY_FCM_CREDENTIALS isn't set",
            ),
        ];
        for (values, vars, expected) in cases {
            let error = load_with(values, vars, Host::Standalone)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{expected}: {error}");
        }
    }

    /// A value that doesn't read stops the relay, wherever it came from.
    #[tokio::test]
    async fn invalid_values_stop_the_relay() {
        let cases: &[Case] = &[
            (
                &[("handles.apps", "{\"monokular\":{\"apns_topik\":\"x\"}}")],
                &[],
                "handles.apps",
            ),
            (
                &[("handles.apps", "{\"Monokular\":{}}")],
                &[],
                "handles.apps",
            ),
            (
                &[("handles.apps", "{\"m\":{\"fcm_project_id\":\"UPPER\"}}")],
                &[],
                "handles.apps",
            ),
            (&[("handles.apps", "[]")], &[], "handles.apps"),
            (
                &[("server.public_uri", "http://relay.example.org")],
                &[],
                "server.public_uri",
            ),
            (
                &[("server.public_uri", "https://relay.example.org/relay")],
                &[],
                "server.public_uri",
            ),
            (
                &[("server.public_uri", "https://abc.onion")],
                &[],
                "server.public_uri",
            ),
            (
                &[("apns.endpoint", "http://api.push.apple.com")],
                &[],
                "apns.endpoint",
            ),
            (&[("apns.team_id", "abc")], &[], "apns.team_id"),
            (
                &[("handles.unused_ttl_days", "100")],
                &[],
                "handles.unused_ttl_days: is 100, more than handles.ttl_days (90)",
            ),
            (
                &[("server.trusted_proxies", "a.b.c.d")],
                &[],
                "server.trusted_proxies",
            ),
            (
                &[],
                &[("MONOKULO_RELAY_APNS_KEY", "not a key")],
                "MONOKULO_RELAY_APNS_KEY",
            ),
            (
                &[],
                &[("MONOKULO_RELAY_FCM_CREDENTIALS", "{}")],
                "MONOKULO_RELAY_FCM_CREDENTIALS",
            ),
        ];
        for (values, vars, expected) in cases {
            let error = load_with(values, vars, Host::Standalone)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{expected}: {error}");
        }
    }

    /// A key file the environment holds encoded as a JSON string once more
    /// stops the relay with an error that says so and quotes none of it.
    #[tokio::test]
    async fn a_service_account_encoded_twice_stops_the_relay_without_its_key() {
        let twice = serde_json::to_string(&fcm_account(GOOGLE_TOKEN_URI)).unwrap();
        let error = load_with(
            &[],
            &[("MONOKULO_RELAY_FCM_CREDENTIALS", &twice)],
            Host::Standalone,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("encoded as JSON twice"), "{error}");
        assert!(!error.contains("PRIVATE KEY"), "{error}");
        assert!(!error.contains("relay@monokular-test"), "{error}");
    }

    /// The public address may be http only on this machine, and is read as
    /// its origin.
    #[test]
    fn the_public_address_is_an_https_origin() {
        let uri = PublicUri::parse("https://Relay.Example.org:443/").unwrap();
        assert_eq!(uri.origin().as_str(), "https://relay.example.org");
        assert_eq!(uri.render(), "https://relay.example.org");
        assert_eq!(PublicUri::parse(&uri.render()), Ok(uri));
        let local = PublicUri::parse("http://127.0.0.1:8444").unwrap();
        assert_eq!(local.origin().as_str(), "http://127.0.0.1:8444");
        for bad in [
            "relay.example.org",
            "https://relay.example.org/?a=1",
            "https://u:p@relay.example.org",
        ] {
            assert!(PublicUri::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_embedded_relay_has_no_logging_of_its_own() {
        let embedded: Vec<&str> = embedded_settings().iter().map(|s| s.key()).collect();
        assert!(!embedded.contains(&"logging.level"));
        assert!(embedded.contains(&"server.listen"));
        assert_eq!(embedded.len(), ALL.len() - STANDALONE_ONLY.len());
        // Every key has two parts: inside monokulo only tables under
        // [relay] are the relay's.
        assert!(ALL.iter().all(|setting| setting.key().contains('.')));
    }
}
