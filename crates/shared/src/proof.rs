//! What the engine reports in `/status` about proof-of-work checking
//! (docs/proof_of_work.md): where each network's proven chain stands, what
//! it was anchored on, and what each node was found to serve. The engine
//! fills these in; monokulo shows them to operators on the status page.

use serde::{Deserialize, Serialize};

/// One network's checking. Absent while checking is off.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProofStatus {
    pub state: ProofState,
    /// One sentence on where it stands, for an operator.
    pub summary: String,
    pub anchor: Option<AnchorStatus>,
    /// The newest proven block.
    pub proven_height: Option<u64>,
    pub proven_hash: Option<String>,
    /// The highest block orders may newly settle on.
    pub ceiling: Option<u64>,
    pub nodes: Vec<NodeProof>,
    /// Blocks whose proof of work was checked since the engine started.
    pub blocks_checked: u64,
    pub hashing: Option<Hashing>,
    /// When the last check finished (Unix seconds).
    pub checked_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofState {
    /// Waiting for enough nodes to agree on an anchor: nothing settles.
    #[default]
    Anchoring,
    /// Following the heaviest valid chain; orders settle on proven blocks.
    Following,
    /// Proven blocks above what the nodes serve can't be followed (a reorg
    /// deeper than the anchor, or every node caught): nothing new settles
    /// until an operator acts.
    Held,
}

/// The block the proven chain started from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorStatus {
    pub height: u64,
    pub hash: String,
    /// How many of `nodes` configured nodes gave it.
    pub agreed: u32,
    pub nodes: u32,
    pub anchored_at: i64,
}

/// What a node was found to serve at the last check.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeProof {
    /// `host:port`, as on the node's own row.
    pub node: String,
    pub height: Option<u64>,
    pub verdict: NodeVerdict,
    /// What was found, for a caught node or one that couldn't be asked.
    pub detail: Option<String>,
    /// Left out of scanning: never pinned or asked.
    pub excluded: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeVerdict {
    /// Not looked at yet.
    #[default]
    Unknown,
    /// Its tip is on the proven chain (at or below the proven tip).
    OnChain,
    /// Its chain goes on past the proven tip, still being checked.
    Ahead,
    /// It serves a valid chain with less work than the proven one.
    Lighter,
    /// It left the proven chain further back than can be followed.
    Diverged,
    /// It served a block that breaks the rules.
    Caught,
    /// It didn't answer.
    Unreachable,
}

/// The RandomX hashing behind the checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hashing {
    /// RandomX's JIT compiler in use (otherwise interpreted, much slower).
    pub jit: bool,
    pub mean_hash_ms: f64,
    pub mean_key_build_ms: f64,
    /// RandomX keys held now, 256 MiB each.
    pub keys_held: u64,
}
