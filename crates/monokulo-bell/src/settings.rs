//! Every Bell setting, declared once with monokulo's `live-settings`
//! library: read from the options file (`bell.toml`, `--options`) and the
//! `--bell-…` command-line options, which win; the Square webhook key and
//! the VAPID key from the environment only. Anything invalid stops the
//! Bell before it listens ([`load`]), every problem named.
//!
//! The Bell reads its settings once, at start: nothing here reloads.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use abuse::TrustedProxies;
use live_settings::{
    AnySetting, BindAddr, Env, FieldError, MemoryStore, Registry, Secret, Section, SettingKind,
    SettingValue, Snapshot, settings,
};
use push::{Contact, Endpoint, VapidKey};
use tracing_subscriber::EnvFilter;

use crate::square::WebhookKey;

settings! {
    LISTEN: BindAddr {
        key: "bell.listen",
        default: live_settings::parsed_default("127.0.0.1:8445"),
        description: "The address and port the Bell listens on. On this machine only by default: put it behind a reverse proxy that serves bell.public_uri over https.",
        example: "127.0.0.1:8445",
        applies: Restart,
    },
    PUBLIC_URI: Option<PublicUri> {
        key: "bell.public_uri",
        default: None,
        description: "The https address Square and the tills reach this Bell at, such as https://bell.example.org. Square's webhook subscription must point at it followed by /v1/square/webhook, exactly: Square signs each webhook for that URL. Required to run the Bell.",
        example: "https://bell.example.org",
        applies: Restart,
    },
    TRUSTED_PROXIES: String {
        key: "bell.trusted_proxies",
        default: "127.0.0.1, ::1".to_owned(),
        check: |value: &String| TrustedProxies::parse(value).map(|_| ()),
        description: "Addresses or CIDR ranges of the reverse proxies in front of the Bell, separated by commas. A registration from one of them is counted against the address its X-Forwarded-For names, not the proxy's. This machine by default, as the Bell listens on it alone; empty when nothing is in front.",
        example: "127.0.0.1, 10.0.0.0/8",
        applies: Restart,
    },
    STORE_PATH: PathBuf {
        key: "bell.store_path",
        default: live_settings::paths::data_file("bell.db").unwrap_or_else(|| PathBuf::from("bell.db")),
        description: "The Bell's database file, ~/.local/share/monokulo/bell.db unless set (or bell.db in the working directory if that can't be used). It holds the tills' registrations alone: never a webhook, never a Square token.",
        example: "/var/lib/monokular/bell.db",
        applies: Restart,
    },
    TIMEOUT_MS: u64 {
        key: "bell.timeout_ms",
        default: 10_000,
        check: range(100, 120_000),
        description: "Milliseconds the Bell waits for Square (checking a registration's location) or a push service (taking a ding) to answer.",
        example: "10000",
        applies: Restart,
    },
    REGISTRATIONS_PER_MINUTE: u32 {
        key: "bell.registrations_per_minute",
        default: 30,
        check: range(1, 1_000_000),
        description: "Registrations a minute from any one address (IPv6 by its /64). Each costs a request to Square.",
        example: "30",
        applies: Restart,
    },
    REGISTRATION_FAILURE_LIMIT: u32 {
        key: "bell.registration_failure_limit",
        default: 20,
        check: range(1, 1000),
        description: "Dings in a row a till's push service may fail to take before its registration is deleted. One it says is gone (404 or 410) is deleted at once.",
        example: "20",
        applies: Restart,
    },
    DINGS_PER_MERCHANT_PER_MINUTE: u32 {
        key: "bell.dings_per_merchant_per_minute",
        default: 120,
        check: range(1, 1_000_000),
        description: "Dings one merchant's tills are sent a minute, across all its locations: a payment dings each till at its location once. The rest of the minute's dings are dropped, as a ding a minute late is worth nothing and the till's timer asks Square anyway. Every merchant's dings are signed with the Bell's one VAPID key, which a push relay counts as one sender, so keep it well below the relay's per-sender limit (600 a minute by default), or one busy merchant spends the relay's budget for all of them.",
        example: "120",
        applies: Restart,
    },
    ALLOW_PRIVATE_ENDPOINTS: bool {
        key: "bell.allow_private_endpoints",
        default: false,
        description: "Whether a till may register a push endpoint on a loopback, private or link-local address, such as a push service on the Bell's own network. Off, every ding goes to a public address only, so a registration can't make the Bell reach inside its network. Never an onion address.",
        example: "false",
        applies: Restart,
    },
    SQUARE_APPLICATION_ID: Option<ApplicationId> {
        key: "bell.square.application_id",
        default: None,
        description: "The Square application whose webhooks this Bell receives, and which the tills using it sign in through: sq0idp-… in production, sandbox-sq0idb-… in the sandbox. Required to run the Bell.",
        example: "sq0idp-Monokular0Bell0Example",
        applies: Restart,
    },
    SQUARE_ENVIRONMENT: SquareEnvironment {
        key: "bell.square.environment",
        default: SquareEnvironment::Production,
        description: "Which Square the application is in: production, or sandbox for testing.",
        example: "sandbox",
        applies: Restart,
    },
    SQUARE_API_URL: Option<ServiceUrl> {
        key: "bell.square.api_url",
        default: None,
        description: "Where Square's API is, instead of the environment's own (https://connect.squareup.com, or https://connect.squareupsandbox.com in the sandbox): for a stand-in Square while developing. https, or http on this machine.",
        example: "http://127.0.0.1:8738",
        applies: Restart,
    },
    SQUARE_WEBHOOK_KEY: Secret {
        key: "bell.square.webhook_key",
        env: "MONOKULAR_BELL_SQUARE_WEBHOOK_KEY",
        default: Secret::default(),
        description: "The signature key of the Square application's webhook subscription, from the Developer Console's Webhooks page: a webhook whose signature it doesn't check is answered 401 and dropped.",
        applies: Restart,
        sources: [Env],
        required: true,
    },
    VAPID_KEY: Secret {
        key: "bell.vapid_key",
        env: "MONOKULAR_BELL_VAPID_KEY",
        default: Secret::default(),
        check: check_vapid_key,
        description: "The Bell's VAPID private key (RFC 8292), which signs every ding: 32 bytes as 64 hex digits, such as `openssl rand -hex 32` makes. Its public key is what the tills' push subscriptions are made for (GET /v1/info), so keep it: a new key leaves every registration's subscription refusing the Bell's dings.",
        applies: Restart,
        sources: [Env],
        required: true,
    },
    LOGGING_LEVEL: String {
        key: "logging.level",
        default: "info".to_owned(),
        check: |level: &String| check_level(level),
        description: "Which log lines the Bell writes: a level (error, warn, info, debug, trace), optionally followed by target=level pairs. No line carries a webhook, a Square token or a push endpoint, at any level.",
        example: "info,monokular_bell=debug",
        applies: Restart,
    },
}

/// Why the settings can't be used: every problem, one per line.
#[derive(Debug, thiserror::Error)]
#[error("the Bell's settings: {0}")]
pub struct LoadError(String);

/// The Bell's settings from `stored` (what its options file holds, by key,
/// already checked line by line) and `env` (the command line and the
/// environment). Every value is checked again where it came from, then
/// together ([`BellConfig`]'s rules); anything wrong is the error, never a
/// default in its place.
pub async fn load(stored: HashMap<String, String>, env: Env) -> Result<BellConfig, LoadError> {
    let store = Arc::new(MemoryStore::with(stored));
    let mut builder = Registry::builder_with_env(store, ALL, env).await;
    let bell = builder.section::<BellConfig>();
    // Read before the registry (logging starts first); registered so a bad
    // value there stops the Bell like any other.
    builder.section::<LoggingConfig>();
    builder
        .build()
        .map_err(|error| LoadError(error.to_string()))?;
    Ok((*bell.load()).clone())
}

/// The settings the Bell runs on, checked together.
///
/// The address, the Square application and the two keys are `None` only
/// in the section every setting at its default makes, which the settings
/// library builds as its fallback: a Bell is never started on it, as
/// [`load`] refuses the unset keys and [`crate::Bell::start`] the unset
/// address and application.
#[derive(Clone, PartialEq)]
pub struct BellConfig {
    pub listen: SocketAddr,
    pub public_uri: Option<PublicUri>,
    pub trusted_proxies: TrustedProxies,
    pub store_path: PathBuf,
    /// How long Square and the push services get to answer.
    pub timeout: Duration,
    pub registrations_per_minute: u32,
    pub registration_failure_limit: u32,
    pub dings_per_merchant_per_minute: u32,
    pub allow_private_endpoints: bool,
    pub square: SquareConfig,
    pub webhook_key: Option<WebhookKey>,
    pub vapid_key: Option<VapidSecret>,
}

/// The Square application the Bell serves, and where its API is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SquareConfig {
    pub application_id: Option<ApplicationId>,
    pub environment: SquareEnvironment,
    /// The environment's API, or `bell.square.api_url` when set.
    pub api_url: ServiceUrl,
}

/// Names the settings, never the keys.
impl fmt::Debug for BellConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BellConfig")
            .field("listen", &self.listen)
            .field("public_uri", &self.public_uri)
            .field("store_path", &self.store_path)
            .field("timeout", &self.timeout)
            .field("square", &self.square)
            .field(
                "vapid_public_key",
                &self.vapid_key.as_ref().map(|key| key.key().public()),
            )
            .finish_non_exhaustive()
    }
}

impl Section for BellConfig {
    const NAME: &'static str = "bell";

    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &LISTEN,
            &PUBLIC_URI,
            &TRUSTED_PROXIES,
            &STORE_PATH,
            &TIMEOUT_MS,
            &REGISTRATIONS_PER_MINUTE,
            &REGISTRATION_FAILURE_LIMIT,
            &DINGS_PER_MERCHANT_PER_MINUTE,
            &ALLOW_PRIVATE_ENDPOINTS,
            &SQUARE_APPLICATION_ID,
            &SQUARE_ENVIRONMENT,
            &SQUARE_API_URL,
            &SQUARE_WEBHOOK_KEY,
            &VAPID_KEY,
        ]
    }

    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        let environment = snapshot.get(&SQUARE_ENVIRONMENT);
        let application_id = snapshot.get(&SQUARE_APPLICATION_ID);
        if let Some(id) = &application_id
            && id.environment() != environment
        {
            return Err(vec![FieldError::new(
                SQUARE_APPLICATION_ID.key,
                format!(
                    "is a {} application's, but {} is {}: Square sends its webhooks from the application's own environment.",
                    id.environment().as_str(),
                    SQUARE_ENVIRONMENT.key,
                    environment.as_str()
                ),
            )]);
        }
        let webhook_key = snapshot.get(&SQUARE_WEBHOOK_KEY);
        let vapid_key = snapshot.get(&VAPID_KEY);
        Ok(BellConfig {
            listen: snapshot.get(&LISTEN).0,
            public_uri: snapshot.get(&PUBLIC_URI),
            // Checked by the setting itself.
            trusted_proxies: TrustedProxies::parse(&snapshot.get(&TRUSTED_PROXIES))
                .unwrap_or_default(),
            store_path: snapshot.get(&STORE_PATH),
            timeout: Duration::from_millis(snapshot.get(&TIMEOUT_MS)),
            registrations_per_minute: snapshot.get(&REGISTRATIONS_PER_MINUTE),
            registration_failure_limit: snapshot.get(&REGISTRATION_FAILURE_LIMIT),
            dings_per_merchant_per_minute: snapshot.get(&DINGS_PER_MERCHANT_PER_MINUTE),
            allow_private_endpoints: snapshot.get(&ALLOW_PRIVATE_ENDPOINTS),
            square: SquareConfig {
                application_id,
                environment,
                api_url: snapshot
                    .get(&SQUARE_API_URL)
                    .unwrap_or_else(|| environment.api_url()),
            },
            // Both are required, so empty only at their defaults.
            webhook_key: (!webhook_key.is_empty()).then(|| WebhookKey::new(webhook_key)),
            vapid_key: VapidSecret::parse(&vapid_key).ok(),
        })
    }
}

/// `monokular-bell`'s logging, read before anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggingConfig {
    pub level: String,
}

impl Section for LoggingConfig {
    const NAME: &'static str = "logging";

    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&LOGGING_LEVEL]
    }

    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(LoggingConfig {
            level: snapshot.get(&LOGGING_LEVEL),
        })
    }
}

fn check_level(level: &str) -> Result<(), String> {
    EnvFilter::builder()
        .parse(level.trim())
        .map(|_| ())
        .map_err(|error| {
            format!("Not a log filter ({error}). Use a level (error, warn, info, debug, trace), optionally followed by target=level pairs, such as info,monokular_bell=debug.")
        })
}

fn check_vapid_key(value: &Secret) -> Result<(), String> {
    VapidSecret::parse(value).map(|_| ())
}

/// The Bell's VAPID key, read once from its 64 hex digits. Compared by
/// that text, which is what the setting holds.
#[derive(Clone)]
pub struct VapidSecret {
    hex: Secret,
    key: Arc<VapidKey>,
}

impl VapidSecret {
    fn parse(value: &Secret) -> Result<Self, String> {
        let wrong = || {
            "Give 32 bytes as 64 hex digits, such as `openssl rand -hex 32` makes, of a valid P-256 private key.".to_owned()
        };
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(value.expose(), &mut bytes).map_err(|_| wrong())?;
        let key = VapidKey::from_bytes(&bytes).map_err(|_| wrong());
        zeroize::Zeroize::zeroize(&mut bytes);
        Ok(VapidSecret {
            hex: value.clone(),
            key: Arc::new(key?),
        })
    }

    pub fn key(&self) -> &Arc<VapidKey> {
        &self.key
    }
}

impl PartialEq for VapidSecret {
    fn eq(&self, other: &Self) -> bool {
        self.hex == other.hex
    }
}

/// An address the Bell reaches or is reached at: `https`, or `http` to
/// this machine (`127.0.0.0/8`, `[::1]`, `localhost`, where a stand-in
/// runs), never an onion address or with a user name in it: a push
/// endpoint's rule ([`push::Endpoint`]). Square is called with a merchant's
/// token, and the Bell's own address is what webhooks are signed for, so
/// neither is ever plain `http` across a network.
#[derive(Clone, PartialEq, Eq)]
pub struct ServiceUrl(Endpoint);

impl ServiceUrl {
    /// `path` (starting with `/`) under this address, without doubling the
    /// slash a bare host has.
    pub fn join(&self, path: &str) -> String {
        format!("{}{path}", self.0.as_str().trim_end_matches('/'))
    }

    pub fn as_url(&self) -> &url::Url {
        self.0.as_url()
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

/// The Bell's own address, `bell.public_uri`: a [`ServiceUrl`] with no
/// query or fragment, kept without a trailing slash. A path is allowed
/// (a reverse proxy serving the Bell under one), and the webhook URL
/// Square signs is this followed by [`crate::square::WEBHOOK_PATH`].
#[derive(Clone, PartialEq, Eq)]
pub struct PublicUri {
    base: String,
}

impl PublicUri {
    /// The URL Square sends webhooks to, and signs them for.
    pub fn notification_url(&self) -> String {
        format!("{}{}", self.base, crate::square::WEBHOOK_PATH)
    }

    /// The `sub` of every VAPID token the Bell signs, how a push service's
    /// operator can reach whoever runs it: the address itself, when it is
    /// `https` (RFC 8292 §2.1 takes no `http` one).
    pub fn contact(&self) -> Option<Contact> {
        Contact::parse(&self.base).ok()
    }
}

impl fmt::Debug for PublicUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicUri({})", self.base)
    }
}

impl SettingValue for PublicUri {
    fn parse(raw: &str) -> Result<Self, String> {
        let url = ServiceUrl::parse(raw)?;
        let url = url.as_url();
        if url.query().is_some() || url.fragment().is_some() {
            return Err(
                "Enter the Bell's address without a query or fragment, like https://bell.example.org."
                    .to_owned(),
            );
        }
        Ok(PublicUri {
            base: url.as_str().trim_end_matches('/').to_owned(),
        })
    }

    fn render(&self) -> String {
        self.base.clone()
    }

    fn kind() -> SettingKind {
        SettingKind::Url
    }
}

live_settings::choice_value! {
    /// Which Square an application is in.
    pub enum SquareEnvironment { Production = "production", Sandbox = "sandbox" }
}

impl SquareEnvironment {
    /// The environment's API.
    pub fn api_url(self) -> ServiceUrl {
        // Both parse; the tests load each environment.
        live_settings::parsed_default(match self {
            SquareEnvironment::Production => "https://connect.squareup.com",
            SquareEnvironment::Sandbox => "https://connect.squareupsandbox.com",
        })
    }
}

/// A Square application's ID: `sq0idp-` (production) or `sandbox-sq0idb-`
/// (sandbox) and the letters, digits, `-` and `_` Square gives it. Public:
/// the tills sign in with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationId(String);

impl ApplicationId {
    const PRODUCTION: &'static str = "sq0idp-";
    const SANDBOX: &'static str = "sandbox-sq0idb-";

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The environment the application is in, by its prefix.
    pub fn environment(&self) -> SquareEnvironment {
        if self.0.starts_with(Self::SANDBOX) {
            SquareEnvironment::Sandbox
        } else {
            SquareEnvironment::Production
        }
    }
}

impl SettingValue for ApplicationId {
    fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        let rest = raw
            .strip_prefix(Self::PRODUCTION)
            .or_else(|| raw.strip_prefix(Self::SANDBOX));
        let fits = rest.is_some_and(|rest| {
            (1..=64).contains(&rest.len())
                && rest
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        });
        if fits {
            Ok(ApplicationId(raw.to_owned()))
        } else {
            Err("Enter the application ID the Developer Console shows: sq0idp-… for production, sandbox-sq0idb-… for the sandbox.".to_owned())
        }
    }

    fn render(&self) -> String {
        self.0.clone()
    }

    fn kind() -> SettingKind {
        SettingKind::Text
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A VAPID key the tests sign with: any 32 bytes below the curve's order.
    pub(crate) const VAPID_HEX: &str =
        "0101010101010101010101010101010101010101010101010101010101010101";

    /// The settings from `values` (as the options file holds them) and
    /// `vars` (the environment), as the Bell loads them.
    pub(crate) async fn load_with(
        values: &[(&str, &str)],
        vars: &[(&str, &str)],
    ) -> Result<BellConfig, LoadError> {
        let stored = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        load(stored, Env::fixed(vars.iter().copied())).await
    }

    const SECRETS: &[(&str, &str)] = &[
        (
            "MONOKULAR_BELL_SQUARE_WEBHOOK_KEY",
            "Ibxx_5AKakO-3qeNVR61Dw",
        ),
        ("MONOKULAR_BELL_VAPID_KEY", VAPID_HEX),
    ];

    #[tokio::test]
    async fn the_defaults_load_with_the_secrets() {
        let config = load_with(&[], SECRETS).await.unwrap();
        assert_eq!(config.listen, "127.0.0.1:8445".parse().unwrap());
        assert_eq!(config.public_uri, None);
        assert_eq!(config.timeout, Duration::from_secs(10));
        assert_eq!(config.registrations_per_minute, 30);
        assert_eq!(config.registration_failure_limit, 20);
        assert_eq!(config.dings_per_merchant_per_minute, 120);
        assert!(!config.allow_private_endpoints);
        assert!(
            config
                .trusted_proxies
                .contains("127.0.0.1".parse().unwrap())
        );
        assert!(config.trusted_proxies.contains("::1".parse().unwrap()));
        assert_eq!(config.square.environment, SquareEnvironment::Production);
        assert_eq!(
            config.square.api_url.as_url().as_str(),
            "https://connect.squareup.com/"
        );
        assert!(config.webhook_key.is_some());
        assert!(
            config.store_path.ends_with("bell.db"),
            "{:?}",
            config.store_path
        );
    }

    /// The VAPID key is the one the 64 hex digits give, and its public key
    /// follows from it.
    #[tokio::test]
    async fn the_vapid_key_is_read_from_its_hex() {
        let config = load_with(&[], SECRETS).await.unwrap();
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(VAPID_HEX, &mut bytes).unwrap();
        assert_eq!(
            config.vapid_key.unwrap().key().public(),
            VapidKey::from_bytes(&bytes).unwrap().public()
        );
    }

    /// The sandbox has its own API; `bell.square.api_url` wins over either.
    #[tokio::test]
    async fn the_square_api_follows_the_environment_unless_it_is_set() {
        let sandbox = load_with(
            &[
                ("bell.square.environment", "sandbox"),
                ("bell.square.application_id", "sandbox-sq0idb-Test"),
            ],
            SECRETS,
        )
        .await
        .unwrap();
        assert_eq!(
            sandbox.square.api_url.join("/v2/locations/L1"),
            "https://connect.squareupsandbox.com/v2/locations/L1"
        );
        let local = load_with(
            &[
                ("bell.square.environment", "sandbox"),
                ("bell.square.api_url", "http://127.0.0.1:8738"),
            ],
            SECRETS,
        )
        .await
        .unwrap();
        assert_eq!(
            local.square.api_url.join("/v2/locations/L1"),
            "http://127.0.0.1:8738/v2/locations/L1"
        );
        let production = load_with(
            &[("bell.square.api_url", "https://square.example.org/")],
            SECRETS,
        )
        .await
        .unwrap();
        assert_eq!(
            production.square.api_url.join("/v2/locations/L1"),
            "https://square.example.org/v2/locations/L1"
        );
    }

    /// Options file values, environment variables, and what the error says.
    type Case<'a> = (&'a [(&'a str, &'a str)], &'a [(&'a str, &'a str)], &'a str);

    /// A value that doesn't read, or a key that isn't set, stops the Bell,
    /// wherever it came from.
    #[tokio::test]
    async fn invalid_or_missing_values_stop_the_bell() {
        const ZERO_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";
        let not_hex = format!("{}zz", &VAPID_HEX[2..]);
        let cases: &[Case] = &[
            (&[], &[], "MONOKULAR_BELL_SQUARE_WEBHOOK_KEY must be set"),
            (&[], &[], "MONOKULAR_BELL_VAPID_KEY must be set"),
            (
                &[],
                &[
                    ("MONOKULAR_BELL_SQUARE_WEBHOOK_KEY", "k"),
                    ("MONOKULAR_BELL_VAPID_KEY", "abc"),
                ],
                "MONOKULAR_BELL_VAPID_KEY",
            ),
            (
                &[],
                &[
                    ("MONOKULAR_BELL_SQUARE_WEBHOOK_KEY", "k"),
                    // The curve's order: not a private key.
                    (
                        "MONOKULAR_BELL_VAPID_KEY",
                        "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551",
                    ),
                ],
                "MONOKULAR_BELL_VAPID_KEY",
            ),
            (
                &[("bell.public_uri", "http://bell.example.org")],
                SECRETS,
                "bell.public_uri",
            ),
            (
                &[("bell.public_uri", "https://bell.example.org/?a=1")],
                SECRETS,
                "bell.public_uri",
            ),
            (
                &[("bell.public_uri", "https://abc.onion")],
                SECRETS,
                "bell.public_uri",
            ),
            (
                &[("bell.square.api_url", "http://connect.squareup.com")],
                SECRETS,
                "bell.square.api_url",
            ),
            (
                &[("bell.square.application_id", "my-app")],
                SECRETS,
                "bell.square.application_id",
            ),
            (
                &[("bell.square.environment", "staging")],
                SECRETS,
                "bell.square.environment",
            ),
            (
                &[("bell.square.application_id", "sandbox-sq0idb-Test")],
                SECRETS,
                "is a sandbox application's, but bell.square.environment is production",
            ),
            (
                &[("bell.trusted_proxies", "a.b.c.d")],
                SECRETS,
                "bell.trusted_proxies",
            ),
            // Set, but to nothing: as unset.
            (
                &[],
                &[
                    ("MONOKULAR_BELL_SQUARE_WEBHOOK_KEY", ""),
                    ("MONOKULAR_BELL_VAPID_KEY", VAPID_HEX),
                ],
                "MONOKULAR_BELL_SQUARE_WEBHOOK_KEY",
            ),
            (
                &[],
                &[
                    ("MONOKULAR_BELL_SQUARE_WEBHOOK_KEY", "k"),
                    ("MONOKULAR_BELL_VAPID_KEY", ""),
                ],
                "MONOKULAR_BELL_VAPID_KEY",
            ),
            (
                &[],
                &[
                    ("MONOKULAR_BELL_SQUARE_WEBHOOK_KEY", "k"),
                    // Zero is no private key.
                    ("MONOKULAR_BELL_VAPID_KEY", ZERO_KEY),
                ],
                "MONOKULAR_BELL_VAPID_KEY",
            ),
            (
                &[],
                &[
                    ("MONOKULAR_BELL_SQUARE_WEBHOOK_KEY", "k"),
                    // 64 characters, not all hex.
                    ("MONOKULAR_BELL_VAPID_KEY", not_hex.as_str()),
                ],
                "MONOKULAR_BELL_VAPID_KEY",
            ),
            (&[("bell.timeout_ms", "0")], SECRETS, "bell.timeout_ms"),
            (&[("bell.timeout_ms", "120001")], SECRETS, "bell.timeout_ms"),
            (
                &[("bell.registration_failure_limit", "0")],
                SECRETS,
                "bell.registration_failure_limit",
            ),
            (
                &[("bell.registrations_per_minute", "0")],
                SECRETS,
                "bell.registrations_per_minute",
            ),
            (
                &[("bell.dings_per_merchant_per_minute", "0")],
                SECRETS,
                "bell.dings_per_merchant_per_minute",
            ),
            (&[("bell.listen", "localhost")], SECRETS, "bell.listen"),
            (
                &[("bell.square.api_url", "https://connect.squareup.onion")],
                SECRETS,
                "bell.square.api_url",
            ),
            (&[("logging.level", "info,[")], SECRETS, "logging.level"),
        ];
        for (values, vars, expected) in cases {
            let error = load_with(values, vars).await.unwrap_err().to_string();
            assert!(error.contains(expected), "{expected}: {error}");
        }
    }

    /// The address is kept without a trailing slash, so the webhook URL is
    /// the same however it was written; a path stays.
    #[test]
    fn the_public_address_gives_the_webhook_url() {
        for (raw, url) in [
            (
                "https://Bell.Example.org",
                "https://bell.example.org/v1/square/webhook",
            ),
            (
                "https://bell.example.org/",
                "https://bell.example.org/v1/square/webhook",
            ),
            (
                "https://example.org/bell/",
                "https://example.org/bell/v1/square/webhook",
            ),
            (
                "http://127.0.0.1:8445",
                "http://127.0.0.1:8445/v1/square/webhook",
            ),
        ] {
            let uri = PublicUri::parse(raw).unwrap();
            assert_eq!(uri.notification_url(), url, "{raw}");
            assert_eq!(PublicUri::parse(&uri.render()), Ok(uri));
        }
        assert_eq!(
            PublicUri::parse("https://bell.example.org")
                .unwrap()
                .contact()
                .map(|contact| contact.to_string()),
            Some("https://bell.example.org/".to_owned())
        );
        assert_eq!(
            PublicUri::parse("http://127.0.0.1:8445").unwrap().contact(),
            None
        );
        for bad in [
            "bell.example.org",
            "https://u:p@bell.example.org",
            "https://bell.example.org/#x",
        ] {
            assert!(PublicUri::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_application_id_names_its_environment() {
        assert_eq!(
            ApplicationId::parse("sq0idp-AbC_12-x")
                .unwrap()
                .environment(),
            SquareEnvironment::Production
        );
        assert_eq!(
            ApplicationId::parse("sandbox-sq0idb-AbC")
                .unwrap()
                .environment(),
            SquareEnvironment::Sandbox
        );
        for bad in [
            "",
            "sq0idp-",
            "sq0idp-a b",
            "sq0atp-abc",
            "sandbox-sq0idb-ä",
        ] {
            assert!(ApplicationId::parse(bad).is_err(), "{bad}");
        }
    }
}
