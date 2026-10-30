//! Every Bell setting, declared once with monokulo's `live-settings`
//! library: read from the options file (`bell.toml`, `--options`) and the
//! `--bell-…` command-line options, which win; the integrations' secrets
//! (Square's webhook keys, Clover's auth codes) and the VAPID key from the
//! environment only. Anything invalid stops the Bell before it listens
//! ([`load`]), every problem named.
//!
//! An integration (a point of sale's environment) is on when its table has
//! an `application_id` (`[bell.square.production]`,
//! `[bell.clover.europe]`), and its secret must be set then and only then
//! (decision 51); the Bell serves at least one ([`crate::Bell::start`]),
//! and as many at once as are on (decision 75). The settings a Bell took
//! before then stop it, naming what replaced them ([`REPLACED`],
//! [`REPLACED_ENV`], decision 76).
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
    AnySetting, Applies, BindAddr, Env, FieldError, LoopbackAddr, MemoryStore, Registry, Renamed,
    Secret, Section, Setting, SettingKind, SettingValue, Snapshot, Source, Sources,
};
use push::{Contact, Endpoint, VapidKey};

use crate::clover::AuthCode;
use crate::pos::{CloverEnvironment, Integration, SquareEnvironment};
use crate::square::WebhookKey;

/// Every setting but the integrations' ([`IntegrationSettings`]), in one
/// `settings!` block; its `ALL` is shadowed by this module's own, which
/// adds theirs.
mod declared {
    use super::*;

    live_settings::settings! {
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
            description: "The https address the points of sale and the tills reach this Bell at, such as https://bell.example.org. Each integration's webhooks go to it followed by /v1/<point of sale>/<environment>/webhook, such as /v1/square/production/webhook; Square's subscription must name that URL exactly, as Square signs each webhook for it. Required to run the Bell.",
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
            description: "The Bell's database file, ~/.local/share/monokulo/bell.db unless set (or bell.db in the working directory if that can't be used). It holds the tills' registrations alone: never a webhook, never a point of sale's token.",
            example: "/var/lib/monokulo/bell.db",
            applies: Restart,
        },
        TIMEOUT_MS: u64 {
            key: "bell.timeout_ms",
            default: 10_000,
            check: range(100, 120_000),
            description: "Milliseconds the Bell waits for a point of sale (checking a registration's location) or a push service (taking a ding) to answer.",
            example: "10000",
            applies: Restart,
        },
        REGISTRATIONS_PER_MINUTE: u32 {
            key: "bell.registrations_per_minute",
            default: 30,
            check: range(1, 1_000_000),
            description: "Registrations a minute from any one address (IPv6 by its /64). Each costs a request to the till's point of sale.",
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
            description: "Dings one merchant's tills are sent a minute, across all its locations: a payment dings each till at its location once. A merchant is its integration and its id: a Square production merchant, a Square sandbox one and a Clover one have a share each. The rest of the minute's dings are dropped, as a ding a minute late is worth nothing and the till's timer asks its point of sale anyway. Clover's webhooks don't say how a payment was paid, so at a Clover merchant every payment dings. Every merchant's dings are signed with the Bell's one VAPID key, which a push relay counts as one sender, so keep it well below the relay's per-sender limit (600 a minute by default), or one busy merchant spends the relay's budget for all of them.",
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
        VAPID_KEY: Secret {
            key: "bell.vapid_key",
            env: "MONOKULO_BELL_VAPID_KEY",
            default: Secret::default(),
            check: check_vapid_key,
            description: "The Bell's VAPID private key (RFC 8292), which signs every ding: 32 bytes as 64 hex digits, such as `openssl rand -hex 32` makes. Its public key is what the tills' push subscriptions are made for (GET /v1/info), so keep it: a new key leaves every registration's subscription refusing the Bell's dings.",
            applies: Restart,
            sources: [Env],
            required: true,
        },
        STATUS_LISTEN: Option<LoopbackAddr> {
            key: "status.listen",
            default: live_settings::parsed_default("127.0.0.1:8447"),
            description: "The loopback address and port the Bell answers GET /status on, for this machine only: what it listens on, how many registrations it holds, which integrations are on and how the last ding went, as JSON, never a token, an endpoint or a payment. A loopback address alone is taken. Empty for none.",
            example: "127.0.0.1:8447",
            applies: Restart,
        },
        LOGGING_LEVEL: String {
            key: "logging.level",
            default: telemetry::DEFAULT_LEVEL.to_owned(),
            check: telemetry::check_level,
            description: "Which log lines the Bell writes: a level (error, warn, info, debug, trace), optionally followed by target=level pairs. No line carries a webhook, a point of sale's token or a push endpoint, at any level; the one exception is Clover's webhook verification code, logged at info for the operator to enter in Clover's dashboard.",
            example: "info,monokulo_bell=debug",
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
}

pub use declared::*;

/// The settings of one integration: its table, `[bell.<point of
/// sale>.<environment>]`, whose name is the environment, holds its
/// application and, optionally, where its API is; its secret is an
/// environment variable of its own.
pub struct IntegrationSettings<Id: SettingValue> {
    /// Set, the integration is on.
    pub application_id: Setting<Option<Id>>,
    /// Unset, the environment's own API.
    pub api_url: Setting<Option<ServiceUrl>>,
    /// Square's webhook signature key, or Clover's Auth Code: set when,
    /// and only when, the application is.
    pub secret: Setting<Secret>,
}

/// One integration's [`IntegrationSettings`], every key and text made from
/// its point of sale's words (`square!` and `clover!` below) and its
/// environment's, so the six read alike and none is written out by hand.
macro_rules! integration {
    (
        $id:ty, $pos:literal, $env:literal, $var:literal,
        application: [$($application:literal),+], example: $example:literal,
        api: $api:literal, api_example: $api_example:literal,
        secret: $secret:literal, secret_is: [$($secret_is:literal),+]
    ) => {
        IntegrationSettings::<$id> {
            application_id: Setting {
                key: concat!("bell.", $pos, ".", $env, ".application_id"),
                env_var: "",
                default: || None,
                check: None,
                bounds: None,
                description: concat!(
                    $($application,)+
                    " Set, the Bell serves its tills and takes its webhooks at bell.public_uri followed by /v1/",
                    $pos, "/", $env, "/webhook (and ", $var,
                    " must be set too); unset, it serves none of this environment's."
                ),
                example: Some($example),
                applies: Applies::Restart,
                sources: Sources::CONFIG,
                required: false,
                editable: true,
            },
            api_url: Setting {
                key: concat!("bell.", $pos, ".", $env, ".api_url"),
                env_var: "",
                default: || None,
                check: None,
                bounds: None,
                description: concat!(
                    "Where the API of this environment is, instead of its own (", $api,
                    "): for a stand-in while developing. https, or http on this machine."
                ),
                example: Some($api_example),
                applies: Applies::Restart,
                sources: Sources::CONFIG,
                required: false,
                editable: true,
            },
            secret: Setting {
                key: concat!("bell.", $pos, ".", $env, ".", $secret),
                env_var: $var,
                default: Secret::default,
                check: None,
                bounds: None,
                description: concat!(
                    $($secret_is,)+
                    " Set when, and only when, bell.", $pos, ".", $env, ".application_id is."
                ),
                example: None,
                applies: Applies::Restart,
                sources: Sources::of(&[Source::Env]),
                required: false,
                editable: true,
            },
        }
    };
}

/// A Square environment's settings, `[bell.square.<env>]`.
macro_rules! square {
    ($env:literal, $var:literal, $prefix:literal, $example:literal, $api:literal) => {
        integration!(
            ApplicationId, "square", $env, $var,
            application: [
                "The Square ", $env, " application whose webhooks this Bell receives, and which the tills using it sign in through: ",
                $prefix, "…, from the Developer Console."
            ],
            example: $example,
            api: $api, api_example: "http://127.0.0.1:8738",
            secret: "webhook_key",
            secret_is: [
                "The signature key of the Square ", $env,
                " application's webhook subscription, from the Developer Console's Webhooks page: a webhook whose signature it doesn't check is answered 401 and dropped."
            ]
        )
    };
}

/// A Clover environment's settings, `[bell.clover.<env>]`.
macro_rules! clover {
    ($env:literal, $var:literal, $api:literal) => {
        integration!(
            CloverApplicationId, "clover", $env, $var,
            application: [
                "The Clover ", $env, " app whose webhooks this Bell receives, and which the tills using it sign in through: its App ID, 13 letters and digits, from the ", $env,
                " developer dashboard. A Clover App ID doesn't say which environment it is, so the Bell can't check it as it checks Square's."
            ],
            example: "MONOKULARBELL",
            api: $api, api_example: "http://127.0.0.1:8740",
            secret: "auth_code",
            secret_is: [
                "The Clover ", $env,
                " app's Auth Code, from the developer dashboard's Webhooks page: Clover sends it as the X-Clover-Auth header of every webhook, and one without it is answered 401 and dropped."
            ]
        )
    };
}

pub const SQUARE_PRODUCTION: IntegrationSettings<ApplicationId> = square!(
    "production",
    "MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY",
    "sq0idp-",
    "sq0idp-Monokular0Bell0Example",
    "https://connect.squareup.com"
);
pub const SQUARE_SANDBOX: IntegrationSettings<ApplicationId> = square!(
    "sandbox",
    "MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY",
    "sandbox-sq0idb-",
    "sandbox-sq0idb-Monokular0Bell0Example",
    "https://connect.squareupsandbox.com"
);
pub const CLOVER_NORTH_AMERICA: IntegrationSettings<CloverApplicationId> = clover!(
    "north_america",
    "MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE",
    "https://api.clover.com"
);
pub const CLOVER_EUROPE: IntegrationSettings<CloverApplicationId> = clover!(
    "europe",
    "MONOKULO_BELL_CLOVER_EUROPE_AUTH_CODE",
    "https://api.eu.clover.com"
);
pub const CLOVER_LATIN_AMERICA: IntegrationSettings<CloverApplicationId> = clover!(
    "latin_america",
    "MONOKULO_BELL_CLOVER_LATIN_AMERICA_AUTH_CODE",
    "https://api.la.clover.com"
);
pub const CLOVER_SANDBOX: IntegrationSettings<CloverApplicationId> = clover!(
    "sandbox",
    "MONOKULO_BELL_CLOVER_SANDBOX_AUTH_CODE",
    "https://apisandbox.dev.clover.com"
);

impl SquareEnvironment {
    /// The environment's settings.
    pub fn settings(self) -> &'static IntegrationSettings<ApplicationId> {
        match self {
            SquareEnvironment::Production => &SQUARE_PRODUCTION,
            SquareEnvironment::Sandbox => &SQUARE_SANDBOX,
        }
    }
}

impl CloverEnvironment {
    /// The environment's settings.
    pub fn settings(self) -> &'static IntegrationSettings<CloverApplicationId> {
        match self {
            CloverEnvironment::NorthAmerica => &CLOVER_NORTH_AMERICA,
            CloverEnvironment::Europe => &CLOVER_EUROPE,
            CloverEnvironment::LatinAmerica => &CLOVER_LATIN_AMERICA,
            CloverEnvironment::Sandbox => &CLOVER_SANDBOX,
        }
    }
}

/// Every integration's settings, in [`Integration::ALL`]'s order.
const INTEGRATIONS: &[&dyn AnySetting] = &[
    &SQUARE_PRODUCTION.application_id,
    &SQUARE_PRODUCTION.api_url,
    &SQUARE_PRODUCTION.secret,
    &SQUARE_SANDBOX.application_id,
    &SQUARE_SANDBOX.api_url,
    &SQUARE_SANDBOX.secret,
    &CLOVER_NORTH_AMERICA.application_id,
    &CLOVER_NORTH_AMERICA.api_url,
    &CLOVER_NORTH_AMERICA.secret,
    &CLOVER_EUROPE.application_id,
    &CLOVER_EUROPE.api_url,
    &CLOVER_EUROPE.secret,
    &CLOVER_LATIN_AMERICA.application_id,
    &CLOVER_LATIN_AMERICA.api_url,
    &CLOVER_LATIN_AMERICA.secret,
    &CLOVER_SANDBOX.application_id,
    &CLOVER_SANDBOX.api_url,
    &CLOVER_SANDBOX.secret,
];

/// `a` followed by `b`, in a `const`.
const fn joined<const N: usize>(
    a: &[&'static dyn AnySetting],
    b: &[&'static dyn AnySetting],
) -> [&'static dyn AnySetting; N] {
    assert!(N == a.len() + b.len());
    let mut out = [a[0]; N];
    let mut at = 0;
    while at < a.len() {
        out[at] = a[at];
        at += 1;
    }
    while at < N {
        out[at] = b[at - a.len()];
        at += 1;
    }
    out
}

/// Every Bell setting: the ones `settings!` declares, then the
/// integrations'. `--init` writes each table's in this order, so the
/// integrations' come after the Bell's own, under `[bell]`.
pub const ALL: &[&dyn AnySetting] =
    &joined::<{ declared::ALL.len() + INTEGRATIONS.len() }>(declared::ALL, INTEGRATIONS);

/// The Bell's own section's settings, [`BellConfig`]'s.
const BELL: &[&dyn AnySetting] = &joined::<{ 10 + INTEGRATIONS.len() }>(
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
        &VAPID_KEY,
    ],
    INTEGRATIONS,
);

/// The settings a Bell took before it served several environments of a
/// point of sale at once, each with what replaced it, as "`old` is now
/// called `new`" reads (decision 76). In the options file or on the
/// command line, each stops the Bell, saying so: one left in place would
/// otherwise turn its integration off without a word.
pub const REPLACED: &[(&str, &str)] = &[
    (
        "bell.square.application_id",
        "bell.square.production.application_id or bell.square.sandbox.application_id, by the application's environment",
    ),
    (
        "bell.square.environment",
        "the name of the application's table: [bell.square.production] or [bell.square.sandbox]",
    ),
    (
        "bell.square.api_url",
        "bell.square.production.api_url or bell.square.sandbox.api_url",
    ),
    (
        "bell.clover.application_id",
        "bell.clover.<environment>.application_id, for north_america, europe, latin_america or sandbox",
    ),
    (
        "bell.clover.environment",
        "the name of the app's table: [bell.clover.north_america], [bell.clover.europe], [bell.clover.latin_america] or [bell.clover.sandbox]",
    ),
    (
        "bell.clover.api_url",
        "bell.clover.<environment>.api_url, for north_america, europe, latin_america or sandbox",
    ),
];

/// The secrets' variables a Bell took before then, the monokular-bell
/// names among them, each with what replaced it: one set in the
/// environment stops the Bell, saying so ([`load`]).
pub const REPLACED_ENV: &[(&str, &str)] = &[
    ("MONOKULO_BELL_SQUARE_WEBHOOK_KEY", SQUARE_KEYS),
    ("MONOKULAR_BELL_SQUARE_WEBHOOK_KEY", SQUARE_KEYS),
    ("MONOKULO_BELL_CLOVER_AUTH_CODE", CLOVER_CODES),
    ("MONOKULAR_BELL_CLOVER_AUTH_CODE", CLOVER_CODES),
];

const SQUARE_KEYS: &str = "MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY or MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY, by the application's environment";
const CLOVER_CODES: &str = "MONOKULO_BELL_CLOVER_<ENVIRONMENT>_AUTH_CODE, for NORTH_AMERICA, EUROPE, LATIN_AMERICA or SANDBOX";

/// The VAPID key's variable as it was named in the monokular repository,
/// read in place of its new name, with a warning at start, for one
/// release (decision 68): a service file written for monokular-bell still
/// finds it. The first version tag after the one that ships this removes
/// it. The points of sale's secrets aren't here: their variables were
/// replaced since ([`REPLACED_ENV`]).
pub const RENAMED_ENV: &[Renamed] = &[Renamed {
    old: "MONOKULAR_BELL_VAPID_KEY",
    new: "MONOKULO_BELL_VAPID_KEY",
    since: "2026-10-29",
}];

/// Why the settings can't be used: every problem, one per line.
#[derive(Debug, thiserror::Error)]
#[error("the Bell's settings: {0}")]
pub struct LoadError(String);

/// What [`load`] reads: the Bell's settings, and its status listener's.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub bell: BellConfig,
    pub status: StatusConfig,
}

/// The Bell's settings from `stored` (what its options file holds, by key,
/// already checked line by line) and `env` (the command line and the
/// environment). Every value is checked again where it came from, then
/// together ([`BellConfig`]'s rules); anything wrong is the error, never a
/// default in its place, and so is a secret's variable that was replaced
/// ([`REPLACED_ENV`]).
pub async fn load(stored: HashMap<String, String>, env: Env) -> Result<Loaded, LoadError> {
    let mut problems: Vec<String> = REPLACED_ENV
        .iter()
        .filter(|(old, _)| env.get(old).is_some())
        .map(|(old, new)| format!("{old} is now called {new}: rename it in the environment"))
        .collect();
    let store = Arc::new(MemoryStore::with(stored));
    let mut builder = Registry::builder_with_env(store, ALL, env).await;
    let bell = builder.section::<BellConfig>();
    let status = builder.section::<StatusConfig>();
    // Read before the registry (logging starts first); registered so a bad
    // value there stops the Bell like any other.
    builder.section::<LoggingConfig>();
    if let Err(error) = builder.build() {
        problems.push(error.to_string());
    }
    if !problems.is_empty() {
        return Err(LoadError(problems.join("; ")));
    }
    Ok(Loaded {
        bell: (*bell.load()).clone(),
        status: *status.load(),
    })
}

/// The settings the Bell runs on, checked together.
///
/// The address and the VAPID key are `None` only in the section every
/// setting at its default makes, which the settings library builds as its
/// fallback: a Bell is never started on it, as [`load`] refuses the unset
/// key and [`crate::Bell::start`] the unset address, and a Bell serving no
/// integration.
#[derive(Clone, PartialEq)]
pub struct BellConfig {
    pub listen: SocketAddr,
    pub public_uri: Option<PublicUri>,
    pub trusted_proxies: TrustedProxies,
    pub store_path: PathBuf,
    /// How long the points of sale and the push services get to answer.
    pub timeout: Duration,
    pub registrations_per_minute: u32,
    pub registration_failure_limit: u32,
    pub dings_per_merchant_per_minute: u32,
    pub allow_private_endpoints: bool,
    /// The Square environments that are on, in their order.
    pub square: Vec<SquareConfig>,
    /// The Clover environments that are on, in their order.
    pub clover: Vec<CloverConfig>,
    pub vapid_key: Option<VapidSecret>,
}

/// A Square environment the Bell serves: its application, where its API
/// is, and its webhook key.
#[derive(Clone, PartialEq)]
pub struct SquareConfig {
    pub environment: SquareEnvironment,
    pub application_id: ApplicationId,
    /// The environment's API, or its table's `api_url` when set.
    pub api_url: ServiceUrl,
    pub webhook_key: WebhookKey,
}

/// A Clover environment the Bell serves: its app, where its API is, and
/// its auth code.
#[derive(Clone, PartialEq)]
pub struct CloverConfig {
    pub environment: CloverEnvironment,
    pub application_id: CloverApplicationId,
    /// The environment's API, or its table's `api_url` when set.
    pub api_url: ServiceUrl,
    pub auth_code: AuthCode,
}

/// Names the settings, never the key.
impl fmt::Debug for SquareConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SquareConfig")
            .field("environment", &self.environment)
            .field("application_id", &self.application_id)
            .field("api_url", &self.api_url)
            .finish_non_exhaustive()
    }
}

/// Names the settings, never the code.
impl fmt::Debug for CloverConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloverConfig")
            .field("environment", &self.environment)
            .field("application_id", &self.application_id)
            .field("api_url", &self.api_url)
            .finish_non_exhaustive()
    }
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
            .field("clover", &self.clover)
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
        BELL
    }

    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        let mut errors = Vec::new();
        let mut square = Vec::new();
        for environment in SquareEnvironment::ALL {
            let settings = environment.settings();
            let Some(on) = turned_on(
                snapshot,
                Integration::Square(environment),
                settings,
                &mut errors,
            ) else {
                continue;
            };
            let in_table = on.application_id.environment();
            if in_table != environment {
                errors.push(FieldError::new(
                    settings.application_id.key,
                    format!(
                        "is a {} application's: it goes in [bell.square.{}], as Square sends its webhooks from the application's own environment.",
                        in_table.as_str(),
                        in_table.as_str()
                    ),
                ));
                continue;
            }
            square.push(SquareConfig {
                environment,
                application_id: on.application_id,
                api_url: on.api_url.unwrap_or_else(|| environment.api_url()),
                webhook_key: WebhookKey::new(on.secret),
            });
        }
        let mut clover = Vec::new();
        for environment in CloverEnvironment::ALL {
            let Some(on) = turned_on(
                snapshot,
                Integration::Clover(environment),
                environment.settings(),
                &mut errors,
            ) else {
                continue;
            };
            clover.push(CloverConfig {
                environment,
                application_id: on.application_id,
                api_url: on.api_url.unwrap_or_else(|| environment.api_url()),
                auth_code: AuthCode::new(on.secret),
            });
        }
        if !errors.is_empty() {
            return Err(errors);
        }
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
            square,
            clover,
            // Required, so empty only at its default.
            vapid_key: VapidSecret::parse(&vapid_key).ok(),
        })
    }
}

/// An integration whose application is set, with its secret.
struct On<Id> {
    application_id: Id,
    api_url: Option<ServiceUrl>,
    secret: Secret,
}

/// `integration`'s settings, when it is on and its secret is set; a
/// secret without its application, or an application without its secret,
/// is one of `errors`.
fn turned_on<Id: SettingValue>(
    snapshot: &Snapshot,
    integration: Integration,
    settings: &IntegrationSettings<Id>,
    errors: &mut Vec<FieldError>,
) -> Option<On<Id>> {
    let application_id = snapshot.get(&settings.application_id);
    let secret = snapshot.get(&settings.secret);
    if let Some(error) = secret_matches_application(
        integration,
        &settings.application_id,
        application_id.is_some(),
        &settings.secret,
        &secret,
    ) {
        errors.push(error);
        return None;
    }
    Some(On {
        application_id: application_id?,
        api_url: snapshot.get(&settings.api_url),
        secret,
    })
}

/// An integration's secret is set when, and only when, its application
/// is (decision 51): missing, the Bell would refuse every webhook; set for
/// one that is off, it is most likely a table misnamed, which would
/// otherwise pass unnoticed.
fn secret_matches_application<T: SettingValue>(
    integration: Integration,
    application: &Setting<T>,
    on: bool,
    secret: &Setting<Secret>,
    value: &Secret,
) -> Option<FieldError> {
    let variable = secret.env_var;
    match (on, value.is_empty()) {
        (true, true) => Some(FieldError::new(
            secret.key,
            format!(
                "{variable} must be set when {} is: without it every {integration} webhook is refused.",
                application.key,
            ),
        )),
        (false, false) => Some(FieldError::new(
            secret.key,
            format!(
                "{variable} is set but {} isn't: set the application id to serve {integration} tills, or unset the variable.",
                application.key,
            ),
        )),
        _ => None,
    }
}

/// The Bell's status listener (`GET /status`), when it has one: a
/// loopback address by its type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusConfig {
    pub listen: Option<SocketAddr>,
}

impl Section for StatusConfig {
    const NAME: &'static str = "status";

    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&STATUS_LISTEN]
    }

    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(StatusConfig {
            listen: snapshot.get(&STATUS_LISTEN).map(LoopbackAddr::addr),
        })
    }
}

/// `monokulo-bell`'s logging, read before anything else.
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
/// endpoint's rule ([`push::Endpoint`]). A point of sale is called with a
/// merchant's token, and the Bell's own address is what webhooks are
/// signed for, so neither is ever plain `http` across a network.
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
/// (a reverse proxy serving the Bell under one), and each integration's
/// webhook URL is this followed by its [`Integration::webhook_path`].
#[derive(Clone, PartialEq, Eq)]
pub struct PublicUri {
    base: String,
}

impl PublicUri {
    /// The address, as set: an origin, `https://bell.example.org`.
    pub fn as_str(&self) -> &str {
        &self.base
    }

    /// The URL `integration` sends webhooks to (and Square signs them for).
    pub fn notification_url(&self, integration: Integration) -> String {
        format!("{}{}", self.base, integration.webhook_path())
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

    /// The environment the application is in, by its prefix: the one
    /// whose table it must be in.
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

/// A Clover app's ID: the App ID the developer dashboard shows, 13 letters
/// and digits; taken as 1 to 64 letters, digits, `-` and `_`, as Square's
/// ids are. Public: the tills sign in with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloverApplicationId(String);

impl CloverApplicationId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl SettingValue for CloverApplicationId {
    fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        let fits = (1..=64).contains(&raw.len())
            && raw
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        if fits {
            Ok(CloverApplicationId(raw.to_owned()))
        } else {
            Err(
                "Enter the App ID Clover's developer dashboard shows: 13 letters and digits."
                    .to_owned(),
            )
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
pub(crate) mod tests;
