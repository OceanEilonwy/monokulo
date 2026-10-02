//! What the engine reports in `/status` about its nodes' ZMQ announcements
//! (docs/monero_zmq.md): each publisher it listens to, and how often an
//! announcement started a scan pass early. The engine fills these in;
//! monokulo shows them to operators on the status page.

use serde::{Deserialize, Serialize};

/// One network's announcements. Absent for a network with no `zmq_pub`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Announcements {
    pub publishers: Vec<Publisher>,
    /// Pool passes an announcement started before their interval was up.
    pub pool_passes_woken: u64,
    /// Scan rounds an announcement started before their interval was up.
    pub rounds_woken: u64,
}

/// One node's publisher (`zmq_pub`), since the engine started listening
/// to it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Publisher {
    /// The node it belongs to, `host:port`, as on the node's own row.
    pub node: String,
    pub endpoint: String,
    pub connected: bool,
    /// When the current connection was made (Unix seconds).
    pub connected_since: Option<i64>,
    /// Times a connection was made: more than one means it was lost.
    pub connections: u64,
    pub pool_announcements: u64,
    pub block_announcements: u64,
    pub last_announcement_at: Option<i64>,
    /// Why the last connection failed or ended, and when.
    pub last_error: Option<String>,
    pub last_error_at: Option<i64>,
}
