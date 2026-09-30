//! The wallet itself: [`WalletKeys`] for everything that works offline
//! against the wallet file, [`Wallet`] (which derefs to it) for everything
//! that needs a node.

use std::path::{Path, PathBuf};

use monero_daemon_rpc::{prelude::*, MoneroDaemon};
use monero_wallet::{
    address::{MoneroAddress, Network, SubaddressIndex},
    ed25519::{Point, Scalar},
    extra::PaymentId,
    interface::FeePriority,
    ringct::RctType,
    send::{Change, SendError, SignableTransaction},
    transaction::Transaction,
    OutputWithDecoys, Scanner, ViewPair, WalletOutput,
};
use rand_core::OsRng;
use serde::Serialize;
use serde_json::Value;
use zeroize::Zeroizing;

use crate::file::{
    default_busy_handler, BusyHandler, OutputRecord, SentDestination, SentRecord, WalletData,
    WalletFile, WalletFileLock,
};
use crate::meta::WalletMeta;
use crate::{
    decode_output, locate_height, scalar_from_hex, DecoyCache, ReqwestTransport, WalletError,
    RING_LEN, SPENDABLE_AGE,
};

/// The highest fee rate (piconero per unit of weight) this crate accepts
/// from a node - comfortably above the `priority` level (about 4_000_000 on
/// stagenet today), still far below anything that could drain a wallet.
const MAX_FEE_PER_WEIGHT: u64 = 20_000_000;

/// Everything a wallet can do without a node: its keys and addresses, and
/// reading and editing its own wallet file. [`Wallet`] derefs to this, so
/// every method here works on a connected wallet too.
pub struct WalletKeys {
    view_pair: ViewPair,
    spend_key: Zeroizing<Scalar>,
    view_key: Zeroizing<Scalar>,
    address: MoneroAddress,
    path: PathBuf,
    /// Decides what happens when another process holds the wallet file's
    /// lock - see [`Self::set_busy_handler`].
    busy_handler: BusyHandler,
}

/// One output this wallet received, as its wallet file records it.
#[derive(Clone)]
pub struct OwnedOutput {
    pub txid: String,
    pub height: u64,
    pub timestamp: Option<u64>,
    pub spent: bool,
    pub frozen: bool,
    pub output: WalletOutput,
    pub key_image: [u8; 32],
}

impl OwnedOutput {
    pub fn amount(&self) -> u64 {
        self.output.commitment().amount
    }

    pub fn global_index(&self) -> u64 {
        self.output.index_on_blockchain()
    }

    /// `(account, address index)` this output was received on.
    pub fn subaddress(&self) -> (u32, u32) {
        self.output
            .subaddress()
            .map_or((0, 0), |index| (index.account(), index.address()))
    }

    /// Old enough to spend ([`SPENDABLE_AGE`] blocks) at chain height `tip`.
    pub fn unlocked(&self, tip: u64) -> bool {
        tip.saturating_sub(self.height) >= SPENDABLE_AGE
    }

    /// The payment ID the sender attached, if a real one (a plain address
    /// gets an all-zero dummy, which reads as none).
    pub fn payment_id(&self) -> Option<[u8; 8]> {
        match self.output.payment_id() {
            Some(PaymentId::Encrypted(id)) if id != [0; 8] => Some(id),
            _ => None,
        }
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(self.output.key().compress().to_bytes())
    }

    /// `(txid, index in transaction)` - what identifies it in the file.
    fn id(&self) -> (String, u64) {
        (self.txid.clone(), self.output.index_in_transaction())
    }
}

impl WalletKeys {
    /// Derives keys from a wallet file's hex-encoded private keys,
    /// asserting the derived address matches the recorded one.
    pub(crate) fn from_data(data: &WalletData, path: PathBuf) -> WalletKeys {
        let spend_key = scalar_from_hex(&data.private_spend_key);
        let view_key = scalar_from_hex(&data.private_view_key);
        let spend_key_dalek: Zeroizing<curve25519_dalek::Scalar> =
            Zeroizing::new((*spend_key).into());
        let public_spend =
            Point::from(&*spend_key_dalek * curve25519_dalek::constants::ED25519_BASEPOINT_TABLE);
        let view_pair = ViewPair::new(public_spend, view_key.clone())
            .expect("torsioned spend key in a wallet file");
        let address = view_pair.legacy_address(Network::Stagenet);
        assert_eq!(
            address.to_string(),
            data.address,
            "derived address doesn't match the expected address - private_spend_key/private_view_key don't match that address"
        );
        WalletKeys {
            view_pair,
            spend_key,
            view_key,
            address,
            path,
            busy_handler: default_busy_handler(),
        }
    }

    pub fn address(&self) -> String {
        self.address.to_string()
    }

    /// The wallet file this wallet lives in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the wallet file as it is now.
    pub fn load(&self) -> Result<WalletFile, WalletError> {
        WalletFile::load(&self.path)
    }

    /// The address of subaddress `index` of `account` - `(0, 0)` is the
    /// primary address.
    pub fn subaddress(&self, account: u32, index: u32) -> String {
        match SubaddressIndex::new(account, index) {
            Some(subaddress) => self
                .view_pair
                .subaddress(Network::Stagenet, subaddress)
                .to_string(),
            None => self.address(),
        }
    }

    pub fn integrated_address(&self, payment_id: [u8; 8]) -> String {
        self.view_pair
            .legacy_integrated_address(Network::Stagenet, payment_id)
            .to_string()
    }

    /// `(secret, public)` spend key, hex-encoded.
    pub fn spend_key_hex(&self) -> (Zeroizing<String>, String) {
        (
            Zeroizing::new(hex::encode(<[u8; 32]>::from(*self.spend_key))),
            hex::encode(self.view_pair.spend().compress().to_bytes()),
        )
    }

    /// `(secret, public)` view key, hex-encoded.
    pub fn view_key_hex(&self) -> (Zeroizing<String>, String) {
        (
            Zeroizing::new(hex::encode(<[u8; 32]>::from(*self.view_key))),
            hex::encode(self.view_pair.view().compress().to_bytes()),
        )
    }

    /// Whether `output` is this wallet's: an output's key is always the
    /// wallet's spend key plus `key_offset`.
    pub(crate) fn owns(&self, output: &WalletOutput) -> bool {
        let offset = output.key_offset().into();
        output.key().into() - &offset * curve25519_dalek::constants::ED25519_BASEPOINT_TABLE
            == self.view_pair.spend().into()
    }

    /// The key image `output` is spent under - what the chain records when
    /// it's spent, so it identifies an output in `freeze`, `sweep_single`
    /// and the rest the same way the reference wallet does.
    pub fn key_image(&self, output: &WalletOutput) -> [u8; 32] {
        let secret = (*self.spend_key).into() + output.key_offset().into();
        (secret * Point::biased_hash(output.key().compress().to_bytes()).into())
            .compress()
            .to_bytes()
    }

    /// Every output in `data`, in file order.
    pub fn outputs(&self, data: &WalletData) -> Result<Vec<OwnedOutput>, WalletError> {
        data.outputs
            .iter()
            .map(|record| {
                let output = decode_output(&record.txid, &record.serialized_output_hex)?;
                let key_image = self.key_image(&output);
                Ok(OwnedOutput {
                    txid: record.txid.clone(),
                    height: record.height,
                    timestamp: record.timestamp,
                    spent: record.spent,
                    frozen: record.frozen,
                    output,
                    key_image,
                })
            })
            .collect()
    }

    /// Sets what happens when another process holds this wallet file's
    /// lock: by default a warning and a wait ([`default_busy_handler`]); an
    /// interactive caller can ask the user instead.
    pub fn set_busy_handler(&mut self, handler: BusyHandler) {
        self.busy_handler = handler;
    }

    /// Takes this wallet file's lock (see [`WalletFile::lock_with`]).
    pub async fn lock(&self) -> Result<WalletFileLock, WalletError> {
        WalletFile::lock_with(&self.path, &self.busy_handler).await
    }

    /// Changes the wallet file under its lock (see [`WalletFile::update`]).
    pub async fn update<T>(
        &self,
        change: impl FnOnce(&mut WalletData) -> Result<T, WalletError>,
    ) -> Result<T, WalletError> {
        WalletFile::update_with(&self.path, &self.busy_handler, change).await
    }

    /// Changes this wallet's [`WalletMeta`] under the file lock, returning
    /// whatever `change` does.
    pub async fn update_meta<T>(
        &self,
        change: impl FnOnce(&mut WalletMeta) -> Result<T, String>,
    ) -> Result<T, WalletError> {
        self.update(|data| change(&mut data.meta).map_err(WalletError::Invalid))
            .await
    }

    /// `freeze`/`thaw <key_image>`. Returns `false` if no output of this
    /// wallet's has that key image.
    pub async fn set_frozen(&self, key_image: [u8; 32], frozen: bool) -> Result<bool, WalletError> {
        self.update_output(
            |o| o.key_image == key_image,
            |record| record.frozen = frozen,
        )
        .await
    }

    /// `mark_output_spent`/`mark_output_unspent <amount>/<offset>`, where
    /// `offset` is the output's global index. Returns `false` if no output
    /// of this wallet's is at that index.
    pub async fn set_spent(&self, global_index: u64, spent: bool) -> Result<bool, WalletError> {
        self.update_output(
            |o| o.global_index() == global_index,
            |record| record.spent = spent,
        )
        .await
    }

    async fn update_output(
        &self,
        find: impl Fn(&OwnedOutput) -> bool,
        change: impl FnMut(&mut OutputRecord),
    ) -> Result<bool, WalletError> {
        self.update(|data| {
            let Some(id) = self
                .outputs(data)?
                .into_iter()
                .find(|o| find(o))
                .map(|o| o.id())
            else {
                return Ok(false);
            };
            Ok(update_records(data, &[id], change) > 0)
        })
        .await
    }

    /// `set_tx_note`: an empty note clears it, as in the reference wallet.
    pub async fn set_note(&self, txid: &str, note: &str) -> Result<(), WalletError> {
        self.update(|data| {
            if note.is_empty() {
                data.tx_notes.remove(txid);
            } else {
                data.tx_notes.insert(txid.to_string(), note.to_string());
            }
            Ok(())
        })
        .await
    }

    /// A scanner that finds outputs to every subaddress `meta` has created,
    /// not just the primary address.
    fn scanner(&self, meta: &WalletMeta) -> Scanner {
        let mut scanner = Scanner::new(self.view_pair.clone());
        for (account, index) in meta.subaddress_indexes() {
            if let Some(subaddress) = SubaddressIndex::new(account, index) {
                scanner.register_subaddress(subaddress);
            }
        }
        scanner
    }
}

/// Which output of its transaction a record is.
fn record_index(record: &OutputRecord) -> Option<u64> {
    decode_output(&record.txid, &record.serialized_output_hex)
        .ok()
        .map(|output| output.index_in_transaction())
}

/// Applies `change` to exactly the given outputs (`(txid, index in
/// transaction)`), leaving any other outputs of the same transactions
/// alone. Returns how many matched.
fn update_records(
    data: &mut WalletData,
    ids: &[(String, u64)],
    mut change: impl FnMut(&mut OutputRecord),
) -> usize {
    let mut matched = 0;
    for record in data.outputs.iter_mut() {
        if ids
            .iter()
            .any(|(txid, index)| *txid == record.txid && record_index(record) == Some(*index))
        {
            change(record);
            matched += 1;
        }
    }
    matched
}

/// Records every output of confirmed transaction `txid` that pays this
/// wallet, one record each, skipping any already recorded (so recording
/// the same transaction twice changes nothing), takes `txid` off the
/// pending list, and dates this wallet's [`SentRecord`] for it, if any.
/// Returns how many outputs were newly recorded.
pub(crate) fn record_resolved(
    data: &mut WalletData,
    txid: &str,
    height: u64,
    timestamp: Option<u64>,
    outputs: &[WalletOutput],
) -> usize {
    let mut recorded = 0;
    for output in outputs {
        let index = output.index_in_transaction();
        if data
            .outputs
            .iter()
            .any(|record| record.txid == txid && record_index(record) == Some(index))
        {
            continue;
        }
        data.outputs.push(OutputRecord {
            txid: txid.to_string(),
            height,
            timestamp,
            serialized_output_hex: hex::encode(output.serialize()),
            amount_piconero: output.commitment().amount,
            spent: false,
            frozen: false,
        });
        recorded += 1;
    }
    data.pending.retain(|pending| pending.txid != txid);
    for sent in data.sent.iter_mut().filter(|sent| sent.txid == txid) {
        sent.height = Some(height);
        sent.timestamp = timestamp;
    }
    recorded
}

pub struct Wallet {
    pub(crate) keys: WalletKeys,
    pub(crate) rpc: MoneroDaemon<ReqwestTransport>,
    pub(crate) decoy_cache: DecoyCache,
    pub(crate) http_client: reqwest::Client,
    pub(crate) node_url: String,
}

impl std::ops::Deref for Wallet {
    type Target = WalletKeys;

    fn deref(&self) -> &WalletKeys {
        &self.keys
    }
}

/// What a transfer spends and where it sends it - see
/// [`Wallet::prepare_transfer`].
#[derive(Debug, Clone)]
pub struct TransferRequest {
    /// The account to spend from; change goes back to its address 0.
    pub account: u32,
    /// Only spend outputs received on these address indexes of `account`
    /// (`index=`). `None` means any of them.
    pub subaddress_indexes: Option<Vec<u32>>,
    pub priority: FeePriority,
    pub kind: TransferKind,
}

#[derive(Debug, Clone)]
pub enum TransferKind {
    /// Pay each destination exactly, spending as few outputs as needed.
    Pay {
        destinations: Vec<(String, u64)>,
        /// Destinations (indexes into `destinations`) that share the fee
        /// between them instead of the sender paying it on top.
        subtract_fee_from: Vec<usize>,
        /// Instead of one change output, split whatever's left over into
        /// this many self-addressed outputs.
        split_change_into: Option<usize>,
    },
    /// Spend every selected output and send all of it, less the fee, to
    /// `address`, split over `outputs` outputs.
    Sweep {
        address: String,
        outputs: usize,
        select: SweepSelect,
    },
    /// `pocketchange`: spend the `inputs` largest spendable outputs and pay
    /// all of it, less the fee, back to the account as `pieces` equal
    /// outputs - more independently spendable outputs, so the e2e suites
    /// never wait on one output's change to mature. The change output is
    /// one of the pieces, so up to [`MAX_OUTPUTS`] pieces fit in one
    /// transaction.
    Pocketchange { pieces: usize, inputs: usize },
}

/// The most outputs one Monero transaction can have (change included) -
/// Bulletproofs+ prove at most this many amounts at once.
pub const MAX_OUTPUTS: usize = 16;

#[derive(Debug, Clone, Copy)]
pub enum SweepSelect {
    /// `sweep_all`/`sweep_account`.
    All,
    /// `sweep_single <key_image>`.
    KeyImage([u8; 32]),
    /// `sweep_below <amount_threshold>`: outputs smaller than this.
    Below(u64),
}

impl TransferRequest {
    /// A payment from account 0, at the priority this crate has always
    /// used for the e2e suites.
    pub fn pay(destinations: Vec<(String, u64)>, split_change_into: Option<usize>) -> Self {
        TransferRequest {
            account: 0,
            subaddress_indexes: None,
            priority: FeePriority::Unimportant,
            kind: TransferKind::Pay {
                destinations,
                subtract_fee_from: vec![],
                split_change_into,
            },
        }
    }
}

/// A built, not yet signed, transaction - what the reference wallet shows
/// before asking "Is this okay?". Holds the wallet file's lock, so nothing
/// else can spend the same outputs until it's committed or dropped.
pub struct PreparedTransfer {
    _lock: WalletFileLock,
    signable: SignableTransaction,
    spent: Vec<(String, u64)>,
    account: u32,
    pub destinations: Vec<SentDestination>,
    pub fee: u64,
    pub change: u64,
    pub inputs: usize,
    pub inputs_total: u64,
}

/// A signed transaction: broadcast and recorded, or (not relayed) just its
/// bytes.
pub struct CommittedTransfer {
    pub hash: [u8; 32],
    /// The serialized transaction, hex-encoded, when it wasn't relayed.
    pub unrelayed_hex: Option<String>,
}

/// `/get_version`'s answer, for `status`.
pub struct DaemonVersion {
    pub major: u32,
    pub minor: u32,
}

impl Wallet {
    pub fn node_url(&self) -> &str {
        &self.node_url
    }

    /// See [`WalletKeys::set_busy_handler`].
    pub fn set_busy_handler(&mut self, handler: BusyHandler) {
        self.keys.set_busy_handler(handler);
    }

    /// The chain height, as the output-age checks use it.
    pub async fn tip(&self) -> Result<u64, WalletError> {
        Ok(self
            .rpc
            .latest_block_number()
            .await
            .map_err(|e| WalletError::Rpc(e.to_string()))? as u64)
    }

    /// The fee rate, piconero per unit of weight, at `priority`.
    pub async fn fee_per_weight(&self, priority: FeePriority) -> Result<u64, WalletError> {
        Ok(self
            .rpc
            .fee_rate(priority, MAX_FEE_PER_WEIGHT)
            .await
            .map_err(|e| WalletError::Rpc(e.to_string()))?
            .per_weight())
    }

    pub async fn daemon_version(&self) -> Result<DaemonVersion, WalletError> {
        let response = self
            .post_json(
                "json_rpc",
                serde_json::json!({ "jsonrpc": "2.0", "id": "0", "method": "get_version" }),
            )
            .await?;
        let version = response["result"]["version"].as_u64().ok_or_else(|| {
            WalletError::Rpc(format!("get_version returned no version: {response}"))
        })?;
        Ok(DaemonVersion {
            major: (version >> 16) as u32,
            minor: (version & 0xffff) as u32,
        })
    }

    async fn post_json(&self, route: &str, body: Value) -> Result<Value, WalletError> {
        self.http_client
            .post(format!("{}/{route}", self.node_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| WalletError::Rpc(format!("{route} request failed: {e}")))?
            .json()
            .await
            .map_err(|e| WalletError::Rpc(format!("{route} returned invalid JSON: {e}")))
    }

    /// Resolves every pending txid it can (locates its block over a plain
    /// `/get_transactions` call, scans *that one block* - deliberately not
    /// the whole chain - for this wallet's outputs, and records them). A
    /// txid not yet confirmed is left pending for a later run - not an
    /// error. Changes `data` only; the caller saves. Returns how many
    /// transactions resolved.
    async fn resolve_pending(&self, data: &mut WalletData) -> Result<usize, WalletError> {
        let mut resolved = 0;
        for txid in data
            .pending
            .iter()
            .map(|p| p.txid.clone())
            .collect::<Vec<_>>()
        {
            let Some((height, timestamp, outputs)) =
                self.scan_transaction(&data.meta, &txid).await?
            else {
                continue;
            };
            // A split, or a send whose change was split, pays this wallet
            // several outputs in one transaction: each gets its own record.
            record_resolved(data, &txid, height, Some(timestamp), &outputs);
            resolved += 1;
        }
        Ok(resolved)
    }

    /// Takes the file lock, loads the file, and resolves what's pending -
    /// the start of every operation that needs the wallet's current
    /// outputs. Saves if anything resolved.
    async fn lock_and_resolve(&self) -> Result<(WalletFileLock, WalletFile, usize), WalletError> {
        let lock = self.lock().await?;
        let mut file = self.load()?;
        let resolved = self.resolve_pending(&mut file.data).await?;
        if resolved > 0 {
            file.save()?;
        }
        Ok((lock, file, resolved))
    }

    /// `refresh`: resolves what's pending and returns the wallet file as it
    /// then is, plus how many transactions resolved.
    pub async fn refresh(&self) -> Result<(WalletData, usize), WalletError> {
        let (_lock, file, resolved) = self.lock_and_resolve().await?;
        Ok((file.data, resolved))
    }

    /// Locates `txid`'s block and returns its height, timestamp and every
    /// output in it that pays this wallet, in output order - `None` while
    /// it's still unconfirmed (or pays this wallet nothing, which is
    /// logged).
    async fn scan_transaction(
        &self,
        meta: &WalletMeta,
        txid: &str,
    ) -> Result<Option<(u64, u64, Vec<WalletOutput>)>, WalletError> {
        let Some(height) = locate_height(&self.http_client, &self.node_url, txid).await? else {
            return Ok(None);
        };
        let block = self
            .rpc
            .block_by_number(height as usize)
            .await
            .map_err(|e| WalletError::Rpc(e.to_string()))?;
        let timestamp = block.header.timestamp;
        let scannable = self
            .rpc
            .expand_to_scannable_block(block)
            .await
            .map_err(|e| WalletError::Rpc(e.to_string()))?;
        let found = self
            .scanner(meta)
            .scan(scannable)
            .map_err(|e| WalletError::Rpc(e.to_string()))?
            .not_additionally_locked();
        let mut outputs: Vec<WalletOutput> = found
            .into_iter()
            .filter(|o| hex::encode(o.transaction()) == txid)
            .collect();
        if outputs.is_empty() {
            // A wrong txid is a data problem, not a reason to crash the
            // whole run.
            eprintln!("cli-wallet: txid {txid} confirmed at height {height} but no output of it pays this wallet - leaving it pending");
            return Ok(None);
        }
        outputs.sort_by_key(|o| o.index_in_transaction());
        Ok(Some((height, timestamp, outputs)))
    }

    /// `rescan_spent`: asks the node which of this wallet's outputs' key
    /// images are spent (one `/is_key_image_spent` call - no scanning) and
    /// corrects the file wherever it disagrees. Returns each output whose
    /// flag changed, with its new value.
    pub async fn rescan_spent(&self) -> Result<Vec<(OwnedOutput, bool)>, WalletError> {
        let (_lock, mut file, _) = self.lock_and_resolve().await?;
        let outputs = self.outputs(&file.data)?;
        if outputs.is_empty() {
            return Ok(Vec::new());
        }
        let key_images: Vec<String> = outputs.iter().map(|o| hex::encode(o.key_image)).collect();
        let response = self
            .post_json(
                "is_key_image_spent",
                serde_json::json!({ "key_images": key_images }),
            )
            .await?;
        let statuses = response["spent_status"]
            .as_array()
            .filter(|statuses| statuses.len() == outputs.len())
            .ok_or_else(|| {
                WalletError::Rpc(format!(
                    "is_key_image_spent returned an unexpected answer: {response}"
                ))
            })?;
        let mut changed = Vec::new();
        for (output, status) in outputs.into_iter().zip(statuses) {
            // 0: unspent, 1: spent on chain, 2: spent in the pool.
            let spent = status.as_u64() != Some(0);
            if spent != output.spent {
                update_records(&mut file.data, &[output.id()], |record| {
                    record.spent = spent
                });
                changed.push((output, spent));
            }
        }
        if !changed.is_empty() {
            file.save()?;
        }
        Ok(changed)
    }

    /// Resolves anything pending, then builds (but doesn't sign) the
    /// transaction `request` describes from the wallet's own spendable
    /// outputs. Nothing is written until [`Self::commit`]; the wallet file
    /// stays locked until then.
    pub async fn prepare_transfer(
        &self,
        request: &TransferRequest,
    ) -> Result<PreparedTransfer, WalletError> {
        let (lock, file, _) = self.lock_and_resolve().await?;

        let latest_height = self.tip().await?;
        let mut candidates: Vec<OwnedOutput> = self
            .outputs(&file.data)?
            .into_iter()
            .filter(|o| !o.spent && !o.frozen && o.unlocked(latest_height))
            .filter(|o| o.subaddress().0 == request.account)
            .filter(|o| {
                request
                    .subaddress_indexes
                    .as_ref()
                    .is_none_or(|indexes| indexes.contains(&o.subaddress().1))
            })
            .collect();
        // Largest-first: most payments are covered by a single existing
        // output, so trying the biggest first keeps the common case to one
        // `OutputWithDecoys::new` call.
        candidates.sort_unstable_by_key(|o| std::cmp::Reverse(o.amount()));

        // One block of lag margin for decoy selection, not the tip itself -
        // a pooled public endpoint's backends can disagree by one block.
        let decoy_block_number = (latest_height.saturating_sub(1)) as usize;
        let fee_rate = self
            .rpc
            .fee_rate(request.priority, MAX_FEE_PER_WEIGHT)
            .await
            .map_err(|e| WalletError::Rpc(e.to_string()))?;
        let change = Change::new(
            self.view_pair.clone(),
            SubaddressIndex::new(request.account, 0),
        );
        let build = |inputs: &[OutputWithDecoys], payments: Vec<(MoneroAddress, u64)>| {
            let mut outgoing_view_key = Zeroizing::new([0u8; 32]);
            use rand_core::RngCore;
            OsRng.fill_bytes(outgoing_view_key.as_mut());
            let signable = SignableTransaction::new(
                RctType::ClsagBulletproofPlus,
                outgoing_view_key,
                inputs.to_vec(),
                payments.clone(),
                change.clone(),
                vec![],
                fee_rate,
            )?;
            Ok(Built { signable, payments })
        };

        let mut inputs = Vec::new();
        let mut spent = Vec::new();
        let built = match &request.kind {
            TransferKind::Sweep {
                address,
                outputs,
                select,
            } => {
                let address = parse_address(address)?;
                let selected: Vec<OwnedOutput> = match select {
                    SweepSelect::All => candidates,
                    SweepSelect::KeyImage(key_image) => candidates
                        .into_iter()
                        .filter(|o| o.key_image == *key_image)
                        .collect(),
                    SweepSelect::Below(threshold) => candidates
                        .into_iter()
                        .filter(|o| o.amount() < *threshold)
                        .collect(),
                };
                if selected.is_empty() {
                    return Err(WalletError::Invalid(
                        "No unlocked outputs to sweep".to_string(),
                    ));
                }
                for owned in selected {
                    spent.push(owned.id());
                    inputs.push(self.with_decoys(decoy_block_number, owned.output).await?);
                }
                let total_in: u64 = inputs.iter().map(|i| i.commitment().amount).sum();
                let pieces = (*outputs).max(1) as u64;
                settle_fee(|fee| {
                    let amount = total_in
                        .checked_sub(fee)
                        .filter(|amount| *amount >= pieces)?;
                    let mut payments = vec![(address, amount / pieces); pieces as usize];
                    payments[0].1 += amount % pieces;
                    Some(build(&inputs, payments))
                })?
            }
            TransferKind::Pocketchange {
                pieces,
                inputs: input_count,
            } => {
                if !(2..=MAX_OUTPUTS).contains(pieces) {
                    return Err(WalletError::Invalid(format!(
                        "pocketchange splits into 2 to {MAX_OUTPUTS} pieces, not {pieces}"
                    )));
                }
                if *input_count == 0 {
                    return Err(WalletError::Invalid(
                        "pocketchange needs at least 1 input".to_string(),
                    ));
                }
                if candidates.is_empty() {
                    return Err(WalletError::Invalid(
                        "No unlocked outputs to split".to_string(),
                    ));
                }
                for owned in candidates.into_iter().take(*input_count) {
                    spent.push(owned.id());
                    inputs.push(self.with_decoys(decoy_block_number, owned.output).await?);
                }
                let own_address = parse_address(&self.subaddress(request.account, 0))?;
                let total_in: u64 = inputs.iter().map(|i| i.commitment().amount).sum();
                let pieces = *pieces as u64;
                // `pieces - 1` payments to the account's own address; the
                // change output, also the account's, is the last piece (plus
                // the division's remainder).
                settle_fee(|fee| {
                    let piece = total_in.checked_sub(fee)? / pieces;
                    if piece == 0 {
                        return None;
                    }
                    Some(build(
                        &inputs,
                        vec![(own_address, piece); pieces as usize - 1],
                    ))
                })?
            }
            TransferKind::Pay {
                destinations,
                subtract_fee_from,
                split_change_into,
            } => {
                let destinations: Vec<(MoneroAddress, u64)> = destinations
                    .iter()
                    .map(|(to, amount)| Ok((parse_address(to)?, *amount)))
                    .collect::<Result<_, WalletError>>()?;
                if let Some(bad) = subtract_fee_from.iter().find(|&&i| i >= destinations.len()) {
                    return Err(WalletError::Invalid(format!(
                        "subtractfeefrom index {bad} is out of range"
                    )));
                }
                let amount: u64 = destinations.iter().map(|(_, amount)| amount).sum();
                let mut last_necessary_fee: Option<u64> = None;
                let mut remaining = candidates.into_iter();
                loop {
                    let Some(owned) = remaining.next() else {
                        return Err(WalletError::InsufficientFunds {
                            needed: amount
                                + if subtract_fee_from.is_empty() {
                                    last_necessary_fee.unwrap_or(0)
                                } else {
                                    0
                                },
                            available: inputs
                                .iter()
                                .map(|i: &OutputWithDecoys| i.commitment().amount)
                                .sum(),
                            address: self.address(),
                        });
                    };
                    // (txid, output index): one transaction can pay this
                    // wallet several outputs, and only this one is spent.
                    spent.push(owned.id());
                    inputs.push(self.with_decoys(decoy_block_number, owned.output).await?);
                    let total_in: u64 = inputs.iter().map(|i| i.commitment().amount).sum();

                    if !subtract_fee_from.is_empty() {
                        if total_in < amount {
                            continue;
                        }
                        // Enough to cover the destinations: the fee comes
                        // out of them, so another input changes nothing.
                        break settle_fee(|fee| {
                            let share = fee.div_ceil(subtract_fee_from.len() as u64);
                            let mut payments = destinations.clone();
                            for &i in subtract_fee_from {
                                payments[i].1 = payments[i]
                                    .1
                                    .checked_sub(share)
                                    .filter(|amount| *amount > 0)?;
                            }
                            Some(build(&inputs, payments))
                        })?;
                    }

                    // Recomputed every iteration against `inputs` as it
                    // grows - once big enough to also cover the split
                    // pieces' own share of the fee, this succeeds.
                    let mut payments = destinations.clone();
                    if let Some(n) = split_change_into.filter(|&n| n >= 2) {
                        if let Some(leftover) = total_in.checked_sub(amount) {
                            let piece = leftover / n as u64;
                            if piece > 0 {
                                payments.extend(std::iter::repeat_n((self.address, piece), n - 1));
                            }
                        }
                    }
                    match build(&inputs, payments) {
                        Ok(built) => break built,
                        Err(SendError::NotEnoughFunds { necessary_fee, .. }) => {
                            last_necessary_fee = necessary_fee;
                            continue;
                        }
                        Err(SendError::NoInputs) => continue,
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        };

        let inputs_total: u64 = inputs.iter().map(|i| i.commitment().amount).sum();
        let fee = built.signable.necessary_fee();
        let destinations: Vec<SentDestination> = built
            .payments
            .iter()
            .map(|(address, amount)| SentDestination {
                address: address.to_string(),
                amount_piconero: *amount,
            })
            .collect();
        let change =
            inputs_total - destinations.iter().map(|d| d.amount_piconero).sum::<u64>() - fee;
        Ok(PreparedTransfer {
            _lock: lock,
            signable: built.signable,
            spent,
            account: request.account,
            destinations,
            fee,
            change,
            inputs: inputs.len(),
            inputs_total,
        })
    }

    async fn with_decoys(
        &self,
        decoy_block_number: usize,
        output: WalletOutput,
    ) -> Result<OutputWithDecoys, WalletError> {
        OutputWithDecoys::new(
            &mut OsRng,
            &self.decoy_cache,
            RING_LEN,
            decoy_block_number,
            output,
        )
        .await
        .map_err(|e| WalletError::Rpc(e.to_string()))
    }

    /// Signs `prepared` and, if `relay`, broadcasts it, marks the outputs
    /// it spent, and records it (a [`SentRecord`], plus its change as
    /// pending). Without `relay` the file is left alone and the signed
    /// transaction's bytes are returned instead.
    pub async fn commit(
        &self,
        prepared: PreparedTransfer,
        relay: bool,
    ) -> Result<CommittedTransfer, WalletError> {
        let tx: Transaction = prepared.signable.sign(&mut OsRng, &self.spend_key)?;
        let hash = tx.hash();
        if !relay {
            return Ok(CommittedTransfer {
                hash,
                unrelayed_hex: Some(hex::encode(tx.serialize())),
            });
        }
        self.rpc
            .publish_transaction(&tx)
            .await
            .map_err(WalletError::Broadcast)?;

        // Only now, after a successful broadcast, change the file - a
        // failure anywhere above must leave it exactly as it was, so a
        // caller's own retry sees the same spendable set again. The lock
        // `prepared` holds is still held, so a fresh load is current.
        let txid = hex::encode(hash);
        let mut file = self.load()?;
        update_records(&mut file.data, &prepared.spent, |record| {
            record.spent = true
        });
        file.data.sent.push(SentRecord {
            txid: txid.clone(),
            account: prepared.account,
            destinations: prepared.destinations,
            fee_piconero: prepared.fee,
            change_piconero: prepared.change,
            height: None,
            timestamp: None,
        });
        file.data.add_pending(&txid, prepared.change);
        file.save()?;
        Ok(CommittedTransfer {
            hash,
            unrelayed_hex: None,
        })
    }

    async fn transfer(&self, request: TransferRequest) -> Result<[u8; 32], WalletError> {
        let prepared = self.prepare_transfer(&request).await?;
        Ok(self.commit(prepared, true).await?.hash)
    }

    /// Resolves anything pending, builds/signs/broadcasts a transaction
    /// sending `amount` piconero to `to` from the wallet's own spendable
    /// outputs, and records it. Returns the new transaction's hash.
    pub async fn send(&self, to: &str, amount: u64) -> Result<[u8; 32], WalletError> {
        self.transfer(TransferRequest::pay(vec![(to.to_string(), amount)], None))
            .await
    }

    /// Same as [`Self::send`], but splits whatever's left over after
    /// `amount` + fee into `split_change_into` explicit self-addressed
    /// outputs instead of one opaque `Change` output - so an ordinary
    /// payment also grows the pool of independently-aged spendable outputs
    /// a later send can draw on, at no extra RPC cost (same tx, more
    /// outputs). `split_change_into < 2` behaves exactly like `send`.
    pub async fn send_with_change_split(
        &self,
        to: &str,
        amount: u64,
        split_change_into: usize,
    ) -> Result<[u8; 32], WalletError> {
        self.transfer(TransferRequest::pay(
            vec![(to.to_string(), amount)],
            Some(split_change_into),
        ))
        .await
    }

    /// Splits account 0's `inputs` largest spendable outputs into `pieces`
    /// equal outputs of its own (see [`TransferKind::Pocketchange`]). Each
    /// new piece needs its own `SPENDABLE_AGE` confirmations before it's
    /// usable, same as any other change output.
    pub async fn pocketchange(
        &self,
        pieces: usize,
        inputs: usize,
    ) -> Result<[u8; 32], WalletError> {
        self.transfer(TransferRequest {
            kind: TransferKind::Pocketchange { pieces, inputs },
            ..TransferRequest::pay(vec![], None)
        })
        .await
    }

    /// Records a transaction this wallet didn't sign itself (a faucet
    /// payout, funds sent in from elsewhere) and resolves it immediately if
    /// it's already confirmed. Adding one that's already recorded picks up
    /// any of its outputs the file is missing.
    pub async fn add_output(&self, txid: &str) -> Result<(), WalletError> {
        let _lock = self.lock().await?;
        let mut file = self.load()?;
        let known = file.data.outputs.iter().any(|record| record.txid == txid);
        if known {
            if let Some((height, timestamp, outputs)) =
                self.scan_transaction(&file.data.meta, txid).await?
            {
                let added =
                    record_resolved(&mut file.data, txid, height, Some(timestamp), &outputs);
                if added > 0 {
                    eprintln!("cli-wallet: recovered {added} untracked output(s) of {txid}");
                }
            }
        } else {
            file.data.add_pending(txid, 0);
            self.resolve_pending(&mut file.data).await?;
        }
        file.save()
    }

    /// A cheap, real pre-flight check for callers that want to fail fast
    /// with a clear, actionable message before doing anything else (an
    /// expensive setup sequence, a whole connect-flow test) rather than
    /// discovering an empty wallet deep inside `send`'s own error. Resolves
    /// anything pending first, so this reports the wallet's real, current
    /// state. Frozen outputs don't count.
    pub async fn balance(&self) -> Result<WalletBalance, WalletError> {
        let (data, _) = self.refresh().await?;
        let latest_height = self.tip().await?;
        let mut balance = WalletBalance::default();
        for output in self
            .outputs(&data)?
            .into_iter()
            .filter(|o| !o.spent && !o.frozen)
        {
            if output.unlocked(latest_height) {
                balance.spendable_piconero += output.amount();
                balance.spendable_outputs += 1;
            } else {
                balance.pending_piconero += output.amount();
                balance.pending_outputs += 1;
            }
        }
        Ok(balance)
    }
}

/// See [`Wallet::balance`].
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct WalletBalance {
    pub spendable_piconero: u64,
    pub spendable_outputs: usize,
    pub pending_piconero: u64,
    pub pending_outputs: usize,
}

pub(crate) fn parse_address(address: &str) -> Result<MoneroAddress, WalletError> {
    MoneroAddress::from_str(Network::Stagenet, address)
        .map_err(|e| WalletError::Invalid(format!("failed to parse address {address}: {e}")))
}

/// A built transaction plus the payments it was built with -
/// `SignableTransaction` keeps its own private.
struct Built {
    signable: SignableTransaction,
    payments: Vec<(MoneroAddress, u64)>,
}

/// Builds with `build(fee)` for a growing fee guess until the transaction
/// pays at least what it needs - for transfers where the fee comes out of
/// what's sent (a sweep, `subtractfeefrom`) rather than out of change. A
/// transaction's weight doesn't depend on its amounts, so this settles on
/// the second build in practice. `build` returns `None` once the fee is
/// more than there is to send.
fn settle_fee(
    mut build: impl FnMut(u64) -> Option<Result<Built, SendError>>,
) -> Result<Built, WalletError> {
    let too_small =
        || WalletError::Invalid("the fee is more than the amount being sent".to_string());
    let mut fee = 0;
    for _ in 0..8 {
        match build(fee).ok_or_else(too_small)? {
            Ok(built) if built.signable.necessary_fee() <= fee => return Ok(built),
            Ok(built) => fee = built.signable.necessary_fee(),
            Err(SendError::NotEnoughFunds {
                necessary_fee: Some(needed),
                ..
            }) if needed > fee => fee = needed,
            Err(e) => return Err(e.into()),
        }
    }
    Err(WalletError::Invalid("the fee didn't settle".to_string()))
}
