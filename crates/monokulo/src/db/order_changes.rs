//! The POS orders each write changed, as the database itself reports them.
//!
//! Migration 0039's trigger calls `pos_order_changed(connection_id,
//! order_id)` whenever a statement changes what an order's live stream
//! shows. SQLite can't reach this process's memory from SQL, so that
//! function is defined here, on the writing connection, and only notes the
//! order: the change isn't visible to the read connections until its
//! transaction commits. A commit hook keeps what was noted, a rollback
//! hook drops it, and [`super::Database::write`] wakes the orders' streams
//! once its job (and so every commit in it) has finished.

use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::functions::FunctionFlags;
use rusqlite::Connection;

use super::{ConnectionId, OrderId};

/// The orders changed on one connection.
#[derive(Default)]
pub(super) struct ChangedOrders {
    /// Changed by the transaction still open, if any.
    uncommitted: Vec<(ConnectionId, OrderId)>,
    /// Changed by transactions that have committed, not yet announced.
    committed: Vec<(ConnectionId, OrderId)>,
}

impl ChangedOrders {
    /// The committed changes, each order once, for announcing.
    pub(super) fn take_committed(&mut self) -> Vec<(ConnectionId, OrderId)> {
        std::mem::take(&mut self.committed)
    }
}

/// Defines `pos_order_changed` on `conn` and hooks its commits and
/// rollbacks; the returned record fills as its writes commit.
pub(super) fn record(conn: &Connection) -> rusqlite::Result<Arc<Mutex<ChangedOrders>>> {
    let changed = Arc::new(Mutex::new(ChangedOrders::default()));
    let noted = Arc::clone(&changed);
    // Not deterministic: each call has an effect beyond its result.
    conn.create_scalar_function(
        "pos_order_changed",
        2,
        FunctionFlags::SQLITE_UTF8,
        move |call| {
            let order = (call.get::<ConnectionId>(0)?, call.get::<OrderId>(1)?);
            let mut changed = noted.lock();
            if !changed.uncommitted.contains(&order) {
                changed.uncommitted.push(order);
            }
            Ok(rusqlite::types::Null)
        },
    )?;
    let committed = Arc::clone(&changed);
    conn.commit_hook(Some(move || {
        let mut changed = committed.lock();
        for order in std::mem::take(&mut changed.uncommitted) {
            if !changed.committed.contains(&order) {
                changed.committed.push(order);
            }
        }
        // Let the commit go ahead.
        false
    }))?;
    let rolled_back = Arc::clone(&changed);
    conn.rollback_hook(Some(move || rolled_back.lock().uncommitted.clear()))?;
    Ok(changed)
}
