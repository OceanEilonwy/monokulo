//! The `monokulo-push-relay` command line, with `clap`: `--options` and
//! `--init` for its options file, and an option for every setting that
//! takes one, with help from the settings' own declarations
//! (`live_settings::cli`). Its secrets are listed after the options, with
//! their environment variables.

use std::sync::LazyLock;

use clap::Command;

const ABOUT: &str = "The push relay of Monokulo, a self-hosted Monero payment gateway: it hands \
Web Push from monokulo and the Monokular Bell on to APNs and FCM.";

const LONG_ABOUT: &str =
    "The push relay of Monokulo, a self-hosted Monero payment gateway: it hands \
Web Push from monokulo and the Monokular Bell on to APNs and FCM.

An app registers its device token here with the VAPID key of the sender it wants pushes from, \
and hands the endpoint it is given to that sender. Pushes to it are accepted only when that \
sender signed them for this relay's address (server.public_uri, required). The relay keeps \
only its handles, in its own database file (store.path); it stores no push and logs no body, \
token or endpoint.

Settings are kept in the options file (--options, by default ~/.config/monokulo/relay.toml); \
--init writes one with every setting described. An option below wins over the file. Secrets \
have no option and are never kept in the file, only in their environment variable (listed \
after the options): every user on the machine can read the process list. The same settings \
run a relay inside monokulo, under its options file's [relay.*] tables.";

const EXAMPLES: &str = "Examples:
  monokulo-push-relay --init
      Write the options file, with every setting described, and say where.
  MONOKULO_RELAY_HANDLE_KEY=\"$(cat handle.key)\" monokulo-push-relay --check
      Say whether the relay would start on the options file and the environment, and on what.
  MONOKULO_RELAY_HANDLE_KEY=\"$(cat handle.key)\" MONOKULO_RELAY_APNS_KEY=\"$(cat AuthKey_ABCDE12345.p8)\" monokulo-push-relay --server-public-uri https://relay.example.org
      Start the relay, handing pushes on to APNs.";

/// The relay's options file, in `~/.config/monokulo/` unless `--options`
/// names another.
pub const OPTIONS_FILE: &str = "relay.toml";

/// `--version`: the version, and the commit it was built from when the
/// build said.
static VERSION: LazyLock<String> =
    LazyLock::new(|| service_runtime::status::version(env!("CARGO_PKG_VERSION")));

/// The command line: the options file (`--check` too), then an option per
/// setting.
pub fn command() -> Command {
    let command = Command::new("monokulo-push-relay")
        .version(VERSION.as_str())
        .about(ABOUT)
        .long_about(LONG_ABOUT)
        .after_help(EXAMPLES);
    live_settings::cli::with_check(live_settings::cli::with_settings(
        command,
        crate::settings::ALL,
        OPTIONS_FILE,
    ))
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
    Ok(live_settings::cli::start(
        &matches,
        crate::settings::ALL,
        OPTIONS_FILE,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_is_well_formed() {
        command().debug_assert();
    }

    /// Every setting that takes one is an option; the secrets are listed
    /// by their variables and have none.
    #[test]
    fn the_help_lists_every_option_and_every_secret() {
        let help = command().render_long_help().to_string();
        for setting in crate::settings::ALL {
            let flag = format!("--{} <", live_settings::cli_flag(setting.key()));
            assert_eq!(help.contains(&flag), setting.sources().cli, "{flag}");
        }
        for secret in [
            "MONOKULO_RELAY_HANDLE_KEY",
            "MONOKULO_RELAY_APNS_KEY",
            "MONOKULO_RELAY_FCM_CREDENTIALS",
        ] {
            assert!(help.contains(secret), "{secret}: {help}");
        }
        assert!(parse_args(["monokulo-push-relay", "--apns-key", "x"]).is_err());
        let args = parse_args([
            "monokulo-push-relay",
            "--server-public-uri",
            "https://relay.example.org",
        ])
        .unwrap();
        assert_eq!(
            args.env.cli("server.public_uri").as_deref(),
            Some("https://relay.example.org")
        );
        // Checked as it is parsed.
        assert!(
            parse_args(["monokulo-push-relay", "--server-public-uri", "http://x.org"]).is_err()
        );
        assert!(args.options.ends_with(OPTIONS_FILE), "{:?}", args.options);
    }
}
