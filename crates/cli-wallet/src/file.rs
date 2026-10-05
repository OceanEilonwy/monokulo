//! One SQLite database file per wallet - keys, `monero-wallet-cli`-style
//! metadata, and the wallet's own ledger of outputs and sends, all
//! together (see [`WalletData`], and [`crate::store`] for the schema). The
//! file is the whole wallet: nothing about one wallet lives anywhere else,
//! and no two wallets share a file.
//!
//! Every change is a read-modify-write under an exclusive lock on a
//! sibling `<file>.lock` ([`WalletFile::lock`]), and every write is one
//! database transaction, so parallel e2e runs sending from the same wallet
//! can neither corrupt the file nor lose each other's updates.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::meta::WalletMeta;
use crate::{network_name, parse_network, Network, WalletCredentials, WalletError};

/// Everything one wallet file holds, read whole into memory. Its `Debug`
/// leaves out the private keys and the mnemonic.
#[derive(Clone, PartialEq)]
pub struct WalletData {
    /// `stagenet` or `testnet` (see [`crate::NETWORKS`]): which network the
    /// keys derive addresses for - [`Self::network`] reads it.
    pub network: String,
    pub address: String,
    pub private_spend_key: String,
    pub private_view_key: String,
    /// The seed phrase these keys came from, when known - what `seed`
    /// prints.
    pub mnemonic: Option<String>,
    pub meta: WalletMeta,
    /// Every output this wallet has been told about and resolved - see the
    /// crate docs for the "informed, not scanned" model.
    pub outputs: Vec<OutputRecord>,
    /// Transactions expected to pay this wallet that haven't confirmed yet
    /// (a send's own change, a faucet payout added with `add_output`).
    pub pending: Vec<PendingTx>,
    /// Transactions this wallet built and broadcast - what
    /// `show_transfers` lists as `out`. Nothing on-chain says where a
    /// transaction's money went, so only this record knows.
    pub sent: Vec<SentRecord>,
    /// `set_tx_note`: txid to note.
    pub tx_notes: BTreeMap<String, String>,
    /// Anything else recorded about this wallet (its e2e role, where it
    /// was funded from), kept as-is.
    pub extra: serde_json::Map<String, Value>,
}

impl std::fmt::Debug for WalletData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletData")
            .field("network", &self.network)
            .field("address", &self.address)
            .field("private_spend_key", &"<redacted>")
            .field("private_view_key", &"<redacted>")
            .field("mnemonic", &self.mnemonic.as_ref().map(|_| "<redacted>"))
            .field("outputs", &self.outputs.len())
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

/// One resolved output: the whole `WalletOutput`, serialized, so reading
/// it back never touches the chain.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputRecord {
    pub txid: String,
    pub height: u64,
    /// The confirming block's timestamp. `None` for outputs resolved
    /// before it was recorded.
    pub timestamp: Option<u64>,
    /// Hex-encoded `WalletOutput::serialize()` (a BLOB in the file).
    pub serialized_output_hex: String,
    /// Informational - the authoritative amount is inside the serialized
    /// output.
    pub amount_piconero: u64,
    pub spent: bool,
    /// `freeze <key_image>`: never picked as an input, and left out of the
    /// balance, until thawed.
    pub frozen: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingTx {
    pub txid: String,
    /// What the transaction is expected to pay this wallet, when known
    /// (a send's change) - informational only.
    pub amount_piconero: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SentRecord {
    pub txid: String,
    pub account: u32,
    pub destinations: Vec<SentDestination>,
    pub fee_piconero: u64,
    pub change_piconero: u64,
    /// Filled in once the transaction's own change output resolves (same
    /// block, so no extra lookup).
    pub height: Option<u64>,
    pub timestamp: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SentDestination {
    pub address: String,
    pub amount_piconero: u64,
}

impl WalletData {
    /// A fresh wallet file's contents for `credentials`, on `network`: no
    /// outputs yet.
    pub fn new(network: Network, credentials: WalletCredentials) -> Self {
        WalletData {
            network: network_name(network).to_string(),
            address: credentials.address,
            private_spend_key: credentials.private_spend_key_hex,
            private_view_key: credentials.private_view_key_hex,
            mnemonic: credentials.mnemonic,
            meta: WalletMeta::default(),
            outputs: Vec::new(),
            pending: Vec::new(),
            sent: Vec::new(),
            tx_notes: BTreeMap::new(),
            extra: serde_json::Map::new(),
        }
    }

    /// The network this wallet is on, refusing one it doesn't work on.
    pub fn network(&self) -> Result<Network, WalletError> {
        parse_network(&self.network)
    }

    /// Adds `txid` to the pending list, unless it's already there.
    pub fn add_pending(&mut self, txid: &str, amount_piconero: u64) {
        if !self.pending.iter().any(|p| p.txid == txid) {
            self.pending.push(PendingTx {
                txid: txid.to_string(),
                amount_piconero,
            });
        }
    }
}

/// A wallet file on disk and its contents.
pub struct WalletFile {
    path: PathBuf,
    pub data: WalletData,
}

/// Holds a wallet file's exclusive lock until dropped. See
/// [`WalletFile::lock`].
pub struct WalletFileLock {
    _file: std::fs::File,
}

impl WalletFile {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, WalletError> {
        let path = path.as_ref();
        let data = crate::store::read(path)?;
        // Addresses are derived for the recorded network, so it has to be
        // one this wallet works on (never mainnet).
        if let Err(e) = data.network() {
            return Err(WalletError::WalletFile(format!("{}: {e}", path.display())));
        }
        Ok(WalletFile {
            path: path.to_path_buf(),
            data,
        })
    }

    /// Writes a new wallet file. Refuses to replace one that exists.
    pub fn create(path: impl AsRef<Path>, data: WalletData) -> Result<Self, WalletError> {
        let path = path.as_ref();
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| {
                WalletError::WalletFile(format!("failed to create {}: {e}", dir.display()))
            })?;
        }
        // Claimed atomically: of two creates of one path, one gets the file
        // and the other an error, rather than both "succeeding" and the
        // second overwriting the first's keys.
        let mut claim = std::fs::OpenOptions::new();
        claim.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            claim.mode(0o600);
        }
        claim.open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                WalletError::WalletFile(format!("wallet file {} already exists", path.display()))
            } else {
                WalletError::WalletFile(format!("failed to create {}: {e}", path.display()))
            }
        })?;
        let file = WalletFile {
            path: path.to_path_buf(),
            data,
        };
        // A wallet that couldn't be written leaves no empty file behind.
        if let Err(e) = file.save() {
            let _ = std::fs::remove_file(path);
            return Err(e);
        }
        Ok(file)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Atomically and durably replaces what the file holds with
    /// `self.data`: one transaction, synced before it commits, so a crash
    /// just after a send can't lose the outputs it spent and let the next
    /// send double-spend them. Callers changing a file that already exists
    /// hold its [`Self::lock`].
    ///
    /// The file holds spend keys and a mnemonic, so it is readable by its
    /// owner alone (0600 on unix).
    pub fn save(&self) -> Result<(), WalletError> {
        crate::store::write(&self.path, &self.data)
    }

    /// Takes the exclusive lock on `path`'s sibling `<path>.lock`, with
    /// [`default_busy_handler`] deciding what happens if someone else holds
    /// it.
    pub async fn lock(path: impl AsRef<Path>) -> Result<WalletFileLock, WalletError> {
        Self::lock_with(path, &default_busy_handler()).await
    }

    /// Takes the exclusive lock on `path`'s sibling `<path>.lock`. If
    /// another holder (another process, or another task in this one) has
    /// it, `on_busy` is told who and chooses: try again, wait for it, or
    /// give up with [`WalletError::Locked`]. Once taken, who holds it is
    /// written beside it (`<path>.lock.holder`, since Windows won't let
    /// another process read a locked file), for the next process's
    /// `on_busy`. Everything runs off the async runtime's worker threads.
    ///
    /// An OS file lock is released when its process exits, however it
    /// exits, so a lock is never left behind by a crash.
    pub async fn lock_with(
        path: impl AsRef<Path>,
        on_busy: &BusyHandler,
    ) -> Result<WalletFileLock, WalletError> {
        let lock_path = lock_path(path.as_ref());
        let on_busy = on_busy.clone();
        tokio::task::spawn_blocking(move || {
            let open_error = |e: std::io::Error| {
                WalletError::WalletFile(format!("failed to open {}: {e}", lock_path.display()))
            };
            let lock_error = |e: std::io::Error| {
                WalletError::WalletFile(format!("failed to lock {}: {e}", lock_path.display()))
            };
            loop {
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(&lock_path)
                    .map_err(open_error)?;
                match file.try_lock() {
                    Ok(()) => return Ok(WalletFileLock::recording_holder(file, &lock_path)),
                    Err(std::fs::TryLockError::WouldBlock) => {}
                    Err(std::fs::TryLockError::Error(e)) => return Err(lock_error(e)),
                }
                let holder = LockHolder {
                    lock_path: lock_path.clone(),
                    description: std::fs::read_to_string(holder_path(&lock_path))
                        .ok()
                        .filter(|held_by| !held_by.trim().is_empty())
                        .unwrap_or_else(|| "an unknown process".to_string()),
                };
                match on_busy(&holder) {
                    BusyChoice::Retry => continue,
                    BusyChoice::Wait => {
                        file.lock().map_err(lock_error)?;
                        return Ok(WalletFileLock::recording_holder(file, &lock_path));
                    }
                    BusyChoice::Cancel => return Err(WalletError::Locked(holder.to_string())),
                }
            }
        })
        .await
        .expect("the lock task never panics")
    }

    /// Locks `path`, loads it fresh, applies `change`, and saves - the one
    /// safe way to change a wallet file another process might be changing
    /// too. Nothing is saved if `change` fails.
    pub async fn update<T>(
        path: impl AsRef<Path>,
        change: impl FnOnce(&mut WalletData) -> Result<T, WalletError>,
    ) -> Result<T, WalletError> {
        Self::update_with(path, &default_busy_handler(), change).await
    }

    /// [`Self::update`], with `on_busy` handling a held lock (see
    /// [`Self::lock_with`]).
    pub async fn update_with<T>(
        path: impl AsRef<Path>,
        on_busy: &BusyHandler,
        change: impl FnOnce(&mut WalletData) -> Result<T, WalletError>,
    ) -> Result<T, WalletError> {
        let _lock = Self::lock_with(path.as_ref(), on_busy).await?;
        let mut file = Self::load(path)?;
        let result = change(&mut file.data)?;
        file.save()?;
        Ok(result)
    }
}

impl WalletFileLock {
    /// Writes who now holds the lock beside the lock file, so a process
    /// that finds it busy can say who's using the wallet.
    fn recording_holder(file: std::fs::File, lock_path: &Path) -> Self {
        let program = std::env::args()
            .next()
            .map(|path| {
                Path::new(&path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_default();
        let args = redacted_args(std::env::args().skip(1));
        let mut holder = format!("pid {} ({program} {})", std::process::id(), args.join(" "));
        holder.truncate(200);
        // Best effort: the lock itself is what matters, not the note.
        let _ = std::fs::write(holder_path(lock_path), holder.trim_end());
        WalletFileLock { _file: file }
    }
}

/// The command line as the lock note records it: the value of any option
/// that can carry a secret (a seed, a key, a password) is replaced, since
/// the note is a plain file left on disk and shown to whoever next finds
/// the wallet busy.
fn redacted_args(args: impl Iterator<Item = String>) -> Vec<String> {
    let secret = |name: &str| {
        let name = name.to_ascii_lowercase();
        ["seed", "key", "password", "mnemonic", "secret"]
            .iter()
            .any(|word| name.contains(word))
    };
    let mut out = Vec::new();
    let mut hide_next = false;
    for arg in args {
        if hide_next {
            out.push("<redacted>".to_string());
            hide_next = false;
            continue;
        }
        match arg.split_once('=') {
            Some((name, _)) if name.starts_with('-') && secret(name) => {
                out.push(format!("{name}=<redacted>"));
            }
            _ if arg.starts_with('-') && secret(&arg) => {
                out.push(arg);
                hide_next = true;
            }
            _ => out.push(arg),
        }
    }
    out
}

/// Who holds a wallet file's lock, as the holder recorded it.
#[derive(Debug, Clone)]
pub struct LockHolder {
    pub lock_path: PathBuf,
    /// e.g. `pid 1234 (wallet-cli transfer ...)`.
    pub description: String,
}

impl std::fmt::Display for LockHolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is locked by {}",
            self.lock_path.with_extension("").display(),
            self.description
        )
    }
}

/// What to do about a wallet file someone else has locked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyChoice {
    /// Try to take it again straight away (and ask again if still busy).
    Retry,
    /// Block until the holder lets go.
    Wait,
    /// Give up: the operation fails with [`WalletError::Locked`].
    Cancel,
}

/// Decides what to do when a wallet file's lock is held - see
/// [`WalletFile::lock_with`]. Called on a blocking thread, so it may
/// prompt the user.
pub type BusyHandler = std::sync::Arc<dyn Fn(&LockHolder) -> BusyChoice + Send + Sync>;

/// Says who holds the lock, on stderr, and waits for them - right for the
/// e2e suites and scripts, where nobody's there to ask.
pub fn default_busy_handler() -> BusyHandler {
    std::sync::Arc::new(|holder: &LockHolder| {
        eprintln!("cli-wallet: {holder}; waiting for it to finish");
        BusyChoice::Wait
    })
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

/// Where a lock's holder writes who it is: `<lock>.holder`.
fn holder_path(lock_path: &Path) -> PathBuf {
    let mut name = lock_path.file_name().unwrap_or_default().to_os_string();
    name.push(".holder");
    lock_path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lock note never carries a secret from the command line.
    #[test]
    fn secret_option_values_are_redacted_from_the_lock_note() {
        let args = [
            "--restore-deterministic-wallet",
            "--electrum-seed",
            "abbey abbey abbey",
            "--spend-key=deadbeef",
            "balance",
        ]
        .map(String::from);
        let redacted = redacted_args(args.into_iter()).join(" ");
        assert!(!redacted.contains("abbey"), "{redacted}");
        assert!(!redacted.contains("deadbeef"), "{redacted}");
        assert!(redacted.contains("balance"), "{redacted}");
        assert!(
            redacted.contains("--electrum-seed <redacted>"),
            "{redacted}"
        );
    }

    /// A saved wallet file is its owner's alone.
    #[cfg(unix)]
    #[test]
    fn a_saved_wallet_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("cli-wallet-perms-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("w.db");
        let file = WalletFile {
            path: path.clone(),
            data: WalletData::new(
                Network::Stagenet,
                crate::WalletCredentials {
                    address: "a".into(),
                    private_spend_key_hex: "00".repeat(32),
                    private_view_key_hex: "00".repeat(32),
                    mnemonic: None,
                },
            ),
        };
        file.save().unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
