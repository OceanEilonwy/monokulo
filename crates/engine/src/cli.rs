//! The `monokulo-engine` command line, with `clap`: the one-off commands
//! (`--bootstrap-wallet`, `--rotate-secret`, `--show-tenant`) and an option
//! for every setting that takes one, with help from the settings' own
//! declarations (`live_settings::cli`). Kept in the lib crate (not
//! `main.rs`) so it's unit-testable the normal way, and so `main.rs` stays a
//! thin wrapper around whatever this decides the process should do.
//!
//! A setting comes from its environment variable (secrets only), then its
//! option, then the options file (or, for the runtime switches, the
//! database), then its default. Every mode below agrees on where the
//! database is: `database.path` ([`database_path`]).

use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::local_admin::BootstrapWalletArgs;

#[derive(Debug)]
pub enum Action {
    RunServer,
    /// Mints a fresh admin secret for a tenant, invalidating the old one - see
    /// `local_admin::rotate_secret`.
    RotateSecret {
        pk: Option<String>,
    },
    /// Prints a tenant's non-secret settings - see `local_admin::show_tenant`.
    ShowTenant {
        pk: Option<String>,
    },
    /// Provisions the one tenant a self-hosted deployment needs, replacing what
    /// used to be the `[wallet]` section of the (now-removed) TOML config file -
    /// see `local_admin::bootstrap_wallet`.
    BootstrapWallet(BootstrapWalletCommand),
}

/// What the command line asked for: the mode, and the settings it gave,
/// over the process environment (what the settings resolve against).
#[derive(Debug)]
pub struct Invocation {
    pub action: Action,
    /// The settings given on the command line, the options file to read,
    /// and whether to write one instead (`--init`).
    pub start: live_settings::cli::Start,
}

/// `--bootstrap-wallet` as parsed: everything `local_admin::bootstrap_wallet`
/// needs except the private view key itself, which is read from a file (or
/// standard input) by [`BootstrapWalletCommand::read_view_key`] - never
/// taken from the argument list, where every user on the machine can read
/// it (`ps`, `/proc/*/cmdline`) and the shell's history keeps it.
#[derive(Debug, PartialEq)]
pub struct BootstrapWalletCommand {
    pub primary_address: String,
    /// Where the hex-encoded private view key is read from; `-` is standard
    /// input.
    pub view_key_file: String,
    pub spend_pubkey_hex: String,
    pub network: String,
    pub key_custody_backend: Option<String>,
}

impl BootstrapWalletCommand {
    /// Reads the view key from where `--view-key-file` points, trimming
    /// the newline an editor or `echo` leaves.
    pub fn read_view_key(self) -> Result<BootstrapWalletArgs, String> {
        let raw = if self.view_key_file == "-" {
            let mut raw = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut raw)
                .map_err(|e| format!("could not read the view key from standard input: {e}"))?;
            raw
        } else {
            std::fs::read_to_string(&self.view_key_file).map_err(|e| {
                format!(
                    "could not read the view key from {:?}: {e}",
                    self.view_key_file
                )
            })?
        };
        let view_key_hex = raw.trim().to_string();
        if view_key_hex.is_empty() {
            return Err(format!(
                "the view key file {:?} is empty",
                self.view_key_file
            ));
        }
        Ok(BootstrapWalletArgs {
            primary_address: self.primary_address,
            view_key_hex,
            spend_pubkey_hex: self.spend_pubkey_hex,
            network: self.network,
            key_custody_backend: self.key_custody_backend,
        })
    }
}

const ABOUT: &str = "The engine of Monokulo, a self-hosted Monero payment gateway: it \
watches each network's chain for stores' payments. Only monokulo should reach it.";

const LONG_ABOUT: &str = "The engine of Monokulo, a self-hosted Monero payment gateway: it \
watches each network's chain for stores' payments. Only monokulo should reach it.

With none of the one-off commands, it starts the server. Settings are kept in the options \
file (--options, by default ~/.config/monokulo/engine.toml), which the admin settings page \
edits and reloads without a restart; --init writes one with every setting described. An \
option below wins over the file and locks that setting on the admin page. Secrets have no \
option and are never kept in the file, only in their environment variable (listed after the \
options): every user on the machine can read the process list. ENGINE_TOKEN is required.";

const EXAMPLES: &str = "Examples:
  monokulo-engine --init
      Write the options file, with every setting described, and say where.
  ENGINE_TOKEN=$(cat engine.token) monokulo-engine
      Start the server.
  ENGINE_TOKEN=$(cat engine.token) monokulo-engine --server-bind 10.0.0.2:8443 --monero-node-strict-tls true
      Start it on a private address, refusing self-signed node certificates.
  monokulo-engine --bootstrap-wallet --primary-address 4... --view-key-file view.key --spend-pubkey <hex>
      Provision the one self-hosted tenant.
  printf '%s' <hex> | monokulo-engine --bootstrap-wallet ... --view-key-file -
      The same, with the key on standard input.
  monokulo-engine --rotate-secret
      Mint a fresh admin secret for the sole tenant.";

/// The one-off commands' heading in the help.
const COMMANDS: &str = "One-off commands";

/// The command line: the one-off commands, then an option per setting.
pub fn command() -> Command {
    let only_for_bootstrap = |arg: Arg| arg.requires("bootstrap-wallet").help_heading(COMMANDS);
    let command = Command::new("monokulo-engine")
        .version(env!("CARGO_PKG_VERSION"))
        .about(ABOUT)
        .long_about(LONG_ABOUT)
        .after_help(EXAMPLES)
        .arg(
            Arg::new("bootstrap-wallet")
                .long("bootstrap-wallet")
                .action(ArgAction::SetTrue)
                .requires_all(["primary-address", "view-key-file", "spend-pubkey"])
                .conflicts_with_all(["rotate-secret", "show-tenant"])
                .help_heading(COMMANDS)
                .help("Create the one tenant a self-hosted deployment needs, from a watch-only view key and spend public key, then exit. Refuses if a tenant already exists."),
        )
        .arg(only_for_bootstrap(
            Arg::new("primary-address")
                .long("primary-address")
                .value_name("ADDRESS")
                .help("The wallet's own primary address. It must be the wallet of the given keys on the given network."),
        ))
        .arg(only_for_bootstrap(
            Arg::new("view-key-file")
                .long("view-key-file")
                .value_name("PATH")
                .help("A file holding the wallet's private view key, hex-encoded; - reads it from standard input. Never the key itself: the argument list is readable by every user on the machine and kept by the shell's history."),
        ))
        .arg(
            Arg::new("view-key")
                .long("view-key")
                .hide(true)
                .value_parser(|_: &str| -> Result<String, String> {
                    Err("a key in the argument list is readable by every user on the machine - put it in a file and pass --view-key-file <PATH> (or - for standard input)".to_string())
                }),
        )
        .arg(only_for_bootstrap(
            Arg::new("spend-pubkey")
                .long("spend-pubkey")
                .value_name("HEX")
                .help("The wallet's public spend key, hex-encoded: the public half only, never the private spend key."),
        ))
        .arg(only_for_bootstrap(
            Arg::new("network")
                .long("network")
                .value_parser(["mainnet", "stagenet", "testnet"])
                .default_value("mainnet")
                .help("Which network the bootstrap tenant watches."),
        ))
        .arg(only_for_bootstrap(
            Arg::new("key-custody-backend")
                .long("key-custody-backend")
                .value_name("BACKEND")
                .help("Where the wallet's keys are kept: one of the enabled key custody backends. key_custody.default_backend when not given."),
        ))
        .arg(
            Arg::new("rotate-secret")
                .long("rotate-secret")
                .action(ArgAction::SetTrue)
                .conflicts_with("show-tenant")
                .help_heading(COMMANDS)
                .help("Mint a fresh admin secret (sk_...) for a tenant, invalidating the old one, then exit: the only way back in if it's lost."),
        )
        .arg(
            Arg::new("show-tenant")
                .long("show-tenant")
                .action(ArgAction::SetTrue)
                .help_heading(COMMANDS)
                .help("Print a tenant's current settings (public key, network, address, thresholds), never its keys or secret, then exit."),
        )
        .arg(
            Arg::new("pk")
                .long("pk")
                .value_name("PK")
                .help_heading(COMMANDS)
                .help("Which tenant --rotate-secret or --show-tenant act on. Only needed when there is more than one."),
        );
    live_settings::cli::with_settings(command, crate::engine_settings::ALL, OPTIONS_FILE)
}

/// Parses the full argument list, argv[0] included. Help, the version and
/// mistakes come back as clap's error, which the caller prints and exits on
/// (`clap::Error::exit`).
pub fn parse_args<I, T>(args: I) -> Result<Invocation, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let matches = command().try_get_matches_from(args)?;
    Ok(invocation(&matches))
}

fn invocation(matches: &ArgMatches) -> Invocation {
    let text = |id: &str| matches.get_one::<String>(id).cloned();
    let action = if matches.get_flag("bootstrap-wallet") {
        // `requires_all` above: clap has refused the command without them.
        Action::BootstrapWallet(BootstrapWalletCommand {
            primary_address: text("primary-address").unwrap_or_default(),
            view_key_file: text("view-key-file").unwrap_or_default(),
            spend_pubkey_hex: text("spend-pubkey").unwrap_or_default(),
            network: text("network").unwrap_or_else(|| "mainnet".to_string()),
            key_custody_backend: text("key-custody-backend"),
        })
    } else if matches.get_flag("rotate-secret") {
        Action::RotateSecret { pk: text("pk") }
    } else if matches.get_flag("show-tenant") {
        Action::ShowTenant { pk: text("pk") }
    } else {
        Action::RunServer
    };
    Invocation {
        action,
        start: live_settings::cli::start(matches, crate::engine_settings::ALL, OPTIONS_FILE),
    }
}

/// The engine's options file, in `~/.config/monokulo/` unless `--options`
/// names another.
pub const OPTIONS_FILE: &str = "engine.toml";

/// The database file every mode reads: `database.path`, from its option or
/// the options file, otherwise `~/.local/share/monokulo/engine.db` (or
/// `engine.db` in the working directory if that can't be used).
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

    fn parse(argv: &[&str]) -> Result<Invocation, clap::Error> {
        let matches = command()
            .try_get_matches_from(std::iter::once("monokulo-engine").chain(argv.iter().copied()))?;
        Ok(invocation(&matches))
    }

    #[test]
    fn the_command_line_is_well_formed() {
        command().debug_assert();
    }

    #[test]
    fn no_arguments_starts_the_server() {
        assert!(matches!(parse(&[]).unwrap().action, Action::RunServer));
    }

    /// A setting given as an option reaches the settings, checked as it is
    /// parsed; the old `--strict-tls` is the setting's own option now.
    #[test]
    fn a_setting_can_be_given_as_an_option() {
        let invocation = parse(&[
            "--monero-node-strict-tls",
            "true",
            "--database-path",
            "/data/e.db",
        ])
        .unwrap();
        assert!(matches!(invocation.action, Action::RunServer));
        let start = live_settings::Snapshot::new(Default::default(), invocation.start.env.clone());
        assert_eq!(
            database_path(&start),
            std::path::PathBuf::from("/data/e.db")
        );
        assert_eq!(
            invocation
                .start
                .env
                .cli("monero_node.strict_tls")
                .as_deref(),
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
    }

    #[test]
    fn help_and_version_are_answered_by_clap() {
        for argv in [
            &["--help"][..],
            &["-h"],
            &["--rotate-secret", "--help"],
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

    /// The help names every command and every setting's option, with its
    /// environment variable.
    #[test]
    fn the_help_documents_the_commands_and_every_setting() {
        let help = command().render_long_help().to_string();
        for flag in [
            "--bootstrap-wallet",
            "--primary-address",
            "--view-key-file",
            "--spend-pubkey",
            "--network",
            "--rotate-secret",
            "--show-tenant",
            "--pk",
            "--help",
        ] {
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
        assert!(
            !help.contains("--view-key "),
            "the refused flag stays hidden"
        );
    }

    #[test]
    fn rotate_secret_defaults_to_no_pk_and_show_tenant_takes_one() {
        match parse(&["--rotate-secret"]).unwrap().action {
            Action::RotateSecret { pk } => assert!(pk.is_none()),
            other => panic!("expected RotateSecret, got {other:?}"),
        }
        match parse(&["--show-tenant", "--pk", "pk_abc"]).unwrap().action {
            Action::ShowTenant { pk } => assert_eq!(pk.as_deref(), Some("pk_abc")),
            other => panic!("expected ShowTenant, got {other:?}"),
        }
    }

    /// A mistyped flag, or a bootstrap-only one, with `--rotate-secret`/
    /// `--show-tenant` is refused, not ignored: ignored, it would act on the
    /// default tenant.
    #[test]
    fn the_local_admin_modes_refuse_unknown_arguments() {
        for argv in [
            &["--rotate-secret", "--pks", "pk_x"][..],
            &["--show-tenant", "--network", "stagenet"][..],
            &["--rotate-secret", "--pk"][..],
            &["--rotate-secret", "--show-tenant"][..],
        ] {
            assert!(parse(argv).is_err(), "{argv:?}");
        }
    }

    #[test]
    fn bootstrap_wallet_requires_the_three_key_material_flags() {
        assert!(parse(&["--bootstrap-wallet"]).is_err());
        assert!(parse(&["--bootstrap-wallet", "--primary-address", "4abc"]).is_err());
        assert!(parse(&[
            "--bootstrap-wallet",
            "--primary-address",
            "4abc",
            "--view-key-file",
            "view.key",
            "--spend-pubkey",
            "bb",
        ])
        .is_ok());
    }

    /// The private view key is never an argument: the old flag is refused
    /// with the fix named, and the file (or standard input) is read, with
    /// the newline an editor leaves trimmed.
    #[test]
    fn the_view_key_comes_from_a_file_never_from_the_argument_list() {
        let refused = parse(&[
            "--bootstrap-wallet",
            "--primary-address",
            "4abc",
            "--view-key",
            "aa",
            "--view-key-file",
            "x",
            "--spend-pubkey",
            "bb",
        ])
        .unwrap_err()
        .to_string();
        assert!(refused.contains("--view-key-file"), "{refused}");

        let dir = std::env::temp_dir().join(format!("engine-view-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("view.key");
        std::fs::write(&path, "aabb\n").unwrap();
        let command = match parse(&[
            "--bootstrap-wallet",
            "--primary-address",
            "4abc",
            "--view-key-file",
            path.to_str().unwrap(),
            "--spend-pubkey",
            "bb",
        ])
        .unwrap()
        .action
        {
            Action::BootstrapWallet(command) => command,
            other => panic!("expected BootstrapWallet, got {other:?}"),
        };
        let read = command.read_view_key().unwrap();
        assert_eq!(read.view_key_hex, "aabb");
        assert_eq!(read.spend_pubkey_hex, "bb");

        std::fs::write(&path, " \n").unwrap();
        let empty = BootstrapWalletCommand {
            view_key_file: path.to_str().unwrap().to_string(),
            ..bootstrap_command()
        };
        assert!(empty.read_view_key().unwrap_err().contains("empty"));
        let missing = BootstrapWalletCommand {
            view_key_file: dir.join("nowhere").to_str().unwrap().to_string(),
            ..bootstrap_command()
        };
        assert!(missing.read_view_key().is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn bootstrap_command() -> BootstrapWalletCommand {
        BootstrapWalletCommand {
            primary_address: "4abc".to_string(),
            view_key_file: "-".to_string(),
            spend_pubkey_hex: "bb".to_string(),
            network: "mainnet".to_string(),
            key_custody_backend: None,
        }
    }

    #[test]
    fn bootstrap_wallet_takes_a_network_defaulting_to_mainnet_and_refuses_the_removed_origins_flag()
    {
        let bootstrap = [
            "--bootstrap-wallet",
            "--primary-address",
            "4abc",
            "--view-key-file",
            "view.key",
            "--spend-pubkey",
            "bb",
        ];
        match parse(&bootstrap).unwrap().action {
            Action::BootstrapWallet(a) => assert_eq!(a.network, "mainnet"),
            other => panic!("expected BootstrapWallet, got {other:?}"),
        }
        let stagenet: Vec<&str> = bootstrap
            .iter()
            .copied()
            .chain(["--network", "stagenet"])
            .collect();
        match parse(&stagenet).unwrap().action {
            Action::BootstrapWallet(a) => assert_eq!(a.network, "stagenet"),
            other => panic!("expected BootstrapWallet, got {other:?}"),
        }
        // The engine has no origin list any more (embedding policy is
        // monokulo's), so the old flag is an error rather than silently ignored.
        let origins: Vec<&str> = bootstrap
            .iter()
            .copied()
            .chain(["--allowed-origins", "https://a.example"])
            .collect();
        assert!(parse(&origins).is_err());
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
