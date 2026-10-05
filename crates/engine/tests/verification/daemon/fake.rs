use super::{
    ChainBlock, ChainHeader, ChainTip, DaemonError, DifficultyHeader, FetchedTx, KeyImageStatus,
    MoneroDaemonClient, PoolOutlook, ScanTx, Transaction, TxLocation,
};
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
        let incoming: std::collections::HashSet<_> = txs.iter().map(tx_id_hex).collect();
        let removed: Vec<_> = state
            .mempool
            .iter()
            .map(tx_id_hex)
            .filter(|id| !incoming.contains(id))
            .collect();
        for id in removed {
            if state.tx_locations.get(&id) == Some(&TxLocation::InPool) {
                state.tx_locations.insert(id, TxLocation::NotFound);
            }
        }
        for tx in &txs {
            let entry = state
                .tx_locations
                .entry(tx_id_hex(tx))
                .or_insert(TxLocation::InPool);
            if *entry == TxLocation::NotFound {
                *entry = TxLocation::InPool;
            }
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
