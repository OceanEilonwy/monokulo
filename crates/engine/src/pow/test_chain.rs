//! Real Monero blocks for tests: real blobs, ids, RandomX keys and proofs
//! of work, at difficulties small enough to mine in a few hashes.
//!
//! A chain starts with blocks that claim difficulty 1, which any hash
//! meets, so nothing is mined for them: the window an anchor takes. With
//! 735 of them a minute apart, the blocks after need difficulty 2, and
//! those are mined for real, so a block with a wrong proof is as easy to
//! make as a right one.

use monero::consensus::encode::{serialize, VarInt};
use monero::cryptonote::hash::Hashable;

use super::hasher::Hasher;
use super::{check_hash, seed_height, ProvenBlock, Window, DIFFICULTY_BLOCKS};

/// One block of a [`TestChain`].
#[derive(Clone, Debug)]
pub struct TestBlock {
    pub height: u64,
    pub id: [u8; 32],
    pub prev_id: [u8; 32],
    pub timestamp: u64,
    /// The difficulty it claims (1 for an unmined block) or was mined to.
    pub difficulty: u128,
    pub cumulative_difficulty: u128,
    pub blob: Vec<u8>,
    pub txs: Vec<monero::Transaction>,
}

impl TestBlock {
    pub fn id_hex(&self) -> String {
        hex::encode(self.id)
    }

    pub fn proven(&self) -> ProvenBlock {
        ProvenBlock {
            height: self.height,
            id: self.id,
            timestamp: self.timestamp,
            cumulative_difficulty: self.cumulative_difficulty,
        }
    }
}

/// Consecutive real blocks.
#[derive(Clone)]
pub struct TestChain {
    hasher: Hasher,
    blocks: Vec<TestBlock>,
    /// Seconds between mined blocks.
    pub spacing: u64,
}

/// The RandomX key for a key height below a chain's first block: made up,
/// but the same for every chain, so forks share it.
pub fn made_up_key(height: u64) -> [u8; 32] {
    let mut input = b"test chain key ".to_vec();
    input.extend_from_slice(&height.to_le_bytes());
    monero::cryptonote::hash::keccak_256(&input)
}

/// A coinbase naming `height`, paying nothing.
fn coinbase(height: u64) -> Vec<u8> {
    let mut tx = Vec::new();
    tx.extend(serialize(&VarInt(2))); // version
    tx.extend(serialize(&VarInt(height + 60))); // unlock time
    tx.extend(serialize(&VarInt(1))); // one input
    tx.push(0xff); // txin_gen
    tx.extend(serialize(&VarInt(height)));
    tx.extend(serialize(&VarInt(0))); // no outputs
    tx.extend(serialize(&VarInt(0))); // no extra
    tx.push(0); // RCTTypeNull
    tx
}

fn block_blob(
    height: u64,
    timestamp: u64,
    prev_id: [u8; 32],
    txs: &[monero::Transaction],
) -> monero::Block {
    let mut blob = Vec::new();
    blob.extend(serialize(&VarInt(16))); // major version
    blob.extend(serialize(&VarInt(16))); // minor version
    blob.extend(serialize(&VarInt(timestamp)));
    blob.extend_from_slice(&prev_id);
    blob.extend_from_slice(&0u32.to_le_bytes());
    blob.extend(coinbase(height));
    blob.extend(serialize(&VarInt(txs.len() as u64)));
    for tx in txs {
        blob.extend_from_slice(&tx.hash().0);
    }
    #[allow(
        clippy::expect_used,
        reason = "test support: the blob is built right above"
    )]
    monero::consensus::deserialize(&blob).expect("a test block decodes")
}

impl TestChain {
    /// `count` unmined blocks claiming difficulty 1, ending at height `top`
    /// at time `last_time`, `spacing` seconds apart, the first one's parent
    /// made up.
    pub fn unmined(hasher: Hasher, top: u64, count: u64, spacing: u64, last_time: u64) -> Self {
        Self::unmined_claiming(hasher, top, count, spacing, last_time, 1)
    }

    /// [`Self::unmined`], each block claiming `difficulty`: above 1, their
    /// proofs of work don't meet it.
    pub fn unmined_claiming(
        hasher: Hasher,
        top: u64,
        count: u64,
        spacing: u64,
        last_time: u64,
        difficulty: u128,
    ) -> Self {
        let mut chain = TestChain {
            hasher,
            blocks: Vec::new(),
            spacing,
        };
        let first = top + 1 - count;
        let mut prev_id = made_up_key(first - 1);
        for height in first..=top {
            let timestamp = last_time - (top - height) * spacing;
            let block = block_blob(height, timestamp, prev_id, &[]);
            let id = block.id().0;
            chain.blocks.push(TestBlock {
                height,
                id,
                prev_id,
                timestamp,
                difficulty,
                cumulative_difficulty: u128::from(height) * difficulty,
                blob: serialize(&block),
                txs: Vec::new(),
            });
            prev_id = id;
        }
        chain
    }

    /// The window [`Self::unmined`] makes for an anchor at `top`: 735
    /// blocks a minute apart, the last at `last_time`; the next block
    /// needs difficulty 2.
    pub fn anchored_at(hasher: Hasher, top: u64, last_time: u64) -> Self {
        Self::unmined(hasher, top, DIFFICULTY_BLOCKS as u64, 60, last_time)
    }

    pub fn blocks(&self) -> &[TestBlock] {
        &self.blocks
    }

    #[allow(clippy::expect_used, reason = "test support: a chain is never empty")]
    pub fn tip(&self) -> &TestBlock {
        self.blocks.last().expect("a test chain has blocks")
    }

    pub fn get(&self, height: u64) -> Option<&TestBlock> {
        let first = self.blocks.first()?.height;
        self.blocks
            .get(usize::try_from(height.checked_sub(first)?).ok()?)
    }

    /// The RandomX key block `height` is hashed with: the id of the block
    /// at its key height, or a made-up one below the chain.
    pub fn key(&self, height: u64) -> [u8; 32] {
        let at = seed_height(height);
        self.get(at).map_or_else(|| made_up_key(at), |b| b.id)
    }

    /// The window the next block is judged against.
    pub fn window(&self) -> Window {
        let skip = self.blocks.len().saturating_sub(DIFFICULTY_BLOCKS);
        Window::new(self.blocks[skip..].iter().map(TestBlock::proven))
    }

    /// Mines the next block holding `txs`, `spacing` seconds after the tip.
    pub fn mine(&mut self, txs: Vec<monero::Transaction>) -> &TestBlock {
        let timestamp = self.tip().timestamp + self.spacing;
        self.push(txs, timestamp, true)
    }

    /// Mines `n` empty blocks.
    pub fn mine_empty(&mut self, n: usize) {
        for _ in 0..n {
            self.mine(Vec::new());
        }
    }

    /// The next block, holding `txs`, with a proof of work that doesn't
    /// meet its difficulty.
    pub fn forge(&mut self, txs: Vec<monero::Transaction>) -> &TestBlock {
        let timestamp = self.tip().timestamp + self.spacing;
        self.push(txs, timestamp, false)
    }

    /// The next block at `timestamp`, its proof meeting its difficulty or
    /// (`valid` false) not.
    #[allow(clippy::expect_used, reason = "test support: hashing can't fail here")]
    pub fn push(
        &mut self,
        txs: Vec<monero::Transaction>,
        timestamp: u64,
        valid: bool,
    ) -> &TestBlock {
        let window = self.window();
        let difficulty = window.next_difficulty();
        let tip = self.tip().clone();
        let height = tip.height + 1;
        let key = self.key(height);
        let mut block = block_blob(height, timestamp, tip.id, &txs);
        // A nonce at a time: at these difficulties the first or second
        // usually does, and every hash costs about 16 ms.
        for nonce in 0u32.. {
            block.header.nonce = nonce;
            let hash = self
                .hasher
                .hash_blocking(&key, vec![block.serialize_hashable()])
                .expect("a test chain's hashes");
            if check_hash(&hash[0], difficulty) == valid {
                break;
            }
        }
        self.blocks.push(TestBlock {
            height,
            id: block.id().0,
            prev_id: tip.id,
            timestamp,
            difficulty,
            cumulative_difficulty: tip.cumulative_difficulty + difficulty,
            blob: serialize(&block),
            txs,
        });
        self.tip()
    }

    /// This chain up to `height`: where a fork starts.
    pub fn truncated(&self, height: u64) -> TestChain {
        let mut chain = self.clone();
        chain.blocks.retain(|b| b.height <= height);
        chain
    }

    /// Puts this chain's blocks from `from` up into `node` (replacing what
    /// it had there and above), with their blobs and difficulties for
    /// proof checking, and the made-up keys below the chain.
    pub fn install(&self, node: &crate::daemon::fake::FakeDaemonClient, from: u64) {
        let blocks: Vec<&TestBlock> = self.blocks.iter().filter(|b| b.height >= from).collect();
        node.replace_from(
            from,
            blocks
                .iter()
                .map(|b| crate::daemon::fake::ProofBlock {
                    height: b.height,
                    hash: b.id_hex(),
                    timestamp: b.timestamp,
                    txs: b.txs.clone(),
                    blob: b.blob.clone(),
                    difficulty: b.difficulty,
                    cumulative_difficulty: b.cumulative_difficulty,
                })
                .collect(),
        );
        let first = self.blocks.first().map_or(0, |b| b.height);
        let mut keys: Vec<u64> = blocks.iter().map(|b| seed_height(b.height)).collect();
        keys.dedup();
        for key_height in keys.into_iter().filter(|h| *h < first) {
            node.seed_key_block(key_height, &hex::encode(made_up_key(key_height)));
        }
    }
}
