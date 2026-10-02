//! The `monokulo-engine` command line, with `clap`: `--options` and `--init`
//! for the options file, and an option for every setting that takes one,
//! with help from the settings' own declarations (`live_settings::cli`).
//! Kept in the lib crate (not `main.rs`) so it's unit-testable the normal
//! way. The engine has no one-off commands: stores are created, read and
//! changed by monokulo, through the admin API.
//!
//! A setting comes from its environment variable (secrets only), then its
//! option, then the options file (or, for the runtime switches, the
//! database), then its default.

use clap::{ArgMatches, Command};

const ABOUT: &str = "The engine of Monokulo, a self-hosted Monero payment gateway: it \
watches each network's chain for stores' payments. Only monokulo should reach it.";

const LONG_ABOUT: &str = "The engine of Monokulo, a self-hosted Monero payment gateway: it \
watches each network's chain for stores' payments. Only monokulo should reach it.

Settings are kept in the options file (--options, by default \
~/.config/monokulo/engine.toml), which the admin settings page edits and reloads without a \
restart; --init writes one with every setting described. An option below wins over the file \
and locks that setting on the admin page. Secrets have no option and are never kept in the \
file, only in their environment variable (listed after the options): every user on the \
machine can read the process list. ENGINE_TOKEN is required.";

const EXAMPLES: &str = "Examples:
  monokulo-engine --init
      Write the options file, with every setting described, and say where.
  ENGINE_TOKEN=$(cat engine.token) monokulo-engine
      Start the server.
  ENGINE_TOKEN=$(cat engine.token) monokulo-engine --server-bind 10.0.0.2:8443 --monero-node-strict-tls true
      Start it on a private address, refusing self-signed node certificates.";

/// The command line: the options file, then an option per setting.
pub fn command() -> Command {
    let command = Command::new("monokulo-engine")
        .version(env!("CARGO_PKG_VERSION"))
        .about(ABOUT)
        .long_about(LONG_ABOUT)
        .after_help(EXAMPLES);
    live_settings::cli::with_settings(command, crate::engine_settings::ALL, OPTIONS_FILE)
}

/// Parses the full argument list, argv[0] included, into the settings it
/// gives, the options file to read, and whether to write one instead
/// (`--init`). Help, the version and mistakes come back as clap's error,
/// which the caller prints and exits on (`clap::Error::exit`).
pub fn parse_args<I, T>(args: I) -> Result<live_settings::cli::Start, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let matches = command().try_get_matches_from(args)?;
    Ok(start(&matches))
}

fn start(matches: &ArgMatches) -> live_settings::cli::Start {
    live_settings::cli::start(matches, crate::engine_settings::ALL, OPTIONS_FILE)
}

/// The engine's options file, in `~/.config/monokulo/` unless `--options`
/// names another.
pub const OPTIONS_FILE: &str = "engine.toml";

/// The database file: `database.path`, from its option or the options
/// file, otherwise `~/.local/share/monokulo/engine.db` (or `engine.db` in
/// the working directory if that can't be used).
pub fn database_path(start: &live_settings::Snapshot) -> std::path::PathBuf {
    let path = start.get(&crate::engine_settings::DATABASE_PATH);
    if start.source(&crate::engine_settings::DATABASE_PATH) == live_settings::SettingSource::Default
    {
        live_settings::paths::usable_or_cwd(Some(path), "engine.db")
    } else {
        path
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Result<live_settings::cli::Start, clap::Error> {
        parse_args(std::iter::once("monokulo-engine").chain(argv.iter().copied()))
    }

    #[test]
    fn the_command_line_is_well_formed() {
        command().debug_assert();
    }

    #[test]
    fn no_arguments_starts_the_server_from_the_default_options_file() {
        let start = parse(&[]).unwrap();
        assert!(!start.init);
        assert!(start.options.ends_with(OPTIONS_FILE), "{:?}", start.options);
    }

    /// A setting given as an option reaches the settings, checked as it is
    /// parsed; the old `--strict-tls` is the setting's own option now.
    #[test]
    fn a_setting_can_be_given_as_an_option() {
        let start = parse(&[
            "--monero-node-strict-tls",
            "true",
            "--database-path",
            "/data/e.db",
        ])
        .unwrap();
        let snapshot = live_settings::Snapshot::new(Default::default(), start.env.clone());
        assert_eq!(
            database_path(&snapshot),
            std::path::PathBuf::from("/data/e.db")
        );
        assert_eq!(
            start.env.cli("monero_node.strict_tls").as_deref(),
            Some("true")
        );
        let refused = parse(&["--payment-reorg-check-depth", "0"])
            .unwrap_err()
            .to_string();
        assert!(refused.contains("--payment-reorg-check-depth"), "{refused}");
        assert!(parse(&["--strict-tls"]).is_err(), "the old flag is gone");
    }

    #[test]
    fn an_unrecognized_bare_argument_is_a_clear_error_not_silently_ignored() {
        // A leftover config-file-path-shaped argument (from the removed
        // `--config`/positional-path era) must not be silently accepted and
        // ignored - that would look like it worked while doing nothing.
        assert!(parse(&["moneropay.toml"]).is_err());
        // The one-off commands are gone: stores are created, read and
        // changed by monokulo, through the admin API.
        for removed in [
            "--bootstrap-wallet",
            "--rotate-secret",
            "--show-tenant",
            "--pk",
        ] {
            assert!(parse(&[removed]).is_err(), "{removed}");
        }
    }

    #[test]
    fn help_and_version_are_answered_by_clap() {
        for argv in [
            &["--help"][..],
            &["-h"],
            &["--init", "--help"],
            &["--version"],
        ] {
            let kind = parse(argv).unwrap_err().kind();
            assert!(
                matches!(
                    kind,
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                ),
                "{argv:?}: {kind:?}"
            );
        }
    }

    /// The help names the options file and every setting's option, with
    /// the secrets' environment variables.
    #[test]
    fn the_help_documents_the_options_file_and_every_setting() {
        let help = command().render_long_help().to_string();
        for flag in ["--options", "--init", "--help"] {
            assert!(help.contains(flag), "help should mention {flag}");
        }
        // An option for each setting that takes one; a secret's variable is
        // listed after them, with no option (the process list shows options).
        for setting in crate::engine_settings::ALL {
            let flag = format!("--{}", live_settings::cli_flag(setting.key()));
            assert_eq!(
                help.contains(&flag),
                setting.sources().cli,
                "help and {flag}"
            );
            assert!(
                help.contains(setting.env_var()),
                "help should mention {}",
                setting.env_var()
            );
        }
    }

    /// The database is under ~/.local/share/monokulo unless the options
    /// file or the command line says otherwise.
    #[test]
    fn the_database_path_follows_xdg_unless_given() {
        let none = live_settings::Env::fixed(Vec::<(String, String)>::new());
        let from = |file: &[(&str, &str)], env: live_settings::Env| {
            let stored = file
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            database_path(&live_settings::Snapshot::new(stored, env))
        };
        let default = from(&[], none.clone());
        assert!(
            default == std::path::Path::new("engine.db") || default.ends_with("monokulo/engine.db"),
            "{default:?}"
        );
        assert_eq!(
            from(&[("database.path", "/srv/engine.db")], none.clone()),
            std::path::PathBuf::from("/srv/engine.db")
        );
        let flagged =
            none.with_cli([("database.path".to_string(), "/cli/engine.db".to_string())].into());
        assert_eq!(
            from(&[("database.path", "/srv/engine.db")], flagged),
            std::path::PathBuf::from("/cli/engine.db"),
            "the option wins over the file"
        );
    }
}
