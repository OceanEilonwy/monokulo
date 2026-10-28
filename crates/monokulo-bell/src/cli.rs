//! The `monokular-bell` command line, with `clap`: `--options` and
//! `--init` for its options file, and an option for every setting that
//! takes one, with help from the settings' own declarations
//! (`live_settings::cli`). Its secrets are listed after the options, with
//! their environment variables.

use clap::Command;

const ABOUT: &str = "The Monokular Bell: it turns a Square application's payment webhooks into \
Web Push dings that wake the Monokular tills at that location.";

const LONG_ABOUT: &str =
    "The Monokular Bell: it turns a Square application's payment webhooks into \
Web Push dings that wake the Monokular tills at that location.

A till registers its push subscription here for one of its merchant's locations, with the \
merchant's Square access token, which the Bell uses once to check the location with Square and \
never keeps. Square's webhooks come to bell.public_uri followed by /v1/square/webhook, signed \
with the subscription's key; a completed payment dings that location's tills. The Bell keeps \
only the registrations, in its own database file (bell.store_path): no webhook, no payment and \
no token is stored or logged.

Settings are kept in the options file (--options, by default ~/.config/monokulo/bell.toml); \
--init writes one with every setting described. An option below wins over the file. Secrets \
have no option and are never kept in the file, only in their environment variable (listed \
after the options): every user on the machine can read the process list.";

const EXAMPLES: &str = "Examples:
  monokular-bell --init
      Write the options file, with every setting described, and say where.
  MONOKULAR_BELL_SQUARE_WEBHOOK_KEY=… MONOKULAR_BELL_VAPID_KEY=$(openssl rand -hex 32) \\
    monokular-bell --bell-public-uri https://bell.example.org --bell-square-application-id sq0idp-…
      Start the Bell. Keep the VAPID key: the tills' subscriptions are made for it.";

/// The Bell's options file, in `~/.config/monokulo/` unless `--options`
/// names another.
pub const OPTIONS_FILE: &str = "bell.toml";

/// The command line: the options file, then an option per setting.
pub fn command() -> Command {
    let command = Command::new("monokular-bell")
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

    /// Every setting that takes one is an option; both secrets are listed
    /// by their variables and have none.
    #[test]
    fn the_help_lists_every_option_and_both_secrets() {
        let help = command().render_long_help().to_string();
        for setting in crate::settings::ALL {
            let flag = format!("--{} <", live_settings::cli_flag(setting.key()));
            assert_eq!(help.contains(&flag), setting.sources().cli, "{flag}");
        }
        for secret in [
            "MONOKULAR_BELL_SQUARE_WEBHOOK_KEY",
            "MONOKULAR_BELL_VAPID_KEY",
        ] {
            assert!(help.contains(secret), "{secret}: {help}");
        }
        assert!(parse_args(["monokular-bell", "--bell-vapid-key", "x"]).is_err());
        let args = parse_args([
            "monokular-bell",
            "--bell-public-uri",
            "https://bell.example.org",
        ])
        .unwrap();
        assert_eq!(
            args.env.cli("bell.public_uri").as_deref(),
            Some("https://bell.example.org")
        );
        // Checked as it is parsed.
        assert!(parse_args(["monokular-bell", "--bell-public-uri", "http://x.org"]).is_err());
        assert!(args.options.ends_with(OPTIONS_FILE), "{:?}", args.options);
    }
}
