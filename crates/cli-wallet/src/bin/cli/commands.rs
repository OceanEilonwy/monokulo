//! Every `monero-wallet-cli` command this wallet supports, named and
//! printed as the reference wallet does. See the crate README for the full
//! table of what's supported and why the rest isn't.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, IsTerminal, Write};
use std::sync::Arc;

use clap::Subcommand;
use cli_wallet::amount::{format_amount, parse_amount, Unit};
use cli_wallet::file::{default_busy_handler, BusyChoice, BusyHandler, LockHolder, WalletData};
use cli_wallet::meta::AddressBookEntry;
use cli_wallet::{
    legacy_seed_for, FeePriority, OwnedOutput, ResolvedWallet, SweepSelect, TransferKind, TransferRequest, Wallet, WalletCtx, WalletError, WalletKeys,
    RING_LEN,
};

use crate::args::{self, IndexSelection, SubtractFee, PRIORITY_NAMES};

#[derive(Subcommand, Debug)]
#[command(rename_all = "snake_case")]
pub enum Command {
    /// Show the balance of the currently selected account.
    #[command(override_usage = "balance [detail]")]
    Balance {
        #[arg(value_parser = ["detail"])]
        detail: Option<String>,
    },
    /// List accounts, or create, switch, label and tag them.
    #[command(
        override_usage = "account\n       account new <label text with white spaces allowed>\n       account switch <index>\n       account label <index> <label text with white spaces allowed>\n       account tag <tag_name> <account_index_1> [<account_index_2> ...]\n       account untag <account_index_1> [<account_index_2> ...]\n       account tag_description <tag_name> <description>"
    )]
    Account {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show or create addresses of the current account.
    #[command(
        override_usage = "address [ new <label text with white spaces allowed> | all | <index_min> [<index_max>] | label <index> <label text with white spaces allowed> | one-off <account> <subaddress> ]"
    )]
    Address {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show a random payment ID and its integrated address, the integrated
    /// address for a given payment ID, or what an integrated address holds.
    #[command(override_usage = "integrated_address [<payment_id> | <address>]")]
    IntegratedAddress { arg: Option<String> },
    /// Generate a random payment ID.
    PaymentId,
    /// Show incoming payments with the given payment IDs.
    #[command(override_usage = "payments <PID_1> [<PID_2> ... <PID_N>]")]
    Payments {
        #[arg(required = true)]
        payment_ids: Vec<String>,
    },
    /// Send to one or more addresses. Amounts are in the `set unit` unit
    /// (monero by default).
    #[command(
        override_usage = "transfer [index=<N1>[,<N2>,...]] [<priority>] [<ring_size>] (<URI> | <address> <amount>) [<address> <amount> ...] [subtractfeefrom=<D0>[,<D1>,all,...]]"
    )]
    Transfer {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Send the whole unlocked balance of the current account to one address.
    #[command(override_usage = "sweep_all [index=<N1>[,<N2>,...] | index=all] [<priority>] [<ring_size>] [outputs=<N>] <address>")]
    SweepAll {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Send the whole unlocked balance of an account to one address.
    #[command(override_usage = "sweep_account <account> [index=<N1>[,<N2>,...] | index=all] [<priority>] [<ring_size>] [outputs=<N>] <address>")]
    SweepAccount {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Send every unlocked output smaller than a threshold to one address.
    #[command(override_usage = "sweep_below <amount_threshold> [index=<N1>[,<N2>,...]] [<priority>] [<ring_size>] <address>")]
    SweepBelow {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Send one output, named by its key image, to one address.
    #[command(override_usage = "sweep_single [<priority>] [<ring_size>] [outputs=<N>] <key_image> <address>")]
    SweepSingle {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Show incoming and outgoing transfers.
    #[command(override_usage = "show_transfers [in|out|all|pending|failed|pool|coinbase] [index=<N1>[,<N2>,...]] [<min_height> [<max_height>]]")]
    ShowTransfers {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show one transfer.
    ShowTransfer { txid: String },
    /// Write transfers to a CSV file.
    #[command(override_usage = "export_transfers [in|out|all|pending|failed|coinbase] [index=<N1>[,<N2>,...]] [<min_height> [<max_height>]] [output=<filepath>]")]
    ExportTransfers {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show incoming outputs.
    #[command(override_usage = "incoming_transfers [available|unavailable] [verbose] [uses] [index=<N1>[,<N2>[,...]]]")]
    IncomingTransfers {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// List unspent outputs, with a histogram of their heights.
    #[command(override_usage = "unspent_outputs [index=<N1>[,<N2>,...]] [<min_amount> [<max_amount>]]")]
    UnspentOutputs {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Set (or, with no text, clear) a transaction's note.
    #[command(override_usage = "set_tx_note <txid> [free text note]")]
    SetTxNote {
        txid: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        note: Vec<String>,
    },
    /// Show a transaction's note.
    GetTxNote { txid: String },
    /// Set (or, with no text, clear) the wallet's description.
    #[command(override_usage = "set_description [free text note]")]
    SetDescription {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        text: Vec<String>,
    },
    /// Show the wallet's description.
    GetDescription,
    /// List, add to, or delete from the address book.
    #[command(override_usage = "address_book [(add <address> [<description possibly with whitespaces>])|(delete <index>)]")]
    AddressBook {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Never spend an output (named by its key image) until thawed.
    Freeze { key_image: String },
    /// Allow a frozen output to be spent again.
    Thaw { key_image: String },
    /// Show whether an output is frozen.
    Frozen { key_image: String },
    /// Mark an output spent in the wallet file.
    #[command(override_usage = "mark_output_spent <amount>/<offset>")]
    MarkOutputSpent { output: String },
    /// Mark an output unspent in the wallet file.
    #[command(override_usage = "mark_output_unspent <amount>/<offset>")]
    MarkOutputUnspent { output: String },
    /// Show whether the wallet file has an output as spent.
    #[command(override_usage = "is_output_spent <amount>/<offset>")]
    IsOutputSpent { output: String },
    /// Check every output's spent status against the node (one key-image
    /// query; no scanning) and correct the wallet file.
    RescanSpent,
    /// Resolve pending transactions (each one block lookup; no scanning).
    Refresh,
    /// Show the node's height and the wallet's sync state.
    Status,
    /// Show the current fee rates.
    Fee,
    /// Show the blockchain height.
    BcHeight,
    /// Show the wallet file, description, address, type and network.
    WalletInfo,
    /// Show the seed phrase.
    Seed,
    /// Show the spend key.
    Spendkey,
    /// Show the view key.
    Viewkey,
    /// Show or change options: priority, unit, always-confirm-transfers,
    /// default-ring-size.
    #[command(override_usage = "set <option> [<value>]")]
    Set {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Save the wallet (every change is already saved as it happens).
    Save,
    /// Show the version.
    Version,
    /// Not in the reference wallet: split the largest unlocked output (or
    /// the `inputs=<N>` largest, merged) into equal outputs of this
    /// account's own - 16 by default, the most one transaction holds - so
    /// the e2e suites have plenty of independently spendable outputs.
    #[command(override_usage = "pocketchange [<pieces>] [inputs=<N>] [<priority>]", visible_alias = "split")]
    Pocketchange {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Not in the reference wallet: record a transaction that pays this
    /// wallet (a faucet payout) and resolve it once it confirms.
    AddOutput { txid: String },
}

#[derive(Debug)]
pub enum CliError {
    Wallet(WalletError),
    Usage(String),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Wallet(e) => write!(f, "{e}"),
            CliError::Usage(message) => write!(f, "{message}"),
        }
    }
}

impl From<WalletError> for CliError {
    fn from(e: WalletError) -> Self {
        CliError::Wallet(e)
    }
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        CliError::Usage(message)
    }
}

impl From<&str> for CliError {
    fn from(message: &str) -> Self {
        CliError::Usage(message.to_string())
    }
}

/// One open wallet: its file, and a node connection made the first time a
/// command needs one and kept for the rest of the session.
pub struct Session {
    pub resolved: ResolvedWallet,
    pub keys: WalletKeys,
    wallet: Option<Wallet>,
    pub do_not_relay: bool,
}

impl Session {
    pub fn open(ctx: &WalletCtx, path: &std::path::Path, do_not_relay: bool) -> Result<Self, CliError> {
        let resolved = ResolvedWallet::open(ctx, path)?;
        let mut keys = resolved.keys();
        keys.set_busy_handler(busy_handler());
        Ok(Session { resolved, keys, wallet: None, do_not_relay })
    }

    async fn wallet(&mut self) -> Result<&Wallet, CliError> {
        if self.wallet.is_none() {
            let mut wallet = self.resolved.connect().await?;
            wallet.set_busy_handler(busy_handler());
            self.wallet = Some(wallet);
        }
        Ok(self.wallet.as_ref().expect("just connected"))
    }
}

/// When another process has the wallet file locked: at a terminal, say who
/// and ask whether to retry, wait or cancel; otherwise (a script), warn and
/// wait, as the library does.
fn busy_handler() -> BusyHandler {
    if !std::io::stdin().is_terminal() {
        return default_busy_handler();
    }
    Arc::new(|holder: &LockHolder| loop {
        eprint!("{holder}.\n[R]etry, [w]ait for it, [c]ancel? ");
        std::io::stderr().flush().ok();
        let mut answer = String::new();
        if std::io::stdin().lock().read_line(&mut answer).unwrap_or(0) == 0 {
            return BusyChoice::Cancel;
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "" | "r" | "retry" => return BusyChoice::Retry,
            "w" | "wait" => {
                eprintln!("Waiting for it to finish (Ctrl-C to give up)...");
                return BusyChoice::Wait;
            }
            "c" | "cancel" => return BusyChoice::Cancel,
            _ => continue,
        }
    })
}

impl Session {

    fn data(&self) -> Result<WalletData, CliError> {
        Ok(self.keys.load()?.data)
    }

    fn unit(&self) -> Result<Unit, CliError> {
        Ok(self.data()?.meta.settings.unit)
    }
}

fn money(amount: u64, unit: Unit) -> String {
    format_amount(amount, unit)
}

fn fee_priority(priority: u32) -> FeePriority {
    match priority {
        0 | 1 => FeePriority::Unimportant,
        2 => FeePriority::Normal,
        3 => FeePriority::Elevated,
        _ => FeePriority::Priority,
    }
}

fn index_filter(indexes: Option<IndexSelection>) -> Option<Vec<u32>> {
    match indexes {
        Some(IndexSelection::Some(indexes)) => Some(indexes),
        Some(IndexSelection::All) | None => None,
    }
}

fn joined(words: &[String]) -> String {
    words.join(" ")
}

fn short(address: &str) -> &str {
    &address[..address.len().min(6)]
}

/// `YYYY-MM-DD HH:MM:SS`, UTC.
pub fn format_timestamp(timestamp: Option<u64>) -> String {
    let Some(secs) = timestamp else { return "-".to_string() };
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}", rem / 3_600, rem % 3_600 / 60, rem % 60)
}

/// `(balance, unlocked balance, output count)` per `(account, address
/// index)`.
type Balances = BTreeMap<(u32, u32), (u64, u64, usize)>;

/// Balance and unlocked balance per `(account, address index)`, plus how
/// many outputs make each up. Unconfirmed change from this wallet's own
/// sends counts towards balance (not unlocked), as in the reference
/// wallet; frozen outputs count towards neither.
fn balances(keys: &WalletKeys, data: &WalletData, tip: u64) -> Result<Balances, CliError> {
    let mut balances = Balances::new();
    for output in keys.outputs(data)?.into_iter().filter(|o| !o.spent && !o.frozen) {
        let entry = balances.entry(output.subaddress()).or_default();
        entry.0 += output.amount();
        entry.2 += 1;
        if output.unlocked(tip) {
            entry.1 += output.amount();
        }
    }
    for sent in data.sent.iter().filter(|sent| sent.height.is_none()) {
        balances.entry((sent.account, 0)).or_default().0 += sent.change_piconero;
    }
    Ok(balances)
}

fn account_totals(balances: &Balances, account: u32) -> (u64, u64) {
    balances.iter().filter(|((a, _), _)| *a == account).fold((0, 0), |(b, u), (_, (bal, unl, _))| (b + bal, u + unl))
}

pub async fn run(session: &mut Session, command: Command) -> Result<(), CliError> {
    match command {
        Command::Balance { detail } => balance(session, detail.is_some()).await,
        Command::Account { args } => account(session, &args).await,
        Command::Address { args } => address(session, &args).await,
        Command::IntegratedAddress { arg } => integrated_address(session, arg.as_deref()),
        Command::PaymentId => {
            println!("Random payment ID: {}", hex::encode(rand_payment_id()));
            Ok(())
        }
        Command::Payments { payment_ids } => payments(session, &payment_ids),
        Command::Transfer { args } => transfer(session, &args).await,
        Command::SweepAll { args } => {
            let parsed = args::parse_sweep_all(&args)?;
            let account = session.data()?.meta.current_account;
            sweep(session, account, parsed, SweepSelect::All).await
        }
        Command::SweepAccount { args } => {
            let (account, parsed) = args::parse_sweep_account(&args)?;
            session.data()?.meta.account(account)?;
            sweep(session, account, parsed, SweepSelect::All).await
        }
        Command::SweepBelow { args } => {
            let (threshold, parsed) = args::parse_sweep_below(&args, session.unit()?)?;
            let account = session.data()?.meta.current_account;
            sweep(session, account, parsed, SweepSelect::Below(threshold)).await
        }
        Command::SweepSingle { args } => {
            let (key_image, parsed) = args::parse_sweep_single(&args)?;
            let data = session.data()?;
            let output = session.keys.outputs(&data)?.into_iter().find(|o| o.key_image == key_image).ok_or("Failed to find key image")?;
            sweep(session, output.subaddress().0, parsed, SweepSelect::KeyImage(key_image)).await
        }
        Command::ShowTransfers { args } => show_transfers(session, &args).await,
        Command::ShowTransfer { txid } => show_transfer(session, &txid).await,
        Command::ExportTransfers { args } => export_transfers(session, &args).await,
        Command::IncomingTransfers { args } => incoming_transfers(session, &args).await,
        Command::UnspentOutputs { args } => unspent_outputs(session, &args).await,
        Command::SetTxNote { txid, note } => Ok(session.keys.set_note(&txid, &joined(&note)).await?),
        Command::GetTxNote { txid } => {
            match session.data()?.tx_notes.get(&txid) {
                Some(note) => println!("note found: {note}"),
                None => println!("no note found"),
            }
            Ok(())
        }
        Command::SetDescription { text } => {
            let text = joined(&text);
            session.keys.update_meta(|meta| {
                let _: () = meta.description = Some(text).filter(|t| !t.is_empty());
                Ok(())
            }).await?;
            Ok(())
        }
        Command::GetDescription => {
            match session.data()?.meta.description {
                Some(description) => println!("{description}"),
                None => println!("no description found"),
            }
            Ok(())
        }
        Command::AddressBook { args } => address_book(session, &args).await,
        Command::Freeze { key_image } => freeze(session, &key_image, true).await,
        Command::Thaw { key_image } => freeze(session, &key_image, false).await,
        Command::Frozen { key_image } => {
            let image = args::parse_key_image(&key_image)?;
            let data = session.data()?;
            let output = session.keys.outputs(&data)?.into_iter().find(|o| o.key_image == image).ok_or("Failed to find key image")?;
            println!("{}: {key_image}", if output.frozen { "Frozen" } else { "Not frozen" });
            Ok(())
        }
        Command::MarkOutputSpent { output } => mark_output(session, &output, true).await,
        Command::MarkOutputUnspent { output } => mark_output(session, &output, false).await,
        Command::IsOutputSpent { output } => {
            let global_index = args::parse_output_spec(&output)?;
            let data = session.data()?;
            let found = session.keys.outputs(&data)?.into_iter().find(|o| o.global_index() == global_index).ok_or("Output not found in the wallet")?;
            println!("{}: {output}", if found.spent { "Spent" } else { "Not spent" });
            Ok(())
        }
        Command::RescanSpent => {
            let unit = session.unit()?;
            let changed = session.wallet().await?.rescan_spent().await?;
            if changed.is_empty() {
                println!("No changes: every output's spent status matches the chain");
            }
            for (output, spent) in changed {
                println!("Output {} ({}, tx <{}>) is now {}", output.global_index(), money(output.amount(), unit), output.txid, if spent { "spent" } else { "unspent" });
            }
            Ok(())
        }
        Command::Refresh => {
            let wallet = session.wallet().await?;
            let (data, resolved) = wallet.refresh().await?;
            let tip = wallet.tip().await?;
            println!("Refresh done, transactions resolved: {resolved}, still pending: {}", data.pending.len());
            let unit = data.meta.settings.unit;
            let (balance, unlocked) = account_totals(&balances(&session.keys, &data, tip)?, data.meta.current_account);
            println!("Balance: {}, unlocked balance: {}", money(balance, unit), money(unlocked, unit));
            Ok(())
        }
        Command::Status => {
            let pending = session.data()?.pending.len();
            let wallet = session.wallet().await?;
            let tip = wallet.tip().await?;
            let version = wallet.daemon_version().await?;
            let ssl = if wallet.node_url().starts_with("https://") { "SSL" } else { "no SSL" };
            let sync = if pending == 0 { "synced".to_string() } else { format!("{pending} transaction(s) awaiting confirmation") };
            println!("Refreshed {tip}/{tip}, {sync}, daemon RPC v{}.{}, {ssl}", version.major, version.minor);
            Ok(())
        }
        Command::Fee => {
            let unit = session.unit()?;
            let wallet = session.wallet().await?;
            for (priority, name) in PRIORITY_NAMES.iter().enumerate().skip(1) {
                let per_weight = wallet.fee_per_weight(fee_priority(priority as u32)).await?;
                println!("Fee at priority {priority} ({name}): {} {} per byte", money(per_weight, unit), unit.name());
            }
            Ok(())
        }
        Command::BcHeight => {
            println!("{}", session.wallet().await?.tip().await?);
            Ok(())
        }
        Command::WalletInfo => {
            let data = session.data()?;
            println!("Filename: {}", session.keys.path().display());
            println!("Description: {}", data.meta.description.as_deref().unwrap_or("<Not set>"));
            println!("Address: {}", data.address);
            println!("Type: Normal");
            println!("Network type: Stagenet");
            Ok(())
        }
        Command::Seed => {
            let data = session.data()?;
            let seed = match data.mnemonic {
                Some(mnemonic) => mnemonic,
                None => legacy_seed_for(&data.private_spend_key, "English")?,
            };
            println!(
                "NOTE: the following {} words can be used to recover access to your wallet. Write them down and store them somewhere safe and secure. Please do not store them in your email or on file storage services outside of your immediate control.\n",
                seed.split_whitespace().count()
            );
            println!("{seed}");
            Ok(())
        }
        Command::Spendkey => {
            let (secret, public) = session.keys.spend_key_hex();
            println!("secret: {}\npublic: {public}", *secret);
            Ok(())
        }
        Command::Viewkey => {
            let (secret, public) = session.keys.view_key_hex();
            println!("secret: {}\npublic: {public}", *secret);
            Ok(())
        }
        Command::Set { args } => set(session, &args).await,
        Command::Save => {
            println!("Wallet data saved");
            Ok(())
        }
        Command::Version => {
            println!("stagenet-wallet-cli v{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Pocketchange { args } => {
            let parsed = args::parse_pocketchange(&args)?;
            let data = session.data()?;
            let request = TransferRequest {
                account: data.meta.current_account,
                subaddress_indexes: None,
                priority: fee_priority(parsed.priority.unwrap_or(data.meta.settings.priority)),
                kind: TransferKind::Pocketchange { pieces: parsed.pieces, inputs: parsed.inputs },
            };
            send_prepared(session, request, |prepared, unit| {
                let piece = prepared.destinations.first().map_or(0, |d| d.amount_piconero);
                format!(
                    "Splitting {} from {} output(s) into {} outputs of {} each (plus {} left over in the last).  The transaction fee is {}",
                    money(prepared.inputs_total, unit),
                    prepared.inputs,
                    prepared.destinations.len() + 1,
                    money(piece, unit),
                    money(prepared.change.saturating_sub(piece), unit),
                    money(prepared.fee, unit)
                )
            })
            .await
        }
        Command::AddOutput { txid } => {
            session.wallet().await?.add_output(&txid).await?;
            let data = session.data()?;
            if data.pending.iter().any(|p| p.txid == txid) {
                println!("Added {txid}; it isn't confirmed yet - refresh picks it up once it is");
            } else {
                println!("Added {txid}");
            }
            Ok(())
        }
    }
}

async fn balance(session: &mut Session, detail: bool) -> Result<(), CliError> {
    let wallet = session.wallet().await?;
    let (data, _) = wallet.refresh().await?;
    let tip = wallet.tip().await?;
    let unit = data.meta.settings.unit;
    let current = data.meta.current_account;
    let account = data.meta.account(current)?;
    let balances = balances(&session.keys, &data, tip)?;
    let (balance, unlocked) = account_totals(&balances, current);
    println!("Currently selected account: [{current}] {}", account.label);
    println!("Tag: {}", account.tag.as_deref().unwrap_or("(No tag assigned)"));
    println!("Balance: {}, unlocked balance: {}", money(balance, unit), money(unlocked, unit));
    if detail {
        println!("Balance per address:");
        println!("{:>15} {:>21} {:>21} {:>7} {:>21}", "Address", "Balance", "Unlocked balance", "Outputs", "Label");
        for (index, label) in account.subaddress_labels.iter().enumerate() {
            let (balance, unlocked, count) = balances.get(&(current, index as u32)).copied().unwrap_or_default();
            let address = session.keys.subaddress(current, index as u32);
            println!("{index:>8} {:>6} {:>21} {:>21} {count:>7} {label:>21}", short(&address), money(balance, unit), money(unlocked, unit));
        }
    }
    Ok(())
}

async fn print_accounts(session: &mut Session) -> Result<(), CliError> {
    let wallet = session.wallet().await?;
    let (data, _) = wallet.refresh().await?;
    let tip = wallet.tip().await?;
    let unit = data.meta.settings.unit;
    let balances = balances(&session.keys, &data, tip)?;
    let accounts = data.meta.accounts();
    let mut groups: BTreeMap<Option<String>, Vec<usize>> = BTreeMap::new();
    for (index, account) in accounts.iter().enumerate() {
        groups.entry(account.tag.clone()).or_default().push(index);
    }
    let (mut total, mut total_unlocked) = (0, 0);
    for (tag, indexes) in groups.iter().rev() {
        match tag {
            Some(tag) => {
                println!("Accounts with tag: {tag}");
                println!("Tag's description: {}", data.meta.tag_descriptions.get(tag).map(String::as_str).unwrap_or(""));
            }
            None if groups.len() > 1 => println!("Untagged accounts:"),
            None => {}
        }
        println!("{:>17} {:>21} {:>21} {:>21}", "Account", "Balance", "Unlocked balance", "Label");
        for &index in indexes {
            let (balance, unlocked) = account_totals(&balances, index as u32);
            total += balance;
            total_unlocked += unlocked;
            let marker = if index as u32 == data.meta.current_account { '*' } else { ' ' };
            let address = session.keys.subaddress(index as u32, 0);
            println!(" {marker}{index:>8} {:>6} {:>21} {:>21} {:>21}", short(&address), money(balance, unit), money(unlocked, unit), accounts[index].label);
        }
    }
    println!("----------------------------------------------------------------------------------");
    println!("{:>15} {:>21} {:>21}", "Total", money(total, unit), money(total_unlocked, unit));
    Ok(())
}

fn parse_account_index(word: Option<&String>) -> Result<u32, CliError> {
    let word = word.ok_or("missing account index")?;
    word.parse().map_err(|_| CliError::Usage(format!("failed to parse index: {word}")))
}

async fn account(session: &mut Session, args: &[String]) -> Result<(), CliError> {
    match args.first().map(String::as_str) {
        None => print_accounts(session).await,
        Some("new") => {
            let label = joined(&args[1..]);
            session.keys.update_meta(|meta| {
                meta.current_account = meta.add_account(&label);
                Ok(())
            })
            .await?;
            print_accounts(session).await
        }
        Some("switch") => {
            let index = parse_account_index(args.get(1))?;
            session
                .keys
                .update_meta(|meta| {
                    meta.account(index)?;
                    meta.current_account = index;
                    Ok(())
                })
                .await?;
            balance(session, false).await
        }
        Some("label") => {
            let index = parse_account_index(args.get(1))?;
            let label = joined(&args[2..]);
            session.keys.update_meta(|meta| meta.label_account(index, &label)).await?;
            print_accounts(session).await
        }
        Some("tag") => {
            let tag = args.get(1).ok_or("missing tag name")?.clone();
            let indexes = args[2..].iter().map(|w| parse_account_index(Some(w))).collect::<Result<Vec<_>, _>>()?;
            if indexes.is_empty() {
                return Err("missing account index".into());
            }
            session.keys.update_meta(|meta| meta.tag_accounts(Some(&tag), &indexes)).await?;
            print_accounts(session).await
        }
        Some("untag") => {
            let indexes = args[1..].iter().map(|w| parse_account_index(Some(w))).collect::<Result<Vec<_>, _>>()?;
            if indexes.is_empty() {
                return Err("missing account index".into());
            }
            session.keys.update_meta(|meta| meta.tag_accounts(None, &indexes)).await?;
            print_accounts(session).await
        }
        Some("tag_description") => {
            let tag = args.get(1).ok_or("missing tag name")?.clone();
            let description = joined(&args[2..]);
            session
                .keys
                .update_meta(|meta| {
                    if !meta.accounts().iter().any(|a| a.tag.as_deref() == Some(tag.as_str())) {
                        return Err(format!("Tag {tag} is unregistered."));
                    }
                    meta.tag_descriptions.insert(tag, description);
                    Ok(())
                })
                .await?;
            print_accounts(session).await
        }
        Some(other) => Err(format!("unknown account subcommand: {other}").into()),
    }
}

fn print_address_row(keys: &WalletKeys, account: u32, index: u32, label: &str) {
    println!("{index}  {}  {label}", keys.subaddress(account, index));
}

async fn address(session: &mut Session, args: &[String]) -> Result<(), CliError> {
    let data = session.data()?;
    let current = data.meta.current_account;
    let labels = data.meta.account(current)?.subaddress_labels;
    match args.first().map(String::as_str) {
        None => print_address_row(&session.keys, current, 0, &labels[0]),
        Some("all") => {
            for (index, label) in labels.iter().enumerate() {
                print_address_row(&session.keys, current, index as u32, label);
            }
        }
        Some("new") => {
            let label = joined(&args[1..]);
            let index = session.keys.update_meta(|meta| meta.add_subaddress(current, &label)).await?;
            print_address_row(&session.keys, current, index, &label);
        }
        Some("label") => {
            let index: u32 = args.get(1).ok_or("missing address index")?.parse().map_err(|_| "failed to parse index")?;
            let label = joined(&args[2..]);
            session.keys.update_meta(|meta| meta.label_subaddress(current, index, &label)).await?;
            print_address_row(&session.keys, current, index, &label);
        }
        Some("one-off") => {
            let [account, index] = [args.get(1), args.get(2)].map(|w| w.and_then(|w| w.parse::<u32>().ok()));
            let (Some(account), Some(index)) = (account, index) else { return Err("usage: address one-off <account> <subaddress>".into()) };
            print_address_row(&session.keys, account, index, "");
        }
        Some("device") => return Err("address device: this wallet has no hardware device".into()),
        Some(min) => {
            let parse = |w: &str| w.parse::<usize>().map_err(|_| CliError::Usage(format!("failed to parse index: {w}")));
            let min = parse(min)?;
            let max = match args.get(1) {
                Some(max) => parse(max)?,
                None => min,
            };
            if min > max || max >= labels.len() {
                return Err(format!("specify indices between 0 and {}", labels.len() - 1).into());
            }
            for (index, label) in labels.iter().enumerate().take(max + 1).skip(min) {
                print_address_row(&session.keys, current, index as u32, label);
            }
        }
    }
    Ok(())
}

fn rand_payment_id() -> [u8; 8] {
    use rand_core::RngCore;
    let mut id = [0; 8];
    rand_core::OsRng.fill_bytes(&mut id);
    id
}

fn integrated_address(session: &Session, arg: Option<&str>) -> Result<(), CliError> {
    match arg {
        None => {
            let id = rand_payment_id();
            println!("Random payment ID: {}", hex::encode(id));
            println!("Matching integrated address: {}", session.keys.integrated_address(id));
        }
        Some(pid) if pid.len() == 16 && pid.bytes().all(|b| b.is_ascii_hexdigit()) => {
            let id: [u8; 8] = hex::decode(pid).expect("checked hex").try_into().expect("checked length");
            println!("Matching integrated address: {}", session.keys.integrated_address(id));
        }
        Some(address) => {
            let parsed = monero_wallet::address::MoneroAddress::from_str(monero_wallet::address::Network::Stagenet, address)
                .map_err(|_| format!("failed to parse payment ID or address: {address}"))?;
            let id = parsed.payment_id().ok_or("Address is not an integrated address")?;
            let standard = monero_wallet::address::MoneroAddress::new(
                monero_wallet::address::Network::Stagenet,
                monero_wallet::address::AddressType::Legacy,
                parsed.spend(),
                parsed.view(),
            );
            println!("Integrated address: {standard}, payment ID: {}", hex::encode(id));
        }
    }
    Ok(())
}

fn payments(session: &Session, payment_ids: &[String]) -> Result<(), CliError> {
    let data = session.data()?;
    let unit = data.meta.settings.unit;
    let outputs = session.keys.outputs(&data)?;
    let mut header = false;
    for pid in payment_ids {
        let id: [u8; 8] = hex::decode(pid).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| format!("payment ID has invalid format, expected 16 hex characters: {pid}"))?;
        let matching: Vec<&OwnedOutput> = outputs.iter().filter(|o| o.payment_id() == Some(id)).collect();
        if matching.is_empty() {
            println!("No payments with id {pid}");
            continue;
        }
        if !header {
            println!("{:>68}{:>68}{:>12}{:>21}{:>16}", "payment", "transaction", "height", "amount", "addr index");
            header = true;
        }
        for output in matching {
            println!("{pid:>68}{:>68}{:>12}{:>21}{:>16}", output.txid, output.height, money(output.amount(), unit), output.subaddress().1);
        }
    }
    Ok(())
}

/// Asks "Is this okay?" unless `set always-confirm-transfers 0` or stdin
/// isn't a terminal (a script).
fn confirm(data: &WalletData, summary: &str) -> Result<bool, CliError> {
    if data.meta.settings.skip_transfer_confirmation || !std::io::stdin().is_terminal() {
        println!("{summary}");
        return Ok(true);
    }
    print!("{summary}\nIs this okay?  (Y/Yes/N/No): ");
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).map_err(|e| format!("failed to read the answer: {e}"))?;
    Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

async fn send_prepared(session: &mut Session, request: TransferRequest, summarize: impl FnOnce(&cli_wallet::PreparedTransfer, Unit) -> String) -> Result<(), CliError> {
    let data = session.data()?;
    let relay = !session.do_not_relay;
    let wallet = session.wallet().await?;
    let prepared = wallet.prepare_transfer(&request).await?;
    let summary = summarize(&prepared, data.meta.settings.unit);
    if !confirm(&data, &summary)? {
        println!("transaction cancelled.");
        return Ok(());
    }
    let committed = wallet.commit(prepared, relay).await?;
    let txid = hex::encode(committed.hash);
    match committed.unrelayed_hex {
        None => {
            println!("Transaction successfully submitted, transaction <{txid}>");
            println!("You can check its status by using the `show_transfers` command.");
        }
        Some(tx_hex) => {
            std::fs::write("raw_monero_tx", tx_hex).map_err(|e| format!("failed to write raw_monero_tx: {e}"))?;
            println!("Transaction successfully saved to raw_monero_tx, txid <{txid}>");
        }
    }
    Ok(())
}

async fn transfer(session: &mut Session, words: &[String]) -> Result<(), CliError> {
    let data = session.data()?;
    let parsed = args::parse_transfer(words, data.meta.settings.unit)?;
    let subtract_fee_from = match parsed.subtract_fee {
        SubtractFee::None => vec![],
        SubtractFee::All => (0..parsed.destinations.len()).collect(),
        SubtractFee::Some(indexes) => indexes,
    };
    let request = TransferRequest {
        account: data.meta.current_account,
        subaddress_indexes: index_filter(parsed.indexes),
        priority: fee_priority(parsed.priority.unwrap_or(data.meta.settings.priority)),
        kind: TransferKind::Pay { destinations: parsed.destinations, subtract_fee_from, split_change_into: None },
    };
    send_prepared(session, request, |prepared, unit| {
        let sent: u64 = prepared.destinations.iter().map(|d| d.amount_piconero).sum();
        format!("Sending {}.  The transaction fee is {}", money(sent, unit), money(prepared.fee, unit))
    })
    .await
}

async fn sweep(session: &mut Session, account: u32, parsed: args::SweepArgs, select: SweepSelect) -> Result<(), CliError> {
    let data = session.data()?;
    let request = TransferRequest {
        account,
        subaddress_indexes: index_filter(parsed.indexes),
        priority: fee_priority(parsed.priority.unwrap_or(data.meta.settings.priority)),
        kind: TransferKind::Sweep { address: parsed.address, outputs: parsed.outputs, select },
    };
    send_prepared(session, request, |prepared, unit| {
        format!("Sweeping {} in 1 transaction for a total fee of {}.", money(prepared.inputs_total, unit), money(prepared.fee, unit))
    })
    .await
}

/// One `show_transfers` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRow {
    pub height: Option<u64>,
    pub direction: &'static str,
    pub unlocked: Option<bool>,
    pub timestamp: Option<u64>,
    pub amount: u64,
    pub txid: String,
    pub payment_id: Option<[u8; 8]>,
    pub fee: Option<u64>,
    pub destinations: Vec<(String, u64)>,
    pub indexes: BTreeSet<u32>,
    pub account: u32,
}

/// Every transfer the wallet file knows of: `in` (outputs received,
/// grouped per transaction and account, from transactions it didn't send
/// itself), `out`/`pending` (its own sends, confirmed or not) and `pool`
/// (payments it was told to expect that haven't confirmed).
pub fn transfer_rows(keys: &WalletKeys, data: &WalletData, tip: u64) -> Result<Vec<TransferRow>, WalletError> {
    let sent_txids: BTreeSet<&str> = data.sent.iter().map(|s| s.txid.as_str()).collect();
    let mut incoming: BTreeMap<(String, u32), TransferRow> = BTreeMap::new();
    for output in keys.outputs(data)?.into_iter().filter(|o| !sent_txids.contains(o.txid.as_str())) {
        let (account, index) = output.subaddress();
        let row = incoming.entry((output.txid.clone(), account)).or_insert_with(|| TransferRow {
            height: Some(output.height),
            direction: "in",
            unlocked: Some(output.unlocked(tip)),
            timestamp: output.timestamp,
            amount: 0,
            txid: output.txid.clone(),
            payment_id: output.payment_id(),
            fee: None,
            destinations: vec![],
            indexes: BTreeSet::new(),
            account,
        });
        row.amount += output.amount();
        row.indexes.insert(index);
    }
    let mut rows: Vec<TransferRow> = incoming.into_values().collect();
    for sent in &data.sent {
        rows.push(TransferRow {
            height: sent.height,
            direction: if sent.height.is_some() { "out" } else { "pending" },
            unlocked: None,
            timestamp: sent.timestamp,
            amount: sent.destinations.iter().map(|d| d.amount_piconero).sum(),
            txid: sent.txid.clone(),
            payment_id: None,
            fee: Some(sent.fee_piconero),
            destinations: sent.destinations.iter().map(|d| (d.address.clone(), d.amount_piconero)).collect(),
            indexes: BTreeSet::from([0]),
            account: sent.account,
        });
    }
    for pending in data.pending.iter().filter(|p| !sent_txids.contains(p.txid.as_str())) {
        rows.push(TransferRow {
            height: None,
            direction: "pool",
            unlocked: Some(false),
            timestamp: None,
            amount: pending.amount_piconero,
            txid: pending.txid.clone(),
            payment_id: None,
            fee: None,
            destinations: vec![],
            indexes: BTreeSet::new(),
            account: data.meta.current_account,
        });
    }
    rows.sort_by_key(|row| (row.height.unwrap_or(u64::MAX), row.timestamp));
    Ok(rows)
}

fn filter_rows(rows: Vec<TransferRow>, filter: &args::HistoryArgs, account: u32) -> Vec<TransferRow> {
    let indexes = index_filter(filter.indexes.clone());
    rows.into_iter()
        .filter(|row| row.account == account)
        .filter(|row| match row.direction {
            "in" => filter.incoming,
            "out" => filter.outgoing,
            "pending" => filter.pending,
            _ => filter.pool,
        })
        .filter(|row| row.height.is_none_or(|h| h >= filter.min_height && h <= filter.max_height))
        .filter(|row| indexes.as_ref().is_none_or(|wanted| row.indexes.is_empty() || row.indexes.iter().any(|i| wanted.contains(i))))
        .collect()
}

pub fn format_transfer_row(row: &TransferRow, note: &str, unit: Unit) -> String {
    let height = row.height.map_or("-".to_string(), |h| h.to_string());
    let lock = match row.unlocked {
        Some(true) => "unlocked",
        Some(false) => "locked",
        None => "-",
    };
    let payment_id = hex::encode(row.payment_id.unwrap_or([0; 8]));
    let fee = row.fee.map_or("-".to_string(), |fee| money(fee, unit));
    let destinations = if row.destinations.is_empty() {
        "-".to_string()
    } else {
        row.destinations.iter().map(|(address, amount)| format!("{}:{}", short(address), money(*amount, unit))).collect::<Vec<_>>().join(", ")
    };
    let indexes = row.indexes.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    format!(
        "{height:>8} {:>7} {lock:>8} {:>25} {:>20} {} {payment_id} {fee:>14} {destinations} {indexes} - {note}",
        row.direction,
        format_timestamp(row.timestamp),
        money(row.amount, unit),
        row.txid
    )
}

async fn tip_and_data(session: &mut Session) -> Result<(u64, WalletData), CliError> {
    let wallet = session.wallet().await?;
    let (data, _) = wallet.refresh().await?;
    Ok((wallet.tip().await?, data))
}

async fn show_transfers(session: &mut Session, words: &[String]) -> Result<(), CliError> {
    let filter = args::parse_history(words)?;
    let (tip, data) = tip_and_data(session).await?;
    let unit = data.meta.settings.unit;
    let rows = filter_rows(transfer_rows(&session.keys, &data, tip)?, &filter, data.meta.current_account);
    if rows.is_empty() {
        println!("No transfers found");
    }
    for row in rows {
        println!("{}", format_transfer_row(&row, data.tx_notes.get(&row.txid).map(String::as_str).unwrap_or(""), unit));
    }
    Ok(())
}

async fn show_transfer(session: &mut Session, txid: &str) -> Result<(), CliError> {
    let (tip, data) = tip_and_data(session).await?;
    let unit = data.meta.settings.unit;
    let rows: Vec<TransferRow> = transfer_rows(&session.keys, &data, tip)?.into_iter().filter(|row| row.txid == txid).collect();
    if rows.is_empty() {
        return Err("Transaction ID not found".into());
    }
    let note = data.tx_notes.get(txid).map(String::as_str).unwrap_or("");
    for row in rows {
        let direction = match row.direction {
            "in" => "Incoming transaction found",
            "out" => "Outgoing transaction found",
            "pending" => "Unconfirmed outgoing transaction found",
            _ => "Unconfirmed incoming transaction found",
        };
        println!("{direction}");
        println!("txid: {}", row.txid);
        println!("Height: {}", row.height.map_or("-".to_string(), |h| h.to_string()));
        println!("Timestamp: {}", format_timestamp(row.timestamp));
        println!("Amount: {}", money(row.amount, unit));
        println!("Payment ID: {}", hex::encode(row.payment_id.unwrap_or([0; 8])));
        if let Some(fee) = row.fee {
            let change = data.sent.iter().find(|s| s.txid == row.txid).map_or(0, |s| s.change_piconero);
            println!("Change: {}", money(change, unit));
            println!("Fee: {}", money(fee, unit));
            for (address, amount) in &row.destinations {
                println!("Destination: {address} {}", money(*amount, unit));
            }
        } else if row.direction == "in" {
            println!("Address index: {}", row.indexes.iter().map(u32::to_string).collect::<Vec<_>>().join(","));
            if let Some(unlocked) = row.unlocked {
                println!("{}", if unlocked { "Unlocked" } else { "Locked" });
            }
        }
        println!("Note: {note}");
    }
    Ok(())
}

fn csv_field(field: &str) -> String {
    if field.contains([',', '"', '\n']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

async fn export_transfers(session: &mut Session, words: &[String]) -> Result<(), CliError> {
    let filter = args::parse_history(words)?;
    let (tip, data) = tip_and_data(session).await?;
    let unit = data.meta.settings.unit;
    let account = data.meta.current_account;
    let path = filter.output.clone().unwrap_or_else(|| format!("output{account}.csv"));
    let rows = filter_rows(transfer_rows(&session.keys, &data, tip)?, &filter, account);
    let mut csv = String::from("block,direction,unlocked,timestamp,amount,running balance,hash,payment ID,fee,destination,amount,index,note\n");
    let mut running: i128 = 0;
    for row in rows {
        match row.direction {
            "in" => running += row.amount as i128,
            "out" | "pending" => running -= (row.amount + row.fee.unwrap_or(0)) as i128,
            _ => {}
        }
        let running_text = if running < 0 { format!("-{}", money(running.unsigned_abs() as u64, unit)) } else { money(running as u64, unit) };
        let lines: Vec<(String, String)> = if row.destinations.is_empty() {
            vec![(String::new(), String::new())]
        } else {
            row.destinations.iter().map(|(address, amount)| (address.clone(), money(*amount, unit))).collect()
        };
        for (i, (destination, destination_amount)) in lines.into_iter().enumerate() {
            let fields = [
                row.height.map_or(String::new(), |h| h.to_string()),
                row.direction.to_string(),
                row.unlocked.map_or(String::new(), |u| if u { "unlocked" } else { "locked" }.to_string()),
                format_timestamp(row.timestamp),
                if i == 0 { money(row.amount, unit) } else { String::new() },
                if i == 0 { running_text.clone() } else { String::new() },
                row.txid.clone(),
                hex::encode(row.payment_id.unwrap_or([0; 8])),
                if i == 0 { row.fee.map_or(String::new(), |f| money(f, unit)) } else { String::new() },
                destination,
                destination_amount,
                row.indexes.iter().map(u32::to_string).collect::<Vec<_>>().join(","),
                data.tx_notes.get(&row.txid).cloned().unwrap_or_default(),
            ];
            csv.push_str(&fields.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
            csv.push('\n');
        }
    }
    std::fs::write(&path, csv).map_err(|e| format!("failed to write {path}: {e}"))?;
    println!("CSV exported to {path}");
    Ok(())
}

pub fn format_incoming_header(verbose: bool) -> String {
    let mut header = format!("{:>21}{:>8}{:>12}{:>8}{:>16}{:>68}{:>16}", "amount", "spent", "unlocked", "ringct", "global index", "tx id", "addr index");
    if verbose {
        header.push_str(&format!("{:>68}{:>68}", "pubkey", "key image"));
    }
    header
}

pub fn format_incoming_row(output: &OwnedOutput, tip: u64, verbose: bool, unit: Unit) -> String {
    let lock = if output.frozen {
        "[frozen]"
    } else if output.unlocked(tip) {
        "unlocked"
    } else {
        "locked"
    };
    let mut row = format!(
        "{:>21}{:>8}{:>12}{:>8}{:>16}{:>68}{:>16}",
        money(output.amount(), unit),
        if output.spent { "T" } else { "F" },
        lock,
        "RingCT",
        output.global_index(),
        format!("<{}>", output.txid),
        output.subaddress().1
    );
    if verbose {
        row.push_str(&format!("{:>68}{:>68}", format!("<{}>", output.public_key_hex()), format!("<{}>", hex::encode(output.key_image))));
    }
    row
}

async fn incoming_transfers(session: &mut Session, words: &[String]) -> Result<(), CliError> {
    let mut args: std::collections::VecDeque<String> = words.iter().cloned().collect();
    let mut available = None;
    let mut verbose = false;
    while let Some(word) = args.front().cloned() {
        match word.as_str() {
            "available" => available = Some(true),
            "unavailable" => available = Some(false),
            "verbose" => verbose = true,
            "uses" => return Err("incoming_transfers uses: not supported - it needs a chain scan".into()),
            _ => break,
        }
        args.pop_front();
    }
    let indexes = index_filter(args::take_index(&mut args, false)?);
    if let Some(extra) = args.front() {
        return Err(format!("unexpected argument: {extra}").into());
    }
    let (tip, data) = tip_and_data(session).await?;
    let unit = data.meta.settings.unit;
    let account = data.meta.current_account;
    let outputs: Vec<OwnedOutput> = session
        .keys
        .outputs(&data)?
        .into_iter()
        .filter(|o| o.subaddress().0 == account)
        .filter(|o| indexes.as_ref().is_none_or(|wanted| wanted.contains(&o.subaddress().1)))
        .filter(|o| available.is_none_or(|available| available != o.spent))
        .collect();
    if outputs.is_empty() {
        match available {
            Some(true) => println!("No incoming available transfers"),
            Some(false) => println!("No incoming unavailable transfers"),
            None => println!("No incoming transfers"),
        }
        return Ok(());
    }
    println!("{}", format_incoming_header(verbose));
    for output in outputs {
        println!("{}", format_incoming_row(&output, tip, verbose, unit));
    }
    Ok(())
}

async fn unspent_outputs(session: &mut Session, words: &[String]) -> Result<(), CliError> {
    let unit = session.unit()?;
    let mut args: std::collections::VecDeque<String> = words.iter().cloned().collect();
    let indexes = index_filter(args::take_index(&mut args, false)?);
    let min_amount = args.pop_front().map(|w| parse_amount(&w, unit)).transpose()?.unwrap_or(0);
    let max_amount = args.pop_front().map(|w| parse_amount(&w, unit)).transpose()?.unwrap_or(u64::MAX);
    if min_amount > max_amount {
        return Err("<min_amount> should be smaller than <max_amount>".into());
    }
    let (_, data) = tip_and_data(session).await?;
    let account = data.meta.current_account;
    let outputs: Vec<OwnedOutput> = session
        .keys
        .outputs(&data)?
        .into_iter()
        .filter(|o| !o.spent && o.subaddress().0 == account)
        .filter(|o| indexes.as_ref().is_none_or(|wanted| wanted.contains(&o.subaddress().1)))
        .filter(|o| (min_amount..=max_amount).contains(&o.amount()))
        .collect();
    if outputs.is_empty() {
        println!("There is no unspent output in the specified address");
        return Ok(());
    }
    print!("{}", format_unspent(&outputs, unit));
    Ok(())
}

/// The table, summary and height histogram `unspent_outputs` prints.
pub fn format_unspent(outputs: &[OwnedOutput], unit: Unit) -> String {
    let mut text = format!("{:>21} {:>12}\n", "Amount", "Height");
    for output in outputs {
        text.push_str(&format!("{:>21} {:>12}\n", money(output.amount(), unit), output.height));
    }
    let heights = outputs.iter().map(|o| o.height);
    let amounts = outputs.iter().map(|o| o.amount());
    let (min_height, max_height) = (heights.clone().min().unwrap_or(0), heights.max().unwrap_or(0));
    text.push_str(&format!("\nMin block height: {min_height}\nMax block height: {max_height}\n"));
    text.push_str(&format!("Min amount found: {}\nMax amount found: {}\n", money(amounts.clone().min().unwrap_or(0), unit), money(amounts.max().unwrap_or(0), unit)));
    text.push_str(&format!("Total count: {}\n", outputs.len()));

    // Outputs per block-height bin, as a bar chart.
    const BINS: u64 = 10;
    let bin_size = (max_height - min_height) / BINS + 1;
    let mut counts = [0usize; BINS as usize];
    for output in outputs {
        counts[(((output.height - min_height) / bin_size).min(BINS - 1)) as usize] += 1;
    }
    text.push_str(&format!("\nBin size: {bin_size}\nOutputs per bin (block height from {min_height}):\n"));
    for (bin, count) in counts.iter().enumerate() {
        text.push_str(&format!("{:>12} |{}\n", min_height + bin as u64 * bin_size, "*".repeat(*count)));
    }
    text
}

async fn address_book(session: &mut Session, args: &[String]) -> Result<(), CliError> {
    match args.first().map(String::as_str) {
        None => {
            let book = session.data()?.meta.address_book;
            if book.is_empty() {
                println!("Address book is empty.");
            }
            for (index, entry) in book.iter().enumerate() {
                println!("Index: {index}\nAddress: {}\nDescription: {}\n", entry.address, entry.description);
            }
            Ok(())
        }
        Some("add") => {
            let address = args.get(1).ok_or("missing address")?.clone();
            monero_wallet::address::MoneroAddress::from_str(monero_wallet::address::Network::Stagenet, &address)
                .map_err(|e| format!("failed to parse address {address}: {e}"))?;
            let description = joined(&args[2..]);
            session.keys.update_meta(|meta| {
                let _: () = meta.address_book.push(AddressBookEntry { address, description });
                Ok(())
            }).await?;
            Ok(())
        }
        Some("delete") => {
            let index: usize = args.get(1).ok_or("missing index")?.parse().map_err(|_| "failed to parse index")?;
            session
                .keys
                .update_meta(|meta| {
                    if index >= meta.address_book.len() {
                        return Err(format!("failed to delete address book row {index}: there is no such row"));
                    }
                    meta.address_book.remove(index);
                    Ok(())
                })
                .await?;
            Ok(())
        }
        Some(other) => Err(format!("unknown address_book subcommand: {other}").into()),
    }
}

async fn freeze(session: &mut Session, key_image: &str, frozen: bool) -> Result<(), CliError> {
    let image = args::parse_key_image(key_image)?;
    if !session.keys.set_frozen(image, frozen).await? {
        return Err("Failed to find key image".into());
    }
    Ok(())
}

async fn mark_output(session: &mut Session, spec: &str, spent: bool) -> Result<(), CliError> {
    let global_index = args::parse_output_spec(spec)?;
    if !session.keys.set_spent(global_index, spent).await? {
        return Err("Output not found in the wallet".into());
    }
    Ok(())
}

async fn set(session: &mut Session, args: &[String]) -> Result<(), CliError> {
    let data = session.data()?;
    let settings = &data.meta.settings;
    let Some(option) = args.first() else {
        println!("seed = Electrum");
        println!("always-confirm-transfers = {}", u8::from(!settings.skip_transfer_confirmation));
        println!("default-ring-size = {RING_LEN}");
        println!("priority = {} ({})", settings.priority, PRIORITY_NAMES[settings.priority as usize]);
        println!("unit = {}", settings.unit.name());
        return Ok(());
    };
    let value = args.get(1).ok_or_else(|| format!("set {option}: needs a value"))?.clone();
    match option.as_str() {
        "priority" => {
            let priority = args::parse_priority(&value).ok_or_else(|| format!("priority must be one of {PRIORITY_NAMES:?} or 0-4"))?;
            session.keys.update_meta(|meta| {
                let _: () = meta.settings.priority = priority;
                Ok(())
            }).await?;
        }
        "unit" => {
            let unit = Unit::parse(&value).ok_or_else(|| format!("unit must be one of {:?}", Unit::ALL.map(Unit::name)))?;
            session.keys.update_meta(|meta| {
                let _: () = meta.settings.unit = unit;
                Ok(())
            }).await?;
        }
        "always-confirm-transfers" => {
            let confirm = match value.as_str() {
                "1" | "yes" | "true" => true,
                "0" | "no" | "false" => false,
                _ => return Err("always-confirm-transfers must be 0 or 1".into()),
            };
            session.keys.update_meta(|meta| {
                let _: () = meta.settings.skip_transfer_confirmation = !confirm;
                Ok(())
            }).await?;
        }
        "default-ring-size" => {
            if value != "0" && value != RING_LEN.to_string() {
                return Err(format!("default-ring-size must be {RING_LEN}, the only size the network accepts").into());
            }
        }
        other => return Err(format!("set: {other} isn't supported by this wallet").into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_format_as_utc_dates() {
        assert_eq!(format_timestamp(Some(0)), "1970-01-01 00:00:00");
        assert_eq!(format_timestamp(Some(1_700_000_000)), "2023-11-14 22:13:20");
        assert_eq!(format_timestamp(Some(951_782_400)), "2000-02-29 00:00:00");
        assert_eq!(format_timestamp(None), "-");
    }

    #[test]
    fn csv_fields_are_quoted_only_when_needed() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }
}
