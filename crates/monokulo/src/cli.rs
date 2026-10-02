//! The `monokulo` command line, with `clap`: an option for every setting
//! that takes one, with help from the settings' own declarations
//! (`live_settings::cli`). A setting comes from its option, then its
//! environment variable, then the value saved on the admin page, then its
//! default.

use clap::Command;

const ABOUT: &str = "Monokulo, a self-hosted Monero payment gateway: the web app merchants, \
their customers and their shops' plugins use. It talks to its engine, which watches the chain.";

const LONG_ABOUT: &str = "Monokulo, a self-hosted Monero payment gateway: the web app merchants, \
their customers and their shops' plugins use. It talks to its engine, which watches the chain.

Every setting below can be given as an option, as its environment variable, or (unless it \
says it isn't saved) on the admin settings page, which applies it without a restart. An \
option wins over the environment variable, which wins over the saved value, which wins over \
the default. MONOKULO_ENCRYPTION_KEY and MONOKULO_ENGINE_TOKEN (or their options) are \
required.";

const EXAMPLES: &str = "Examples:
  MONOKULO_ENCRYPTION_KEY=$(cat monokulo.key) MONOKULO_ENGINE_TOKEN=$(cat engine.token) monokulo
      Start monokulo, with its engine on this machine.
  ... monokulo --server-bind 0.0.0.0:8081 --engine-url http://engine:8443
      Listen on every address, with the engine on another host.";

/// The command line: an option per setting.
pub fn command() -> Command {
    let command = Command::new("monokulo")
        .version(env!("CARGO_PKG_VERSION"))
        .about(ABOUT)
        .long_about(LONG_ABOUT)
        .after_help(EXAMPLES);
    live_settings::cli::with_settings(command, crate::settings::ALL)
}

/// Parses the full argument list, argv[0] included, into the command line
/// and environment the settings resolve against. Help, the version and
/// mistakes come back as clap's error, which the caller prints and exits on
/// (`clap::Error::exit`).
pub fn parse_args<I, T>(args: I) -> Result<live_settings::Env, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let matches = command().try_get_matches_from(args)?;
    Ok(live_settings::Env::process()
        .with_cli(live_settings::cli::values(&matches, crate::settings::ALL)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_is_well_formed() {
        command().debug_assert();
    }

    /// Every setting is an option, named in the help with its environment
    /// variable; a value is checked as it is parsed.
    #[test]
    fn every_setting_is_an_option_and_a_bad_value_is_refused() {
        let help = command().render_long_help().to_string();
        for setting in crate::settings::ALL {
            let flag = format!("--{}", live_settings::cli_flag(setting.key()));
            assert!(help.contains(&flag), "help should mention {flag}");
            assert!(
                help.contains(setting.env_var()),
                "help should mention {}",
                setting.env_var()
            );
        }
        let env = parse_args(["monokulo", "--server-bind", "0.0.0.0:9000"]).unwrap();
        assert_eq!(env.cli("server.bind").as_deref(), Some("0.0.0.0:9000"));
        let refused = parse_args(["monokulo", "--abuse-challenge-bits", "lots"])
            .unwrap_err()
            .to_string();
        assert!(refused.contains("--abuse-challenge-bits"), "{refused}");
        assert!(parse_args(["monokulo", "stray"]).is_err());
    }
}
