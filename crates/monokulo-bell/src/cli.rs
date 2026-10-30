//! The `monokulo-bell` command line, with `clap`: `--options` and
//! `--init` for its options file, and an option for every setting that
//! takes one, with help from the settings' own declarations
//! (`live_settings::cli`). Its secrets are listed after the options, with
//! their environment variables. An option of a setting replaced since
//! ([`crate::settings::REPLACED`]) is refused, naming what replaced it.

use std::sync::LazyLock;

use clap::{Arg, Command};

const ABOUT: &str = "The Monokular Bell: it turns a point of sale's payment webhooks (Square's, \
Clover's, or both, in one environment or several) into Web Push dings that wake the Monokular \
tills at that location.";

const LONG_ABOUT: &str = "The Monokular Bell: it turns a point of sale's payment webhooks \
(Square's, Clover's, or both, in one environment or several) into Web Push dings that wake the \
Monokular tills at that location.

An integration, a point of sale's environment, is on when its application is set \
(bell.square.production.application_id, bell.square.sandbox.application_id, \
bell.clover.north_america.application_id, bell.clover.europe.application_id, \
bell.clover.latin_america.application_id, bell.clover.sandbox.application_id), with its secret; \
as many are on at once as are set. A till registers its push subscription here for its \
integration and one of its merchant's locations (a Clover merchant is its own location), with \
the merchant's access token, which the Bell uses once to check the location and never keeps. \
Webhooks come to bell.public_uri followed by /v1/<point of sale>/<environment>/webhook, such as \
/v1/square/production/webhook (signed with that subscription's key) or \
/v1/clover/europe/webhook (carrying that app's Auth Code); a payment dings that location's \
tills. The Bell keeps only the registrations, in its own database file (bell.store_path): no \
webhook, no payment and no token is stored or logged.

Settings are kept in the options file (--options, by default ~/.config/monokulo/bell.toml); \
--init writes one with every setting described. An option below wins over the file. Secrets \
have no option and are never kept in the file, only in their environment variable (listed \
after the options): every user on the machine can read the process list.";

const EXAMPLES: &str = "Examples:
  monokulo-bell --init
      Write the options file, with every setting described, and say where.
  MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY=… MONOKULO_BELL_VAPID_KEY=… monokulo-bell --check
      Say whether the Bell would start on the options file and the environment, and on what.
  MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY=… MONOKULO_BELL_VAPID_KEY=$(openssl rand -hex 32) \\
    monokulo-bell --bell-public-uri https://bell.example.org \\
    --bell-square-production-application-id sq0idp-…
      Start a Bell for Square tills. Keep the VAPID key: the tills' subscriptions are made for it.
  MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE=… MONOKULO_BELL_CLOVER_SANDBOX_AUTH_CODE=… \\
    MONOKULO_BELL_VAPID_KEY=… monokulo-bell --bell-public-uri https://bell.example.org \\
    --bell-clover-north-america-application-id … --bell-clover-sandbox-application-id …
      Start a Bell for Clover tills in North America and in Clover's sandbox at once.
      Then send each app's verification code from Clover's dashboard and read it in the log.";

/// The Bell's options file, in `~/.config/monokulo/` unless `--options`
/// names another.
pub const OPTIONS_FILE: &str = "bell.toml";

/// `--version`: the version, and the commit it was built from when the
/// build said.
static VERSION: LazyLock<String> =
    LazyLock::new(|| service_runtime::status::version(env!("CARGO_PKG_VERSION")));

/// The command line: the options file (`--check` too), then an option per
/// setting, and a hidden one for each setting replaced, so it is refused
/// with what replaced it rather than as unknown.
pub fn command() -> Command {
    let mut command = Command::new("monokulo-bell")
        .version(VERSION.as_str())
        .about(ABOUT)
        .long_about(LONG_ABOUT)
        .after_help(EXAMPLES);
    for (key, _) in crate::settings::REPLACED {
        command = command.arg(
            Arg::new(*key)
                .long(live_settings::cli_flag(key))
                .value_name("VALUE")
                .hide(true),
        );
    }
    live_settings::cli::with_check(live_settings::cli::with_settings(
        command,
        crate::settings::ALL,
        OPTIONS_FILE,
    ))
}

/// Parses the full argument list, argv[0] included, into the settings it
/// gives (the environment read under the VAPID key's old name too,
/// [`crate::settings::RENAMED_ENV`]), the options file to read, and
/// whether to write one instead (`--init`) or only check it (`--check`).
/// Help, the version and mistakes, a replaced setting's option among them,
/// come back as clap's error, which the caller prints and exits on
/// (`clap::Error::exit`).
pub fn parse_args<I, T>(args: I) -> Result<live_settings::cli::Start, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let mut command = command();
    let matches = command.try_get_matches_from_mut(args)?;
    if let Some((key, new)) = crate::settings::REPLACED
        .iter()
        .find(|(key, _)| matches.get_one::<String>(key).is_some())
    {
        return Err(command.error(
            clap::error::ErrorKind::UnknownArgument,
            format!(
                "--{} was replaced: {key} is now called {new}",
                live_settings::cli_flag(key)
            ),
        ));
    }
    let start = live_settings::cli::start(&matches, crate::settings::ALL, OPTIONS_FILE);
    Ok(live_settings::cli::Start {
        env: start.env.with_renamed(crate::settings::RENAMED_ENV),
        ..start
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_is_well_formed() {
        command().debug_assert();
    }

    /// Every setting that takes one is an option; the secrets are listed by
    /// their variables and have none.
    #[test]
    fn the_help_lists_every_option_and_the_secrets() {
        let help = command().render_long_help().to_string();
        for setting in crate::settings::ALL {
            let flag = format!("--{} <", live_settings::cli_flag(setting.key()));
            assert_eq!(help.contains(&flag), setting.sources().cli, "{flag}");
        }
        for setting in crate::settings::ALL
            .iter()
            .filter(|setting| setting.sources().env)
        {
            let secret = setting.env_var();
            assert!(help.contains(secret), "{secret}: {help}");
        }
        for (key, _) in crate::settings::REPLACED {
            let flag = format!("--{} ", live_settings::cli_flag(key));
            assert!(!help.contains(&flag), "{flag}");
        }
        assert!(parse_args(["monokulo-bell", "--bell-vapid-key", "x"]).is_err());
        assert!(parse_args(["monokulo-bell", "--bell-clover-europe-auth-code", "x"]).is_err());
        let args = parse_args([
            "monokulo-bell",
            "--bell-public-uri",
            "https://bell.example.org",
        ])
        .unwrap();
        assert_eq!(
            args.env.cli("bell.public_uri").as_deref(),
            Some("https://bell.example.org")
        );
        // Checked as it is parsed.
        assert!(parse_args(["monokulo-bell", "--bell-public-uri", "http://x.org"]).is_err());
        assert!(args.options.ends_with(OPTIONS_FILE), "{:?}", args.options);
    }

    /// An option of a setting from before the integrations stops the Bell,
    /// saying what replaced it, rather than being taken or called unknown.
    #[test]
    fn a_replaced_option_is_refused_with_what_replaced_it() {
        for (option, new) in [
            (
                "--bell-square-application-id",
                "bell.square.production.application_id or bell.square.sandbox.application_id",
            ),
            ("--bell-square-environment", "[bell.square.sandbox]"),
            ("--bell-clover-api-url", "bell.clover.<environment>.api_url"),
            ("--bell-clover-environment", "[bell.clover.europe]"),
        ] {
            let error = parse_args(["monokulo-bell", option, "x"])
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(&format!("{option} was replaced")) && error.contains(new),
                "{option}: {error}"
            );
        }
        let args = parse_args([
            "monokulo-bell",
            "--bell-square-sandbox-application-id",
            "sandbox-sq0idb-Test",
        ])
        .unwrap();
        assert_eq!(
            args.env
                .cli("bell.square.sandbox.application_id")
                .as_deref(),
            Some("sandbox-sq0idb-Test")
        );
    }

    /// The files in `service/` (shipped in the release zip) start the
    /// Bell with options it takes, its options file and its database at
    /// fixed places, and name each of its secrets by its variable and no
    /// other: a renamed option or variable can't leave them behind.
    #[test]
    fn the_service_files_start_it_with_its_own_options_and_secrets() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("service");
        let read = |file: &str| std::fs::read_to_string(dir.join(file)).unwrap();
        let unit = read("monokulo-bell.service");
        let plist = read("com.monokulo.bell.plist");
        let task = read("install-windows-task.ps1");
        let started = |args: &[&str]| {
            assert!(args[0].ends_with("monokulo-bell"), "{args:?}");
            let start = parse_args(args).unwrap();
            assert!(start.options.ends_with(OPTIONS_FILE), "{args:?}");
            assert!(start.env.cli("bell.store_path").is_some(), "{args:?}");
            start
        };

        // systemd: the same command line with --check before each start.
        let exec = |key: &str| {
            let line = unit.lines().find_map(|line| line.strip_prefix(key));
            line.unwrap().split_whitespace().collect::<Vec<_>>()
        };
        let (check, run) = (exec("ExecStartPre="), exec("ExecStart="));
        assert!(started(&check).check && !started(&run).check);
        assert_eq!(check, [run.as_slice(), &["--check"]].concat());

        // launchd: ProgramArguments, one <string> each.
        let arguments = plist.split("<key>ProgramArguments</key>").nth(1).unwrap();
        let arguments = &arguments[..arguments.find("</array>").unwrap()];
        let arguments: Vec<&str> = arguments
            .split("<string>")
            .skip(1)
            .map(|rest| &rest[..rest.find("</string>").unwrap()])
            .collect();
        assert!(!started(&arguments).check);

        // Windows: the runner's own values.
        let value = |name: &str| {
            let line = task
                .lines()
                .find_map(|line| line.strip_prefix(&format!("${name} = '")));
            line.unwrap().trim_end_matches('\'').to_owned()
        };
        assert_eq!(value("Name"), "monokulo-bell");
        assert_eq!(value("OptionsFile"), OPTIONS_FILE);
        let store = value("StoreFlag");
        started(&[
            "monokulo-bell",
            "--options",
            OPTIONS_FILE,
            &store,
            "bell.db",
        ]);

        let secrets: std::collections::BTreeSet<&str> = crate::settings::ALL
            .iter()
            .filter(|setting| setting.sources().env && !setting.sources().cli)
            .map(|setting| setting.env_var())
            .collect();
        assert!(secrets.contains(value("GeneratedSecret").as_str()));
        for (file, text) in [("unit", &unit), ("plist", &plist), ("task", &task)] {
            let named: std::collections::BTreeSet<&str> = text
                .match_indices("MONOKULO_")
                .map(|(at, _)| {
                    let rest = &text[at..];
                    let end = rest
                        .find(|c: char| !(c.is_ascii_uppercase() || c == '_'))
                        .unwrap_or(rest.len());
                    &rest[..end]
                })
                .collect();
            assert_eq!(named, secrets, "{file}");
        }
    }
}
