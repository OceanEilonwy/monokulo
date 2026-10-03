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
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod fake {
    use super::*;
    use parking_lot::Mutex;
    use std::collections::HashMap;

    #[derive(Clone)]
    struct FakeBlock {
        hash: String,
        txs: Vec<Transaction>,
        timestamp: u64,
        /// Its blob and difficulties, for a block a test made with
        /// `pow::test_chain`: what proof-of-work checking reads.
        proof: Option<FakeProof>,
    }

    #[derive(Clone)]
    struct FakeProof {
        blob: Vec<u8>,
        difficulty: u128,
        cumulative_difficulty: u128,
    }

    /// A real block for [`FakeDaemonClient::replace_from`].
    pub struct ProofBlock {
        pub height: u64,
        pub hash: String,
        pub timestamp: u64,
        pub txs: Vec<Transaction>,
        pub blob: Vec<u8>,
        pub difficulty: u128,
        pub cumulative_difficulty: u128,
    }

    /// A fixed, arbitrary epoch and block spacing for `FakeBlock`'s own
    /// default timestamp formula (`FAKE_GENESIS_TIMESTAMP + height *
    /// FAKE_BLOCK_TIME_SECS`) - deterministic and monotonic by
    /// construction, so any test that never calls `set_block_timestamp`
    /// still gets *some* well-defined, increasing value per height for
    /// free. Neither number needs to match anything real; `set_block_timestamp`
    /// exists specifically for a test that wants to script something else.
    const FAKE_GENESIS_TIMESTAMP: u64 = 1_700_000_000;
    const FAKE_BLOCK_TIME_SECS: u64 = 120;

    fn default_fake_timestamp(height: u64) -> u64 {
        FAKE_GENESIS_TIMESTAMP + height * FAKE_BLOCK_TIME_SECS
    }

    #[derive(Default)]
    struct State {
        blocks: HashMap<u64, FakeBlock>,
        height: u64,
        /// When set, `get_height` reports this instead of `height` - models a
        /// public endpoint's height-reporting backend being temporarily ahead of
        /// whatever backend actually serves block data, an inconsistency observed
        /// live against a real public node (see `run_scan_tick`'s bootstrap
        /// branch). Deliberately independent of `height`, which `push_block` still
        /// uses for its own auto-increment - the two heights need to be able to
        /// disagree for the scenario this exists to model.
        height_override: Option<u64>,
        mempool: Vec<Transaction>,
        /// txid -> location, explicitly tracked rather than derived from `blocks`
        /// so a test can put a tx "in the pool" without it ever having been mined,
        /// or mark it fully gone (`NotFound`) after a reorg.
        tx_locations: HashMap<String, TxLocation>,
        key_image_status: HashMap<String, KeyImageStatus>,
        /// Weights a test gives blocks in place of their real size, so a
        /// block can be "large" without the bytes (`set_block_weight`).
        weights: HashMap<u64, u64>,
    }

    #[derive(Default)]
    pub struct FakeDaemonClient {
        state: Mutex<State>,
        /// Defaults to `false` (see `#[derive(Default)]`) - `new()` immediately sets
        /// it `true`, so every existing test that never touches this stays online as
        /// before. Lets a test simulate this node going unreachable (for exercising
        /// `daemon_fallback::FallbackDaemonClient` failover) without a second,
        /// differently-implemented test double - see `set_online`.
        online: std::sync::atomic::AtomicBool,
    }

    /// The id of a whole transaction, as the real chain names it: what the
    /// fake's blocks and pool carry their transactions under, and what tests
    /// name them by.
    ///
    /// (A pruned transaction, as the real client fetches them, doesn't hash
    /// to its id.)
    pub fn tx_id_hex(tx: &Transaction) -> String {
        use monero::cryptonote::hash::Hashable as _;
        hex::encode(tx.hash().to_bytes())
    }

    impl FakeDaemonClient {
        pub fn new() -> Self {
            let client = Self::default();
            client
                .online
                .store(true, std::sync::atomic::Ordering::Relaxed);
            client
        }

        /// Simulates this node going unreachable (`online = false`) or coming back
        /// (`online = true`). While offline, every `MoneroDaemonClient` method
        /// returns `Err` instead of consulting the scripted chain state, which is
        /// otherwise left completely untouched - flipping back online resumes
        /// exactly where the scripted chain was left, nothing lost or reset.
        pub fn set_online(&self, online: bool) {
            self.online
                .store(online, std::sync::atomic::Ordering::Relaxed);
        }

        fn require_online(&self) -> Result<(), DaemonError> {
            if self.online.load(std::sync::atomic::Ordering::Relaxed) {
                Ok(())
            } else {
                Err(DaemonError::Request("fake daemon is offline".to_owned()))
            }
        }

        /// Mines a new block at the next height, containing `txs`. Each tx is
        /// recorded as `InBlock(height)`, matching what a real node would report.
        pub fn push_block(&self, hash: &str, txs: Vec<Transaction>) -> u64 {
            let mut state = self.state.lock();
            let height = state.height + 1;
            for tx in &txs {
                state
                    .tx_locations
                    .insert(tx_id_hex(tx), TxLocation::InBlock(height));
            }
            state.blocks.insert(
                height,
                FakeBlock {
                    hash: hash.to_owned(),
                    txs,
                    timestamp: default_fake_timestamp(height),
                    proof: None,
                },
            );
            state.height = height;
            state.height_override = None; // a real block now backs this height - any prior lag is resolved
            height
        }

        /// Places a block at an explicit height, rather than at "one past the last
        /// one pushed". The only way to script a chain that includes height 0:
        /// `push_block` starts counting at 1, so the genesis-divergence branch of
        /// reorg handling (no common ancestor to re-anchor to) is otherwise
        /// unreachable from a test. Also useful for building a chain around a gap.
        pub fn seed_block_at(&self, height: u64, hash: &str, txs: Vec<Transaction>) {
            let mut state = self.state.lock();
            for tx in &txs {
                state
                    .tx_locations
                    .insert(tx_id_hex(tx), TxLocation::InBlock(height));
            }
            state.blocks.insert(
                height,
                FakeBlock {
                    hash: hash.to_owned(),
                    txs,
                    timestamp: default_fake_timestamp(height),
                    proof: None,
                },
            );
            state.height = state.height.max(height);
        }

        /// Overrides a block's timestamp after the fact (the block must
        /// already exist - `push_block`/`seed_block_at` it first). Only a
        /// test about block times needs this; every other test gets a
        /// deterministic, monotonic default for free and never needs to
        /// call it.
        pub fn set_block_timestamp(&self, height: u64, timestamp: u64) {
            let mut state = self.state.lock();
            if let Some(block) = state.blocks.get_mut(&height) {
                block.timestamp = timestamp;
            }
        }

        /// Makes `get_height` report one past the last real block, without that
        /// block actually existing - models a public endpoint's height-reporting
        /// backend being briefly ahead of its block-serving backend. See
        /// `height_override`'s doc comment.
        pub fn advance_height_without_a_block(&self) {
            let mut state = self.state.lock();
            state.height_override = Some(state.height + 1);
        }

        /// Makes `get_height` report an arbitrary height, however far from the
        /// blocks this fake can actually serve. The generalisation of
        /// `advance_height_without_a_block`, for the case where the discrepancy is
        /// not a one-block lag but a daemon simply asserting something untrue - see
        /// `docs/DESIGN.md` §7.7 on what the scanner does and does not take on
        /// trust from its configured node.
        pub fn report_height(&self, height: u64) {
            self.state.lock().height_override = Some(height);
        }

        pub fn set_mempool(&self, txs: Vec<Transaction>) {
            let mut state = self.state.lock();
            for tx in &txs {
                state
                    .tx_locations
                    .entry(tx_id_hex(tx))
                    .or_insert(TxLocation::InPool);
            }
            state.mempool = txs;
        }

        /// Simulates a reorg: replaces every block from `from_height` to the
        /// current tip with `new_blocks` (each `(hash, txs)`), re-tagging every
        /// previously-known tx that isn't in one of the new blocks as `NotFound`
        /// (the caller then decides, via `set_key_image_status`, whether that's a
        /// "still propagating" or "proven double-spend" situation).
        pub fn reorg_from(&self, from_height: u64, new_blocks: Vec<(&str, Vec<Transaction>)>) {
            let mut state = self.state.lock();
            let old_txids: Vec<String> = state
                .blocks
                .iter()
                .filter(|(h, _)| **h >= from_height)
                .flat_map(|(_, b)| b.txs.iter().map(tx_id_hex))
                .collect();
            state.blocks.retain(|h, _| *h < from_height);

            let mut height = from_height - 1;
            let mut new_txids = Vec::new();
            for (hash, txs) in new_blocks {
                height += 1;
                for tx in &txs {
                    let id = tx_id_hex(tx);
                    state
                        .tx_locations
                        .insert(id.clone(), TxLocation::InBlock(height));
                    new_txids.push(id);
                }
                state.blocks.insert(
                    height,
                    FakeBlock {
                        hash: hash.to_owned(),
                        txs,
                        timestamp: default_fake_timestamp(height),
                        proof: None,
                    },
                );
            }
            state.height = height;

            for txid in old_txids {
                if !new_txids.contains(&txid) {
                    state.tx_locations.insert(txid, TxLocation::NotFound);
                }
            }
        }

        pub fn set_key_image_status(&self, key_image_hex: &str, status: KeyImageStatus) {
            self.state
                .lock()
                .key_image_status
                .insert(key_image_hex.to_owned(), status);
        }

        /// Has block `height`'s header report `weight` bytes, whatever its
        /// transactions add up to: a block too large to fetch whole, without
        /// the bytes.
        pub fn set_block_weight(&self, height: u64, weight: u64) {
            self.state.lock().weights.insert(height, weight);
        }

        /// Replaces every block from `from` up with `blocks` (real ones, from
        /// `pow::test_chain`), as [`Self::reorg_from`] does: the node's chain
        /// now ends with them. A transaction no longer in a block is gone.
        pub fn replace_from(&self, from: u64, blocks: Vec<ProofBlock>) {
            let mut state = self.state.lock();
            let old_txids: Vec<String> = state
                .blocks
                .iter()
                .filter(|(h, _)| **h >= from)
                .flat_map(|(_, b)| b.txs.iter().map(tx_id_hex))
                .collect();
            state.blocks.retain(|h, _| *h < from);
            let mut new_txids = Vec::new();
            let mut top = from.saturating_sub(1);
            for block in blocks {
                for tx in &block.txs {
                    let id = tx_id_hex(tx);
                    state
                        .tx_locations
                        .insert(id.clone(), TxLocation::InBlock(block.height));
                    new_txids.push(id);
                }
                top = block.height;
                state.blocks.insert(
                    block.height,
                    FakeBlock {
                        hash: block.hash,
                        txs: block.txs,
                        timestamp: block.timestamp,
                        proof: Some(FakeProof {
                            blob: block.blob,
                            difficulty: block.difficulty,
                            cumulative_difficulty: block.cumulative_difficulty,
                        }),
                    },
                );
            }
            state.height = top;
            state.height_override = None;
            for txid in old_txids {
                if !new_txids.contains(&txid) {
                    state.tx_locations.insert(txid, TxLocation::NotFound);
                }
            }
        }

        /// Has block `height`'s header claim `difficulty`, whatever it was
        /// mined to: a node lying about the one thing a block's id doesn't
        /// commit to.
        pub fn set_claimed_difficulty(&self, height: u64, difficulty: u128) {
            if let Some(proof) = self
                .state
                .lock()
                .blocks
                .get_mut(&height)
                .and_then(|b| b.proof.as_mut())
            {
                proof.difficulty = difficulty;
            }
        }

        /// A block known only by its id, at a `RandomX` key height below a
        /// test chain: `get_block_hash` answers with it.
        pub fn seed_key_block(&self, height: u64, hash: &str) {
            self.state.lock().blocks.insert(
                height,
                FakeBlock {
                    hash: hash.to_owned(),
                    txs: Vec::new(),
                    timestamp: default_fake_timestamp(height),
                    proof: None,
                },
            );
        }

        pub fn drop_from_mempool(&self, tx: &Transaction) {
            let mut state = self.state.lock();
            state.mempool.retain(|t| tx_id_hex(t) != tx_id_hex(tx));
            state
                .tx_locations
                .insert(tx_id_hex(tx), TxLocation::NotFound);
        }
    }

    #[async_trait::async_trait]
    impl MoneroDaemonClient for FakeDaemonClient {
        async fn get_height(&self) -> Result<u64, DaemonError> {
            self.require_online()?;
            let state = self.state.lock();
            Ok(state.height_override.unwrap_or(state.height))
        }

        /// The height and the tip block's id under one lock, as a real
        /// node's single answer. No id while the reported height has no
        /// block behind it (`height_override`).
        async fn get_tip(&self) -> Result<ChainTip, DaemonError> {
            self.require_online()?;
            let state = self.state.lock();
            let height = state.height_override.unwrap_or(state.height);
            Ok(ChainTip {
                height,
                hash: state.blocks.get(&height).map(|b| b.hash.clone()),
            })
        }

        async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
            self.require_online()?;
            self.state
                .lock()
                .blocks
                .get(&height)
                .map(|b| b.hash.clone())
                .ok_or_else(|| DaemonError::Request(format!("no block at height {height}")))
        }

        /// A consistent snapshot: every block and its parent's id under one
        /// lock, as a real node's single `get_blocks.bin` answer would be.
        async fn get_chain_blocks(
            &self,
            start_height: u64,
            count: u64,
        ) -> Result<Vec<ChainBlock>, DaemonError> {
            self.require_online()?;
            let state = self.state.lock();
            let mut out = Vec::new();
            for height in start_height..start_height.saturating_add(count) {
                let Some(block) = state.blocks.get(&height) else {
                    break;
                };
                let prev_hash = height
                    .checked_sub(1)
                    .and_then(|p| state.blocks.get(&p))
                    .map_or_default(|b| b.hash.clone());
                out.push(ChainBlock {
                    height,
                    hash: block.hash.clone(),
                    prev_hash,
                    timestamp: block.timestamp,
                    txs: block.txs.iter().map(ScanTx::of).collect(),
                    txids: block.txs.iter().map(tx_id_hex).collect(),
                    wire_bytes: block
                        .txs
                        .iter()
                        .map(|tx| monero::consensus::encode::serialize(tx).len() as u64)
                        .sum(),
                });
            }
            if out.is_empty() {
                return Err(DaemonError::Request(format!(
                    "no block at height {start_height}"
                )));
            }
            Ok(out)
        }

        /// Headers from the scripted chain itself, as a real node answers
        /// for headers without reading any block's transactions.
        async fn get_chain_headers(
            &self,
            start_height: u64,
            count: u64,
        ) -> Result<Vec<ChainHeader>, DaemonError> {
            self.require_online()?;
            let state = self.state.lock();
            let mut out = Vec::new();
            for height in start_height..start_height.saturating_add(count) {
                let Some(block) = state.blocks.get(&height) else {
                    break;
                };
                let prev_hash = height
                    .checked_sub(1)
                    .and_then(|p| state.blocks.get(&p))
                    .map_or_default(|b| b.hash.clone());
                let real: u64 = block
                    .txs
                    .iter()
                    .map(|tx| monero::consensus::encode::serialize(tx).len() as u64)
                    .sum();
                out.push(ChainHeader {
                    height,
                    hash: block.hash.clone(),
                    prev_hash,
                    timestamp: block.timestamp,
                    weight: Some(state.weights.get(&height).copied().unwrap_or(real)),
                    tx_count: Some(block.txs.len() as u64),
                });
            }
            if out.is_empty() {
                return Err(DaemonError::Request(format!(
                    "no block at height {start_height}"
                )));
            }
            Ok(out)
        }

        async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
            self.require_online()?;
            Ok(self.state.lock().mempool.iter().map(tx_id_hex).collect())
        }

        /// The pool's transactions and their serialized size, against the
        /// floor of the penalty-free zone.
        async fn get_pool_outlook(&self) -> Result<Option<PoolOutlook>, DaemonError> {
            self.require_online()?;
            let state = self.state.lock();
            let bytes = state
                .mempool
                .iter()
                .map(|tx| monero::consensus::encode::serialize(tx).len())
                .sum::<usize>();
            Ok(Some(PoolOutlook {
                txs: u64::try_from(state.mempool.len()).unwrap_or(u64::MAX),
                bytes: Some(u64::try_from(bytes).unwrap_or(u64::MAX)),
                penalty_free: PoolOutlook::FULL_REWARD_ZONE,
            }))
        }

        async fn get_block_blob(&self, height: u64) -> Result<Vec<u8>, DaemonError> {
            self.require_online()?;
            self.state
                .lock()
                .blocks
                .get(&height)
                .and_then(|b| b.proof.as_ref())
                .map(|p| p.blob.clone())
                .ok_or_else(|| DaemonError::Request(format!("no block blob at height {height}")))
        }

        async fn get_difficulty_headers(
            &self,
            start_height: u64,
            count: u64,
        ) -> Result<Vec<DifficultyHeader>, DaemonError> {
            self.require_online()?;
            let state = self.state.lock();
            let mut out = Vec::new();
            for height in start_height..start_height.saturating_add(count) {
                let Some(block) = state.blocks.get(&height) else {
                    break;
                };
                let Some(proof) = &block.proof else {
                    break;
                };
                let prev_hash = height
                    .checked_sub(1)
                    .and_then(|p| state.blocks.get(&p))
                    .map_or_default(|b| b.hash.clone());
                out.push(DifficultyHeader {
                    height,
                    hash: block.hash.clone(),
                    prev_hash,
                    timestamp: block.timestamp,
                    difficulty: proof.difficulty,
                    cumulative_difficulty: proof.cumulative_difficulty,
                });
            }
            if out.is_empty() {
                return Err(DaemonError::Request(format!(
                    "no headers from height {start_height}"
                )));
            }
            Ok(out)
        }

        /// From the pool or a block, as a real node finds either.
        async fn get_transactions_with_ids(
            &self,
            txids: &[String],
        ) -> Result<Vec<FetchedTx>, DaemonError> {
            self.require_online()?;
            let state = self.state.lock();
            Ok(state
                .mempool
                .iter()
                .chain(state.blocks.values().flat_map(|block| &block.txs))
                .map(|tx| (tx_id_hex(tx), tx))
                .filter(|(txid, _)| txids.contains(txid))
                .map(|(txid, tx)| FetchedTx {
                    txid,
                    tx: tx.clone(),
                })
                .collect())
        }

        async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
            self.require_online()?;
            Ok(self
                .state
                .lock()
                .tx_locations
                .get(txid)
                .copied()
                .unwrap_or(TxLocation::NotFound))
        }

        /// Every transaction asked about, in one answer, as the real
        /// client's batched lookup gives.
        async fn locate_transactions(
            &self,
            txids: &[String],
        ) -> Result<HashMap<String, TxLocation>, DaemonError> {
            self.require_online()?;
            let state = self.state.lock();
            Ok(txids
                .iter()
                .map(|txid| {
                    let location = state
                        .tx_locations
                        .get(txid)
                        .copied()
                        .unwrap_or(TxLocation::NotFound);
                    (txid.clone(), location)
                })
                .collect())
        }

        async fn is_key_image_spent(
            &self,
            key_images: &[String],
        ) -> Result<Vec<KeyImageStatus>, DaemonError> {
            self.require_online()?;
            let state = self.state.lock();
            Ok(key_images
                .iter()
                .map(|ki| {
                    state
                        .key_image_status
                        .get(ki)
                        .copied()
                        .unwrap_or(KeyImageStatus::Unspent)
                })
                .collect())
        }
    }
}
