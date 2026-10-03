//! Monero's proof-of-work rules, as monerod applies them to a block
//! (docs/proof_of_work.md): the RandomX seed a block is hashed with, the
//! difficulty it must meet (computed from the blocks before it, never taken
//! from a node), the check of its hash against that difficulty, and the
//! timestamp rules that keep the difficulty honest.
//!
//! Everything here is pure; [`hasher`] computes the RandomX hashes on a
//! thread of its own.

use std::collections::VecDeque;

pub mod hasher;
#[cfg(test)]
pub mod test_chain;

/// Seconds a block is meant to take (`DIFFICULTY_TARGET_V2`).
pub const DIFFICULTY_TARGET_SECS: u128 = 120;
/// Blocks whose work and timestamps set the difficulty (`DIFFICULTY_WINDOW`).
pub const DIFFICULTY_WINDOW: usize = 720;
/// The newest blocks left out of the difficulty (`DIFFICULTY_LAG`).
pub const DIFFICULTY_LAG: usize = 15;
/// Timestamps cut from each end once sorted (`DIFFICULTY_CUT`).
pub const DIFFICULTY_CUT: usize = 60;
/// Blocks before a block that its difficulty is computed from
/// (`DIFFICULTY_BLOCKS_COUNT`).
pub const DIFFICULTY_BLOCKS: usize = DIFFICULTY_WINDOW + DIFFICULTY_LAG;
/// Blocks whose median timestamp a new block's may not fall below
/// (`BLOCKCHAIN_TIMESTAMP_CHECK_WINDOW`).
pub const TIMESTAMP_CHECK_WINDOW: usize = 60;
/// How far ahead of the clock a block's timestamp may be
/// (`CRYPTONOTE_BLOCK_FUTURE_TIME_LIMIT`).
pub const FUTURE_TIME_LIMIT_SECS: u64 = 2 * 60 * 60;
/// Blocks per RandomX key (`SEEDHASH_EPOCH_BLOCKS`).
pub const SEEDHASH_EPOCH_BLOCKS: u64 = 2048;
/// Blocks after a key block before its key is used (`SEEDHASH_EPOCH_LAG`).
pub const SEEDHASH_EPOCH_LAG: u64 = 64;
/// The first block version hashed with RandomX (hard fork 12).
pub const RANDOMX_MAJOR_VERSION: u64 = 12;

/// The first block of `network` hashed with RandomX (hard fork 12, from
/// monerod's `hardforks.cpp`). Blocks before it used CryptoNight variants,
/// which this engine doesn't check: an anchor's window must start at or
/// after it.
pub fn randomx_fork_height(network: monero::Network) -> u64 {
    match network {
        monero::Network::Mainnet => 1_978_433,
        monero::Network::Testnet => 1_308_737,
        monero::Network::Stagenet => 454_721,
    }
}

/// The height of the block whose id is the RandomX key for block `height`
/// (`rx_seedheight`).
pub fn seed_height(height: u64) -> u64 {
    if height <= SEEDHASH_EPOCH_BLOCKS + SEEDHASH_EPOCH_LAG {
        0
    } else {
        (height - SEEDHASH_EPOCH_LAG - 1) & !(SEEDHASH_EPOCH_BLOCKS - 1)
    }
}

/// The difficulty of the block after `window`, monerod's `next_difficulty`:
/// `window` is the blocks before it, oldest first, as (timestamp,
/// cumulative difficulty), up to [`DIFFICULTY_BLOCKS`] of them. The newest
/// [`DIFFICULTY_LAG`] are left out, the timestamps sorted and
/// [`DIFFICULTY_CUT`] cut from each end; the cumulative difficulties are
/// read at the same positions *unsorted*, exactly as monerod does.
pub fn next_difficulty(window: &[(u64, u128)]) -> u128 {
    let window = &window[..window.len().min(DIFFICULTY_WINDOW)];
    let length = window.len();
    if length <= 1 {
        return 1;
    }
    let mut timestamps: Vec<u64> = window.iter().map(|(t, _)| *t).collect();
    timestamps.sort_unstable();
    let kept = DIFFICULTY_WINDOW - 2 * DIFFICULTY_CUT;
    let (begin, end) = if length <= kept {
        (0, length)
    } else {
        let begin = (length - kept).div_ceil(2);
        (begin, begin + kept)
    };
    let time_span = u128::from(timestamps[end - 1].saturating_sub(timestamps[begin]).max(1));
    let total_work = window[end - 1].1.saturating_sub(window[begin].1);
    // monerod computes this in 256 bits; a work over 2^121 would need a
    // hash rate the network is nowhere near, but saturate rather than wrap.
    match total_work.checked_mul(DIFFICULTY_TARGET_SECS) {
        Some(scaled) => scaled.div_ceil(time_span),
        None => u128::MAX,
    }
}

/// The median of `timestamps` (epee's `median`: the mean of the two middle
/// values for an even count, rounded down), or `None` for none.
pub fn median(timestamps: &[u64]) -> Option<u64> {
    let mut sorted = timestamps.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(sorted[n / 2]),
        _ => {
            let (a, b) = (sorted[n / 2 - 1], sorted[n / 2]);
            Some(a / 2 + b / 2 + (a % 2 + b % 2) / 2)
        }
    }
}

/// Whether a RandomX `hash` meets `difficulty`: read as a little-endian
/// 256-bit number, `hash × difficulty` must fit in 256 bits (monerod's
/// `check_hash`).
pub fn check_hash(hash: &[u8; 32], difficulty: u128) -> bool {
    if difficulty == 0 {
        return false;
    }
    let mut limbs = [0u64; 4];
    for (limb, bytes) in limbs.iter_mut().zip(hash.chunks_exact(8)) {
        let mut word = [0u8; 8];
        word.copy_from_slice(bytes);
        *limb = u64::from_le_bytes(word);
    }
    let factors = [difficulty as u64, (difficulty >> 64) as u64];
    // Schoolbook multiplication into six 64-bit limbs.
    let mut product = [0u128; 6];
    for (i, &limb) in limbs.iter().enumerate() {
        let mut carry = 0u128;
        for (j, &factor) in factors.iter().enumerate() {
            let sum = product[i + j] + u128::from(limb) * u128::from(factor) + carry;
            product[i + j] = sum & u128::from(u64::MAX);
            carry = sum >> 64;
        }
        let mut k = i + 2;
        while carry != 0 && k < product.len() {
            let sum = product[k] + carry;
            product[k] = sum & u128::from(u64::MAX);
            carry = sum >> 64;
            k += 1;
        }
    }
    product[4] == 0 && product[5] == 0
}

/// One block of the proven chain: what the next blocks' rules read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenBlock {
    pub height: u64,
    pub id: [u8; 32],
    pub timestamp: u64,
    pub cumulative_difficulty: u128,
}

/// The blocks a new block is judged against: up to [`DIFFICULTY_BLOCKS`]
/// before it, oldest first, ending at its parent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    blocks: VecDeque<ProvenBlock>,
}

impl Window {
    /// A window of `blocks`, oldest first and consecutive. Only the newest
    /// [`DIFFICULTY_BLOCKS`] are kept.
    pub fn new(blocks: impl IntoIterator<Item = ProvenBlock>) -> Self {
        let mut window = Self::default();
        for block in blocks {
            window.push(block);
        }
        window
    }

    /// The newest block: the parent of the next.
    pub fn tip(&self) -> Option<&ProvenBlock> {
        self.blocks.back()
    }

    /// Whether the window holds every block the next block's difficulty
    /// reads.
    pub fn is_full(&self) -> bool {
        self.blocks.len() == DIFFICULTY_BLOCKS
    }

    pub fn push(&mut self, block: ProvenBlock) {
        self.blocks.push_back(block);
        while self.blocks.len() > DIFFICULTY_BLOCKS {
            self.blocks.pop_front();
        }
    }

    /// The difficulty the next block must meet.
    pub fn next_difficulty(&self) -> u128 {
        let rows: Vec<(u64, u128)> = self
            .blocks
            .iter()
            .map(|b| (b.timestamp, b.cumulative_difficulty))
            .collect();
        next_difficulty(&rows)
    }

    /// The median timestamp the next block's may not fall below, once
    /// there are [`TIMESTAMP_CHECK_WINDOW`] blocks to take it from.
    pub fn median_timestamp(&self) -> Option<u64> {
        if self.blocks.len() < TIMESTAMP_CHECK_WINDOW {
            return None;
        }
        let recent: Vec<u64> = self
            .blocks
            .iter()
            .rev()
            .take(TIMESTAMP_CHECK_WINDOW)
            .map(|b| b.timestamp)
            .collect();
        median(&recent)
    }
}

/// A block as a node sent it, decoded: what its proof is checked from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub height: u64,
    pub id: [u8; 32],
    pub prev_id: [u8; 32],
    pub timestamp: u64,
    pub major_version: u64,
    /// What RandomX hashes: the header, the transactions' Merkle root and
    /// their count (`get_block_hashing_blob`).
    pub pow_input: Vec<u8>,
}

/// Why a block wasn't accepted.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    #[error("block {height} could not be decoded: {reason}")]
    Undecodable { height: u64, reason: String },
    #[error("asked for block {height}, the node sent block {sent}")]
    WrongHeight { height: u64, sent: u64 },
    #[error("block {height} has version {version}, older than RandomX")]
    NotRandomX { height: u64, version: u64 },
    #[error("block {height} does not follow the block before it")]
    DoesNotFollow { height: u64 },
    #[error("block {height}'s timestamp {timestamp} is before the median {median} of the {TIMESTAMP_CHECK_WINDOW} blocks before it")]
    TimestampBeforeMedian {
        height: u64,
        timestamp: u64,
        median: u64,
    },
    #[error("block {height}'s timestamp {timestamp} is more than two hours ahead of this machine's clock ({now})")]
    TimestampInFuture {
        height: u64,
        timestamp: u64,
        now: u64,
    },
    #[error("block {height}'s proof of work doesn't meet its difficulty {difficulty}")]
    ProofOfWork { height: u64, difficulty: u128 },
}

/// What a rejection says about the node that sent the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The block breaks a rule no honest node would serve: the node is
    /// caught.
    Invalid,
    /// The block may become acceptable later (its time hasn't come): asked
    /// again later, nothing held against the node.
    NotYet,
    /// The node's chain moved under us (another block at that height now):
    /// looked at again from the top.
    Moved,
}

impl Rejection {
    pub fn verdict(&self) -> Verdict {
        match self {
            Rejection::TimestampInFuture { .. } => Verdict::NotYet,
            Rejection::DoesNotFollow { .. } => Verdict::Moved,
            Rejection::Undecodable { .. }
            | Rejection::WrongHeight { .. }
            | Rejection::NotRandomX { .. }
            | Rejection::TimestampBeforeMedian { .. }
            | Rejection::ProofOfWork { .. } => Verdict::Invalid,
        }
    }

    pub fn height(&self) -> u64 {
        match self {
            Rejection::Undecodable { height, .. }
            | Rejection::WrongHeight { height, .. }
            | Rejection::NotRandomX { height, .. }
            | Rejection::DoesNotFollow { height }
            | Rejection::TimestampBeforeMedian { height, .. }
            | Rejection::TimestampInFuture { height, .. }
            | Rejection::ProofOfWork { height, .. } => *height,
        }
    }
}

/// Decodes block `height` from its blob: its id is computed here, never
/// taken from the node, and its coinbase must name `height`.
pub fn decode(height: u64, blob: &[u8]) -> Result<Candidate, Rejection> {
    let block: monero::Block =
        monero::consensus::deserialize(blob).map_err(|e| Rejection::Undecodable {
            height,
            reason: e.to_string(),
        })?;
    match block.miner_tx.prefix.inputs.first() {
        Some(monero::blockdata::transaction::TxIn::Gen { height: named }) if named.0 == height => {}
        Some(monero::blockdata::transaction::TxIn::Gen { height: named }) => {
            return Err(Rejection::WrongHeight {
                height,
                sent: named.0,
            })
        }
        _ => {
            return Err(Rejection::Undecodable {
                height,
                reason: "no coinbase input".to_string(),
            })
        }
    }
    Ok(Candidate {
        height,
        id: block.id().0,
        prev_id: block.header.prev_id.0,
        timestamp: block.header.timestamp.0,
        major_version: block.header.major_version.0,
        pow_input: block.serialize_hashable(),
    })
}

/// Checks everything about `candidate` but its hash, against the proven
/// `window` ending at its parent, at `now` (unix seconds): it must follow
/// the window's tip, be a RandomX block, and keep the timestamp rules.
/// Returns the difficulty its RandomX hash must then meet.
pub fn check_header(window: &Window, candidate: &Candidate, now: u64) -> Result<u128, Rejection> {
    let height = candidate.height;
    let follows = window
        .tip()
        .is_some_and(|tip| tip.height + 1 == height && tip.id == candidate.prev_id);
    if !follows {
        return Err(Rejection::DoesNotFollow { height });
    }
    if candidate.major_version < RANDOMX_MAJOR_VERSION {
        return Err(Rejection::NotRandomX {
            height,
            version: candidate.major_version,
        });
    }
    if candidate.timestamp > now.saturating_add(FUTURE_TIME_LIMIT_SECS) {
        return Err(Rejection::TimestampInFuture {
            height,
            timestamp: candidate.timestamp,
            now,
        });
    }
    if let Some(median) = window.median_timestamp() {
        if candidate.timestamp < median {
            return Err(Rejection::TimestampBeforeMedian {
                height,
                timestamp: candidate.timestamp,
                median,
            });
        }
    }
    Ok(window.next_difficulty())
}

/// `candidate` with its RandomX `hash`, against `difficulty`: the block
/// that joins the proven chain, or why not.
pub fn accept(
    window: &Window,
    candidate: &Candidate,
    difficulty: u128,
    hash: &[u8; 32],
) -> Result<ProvenBlock, Rejection> {
    if !check_hash(hash, difficulty) {
        return Err(Rejection::ProofOfWork {
            height: candidate.height,
            difficulty,
        });
    }
    let parent = window.tip().map_or(0, |tip| tip.cumulative_difficulty);
    Ok(ProvenBlock {
        height: candidate.height,
        id: candidate.id,
        timestamp: candidate.timestamp,
        cumulative_difficulty: parent.saturating_add(difficulty),
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
