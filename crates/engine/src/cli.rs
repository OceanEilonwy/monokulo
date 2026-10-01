//! Top-level argv parsing for the `scanner` binary - kept in the lib crate
//! (not `main.rs`) so it's unit-testable the normal way, and so `main.rs` stays a
//! thin wrapper around whatever this decides the process should do.
//!
//! No `--config`/config-file concept any more (`config.rs` and the interactive
//! `--init` wizard it backed are both gone - every setting that used to live in
//! a TOML file now lives in the `settings` table instead, editable at runtime
//! over the instance-admin HTTP API rather than only at boot from a file). The
//! one thing every mode below still needs to agree on is *where the database
//! is* - see [`database_path`].

use crate::local_admin::BootstrapWalletArgs;

#[derive(Debug)]
pub enum Action {
    RunServer {
        strict_tls: bool,
    },
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
    Help,
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

pub const HELP_TEXT: &str = "\
scanner - a self-hosted Monero payment gateway

USAGE:
    scanner [--strict-tls]
    scanner --bootstrap-wallet --primary-address <ADDR> --view-key-file <PATH> \
--spend-pubkey <HEX> [--network mainnet|stagenet|testnet] \
[--key-custody-backend plain|socket]
    scanner --rotate-secret [--pk <PK>]
    scanner --show-tenant [--pk <PK>]

With no other mode, starts the HTTP server. Every runtime-configurable setting
(Monero node endpoints, confirmation/expiry thresholds, rate limits, webhook
policy, ...) is read from the database (`env` var overrides win over a stored
value, which wins over a built-in default) and editable at runtime over the
instance-admin HTTP API (`GET`/`POST /api/v1/admin/settings`) - see that API's
own doc comment (`http::instance_admin`) for the full list and how each one's
environment-variable override is named. This binary itself only ever needs to
know where its own database file lives - see DATABASE below.

DATABASE:
    Read from ENGINE_DB_PATH if set, otherwise ./engine.db in the current
    working directory - by every mode below, so a plain `scanner` server run
    and every one of the commands here always agree on which file they mean.

OPTIONS:
    --strict-tls          Require a real CA-signed certificate from every
                          configured node, overriding that node's own
                          accept_self_signed_certs setting. Only meaningful
                          when starting the server.
    --bootstrap-wallet     Create the one tenant a self-hosted deployment
                          needs, from a watch-only view key and spend public
                          key - refuses if a tenant already exists (this is a
                          one-time action, not an ongoing setting; a hosted
                          instance creates tenants at runtime via the admin
                          HTTP API instead and never uses this at all).
    --primary-address      The wallet's own primary address (bootstrap only).
    --view-key-file        A file holding the wallet's private view key,
                          hex-encoded (bootstrap only) - never a spend key.
                          `-` reads it from standard input. Never an
                          argument: the argument list is readable by every
                          user on the machine and kept by the shell's
                          history.
    --spend-pubkey         The wallet's public spend key, hex-encoded
                          (bootstrap only) - the public half only, never the
                          private spend key.
    --key-custody-backend  Where the wallet's keys are kept (bootstrap only):
                          one of the enabled key_custody.enabled_backends;
                          key_custody.default_backend when not given. The
                          primary address must be the wallet of the given
                          keys on the given network, or nothing is created.
    --network              Which network the bootstrap tenant watches -
                          mainnet (default), stagenet, or testnet.
    --rotate-secret        Mint a fresh admin secret (sk_...) for a tenant,
                          invalidating the old one - the only way back in if
                          you've lost it. Needs a tenant to already exist.
    --show-tenant          Print a tenant's current settings (public key,
                          network, address, allowed origins, thresholds) -
                          never its keys or secret.
    --pk <PK>              Which tenant --rotate-secret/--show-tenant act on.
                          Only needed if more than one tenant is configured -
                          the common, self-hosted, single-tenant case is found
                          automatically.
    --help, -h             Print this help and exit.

EXAMPLES:
    scanner                        Start the server.
    scanner --strict-tls           Start the server, rejecting self-signed node certs.
    scanner --bootstrap-wallet --primary-address 4... --view-key-file view.key --spend-pubkey <hex>
                                   Provision the one self-hosted tenant.
    printf '%s' <hex> | scanner --bootstrap-wallet ... --view-key-file -
                                   The same, with the key on standard input.
    scanner --rotate-secret        Mint a fresh admin secret for the sole tenant.
";

/// The database file every mode below reads from - `ENGINE_DB_PATH` if set,
/// otherwise `engine.db` in the current working directory. A single,
/// deliberately simple convention now that there's no config file for a path
/// to be derived alongside any more (the former `init_wizard::database_path_for`
/// always placed the database next to whatever config file was in use - this
/// is that same idea with the "next to a config file" half removed, since
/// there is no config file to be next to).
pub fn database_path() -> std::path::PathBuf {
    database_path_from(std::env::var("ENGINE_DB_PATH").ok().as_deref())
}

/// [`database_path`] for a given `ENGINE_DB_PATH` value (`None` when
/// unset). Pure, so it is tested without touching the process environment,
/// which other tests in the same binary read at the same time.
fn database_path_from(configured: Option<&str>) -> std::path::PathBuf {
    configured
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("engine.db"))
}

/// Parses the full process argv (excluding argv[0]). `--help`/`-h` short-circuits
/// everything else - present anywhere, it wins.
pub fn parse_args(args: &[String]) -> Result<Action, String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Action::Help);
    }
    if args.iter().any(|a| a == "--bootstrap-wallet") {
        return parse_bootstrap_wallet_args(args).map(Action::BootstrapWallet);
    }
    if args.iter().any(|a| a == "--rotate-secret") {
        return Ok(Action::RotateSecret {
            pk: parse_pk_arg("--rotate-secret", args)?,
        });
    }
    if args.iter().any(|a| a == "--show-tenant") {
        return Ok(Action::ShowTenant {
            pk: parse_pk_arg("--show-tenant", args)?,
        });
    }
    let mut strict_tls = false;
    for arg in args {
        match arg.as_str() {
            "--strict-tls" => strict_tls = true,
            other => {
                return Err(format!(
                    "unrecognized argument {other:?} - run with --help for usage"
                ))
            }
        }
    }
    Ok(Action::RunServer { strict_tls })
}

/// Shared `--pk` parsing for `--rotate-secret` and `--show-tenant`. Any
/// other argument is an error, as in every other mode: a mistyped flag must
/// not act on the default tenant as if nothing had been asked.
fn parse_pk_arg(mode: &str, args: &[String]) -> Result<Option<String>, String> {
    let mut pk = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--pk" => {
                pk = Some(
                    iter.next()
                        .ok_or_else(|| "--pk needs a value".to_string())?
                        .clone(),
                );
            }
            flag if flag == mode => {}
            other => return Err(format!("unrecognized argument {other:?} for {mode}")),
        }
    }
    Ok(pk)
}

fn parse_bootstrap_wallet_args(args: &[String]) -> Result<BootstrapWalletCommand, String> {
    let mut primary_address = None;
    let mut view_key_file = None;
    let mut spend_pubkey_hex = None;
    let mut network = "mainnet".to_string();
    let mut key_custody_backend = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--primary-address" => {
                primary_address = Some(
                    iter.next()
                        .ok_or("--primary-address needs a value")?
                        .clone(),
                )
            }
            "--view-key-file" => {
                view_key_file = Some(iter.next().ok_or("--view-key-file needs a value")?.clone())
            }
            "--view-key" => {
                return Err(
                    "--view-key is not accepted: a key in the argument list is readable by every user on the machine - put it in a file and pass --view-key-file <PATH> (or - for standard input)".to_string(),
                )
            }
            "--spend-pubkey" => {
                spend_pubkey_hex = Some(iter.next().ok_or("--spend-pubkey needs a value")?.clone())
            }
            "--network" => network = iter.next().ok_or("--network needs a value")?.clone(),
            "--key-custody-backend" => {
                key_custody_backend = Some(
                    iter.next()
                        .ok_or("--key-custody-backend needs a value")?
                        .clone(),
                )
            }
            "--bootstrap-wallet" => {}
            other => {
                return Err(format!(
                    "unrecognized argument {other:?} for --bootstrap-wallet"
                ))
            }
        }
    }
    Ok(BootstrapWalletCommand {
        primary_address: primary_address.ok_or("--bootstrap-wallet requires --primary-address")?,
        view_key_file: view_key_file.ok_or("--bootstrap-wallet requires --view-key-file")?,
        spend_pubkey_hex: spend_pubkey_hex.ok_or("--bootstrap-wallet requires --spend-pubkey")?,
        network,
        key_custody_backend,
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_arguments_starts_the_server_with_default_options() {
        match parse_args(&args(&[])).unwrap() {
            Action::RunServer { strict_tls } => assert!(!strict_tls),
            _ => panic!("expected RunServer"),
        }
    }

    #[test]
    fn strict_tls_flag_is_recognized() {
        match parse_args(&args(&["--strict-tls"])).unwrap() {
            Action::RunServer { strict_tls } => assert!(strict_tls),
            _ => panic!("expected RunServer"),
        }
    }

    #[test]
    fn an_unrecognized_bare_argument_is_a_clear_error_not_silently_ignored() {
        // A leftover config-file-path-shaped argument (from the removed
        // `--config`/positional-path era) must not be silently accepted and
        // ignored - that would look like it worked while doing nothing.
        assert!(parse_args(&args(&["moneropay.toml"])).is_err());
    }

    #[test]
    fn help_flag_wins_over_everything_else_wherever_it_appears() {
        assert!(matches!(
            parse_args(&args(&["--help"])).unwrap(),
            Action::Help
        ));
        assert!(matches!(parse_args(&args(&["-h"])).unwrap(), Action::Help));
        assert!(matches!(
            parse_args(&args(&["--bootstrap-wallet", "--help"])).unwrap(),
            Action::Help
        ));
        assert!(matches!(
            parse_args(&args(&["--rotate-secret", "--help"])).unwrap(),
            Action::Help
        ));
        assert!(matches!(
            parse_args(&args(&["--show-tenant", "--help"])).unwrap(),
            Action::Help
        ));
    }

    #[test]
    fn help_text_documents_every_real_flag() {
        for flag in [
            "--strict-tls",
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
            assert!(HELP_TEXT.contains(flag), "help text should mention {flag}");
        }
    }

    #[test]
    fn rotate_secret_flag_defaults_to_no_pk() {
        match parse_args(&args(&["--rotate-secret"])).unwrap() {
            Action::RotateSecret { pk } => assert!(pk.is_none()),
            _ => panic!("expected RotateSecret"),
        }
    }

    #[test]
    fn show_tenant_flag_accepts_a_pk_override() {
        match parse_args(&args(&["--show-tenant", "--pk", "pk_abc"])).unwrap() {
            Action::ShowTenant { pk } => assert_eq!(pk.as_deref(), Some("pk_abc")),
            _ => panic!("expected ShowTenant"),
        }
    }

    /// A mistyped flag with `--rotate-secret`/`--show-tenant` is refused,
    /// not ignored: ignored, it would act on the default tenant.
    #[test]
    fn the_local_admin_modes_refuse_unknown_arguments() {
        for argv in [
            &["--rotate-secret", "--pks", "pk_x"][..],
            &["--show-tenant", "--network", "stagenet"][..],
            &["--rotate-secret", "--pk"][..],
        ] {
            assert!(parse_args(&args(argv)).is_err(), "{argv:?}");
        }
    }

    #[test]
    fn bootstrap_wallet_requires_the_three_key_material_flags() {
        assert!(parse_args(&args(&["--bootstrap-wallet"])).is_err());
        assert!(parse_args(&args(&["--bootstrap-wallet", "--primary-address", "4abc"])).is_err());
        assert!(parse_args(&args(&[
            "--bootstrap-wallet",
            "--primary-address",
            "4abc",
            "--view-key-file",
            "view.key",
            "--spend-pubkey",
            "bb",
        ]))
        .is_ok());
    }

    /// The private view key is never an argument: the old flag is refused
    /// with the fix named, and the file (or standard input) is read, with
    /// the newline an editor leaves trimmed.
    #[test]
    fn the_view_key_comes_from_a_file_never_from_the_argument_list() {
        let refused = parse_args(&args(&[
            "--bootstrap-wallet",
            "--primary-address",
            "4abc",
            "--view-key",
            "aa",
            "--spend-pubkey",
            "bb",
        ]))
        .unwrap_err();
        assert!(refused.contains("--view-key-file"), "{refused}");

        let dir = std::env::temp_dir().join(format!("scanner-view-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("view.key");
        std::fs::write(&path, "aabb\n").unwrap();
        let command = match parse_args(&args(&[
            "--bootstrap-wallet",
            "--primary-address",
            "4abc",
            "--view-key-file",
            path.to_str().unwrap(),
            "--spend-pubkey",
            "bb",
        ]))
        .unwrap()
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
    fn bootstrap_wallet_defaults_network_to_mainnet_and_origins_to_empty() {
        match parse_args(&args(&[
            "--bootstrap-wallet",
            "--primary-address",
            "4abc",
            "--view-key-file",
            "view.key",
            "--spend-pubkey",
            "bb",
        ]))
        .unwrap()
        {
            Action::BootstrapWallet(a) => {
                assert_eq!(a.network, "mainnet");
            }
            _ => panic!("expected BootstrapWallet"),
        }
    }

    #[test]
    fn bootstrap_wallet_parses_network_and_refuses_the_removed_origins_flag() {
        match parse_args(&args(&[
            "--bootstrap-wallet",
            "--primary-address",
            "4abc",
            "--view-key-file",
            "view.key",
            "--spend-pubkey",
            "bb",
            "--network",
            "stagenet",
        ]))
        .unwrap()
        {
            Action::BootstrapWallet(a) => assert_eq!(a.network, "stagenet"),
            _ => panic!("expected BootstrapWallet"),
        }
        // The engine has no origin list any more (embedding policy is
        // monokulo's), so the old flag is an error rather than silently ignored.
        let refused = parse_args(&args(&[
            "--bootstrap-wallet",
            "--primary-address",
            "4abc",
            "--view-key-file",
            "view.key",
            "--spend-pubkey",
            "bb",
            "--allowed-origins",
            "https://a.example",
        ]));
        assert!(refused.is_err());
    }

    #[test]
    fn database_path_defaults_to_a_fixed_relative_path_and_the_env_var_overrides_it() {
        assert_eq!(
            database_path_from(None),
            std::path::PathBuf::from("engine.db")
        );
        assert_eq!(
            database_path_from(Some("")),
            std::path::PathBuf::from("engine.db"),
            "blank is unset"
        );
        assert_eq!(
            database_path_from(Some("/tmp/somewhere/custom.db")),
            std::path::PathBuf::from("/tmp/somewhere/custom.db")
        );
    }
}
