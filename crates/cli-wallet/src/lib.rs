//! A purpose-built, fast, reliable **stagenet and testnet test wallet** - not a
//! general-purpose Monero wallet, and never meant to become one. Built to
//! replace `engine::e2e_wallet::StagenetSpendWallet` as the thing this
//! repo's real-stagenet e2e suites use to pay a real order with a real,
//! signed, broadcast transaction, and driven by hand through the
//! `wallet-cli` binary, whose commands follow `monero-wallet-cli`.
//! A wallet file records which of the two networks it's for
//! ([`file::WalletData::network`]); mainnet is refused, since every key here
//! is kept in plaintext.
//!
//! # Why this exists
//!
//! The predecessor (`engine::e2e_wallet`) does real, correct Monero wallet
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
//!   about its own outputs directly - a txid added as pending, whose one
//!   block is then scanned once it confirms - rather than rediscovering
//!   them by asking the chain "what's mine?" on every run. Once resolved,
//!   an output's entire [`monero_wallet::WalletOutput`] is serialized into
//!   the wallet's own SQLite file ([`file::WalletData`]) and committed, so
//!   every later run reads it straight off disk with zero RPC calls.
//!   The one exception is `rescan <blocks>` ([`Wallet::rescan`]), which
//!   scans a chosen range of blocks ([`block_range::BlockRange`]) - only
//!   when asked, never as part of any other operation.
//! - **Decoy selection still runs the real, correct algorithm** (still
//!   picks genuine, unlocked, on-chain outputs - a node will reject
//!   anything less, stagenet or not) but is fed from a *cached, committed*
//!   output-distribution snapshot instead of a live fetch every time - see
//!   [`DecoyCache`]'s own doc comment for why this is provably safe, not
//!   just fast.
//! - **No live spent-status checks by default.** Each wallet is the only
//!   spender of its own keys - it marks an output `spent` the moment it
//!   successfully broadcasts a transaction spending it, and trusts that
//!   record on every later run. `rescan_spent` checks it against the chain
//!   (one key-image query, still no scanning) when asked.
//!
//! What's *not* narrowed: the actual transaction construction and signing
//! (`monero_wallet::send::SignableTransaction`, real CLSAG + Bulletproofs+)
//! and broadcast are unchanged from the predecessor - a transaction this
//! crate builds is exactly as real and exactly as valid as any other Monero
//! wallet's.

pub mod amount;
pub mod block_range;
pub mod file;
pub mod meta;
mod store;
mod wallet;

use std::ops::RangeBounds;
use std::path::{Path, PathBuf};

use monero_daemon_rpc::{prelude::*, HttpTransport, MoneroDaemon};
use monero_seed::{Language as ElectrumLanguage, Seed as ElectrumSeed};
use monero_wallet::{
    ed25519::{Point, Scalar},
    interface::ProvidesUnvalidatedDecoys,
    send::SendError,
    ViewPair, WalletOutput,
};
use polyseed::{Language as PolyseedLanguage, Polyseed};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

pub use file::{WalletData, WalletFile};
pub use monero_wallet::address::Network;
pub use monero_wallet::interface::FeePriority;
pub use wallet::{
    CommittedTransfer, DaemonVersion, OwnedOutput, PreparedTransfer, RescanReport, SweepSelect,
    TransferKind, TransferRequest, Wallet, WalletBalance, WalletKeys, MAX_OUTPUTS,
};

/// The ring size required for the `ClsagBulletproofPlus` RCT type this
/// module always signs with - the standard type on every live Monero
/// network today. Mirrors `engine::e2e_wallet`'s own constant.
pub const RING_LEN: u8 = 16;

/// Monero requires this many confirmations on any output before it's
/// spendable - `CRYPTONOTE_DEFAULT_TX_SPENDABLE_AGE`, a real consensus rule.
pub const SPENDABLE_AGE: u64 = 10;

/// The networks this wallet works on, by the name a wallet file records
/// ([`file::WalletData::network`]). Mainnet isn't one: every key here is
/// kept in plaintext.
pub const NETWORKS: [(&str, Network); 2] = [
    ("stagenet", Network::Stagenet),
    ("testnet", Network::Testnet),
];

/// `network`'s name in a wallet file and in messages: `stagenet`,
/// `testnet` (and `mainnet`, which no wallet file may name).
pub fn network_name(network: Network) -> &'static str {
    match network {
        Network::Mainnet => "mainnet",
        Network::Stagenet => "stagenet",
        Network::Testnet => "testnet",
    }
}

/// The network a wallet file names, refusing any this wallet doesn't work
/// on (see [`NETWORKS`]).
pub fn parse_network(name: &str) -> Result<Network, WalletError> {
    NETWORKS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, network)| *network)
        .ok_or_else(|| {
            WalletError::Invalid(format!(
                "{name} isn't a network this wallet works on - one of {:?}",
                NETWORKS.map(|(known, _)| known)
            ))
        })
}

/// How to fund a wallet on `network` that has run out, for
/// [`WalletError::InsufficientFunds`].
fn funding_steps(network: Network, address: &str) -> String {
    match network {
        Network::Stagenet => format!(
            "fund the wallet from the stagenet faucet:\n\
             1. open https://stagenet-faucet.xmr-tw.org/\n\
             2. send to: {address}\n\
             3. record the faucet's txid: wallet-cli add_output <txid>"
        ),
        other => format!(
            "fund the wallet with {} XMR (from another wallet or by mining to it):\n\
             1. send to: {address}\n\
             2. record the txid: wallet-cli add_output <txid>",
            network_name(other)
        ),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WalletError {
    #[error("cannot reach the {} node at {url}: {source}", network_name(*.network))]
    DaemonUnreachable {
        network: Network,
        url: String,
        source: InterfaceError,
    },
    #[error(
        "insufficient funds: this payment needs {needed} piconero, but only {available} \
         piconero of spendable outputs were found (an output needs {SPENDABLE_AGE} \
         confirmations before it's spendable - if a recent send's change is still that young, \
         this is expected; wait and retry).\n\
         Otherwise, {}", funding_steps(*.network, .address)
    )]
    InsufficientFunds {
        needed: u64,
        available: u64,
        network: Network,
        address: String,
    },
    #[error("failed to build/sign the transaction: {0}")]
    Send(#[from] SendError),
    /// The node may have accepted the transaction even so (a timeout, a
    /// reset connection): its id is kept, so it can be looked for.
    #[error(
        "failed to broadcast transaction {txid}: {source}\n\
         It may still have reached the node: run rescan_spent, then add_output {txid} if it was mined."
    )]
    Broadcast {
        txid: String,
        #[source]
        source: PublishTransactionError,
    },
    /// The transaction was broadcast, but the wallet file couldn't record
    /// it. Never retried: the inputs still look unspent in the file, and a
    /// retry would sign and send them again.
    #[error(
        "transaction {txid} was sent, but recording it in the wallet file failed: {reason}\n\
         Run rescan_spent, then add_output {txid}, before sending again."
    )]
    RecordFailed { txid: String, reason: String },
    #[error("daemon RPC call failed: {0}")]
    Rpc(String),
    #[error("wallet file error: {0}")]
    WalletFile(String),
    /// Someone else held the wallet file's lock and the caller chose not to
    /// wait (see [`file::WalletFile::lock_with`]).
    #[error("{0}; cancelled")]
    Locked(String),
    #[error("decoy distribution error: {0}")]
    Decoys(String),
    /// A request that can't be carried out as asked (a bad address, an
    /// impossible fee split) - the message says why.
    #[error("{0}")]
    Invalid(String),
}

/// `monero-daemon-rpc`'s `HttpTransport` over a plain `reqwest::Client` -
/// verbatim from `engine::e2e_wallet::ReqwestTransport`, see that type's
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
        let request = self
            .client
            .post(format!("{}/{route}", self.base_url))
            .header("content-type", "application/json")
            .body(body);
        async move {
            let response = request
                .send()
                .await
                .map_err(|e| InterfaceError::InterfaceError(format!("request failed: {e}")))?;
            if let (Some(limit), Some(len)) = (response_size_limit, response.content_length()) {
                if len > limit as u64 {
                    return Err(InterfaceError::InterfaceError(format!("response claimed {len} bytes, exceeding the {limit}-byte limit for {route}")));
                }
            }
            let bytes = response
                .bytes()
                .await
                .map_err(|e| InterfaceError::InterfaceError(format!("{e}")))?;
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
///
/// With no snapshot (`distribution: None`, testnet's case) the distribution
/// is fetched from the node like everything else.
struct DecoyCache {
    daemon: MoneroDaemon<ReqwestTransport>,
    distribution: Option<Vec<u64>>,
}

impl ProvidesBlockchainMeta for DecoyCache {
    fn latest_block_number(
        &self,
    ) -> impl Send + std::future::Future<Output = Result<usize, InterfaceError>> {
        self.daemon.latest_block_number()
    }
}

impl ProvidesUnvalidatedDecoys for DecoyCache {
    fn ringct_output_distribution(
        &self,
        range: impl Send + RangeBounds<usize>,
    ) -> impl Send + std::future::Future<Output = Result<Vec<u64>, InterfaceError>> {
        // With a snapshot, the range argument is deliberately ignored - see
        // this type's own doc comment for why that's safe for how the one
        // real caller (`select_n`) actually uses the result.
        let cached = self.distribution.clone();
        let live = cached
            .is_none()
            .then(|| ProvidesUnvalidatedDecoys::ringct_output_distribution(&self.daemon, range));
        async move {
            match (cached, live) {
                (Some(distribution), _) => Ok(distribution),
                (None, Some(live)) => live.await,
                (None, None) => unreachable!("no snapshot means a live fetch"),
            }
        }
    }

    fn unlocked_ringct_outputs(
        &self,
        indexes: &[u64],
        evaluate_unlocked: EvaluateUnlocked,
    ) -> impl Send + std::future::Future<Output = Result<Vec<Option<[Point; 2]>>, TransactionsError>>
    {
        ProvidesUnvalidatedDecoys::unlocked_ringct_outputs(&self.daemon, indexes, evaluate_unlocked)
    }
}

/// A wallet's key material: what a new wallet file starts from, and how
/// the old shared `stagenet-wallets.json` recorded each wallet (hence the
/// serde names).
///
/// The keys and seed are plain `String`s and are not zeroised on drop, nor
/// are their copies in [`WalletData`], `ResolvedWallet` or the SQLite
/// buffers. These are stagenet test wallets whose files hold the same keys
/// in plaintext, so clearing memory would protect nothing. The
/// `Zeroizing` scalars in the signing code clear only those working copies.
#[derive(Clone, Serialize, Deserialize)]
pub struct WalletCredentials {
    pub address: String,
    #[serde(rename = "private_spend_key")]
    pub private_spend_key_hex: String,
    #[serde(rename = "private_view_key")]
    pub private_view_key_hex: String,
    /// The seed phrase these keys came from, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mnemonic: Option<String>,
}

/// Every path/network setting a real caller needs to talk to the e2e
/// fixtures, in one place. Every real caller in this repo wants the same
/// standard `e2e/*` layout, so `WalletCtx::default()` is the one thing most
/// callers need to name at all.
#[derive(Debug, Clone)]
pub struct WalletCtx {
    /// The network the wallets are on. A wallet file for another network
    /// is refused ([`ResolvedWallet::open`]).
    pub network: Network,
    /// Nodes to use, in order of preference. A node that can't be
    /// reached (public nodes rate-limit, and one busy address - an e2e
    /// test's own engine talking to the same node - is enough to have
    /// connections reset) is skipped for the next one; see
    /// [`ResolvedWallet::connect`] and [`send_payment`].
    pub node_urls: Vec<String>,
    pub accept_invalid_certs: bool,
    /// Where wallet files live: a wallet named `spender` is
    /// `<wallet_dir>/spender.db`.
    pub wallet_dir: PathBuf,
    /// The decoy-distribution snapshot to select decoys from (see
    /// [`DecoyCache`]). `None` fetches the distribution from the node on
    /// each connection instead - what testnet does, having no committed
    /// snapshot.
    pub decoy_distribution_path: Option<String>,
}

/// `nodes` in the order a connection attempt tries them: starting at index
/// `start` (wrapping around), each without a trailing `/`.
fn nodes_in_order(nodes: &[String], start: usize) -> Vec<String> {
    (0..nodes.len())
        .map(|offset| {
            nodes[(start + offset) % nodes.len()]
                .trim_end_matches('/')
                .to_string()
        })
        .collect()
}

/// See [`WalletCtx::node_urls`].
pub const DEFAULT_TESTNET_NODES: [&str; 3] = [
    "http://node.monerodevs.org:28089",
    "http://node2.monerodevs.org:28089",
    "http://node3.monerodevs.org:28089",
];

/// See [`WalletCtx::node_urls`].
pub const DEFAULT_STAGENET_NODES: [&str; 3] = [
    "http://node.monerodevs.org:38089",
    "http://node2.monerodevs.org:38089",
    "http://node3.monerodevs.org:38089",
];

/// The repository's own `e2e/` directory, anchored to *this crate's* own
/// compile-time location (`crates/cli-wallet/`) rather than whatever the
/// process's current directory happens to be at runtime. Deliberate:
/// `cargo test` sets a test binary's working directory to its *own
/// package's* manifest directory, so a caller-relative path like `"e2e/..."`
/// is only ever correct for whichever one crate happened to inspire it, and
/// silently wrong for every other crate's tests (confirmed the hard way:
/// this bit both `crates/engine`'s and `crates/mock-woocommerce`'s real
/// e2e tests before this fix). `CARGO_MANIFEST_DIR` is fixed at compile
/// time to wherever *this* crate's `Cargo.toml` lives, so this is correct
/// everywhere, always, by construction.
macro_rules! e2e_path {
    ($file:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../e2e/", $file)
    };
}

impl Default for WalletCtx {
    /// The standard layout every real e2e suite in this repo already uses -
    /// see `e2e/README.md`.
    fn default() -> Self {
        Self::for_network(Network::Stagenet)
    }
}

impl WalletCtx {
    /// [`WalletCtx::default`]'s layout for wallets on `network`: its
    /// default nodes, and the committed decoy snapshot where there is one
    /// (stagenet's).
    pub fn for_network(network: Network) -> Self {
        let (nodes, decoy_distribution_path) = match network {
            Network::Testnet => (DEFAULT_TESTNET_NODES, None),
            // The same three community stagenet nodes monokulo's stagenet
            // config uses (`e2e/moneropay-stagenet.toml`): separate
            // machines, so one rate-limiting us doesn't block the others.
            _ => (
                DEFAULT_STAGENET_NODES,
                Some(e2e_path!("stagenet-decoy-distribution.json").to_string()),
            ),
        };
        Self {
            network,
            node_urls: nodes.iter().map(|url| url.to_string()).collect(),
            accept_invalid_certs: true,
            wallet_dir: PathBuf::from(e2e_path!("wallets")),
            decoy_distribution_path,
        }
    }

    /// A `--wallet-file` argument as a path: a bare name (`spender`) is
    /// `<wallet_dir>/<name>.db`; anything that looks like a path (a
    /// directory, or an extension of its own) is used as given.
    pub fn wallet_path(&self, name_or_path: &str) -> PathBuf {
        if name_or_path.contains(std::path::MAIN_SEPARATOR)
            || Path::new(name_or_path).extension().is_some()
        {
            PathBuf::from(name_or_path)
        } else {
            self.wallet_dir.join(format!("{name_or_path}.db"))
        }
    }
}

/// A wallet file's key material plus enough of a [`WalletCtx`] to connect
/// to it - what [`WalletStore::wallet`] returns, and everything
/// [`Self::connect`]/[`send_payment`] need.
#[derive(Clone)]
pub struct ResolvedWallet {
    /// The wallet file.
    pub path: PathBuf,
    pub address: String,
    pub private_spend_key_hex: String,
    pub private_view_key_hex: String,
    pub network: Network,
    /// See [`WalletCtx::node_urls`].
    pub node_urls: Vec<String>,
    pub accept_invalid_certs: bool,
    /// See [`WalletCtx::decoy_distribution_path`].
    pub decoy_distribution_path: Option<String>,
}

/// Without the private keys or the mnemonic.
impl std::fmt::Debug for WalletCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletCredentials")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

/// Without the private keys.
impl std::fmt::Debug for ResolvedWallet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedWallet")
            .field("path", &self.path)
            .field("address", &self.address)
            .field("network", &self.network)
            .field("node_urls", &self.node_urls)
            .finish_non_exhaustive()
    }
}

impl ResolvedWallet {
    /// Reads the wallet file at `path`, with `ctx`'s node settings,
    /// refusing it unless it's for `ctx`'s network.
    pub fn open(ctx: &WalletCtx, path: impl AsRef<Path>) -> Result<Self, WalletError> {
        let file = WalletFile::load(path.as_ref())?;
        let network = file.data.network()?;
        if network != ctx.network {
            return Err(WalletError::WalletFile(format!(
                "{} is a {} wallet, not {}",
                path.as_ref().display(),
                network_name(network),
                network_name(ctx.network)
            )));
        }
        Ok(ResolvedWallet {
            path: path.as_ref().to_path_buf(),
            address: file.data.address,
            private_spend_key_hex: file.data.private_spend_key,
            private_view_key_hex: file.data.private_view_key,
            network,
            node_urls: ctx.node_urls.clone(),
            accept_invalid_certs: ctx.accept_invalid_certs,
            decoy_distribution_path: ctx.decoy_distribution_path.clone(),
        })
    }

    /// This wallet's *public* spend key, hex-encoded - derived locally from
    /// `self.private_spend_key_hex`, never the private key itself. For
    /// handing to something that must never see a private key even for a
    /// worthless stagenet fixture (moneropay's own real connect API, which
    /// only ever takes a view key + spend *public* key for a watch-only
    /// tenant) while this crate still holds full credentials for every
    /// wallet it knows about.
    pub fn spend_public_key_hex(&self) -> Result<String, WalletError> {
        let spend_key = scalar_from_hex(&self.private_spend_key_hex)?;
        let spend_key_dalek: curve25519_dalek::Scalar = (*spend_key).into();
        let public_spend =
            Point::from(&spend_key_dalek * curve25519_dalek::constants::ED25519_BASEPOINT_TABLE);
        Ok(hex::encode(public_spend.compress().to_bytes()))
    }

    /// Everything that works without a node. Derives keys from `self`'s own
    /// hex-encoded private spend/view keys, refusing them unless the
    /// derived address is `self.address`.
    pub fn keys(&self) -> Result<WalletKeys, WalletError> {
        let data = WalletData::new(
            self.network,
            WalletCredentials {
                address: self.address.clone(),
                private_spend_key_hex: self.private_spend_key_hex.clone(),
                private_view_key_hex: self.private_view_key_hex.clone(),
                mnemonic: None,
            },
        );
        WalletKeys::from_data(&data, self.path.clone())
    }

    /// Connects to the first of `self.node_urls` that answers and loads
    /// the cached decoy-distribution snapshot at
    /// `self.decoy_distribution_path` (or, with none, fetches the
    /// distribution from that node).
    pub async fn connect(&self) -> Result<Wallet, WalletError> {
        self.connect_starting_at(0).await
    }

    /// [`Self::connect`], trying [`Self::node_urls`] in order starting at
    /// index `start` (wrapping around) and using the first that answers.
    /// [`send_payment`] moves `start` on for each retry, so a node that
    /// accepts a connection but then fails the payment isn't tried first
    /// every time.
    pub async fn connect_starting_at(&self, start: usize) -> Result<Wallet, WalletError> {
        let keys = self.keys()?;

        let http_client = reqwest::Client::builder()
            .danger_accept_invalid_certs(self.accept_invalid_certs)
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("failed to build reqwest client");
        assert!(
            !self.node_urls.is_empty(),
            "no {} node configured",
            network_name(self.network)
        );
        let mut last_error = None;
        let mut connected = None;
        for node_url in nodes_in_order(&self.node_urls, start) {
            let transport = ReqwestTransport {
                client: http_client.clone(),
                base_url: node_url.clone(),
            };
            match MoneroDaemon::new(transport).await {
                Ok(rpc) => {
                    connected = Some((rpc, node_url));
                    break;
                }
                Err(source) => {
                    let error = WalletError::DaemonUnreachable {
                        network: self.network,
                        url: node_url,
                        source,
                    };
                    eprintln!("cli-wallet: {error}; trying the next node");
                    last_error = Some(error);
                }
            }
        }
        let Some((rpc, node_url)) = connected else {
            return Err(last_error.expect("at least one node was tried"));
        };
        let distribution = match &self.decoy_distribution_path {
            Some(path) => Some(load_decoy_distribution(path)?),
            None => None,
        };
        let decoy_cache = DecoyCache {
            daemon: rpc.clone(),
            distribution,
        };

        Ok(Wallet {
            keys,
            rpc,
            decoy_cache,
            http_client,
            node_url,
        })
    }
}

/// The e2e suites' way in: named wallets in [`WalletCtx::wallet_dir`].
pub struct WalletStore {
    ctx: WalletCtx,
}

impl WalletStore {
    pub fn load(ctx: &WalletCtx) -> Result<Self, WalletError> {
        if !ctx.wallet_dir.is_dir() {
            return Err(WalletError::WalletFile(format!(
                "wallet directory {} doesn't exist",
                ctx.wallet_dir.display()
            )));
        }
        Ok(Self { ctx: ctx.clone() })
    }

    /// The wallet named `name` (`<wallet_dir>/<name>.db`). Every wallet
    /// this crate manages is a worthless stagenet fixture, so every one has
    /// its spend key recorded - even moneropay's own tenant (`merchant`);
    /// only the *public* half ([`ResolvedWallet::spend_public_key_hex`]) is
    /// ever actually handed to moneropay's real connect API, so the e2e
    /// tests still exercise it exactly as a genuinely watch-only tenant
    /// would be.
    pub fn wallet(&self, name: &str) -> Result<ResolvedWallet, WalletError> {
        ResolvedWallet::open(&self.ctx, self.ctx.wallet_path(name))
    }
}

const POLYSEED_LANGUAGES: [PolyseedLanguage; 10] = [
    PolyseedLanguage::English,
    PolyseedLanguage::Spanish,
    PolyseedLanguage::French,
    PolyseedLanguage::Italian,
    PolyseedLanguage::Japanese,
    PolyseedLanguage::Korean,
    PolyseedLanguage::Czech,
    PolyseedLanguage::Portuguese,
    PolyseedLanguage::ChineseSimplified,
    PolyseedLanguage::ChineseTraditional,
];

const ELECTRUM_LANGUAGES: [ElectrumLanguage; 13] = [
    ElectrumLanguage::English,
    ElectrumLanguage::Chinese,
    ElectrumLanguage::Dutch,
    ElectrumLanguage::French,
    ElectrumLanguage::Spanish,
    ElectrumLanguage::German,
    ElectrumLanguage::Italian,
    ElectrumLanguage::Portuguese,
    ElectrumLanguage::Japanese,
    ElectrumLanguage::Russian,
    ElectrumLanguage::Esperanto,
    ElectrumLanguage::Lojban,
    ElectrumLanguage::DeprecatedEnglish,
];

/// `--mnemonic-language` names (the reference wallet's English names) for
/// the legacy 25-word seeds this crate generates and prints, in
/// [`ELECTRUM_LANGUAGES`] order.
pub const SEED_LANGUAGE_NAMES: [&str; 12] = [
    "English",
    "Chinese",
    "Dutch",
    "French",
    "Spanish",
    "German",
    "Italian",
    "Portuguese",
    "Japanese",
    "Russian",
    "Esperanto",
    "Lojban",
];

fn seed_language(name: &str) -> Result<ElectrumLanguage, WalletError> {
    SEED_LANGUAGE_NAMES
        .iter()
        .position(|known| known.eq_ignore_ascii_case(name))
        .map(|i| ELECTRUM_LANGUAGES[i])
        .ok_or_else(|| {
            WalletError::Invalid(format!(
                "unknown mnemonic language {name:?} - one of {SEED_LANGUAGE_NAMES:?}"
            ))
        })
}

/// Derives `WalletCredentials` (address + hex-encoded spend/view keys) from
/// a real spend key - the view key deterministically (`view = Hs(spend)`,
/// exactly [`Scalar::hash`]'s own documented definition, as every Monero
/// wallet derives it) and the address from both, rather than trusting a
/// caller-supplied pair.
fn credentials_from_spend_key(
    network: Network,
    spend_key: Zeroizing<Scalar>,
    mnemonic: Option<String>,
) -> WalletCredentials {
    let view_key = Zeroizing::new(Scalar::hash(<[u8; 32]>::from(*spend_key)));
    let spend_key_dalek: Zeroizing<curve25519_dalek::Scalar> = Zeroizing::new((*spend_key).into());
    let public_spend =
        Point::from(&*spend_key_dalek * curve25519_dalek::constants::ED25519_BASEPOINT_TABLE);
    let address = ViewPair::new(public_spend, Zeroizing::new(*view_key))
        .expect("a freshly derived spend key is never torsioned")
        .legacy_address(network);
    WalletCredentials {
        address: address.to_string(),
        private_spend_key_hex: hex::encode(<[u8; 32]>::from(*spend_key)),
        private_view_key_hex: hex::encode(<[u8; 32]>::from(*view_key)),
        mnemonic,
    }
}

/// `--generate-new-wallet`: fresh random keys for a wallet on `network`,
/// recorded with their 25-word seed in `language`.
pub fn generate_credentials(
    network: Network,
    language: &str,
) -> Result<WalletCredentials, WalletError> {
    let seed = ElectrumSeed::new(&mut OsRng, seed_language(language)?);
    let spend_key = Zeroizing::new(
        Scalar::read(&mut &seed.entropy()[..])
            .expect("a generated legacy Seed's own entropy is always a canonical scalar"),
    );
    Ok(credentials_from_spend_key(
        network,
        spend_key,
        Some(seed.to_string().to_string()),
    ))
}

/// `--generate-from-spend-key`: the wallet on `network` a hex private spend
/// key belongs to.
pub fn credentials_from_spend_key_hex(
    network: Network,
    spend_key_hex: &str,
) -> Result<WalletCredentials, WalletError> {
    let bytes: [u8; 32] = hex::decode(spend_key_hex.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| WalletError::Invalid("failed to parse spend key secret key".to_string()))?;
    let spend_key = Scalar::read(&mut &bytes[..]).map_err(|_| {
        WalletError::Invalid("spend key isn't a canonical ed25519 scalar".to_string())
    })?;
    Ok(credentials_from_spend_key(
        network,
        Zeroizing::new(spend_key),
        None,
    ))
}

/// `--restore-deterministic-wallet`: the wallet on `network` a real Monero
/// seed phrase restores - either a 16-word Polyseed or a 24/25-word legacy
/// Electrum-style seed, tried against every language each format supports
/// (neither crate autodetects language from the words alone).
pub fn credentials_from_seed(
    network: Network,
    phrase: &str,
) -> Result<WalletCredentials, WalletError> {
    let phrase = Zeroizing::new(phrase.split_whitespace().collect::<Vec<_>>().join(" "));
    let word_count = phrase.split_whitespace().count();

    if word_count == 16 {
        for lang in POLYSEED_LANGUAGES {
            if let Ok(seed) = Polyseed::from_string(lang, phrase.clone()) {
                let spend_key = Zeroizing::new(Scalar::from(
                    curve25519_dalek::Scalar::from_bytes_mod_order(*seed.key()),
                ));
                return Ok(credentials_from_spend_key(
                    network,
                    spend_key,
                    Some(phrase.to_string()),
                ));
            }
        }
        return Err(WalletError::Invalid(
            "16-word phrase didn't parse as a Polyseed in any supported language".to_string(),
        ));
    }

    if word_count == 24 || word_count == 25 {
        for lang in ELECTRUM_LANGUAGES {
            if let Ok(seed) = ElectrumSeed::from_string(lang, phrase.clone()) {
                let entropy = seed.entropy();
                let spend_key = Zeroizing::new(
                    Scalar::read(&mut &entropy[..])
                        .expect("a parsed legacy Seed's own entropy is always a canonical scalar"),
                );
                return Ok(credentials_from_spend_key(
                    network,
                    spend_key,
                    Some(phrase.to_string()),
                ));
            }
        }
        return Err(WalletError::Invalid(format!("{word_count}-word phrase didn't parse as a legacy Electrum-style seed in any supported language")));
    }

    Err(WalletError::Invalid(format!("seed phrase has {word_count} words - expected 16 (Polyseed) or 24/25 (legacy Electrum-style)")))
}

/// The 25-word seed that restores `spend_key_hex`, in `language` - what
/// `seed` prints for a wallet with no recorded mnemonic.
pub fn legacy_seed_for(spend_key_hex: &str, language: &str) -> Result<String, WalletError> {
    let seed = ElectrumSeed::from_entropy(seed_language(language)?, hex32(spend_key_hex)?)
        .ok_or_else(|| WalletError::Invalid("spend key has no 25-word seed".to_string()))?;
    Ok(seed.to_string().to_string())
}

/// Reads back an output stored as hex-encoded `WalletOutput::serialize()`.
pub(crate) fn decode_output(txid: &str, hex_bytes: &str) -> Result<WalletOutput, WalletError> {
    let bytes = hex::decode(hex_bytes).map_err(|e| {
        WalletError::WalletFile(format!(
            "output of {txid} has invalid serialized_output_hex: {e}"
        ))
    })?;
    WalletOutput::read(&mut &bytes[..]).map_err(|e| {
        WalletError::WalletFile(format!("output of {txid} failed to deserialize: {e}"))
    })
}

/// 32 bytes of key material from a wallet file. A hand-edited or corrupt
/// file is an error the CLI reports, never a panic.
fn hex32(hex_str: &str) -> Result<Zeroizing<[u8; 32]>, WalletError> {
    let bytes = Zeroizing::new(
        hex::decode(hex_str)
            .map_err(|e| WalletError::WalletFile(format!("key material isn't hex: {e}")))?,
    );
    let array: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        WalletError::WalletFile(format!("key material is {} bytes, not 32", bytes.len()))
    })?;
    Ok(Zeroizing::new(array))
}

fn scalar_from_hex(hex_str: &str) -> Result<Zeroizing<Scalar>, WalletError> {
    Scalar::read(&mut &hex32(hex_str)?[..])
        .map(Zeroizing::new)
        .map_err(|_| {
            WalletError::WalletFile("a private key isn't a canonical ed25519 scalar".to_string())
        })
}

/// Loads a committed output-distribution snapshot (a plain JSON array of
/// `u64`, produced by this crate's own `refresh-decoy-pool` `[[bin]]`) for
/// `DecoyCache` - see that type's own doc comment for why a cached snapshot
/// is a fully valid input, not an approximation.
fn load_decoy_distribution(path: &str) -> Result<Vec<u64>, WalletError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| WalletError::Decoys(format!("failed to read {path}: {e}")))?;
    serde_json::from_str(&contents)
        .map_err(|e| WalletError::Decoys(format!("failed to parse {path}: {e}")))
}

/// Fetches one real `ringct_output_distribution` snapshot over `from..=to`
/// and writes it to `out_path` as a plain JSON array - the one place this
/// crate ever performs the expensive live fetch `DecoyCache` exists to
/// avoid on every send. Meant to be run occasionally, by hand, via the
/// `refresh-decoy-pool` `[[bin]]` - never by the e2e suites themselves.
pub async fn refresh_decoy_distribution(
    node_url: &str,
    accept_invalid_certs: bool,
    from: usize,
    to: usize,
    out_path: &str,
) -> Result<usize, WalletError> {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(accept_invalid_certs)
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("failed to build reqwest client");
    let transport = ReqwestTransport {
        client,
        base_url: node_url.trim_end_matches('/').to_string(),
    };
    let daemon = MoneroDaemon::new(transport)
        .await
        .map_err(|source| WalletError::Rpc(format!("cannot reach {node_url}: {source}")))?;
    let distribution = ProvidesUnvalidatedDecoys::ringct_output_distribution(&daemon, from..=to)
        .await
        .map_err(|e| WalletError::Rpc(e.to_string()))?;
    std::fs::write(out_path, serde_json::to_string(&distribution).unwrap())
        .map_err(|e| WalletError::Decoys(format!("failed to write {out_path}: {e}")))?;
    Ok(distribution.len())
}

/// A single, plain `/get_transactions` call for one txid, reading just the
/// `block_height` field - deliberately not going through
/// `monero-daemon-rpc`'s own typed transaction-fetching (which
/// deserializes the full transaction body, work this only needs metadata
/// for) or `scanner`'s equivalent (this crate has no dependency on
/// `scanner` at all, by design). `Ok(None)` means "not confirmed yet"
/// (still in the mempool, or genuinely unknown) - both are left pending.
async fn locate_height(
    client: &reqwest::Client,
    node_url: &str,
    txid: &str,
) -> Result<Option<u64>, WalletError> {
    let response: Value = client
        .post(format!("{node_url}/get_transactions"))
        .json(&serde_json::json!({ "txs_hashes": [txid], "decode_as_json": false }))
        .send()
        .await
        .map_err(|e| WalletError::Rpc(format!("get_transactions request failed: {e}")))?
        .json()
        .await
        .map_err(|e| WalletError::Rpc(format!("get_transactions returned invalid JSON: {e}")))?;
    Ok(response["txs"]
        .as_array()
        .and_then(|txs| txs.first())
        .and_then(|tx| tx["block_height"].as_u64()))
}

/// Connects, resolves anything pending, and sends `amount` piconero to
/// `to`, retrying the *whole* connect-then-send sequence from scratch (up
/// to `ATTEMPTS` times, `RETRY_DELAY` apart) on any error except
/// [`WalletError::Broadcast`] and [`WalletError::RecordFailed`] - a
/// broadcast was actually attempted (its outcome unknown, or known and not
/// recorded), so those are never retried (a real double-send risk); every
/// other variant fails strictly before anything is signed or broadcast, so
/// retrying from scratch is safe.
///
/// This is the one canonical entry point real callers should reach for -
/// every real e2e test in this repo talks to the same shared public
/// stagenet node, and every one of them was, at one point or another,
/// observed hitting the exact same real, intermittent RPC failures this
/// retries past (see this crate's own module doc comment). Baking the retry
/// in here once, rather than duplicating it at every call site, is what
/// "a robust library is sufficient" actually means in practice.
pub async fn send_payment(
    wallet: ResolvedWallet,
    to: &str,
    amount: u64,
) -> Result<[u8; 32], WalletError> {
    const ATTEMPTS: u32 = 5;
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
    let mut attempt = 1;
    loop {
        let result: Result<[u8; 32], WalletError> = async {
            // Each retry starts from the next node (see
            // `ResolvedWallet::connect_starting_at`).
            let wallet = wallet.connect_starting_at((attempt - 1) as usize).await?;
            wallet.send(to, amount).await
        }
        .await;
        match result {
            Ok(hash) => return Ok(hash),
            Err(e @ (WalletError::Broadcast { .. } | WalletError::RecordFailed { .. })) => {
                return Err(e)
            }
            Err(e) if attempt < ATTEMPTS => {
                eprintln!("cli-wallet: attempt {attempt}/{ATTEMPTS}: retrying after: {e}");
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
    use crate::file::{migrate_legacy, OutputRecord};
    use crate::wallet::{record_resolved, record_scanned, ScannedOutput};

    #[test]
    fn each_retry_starts_from_the_next_node_and_still_tries_them_all() {
        let nodes: Vec<String> = DEFAULT_STAGENET_NODES
            .iter()
            .map(|n| format!("{n}/"))
            .collect();
        let host = |url: &String| {
            url.split("//")
                .nth(1)
                .unwrap()
                .split('.')
                .next()
                .unwrap()
                .to_string()
        };
        let order = |start| {
            nodes_in_order(&nodes, start)
                .iter()
                .map(host)
                .collect::<Vec<_>>()
        };
        assert_eq!(order(0), ["node", "node2", "node3"]);
        assert_eq!(order(1), ["node2", "node3", "node"]);
        assert_eq!(order(4), ["node2", "node3", "node"], "wraps around");
        assert!(nodes_in_order(&nodes, 0)
            .iter()
            .all(|url| !url.ends_with('/')));
    }

    /// Three real outputs of one stagenet split transaction
    /// (`testdata/split_transaction_outputs.json`), all paying `spender`.
    fn split_transaction() -> (String, u64, Vec<WalletOutput>) {
        let fixture: Value =
            serde_json::from_str(include_str!("../testdata/split_transaction_outputs.json"))
                .unwrap();
        let outputs = fixture["serialized_outputs_hex"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hex_bytes| {
                WalletOutput::read(&mut &hex::decode(hex_bytes.as_str().unwrap()).unwrap()[..])
                    .unwrap()
            })
            .collect();
        (
            fixture["txid"].as_str().unwrap().to_string(),
            fixture["height"].as_u64().unwrap(),
            outputs,
        )
    }

    /// The stagenet keys of the e2e `spender` and `merchant` wallets (which
    /// `testdata/split_transaction_outputs.json` pays), fixed here rather
    /// than read from `e2e/wallets`, which the e2e suites lock and rewrite.
    fn fixture_wallet(name: &str) -> WalletCredentials {
        let spend_key = match name {
            "spender" => "df97ded57234e9cfb41c2a1e338721162109db0326239e056fe7ea8228c4f30e",
            "merchant" => "c1062227347db87da4fdeee556fd5ed3e4c3e5857f295489d8a267b69d758d09",
            other => panic!("no fixture wallet named {other}"),
        };
        WalletCredentials {
            mnemonic: None,
            ..credentials_from_spend_key_hex(Network::Stagenet, spend_key).unwrap()
        }
    }

    /// A fixture wallet's keys, with no outputs, in its own temp file.
    fn temp_wallet(test: &str, name: &str) -> (PathBuf, WalletData) {
        let dir = std::env::temp_dir().join(format!("cli-wallet-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = WalletData::new(Network::Stagenet, fixture_wallet(name));
        let path = dir.join(format!("{name}.db"));
        WalletFile::create(&path, data.clone()).unwrap();
        (path, data)
    }

    fn output_index(record: &OutputRecord) -> u64 {
        decode_output(&record.txid, &record.serialized_output_hex)
            .unwrap()
            .index_in_transaction()
    }

    /// Regression: a split pays this wallet many outputs in one
    /// transaction, and the ledger used to keep only the first, silently
    /// losing track of the rest.
    #[test]
    fn every_output_a_transaction_pays_the_wallet_gets_its_own_record() {
        let (txid, height, outputs) = split_transaction();
        let (_, mut data) = temp_wallet("record-all", "spender");
        data.add_pending(&txid, 0);

        assert_eq!(
            record_resolved(&mut data, &txid, height, Some(1_700_000_000), &outputs),
            3
        );
        assert!(
            data.pending.is_empty(),
            "resolving takes the txid off the pending list"
        );
        let recorded: std::collections::BTreeSet<u64> =
            data.outputs.iter().map(output_index).collect();
        assert_eq!(recorded.len(), 3);
        assert!(data
            .outputs
            .iter()
            .all(|o| o.txid == txid && o.height == height && !o.spent && o.amount_piconero > 0));

        // Recording the same transaction again adds nothing.
        assert_eq!(record_resolved(&mut data, &txid, height, None, &outputs), 0);
        assert_eq!(data.outputs.len(), 3);
    }

    /// A rescan finds a transaction's outputs one block-scan result at a
    /// time; they're recorded per transaction, resolving it if it was
    /// pending, and a rescan over the same blocks again changes nothing.
    #[test]
    fn a_rescan_records_what_it_found_once_and_resolves_pending() {
        let (txid, height, outputs) = split_transaction();
        let (_, mut data) = temp_wallet("record-scanned", "spender");
        data.add_pending(&txid, 0);
        let found: Vec<ScannedOutput> = outputs
            .into_iter()
            .map(|output| ScannedOutput {
                height,
                timestamp: 1_700_000_000,
                output,
            })
            .collect();

        assert_eq!(record_scanned(&mut data, &found), 3);
        assert!(data.pending.is_empty());
        assert!(data
            .outputs
            .iter()
            .all(|o| o.txid == txid && o.height == height && o.timestamp == Some(1_700_000_000)));

        assert_eq!(record_scanned(&mut data, &found), 0);
        assert_eq!(data.outputs.len(), 3);
        assert_eq!(record_scanned(&mut data, &[]), 0);
    }

    /// Resolving a transaction this wallet sent dates its sent record, so
    /// `show_transfers` can place it.
    #[test]
    fn resolving_a_send_dates_its_sent_record() {
        let (txid, height, outputs) = split_transaction();
        let (_, mut data) = temp_wallet("date-sent", "spender");
        data.sent.push(file::SentRecord {
            txid: txid.clone(),
            account: 0,
            destinations: vec![],
            fee_piconero: 1,
            change_piconero: 2,
            height: None,
            timestamp: None,
        });
        record_resolved(&mut data, &txid, height, Some(42), &outputs);
        assert_eq!(
            (data.sent[0].height, data.sent[0].timestamp),
            (Some(height), Some(42))
        );
    }

    /// The ownership check is what splits the old shared ledger between
    /// wallets: each output belongs to exactly the wallet that received it.
    #[test]
    fn outputs_belong_to_the_wallet_that_received_them_only() {
        let (_, _, outputs) = split_transaction();
        let (spender_path, spender) = temp_wallet("owns-spender", "spender");
        let (merchant_path, merchant) = temp_wallet("owns-merchant", "merchant");
        let spender = WalletKeys::from_data(&spender, spender_path).unwrap();
        let merchant = WalletKeys::from_data(&merchant, merchant_path).unwrap();
        assert!(outputs.iter().all(|o| spender.owns(o)));
        assert!(outputs.iter().all(|o| !merchant.owns(o)));
    }

    /// Key images are what `freeze`/`sweep_single` name outputs by, so
    /// distinct outputs must get distinct, stable ones.
    #[test]
    fn key_images_are_stable_and_distinct() {
        let (_, _, outputs) = split_transaction();
        let (path, data) = temp_wallet("key-images", "spender");
        let keys = WalletKeys::from_data(&data, path).unwrap();
        let images: std::collections::BTreeSet<[u8; 32]> =
            outputs.iter().map(|o| keys.key_image(o)).collect();
        assert_eq!(images.len(), outputs.len());
        assert_eq!(keys.key_image(&outputs[0]), keys.key_image(&outputs[0]));
    }

    #[tokio::test]
    async fn freezing_and_marking_spent_change_exactly_one_output() {
        let (txid, height, outputs) = split_transaction();
        let (path, data) = temp_wallet("freeze", "spender");
        let keys = WalletKeys::from_data(&data, path.clone()).unwrap();
        keys.update(|data| Ok(record_resolved(data, &txid, height, None, &outputs)))
            .await
            .unwrap();

        let image = keys.key_image(&outputs[1]);
        assert!(keys.set_frozen(image, true).await.unwrap());
        assert!(
            !keys.set_frozen([7; 32], true).await.unwrap(),
            "unknown key image"
        );
        assert!(keys
            .set_spent(outputs[2].index_on_blockchain(), true)
            .await
            .unwrap());

        let reloaded = keys
            .outputs(&WalletFile::load(&path).unwrap().data)
            .unwrap();
        for output in reloaded {
            let index = output.output.index_in_transaction();
            assert_eq!(
                output.frozen,
                index == outputs[1].index_in_transaction(),
                "frozen flag of output {index}"
            );
            assert_eq!(
                output.spent,
                index == outputs[2].index_in_transaction(),
                "spent flag of output {index}"
            );
        }
    }

    #[test]
    fn a_wallet_file_refuses_to_be_created_twice_and_round_trips() {
        let (path, data) = temp_wallet("round-trip", "spender");
        assert!(
            WalletFile::create(&path, data.clone()).is_err(),
            "an existing wallet file is never replaced"
        );
        let reloaded = WalletFile::load(&path).unwrap().data;
        assert_eq!(reloaded.address, data.address);
        assert_eq!(
            serde_json::to_value(&reloaded).unwrap(),
            serde_json::to_value(&data).unwrap()
        );
    }

    /// Concurrent read-modify-writes from many tasks each land - the lock
    /// is what stops one silently overwriting another.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_updates_are_never_lost() {
        let (path, _) = temp_wallet("concurrent", "spender");
        let tasks: Vec<_> = (0..16)
            .map(|i| {
                let path = path.clone();
                tokio::spawn(async move {
                    WalletFile::update(&path, |data| {
                        data.add_pending(&format!("tx{i}"), 0);
                        Ok(())
                    })
                    .await
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        assert_eq!(WalletFile::load(&path).unwrap().data.pending.len(), 16);
    }

    /// A held lock is reported with who holds it, and the busy handler's
    /// choice is followed: retry asks again, cancel fails, wait blocks until
    /// the holder lets go.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_held_lock_names_its_holder_and_lets_the_caller_choose() {
        use crate::file::{BusyChoice, BusyHandler, LockHolder};
        use std::sync::{Arc, Mutex};

        let (path, _) = temp_wallet("busy", "spender");
        let held = WalletFile::lock(&path).await.unwrap();

        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let answers = Arc::new(Mutex::new(vec![BusyChoice::Cancel, BusyChoice::Retry]));
        let handler: BusyHandler = {
            let seen = seen.clone();
            Arc::new(move |holder: &LockHolder| {
                seen.lock().unwrap().push(holder.to_string());
                answers.lock().unwrap().pop().unwrap()
            })
        };
        let error = WalletFile::lock_with(&path, &handler)
            .await
            .err()
            .expect("cancelled");
        assert!(matches!(error, WalletError::Locked(_)), "{error}");
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "retry asks again while it's still held");
        assert!(
            seen[0].contains(&format!("pid {}", std::process::id())),
            "names the holder: {}",
            seen[0]
        );
        assert!(seen[0].contains("spender.db is locked by"), "{}", seen[0]);

        let wait: BusyHandler = Arc::new(|_: &LockHolder| BusyChoice::Wait);
        let release = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            drop(held);
        });
        WalletFile::lock_with(&path, &wait)
            .await
            .expect("waiting gets the lock once it's released");
        release.await.unwrap();
    }

    /// The old shared layout splits into one file per wallet, each output
    /// going to its owner, pending txids to the sender.
    #[test]
    fn legacy_shared_files_split_into_one_file_per_wallet() {
        let (txid, height, outputs) = split_transaction();
        let dir = std::env::temp_dir().join(format!("cli-wallet-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let credentials = |name: &str| {
            let keys = fixture_wallet(name);
            serde_json::json!({ "address": keys.address, "private_spend_key": keys.private_spend_key_hex, "private_view_key": keys.private_view_key_hex, "role": name })
        };
        let wallets = serde_json::json!({
            "_comment": "stagenet only",
            "spender": credentials("spender"),
            "merchant": credentials("merchant"),
            "faucet_used": "https://stagenet-faucet.xmr-tw.org/",
        });
        let mut entries: Vec<Value> = outputs
            .iter()
            .map(|o| serde_json::json!({ "txid": txid, "height": height, "serialized_output_hex": hex::encode(o.serialize()), "amount_piconero": 0, "spent": false }))
            .collect();
        entries.push(serde_json::json!({ "txid": "ab".repeat(32), "height": null, "serialized_output_hex": null, "amount_piconero": 5, "spent": false }));
        std::fs::write(dir.join("wallets.json"), wallets.to_string()).unwrap();
        std::fs::write(
            dir.join("ledger.json"),
            serde_json::json!({ "entries": entries }).to_string(),
        )
        .unwrap();

        let out = dir.join("wallets");
        let report = migrate_legacy(
            &dir.join("wallets.json"),
            &dir.join("ledger.json"),
            &out,
            "spender",
        )
        .unwrap();
        assert!(report.unowned_outputs.is_empty());

        let spender = WalletFile::load(out.join("spender.db")).unwrap().data;
        let merchant = WalletFile::load(out.join("merchant.db")).unwrap().data;
        assert_eq!(spender.outputs.len(), 3);
        assert!(
            spender.outputs.iter().all(|o| o.amount_piconero > 0),
            "amounts come from the outputs themselves"
        );
        assert_eq!(spender.pending.len(), 1);
        assert_eq!(
            spender.extra["faucet_used"],
            "https://stagenet-faucet.xmr-tw.org/"
        );
        assert_eq!(spender.extra["_comment"], "stagenet only");
        assert!(merchant.outputs.is_empty() && merchant.pending.is_empty());
        assert_eq!(merchant.extra["role"], "merchant");
        assert!(!merchant.extra.contains_key("faucet_used"));
        assert!(
            migrate_legacy(
                &dir.join("wallets.json"),
                &dir.join("ledger.json"),
                &out,
                "spender"
            )
            .is_err(),
            "never overwrites"
        );
    }

    #[test]
    fn seeds_restore_the_keys_they_came_from() {
        for (_, network) in NETWORKS {
            let generated = generate_credentials(network, "English").unwrap();
            let phrase = generated.mnemonic.clone().unwrap();
            assert_eq!(phrase.split_whitespace().count(), 25);
            assert_eq!(
                credentials_from_seed(network, &phrase).unwrap().address,
                generated.address
            );
            assert_eq!(
                legacy_seed_for(&generated.private_spend_key_hex, "English").unwrap(),
                phrase
            );
            assert_eq!(
                credentials_from_spend_key_hex(network, &generated.private_spend_key_hex)
                    .unwrap()
                    .address,
                generated.address
            );
        }
        assert!(generate_credentials(Network::Stagenet, "Klingon").is_err());
    }

    /// One seed is one wallet on either network, with each network's own
    /// addresses: `5` on stagenet, `9` on testnet.
    #[test]
    fn a_seed_restores_to_the_network_asked_for() {
        let stagenet = generate_credentials(Network::Stagenet, "English").unwrap();
        let phrase = stagenet.mnemonic.clone().unwrap();
        let testnet = credentials_from_seed(Network::Testnet, &phrase).unwrap();
        assert!(stagenet.address.starts_with('5'), "{}", stagenet.address);
        assert!(testnet.address.starts_with('9'), "{}", testnet.address);
        assert_eq!(
            testnet.private_spend_key_hex,
            stagenet.private_spend_key_hex
        );
        assert_eq!(testnet.private_view_key_hex, stagenet.private_view_key_hex);
    }

    /// A testnet wallet file opens as testnet - its addresses, its nodes,
    /// no decoy snapshot - and is refused where stagenet is expected, and
    /// the other way round.
    #[test]
    fn a_testnet_wallet_file_opens_only_on_testnet() {
        let dir = std::env::temp_dir().join(format!("cli-wallet-testnet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let credentials = generate_credentials(Network::Testnet, "English").unwrap();
        let path = dir.join("t.db");
        WalletFile::create(
            &path,
            WalletData::new(Network::Testnet, credentials.clone()),
        )
        .unwrap();
        assert_eq!(WalletFile::load(&path).unwrap().data.network, "testnet");

        let ctx = WalletCtx::for_network(Network::Testnet);
        let resolved = ResolvedWallet::open(&ctx, &path).unwrap();
        assert_eq!(resolved.network, Network::Testnet);
        assert!(resolved.node_urls.iter().all(|url| url.ends_with(":28089")));
        assert_eq!(resolved.decoy_distribution_path, None);
        let keys = resolved.keys().unwrap();
        assert_eq!(keys.network(), Network::Testnet);
        assert_eq!(keys.address(), credentials.address);
        assert!(
            keys.subaddress(0, 1).starts_with('B'),
            "a testnet subaddress"
        );
        assert!(
            keys.integrated_address([1; 8]).starts_with('A'),
            "a testnet integrated address"
        );

        let error = ResolvedWallet::open(&WalletCtx::default(), &path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("is a testnet wallet, not stagenet"),
            "{error}"
        );
        let (stagenet_path, _) = temp_wallet("testnet-refused", "spender");
        let error = ResolvedWallet::open(&ctx, &stagenet_path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("is a stagenet wallet, not testnet"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(stagenet_path.parent().unwrap());
    }

    /// Every key here is plaintext, so a mainnet wallet is never written,
    /// nor one for a network nobody's heard of; the file keeps what it had.
    #[test]
    fn a_wallet_file_for_mainnet_or_an_unknown_network_is_refused() {
        let (path, data) = temp_wallet("mainnet-refused", "spender");
        for network in ["mainnet", "regtest"] {
            let mut file = WalletFile::load(&path).unwrap();
            file.data.network = network.to_string();
            let error = file.save().unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("{network} isn't a network this wallet works on")),
                "{error}"
            );
            assert_eq!(WalletFile::load(&path).unwrap().data.network, data.network);

            let mut data = data.clone();
            data.network = network.to_string();
            let other = path.with_file_name(format!("{network}.db"));
            assert!(WalletFile::create(&other, data).is_err());
            assert!(!other.exists(), "a refused create leaves no file");
        }
        assert_eq!(network_name(Network::Mainnet), "mainnet");
        assert!(parse_network("mainnet").is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
