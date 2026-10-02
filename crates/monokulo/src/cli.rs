//! The `monokulo` command line, with `clap`: `--options` and `--init` for
//! the options file, and an option for every setting that takes one, with
//! help from the settings' own declarations (`live_settings::cli`).
//!
//! A setting comes from its environment variable (secrets only), then its
//! option, then the options file (or, for the runtime switches, the
//! database), then its default.

use clap::{ArgMatches, Command};

const ABOUT: &str = "Monokulo, a self-hosted Monero payment gateway: the web app merchants, \
their customers and their shops' plugins use. It talks to its engine, which watches the chain.";

const LONG_ABOUT: &str = "Monokulo, a self-hosted Monero payment gateway: the web app merchants, \
their customers and their shops' plugins use. It talks to its engine, which watches the chain.

Settings are kept in the options file (--options, by default ~/.config/monokulo/monokulo.toml), \
which the admin settings page edits and reloads without a restart; --init writes one with \
every setting described. An option below wins over the file and locks that setting on the \
admin page. Secrets have no option and are never kept in the file, only in their environment \
variable (listed after the options): every user on the machine can read the process list. \
MONOKULO_ENCRYPTION_KEY and MONOKULO_ENGINE_TOKEN are required.";

const EXAMPLES: &str = "Examples:
  monokulo --init
      Write the options file, with every setting described, and say where.
  MONOKULO_ENCRYPTION_KEY=$(cat monokulo.key) MONOKULO_ENGINE_TOKEN=$(cat engine.token) monokulo
      Start monokulo, with its engine on this machine.
  ... monokulo --server-bind 0.0.0.0:8081 --engine-url http://engine:8443
      Listen on every address, with the engine on another host.";

/// monokulo's options file, in `~/.config/monokulo/` unless `--options`
/// names another.
pub const OPTIONS_FILE: &str = "monokulo.toml";

/// The command line: the options file, then an option per setting.
pub fn command() -> Command {
    let command = Command::new("monokulo")
        .version(env!("CARGO_PKG_VERSION"))
        .about(ABOUT)
        .long_about(LONG_ABOUT)
        .after_help(EXAMPLES);
    live_settings::cli::with_settings(command, crate::settings::ALL, OPTIONS_FILE)
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
    live_settings::cli::start(matches, crate::settings::ALL, OPTIONS_FILE)
}

/// The database file: `database.path`, from its option or the options
/// file, otherwise `~/.local/share/monokulo/monokulo.db` (or `monokulo.db`
/// in the working directory if that can't be used).
pub fn database_path(start: &live_settings::Snapshot) -> std::path::PathBuf {
    let path = start.get(&crate::settings::DATABASE_PATH);
    if start.source(&crate::settings::DATABASE_PATH) == live_settings::SettingSource::Default {
        live_settings::paths::usable_or_cwd(Some(path), "monokulo.db")
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_is_well_formed() {
        command().debug_assert();
    }

    /// Every setting that takes one is an option; a secret has only its
    /// environment variable, named after the options. A value is checked as
    /// it is parsed.
    #[test]
    fn every_setting_is_an_option_and_a_bad_value_is_refused() {
        let help = command().render_long_help().to_string();
        for setting in crate::settings::ALL {
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
        for flag in ["--options", "--init"] {
            assert!(help.contains(flag), "help should mention {flag}");
        }
        let start = parse_args(["monokulo", "--server-bind", "0.0.0.0:9000"]).unwrap();
        assert_eq!(
            start.env.cli("server.bind").as_deref(),
            Some("0.0.0.0:9000")
        );
        assert!(!start.init);
        let refused = parse_args(["monokulo", "--abuse-challenge-bits", "lots"])
            .unwrap_err()
            .to_string();
        assert!(refused.contains("--abuse-challenge-bits"), "{refused}");
        assert!(parse_args(["monokulo", "stray"]).is_err());
        assert!(
            parse_args(["monokulo", "--engine-token", "x"]).is_err(),
            "a secret has no option"
        );
    }

    /// `--options` names the file; without it, the default is in the
    /// XDG config directory, or the working directory.
    #[test]
    fn the_options_file_is_named_or_defaults_to_the_config_directory() {
        let start = parse_args(["monokulo", "--options", "/etc/mine.toml", "--init"]).unwrap();
        assert_eq!(start.options, std::path::PathBuf::from("/etc/mine.toml"));
        assert!(start.init);
        let start = parse_args(["monokulo"]).unwrap();
        assert!(start.options.ends_with(OPTIONS_FILE), "{:?}", start.options);
    }

    /// The database follows XDG unless it is given.
    #[test]
    fn the_database_path_follows_xdg_unless_given() {
        let none = live_settings::Env::fixed(Vec::<(String, String)>::new());
        let path = database_path(&live_settings::Snapshot::new(
            Default::default(),
            none.clone(),
        ));
        assert!(path.ends_with("monokulo.db"), "{path:?}");
        let given = none.with_cli([("database.path".to_string(), "/data/m.db".to_string())].into());
        assert_eq!(
            database_path(&live_settings::Snapshot::new(Default::default(), given)),
            std::path::PathBuf::from("/data/m.db")
        );
    }
}
