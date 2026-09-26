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
    RunServer { strict_tls: bool },
    /// Mints a fresh admin secret for a tenant, invalidating the old one - see
    /// `local_admin::rotate_secret`.
    RotateSecret { pk: Option<String> },
    /// Prints a tenant's non-secret settings - see `local_admin::show_tenant`.
    ShowTenant { pk: Option<String> },
    /// Provisions the one tenant a self-hosted deployment needs, replacing what
    /// used to be the `[wallet]` section of the (now-removed) TOML config file -
    /// see `local_admin::bootstrap_wallet`.
    BootstrapWallet(BootstrapWalletArgs),
    Help,
}

pub const HELP_TEXT: &str = "\
scanner - a self-hosted Monero payment gateway

USAGE:
    scanner [--strict-tls]
    scanner --bootstrap-wallet --primary-address <ADDR> --view-key <HEX> \
--spend-pubkey <HEX> [--network mainnet|stagenet|testnet]
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
    Read from SCANNER_DB_PATH if set, otherwise ./scanner.db in the current
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
    --view-key             The wallet's private view key, hex-encoded
                          (bootstrap only) - never a spend key.
    --spend-pubkey         The wallet's public spend key, hex-encoded
                          (bootstrap only) - the public half only, never the
                          private spend key.
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
    scanner --bootstrap-wallet --primary-address 4... --view-key <hex> --spend-pubkey <hex>
                                   Provision the one self-hosted tenant.
    scanner --rotate-secret        Mint a fresh admin secret for the sole tenant.
";

/// The database file every mode below reads from - `SCANNER_DB_PATH` if set,
/// otherwise `scanner.db` in the current working directory. A single,
/// deliberately simple convention now that there's no config file for a path
/// to be derived alongside any more (the former `init_wizard::database_path_for`
/// always placed the database next to whatever config file was in use - this
/// is that same idea with the "next to a config file" half removed, since
/// there is no config file to be next to).
pub fn database_path() -> std::path::PathBuf {
    std::env::var("SCANNER_DB_PATH").map(std::path::PathBuf::from).unwrap_or_else(|_| std::path::PathBuf::from("scanner.db"))
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
        return Ok(Action::RotateSecret { pk: parse_pk_arg(args)? });
    }
    if args.iter().any(|a| a == "--show-tenant") {
        return Ok(Action::ShowTenant { pk: parse_pk_arg(args)? });
    }
    let mut strict_tls = false;
    for arg in args {
        match arg.as_str() {
            "--strict-tls" => strict_tls = true,
            other => return Err(format!("unrecognized argument {other:?} - run with --help for usage")),
        }
    }
    Ok(Action::RunServer { strict_tls })
}

/// Shared `--pk` parsing for `--rotate-secret` and `--show-tenant`.
fn parse_pk_arg(args: &[String]) -> Result<Option<String>, String> {
    let mut pk = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--pk" {
            pk = Some(iter.next().ok_or_else(|| "--pk needs a value".to_string())?.clone());
        }
    }
    Ok(pk)
}

fn parse_bootstrap_wallet_args(args: &[String]) -> Result<BootstrapWalletArgs, String> {
    let mut primary_address = None;
    let mut view_key_hex = None;
    let mut spend_pubkey_hex = None;
    let mut network = "mainnet".to_string();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--primary-address" => primary_address = Some(iter.next().ok_or("--primary-address needs a value")?.clone()),
            "--view-key" => view_key_hex = Some(iter.next().ok_or("--view-key needs a value")?.clone()),
            "--spend-pubkey" => spend_pubkey_hex = Some(iter.next().ok_or("--spend-pubkey needs a value")?.clone()),
            "--network" => network = iter.next().ok_or("--network needs a value")?.clone(),
            "--bootstrap-wallet" => {}
            other => return Err(format!("unrecognized argument {other:?} for --bootstrap-wallet")),
        }
    }
    Ok(BootstrapWalletArgs {
        primary_address: primary_address.ok_or("--bootstrap-wallet requires --primary-address")?,
        view_key_hex: view_key_hex.ok_or("--bootstrap-wallet requires --view-key")?,
        spend_pubkey_hex: spend_pubkey_hex.ok_or("--bootstrap-wallet requires --spend-pubkey")?,
        network,
    })
}

#[cfg(test)]
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
        assert!(matches!(parse_args(&args(&["--help"])).unwrap(), Action::Help));
        assert!(matches!(parse_args(&args(&["-h"])).unwrap(), Action::Help));
        assert!(matches!(parse_args(&args(&["--bootstrap-wallet", "--help"])).unwrap(), Action::Help));
        assert!(matches!(parse_args(&args(&["--rotate-secret", "--help"])).unwrap(), Action::Help));
        assert!(matches!(parse_args(&args(&["--show-tenant", "--help"])).unwrap(), Action::Help));
    }

    #[test]
    fn help_text_documents_every_real_flag() {
        for flag in [
            "--strict-tls", "--bootstrap-wallet", "--primary-address", "--view-key", "--spend-pubkey", "--network",
            "--rotate-secret", "--show-tenant", "--pk", "--help",
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

    #[test]
    fn bootstrap_wallet_requires_the_three_key_material_flags() {
        assert!(parse_args(&args(&["--bootstrap-wallet"])).is_err());
        assert!(parse_args(&args(&["--bootstrap-wallet", "--primary-address", "4abc"])).is_err());
        assert!(parse_args(&args(&[
            "--bootstrap-wallet",
            "--primary-address", "4abc",
            "--view-key", "aa",
            "--spend-pubkey", "bb",
        ]))
        .is_ok());
    }

    #[test]
    fn bootstrap_wallet_defaults_network_to_mainnet_and_origins_to_empty() {
        match parse_args(&args(&[
            "--bootstrap-wallet",
            "--primary-address", "4abc",
            "--view-key", "aa",
            "--spend-pubkey", "bb",
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
            "--primary-address", "4abc",
            "--view-key", "aa",
            "--spend-pubkey", "bb",
            "--network", "stagenet",
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
            "--primary-address", "4abc",
            "--view-key", "aa",
            "--spend-pubkey", "bb",
            "--allowed-origins", "https://a.example",
        ]));
        assert!(refused.is_err());
    }

    #[test]
    fn database_path_defaults_to_a_fixed_relative_path_and_the_env_var_overrides_it() {
        std::env::remove_var("SCANNER_DB_PATH");
        assert_eq!(database_path(), std::path::PathBuf::from("scanner.db"));

        std::env::set_var("SCANNER_DB_PATH", "/tmp/somewhere/custom.db");
        assert_eq!(database_path(), std::path::PathBuf::from("/tmp/somewhere/custom.db"));
        std::env::remove_var("SCANNER_DB_PATH");
    }
}
