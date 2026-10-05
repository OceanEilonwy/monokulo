//! `MoneroDaemonClient`: the calls the chain scanner makes to `monerod`.
//!
//! A trait, so reorg/double-spend logic (`src/scanner.rs`) can be tested
//! against a deterministic scripted fake instead of a live node.
//!
//! See `docs/DESIGN.md` §7.1.
//!
//! The trait holds what the engine asks of a node and nothing else: the real
//! implementation is `daemon_rpc::RpcDaemonClient`, behind
//! `daemon_fallback::FallbackDaemonClient`; the scripted fake is here.

use monero::Transaction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyImageStatus {
    Unspent,
    SpentInBlockchain,
    SpentInPool,
    /// Configured nodes disagree; neither "spent" nor "unspent" is established.
    Disputed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxLocation {
    InBlock(u64),
    InPool,
    NotFound,
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("daemon request failed: {0}")]
    Request(String),
    /// The node didn't answer in time: the link may be slower than
    /// estimated, so a smaller request may well succeed.
    #[error("daemon request timed out: {0}")]
    TimedOut(String),
    /// The answer was larger than the cap: a smaller request may succeed.
    #[error("daemon response too large: {0}")]
    TooLarge(String),
}

impl DaemonError {
    /// Whether asking for less might succeed where this failed: a timeout
    /// or an answer over the size cap (`docs/engine_scaling.md` section 2).
    pub fn asks_for_less(&self) -> bool {
        matches!(self, Self::TimedOut(_) | Self::TooLarge(_))
    }
}

/// What a node says about itself (monerod's `get_info`). Only the network
/// it's on is used: the settings API refuses a node saved for the wrong
/// network, and `/status` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonInfo {
    /// `"mainnet"`, `"stagenet"`, `"testnet"`, `"fakechain"`, or
    /// [`DaemonInfo::UNKNOWN`] when the node didn't say.
    pub nettype: String,
    /// The height of the node's tip block, when it said: the same value
    /// as [`MoneroDaemonClient::get_height`], from the same answer, so a
    /// caller that wants both asks once.
    pub height: Option<u64>,
}

impl DaemonInfo {
    pub const UNKNOWN: &'static str = "unknown";

    pub fn unknown() -> Self {
        Self {
            nettype: Self::UNKNOWN.to_owned(),
            height: None,
        }
    }

    /// The network the node is on, when it's one the engine knows. A
    /// `fakechain` (a regtest node) or a node that didn't say is `None`:
    /// never taken as being on the wrong network.
    pub fn network(&self) -> Option<monero::Network> {
        match self.nettype.as_str() {
            "mainnet" => Some(monero::Network::Mainnet),
            "stagenet" => Some(monero::Network::Stagenet),
            "testnet" => Some(monero::Network::Testnet),
            _ => None,
        }
    }
}

/// Requests made to one endpoint of a node (a path, or a JSON-RPC method),
/// and the bytes they cost ([`MoneroDaemonClient::rpc_stats`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct EndpointStats {
    pub endpoint: String,
    /// Requests sent, answered or not.
    pub requests: u64,
    /// Request body bytes.
    pub bytes_sent: u64,
    /// Response body bytes read.
    pub bytes_received: u64,
}

/// One block as the node has it, contents and identity together
/// ([`MoneroDaemonClient::get_chain_blocks`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainBlock {
    pub height: u64,
    /// The block's id, as `get_block_hash` reports it (lowercase hex).
    pub hash: String,
    /// The parent block's id; empty for the genesis block.
    pub prev_hash: String,
    /// The miner's timestamp (unix seconds). Consensus lets it run up to two
    /// hours ahead of real time.
    pub timestamp: u64,
    /// The block's transactions, without the coinbase, as the scan keeps
    /// them (`docs/engine_scaling.md` section 3).
    pub txs: Vec<ScanTx>,
    /// The id of each of `txs` (lowercase hex), in order. They come with
    /// the block: a pruned transaction can't be hashed to its id
    /// (`shared::monero_tx`).
    pub txids: Vec<String>,
    /// The block's size as it came from the node, in bytes: what block
    /// requests are sized and the block cache is counted by.
    pub wire_bytes: u64,
}

impl ChainBlock {
    pub fn header(&self) -> ChainHeader {
        ChainHeader {
            height: self.height,
            hash: self.hash.clone(),
            prev_hash: self.prev_hash.clone(),
            timestamp: self.timestamp,
            weight: Some(self.wire_bytes),
            tx_count: Some(self.txs.len() as u64),
        }
    }
}

/// What the scan keeps of one transaction (`docs/engine_scaling.md` section 3).
///
/// What a view key reads ([`ScanInput`]: the outputs, `extra`, the unlock
/// time, the encrypted amounts and commitments) and the inputs' key images,
/// which a payment is recorded with.
///
/// Ring members and everything else are dropped once the transaction is
/// decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanTx {
    pub input: crate::key_custody::ScanInput,
    pub key_images: Vec<[u8; 32]>,
}

impl ScanTx {
    pub fn of(tx: &Transaction) -> Self {
        Self {
            input: crate::key_custody::ScanInput::of(tx),
            key_images: tx
                .prefix
                .inputs
                .iter()
                .filter_map(|input| match input {
                    monero::blockdata::transaction::TxIn::ToKey {
                        k_image,
                        amount: _,
                        key_offsets: _,
                    } => Some(k_image.image.to_bytes()),
                    monero::blockdata::transaction::TxIn::Gen { height: _ } => None,
                })
                .collect(),
        }
    }
}

/// A block's identity without its contents
/// ([`MoneroDaemonClient::get_chain_headers`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainHeader {
    pub height: u64,
    pub hash: String,
    /// The parent block's id; empty for the genesis block.
    pub prev_hash: String,
    pub timestamp: u64,
    /// The block's weight in bytes, when the node said: whether it is
    /// fetched whole or a page of transactions at a time depends on it
    /// (`docs/engine_scaling.md` section 4).
    pub weight: Option<u64>,
    /// How many transactions it holds besides the coinbase, when the node
    /// said.
    pub tx_count: Option<u64>,
}

/// A block's identity and its transactions' ids in order, without their
/// bodies ([`MoneroDaemonClient::get_block_outline`]).
///
/// A block too large to fetch whole is scanned from this, a page of
/// transactions at a time (`docs/engine_scaling.md` section 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockOutline {
    pub height: u64,
    pub hash: String,
    /// The parent block's id; empty for the genesis block.
    pub prev_hash: String,
    pub timestamp: u64,
    /// Its transactions' ids (lowercase hex), coinbase excluded, in the
    /// block's order.
    pub txids: Vec<String>,
}

/// A block header with what the node says of its difficulty
/// ([`MoneroDaemonClient::get_difficulty_headers`]): taken on the nodes'
/// word only for an anchor's window (`docs/proof_of_work.md`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DifficultyHeader {
    pub height: u64,
    pub hash: String,
    /// The parent block's id; empty for the genesis block.
    pub prev_hash: String,
    pub timestamp: u64,
    pub difficulty: u128,
    pub cumulative_difficulty: u128,
}

/// The node's whole pool, as the next block would be mined from it
/// ([`MoneroDaemonClient::get_pool_outlook`]): for the engine page only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolOutlook {
    /// Transactions in the pool.
    pub txs: u64,
    /// Their size in bytes, when the node said.
    pub bytes: Option<u64>,
    /// The block weight a miner can fill without its reward being cut:
    /// the median of recent blocks, and never under
    /// [`PoolOutlook::FULL_REWARD_ZONE`].
    pub penalty_free: u64,
}

impl PoolOutlook {
    /// monerod's `CRYPTONOTE_BLOCK_GRANTED_FULL_REWARD_ZONE_V5`: the
    /// penalty-free zone's floor, in bytes of weight.
    pub const FULL_REWARD_ZONE: u64 = 300_000;
}

/// The node's tip ([`MoneroDaemonClient::get_tip`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainTip {
    pub height: u64,
    /// The tip block's id, when the node gave it with the height.
    pub hash: Option<String>,
}

/// A transaction with its id, as a node gave it
/// ([`MoneroDaemonClient::get_transactions_with_ids`]). The transaction may
/// be pruned, which is why the id comes with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchedTx {
    pub txid: String,
    pub tx: Transaction,
}

/// The mempool's transaction ids, or why they couldn't be read.
pub type PoolAnswer = Result<Vec<String>, DaemonError>;

/// Which configured node a client's answers come from.
///
/// Nodes can be at different heights or on different forks, so what
/// was read from one isn't kept for use with another's answers (the
/// scan's block cache, `work::blocks`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NodeKey(pub usize);

#[async_trait::async_trait]
pub trait MoneroDaemonClient: Send + Sync {
    /// What this client has asked its node since it was built, by endpoint,
    /// busiest first: for `/status`, so what the engine costs a node can be
    /// seen rather than estimated. Nothing for a client that doesn't count.
    fn rpc_stats(&self) -> Vec<EndpointStats> {
        Vec::new()
    }

    /// The node every answer from this client comes from, or `None` if each
    /// call may go to a different one (a failover client that isn't
    /// pinned). A client of one node is always the same node.
    fn node(&self) -> Option<NodeKey> {
        Some(NodeKey::default())
    }

    /// What this client has measured of its node's link
    /// (`docs/engine_scaling.md` section 1), if it measures one.
    fn link(&self) -> Option<crate::link::LinkSnapshot> {
        None
    }

    /// What a request to the node costs, as measured (round trip, the
    /// node's own work per block, transfer rate), for sizing requests;
    /// `None` when this client doesn't measure.
    fn link_cost(&self) -> Option<crate::link::LinkCost> {
        None
    }

    /// How long [`Self::get_chain_blocks`] asking for `count` blocks may
    /// take: what the link's measurements say it needs, with room to spare.
    /// A client that doesn't measure gets the fixed floor.
    fn chain_blocks_timeout(&self, _count: u64) -> std::time::Duration {
        crate::link::MIN_TIMEOUT
    }

    /// How long an answer of about `bytes` bytes may take, by the same
    /// measure as [`Self::chain_blocks_timeout`]: for a large block's
    /// outline and its pages of transactions.
    fn transfer_timeout(&self, _bytes: u64) -> std::time::Duration {
        crate::link::MIN_TIMEOUT
    }

    /// The node announced a change to its pool (`docs/monero_zmq.md)`: the
    /// next poll asks it, rather than reusing an answer from just before.
    /// Nothing for a client that doesn't reuse answers.
    fn pool_changed(&self) {}

    async fn get_height(&self) -> Result<u64, DaemonError>;

    /// The tip's height and, when the node gives both in one answer, its
    /// id: with it, reorg detection needs no lookup while the recorded
    /// chain ends at the node's tip. The default knows only the height;
    /// `RpcDaemonClient` reads both from monerod's `/get_height`.
    async fn get_tip(&self) -> Result<ChainTip, DaemonError> {
        Ok(ChainTip {
            height: self.get_height().await?,
            hash: None,
        })
    }

    /// [`Self::get_tip`] and [`Self::get_mempool_txids`] together, for a
    /// round that needs both: `RpcDaemonClient` asks them in one request
    /// while the chain still ends at the tip it last saw. Two answers: one
    /// can fail without the other. The default asks twice.
    async fn get_tip_and_mempool(&self) -> (Result<ChainTip, DaemonError>, PoolAnswer) {
        (self.get_tip().await, self.get_mempool_txids().await)
    }

    /// What the node says about itself. The default says nothing
    /// ([`DaemonInfo::unknown`]), which every test double gets for free;
    /// `RpcDaemonClient` asks monerod.
    async fn get_info(&self) -> Result<DaemonInfo, DaemonError> {
        Ok(DaemonInfo::unknown())
    }
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError>;

    /// Up to `count` blocks from `start_height`, in height order: each with
    /// its id, its parent's id, its timestamp and its transactions (the
    /// coinbase excluded) under their ids, all from the same block. May be
    /// shorter than `count` (the node's tip, or its batch limit); empty only
    /// if nothing at `start_height` is available.
    ///
    /// Pairing a block's contents with its own id is the point: a scanner that
    /// reads a hash and the contents separately can record one block's
    /// payments under another block's hash if the chain moves in between.
    /// `RpcDaemonClient` reads both from the blobs of one `get_blocks.bin`
    /// answer.
    async fn get_chain_blocks(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainBlock>, DaemonError>;

    /// Up to `count` block headers from `start_height`, in height order:
    /// what [`Self::get_chain_blocks`] says about each block's identity,
    /// without its transactions. For recording blocks nobody needs scanned.
    /// May be shorter than `count`; empty only if nothing at `start_height`
    /// is available.
    ///
    /// The default reads whole blocks and drops their contents;
    /// `RpcDaemonClient` asks monerod for headers only.
    async fn get_chain_headers(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainHeader>, DaemonError> {
        Ok(self
            .get_chain_blocks(start_height, count)
            .await?
            .iter()
            .map(ChainBlock::header)
            .collect())
    }

    /// Block `height`'s identity and its transactions' ids, without their
    /// bodies: for a block too large for one [`Self::get_chain_blocks`]
    /// answer, whose transactions are then fetched a page at a time with
    /// [`Self::get_transactions_with_ids`]. `tx_count`, from its header,
    /// sizes the answer allowed.
    ///
    /// The default reads the whole block; `RpcDaemonClient` asks monerod's
    /// `get_block`, which sends the block without its transactions.
    async fn get_block_outline(
        &self,
        height: u64,
        tx_count: Option<u64>,
    ) -> Result<BlockOutline, DaemonError> {
        let _ = tx_count;
        let block = self
            .get_chain_blocks(height, 1)
            .await?
            .into_iter()
            .next()
            .filter(|block| block.height == height)
            .ok_or_else(|| DaemonError::Request(format!("no block at height {height}")))?;
        Ok(BlockOutline {
            height,
            hash: block.hash,
            prev_hash: block.prev_hash,
            timestamp: block.timestamp,
            txids: block.txids,
        })
    }

    /// The txids in the mempool, without their bodies, so a scanner that
    /// has already seen most of the pool only fetches what's new (task 7.3).
    /// `RpcDaemonClient` follows the pool by its changes, asking monerod
    /// only for what entered and left since it last asked.
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError>;

    /// The node's whole pool and its penalty-free block weight, for the
    /// engine page. `None` from a client that can't say: the default.
    /// `RpcDaemonClient` asks monerod's `get_info` and
    /// `/get_transaction_pool_stats`.
    async fn get_pool_outlook(&self) -> Result<Option<PoolOutlook>, DaemonError> {
        Ok(None)
    }

    /// Block `height` as the node stores it: its header, coinbase and its
    /// transactions' ids, without the transactions (`docs/proof_of_work.md`).
    /// Nothing in it is taken on trust: its id and its proof of work are
    /// computed from it. `RpcDaemonClient` asks monerod's `get_block`.
    async fn get_block_blob(&self, height: u64) -> Result<Vec<u8>, DaemonError> {
        Err(DaemonError::Request(format!(
            "this client can't fetch block {height}'s blob"
        )))
    }

    /// Up to `count` headers from `start_height`, with what the node says
    /// of their difficulty: for an anchor's window, the one thing taken on
    /// the nodes' word (`docs/proof_of_work.md`). May be shorter than
    /// `count`; empty only if nothing at `start_height` is available.
    async fn get_difficulty_headers(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<DifficultyHeader>, DaemonError> {
        let _ = count;
        Err(DaemonError::Request(format!(
            "this client can't fetch difficulty headers from {start_height}"
        )))
    }

    /// Several transactions by txid, each with its id, in any order; ones
    /// the node doesn't have are left out. The id comes with each because
    /// the transaction may be pruned (`RpcDaemonClient` fetches them so),
    /// and then can't be hashed to it.
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<FetchedTx>, DaemonError>;

    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError>;

    /// A second opinion on a [`Self::locate_transaction`] answer of
    /// `NotFound`, for the one question where a node's *absence* answer has
    /// a permanent consequence: a payment whose transaction is nowhere, and
    /// whose inputs are spent, is voided as double-spent. Its own inputs are
    /// spent on every honest node too (its own transaction spent them), so
    /// the key-image check corroborates nothing about the absence. A client
    /// that knows more than one node (`daemon_fallback::FallbackDaemonClient`)
    /// asks every node and answers `NotFound` only when all that answered
    /// agree. The default, for a single node, has no second opinion to give
    /// (`None`): asking the same node again would cost a round trip for the
    /// same answer.
    async fn locate_transaction_corroborated(
        &self,
        txid: &str,
    ) -> Result<Option<TxLocation>, DaemonError> {
        let _ = txid;
        Ok(None)
    }

    /// Where several transactions are, in one round trip, for a client that
    /// can: a hint. A transaction missing from the answer (the client can't
    /// batch, or the node's answer didn't settle that one) is asked about
    /// with [`Self::locate_transaction`], which is where a non-answer becomes
    /// an error. The default can't batch and answers nothing, so every test
    /// double is asked one transaction at a time, as before.
    async fn locate_transactions(
        &self,
        _txids: &[String],
    ) -> Result<std::collections::HashMap<String, TxLocation>, DaemonError> {
        Ok(std::collections::HashMap::new())
    }

    /// A transaction and where it is, or `None` if the node has no record of
    /// it. The transaction may be pruned. The default asks twice;
    /// `RpcDaemonClient` once.
    async fn find_transaction(
        &self,
        txid: &str,
    ) -> Result<Option<(FetchedTx, TxLocation)>, DaemonError> {
        let location = self.locate_transaction(txid).await?;
        if location == TxLocation::NotFound {
            return Ok(None);
        }
        let fetched = self
            .get_transactions_with_ids(std::slice::from_ref(&txid.to_owned()))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                DaemonError::Request(format!("daemon placed {txid} but sent no transaction"))
            })?;
        Ok(Some((fetched, location)))
    }

    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError>;

    /// Like `is_key_image_spent`, but for a client that knows about more than one
    /// node (see `daemon_fallback::FallbackDaemonClient::is_key_image_spent_corroborated`)
    /// - corroborates the answer across all of them before ever affirming
    /// `SpentInBlockchain`, since a false positive here permanently voids a real
    /// payment (`docs/DESIGN.md` §7.7). The default here, inherited by every
    /// single-node client (`RpcDaemonClient`, every test double), simply delegates:
    /// with exactly one source of truth to consult, there is nothing to corroborate
    /// against, so behavior is unchanged from plain `is_key_image_spent`.
    async fn is_key_image_spent_corroborated(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.is_key_image_spent(key_images).await
    }
}

/// Test double for `MoneroDaemonClient`, scripted via a small timeline API.
///
/// Lets reorg/double-spend scenarios be constructed deterministically, without a
/// live or regtest `monerod` - see `docs/TESTING.md` §3 for why this matters
/// (reorgs are rare in production, so bugs here are exactly the kind that go
/// unnoticed for a long time otherwise).
#[cfg(any(test, feature = "fuzzing"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/internal/daemon/fake.rs"]
pub mod fake;
