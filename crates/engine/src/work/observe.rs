//! The snapshot the engine page rebuilds from (`docs/engine_visualizer.md`):
//! what the database and the scheduler's memory say about one network at
//! one moment. The scan loop records one every
//! [`crate::activity::SNAPSHOT_EVERY`].

use shared::activity::{Database, Node, Pool, ReorgJob, ReorgPhase, Snapshot, StoreGroup};

use super::ScanState;
use crate::scanner::ScannerError;
use crate::store::db::Class;
use crate::store::Db;

/// `network`'s snapshot as of `now` (Unix seconds), its nodes as given.
pub async fn snapshot(
    state: &ScanState,
    db: &Db,
    network: monero::Network,
    nodes: Vec<Node>,
    scan_chunk_memory_budget_mb: u32,
    now: i64,
) -> Result<Snapshot, ScannerError> {
    let tip = match state
        .mempool
        .last_tip
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        0 => None,
        tip => Some(tip),
    };
    let facts = db
        .run(Class::Scanner, move |s| {
            s.activity_facts(network, now, tip, Snapshot::GROUPS)
        })
        .await?;
    let (cached, cache_bytes) = state.blocks.carried_cache();
    let (pool_size, txids) = state.mempool.remembered(Snapshot::POOL_TXS);
    let metrics = db.metrics();
    let listed = u64::try_from(facts.groups.len()).unwrap_or(u64::MAX);
    Ok(Snapshot {
        round: state.activity().round(),
        tip,
        high_water: facts.high_water,
        groups: facts
            .groups
            .into_iter()
            .map(|(cursor, stores)| StoreGroup { cursor, stores })
            .collect(),
        more_groups: facts.all_groups.saturating_sub(listed),
        cached,
        cache_bytes,
        cache_budget_bytes: u64::from(scan_chunk_memory_budget_mb).saturating_mul(1024 * 1024),
        checkpoints: facts.checkpoints,
        reorg: facts.reorg.map(|(fork, collecting, candidates)| ReorgJob {
            fork,
            phase: if collecting {
                ReorgPhase::Collect
            } else {
                ReorgPhase::Process
            },
            candidates,
        }),
        pool: Pool {
            watched: state.mempool.watched(),
            size: u64::try_from(pool_size).unwrap_or(u64::MAX),
            txids,
        },
        recomputes_pending: facts.recomputes_pending,
        orders_due: facts.orders_due,
        database: Database {
            queued: [Class::Scanner, Class::Admin]
                .map(|class| u64::try_from(db.queued(class)).unwrap_or(u64::MAX)),
            capacity: u64::try_from(crate::store::db::QUEUE_CAPACITY).unwrap_or(u64::MAX),
            completed: metrics.completed,
            max_queue_wait_us: metrics.max_queue_wait_us,
            max_run_us: metrics.max_run_us,
        },
        nodes,
    })
}
