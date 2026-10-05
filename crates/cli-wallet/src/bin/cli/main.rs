//! `wallet-cli`: `monero-wallet-cli`'s commands over this crate's
//! fast, no-scanning, one-SQLite-file-per-wallet stagenet (or, with
//! `--testnet`, testnet) wallet.
//!
//! Open a wallet and get a prompt, as with the reference wallet:
//!
//! ```sh
//! cargo run -p cli-wallet --bin wallet-cli -- --wallet-file spender
//! [wallet 56Lc1x]: balance
//! [wallet 56Lc1x]: transfer <address> 0.01
//! ```
//!
//! or run one command and exit (`... -- --wallet-file spender balance`).
//! `--help` lists every command; `help <command>` at the prompt shows one.

mod args;
mod commands;
mod editor;

use std::io::{BufRead, IsTerminal, Write};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use clap::{CommandFactory, Parser, Subcommand};
use cli_wallet::file::{import_json, migrate_legacy, WalletData, WalletFile};
use cli_wallet::{
    credentials_from_seed, credentials_from_spend_key_hex, generate_credentials, Network,
    WalletCtx, SEED_LANGUAGE_NAMES,
};

use commands::{CliError, Command, Session};
use reedline::Signal;

/// The wallet every e2e suite spends from - see `e2e/README.md`.
const DEFAULT_WALLET: &str = "spender";

#[derive(Parser)]
#[command(
    name = "wallet-cli",
    version,
    about = "monero-wallet-cli's commands for the stagenet e2e test wallets, and testnet wallets",
    after_help = "With no command, opens the wallet and prompts for commands, as monero-wallet-cli does."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<TopCommand>,

    /// The wallet to open: a name in the wallet directory (`spender` is
    /// `e2e/wallets/spender.db`) or a path to a wallet file.
    #[arg(long, visible_alias = "wallet", global = true, default_value = DEFAULT_WALLET)]
    wallet_file: String,

    /// Directory bare wallet names resolve in.
    #[arg(long, global = true)]
    wallet_dir: Option<PathBuf>,

    /// Node to use (instead of node, node2 and node3.monerodevs.org on the
    /// wallet's network). Repeat it to give several, tried in order.
    #[arg(long, visible_alias = "node-url", global = true)]
    daemon_address: Vec<String>,

    /// Use stagenet: a new wallet is created on it, and an existing wallet
    /// must be on it. The default for new wallets.
    #[arg(long, global = true, conflicts_with = "testnet")]
    stagenet: bool,

    /// Use testnet: a new wallet is created on it, and an existing wallet
    /// must be on it. Without either flag, an existing wallet opens on
    /// the network its file records.
    #[arg(long, global = true)]
    testnet: bool,

    /// Sign transactions but don't broadcast them: each is written to
    /// `raw_monero_tx` instead, and the wallet file is left alone.
    #[arg(long, global = true)]
    do_not_relay: bool,

    /// Overrides the decoy-distribution snapshot file. Stagenet has one
    /// committed; testnet, without one, fetches the distribution from the
    /// node.
    #[arg(long, global = true)]
    decoy_distribution_path: Option<String>,

    /// Create a new wallet file (a name or a path) and open it. With no
    /// other option it gets fresh random keys.
    #[arg(long, value_name = "WALLET")]
    generate_new_wallet: Option<String>,

    /// With --generate-new-wallet: restore from a seed phrase (16-word
    /// Polyseed or 25-word legacy), prompted for unless --electrum-seed
    /// gives it.
    #[arg(long, requires = "generate_new_wallet")]
    restore_deterministic_wallet: bool,

    /// The seed phrase for --restore-deterministic-wallet.
    #[arg(long, requires = "restore_deterministic_wallet")]
    electrum_seed: Option<String>,

    /// Create a new wallet file from a secret spend key (prompted for) and
    /// open it.
    #[arg(long, value_name = "WALLET", conflicts_with = "generate_new_wallet")]
    generate_from_spend_key: Option<String>,

    /// Language of a newly generated seed.
    #[arg(long, default_value = "English", value_parser = SEED_LANGUAGE_NAMES)]
    mnemonic_language: String,
}

#[derive(Subcommand)]
enum TopCommand {
    #[command(flatten)]
    Wallet(Command),
    /// Print a shell completion script to stdout.
    Completions { shell: clap_complete::Shell },
    /// Convert a JSON wallet file (the format before SQLite) into a new
    /// wallet file: `<wallet>` is a name or path, by default the JSON
    /// file's own path with `.db` in place of `.json`. The JSON file is
    /// left alone.
    #[command(name = "import_json")]
    ImportJson {
        json: PathBuf,
        wallet: Option<String>,
    },
    /// One-off: split the old shared e2e/stagenet-wallets.json +
    /// e2e/stagenet-known-outputs.json into one file per wallet.
    #[command(hide = true, name = "migrate_legacy")]
    MigrateLegacy {
        wallets_json: PathBuf,
        known_outputs_json: PathBuf,
    },
}

/// One line typed at the prompt.
#[derive(Parser)]
#[command(
    name = "",
    no_binary_name = true,
    disable_version_flag = true,
    help_template = "{subcommands}"
)]
struct PromptLine {
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    /// The network `--stagenet`/`--testnet` asked for, if either.
    fn requested_network(&self) -> Option<Network> {
        if self.testnet {
            Some(Network::Testnet)
        } else if self.stagenet {
            Some(Network::Stagenet)
        } else {
            None
        }
    }

    /// Settings for wallets on `network`, with the command line's
    /// overrides.
    fn ctx(&self, network: Network) -> WalletCtx {
        let mut ctx = WalletCtx::for_network(network);
        if !self.daemon_address.is_empty() {
            ctx.node_urls = self
                .daemon_address
                .iter()
                .map(|address| with_scheme(address))
                .collect();
        }
        if let Some(dir) = &self.wallet_dir {
            ctx.wallet_dir = dir.clone();
        }
        if let Some(path) = &self.decoy_distribution_path {
            ctx.decoy_distribution_path = Some(path.clone());
        }
        ctx
    }
}

/// `--daemon-address host:port`, as monero-wallet-cli takes it, is plain
/// HTTP.
fn with_scheme(address: &str) -> String {
    if address.contains("://") {
        address.to_string()
    } else {
        format!("http://{address}")
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    // Rust ignores SIGPIPE, turning a closed pipe (`| head`) into a panic
    // on the next print; exit quietly instead, as other CLIs do.
    #[cfg(unix)]
    // SAFETY: restoring a signal's default action before any other thread
    // could be relying on it.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn prompt(question: &str) -> Result<String, CliError> {
    print!("{question}");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    read_stdin_line(&mut line).map_err(|e| format!("failed to read input: {e}"))?;
    Ok(line.trim().to_string())
}

/// Waits for the user without holding up the runtime: while this worker
/// thread blocks, its other tasks move to another worker.
fn blocking<T>(wait: impl FnOnce() -> T) -> T {
    tokio::task::block_in_place(wait)
}

/// One line of standard input, appended to `line`; see [`blocking`].
fn read_stdin_line(line: &mut String) -> std::io::Result<usize> {
    blocking(|| std::io::stdin().lock().read_line(line))
}

/// `--generate-new-wallet`/`--generate-from-spend-key`: writes the new
/// wallet file, on `ctx`'s network, and returns its path.
fn create_wallet(cli: &Cli, ctx: &WalletCtx) -> Result<Option<PathBuf>, CliError> {
    let network = ctx.network;
    let (name, credentials) = if let Some(name) = &cli.generate_new_wallet {
        let credentials = if cli.restore_deterministic_wallet {
            let seed = match &cli.electrum_seed {
                Some(seed) => seed.clone(),
                None => prompt("Specify Electrum seed: ")?,
            };
            credentials_from_seed(network, &seed)?
        } else {
            generate_credentials(network, &cli.mnemonic_language)?
        };
        (name, credentials)
    } else if let Some(name) = &cli.generate_from_spend_key {
        (
            name,
            credentials_from_spend_key_hex(network, &prompt("Secret spend key: ")?)?,
        )
    } else {
        return Ok(None);
    };
    let path = ctx.wallet_path(name);
    let generated_seed = cli.generate_new_wallet.is_some() && !cli.restore_deterministic_wallet;
    let file = WalletFile::create(&path, WalletData::new(network, credentials))?;
    println!("Generated new wallet: {}", file.data.address);
    println!("Wallet file: {}", path.display());
    if generated_seed {
        println!(
            "\nNOTE: the following 25 words can be used to recover access to your wallet. Write them down and store them somewhere safe and secure.\n\n{}\n",
            file.data.mnemonic.as_deref().unwrap_or_default()
        );
    }
    Ok(Some(path))
}

async fn run(mut cli: Cli) -> Result<(), CliError> {
    let requested = cli.requested_network();
    // A new wallet goes on the network asked for, stagenet by default.
    let ctx = cli.ctx(requested.unwrap_or(Network::Stagenet));
    let created = create_wallet(&cli, &ctx)?;
    let command = match cli.command.take() {
        Some(TopCommand::Completions { shell }) => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "wallet-cli",
                &mut std::io::stdout(),
            );
            return Ok(());
        }
        Some(TopCommand::MigrateLegacy {
            ref wallets_json,
            ref known_outputs_json,
        }) => {
            let report = migrate_legacy(
                wallets_json,
                known_outputs_json,
                &ctx.wallet_dir,
                DEFAULT_WALLET,
            )?;
            for (name, path, outputs, pending) in &report.written {
                println!(
                    "{name}: {} ({outputs} outputs, {pending} pending)",
                    path.display()
                );
            }
            for output in &report.unowned_outputs {
                println!("not owned by any wallet, dropped: {output}");
            }
            return Ok(());
        }
        Some(TopCommand::ImportJson {
            ref json,
            ref wallet,
        }) => {
            let out = match wallet {
                Some(wallet) => ctx.wallet_path(wallet),
                None => json.with_extension("db"),
            };
            let file = import_json(json, &out)?;
            println!(
                "{}: {} wallet {} ({} outputs, {} pending, {} sent)",
                out.display(),
                file.data.network,
                file.data.address,
                file.data.outputs.len(),
                file.data.pending.len(),
                file.data.sent.len()
            );
            return Ok(());
        }
        Some(TopCommand::Wallet(command)) => Some(command),
        None => None,
    };

    let path = created.unwrap_or_else(|| ctx.wallet_path(&cli.wallet_file));
    // An existing wallet opens on the network its file records, unless a
    // flag asked for one (and then it has to match).
    let ctx = match requested {
        Some(_) => ctx,
        None => cli.ctx(WalletFile::load(&path)?.data.network()?),
    };
    let mut session = Session::open(&ctx, &path, cli.do_not_relay)?;
    match command {
        Some(command) => commands::run(&mut session, command).await,
        None => {
            repl(&mut session).await;
            Ok(())
        }
    }
}

/// The interactive prompt: one command per line until `exit` or end of
/// input. A failing command prints its error and the prompt carries on.
///
/// At a terminal, lines come from the full line editor (history, completion
/// menu, hints, selection - see [`editor`]); piped input (scripts, tests)
/// is read plainly, line by line.
async fn repl(session: &mut Session) {
    let address = session.keys.address();
    println!("Opened wallet: {address}");
    let prompt_left = format!("[wallet {}]", &address[..6]);
    if std::io::stdin().is_terminal() {
        println!("Type \"help\" for the list of commands, Tab to complete, \u{2191} for history, Ctrl+R to search it, \"exit\" to leave.");
        let data = Arc::new(Mutex::new(editor::CompletionData::default()));
        data.lock()
            .expect("completion data lock")
            .refresh(&session.keys);
        let mut line_editor = editor::line_editor(data.clone());
        let prompt = editor::WalletPrompt {
            left: prompt_left,
            data: data.clone(),
        };
        loop {
            match blocking(|| line_editor.read_line(&prompt)) {
                Ok(Signal::Success(line)) => {
                    if run_line(session, &line).await.is_break() {
                        break;
                    }
                    data.lock()
                        .expect("completion data lock")
                        .refresh(&session.keys);
                }
                // Ctrl+C clears the line, as in a shell; Ctrl+D leaves.
                Ok(Signal::CtrlC) => continue,
                Ok(Signal::CtrlD) => break,
                Ok(_) => continue,
                Err(e) => {
                    eprintln!("Error: the line editor failed: {e}");
                    break;
                }
            }
        }
    } else {
        println!("Type \"help\" for the list of commands, \"exit\" to leave.");
        loop {
            print!("{prompt_left}: ");
            std::io::stdout().flush().ok();
            let mut line = String::new();
            match read_stdin_line(&mut line) {
                Ok(0) => {
                    println!();
                    break;
                }
                Ok(_) => {
                    if run_line(session, &line).await.is_break() {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("Error: failed to read input: {e}");
                    break;
                }
            }
        }
    }
}

/// Runs one prompt line. `Break` means leave the prompt.
async fn run_line(session: &mut Session, line: &str) -> ControlFlow<()> {
    let words = match args::split_line(line) {
        Ok(words) => words,
        Err(e) => {
            eprintln!("Error: {e}");
            return ControlFlow::Continue(());
        }
    };
    match words.first().map(String::as_str) {
        None => return ControlFlow::Continue(()),
        Some("exit" | "quit" | "q") => return ControlFlow::Break(()),
        _ => {}
    }
    match PromptLine::try_parse_from(&words) {
        Ok(PromptLine { command }) => {
            if let Err(e) = commands::run(session, command).await {
                eprintln!("Error: {e}");
            }
        }
        // Includes `help` and `help <command>`, which clap reports as
        // "errors" that print the help text.
        Err(e) => {
            let _ = e.print();
        }
    }
    ControlFlow::Continue(())
}
