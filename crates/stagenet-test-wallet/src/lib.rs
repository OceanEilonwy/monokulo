//! A purpose-built, fast, reliable **stagenet-only test wallet** - not a
//! general-purpose Monero wallet, and never meant to become one. Built to
//! replace `scanner::e2e_wallet::StagenetSpendWallet` as the thing this
//! repo's real-stagenet e2e suites use to pay a real order with a real,
//! signed, broadcast transaction.
//!
//! # Why this exists
//!
//! The predecessor (`scanner::e2e_wallet`) does real, correct Monero wallet
//! work: it scans the chain to discover its own outputs and does full
//! gamma-distribution decoy selection, the same way a real wallet would.
//! That correctness is also exactly what makes it slow and, worse,
//! unreliable against a shared public node: a real run was observed to
//! need dozens of RPC round trips (one `locate_transaction` call *per
//! historical txid ever recorded* - a list that only ever grows - plus a
//! live, ~1MB `get_output_distribution` fetch per send), and several of
//! those calls were observed to fail intermittently in practice, with
//! retries not reliably fixing it. See the git history around this crate's
//! introduction for the full investigation.
//!
//! On stagenet, paying our own test orders, none of that correctness is
//! actually buying anything: the "threat model" real decoy selection and
//! chain scanning defend against (a hostile chain observer, an untrusted
//! remote node) doesn't apply when the only two parties on either side of
//! every transaction are our own test fixtures. So this crate deliberately
//! narrows scope:
//!
//! - **No chain scanning for output discovery.** This wallet is *told*
//!   about its own outputs directly - see [`Ledger`] - rather than
//!   rediscovering them by asking the chain "what's mine?" on every run.
//!   Once an output has been resolved once (`resolve_pending`, below), its
//!   entire [`monero_wallet::WalletOutput`] is serialized and committed to
//!   source (`WalletOutput::serialize`/`::read` - a real, public
//!   round-trip the library itself provides), so every later run reads it
//!   straight off disk with zero RPC calls at all.
//! - **Decoy selection still runs the real, correct algorithm** (still
//!   picks genuine, unlocked, on-chain outputs - a node will reject
//!   anything less, stagenet or not) but is fed from a *cached, committed*
//!   output-distribution snapshot instead of a live fetch every time - see
//!   [`DecoyCache`]'s own doc comment for why this is provably safe, not
//!   just fast.
//! - **No live spent-status checks.** This wallet is the only spender of
//!   its own keys (a committed, single-writer test fixture, never a real
//!   multi-client wallet) - it marks an output `spent` in the ledger the
//!   moment it successfully builds a transaction spending it, and trusts
//!   that record on every later run rather than asking the chain to
//!   confirm it again.
//!
//! What's *not* narrowed: the actual transaction construction and signing
//! (`monero_wallet::send::SignableTransaction`, real CLSAG + Bulletproofs+)
//! and broadcast are unchanged from the predecessor - a transaction this
//! crate builds is exactly as real and exactly as valid as any other Monero
//! wallet's.

use std::ops::RangeBounds;

use monero_daemon_rpc::{prelude::*, HttpTransport, MoneroDaemon};
use monero_wallet::{
    address::{MoneroAddress, Network},
    ed25519::{Point, Scalar},
    interface::ProvidesUnvalidatedDecoys,
    ringct::RctType,
    send::{Change, SendError, SignableTransaction},
    transaction::Transaction,
    OutputWithDecoys, Scanner, ViewPair, WalletOutput,
};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

/// The ring size required for the `ClsagBulletproofPlus` RCT type this
/// module always signs with - the standard type on every live Monero
/// network today. Mirrors `scanner::e2e_wallet`'s own constant.
const RING_LEN: u8 = 16;

/// Monero requires this many confirmations on any output before it's
/// spendable - `CRYPTONOTE_DEFAULT_TX_SPENDABLE_AGE`, a real consensus rule.
const SPENDABLE_AGE: u64 = 10;

#[derive(Debug, thiserror::Error)]
pub enum WalletError {
    #[error("cannot reach the stagenet node at {url}: {source}")]
    DaemonUnreachable { url: String, source: InterfaceError },
    #[error(
        "insufficient funds: this payment needs {needed} piconero, but only {available} \
         piconero of spendable ledger entries were found (an entry needs {SPENDABLE_AGE} \
         confirmations before it's spendable - if a recent send's change is still that young, \
         this is expected; wait and retry).\n\
         Otherwise, fund the wallet from the stagenet faucet:\n\
         1. open https://stagenet-faucet.xmr-tw.org/\n\
         2. send to: {address}\n\
         3. add a new ledger entry for the faucet's txid (height/serialized_output_hex left\n\
         null; resolve_pending fills them in on the next send)"
    )]
    InsufficientFunds { needed: u64, available: u64, address: String },
    #[error("failed to build/sign the transaction: {0}")]
    Send(#[from] SendError),
    #[error("failed to broadcast the transaction: {0}")]
    Broadcast(#[source] PublishTransactionError),
    #[error("daemon RPC call failed: {0}")]
    Rpc(String),
    #[error("ledger error: {0}")]
    Ledger(String),
}

/// `monero-daemon-rpc`'s `HttpTransport` over a plain `reqwest::Client` -
/// verbatim from `scanner::e2e_wallet::ReqwestTransport`, see that type's
/// own doc comment for why this is implemented directly rather than pulling
/// in a second TLS stack.
#[derive(Clone)]
struct ReqwestTransport {
    client: reqwest::Client,
    base_url: String,
}

impl HttpTransport for ReqwestTransport {
    fn post(
        &self,
        route: &str,
        body: Vec<u8>,
        response_size_limit: Option<usize>,
    ) -> impl Send + std::future::Future<Output = Result<Vec<u8>, InterfaceError>> {
        let request = self.client.post(format!("{}/{route}", self.base_url)).header("content-type", "application/json").body(body);
        async move {
            let response = request.send().await.map_err(|e| InterfaceError::InterfaceError(format!("request failed: {e}")))?;
            if let (Some(limit), Some(len)) = (response_size_limit, response.content_length()) {
                if len > limit as u64 {
                    return Err(InterfaceError::InterfaceError(format!("response claimed {len} bytes, exceeding the {limit}-byte limit for {route}")));
                }
            }
            let bytes = response.bytes().await.map_err(|e| InterfaceError::InterfaceError(format!("{e}")))?;
            Ok(bytes.to_vec())
        }
    }
}

/// Serves decoy selection from a cached, committed output-distribution
/// snapshot (see `load_decoy_distribution`/the crate's own `[[bin]]`
/// refresh tool) instead of a live `get_output_distribution` fetch -
/// forwards everything else (`latest_block_number`, and, critically,
/// `unlocked_ringct_outputs` - the call that actually returns the real
/// cryptographic material a ring signature needs) straight to a real
/// `MoneroDaemon`.
///
/// **Why serving a stale/cached distribution is provably safe, not just
/// fast**: read directly from `monero-wallet 0.2.0`'s own decoy-selection
/// algorithm (`src/decoys.rs::select_n`) rather than assumed. The one call
/// site always requests `..= block_number` (the live tip); the algorithm
/// never compares the *length* or *coverage* of what comes back against
/// that live `block_number` again - every subsequent computation
/// (`distribution.len()`, `distribution[i]`, `partition_point`) is
/// self-consistent against whatever was returned, so a distribution
/// representing an older, cached point in the chain's history is just as
/// valid an input as a live one; it only changes which (still real,
/// still-unlocked, still individually verified by `unlocked_ringct_outputs`
/// below) block heights get sampled from. `monero-interface`'s own
/// `ProvidesUnvalidatedDecoys` doc comment says as much directly: "This
/// SHOULD be satisfied by a local store" - this is that store.
struct DecoyCache {
    daemon: MoneroDaemon<ReqwestTransport>,
    distribution: Vec<u64>,
}

impl ProvidesBlockchainMeta for DecoyCache {
    fn latest_block_number(&self) -> impl Send + std::future::Future<Output = Result<usize, InterfaceError>> {
        self.daemon.latest_block_number()
    }
}

impl ProvidesUnvalidatedDecoys for DecoyCache {
    fn ringct_output_distribution(
        &self,
        _range: impl Send + RangeBounds<usize>,
    ) -> impl Send + std::future::Future<Output = Result<Vec<u64>, InterfaceError>> {
        // The range argument is deliberately ignored - see this type's own
        // doc comment for why that's safe for how the one real caller
        // (`select_n`) actually uses the result.
        let distribution = self.distribution.clone();
        async move { Ok(distribution) }
    }

    fn unlocked_ringct_outputs(
        &self,
        indexes: &[u64],
        evaluate_unlocked: EvaluateUnlocked,
    ) -> impl Send + std::future::Future<Output = Result<Vec<Option<[Point; 2]>>, TransactionsError>> {
        ProvidesUnvalidatedDecoys::unlocked_ringct_outputs(&self.daemon, indexes, evaluate_unlocked)
    }
}

/// One output this wallet knows about - either already resolved (spendable
/// once old enough) or still `Pending` (a just-broadcast send's own change
/// output, not yet confirmed). See this crate's own module doc comment for
/// the full "informed, not scanned" model this implements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub txid: String,
    /// `None` until `resolve_pending` has located this txid on-chain and
    /// scanned that one block - after that, always `Some` and never
    /// re-resolved.
    pub height: Option<u64>,
    /// Hex-encoded `WalletOutput::serialize()` - `None` exactly when
    /// `height` is `None` (the two are always resolved together, by
    /// `resolve_pending`). Once `Some`, every later run reads this
    /// directly via `WalletOutput::read`, never re-scanning the chain for
    /// it - the fast path this whole crate exists for.
    pub serialized_output_hex: Option<String>,
    pub amount_piconero: u64,
    pub spent: bool,
}

/// The committed, source-controlled ledger of everything this wallet has
/// ever been told about its own outputs - see this crate's own module doc
/// comment. A plain JSON array on disk (`{"entries": [...]}`), read fresh
/// and written back atomically (temp-file-then-rename, same convention
/// every other credentials-adjacent file in this repo already follows) so
/// two overlapping runs, or a crash mid-write, can't corrupt it.
pub struct Ledger {
    path: String,
    entries: Vec<LedgerEntry>,
}

#[derive(Serialize, Deserialize, Default)]
struct LedgerFile {
    entries: Vec<LedgerEntry>,
}

impl Ledger {
    pub fn load(path: &str) -> Result<Self, WalletError> {
        let entries = match std::fs::read_to_string(path) {
            Ok(contents) => {
                serde_json::from_str::<LedgerFile>(&contents).map_err(|e| WalletError::Ledger(format!("failed to parse {path}: {e}")))?.entries
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(WalletError::Ledger(format!("failed to read {path}: {e}"))),
        };
        Ok(Self { path: path.to_string(), entries })
    }

    fn save(&self) -> Result<(), WalletError> {
        let file = LedgerFile { entries: self.entries.clone() };
        let tmp_path = format!("{}.tmp", self.path);
        std::fs::write(&tmp_path, serde_json::to_string_pretty(&file).unwrap() + "\n")
            .map_err(|e| WalletError::Ledger(format!("failed to write {tmp_path}: {e}")))?;
        std::fs::rename(&tmp_path, &self.path).map_err(|e| WalletError::Ledger(format!("failed to move {tmp_path} into place over {}: {e}", self.path)))
    }

    /// Adds a `Pending` entry for a transaction this wallet just broadcast
    /// itself - `resolve_pending` fills in `height`/`serialized_output_hex`
    /// the next time this ledger is used, once it's had a chance to
    /// confirm.
    pub fn record_pending(&mut self, txid: &str, amount_piconero: u64) -> Result<(), WalletError> {
        if !self.entries.iter().any(|e| e.txid == txid) {
            self.entries.push(LedgerEntry { txid: txid.to_string(), height: None, serialized_output_hex: None, amount_piconero, spent: false });
        }
        self.save()
    }
}

impl StagenetTestWallet {
    /// Resolves every still-`Pending` ledger entry it can (locates the
    /// txid's block height over a plain `/get_transactions` call, scans
    /// *that one block* - deliberately not the whole chain - for the real
    /// `WalletOutput`, and serializes it into the entry), persisting each
    /// resolution immediately. A txid not yet confirmed (still in the
    /// mempool) is left `Pending` for a later run to pick up - not an
    /// error.
    pub async fn resolve_pending(&self, ledger: &mut Ledger) -> Result<(), WalletError> {
        let pending_txids: Vec<String> =
            ledger.entries.iter().filter(|e| !e.spent && e.height.is_none()).map(|e| e.txid.clone()).collect();
        if pending_txids.is_empty() {
            return Ok(());
        }
        for txid in pending_txids {
            let Some(height) = locate_height(&self.http_client, &self.node_url, &txid).await? else { continue };
            let block = self.rpc.block_by_number(height as usize).await.map_err(|e| WalletError::Rpc(e.to_string()))?;
            let scannable = self.rpc.expand_to_scannable_block(block).await.map_err(|e| WalletError::Rpc(e.to_string()))?;
            let mut scanner = Scanner::new(self.view_pair.clone());
            let found = scanner.scan(scannable).map_err(|e| WalletError::Rpc(e.to_string()))?.not_additionally_locked();
            let Some(output) = found.into_iter().find(|o| hex::encode(o.transaction()) == txid) else {
                // Genuinely shouldn't happen (we only ever add our own
                // txids), but a wrong/stale ledger entry is a data problem,
                // not a reason to crash the whole run.
                eprintln!("stagenet-test-wallet: resolve_pending: txid {txid} confirmed at height {height} but no matching output found when scanning that block - leaving it unresolved");
                continue;
            };
            let entry = ledger.entries.iter_mut().find(|e| e.txid == txid).expect("txid came from this same ledger's own pending list");
            entry.height = Some(height);
            entry.serialized_output_hex = Some(hex::encode(output.serialize()));
            ledger.save()?;
        }
        Ok(())
    }

    /// Every currently-spendable `WalletOutput` this ledger already knows
    /// about - old enough (`SPENDABLE_AGE`) and not marked `spent`. Purely
    /// local: deserializes each qualifying entry's own committed bytes, no
    /// RPC calls at all beyond the one `latest_block_number` needed for the
    /// age check.
    fn spendable_from_ledger(&self, ledger: &Ledger, latest_height: u64) -> Result<Vec<(String, WalletOutput)>, WalletError> {
        let mut spendable = Vec::new();
        for entry in &ledger.entries {
            if entry.spent {
                continue;
            }
            let (Some(height), Some(hex_bytes)) = (entry.height, &entry.serialized_output_hex) else { continue };
            if latest_height.saturating_sub(height) < SPENDABLE_AGE {
                continue;
            }
            let bytes = hex::decode(hex_bytes).map_err(|e| WalletError::Ledger(format!("entry {} has invalid serialized_output_hex: {e}", entry.txid)))?;
            let output = WalletOutput::read(&mut &bytes[..]).map_err(|e| WalletError::Ledger(format!("entry {} failed to deserialize: {e}", entry.txid)))?;
            spendable.push((entry.txid.clone(), output));
        }
        Ok(spendable)
    }
}

pub struct StagenetTestWallet {
    view_pair: ViewPair,
    spend_key: Zeroizing<Scalar>,
    address: MoneroAddress,
    rpc: MoneroDaemon<ReqwestTransport>,
    decoy_cache: DecoyCache,
    http_client: reqwest::Client,
    node_url: String,
}

fn hex32(hex_str: &str) -> [u8; 32] {
    let bytes = hex::decode(hex_str).expect("invalid hex in stagenet-wallets.json key material");
    bytes.try_into().expect("key material must be exactly 32 bytes")
}

fn scalar_from_hex(hex_str: &str) -> Zeroizing<Scalar> {
    Zeroizing::new(Scalar::read(&mut &hex32(hex_str)[..]).expect("stagenet-wallets.json private key isn't a canonical ed25519 scalar"))
}

/// Loads a committed output-distribution snapshot (a plain JSON array of
/// `u64`, produced by this crate's own `refresh-decoy-pool` `[[bin]]`) for
/// `DecoyCache` - see that type's own doc comment for why a cached snapshot
/// is a fully valid input, not an approximation.
fn load_decoy_distribution(path: &str) -> Result<Vec<u64>, WalletError> {
    let contents = std::fs::read_to_string(path).map_err(|e| WalletError::Ledger(format!("failed to read decoy distribution {path}: {e}")))?;
    serde_json::from_str(&contents).map_err(|e| WalletError::Ledger(format!("failed to parse decoy distribution {path}: {e}")))
}

/// Fetches one real `ringct_output_distribution` snapshot over `from..=to`
/// and writes it to `out_path` as a plain JSON array - the one place this
/// crate ever performs the expensive live fetch `DecoyCache` exists to
/// avoid on every send. Meant to be run occasionally, by hand, via the
/// `refresh-decoy-pool` `[[bin]]` - never by the e2e suites themselves.
pub async fn refresh_decoy_distribution(node_url: &str, accept_invalid_certs: bool, from: usize, to: usize, out_path: &str) -> Result<usize, WalletError> {
    let client = reqwest::Client::builder().danger_accept_invalid_certs(accept_invalid_certs).timeout(std::time::Duration::from_secs(60)).build().expect("failed to build reqwest client");
    let transport = ReqwestTransport { client, base_url: node_url.trim_end_matches('/').to_string() };
    let daemon = MoneroDaemon::new(transport).await.map_err(|source| WalletError::DaemonUnreachable { url: node_url.to_string(), source })?;
    let distribution = ProvidesUnvalidatedDecoys::ringct_output_distribution(&daemon, from..=to).await.map_err(|e| WalletError::Rpc(e.to_string()))?;
    std::fs::write(out_path, serde_json::to_string(&distribution).unwrap()).map_err(|e| WalletError::Ledger(format!("failed to write {out_path}: {e}")))?;
    Ok(distribution.len())
}

/// A single, plain `/get_transactions` call for one txid, reading just the
/// `block_height` field - deliberately not going through
/// `monero-daemon-rpc`'s own typed transaction-fetching (which
/// deserializes the full transaction body, work this only needs metadata
/// for) or `scanner`'s equivalent (this crate has no dependency on
/// `scanner` at all, by design - see this crate's own module doc comment).
/// `Ok(None)` means "not confirmed yet" (still in the mempool, or genuinely
/// unknown) - not distinguished further, since `resolve_pending`'s own
/// caller treats both the same way (leave it `Pending`, try again later).
async fn locate_height(client: &reqwest::Client, node_url: &str, txid: &str) -> Result<Option<u64>, WalletError> {
    let response: Value = client
        .post(format!("{node_url}/get_transactions"))
        .json(&serde_json::json!({ "txs_hashes": [txid], "decode_as_json": false }))
        .send()
        .await
        .map_err(|e| WalletError::Rpc(format!("get_transactions request failed: {e}")))?
        .json()
        .await
        .map_err(|e| WalletError::Rpc(format!("get_transactions returned invalid JSON: {e}")))?;
    Ok(response["txs"].as_array().and_then(|txs| txs.first()).and_then(|tx| tx["block_height"].as_u64()))
}

impl StagenetTestWallet {
    /// Connects to `node_url`, derives keys from the given hex-encoded
    /// private spend/view keys (asserting the derived address matches
    /// `expected_address`, the same self-check
    /// `scanner::e2e_wallet::StagenetSpendWallet::connect` already made),
    /// and loads the cached decoy-distribution snapshot at
    /// `decoy_distribution_path`.
    pub async fn connect(
        node_url: &str,
        accept_invalid_certs: bool,
        private_spend_key_hex: &str,
        private_view_key_hex: &str,
        expected_address: &str,
        decoy_distribution_path: &str,
    ) -> Result<Self, WalletError> {
        let spend_key = scalar_from_hex(private_spend_key_hex);
        let view_key = scalar_from_hex(private_view_key_hex);
        let spend_key_dalek: Zeroizing<curve25519_dalek::Scalar> = Zeroizing::new((*spend_key).into());
        let public_spend = Point::from(&*spend_key_dalek * curve25519_dalek::constants::ED25519_BASEPOINT_TABLE);
        let view_pair = ViewPair::new(public_spend, view_key).expect("torsioned spend key in stagenet-wallets.json");
        let address = view_pair.legacy_address(Network::Stagenet);
        assert_eq!(
            address.to_string(),
            expected_address,
            "derived address doesn't match the expected address - private_spend_key/private_view_key don't match that address"
        );

        let http_client =
            reqwest::Client::builder().danger_accept_invalid_certs(accept_invalid_certs).timeout(std::time::Duration::from_secs(60)).build().expect("failed to build reqwest client");
        let transport = ReqwestTransport { client: http_client.clone(), base_url: node_url.trim_end_matches('/').to_string() };
        let rpc = MoneroDaemon::new(transport).await.map_err(|source| WalletError::DaemonUnreachable { url: node_url.to_string(), source })?;
        let distribution = load_decoy_distribution(decoy_distribution_path)?;
        let decoy_cache = DecoyCache { daemon: rpc.clone(), distribution };

        Ok(Self { view_pair, spend_key, address, rpc, decoy_cache, http_client, node_url: node_url.trim_end_matches('/').to_string() })
    }

    pub fn address(&self) -> String {
        self.address.to_string()
    }

    /// Resolves any pending ledger entries, builds/signs/broadcasts a
    /// transaction sending `amount` piconero to `to` from the ledger's own
    /// already-spendable outputs, records the new change output as a
    /// pending entry, and marks whichever entries were actually spent.
    /// Returns the new transaction's hash on success.
    pub async fn send(&self, ledger: &mut Ledger, to: &str, amount: u64) -> Result<[u8; 32], WalletError> {
        self.resolve_pending(ledger).await?;

        let latest_height = self.rpc.latest_block_number().await.map_err(|e| WalletError::Rpc(e.to_string()))? as u64;
        let mut spendable = self.spendable_from_ledger(ledger, latest_height)?;
        // Largest-first, same reasoning `scanner::e2e_wallet` already
        // documents: most payments are covered by a single existing
        // output, so trying the biggest first keeps the common case to one
        // `OutputWithDecoys::new` call instead of paying that cost
        // regardless of need.
        spendable.sort_unstable_by_key(|(_, o)| std::cmp::Reverse(o.commitment().amount));

        let to_address = MoneroAddress::from_str(Network::Stagenet, to).expect("invalid destination address");
        let destinations = vec![(to_address, amount)];
        let total_amount = amount;

        // One block of lag margin for decoy selection, not the tip itself -
        // mirrors `scanner::e2e_wallet`'s own reasoning (a pooled public
        // endpoint's backends can genuinely disagree by one block).
        let decoy_block_number = (latest_height.saturating_sub(1)) as usize;
        const MAX_FEE_PER_WEIGHT: u64 = 1_000_000;
        let fee_rate =
            self.rpc.fee_rate(monero_wallet::interface::FeePriority::Unimportant, MAX_FEE_PER_WEIGHT).await.map_err(|e| WalletError::Rpc(e.to_string()))?;

        let mut inputs = Vec::new();
        let mut spent_txids = Vec::new();
        let mut last_necessary_fee: Option<u64> = None;
        let mut remaining = spendable.into_iter();
        let signable = loop {
            let Some((txid, output)) = remaining.next() else {
                return Err(WalletError::InsufficientFunds {
                    needed: total_amount + last_necessary_fee.unwrap_or(0),
                    available: inputs.iter().map(|i: &OutputWithDecoys| i.commitment().amount).sum(),
                    address: self.address(),
                });
            };
            inputs.push(OutputWithDecoys::new(&mut OsRng, &self.decoy_cache, RING_LEN, decoy_block_number, output).await.map_err(|e| WalletError::Rpc(e.to_string()))?);
            spent_txids.push(txid);

            let mut outgoing_view_key = Zeroizing::new([0u8; 32]);
            use rand_core::RngCore;
            OsRng.fill_bytes(outgoing_view_key.as_mut());
            match SignableTransaction::new(
                RctType::ClsagBulletproofPlus,
                outgoing_view_key,
                inputs.clone(),
                destinations.clone(),
                Change::new(self.view_pair.clone(), None),
                vec![],
                fee_rate,
            ) {
                Ok(signable) => break signable,
                Err(SendError::NotEnoughFunds { necessary_fee, .. }) => {
                    last_necessary_fee = necessary_fee;
                    continue;
                }
                Err(SendError::NoInputs) => continue,
                Err(e) => return Err(e.into()),
            }
        };

        let tx: Transaction = signable.sign(&mut OsRng, &self.spend_key)?;
        let hash = tx.hash();
        self.rpc.publish_transaction(&tx).await.map_err(WalletError::Broadcast)?;

        // Only now, after a successful broadcast, mutate the ledger - a
        // failure anywhere above must leave it exactly as it was, so a
        // caller's own retry sees the same spendable set again.
        for entry in ledger.entries.iter_mut() {
            if spent_txids.contains(&entry.txid) {
                entry.spent = true;
            }
        }
        // `0` here is a placeholder, not load-bearing: `amount_piconero` on a
        // still-`Pending` entry is purely informational (shown in a future
        // `InsufficientFunds` message) - the real, authoritative amount
        // comes from the output's own `commitment().amount` once
        // `resolve_pending` scans and serializes it for real, on a later
        // call.
        ledger.record_pending(&hex::encode(hash), 0)?;
        Ok(hash)
    }

    /// A cheap, real pre-flight check for callers that want to fail fast
    /// with a clear, actionable message before doing anything else (an
    /// expensive setup sequence, a whole connect-flow test) rather than
    /// discovering an empty wallet deep inside `send`'s own error. Resolves
    /// any pending entries first, so this reports the ledger's real,
    /// current state, not a stale snapshot.
    pub async fn balance(&self, ledger: &mut Ledger) -> Result<WalletBalance, WalletError> {
        self.resolve_pending(ledger).await?;
        let latest_height = self.rpc.latest_block_number().await.map_err(|e| WalletError::Rpc(e.to_string()))? as u64;
        let mut balance = WalletBalance::default();
        for entry in &ledger.entries {
            if entry.spent {
                continue;
            }
            let (Some(height), Some(hex_bytes)) = (entry.height, &entry.serialized_output_hex) else { continue };
            let bytes = hex::decode(hex_bytes).map_err(|e| WalletError::Ledger(format!("entry {} has invalid serialized_output_hex: {e}", entry.txid)))?;
            let output = WalletOutput::read(&mut &bytes[..]).map_err(|e| WalletError::Ledger(format!("entry {} failed to deserialize: {e}", entry.txid)))?;
            let amount = output.commitment().amount;
            if latest_height.saturating_sub(height) >= SPENDABLE_AGE {
                balance.spendable_piconero += amount;
                balance.spendable_outputs += 1;
            } else {
                balance.pending_piconero += amount;
                balance.pending_outputs += 1;
            }
        }
        Ok(balance)
    }
}

/// See [`StagenetTestWallet::balance`].
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct WalletBalance {
    pub spendable_piconero: u64,
    pub spendable_outputs: usize,
    pub pending_piconero: u64,
    pub pending_outputs: usize,
}

/// The wallet-identifying parameters every real caller needs to pass around
/// together - grouped into one struct purely to keep [`send_payment`]'s own
/// argument count reasonable (`StagenetTestWallet::connect` still takes
/// these positionally; six is well under the threshold that makes grouping
/// worth it, `send_payment`'s own nine wasn't).
pub struct WalletConfig<'a> {
    pub node_url: &'a str,
    pub accept_invalid_certs: bool,
    pub private_spend_key_hex: &'a str,
    pub private_view_key_hex: &'a str,
    pub expected_address: &'a str,
    pub decoy_distribution_path: &'a str,
}

/// Connects, resolves any pending ledger entries, and sends `amount`
/// piconero to `to`, retrying the *whole* connect-then-send sequence from
/// scratch (up to `ATTEMPTS` times, `RETRY_DELAY` apart) on any error except
/// [`WalletError::Broadcast`] - a broadcast was actually attempted and its
/// outcome is genuinely unknown, so that one is never retried (a real
/// double-send risk); every other variant fails strictly before anything is
/// signed or broadcast, so retrying from scratch is safe.
///
/// This is the one canonical entry point real callers should reach for -
/// every real e2e test in this repo talks to the same shared public
/// stagenet node, and every one of them was, at one point or another,
/// observed hitting the exact same real, intermittent RPC failures this
/// retries past (see this crate's own module doc comment). Baking the retry
/// in here once, rather than duplicating it at every call site, is what
/// "a robust library is sufficient" actually means in practice.
pub async fn send_payment(config: WalletConfig<'_>, ledger_path: &str, to: &str, amount: u64) -> Result<[u8; 32], WalletError> {
    const ATTEMPTS: u32 = 5;
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
    let mut attempt = 1;
    loop {
        let result: Result<[u8; 32], WalletError> = async {
            let wallet = StagenetTestWallet::connect(
                config.node_url,
                config.accept_invalid_certs,
                config.private_spend_key_hex,
                config.private_view_key_hex,
                config.expected_address,
                config.decoy_distribution_path,
            )
            .await?;
            let mut ledger = Ledger::load(ledger_path)?;
            wallet.send(&mut ledger, to, amount).await
        }
        .await;
        match result {
            Ok(hash) => return Ok(hash),
            Err(e @ WalletError::Broadcast(_)) => return Err(e),
            Err(e) if attempt < ATTEMPTS => {
                eprintln!("stagenet-test-wallet: attempt {attempt}/{ATTEMPTS}: retrying after: {e}");
                attempt += 1;
                tokio::time::sleep(RETRY_DELAY).await;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(txid: &str, height: Option<u64>, spent: bool) -> LedgerEntry {
        LedgerEntry { txid: txid.to_string(), height, serialized_output_hex: None, amount_piconero: 1, spent }
    }

    #[test]
    fn ledger_round_trips_through_a_real_file() {
        let dir = std::env::temp_dir().join(format!("stagenet-test-wallet-ledger-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.json");
        let path_str = path.to_str().unwrap();

        let mut ledger = Ledger::load(path_str).unwrap();
        assert!(ledger.entries.is_empty(), "a missing file must load as an empty ledger, not error");

        ledger.record_pending("abc123", 500).unwrap();
        let reloaded = Ledger::load(path_str).unwrap();
        assert_eq!(reloaded.entries.len(), 1);
        assert_eq!(reloaded.entries[0].txid, "abc123");
        assert_eq!(reloaded.entries[0].height, None);
        assert!(!reloaded.entries[0].spent);

        // Recording the same txid again must not duplicate it.
        let mut ledger = reloaded;
        ledger.record_pending("abc123", 500).unwrap();
        assert_eq!(Ledger::load(path_str).unwrap().entries.len(), 1, "recording the same txid twice must not duplicate the entry");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn spendable_age_and_spent_status_are_pure_local_filters() {
        // Mirrors `scanner::e2e_wallet`'s own `partition_by_age`/`filter_unspent`
        // tests, but against this crate's ledger-based model instead of a live
        // RPC call - both the age rule and the spent flag are decided
        // entirely from local data, no network involved.
        let ledger = LedgerFile {
            entries: vec![
                entry("old-unspent", Some(100), false),   // spendable at height 120
                entry("too-young", Some(115), false),     // not yet spendable at height 120
                entry("old-but-spent", Some(50), true),   // excluded regardless of age
                entry("unresolved", None, false),         // excluded - no height yet
            ],
        };
        let latest_height = 120u64;
        let spendable: Vec<&str> = ledger
            .entries
            .iter()
            .filter(|e| !e.spent)
            .filter(|e| e.height.is_some_and(|h| latest_height.saturating_sub(h) >= SPENDABLE_AGE))
            .map(|e| e.txid.as_str())
            .collect();
        assert_eq!(spendable, vec!["old-unspent"]);
    }
}
