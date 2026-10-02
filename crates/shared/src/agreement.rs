//! What the engine reports in `/status` about whether a network's nodes
//! agree on the chain it recorded (docs/chain_agreement.md). The engine
//! fills it in; monokulo shows it to operators on the status page.

use serde::{Deserialize, Serialize};

/// One network's agreement. Absent with a single node.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Agreement {
    /// `checking`, `agreed`, `holding` or `degraded`.
    pub state: String,
    /// While holding: `contested`, `outvoted` or `unverified`.
    pub hold: Option<String>,
    /// When the current state (or hold) began (Unix seconds).
    pub since: Option<i64>,
    /// The highest block an order may newly settle on; `None`: no ceiling.
    pub ceiling: Option<u64>,
    /// When the nodes were last asked.
    pub checked_at: Option<i64>,
    /// The recorded block the last check decided on.
    pub height: Option<u64>,
    /// Each node's vote on that block.
    pub nodes: Vec<NodeVote>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeVote {
    /// `host:port`, as on the node's own row.
    pub node: String,
    /// `agrees`, `disagrees` or `no answer`.
    pub vote: String,
    /// Left out of pinning: the other nodes outvote its chain.
    pub excluded: bool,
}
