//! The `monokulo` command line, with `clap`: `--options` and `--init` for
//! the options file, and an option for every setting that takes one, with
//! help from the settings' own declarations (`live_settings::cli`). The
//! engine that runs inside monokulo (`engine.mode = "embedded"`, the
//! default) has its settings here too, each as `--engine-…`.
//!
//! A setting comes from its environment variable (secrets only), then its
//! option, then the options file (or, for the runtime switches, the
//! database), then its default.

use std::collections::HashMap;

use clap::{ArgMatches, Command};

const ABOUT: &str = "Monokulo, a self-hosted Monero payment gateway: the web app merchants, \
their customers and their shops' plugins use, and the engine that watches the chain.";

const LONG_ABOUT: &str = "Monokulo, a self-hosted Monero payment gateway: the web app merchants, \
their customers and their shops' plugins use, and the engine that watches the chain.

The engine runs inside monokulo unless engine.mode is remote, when monokulo reaches a \
separate monokulo-engine at engine.url with the engine token (MONOKULO_ENGINE_TOKEN).

Settings are kept in the options file (--options, by default ~/.config/monokulo/monokulo.toml), \
which the admin settings page edits and reloads without a restart; --init writes one with \
every setting described, the engine's under [engine.*]. An option below wins over the file \
and locks that setting on the admin page. Secrets have no option and are never kept in the \
file, only in their environment variable (listed after the options): every user on the \
machine can read the process list. MONOKULO_ENCRYPTION_KEY is required.";

const EXAMPLES: &str = "Examples:
  monokulo --init
      Write the options file, with every setting described, and say where.
  MONOKULO_ENCRYPTION_KEY=$(cat monokulo.key) monokulo
      Start monokulo, with its engine inside it.
  ... monokulo --server-bind 0.0.0.0:8081 --engine-payment-confirmations-required 3
      Listen on every address; payments need 3 confirmations unless a store says otherwise.
  ... MONOKULO_ENGINE_TOKEN=$(cat engine.token) monokulo --engine-mode remote --engine-url http://engine:8443
      Use a separate engine on another host.";

/// monokulo's options file, in `~/.config/monokulo/` unless `--options`
/// names another.
pub const OPTIONS_FILE: &str = "monokulo.toml";

/// The command line: the options file, then an option per setting, then
/// the embedded engine's.
pub fn command() -> Command {
    let command = Command::new("monokulo")
        .version(env!("CARGO_PKG_VERSION"))
        .about(ABOUT)
        .long_about(LONG_ABOUT)
        .after_help(EXAMPLES);
    let command = live_settings::cli::with_settings(command, crate::settings::ALL, OPTIONS_FILE);
    #[cfg(feature = "embedded-engine")]
    let command = live_settings::cli::with_nested_settings(
        command,
        crate::settings::ENGINE_TABLE,
        &engine::engine_settings::embedded_settings(),
    );
    command
}

/// What the command line asks for.
#[derive(Debug)]
pub struct Args {
    /// monokulo's own settings given as options, the options file, and
    /// `--init`.
    pub start: live_settings::cli::Start,
    /// The embedded engine's settings given as `--engine-…` options, by
    /// the engine's own keys.
    pub engine: HashMap<String, String>,
}

/// Parses the full argument list, argv[0] included, into the settings it
/// gives, the options file to read, and whether to write one instead
/// (`--init`). Help, the version and mistakes come back as clap's error,
/// which the caller prints and exits on (`clap::Error::exit`).
pub fn parse_args<I, T>(args: I) -> Result<Args, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let matches = command().try_get_matches_from(args)?;
    Ok(Args {
        start: start(&matches),
        engine: engine_values(&matches),
    })
}

fn start(matches: &ArgMatches) -> live_settings::cli::Start {
    live_settings::cli::start(matches, crate::settings::ALL, OPTIONS_FILE)
}

#[cfg(feature = "embedded-engine")]
fn engine_values(matches: &ArgMatches) -> HashMap<String, String> {
    live_settings::cli::nested_values(
        matches,
        crate::settings::ENGINE_TABLE,
        &engine::engine_settings::embedded_settings(),
    )
}

#[cfg(not(feature = "embedded-engine"))]
fn engine_values(_matches: &ArgMatches) -> HashMap<String, String> {
    HashMap::new()
}

/// `--init`: writes the commented options file at `path`, refusing to
/// replace one, with the embedded engine's settings under `[engine.*]`, and
/// says where it went (on standard output, for the person who asked).
pub fn write_init(path: &std::path::Path) -> Result<(), String> {
    #[cfg(feature = "embedded-engine")]
    let text = live_settings::render_init_nested(
        "monokulo",
        crate::settings::ALL,
        crate::settings::ENGINE_TABLE,
        "The engine's own settings, used when it runs inside monokulo (engine.mode = \"embedded\", the default). With a remote engine they belong in that engine's own options file instead, and are refused here.",
        &engine::engine_settings::embedded_settings(),
    );
    #[cfg(not(feature = "embedded-engine"))]
    let text = live_settings::render_init("monokulo", crate::settings::ALL);
    live_settings::write_init(path, &text)?;
    #[expect(
        clippy::print_stdout,
        reason = "--init says where it wrote the file, to the person who asked"
    )]
    {
        println!("Wrote {}", path.display());
    }
    Ok(())
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
        let args = parse_args(["monokulo", "--server-bind", "0.0.0.0:9000"]).unwrap();
        assert_eq!(
            args.start.env.cli("server.bind").as_deref(),
            Some("0.0.0.0:9000")
        );
        assert!(!args.start.init);
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

    /// The embedded engine's settings are monokulo's `--engine-…` options,
    /// handed to the engine by its own keys; the standalone engine's own
    /// server and logging settings aren't offered.
    #[cfg(feature = "embedded-engine")]
    #[test]
    fn the_embedded_engines_settings_are_engine_options() {
        let args = parse_args([
            "monokulo",
            "--engine-payment-confirmations-required",
            "3",
            "--server-bind",
            "0.0.0.0:9000",
        ])
        .unwrap();
        assert_eq!(
            args.engine,
            HashMap::from([(
                "payment.confirmations_required".to_string(),
                "3".to_string()
            )])
        );
        assert_eq!(
            args.start.env.cli("server.bind").as_deref(),
            Some("0.0.0.0:9000"),
            "monokulo's own"
        );
        for standalone in ["--engine-server-bind", "--engine-logging-level"] {
            assert!(
                parse_args(["monokulo", standalone, "x"]).is_err(),
                "{standalone} is the standalone engine's"
            );
        }
        let refused = parse_args([
            "monokulo",
            "--engine-payment-confirmations-required",
            "lots",
        ])
        .unwrap_err()
        .to_string();
        assert!(
            refused.contains("--engine-payment-confirmations-required"),
            "{refused}"
        );
    }

    /// `--options` names the file; without it, the default is in the
    /// XDG config directory, or the working directory.
    #[test]
    fn the_options_file_is_named_or_defaults_to_the_config_directory() {
        let args = parse_args(["monokulo", "--options", "/etc/mine.toml", "--init"]).unwrap();
        assert_eq!(
            args.start.options,
            std::path::PathBuf::from("/etc/mine.toml")
        );
        assert!(args.start.init);
        let args = parse_args(["monokulo"]).unwrap();
        assert!(
            args.start.options.ends_with(OPTIONS_FILE),
            "{:?}",
            args.start.options
        );
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
