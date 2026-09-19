//! A general-purpose CLI over `stagenet-test-wallet`'s `WalletStore`/
//! `StagenetTestWallet` - the same fast, ledger-based, no-chain-scanning
//! wallet every real-stagenet e2e suite in this repo already uses as a
//! library, now reachable by hand for setup/maintenance work (checking a
//! balance, seeding a fresh wallet from a seed phrase, splitting a big
//! output into several smaller ones so the e2e suites have more
//! independently-aged spendable outputs to draw on) instead of writing a
//! one-off script every time.
//!
//! ```sh
//! cargo run -p stagenet-test-wallet --bin stagenet-wallet-cli -- --help
//! ```

use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use stagenet_test_wallet::{Ledger, StagenetTestWallet, WalletStore};

/// Standard locations every real e2e suite in this repo already uses -
/// good defaults for this CLI too, overridable for someone running it
/// against a different checkout or fixture set.
mod defaults {
    pub const NODE_URL: &str = "http://node.monerodevs.org:38089";
    pub const ACCEPT_INVALID_CERTS: bool = true;
    pub const WALLETS_PATH: &str = "e2e/stagenet-wallets.json";
    pub const LEDGER_PATH: &str = "e2e/stagenet-known-outputs.json";
    pub const DECOY_DISTRIBUTION_PATH: &str = "e2e/stagenet-decoy-distribution.json";
    /// The wallet this crate exists to spend from - see `e2e/README.md`.
    pub const WALLET_NAME: &str = "spender";
}

#[derive(Parser)]
#[command(name = "stagenet-wallet-cli", about = "Inspect and drive the stagenet e2e test wallet by hand")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Which named wallet in `--wallets-path` to act as.
    #[arg(long, global = true, default_value = defaults::WALLET_NAME)]
    wallet: String,

    #[arg(long, global = true, default_value = defaults::NODE_URL)]
    node_url: String,

    #[arg(long, global = true, default_value_t = defaults::ACCEPT_INVALID_CERTS)]
    accept_invalid_certs: bool,

    #[arg(long, global = true, default_value = defaults::WALLETS_PATH)]
    wallets_path: String,

    #[arg(long, global = true, default_value = defaults::LEDGER_PATH)]
    ledger_path: String,

    #[arg(long, global = true, default_value = defaults::DECOY_DISTRIBUTION_PATH)]
    decoy_distribution_path: String,
}

#[derive(Subcommand)]
enum Command {
    /// Print `--wallet`'s address.
    Address,
    /// Print `--wallet`'s current spendable/pending balance.
    Balance,
    /// Send `piconero` to `to` from `--wallet`.
    Send {
        to: String,
        piconero: u64,
        /// Instead of one opaque change output, split the leftover into
        /// this many explicit self-addressed outputs.
        #[arg(long)]
        split: Option<usize>,
    },
    /// Split `--wallet`'s spendable balance into `into` roughly-equal
    /// self-addressed outputs, so more of it is independently spendable
    /// (each still needs its own confirmations before it matures).
    Split { into: usize },
    /// Ledger entries this wallet doesn't know about yet.
    #[command(subcommand)]
    Output(OutputCommand),
    /// Wallets in the `--wallets-path` file.
    #[command(subcommand)]
    Wallet(WalletCommand),
    /// Print a shell completion script to stdout.
    Completions { shell: clap_complete::Shell },
}

#[derive(Subcommand)]
enum OutputCommand {
    /// Adds a ledger entry for a transaction `--wallet` didn't send itself
    /// (a faucet payout, funds sent in from elsewhere) and resolves it
    /// immediately if it's already confirmed.
    Add { txid: String },
}

#[derive(Subcommand)]
enum WalletCommand {
    /// Adds a new named wallet to `--wallets-path`.
    Add {
        /// The name to add it under (this becomes `--wallet <name>` for
        /// every other command).
        name: String,
        /// A real Monero seed phrase - a 16-word Polyseed or a 24/25-word
        /// legacy Electrum-style seed. Mutually exclusive with
        /// `--generate`.
        #[arg(long, conflicts_with = "generate")]
        seed: Option<String>,
        /// Generates a brand-new random wallet instead of importing one.
        #[arg(long)]
        generate: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), stagenet_test_wallet::WalletError> {
    match &cli.command {
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), "stagenet-wallet-cli", &mut std::io::stdout());
            return Ok(());
        }
        Command::Wallet(WalletCommand::Add { name, seed, generate }) => {
            let mut store = WalletStore::load(&cli.wallets_path)?;
            match (seed, generate) {
                (Some(phrase), false) => store.add_wallet_from_seed(name, phrase)?,
                (None, true) => {
                    return Err(stagenet_test_wallet::WalletError::WalletStore(
                        "generating a brand-new wallet isn't wired up yet - pass --seed \"<phrase>\" to import one".to_string(),
                    ));
                }
                _ => {
                    return Err(stagenet_test_wallet::WalletError::WalletStore("pass exactly one of --seed <phrase> or --generate".to_string()));
                }
            }
            println!("added wallet {name:?} to {}", cli.wallets_path);
            return Ok(());
        }
        _ => {}
    }

    let credentials = WalletStore::load(&cli.wallets_path)?.wallet(&cli.wallet)?;
    let wallet = StagenetTestWallet::connect(
        &cli.node_url,
        cli.accept_invalid_certs,
        &credentials.private_spend_key_hex,
        &credentials.private_view_key_hex,
        &credentials.address,
        &cli.decoy_distribution_path,
    )
    .await?;

    match cli.command {
        Command::Address => println!("{}", wallet.address()),
        Command::Balance => {
            let mut ledger = Ledger::load(&cli.ledger_path)?;
            let balance = wallet.balance(&mut ledger).await?;
            println!(
                "spendable: {} piconero across {} output(s)\npending:   {} piconero across {} output(s)",
                balance.spendable_piconero, balance.spendable_outputs, balance.pending_piconero, balance.pending_outputs,
            );
        }
        Command::Send { to, piconero, split } => {
            let mut ledger = Ledger::load(&cli.ledger_path)?;
            let hash = match split {
                Some(n) => wallet.send_with_change_split(&mut ledger, &to, piconero, n).await?,
                None => wallet.send(&mut ledger, &to, piconero).await?,
            };
            println!("sent, tx {}", hex::encode(hash));
        }
        Command::Split { into } => {
            let mut ledger = Ledger::load(&cli.ledger_path)?;
            let hash = wallet.split(&mut ledger, into).await?;
            println!("split, tx {}", hex::encode(hash));
        }
        Command::Output(OutputCommand::Add { txid }) => {
            let mut ledger = Ledger::load(&cli.ledger_path)?;
            wallet.add_output(&mut ledger, &txid).await?;
            println!("added output {txid} (resolved if already confirmed, pending otherwise)");
        }
        Command::Wallet(WalletCommand::Add { .. }) | Command::Completions { .. } => unreachable!("handled above"),
    }
    Ok(())
}
