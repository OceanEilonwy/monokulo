//! The snapshot the engine page rebuilds from (`docs/engine_visualizer.md`):
//! what the database and the scheduler's memory say about one network at
//! one moment. The scan loop records one every
//! [`crate::activity::SNAPSHOT_EVERY`].

use shared::activity::{
    Database, Node, Pool, ReorgJob, ReorgPhase, Snapshot, StoreGroup, Webhooks,
};

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
    let window = Webhooks::BUCKET_SECS * i64::try_from(Webhooks::BUCKETS).unwrap_or(i64::MAX);
    let since = now.saturating_sub(window);
    let facts = db
        .run(Class::Scanner, move |s| {
            s.activity_facts(network, now, tip, Snapshot::GROUPS, since)
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
        webhooks: Webhooks {
            due: facts.webhooks_due,
            sent: buckets(&facts.delivered_at, now),
        },
        database: Database {
            queued: [Class::Scanner, Class::Webhook, Class::Admin]
                .map(|class| u64::try_from(db.queued(class)).unwrap_or(u64::MAX)),
            capacity: u64::try_from(crate::store::db::QUEUE_CAPACITY).unwrap_or(u64::MAX),
            completed: metrics.completed,
            max_queue_wait_us: metrics.max_queue_wait_us,
            max_run_us: metrics.max_run_us,
        },
        nodes,
    })
}

/// Deliveries made in each [`Webhooks::BUCKET_SECS`] of the
/// [`Webhooks::BUCKETS`] before `now`, oldest first.
fn buckets(delivered_at: &[i64], now: i64) -> Vec<u32> {
    let mut sent = vec![0u32; Webhooks::BUCKETS];
    for at in delivered_at {
        let ago = now.saturating_sub(*at).max(0) / Webhooks::BUCKET_SECS;
        if let Some(slot) = usize::try_from(ago)
            .ok()
            .and_then(|ago| Webhooks::BUCKETS.checked_sub(ago + 1))
        {
            sent[slot] = sent[slot].saturating_add(1);
        }
    }
    sent
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn deliveries_fall_into_ten_second_buckets_oldest_first() {
        let now = 1_000;
        let sent = buckets(
            &[now, now - 9, now - 10, now - 299, now - 300, now + 5],
            now,
        );
        assert_eq!(sent.len(), Webhooks::BUCKETS);
        assert_eq!(
            sent[Webhooks::BUCKETS - 1],
            3,
            "now, 9 s ago, and a clock skewed ahead"
        );
        assert_eq!(sent[Webhooks::BUCKETS - 2], 1, "10 s ago");
        assert_eq!(sent[0], 1, "299 s ago; 300 s ago is outside");
        assert_eq!(sent.iter().sum::<u32>(), 5);
    }
}
