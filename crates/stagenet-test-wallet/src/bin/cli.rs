//! A general-purpose CLI over `stagenet-test-wallet`'s `WalletStore`/
//! `Wallet` - the same fast, ledger-based, no-chain-scanning
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
use stagenet_test_wallet::{WalletCtx, WalletStore};

/// The wallet this crate exists to spend from - see `e2e/README.md`.
const DEFAULT_WALLET_NAME: &str = "spender";

#[derive(Parser)]
#[command(name = "stagenet-wallet-cli", about = "Inspect and drive the stagenet e2e test wallet by hand")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Which named wallet in `--wallets-path` to act as.
    #[arg(long, global = true, default_value = DEFAULT_WALLET_NAME)]
    wallet: String,

    /// Overrides `WalletCtx::default()`'s node URL.
    #[arg(long, global = true)]
    node_url: Option<String>,

    /// Overrides `WalletCtx::default()`'s wallets file path.
    #[arg(long, global = true)]
    wallets_path: Option<String>,

    /// Overrides `WalletCtx::default()`'s ledger file path.
    #[arg(long, global = true)]
    ledger_path: Option<String>,

    /// Overrides `WalletCtx::default()`'s decoy-distribution file path.
    #[arg(long, global = true)]
    decoy_distribution_path: Option<String>,
}

impl Cli {
    /// The standard `e2e/*` layout ([`WalletCtx::default`]), with any of
    /// this CLI's own path/URL flags overlaid on top.
    fn ctx(&self) -> WalletCtx {
        let mut ctx = WalletCtx::default();
        if let Some(v) = &self.node_url {
            ctx.node_url = v.clone();
        }
        if let Some(v) = &self.wallets_path {
            ctx.wallets_path = v.clone();
        }
        if let Some(v) = &self.ledger_path {
            ctx.ledger_path = v.clone();
        }
        if let Some(v) = &self.decoy_distribution_path {
            ctx.decoy_distribution_path = v.clone();
        }
        ctx
    }
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
    let ctx = cli.ctx();

    match &cli.command {
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), "stagenet-wallet-cli", &mut std::io::stdout());
            return Ok(());
        }
        Command::Wallet(WalletCommand::Add { name, seed, generate }) => {
            let mut store = WalletStore::load(&ctx)?;
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
            println!("added wallet {name:?} to {}", ctx.wallets_path);
            return Ok(());
        }
        _ => {}
    }

    let resolved = WalletStore::load(&ctx)?.wallet(&cli.wallet)?;
    let wallet = resolved.connect().await?;

    match cli.command {
        Command::Address => println!("{}", wallet.address()),
        Command::Balance => {
            let balance = wallet.balance().await?;
            println!(
                "spendable: {} piconero across {} output(s)\npending:   {} piconero across {} output(s)",
                balance.spendable_piconero, balance.spendable_outputs, balance.pending_piconero, balance.pending_outputs,
            );
        }
        Command::Send { to, piconero, split } => {
            let hash = match split {
                Some(n) => wallet.send_with_change_split(&to, piconero, n).await?,
                None => wallet.send(&to, piconero).await?,
            };
            println!("sent, tx {}", hex::encode(hash));
        }
        Command::Split { into } => {
            let hash = wallet.split(into).await?;
            println!("split, tx {}", hex::encode(hash));
        }
        Command::Output(OutputCommand::Add { txid }) => {
            wallet.add_output(&txid).await?;
            println!("added output {txid} (resolved if already confirmed, pending otherwise)");
        }
        Command::Wallet(WalletCommand::Add { .. }) | Command::Completions { .. } => unreachable!("handled above"),
    }
    Ok(())
}
