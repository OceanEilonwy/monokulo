use super::*;
use live_settings::OptionsFile;

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
    load(stored, Env::fixed(vars.iter().copied()))
        .await
        .map(|loaded| loaded.bell)
}

const SQUARE_KEY: &str = "Ibxx_5AKakO-3qeNVR61Dw";
const SANDBOX_KEY: &str = "Sbxx_5AKakO-3qeNVR61Dw";
const AUTH_CODE: &str = "9f2d4a8c-6b1e-4f3a-9c7d-2e5b8a1f4c6d";

/// The VAPID key alone: what a Bell needs whichever integration it serves.
const SECRETS: &[(&str, &str)] = &[("MONOKULO_BELL_VAPID_KEY", VAPID_HEX)];

/// The VAPID key and Square production's webhook key.
const SQUARE_SECRETS: &[(&str, &str)] = &[
    ("MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY", SQUARE_KEY),
    ("MONOKULO_BELL_VAPID_KEY", VAPID_HEX),
];

/// The VAPID key and Square sandbox's webhook key.
const SANDBOX_SECRETS: &[(&str, &str)] = &[
    ("MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY", SANDBOX_KEY),
    ("MONOKULO_BELL_VAPID_KEY", VAPID_HEX),
];

/// The VAPID key and Clover north_america's auth code.
const CLOVER_SECRETS: &[(&str, &str)] = &[
    ("MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE", AUTH_CODE),
    ("MONOKULO_BELL_VAPID_KEY", VAPID_HEX),
];

/// Square production's and Clover north_america's secrets and the VAPID
/// key.
const BOTH_SECRETS: &[(&str, &str)] = &[
    ("MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY", SQUARE_KEY),
    ("MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE", AUTH_CODE),
    ("MONOKULO_BELL_VAPID_KEY", VAPID_HEX),
];

const SQUARE_ON: (&str, &str) = ("bell.square.production.application_id", "sq0idp-Test");
const SANDBOX_ON: (&str, &str) = ("bell.square.sandbox.application_id", "sandbox-sq0idb-Test");
const CLOVER_ON: (&str, &str) = ("bell.clover.north_america.application_id", "MONOKULARBELL");

/// The status listens on this machine alone, on 8447 unless set; set
/// empty, nowhere.
#[tokio::test]
async fn the_status_listens_on_this_machine_alone() {
    let status = |values: &[(&str, &str)]| {
        let stored = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        load(stored, Env::fixed(SECRETS.iter().copied()))
    };
    let listen = |values| async move { status(values).await.unwrap().status.listen };
    assert_eq!(listen(&[]).await, Some("127.0.0.1:8447".parse().unwrap()));
    assert_eq!(
        listen(&[("status.listen", "[::1]:9000")]).await,
        Some("[::1]:9000".parse().unwrap())
    );
    assert_eq!(listen(&[("status.listen", "")]).await, None);
    let error = status(&[("status.listen", "0.0.0.0:8447")])
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("status.listen") && error.contains("isn't a loopback address"),
        "{error}"
    );
}

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
    // No integration is on until its application is set.
    assert!(config.square.is_empty());
    assert!(config.clover.is_empty());
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

/// Each Square environment has its own API; its table's api_url wins.
#[tokio::test]
async fn the_square_api_follows_the_environment_unless_it_is_set() {
    let sandbox = load_with(&[SANDBOX_ON], SANDBOX_SECRETS).await.unwrap();
    assert_eq!(sandbox.square.len(), 1);
    assert_eq!(sandbox.square[0].environment, SquareEnvironment::Sandbox);
    assert_eq!(
        sandbox.square[0].api_url.join("/v2/locations/L1"),
        "https://connect.squareupsandbox.com/v2/locations/L1"
    );
    let local = load_with(
        &[
            SANDBOX_ON,
            ("bell.square.sandbox.api_url", "http://127.0.0.1:8738"),
        ],
        SANDBOX_SECRETS,
    )
    .await
    .unwrap();
    assert_eq!(
        local.square[0].api_url.join("/v2/locations/L1"),
        "http://127.0.0.1:8738/v2/locations/L1"
    );
    let production = load_with(
        &[
            SQUARE_ON,
            (
                "bell.square.production.api_url",
                "https://square.example.org/",
            ),
        ],
        SQUARE_SECRETS,
    )
    .await
    .unwrap();
    assert_eq!(
        production.square[0].api_url.join("/v2/locations/L1"),
        "https://square.example.org/v2/locations/L1"
    );
    let default = load_with(&[SQUARE_ON], SQUARE_SECRETS).await.unwrap();
    assert_eq!(
        default.square[0].api_url.join("/v2/locations/L1"),
        "https://connect.squareup.com/v2/locations/L1"
    );
}

/// Square's production and its sandbox at once: two integrations, each
/// with its application, its API and its own key.
#[tokio::test]
async fn both_square_environments_are_served_at_once() {
    let vars = [
        ("MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY", SQUARE_KEY),
        ("MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY", SANDBOX_KEY),
        ("MONOKULO_BELL_VAPID_KEY", VAPID_HEX),
    ];
    let config = load_with(&[SANDBOX_ON, SQUARE_ON], &vars).await.unwrap();
    let environments: Vec<_> = config
        .square
        .iter()
        .map(|square| (square.environment, square.application_id.as_str()))
        .collect();
    assert_eq!(
        environments,
        [
            (SquareEnvironment::Production, "sq0idp-Test"),
            (SquareEnvironment::Sandbox, "sandbox-sq0idb-Test"),
        ]
    );
    assert!(config.square[0].webhook_key != config.square[1].webhook_key);
    assert!(config.clover.is_empty());
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
        (
            &[SQUARE_ON],
            SECRETS,
            "MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY must be set when bell.square.production.application_id is: without it every Square production webhook is refused",
        ),
        (
            &[SANDBOX_ON],
            SQUARE_SECRETS,
            "MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY must be set when bell.square.sandbox.application_id is",
        ),
        (
            &[CLOVER_ON],
            SECRETS,
            "MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE must be set when bell.clover.north_america.application_id is",
        ),
        (
            &[SQUARE_ON],
            BOTH_SECRETS,
            "MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE is set but bell.clover.north_america.application_id isn't",
        ),
        (
            &[CLOVER_ON],
            BOTH_SECRETS,
            "MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY is set but bell.square.production.application_id isn't",
        ),
        // The key of one environment set for the other: a table misnamed.
        (
            &[SQUARE_ON],
            SANDBOX_SECRETS,
            "MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY is set but bell.square.sandbox.application_id isn't",
        ),
        (
            &[("bell.clover.europe.application_id", "MONOKULARBELL")],
            CLOVER_SECRETS,
            "MONOKULO_BELL_CLOVER_EUROPE_AUTH_CODE must be set",
        ),
        (&[], &[], "MONOKULO_BELL_VAPID_KEY must be set"),
        (
            &[],
            &[("MONOKULO_BELL_VAPID_KEY", "abc")],
            "MONOKULO_BELL_VAPID_KEY",
        ),
        (
            &[],
            &[
                // The curve's order: not a private key.
                (
                    "MONOKULO_BELL_VAPID_KEY",
                    "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551",
                ),
            ],
            "MONOKULO_BELL_VAPID_KEY",
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
            &[(
                "bell.square.production.api_url",
                "http://connect.squareup.com",
            )],
            SECRETS,
            "bell.square.production.api_url",
        ),
        (
            &[("bell.square.production.application_id", "my-app")],
            SECRETS,
            "bell.square.production.application_id",
        ),
        // The table names the environment, and the id says which it is.
        (
            &[(
                "bell.square.production.application_id",
                "sandbox-sq0idb-Test",
            )],
            SQUARE_SECRETS,
            "bell.square.production.application_id: is a sandbox application's: it goes in [bell.square.sandbox]",
        ),
        (
            &[("bell.square.sandbox.application_id", "sq0idp-Test")],
            SANDBOX_SECRETS,
            "bell.square.sandbox.application_id: is a production application's: it goes in [bell.square.production]",
        ),
        (
            &[("bell.trusted_proxies", "a.b.c.d")],
            SECRETS,
            "bell.trusted_proxies",
        ),
        // Set, but to nothing: as unset.
        (
            &[SQUARE_ON],
            &[
                ("MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY", ""),
                ("MONOKULO_BELL_VAPID_KEY", VAPID_HEX),
            ],
            "MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY",
        ),
        (
            &[CLOVER_ON],
            &[
                ("MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE", ""),
                ("MONOKULO_BELL_VAPID_KEY", VAPID_HEX),
            ],
            "MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE",
        ),
        (
            &[("bell.clover.sandbox.api_url", "http://example.org")],
            SECRETS,
            "bell.clover.sandbox.api_url",
        ),
        (
            &[("bell.clover.north_america.application_id", "MONO KULAR")],
            SECRETS,
            "bell.clover.north_america.application_id",
        ),
        (
            &[("bell.clover.latin_america.application_id", "MONOKULAR/BELL")],
            SECRETS,
            "bell.clover.latin_america.application_id",
        ),
        (
            &[],
            &[("MONOKULO_BELL_VAPID_KEY", "")],
            "MONOKULO_BELL_VAPID_KEY",
        ),
        (
            &[],
            &[
                // Zero is no private key.
                ("MONOKULO_BELL_VAPID_KEY", ZERO_KEY),
            ],
            "MONOKULO_BELL_VAPID_KEY",
        ),
        (
            &[],
            &[
                // 64 characters, not all hex.
                ("MONOKULO_BELL_VAPID_KEY", not_hex.as_str()),
            ],
            "MONOKULO_BELL_VAPID_KEY",
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
            &[(
                "bell.square.sandbox.api_url",
                "https://connect.squareup.onion",
            )],
            SECRETS,
            "bell.square.sandbox.api_url",
        ),
        (&[("logging.level", "info,[")], SECRETS, "logging.level"),
    ];
    for (values, vars, expected) in cases {
        let error = load_with(values, vars).await.unwrap_err().to_string();
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

/// Each Clover environment loads on its own table, with its own API
/// unless set; all four, and Square beside them, at once.
#[tokio::test]
async fn every_clover_environment_loads_alone_or_together() {
    let clover = load_with(&[CLOVER_ON], CLOVER_SECRETS).await.unwrap();
    assert_eq!(clover.clover.len(), 1);
    assert_eq!(clover.clover[0].application_id.as_str(), "MONOKULARBELL");
    assert!(clover.square.is_empty());
    for (environment, api) in [
        ("north_america", "https://api.clover.com/v3/merchants/M1"),
        ("europe", "https://api.eu.clover.com/v3/merchants/M1"),
        ("latin_america", "https://api.la.clover.com/v3/merchants/M1"),
        (
            "sandbox",
            "https://apisandbox.dev.clover.com/v3/merchants/M1",
        ),
    ] {
        let key = format!("bell.clover.{environment}.application_id");
        let var = format!(
            "MONOKULO_BELL_CLOVER_{}_AUTH_CODE",
            environment.to_uppercase()
        );
        let config = load_with(
            &[(&key, "MONOKULARBELL")],
            &[(&var, AUTH_CODE), ("MONOKULO_BELL_VAPID_KEY", VAPID_HEX)],
        )
        .await
        .unwrap();
        assert_eq!(config.clover.len(), 1, "{environment}");
        assert_eq!(config.clover[0].environment.as_str(), environment);
        assert_eq!(config.clover[0].api_url.join("/v3/merchants/M1"), api);
    }
    let mut values = vec![SQUARE_ON];
    let mut vars = BOTH_SECRETS.to_vec();
    let keys: Vec<(String, String)> = ["europe", "latin_america", "sandbox"]
        .iter()
        .map(|environment| {
            (
                format!("bell.clover.{environment}.application_id"),
                format!(
                    "MONOKULO_BELL_CLOVER_{}_AUTH_CODE",
                    environment.to_uppercase()
                ),
            )
        })
        .collect();
    values.push(CLOVER_ON);
    for (key, var) in &keys {
        values.push((key, "MONOKULARBELL"));
        vars.push((var, AUTH_CODE));
    }
    values.push(("bell.clover.sandbox.api_url", "http://127.0.0.1:8740"));
    let all = load_with(&values, &vars).await.unwrap();
    assert_eq!(all.square.len(), 1);
    assert_eq!(
        all.clover
            .iter()
            .map(|clover| clover.environment)
            .collect::<Vec<_>>(),
        CloverEnvironment::ALL
    );
    assert_eq!(
        all.clover[3].api_url.join("/v3/merchants/M1"),
        "http://127.0.0.1:8740/v3/merchants/M1"
    );
}

/// Every problem is named at once: a Square application without its
/// key and a Clover code without its application.
#[tokio::test]
async fn every_integration_problem_is_named_at_once() {
    let error = load_with(
        &[SQUARE_ON],
        &[
            ("MONOKULO_BELL_CLOVER_EUROPE_AUTH_CODE", "c"),
            ("MONOKULO_BELL_VAPID_KEY", VAPID_HEX),
        ],
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY must be set"),
        "{error}"
    );
    assert!(
        error.contains("MONOKULO_BELL_CLOVER_EUROPE_AUTH_CODE is set"),
        "{error}"
    );
}

/// A secret's variable from before the integrations, under its monokulo
/// name or its monokular one, stops the Bell, naming what replaced it:
/// read as one integration's it would serve the wrong one, ignored its
/// integration would be off without a word.
#[tokio::test]
async fn a_replaced_secret_stops_the_bell_naming_its_new_names() {
    for (old, new) in [
        (
            "MONOKULO_BELL_SQUARE_WEBHOOK_KEY",
            "MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY or MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY",
        ),
        (
            "MONOKULAR_BELL_SQUARE_WEBHOOK_KEY",
            "MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY or MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY",
        ),
        (
            "MONOKULO_BELL_CLOVER_AUTH_CODE",
            "MONOKULO_BELL_CLOVER_<ENVIRONMENT>_AUTH_CODE, for NORTH_AMERICA, EUROPE, LATIN_AMERICA or SANDBOX",
        ),
        (
            "MONOKULAR_BELL_CLOVER_AUTH_CODE",
            "MONOKULO_BELL_CLOVER_<ENVIRONMENT>_AUTH_CODE",
        ),
    ] {
        let mut vars = SQUARE_SECRETS.to_vec();
        vars.push((old, "a-secret"));
        let error = load_with(&[SQUARE_ON], &vars)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!("{old} is now called {new}")),
            "{old}: {error}"
        );
        assert!(!error.contains("a-secret"), "{error}");
    }
}

/// An options file from before the integrations is refused line by line,
/// each old key with what replaced it, as `monokulo-bell` reads it.
#[test]
fn a_replaced_setting_in_the_options_file_is_refused_with_what_replaced_it() {
    let file = OptionsFile::in_memory(
        "[bell.square]\napplication_id = \"sq0idp-Test\"\nenvironment = \"production\"\napi_url = \"http://127.0.0.1:8738\"\n\n[bell.clover]\napplication_id = \"MONOKULARBELL\"\nenvironment = \"europe\"\napi_url = \"http://127.0.0.1:8740\"\n",
    )
    .with_renamed(REPLACED);
    let error = file.read(ALL).unwrap_err().to_string();
    for (key, new) in REPLACED {
        assert!(
            error.contains(&format!("{key} is now called {new}")),
            "{key}: {error}"
        );
    }
    assert!(
        error.contains("line 2: bell.square.application_id"),
        "{error}"
    );
    // The new tables read.
    let file = OptionsFile::in_memory(
        "[bell.square.sandbox]\napplication_id = \"sandbox-sq0idb-Test\"\n\n[bell.clover.europe]\napplication_id = \"MONOKULARBELL\"\napi_url = \"http://127.0.0.1:8740\"\n",
    )
    .with_renamed(REPLACED);
    let values = file.read(ALL).unwrap();
    assert_eq!(
        values["bell.square.sandbox.application_id"],
        "sandbox-sq0idb-Test"
    );
    assert_eq!(
        values["bell.clover.europe.api_url"],
        "http://127.0.0.1:8740"
    );
}

/// A service file written for monokular-bell still finds the VAPID key
/// for a release, read under its old name and said.
#[tokio::test]
async fn the_vapid_keys_old_name_is_read_for_one_release() {
    let env = Env::fixed([
        ("MONOKULAR_BELL_VAPID_KEY", VAPID_HEX),
        ("MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY", SQUARE_KEY),
    ])
    .with_renamed(RENAMED_ENV);
    let config = load(
        [SQUARE_ON]
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
        env.clone(),
    )
    .await
    .unwrap()
    .bell;
    assert!(config.vapid_key.is_some());
    let warnings = env.renamed_given().unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with(
            "MONOKULAR_BELL_VAPID_KEY was renamed MONOKULO_BELL_VAPID_KEY on 2026-10-29"
        ),
        "{warnings:?}"
    );
}

/// Under both names with different values, which was meant can't be
/// told: the Bell stops, naming both and neither value.
#[test]
fn a_secret_under_both_names_must_agree() {
    let env = Env::fixed([
        ("MONOKULO_BELL_VAPID_KEY", VAPID_HEX),
        ("MONOKULAR_BELL_VAPID_KEY", "02".repeat(32).as_str()),
    ])
    .with_renamed(RENAMED_ENV);
    let error = env.renamed_given().unwrap_err();
    assert!(
        error.starts_with(
            "MONOKULAR_BELL_VAPID_KEY and MONOKULO_BELL_VAPID_KEY are both set, to different values"
        ),
        "{error}"
    );
    assert!(!error.contains(VAPID_HEX), "{error}");
}

/// Every setting is in one section, the Bell's own `bell.*`, once; each
/// integration's are its table's, its secret by its own variable.
#[test]
fn every_setting_is_in_its_section_once() {
    let mut keys: Vec<&str> = ALL.iter().map(|setting| setting.key()).collect();
    let all = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), all);
    let sections: Vec<&str> = BELL
        .iter()
        .chain(StatusConfig::keys())
        .chain(LoggingConfig::keys())
        .map(|setting| setting.key())
        .collect();
    assert_eq!(sections.len(), all);
    assert!(
        BELL.iter()
            .all(|setting| setting.key().starts_with("bell."))
    );
    for integration in Integration::ALL {
        let table = format!(
            "bell.{}.{}.",
            integration.point_of_sale(),
            integration.environment()
        );
        let variable_part = format!(
            "_{}_{}_",
            integration.point_of_sale().as_str().to_uppercase(),
            integration.environment().to_uppercase()
        );
        let in_table: Vec<&dyn AnySetting> = ALL
            .iter()
            .copied()
            .filter(|setting| setting.key().starts_with(&table))
            .collect();
        assert_eq!(in_table.len(), 3, "{table}");
        let secrets: Vec<&str> = in_table
            .iter()
            .filter(|setting| setting.sources().env)
            .map(|setting| setting.env_var())
            .collect();
        assert_eq!(secrets.len(), 1, "{table}");
        assert!(secrets[0].contains(&variable_part), "{}", secrets[0]);
    }
}

/// The secrets never show in the settings' `Debug`.
#[tokio::test]
async fn the_settings_debug_names_no_secret() {
    let config = load_with(&[SQUARE_ON, CLOVER_ON], BOTH_SECRETS)
        .await
        .unwrap();
    let debug = format!("{config:?}");
    for (_, secret) in BOTH_SECRETS {
        assert!(!debug.contains(secret), "{debug}");
    }
    assert!(debug.contains("MONOKULARBELL"), "{debug}");
}

/// The address is kept without a trailing slash, so the webhook URL is
/// the same however it was written; a path stays.
#[test]
fn the_public_address_gives_the_webhook_url() {
    let sandbox = Integration::Square(SquareEnvironment::Sandbox);
    for (raw, url) in [
        (
            "https://Bell.Example.org",
            "https://bell.example.org/v1/square/sandbox/webhook",
        ),
        (
            "https://bell.example.org/",
            "https://bell.example.org/v1/square/sandbox/webhook",
        ),
        (
            "https://example.org/bell/",
            "https://example.org/bell/v1/square/sandbox/webhook",
        ),
        (
            "http://127.0.0.1:8445",
            "http://127.0.0.1:8445/v1/square/sandbox/webhook",
        ),
    ] {
        let uri = PublicUri::parse(raw).unwrap();
        assert_eq!(uri.notification_url(sandbox), url, "{raw}");
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
