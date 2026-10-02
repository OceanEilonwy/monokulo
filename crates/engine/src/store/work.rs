//! Durable state for the scanner's work units (docs/scanner_microtasks.md):
//! the reorg job, the recompute schedule, and rotation positions. Every
//! read here is a bounded page; every multi-row write is one transaction.

use super::{OrderId, TenantId};
use rusqlite::{params, OptionalExtension};

use super::{OrderPaymentRow, Result, Store, StoreError};

pub(crate) use shared::sqlite::Unsigned;

/// A block height as SQLite stores it. Heights never come near `i64::MAX`;
/// one that did is refused rather than wrapped negative.
pub fn sql_height(height: u64) -> Result<i64> {
    i64::try_from(height)
        .map_err(|e| StoreError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e))))
}

/// A list of ids as a JSON array, for `json_each` in SQL.
fn json_array<S: AsRef<str>>(ids: &[S]) -> String {
    serde_json::Value::from(ids.iter().map(|id| id.as_ref()).collect::<Vec<&str>>()).to_string()
}

/// Reads an unsigned column.
fn unsigned<T: TryFrom<i64>>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T> {
    Ok(row.get::<_, Unsigned<T>>(index)?.0)
}

/// How far a reorg job has got. Stored as `reorg_jobs.phase` plus its
/// cursor columns; see migration 0019 for the collection order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReorgPhase {
    /// Collecting confirmed payments at or above the fork, after this
    /// `(block_height, id)` position.
    CollectConfirmed { after_height: u64, after_id: i64 },
    /// Collecting unconfirmed payments, after this id.
    CollectUnconfirmed { after_id: i64 },
    /// Every candidate is collected; `reorg_work` holds what's left.
    Process,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReorgJob {
    pub network: String,
    pub fork_height: u64,
    pub phase: ReorgPhase,
    pub candidate_max_id: i64,
    pub created_at: i64,
}

/// What opening a job did, so the caller can log a deeper fork.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenedReorg {
    Created,
    /// A deeper fork than the open job's: its fork moved down to this one
    /// and collection restarted there.
    Deepened {
        from: u64,
    },
    /// The open job already covers this fork.
    Covered,
}

/// A candidate still to re-examine, with its retry count.
#[derive(Clone, Debug)]
pub struct ReorgCandidate {
    pub payment: OrderPaymentRow,
    pub attempts: u32,
}

/// A rotation position the scheduler keeps across restarts, one row per
/// network per position. Each is a type with its own value type, so a
/// position can't be read as something it isn't. The set is closed: one
/// type per rotation, never one per tenant.
pub trait Position {
    const KEY: &'static str;
    type Value: std::str::FromStr + ToString;
}

/// Positions by name.
pub mod position {
    use super::Position;

    /// The last catch-up group served (a tenant cursor height).
    pub struct CatchUpGroup;
    impl Position for CatchUpGroup {
        const KEY: &'static str = "catch_up_group";
        type Value = u64;
    }

    /// The last unconfirmed payment checked for having left the pool.
    pub struct VanishedPayments;
    impl Position for VanishedPayments {
        const KEY: &'static str = "vanished_payments";
        type Value = i64;
    }

    /// The last voided payment rechecked for a false double-spend (0: no
    /// pass in progress).
    pub struct VoidRecheck;
    impl Position for VoidRecheck {
        const KEY: &'static str = "void_recheck";
        type Value = i64;
    }

    /// When the last full void recheck pass began (unix time).
    pub struct VoidRecheckPassStarted;
    impl Position for VoidRecheckPassStarted {
        const KEY: &'static str = "void_recheck_pass_started";
        type Value = i64;
    }

    /// The last tenant whose orders' scanned range was brought up to date.
    pub struct ScanRange;
    impl Position for ScanRange {
        const KEY: &'static str = "scan_range";
        type Value = String;
    }
}

/// How long a failed reorg lookup waits before it is retried. The first two
/// retries are due at once (the scheduler still tries a candidate at most
/// once a round), so a blip costs nothing; after that the wait doubles from
/// one second, up to about four minutes.
pub fn reorg_retry_delay(attempts: u32) -> i64 {
    match attempts {
        0..=2 => 0,
        n => 1i64 << (n - 3).min(8),
    }
}

fn phase_from_row(phase: &str, after_height: u64, after_id: i64) -> rusqlite::Result<ReorgPhase> {
    Ok(match phase {
        "collect_confirmed" => ReorgPhase::CollectConfirmed {
            after_height,
            after_id,
        },
        "collect_unconfirmed" => ReorgPhase::CollectUnconfirmed { after_id },
        "process" => ReorgPhase::Process,
        other => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown reorg phase {other:?}").into(),
            ))
        }
    })
}

impl Store {
    /// Every row `sql` returns for `params`, each read by `read`. One place
    /// for a query's three ways to fail: the statement (a broken schema), its
    /// parameters (a value out of SQLite's range) and a row (a corrupted
    /// value).
    fn rows<T, P: rusqlite::Params>(
        &self,
        sql: &str,
        params: P,
        read: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>> {
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt
            .query_map(params, read)?
            .collect::<rusqlite::Result<Vec<T>>>()?;
        Ok(rows)
    }

    pub fn reorg_job(&self, network: monero::Network) -> Result<Option<ReorgJob>> {
        self.conn
            .query_row(
                "SELECT network, fork_height, phase, candidate_max_id, collect_after_height, collect_after_id,
                        created_at_utc
                 FROM reorg_jobs WHERE network = ?1",
                [shared::network::SqlNetwork(network)],
                |row| {
                    Ok(ReorgJob {
                        network: row.get(0)?,
                        fork_height: unsigned(row, 1)?,
                        phase: phase_from_row(&row.get::<_, String>(2)?, unsigned(row, 4)?, row.get(5)?)?,
                        candidate_max_id: row.get(3)?,
                        created_at: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Records a reorg forking at `fork_height`, or lowers the open job's
    /// fork to it. From this commit on, the network's orders can't newly
    /// settle (see `settlement_frozen`).
    pub fn open_reorg_job(
        &self,
        network: monero::Network,
        fork_height: u64,
        now: i64,
    ) -> Result<OpenedReorg> {
        self.in_transaction(|s| {
            let max_id: i64 = s.conn.query_row("SELECT COALESCE(MAX(id), 0) FROM order_payments", [], |r| r.get(0))?;
            match s.reorg_job(network)? {
                None => {
                    s.conn.execute(
                        "INSERT INTO reorg_jobs (network, fork_height, phase, candidate_max_id,
                             collect_after_height, collect_after_id, created_at_utc, updated_at_utc)
                         VALUES (?1, ?2, 'collect_confirmed', ?3, ?2, 0, ?4, ?4)",
                        params![shared::network::SqlNetwork(network), Unsigned(fork_height), max_id, now],
                    )?;
                    Ok(OpenedReorg::Created)
                }
                Some(job) if fork_height < job.fork_height => {
                    s.conn.execute(
                        "UPDATE reorg_jobs SET fork_height = ?2, phase = 'collect_confirmed', candidate_max_id = ?3,
                             collect_after_height = ?2, collect_after_id = 0, updated_at_utc = ?4
                         WHERE network = ?1",
                        params![shared::network::SqlNetwork(network), Unsigned(fork_height), max_id.max(job.candidate_max_id), now],
                    )?;
                    Ok(OpenedReorg::Deepened { from: job.fork_height })
                }
                Some(_) => Ok(OpenedReorg::Covered),
            }
        })
    }

    /// Adds up to `limit` candidates to `reorg_work` and moves the job's
    /// collection cursor past them, in one transaction. Returns the job's
    /// phase afterwards (`Process` once collection is complete).
    pub fn collect_reorg_candidates(
        &self,
        network: monero::Network,
        limit: usize,
        now: i64,
    ) -> Result<ReorgPhase> {
        self.in_transaction(|s| {
            let job = s.reorg_job(network)?.ok_or(StoreError::NotFound)?;
            let (ids, next) = match job.phase {
                ReorgPhase::CollectConfirmed { after_height, after_id } => {
                    let rows: Vec<(i64, u64)> = s.rows(
                        "SELECT op.id, op.block_height FROM order_payments op
                         JOIN orders o ON o.id = op.order_id JOIN tenants t ON t.id = o.tenant_id
                         WHERE op.block_height IS NOT NULL AND op.block_height >= ?2
                           AND (op.block_height > ?3 OR (op.block_height = ?3 AND op.id > ?4))
                           AND op.id <= ?5 AND t.network = ?1
                         ORDER BY op.block_height, op.id LIMIT ?6",
                        params![shared::network::SqlNetwork(network), Unsigned(job.fork_height), Unsigned(after_height), after_id, job.candidate_max_id, Unsigned(limit)],
                        |row| Ok((row.get(0)?, unsigned(row, 1)?)),
                    )?;
                    let next = match rows.last() {
                        Some(&(id, height)) if rows.len() == limit => {
                            ReorgPhase::CollectConfirmed { after_height: height, after_id: id }
                        }
                        _ => ReorgPhase::CollectUnconfirmed { after_id: 0 },
                    };
                    (rows.into_iter().map(|(id, _)| id).collect::<Vec<_>>(), next)
                }
                ReorgPhase::CollectUnconfirmed { after_id } => {
                    let ids: Vec<i64> = s.rows(
                        "SELECT op.id FROM order_payments op
                         JOIN orders o ON o.id = op.order_id JOIN tenants t ON t.id = o.tenant_id
                         WHERE op.block_height IS NULL AND op.id > ?2 AND op.id <= ?3 AND t.network = ?1
                         ORDER BY op.id LIMIT ?4",
                        params![shared::network::SqlNetwork(network), after_id, job.candidate_max_id, Unsigned(limit)],
                        |row| row.get(0),
                    )?;
                    let next = match ids.last() {
                        Some(&id) if ids.len() == limit => ReorgPhase::CollectUnconfirmed { after_id: id },
                        _ => ReorgPhase::Process,
                    };
                    (ids, next)
                }
                ReorgPhase::Process => return Ok(ReorgPhase::Process),
            };
            for id in ids {
                s.conn.execute(
                    "INSERT OR IGNORE INTO reorg_work (network, payment_id) VALUES (?1, ?2)",
                    params![shared::network::SqlNetwork(network), id],
                )?;
            }
            let (phase, after_height, after_id) = match next {
                ReorgPhase::CollectConfirmed { after_height, after_id } => ("collect_confirmed", after_height, after_id),
                ReorgPhase::CollectUnconfirmed { after_id } => ("collect_unconfirmed", 0, after_id),
                ReorgPhase::Process => ("process", 0, 0),
            };
            s.conn.execute(
                "UPDATE reorg_jobs SET phase = ?2, collect_after_height = ?3, collect_after_id = ?4, updated_at_utc = ?5
                 WHERE network = ?1",
                params![shared::network::SqlNetwork(network), phase, Unsigned(after_height), after_id, now],
            )?;
            Ok(next)
        })
    }

    /// Up to `limit` candidates whose retry time has come, oldest retry first.
    pub fn due_reorg_candidates(
        &self,
        network: monero::Network,
        now: i64,
        limit: usize,
    ) -> Result<Vec<ReorgCandidate>> {
        self.rows(
            "SELECT op.*, w.attempts AS reorg_attempts FROM reorg_work w
             JOIN order_payments op ON op.id = w.payment_id
             WHERE w.network = ?1 AND w.next_attempt_at_utc <= ?2
             ORDER BY w.next_attempt_at_utc, w.payment_id LIMIT ?3",
            params![shared::network::SqlNetwork(network), now, Unsigned(limit)],
            |row| {
                Ok(ReorgCandidate {
                    payment: Self::row_to_payment(row)?,
                    attempts: row.get::<_, Unsigned<u32>>("reorg_attempts")?.0,
                })
            },
        )
    }

    /// How many candidates are left, and when the soonest one is due.
    pub fn reorg_work_remaining(&self, network: monero::Network) -> Result<(u64, Option<i64>)> {
        self.conn
            .query_row(
                "SELECT COUNT(*), MIN(next_attempt_at_utc) FROM reorg_work WHERE network = ?1",
                [shared::network::SqlNetwork(network)],
                |row| Ok((unsigned(row, 0)?, row.get(1)?)),
            )
            .map_err(Into::into)
    }

    /// Removes a candidate. Call inside the transaction that applies its
    /// outcome, so the two can't come apart.
    pub fn complete_reorg_candidate(
        &self,
        network: monero::Network,
        payment_id: i64,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM reorg_work WHERE network = ?1 AND payment_id = ?2",
            params![shared::network::SqlNetwork(network), payment_id],
        )?;
        Ok(())
    }

    /// A candidate whose lookup failed: retried later, after the others.
    pub fn defer_reorg_candidate(
        &self,
        network: monero::Network,
        payment_id: i64,
        now: i64,
    ) -> Result<()> {
        let attempts: Option<u32> = self
            .conn
            .query_row(
                "SELECT attempts FROM reorg_work WHERE network = ?1 AND payment_id = ?2",
                params![shared::network::SqlNetwork(network), payment_id],
                |row| unsigned(row, 0),
            )
            .optional()?;
        if let Some(attempts) = attempts {
            let attempts = attempts.saturating_add(1);
            self.conn.execute(
                "UPDATE reorg_work SET attempts = ?3, next_attempt_at_utc = ?4 WHERE network = ?1 AND payment_id = ?2",
                params![shared::network::SqlNetwork(network), payment_id, attempts, now + reorg_retry_delay(attempts)],
            )?;
        }
        Ok(())
    }

    /// The final step of a reorg job, in one transaction: forget the losing
    /// chain's blocks at and above the fork, re-anchor the common ancestor
    /// if that emptied the window (so the next scan doesn't re-seed at the
    /// tip and skip the replacement blocks), move every tenant back to the
    /// ancestor, drop block progress on the losing chain, and close the job.
    ///
    /// Refuses (`NotFound`) unless the job still forks at `fork_height` and
    /// has no candidates left: a deeper fork found meanwhile must be
    /// reconciled first.
    pub fn finish_reorg(
        &self,
        network: monero::Network,
        fork_height: u64,
        ancestor: Option<(u64, &str)>,
    ) -> Result<()> {
        self.in_transaction(|s| {
            let job = s.reorg_job(network)?.ok_or(StoreError::NotFound)?;
            let (remaining, _) = s.reorg_work_remaining(network)?;
            if job.fork_height != fork_height || job.phase != ReorgPhase::Process || remaining != 0 {
                return Err(StoreError::NotFound);
            }
            s.forget_scanned_blocks_at_or_above(network, fork_height)?;
            if s.max_scanned_height(network)?.is_none() {
                if let Some((height, hash)) = ancestor {
                    s.set_scanned_block(network, height, hash)?;
                }
            }
            s.clamp_cursors(network, ancestor.map(|(height, _)| height))?;
            s.conn.execute(
                "DELETE FROM partial_block_matches WHERE network = ?1 AND tenant_id IN
                 (SELECT tenant_id FROM partial_block_progress WHERE network = ?1 AND height >= ?2)",
                params![shared::network::SqlNetwork(network), Unsigned(fork_height)],
            )?;
            s.conn.execute(
                "DELETE FROM partial_block_progress WHERE network = ?1 AND height >= ?2",
                params![shared::network::SqlNetwork(network), Unsigned(fork_height)],
            )?;
            s.conn.execute("DELETE FROM reorg_jobs WHERE network = ?1", [shared::network::SqlNetwork(network)])?;
            Ok(())
        })
    }

    /// Whether new settlements on `network` must wait: a reorg is being
    /// reconciled there, so confirmations may be counted on a losing chain.
    pub fn settlement_frozen(&self, network: monero::Network) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM reorg_jobs WHERE network = ?1)",
                [shared::network::SqlNetwork(network)],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// The highest block an order on `network` may newly settle on, if
    /// there is a ceiling (docs/chain_agreement.md).
    pub fn settlement_ceiling(&self, network: monero::Network) -> Result<Option<u64>> {
        self.conn
            .query_row(
                "SELECT height FROM settlement_ceilings WHERE network = ?1",
                [shared::network::SqlNetwork(network)],
                |row| unsigned(row, 0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Applies a chain agreement transition's ceiling. Only
    /// `work::agreement` calls this, with what its state machine decided.
    pub fn write_settlement_ceiling(
        &self,
        network: monero::Network,
        write: crate::work::agreement::CeilingWrite,
        now: i64,
    ) -> Result<()> {
        use crate::work::agreement::CeilingWrite;
        let network = shared::network::SqlNetwork(network);
        match write {
            CeilingWrite::Keep => {}
            CeilingWrite::Set(height) => {
                self.conn.execute(
                    "INSERT INTO settlement_ceilings (network, height, updated_at_utc) VALUES (?1, ?2, ?3)
                     ON CONFLICT (network) DO UPDATE SET height = excluded.height, updated_at_utc = excluded.updated_at_utc",
                    params![network, Unsigned(height), now],
                )?;
            }
            CeilingWrite::KeepOrHoldAll => {
                self.conn.execute(
                    "INSERT OR IGNORE INTO settlement_ceilings (network, height, updated_at_utc) VALUES (?1, 0, ?2)",
                    params![network, now],
                )?;
            }
            CeilingWrite::Clear => {
                self.conn.execute(
                    "DELETE FROM settlement_ceilings WHERE network = ?1",
                    [network],
                )?;
            }
        }
        Ok(())
    }

    /// Stored block hashes on `network` from `from` to `to` inclusive,
    /// lowest first. Bounded by the retained window.
    pub fn scanned_blocks_between(
        &self,
        network: monero::Network,
        from: u64,
        to: u64,
    ) -> Result<Vec<(u64, String)>> {
        self.rows(
            "SELECT height, block_hash FROM scanned_blocks WHERE network = ?1 AND height BETWEEN ?2 AND ?3 ORDER BY height",
            params![shared::network::SqlNetwork(network), Unsigned(from), Unsigned(to)],
            |row| Ok((unsigned(row, 0)?, row.get::<_, String>(1)?)),
        )
    }

    /// Up to `limit` orders on `network` whose status may have changed with
    /// time (`next_due_at_utc <= now`) or height (`next_due_height <= tip`),
    /// earliest due first. Each kind gets half the page before either takes
    /// what the other left: an order held back (its expiry while its store
    /// lags, its settlement during a reorg) is due again at once every
    /// round, and enough of them would otherwise fill the page and leave
    /// confirming orders without their per-block recompute.
    pub fn due_order_ids(
        &self,
        network: monero::Network,
        now: i64,
        tip: u64,
        limit: usize,
    ) -> Result<Vec<OrderId>> {
        let tip = i64::try_from(tip).unwrap_or(i64::MAX);
        let queries = [
            (
                "SELECT o.id FROM orders o JOIN tenants t ON t.id = o.tenant_id
                 WHERE o.next_due_at_utc IS NOT NULL AND o.next_due_at_utc <= ?2 AND t.network = ?1
                 ORDER BY o.next_due_at_utc, o.id LIMIT ?3",
                now,
            ),
            (
                "SELECT o.id FROM orders o JOIN tenants t ON t.id = o.tenant_id
                 WHERE o.next_due_height IS NOT NULL AND o.next_due_height <= ?2 AND t.network = ?1
                 ORDER BY o.next_due_height, o.id LIMIT ?3",
                tip,
            ),
        ];
        let mut found: Vec<Vec<OrderId>> = Vec::with_capacity(queries.len());
        for (sql, due) in queries {
            found.push(self.rows(
                sql,
                params![shared::network::SqlNetwork(network), due, Unsigned(limit)],
                |row| row.get::<_, OrderId>(0),
            )?);
        }
        let mut ids: Vec<OrderId> = Vec::with_capacity(limit);
        let mut take = |from: &mut Vec<OrderId>, up_to: usize| {
            let mut taken = 0;
            while taken < up_to && !from.is_empty() && ids.len() < limit {
                let id = from.remove(0);
                taken += 1;
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        };
        let share = limit.div_ceil(2);
        for list in found.iter_mut() {
            take(list, share);
        }
        for list in found.iter_mut() {
            take(list, limit);
        }
        Ok(ids)
    }

    /// A rotation position, if one was recorded. A value that doesn't parse
    /// (a hand-edited row) is logged and treated as absent: the rotation
    /// starts over, which costs repeated work, never skipped work.
    pub fn scheduler_position<P: Position>(
        &self,
        network: monero::Network,
    ) -> Result<Option<P::Value>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM scheduler_positions WHERE network = ?1 AND position = ?2",
                params![shared::network::SqlNetwork(network), P::KEY],
                |row| row.get(0),
            )
            .optional()?;
        Ok(raw.and_then(|raw| match raw.parse() {
            Ok(value) => Some(value),
            Err(_) => {
                tracing::warn!(network = crate::network::network_str(network), position = P::KEY, value = %raw, "an unreadable scheduler position; starting that rotation over");
                None
            }
        }))
    }

    pub fn set_scheduler_position<P: Position>(
        &self,
        network: monero::Network,
        value: &P::Value,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO scheduler_positions (network, position, value) VALUES (?1, ?2, ?3)
             ON CONFLICT (network, position) DO UPDATE SET value = excluded.value",
            params![
                shared::network::SqlNetwork(network),
                P::KEY,
                value.to_string()
            ],
        )?;
        Ok(())
    }

    /// A payment by its row id, voided or not.
    pub fn payment_by_id(&self, payment_id: i64) -> Result<Option<OrderPaymentRow>> {
        self.conn
            .query_row(
                "SELECT * FROM order_payments WHERE id = ?1",
                [payment_id],
                Self::row_to_payment,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Up to `limit` payments on `network` voided no earlier than `cutoff`,
    /// after payment id `after`, in id order: one page of the slow recheck
    /// for false double-spend accusations.
    pub fn voided_payments_page(
        &self,
        network: monero::Network,
        cutoff: i64,
        after: i64,
        limit: usize,
    ) -> Result<Vec<OrderPaymentRow>> {
        self.rows(
            "SELECT op.* FROM order_payments op
             JOIN orders o ON o.id = op.order_id JOIN tenants t ON t.id = o.tenant_id
             WHERE op.voided_at_utc IS NOT NULL AND op.voided_at_utc >= ?2 AND op.id > ?3 AND t.network = ?1
             ORDER BY op.id LIMIT ?4",
            params![shared::network::SqlNetwork(network), cutoff, after, Unsigned(limit)],
            Self::row_to_payment,
        )
    }
}

/// A block a tenant's scan stopped partway through (its step ran out of
/// time). Resumed only for the same block hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockCheckpoint {
    pub height: u64,
    pub hash: String,
    pub next_tx: usize,
}

/// A match from an unfinished block, held until the block commits.
#[derive(Clone, Debug)]
pub struct StagedPayment {
    pub order_id: OrderId,
    pub txid: String,
    pub output_index: i64,
    pub amount_piconero: u64,
    pub key_images_json: String,
    pub seen_at: i64,
    pub output_key: Option<String>,
}

impl Store {
    pub fn block_checkpoint(
        &self,
        network: monero::Network,
        tenant_id: &TenantId,
    ) -> Result<Option<BlockCheckpoint>> {
        self.conn
            .query_row(
                "SELECT height, block_hash, next_tx_index FROM partial_block_progress WHERE network = ?1 AND tenant_id = ?2",
                params![shared::network::SqlNetwork(network), tenant_id],
                |row| {
                    Ok(BlockCheckpoint {
                        height: unsigned(row, 0)?,
                        hash: row.get(1)?,
                        next_tx: unsigned(row, 2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Records how far a tenant's scan of a block got. A checkpoint for any
    /// other block (another hash: a block's hash names its height too) is
    /// replaced, with its staged matches. Call in the transaction that stages
    /// this block's matches.
    pub fn save_block_checkpoint(
        &self,
        network: monero::Network,
        tenant_id: &TenantId,
        checkpoint: &BlockCheckpoint,
    ) -> Result<()> {
        if self
            .block_checkpoint(network, tenant_id)?
            .is_some_and(|old| old.hash != checkpoint.hash)
        {
            self.clear_partial_block(network, tenant_id)?;
        }
        self.conn.execute(
            "INSERT INTO partial_block_progress (network, tenant_id, height, block_hash, window_generation, next_tx_index)
             VALUES (?1, ?2, ?3, ?4, '', ?5)
             ON CONFLICT (network, tenant_id) DO UPDATE SET
                 height = excluded.height, block_hash = excluded.block_hash, next_tx_index = excluded.next_tx_index",
            params![shared::network::SqlNetwork(network), tenant_id, Unsigned(checkpoint.height), checkpoint.hash, Unsigned(checkpoint.next_tx)],
        )?;
        Ok(())
    }

    /// Removes a tenant's checkpoint and returns its staged matches if it was
    /// for the block with this hash; a stale one is dropped.
    pub fn take_staged_payments(
        &self,
        network: monero::Network,
        tenant_id: &TenantId,
        hash: &str,
    ) -> Result<Vec<StagedPayment>> {
        let current = self
            .block_checkpoint(network, tenant_id)?
            .is_some_and(|c| c.hash == hash);
        let staged = if current {
            self.rows(
                "SELECT order_id, txid, output_index, amount_piconero, key_images_json, seen_at_utc, output_key
                 FROM partial_block_matches WHERE network = ?1 AND tenant_id = ?2",
                params![shared::network::SqlNetwork(network), tenant_id],
                |row| {
                    Ok(StagedPayment {
                        order_id: row.get(0)?,
                        txid: row.get(1)?,
                        output_index: row.get(2)?,
                        amount_piconero: unsigned(row, 3)?,
                        key_images_json: row.get(4)?,
                        seen_at: row.get(5)?,
                        output_key: row.get(6)?,
                    })
                },
            )?
        } else {
            Vec::new()
        };
        self.clear_partial_block(network, tenant_id)?;
        Ok(staged)
    }

    /// Up to `limit` distinct cursor heights below `below` held by enabled
    /// tenants on `network`, after `after` (all, from the lowest, for
    /// `None`): the catch-up groups, in rotation order.
    pub fn scan_group_cursors(
        &self,
        network: monero::Network,
        below: u64,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<u64>> {
        self.rows(
            "SELECT DISTINCT scanned_through_height FROM tenants
             WHERE network = ?1 AND disabled_at_utc IS NULL AND scanned_through_height IS NOT NULL
               AND scanned_through_height < ?2 AND scanned_through_height >= ?3
             ORDER BY scanned_through_height LIMIT ?4",
            params![
                shared::network::SqlNetwork(network),
                Unsigned(below),
                Unsigned(after.map_or(0, |a| a.saturating_add(1))),
                Unsigned(limit)
            ],
            |row| unsigned(row, 0),
        )
    }

    /// Up to `limit` enabled tenants on `network` whose cursor is `cursor`,
    /// leaving out `excluding` (tenants waiting out a retry delay), in id
    /// order.
    pub fn tenants_at_cursor(
        &self,
        network: monero::Network,
        cursor: u64,
        excluding: &[TenantId],
        limit: usize,
    ) -> Result<Vec<TenantId>> {
        let excluding = json_array(excluding);
        self.rows(
            "SELECT id FROM tenants
             WHERE network = ?1 AND disabled_at_utc IS NULL AND scanned_through_height = ?2
               AND id NOT IN (SELECT value FROM json_each(?3))
             ORDER BY id LIMIT ?4",
            params![
                shared::network::SqlNetwork(network),
                Unsigned(cursor),
                excluding,
                Unsigned(limit)
            ],
            |row| row.get::<_, TenantId>(0),
        )
    }

    /// The scan windows of several tenants at once, in one query: the minor
    /// indices of each tenant's orders that are open, or closed no earlier
    /// than `since` minus the grace period (task 7.3, decision D10). The live
    /// scan passes now; catch-up passes the time of the tenants' cursor
    /// block, so orders that closed during their gap are still looked for.
    /// Each list is ascending and never empty: a tenant with nothing in
    /// scope is absent.
    pub fn scan_windows(
        &self,
        tenant_ids: &[TenantId],
        since: i64,
        grace_period_seconds: i64,
    ) -> Result<std::collections::HashMap<TenantId, Vec<u32>>> {
        let ids = json_array(tenant_ids);
        let rows = self.rows(
            &format!(
                "SELECT tenant_id, minor_index FROM orders WHERE id IN ({}) ORDER BY tenant_id, minor_index",
                super::scan_window_orders("o.tenant_id IN (SELECT value FROM json_each(:ids))")
            ),
            rusqlite::named_params! { ":ids": ids, ":since_minus_grace": since.saturating_sub(grace_period_seconds) },
            |row| Ok((row.get::<_, TenantId>(0)?, unsigned::<u32>(row, 1)?)),
        )?;
        let mut windows: std::collections::HashMap<TenantId, Vec<u32>> =
            std::collections::HashMap::new();
        for (tenant_id, minor) in rows {
            windows.entry(tenant_id).or_default().push(minor);
        }
        Ok(windows)
    }

    /// Moves every enabled tenant on `network` at cursor `from` that has
    /// nothing that could have been paid since `since` (no order in its scan
    /// window as of then) straight to `to`: there is nothing in those blocks
    /// for it to find. The predicate is evaluated here, inside the caller's
    /// transaction, so an order committed before it counts. Returns how many
    /// moved.
    pub fn advance_idle_cursors(
        &self,
        network: monero::Network,
        from: u64,
        to: u64,
        since: i64,
        grace_period_seconds: i64,
    ) -> Result<usize> {
        let moved = self.conn.execute(
            &format!(
                "UPDATE tenants SET scanned_through_height = :to
                 WHERE network = :network AND disabled_at_utc IS NULL AND scanned_through_height = :from
                   AND NOT {}",
                super::tenant_in_scope("tenants.id")
            ),
            rusqlite::named_params! {
                ":network": shared::network::SqlNetwork(network),
                ":from": Unsigned(from),
                ":to": Unsigned(to),
                ":since_minus_grace": since.saturating_sub(grace_period_seconds),
            },
        )?;
        Ok(moved)
    }

    /// Moves tenants' cursors past a block that was scanned for them, in one
    /// statement. Only a [`ScannedBlock`](crate::work::ScannedBlock) can do
    /// this, and only the block scan builds one. Conditional on each cursor
    /// still being at the block's parent, so a reorg rewind in between wins;
    /// returns the tenants that moved.
    pub fn advance_scanned_cursors(
        &self,
        network: monero::Network,
        height: u64,
        scanned: &[crate::work::ScannedBlock],
    ) -> Result<std::collections::HashSet<TenantId>> {
        let ids: Vec<&TenantId> = scanned
            .iter()
            .filter(|b| b.height() == height)
            .map(|b| b.tenant_id())
            .collect();
        if ids.is_empty() {
            return Ok(Default::default());
        }
        let ids = json_array(&ids);
        let moved = self.rows(
            "UPDATE tenants SET scanned_through_height = ?2
             WHERE network = ?1 AND scanned_through_height = ?2 - 1 AND id IN (SELECT value FROM json_each(?3))
             RETURNING id",
            params![shared::network::SqlNetwork(network), Unsigned(height), ids],
            |row| row.get::<_, TenantId>(0),
        )?;
        Ok(moved.into_iter().collect())
    }

    /// Up to `limit` enabled tenants on `network` with an order in scope,
    /// after `after` in id order, with their cursors: one page of the
    /// scanned-range bookkeeping.
    pub fn active_tenants_page(
        &self,
        network: monero::Network,
        now: i64,
        grace_period_seconds: i64,
        after: &str,
        limit: usize,
    ) -> Result<Vec<(TenantId, Option<u64>)>> {
        self.rows(
            &format!(
                "SELECT t.id, t.scanned_through_height FROM tenants t
                 WHERE t.network = :network AND t.disabled_at_utc IS NULL AND t.id > :after AND {}
                 ORDER BY t.id LIMIT :limit",
                super::tenant_in_scope("t.id")
            ),
            rusqlite::named_params! {
                ":network": shared::network::SqlNetwork(network),
                ":after": after,
                ":limit": Unsigned(limit),
                ":since_minus_grace": now - grace_period_seconds,
            },
            |row| {
                Ok((
                    row.get::<_, TenantId>(0)?,
                    row.get::<_, Option<Unsigned<u64>>>(1)?.map(|h| h.0),
                ))
            },
        )
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::store::{NewOrder, NewTenant};

    fn file_store() -> (Store, String) {
        let path = std::env::temp_dir().join(format!("scanner_work_{}.db", uuid::Uuid::new_v4()));
        let path = path.to_string_lossy().into_owned();
        (Store::open_file(&path).unwrap(), path)
    }

    fn cleanup(path: &str) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path}{suffix}"));
        }
    }

    fn tenant(store: &Store, network: &str) -> String {
        store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![0u8; 64],
                    primary_address: format!("4{}", uuid::Uuid::new_v4().simple()),
                    network: network.into(),
                    confirmations_required: Some(10),
                    order_expiry_seconds: None,
                },
                100,
            )
            .unwrap()
            .tenant
            .id
            .into_string()
    }

    fn order(store: &Store, tenant_id: &str, expires_at: i64) -> String {
        let index = store
            .allocate_minor_index(&shared::ids::TenantId::new(tenant_id.to_string()))
            .unwrap();
        store
            .create_order(NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: tenant_id.into(),
                merchant_order_id: None,
                minor_index: index,
                address: format!("addr-{}", uuid::Uuid::new_v4().simple()),
                xmr_amount_piconero: 100,
                description: None,
                created_at: 100,
                expires_at,
            })
            .unwrap()
            .id
            .into_string()
    }

    fn pay(store: &Store, order_id: &str, txid: &str, height: Option<i64>) -> i64 {
        store
            .record_payment_match(
                &shared::ids::OrderId::new(order_id.to_string()),
                txid,
                0,
                10,
                "[\"ki\"]",
                100,
                height,
                None,
            )
            .unwrap();
        store
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .into_iter()
            .find(|p| p.txid == txid)
            .unwrap()
            .id
    }

    fn work(store: &Store, network: &str) -> Vec<i64> {
        store
            .due_reorg_candidates(
                shared::network::parse_network(network).unwrap(),
                i64::MAX,
                1000,
            )
            .unwrap()
            .into_iter()
            .map(|c| c.payment.id)
            .collect()
    }

    /// Collection covers confirmed payments at or above the fork and every
    /// unconfirmed one, on this network only, in pages that survive a
    /// restart, and never a payment recorded after the job opened.
    #[test]
    fn a_reorg_job_collects_its_candidates_in_pages_that_survive_a_restart() {
        let (store, path) = fixture();
        let (main_order, other_order) = (&store.1.clone(), &store.2.clone());
        let s = &store.0;
        let below = pay(s, main_order, "below", Some(9));
        let at = pay(s, main_order, "at", Some(10));
        let above: Vec<i64> = (0..5)
            .map(|i| pay(s, main_order, &format!("above{i}"), Some(11 + i)))
            .collect();
        let unconfirmed = pay(s, main_order, "pool", None);
        let _other_network = pay(s, other_order, "other", Some(12));
        assert_eq!(
            s.open_reorg_job(monero::Network::Mainnet, 10, 1000)
                .unwrap(),
            OpenedReorg::Created
        );
        let late = pay(s, main_order, "late", Some(12));

        // Two candidates per page, restarting the process between pages.
        assert!(matches!(
            s.collect_reorg_candidates(monero::Network::Mainnet, 2, 1001)
                .unwrap(),
            ReorgPhase::CollectConfirmed { .. }
        ));
        let s = reopen(store, &path);
        let mut phase = s
            .collect_reorg_candidates(monero::Network::Mainnet, 2, 1002)
            .unwrap();
        while phase != ReorgPhase::Process {
            phase = s
                .collect_reorg_candidates(monero::Network::Mainnet, 2, 1003)
                .unwrap();
        }
        let mut collected = work(&s, "mainnet");
        collected.sort();
        let mut expected = vec![at, unconfirmed];
        expected.extend(&above);
        expected.sort();
        assert_eq!(collected, expected);
        assert!(!collected.contains(&below) && !collected.contains(&late));
        assert!(work(&s, "stagenet").is_empty());
        drop(s);
        cleanup(&path);
    }

    /// A deeper fork found while a job is open lowers its fork and collects
    /// again from there; a shallower one changes nothing.
    #[test]
    fn a_deeper_fork_widens_the_open_job_and_a_shallower_one_does_not() {
        let (store, path) = fixture();
        let s = &store.0;
        let deep = pay(s, &store.1, "deep", Some(5));
        let shallow = pay(s, &store.1, "shallow", Some(10));
        s.open_reorg_job(monero::Network::Mainnet, 8, 1000).unwrap();
        while s
            .collect_reorg_candidates(monero::Network::Mainnet, 10, 1000)
            .unwrap()
            != ReorgPhase::Process
        {}
        assert_eq!(work(s, "mainnet"), vec![shallow]);
        assert_eq!(
            s.open_reorg_job(monero::Network::Mainnet, 9, 1001).unwrap(),
            OpenedReorg::Covered
        );
        assert_eq!(
            s.open_reorg_job(monero::Network::Mainnet, 4, 1002).unwrap(),
            OpenedReorg::Deepened { from: 8 }
        );
        while s
            .collect_reorg_candidates(monero::Network::Mainnet, 10, 1003)
            .unwrap()
            != ReorgPhase::Process
        {}
        let mut collected = work(s, "mainnet");
        collected.sort();
        assert_eq!(collected, vec![deep, shallow]);
        drop(store);
        cleanup(&path);
    }

    /// A failed candidate backs off behind the others; a completed one is
    /// gone; the job finishes only once nothing is left.
    #[test]
    fn a_failed_candidate_waits_behind_the_others_and_blocks_the_rewind_until_done() {
        let (store, path) = fixture();
        let s = &store.0;
        let first = pay(s, &store.1, "first", Some(10));
        let second = pay(s, &store.1, "second", Some(11));
        for h in 8..=12 {
            s.set_scanned_block(monero::Network::Mainnet, h, &format!("old{h}"))
                .unwrap();
        }
        s.open_reorg_job(monero::Network::Mainnet, 10, 1000)
            .unwrap();
        while s
            .collect_reorg_candidates(monero::Network::Mainnet, 10, 1000)
            .unwrap()
            != ReorgPhase::Process
        {}
        assert!(s.settlement_frozen(monero::Network::Mainnet).unwrap());
        assert!(!s.settlement_frozen(monero::Network::Stagenet).unwrap());

        // The first retries are due at once; the third failure waits.
        for _ in 0..3 {
            s.defer_reorg_candidate(monero::Network::Mainnet, first, 1000)
                .unwrap();
        }
        assert_eq!(
            (
                reorg_retry_delay(1),
                reorg_retry_delay(2),
                reorg_retry_delay(3)
            ),
            (0, 0, 1)
        );
        let due: Vec<i64> = s
            .due_reorg_candidates(monero::Network::Mainnet, 1000, 10)
            .unwrap()
            .iter()
            .map(|c| c.payment.id)
            .collect();
        assert_eq!(due, vec![second], "the failed one waits");
        assert!(matches!(
            s.finish_reorg(monero::Network::Mainnet, 10, Some((9, "old9"))),
            Err(StoreError::NotFound)
        ));
        s.complete_reorg_candidate(monero::Network::Mainnet, second)
            .unwrap();
        let later = s
            .due_reorg_candidates(monero::Network::Mainnet, 1000 + reorg_retry_delay(3), 10)
            .unwrap();
        assert_eq!(later.len(), 1);
        assert_eq!((later[0].payment.id, later[0].attempts), (first, 3));
        s.complete_reorg_candidate(monero::Network::Mainnet, first)
            .unwrap();

        assert!(
            matches!(
                s.finish_reorg(monero::Network::Mainnet, 9, Some((8, "old8"))),
                Err(StoreError::NotFound)
            ),
            "wrong fork"
        );
        s.finish_reorg(monero::Network::Mainnet, 10, Some((9, "old9")))
            .unwrap();
        assert_eq!(
            s.max_scanned_height(monero::Network::Mainnet).unwrap(),
            Some(9)
        );
        assert!(s.reorg_job(monero::Network::Mainnet).unwrap().is_none());
        assert!(!s.settlement_frozen(monero::Network::Mainnet).unwrap());
        drop(store);
        cleanup(&path);
    }

    /// When the fork is at or below the oldest stored block, the rewind
    /// leaves the ancestor anchored instead of an empty window (which would
    /// read as "never scanned" and re-seed at the tip).
    #[test]
    fn a_rewind_that_empties_the_window_keeps_the_ancestor_and_clamps_cursors() {
        let (store, path) = fixture();
        let s = &store.0;
        s.set_scanned_block(monero::Network::Mainnet, 20, "old20")
            .unwrap();
        s.set_scanned_block(monero::Network::Mainnet, 21, "old21")
            .unwrap();
        s.execute_raw_for_test("UPDATE tenants SET scanned_through_height = 21")
            .unwrap();
        s.open_reorg_job(monero::Network::Mainnet, 20, 1000)
            .unwrap();
        while s
            .collect_reorg_candidates(monero::Network::Mainnet, 10, 1000)
            .unwrap()
            != ReorgPhase::Process
        {}
        s.finish_reorg(monero::Network::Mainnet, 20, Some((19, "new19")))
            .unwrap();
        assert_eq!(
            s.scanned_blocks_between(monero::Network::Mainnet, 0, 100)
                .unwrap(),
            vec![(19, "new19".to_string())]
        );
        let cursors: Vec<Option<u64>> = s
            .list_active_tenants()
            .unwrap()
            .into_iter()
            .filter(|t| t.network == "mainnet")
            .map(|t| t.scanned_through_height)
            .collect();
        assert!(cursors.iter().all(|c| *c == Some(19)), "{cursors:?}");
        drop(store);
        cleanup(&path);
    }

    /// An open order is due at its deadline; recomputing it schedules the
    /// next point its status can move; a terminal one is unscheduled.
    #[test]
    fn open_orders_are_due_by_deadline_and_height_and_terminal_ones_never() {
        let (store, path) = fixture();
        let s = &store.0;
        let expiring = order(s, &store.3, 5_000);
        assert_eq!(
            s.due_order_ids(monero::Network::Mainnet, 4_999, 0, 10)
                .unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            s.due_order_ids(monero::Network::Mainnet, 5_000, 0, 10)
                .unwrap(),
            vec![expiring.clone()]
        );

        // Fully paid at height 50 with ten confirmations needed: due again
        // each block until it settles, then never.
        s.record_payment_match(
            &shared::ids::OrderId::new(expiring.to_string()),
            "tx",
            0,
            100,
            "[\"ki\"]",
            1_000,
            Some(50),
            None,
        )
        .unwrap();
        s.recompute_order_status(&shared::ids::OrderId::new(expiring.to_string()), 52, 1_000)
            .unwrap();
        assert!(s
            .due_order_ids(monero::Network::Mainnet, 1_000, 52, 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            s.due_order_ids(monero::Network::Mainnet, 1_000, 53, 10)
                .unwrap(),
            vec![expiring.clone()]
        );
        assert!(
            s.due_order_ids(monero::Network::Mainnet, 9_999, 52, 10)
                .unwrap()
                .is_empty(),
            "no deadline once fully paid"
        );
        s.recompute_order_status(&shared::ids::OrderId::new(expiring.to_string()), 59, 1_000)
            .unwrap();
        assert!(
            !s.due_order_ids(monero::Network::Mainnet, i64::MAX, u64::MAX, 10)
                .unwrap()
                .contains(&shared::ids::OrderId::new(expiring.to_string())),
            "settled"
        );
        drop(store);
        cleanup(&path);
    }

    /// While a reorg is open on its network an order can't newly settle; the
    /// recompute obligation stays and it settles after the rewind.
    #[test]
    fn a_settlement_waits_for_an_open_reorg_on_its_network() {
        let (store, path) = fixture();
        let s = &store.0;
        let o = order(s, &store.3, 5_000);
        s.record_payment_match(
            &shared::ids::OrderId::new(o.to_string()),
            "tx",
            0,
            100,
            "[\"ki\"]",
            1_000,
            Some(50),
            None,
        )
        .unwrap();
        s.open_reorg_job(monero::Network::Mainnet, 70, 1_000)
            .unwrap();
        let (_, frozen) = s
            .recompute_order_status(&shared::ids::OrderId::new(o.to_string()), 59, 1_000)
            .unwrap();
        assert_eq!(frozen, crate::status::OrderStatus::Confirming);
        assert_eq!(
            s.pending_payment_recomputes_page(monero::Network::Mainnet, "", 10_000)
                .unwrap(),
            vec![o.clone()]
        );
        assert_eq!(
            s.due_order_ids(monero::Network::Mainnet, 1_000, 0, 10)
                .unwrap(),
            vec![o.clone()],
            "due again at once"
        );

        while s
            .collect_reorg_candidates(monero::Network::Mainnet, 10, 1000)
            .unwrap()
            != ReorgPhase::Process
        {}
        s.finish_reorg(monero::Network::Mainnet, 70, Some((69, "h69")))
            .unwrap();
        let (_, settled) = s
            .recompute_order_status(&shared::ids::OrderId::new(o.to_string()), 59, 1_000)
            .unwrap();
        assert_eq!(settled, crate::status::OrderStatus::Paid);
        assert!(s
            .pending_payment_recomputes_page(monero::Network::Mainnet, "", 10_000)
            .unwrap()
            .is_empty());
        drop(store);
        cleanup(&path);
    }

    /// The scanner's hot queries are answered from indexes, never by
    /// scanning a whole table: the plans are checked here so an edit can't
    /// silently turn one back into a full scan.
    #[test]
    fn the_scanners_hot_queries_use_their_indexes() {
        let store = Store::open_in_memory().unwrap();
        let plan = |sql: &str| -> String {
            let mut stmt = store
                .conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap();
            let rows = stmt.query_map([], |row| row.get::<_, String>(3)).unwrap();
            rows.map(|row| row.unwrap()).collect::<Vec<_>>().join(" | ")
        };
        let window = format!(
            "SELECT minor_index FROM orders WHERE id IN ({}) ORDER BY minor_index",
            crate::store::scan_window_orders("o.tenant_id = 'x'")
        )
        .replace(":since_minus_grace", "0");
        let in_scope = format!(
            "SELECT t.id, t.scanned_through_height FROM tenants t
             WHERE t.network = 'mainnet' AND t.disabled_at_utc IS NULL AND t.id > '' AND {} ORDER BY t.id LIMIT 32",
            crate::store::tenant_in_scope("t.id")
        )
        .replace(":since_minus_grace", "0");
        for (what, sql, index) in [
            ("scan window, open half", window.as_str(), "_status_"),
            ("scan window, closed half", window.as_str(), "orders_tenant_closed_idx"),
            ("tenant in scope, open half", in_scope.as_str(), "_status_"),
            ("tenant in scope, closed half", in_scope.as_str(), "orders_tenant_closed_idx"),
            (
                "catch-up groups",
                "SELECT DISTINCT scanned_through_height FROM tenants WHERE network = 'mainnet' AND disabled_at_utc IS NULL \
                 AND scanned_through_height IS NOT NULL AND scanned_through_height < 10 AND scanned_through_height > 1 \
                 ORDER BY scanned_through_height LIMIT 1",
                "tenants_network_cursor_idx",
            ),
            (
                "group members",
                "SELECT id FROM tenants WHERE network = 'mainnet' AND disabled_at_utc IS NULL AND scanned_through_height = 5 ORDER BY id",
                "tenants_network_cursor_idx",
            ),
            (
                "void recheck page",
                "SELECT op.* FROM order_payments op WHERE op.voided_at_utc IS NOT NULL AND op.voided_at_utc >= 0 AND op.id > 0 \
                 ORDER BY op.id LIMIT 16",
                "order_payments_voided_idx",
            ),
            (
                "due by time",
                "SELECT o.id FROM orders o WHERE o.next_due_at_utc IS NOT NULL AND o.next_due_at_utc <= 5 ORDER BY o.next_due_at_utc, o.id LIMIT 64",
                "orders_due_at_idx",
            ),
        ] {
            let plan = plan(sql);
            assert!(plan.contains(index), "{what}: expected {index} in the plan, got {plan}");
            let full_scan = plan.split(" | ").any(|step| step.starts_with("SCAN ") && !step.contains("json_each"));
            assert!(!full_scan, "{what}: a full scan in {plan}");
        }
    }

    #[test]
    fn scheduler_positions_are_per_network_typed_and_survive_a_restart() {
        use position::{CatchUpGroup, ScanRange, VoidRecheck};
        let (store, path) = fixture();
        store
            .0
            .set_scheduler_position::<CatchUpGroup>(monero::Network::Mainnet, &42)
            .unwrap();
        store
            .0
            .set_scheduler_position::<CatchUpGroup>(monero::Network::Mainnet, &43)
            .unwrap();
        store
            .0
            .set_scheduler_position::<ScanRange>(monero::Network::Mainnet, &"tn_x".to_string())
            .unwrap();
        let s = reopen(store, &path);
        assert_eq!(
            s.scheduler_position::<CatchUpGroup>(monero::Network::Mainnet)
                .unwrap(),
            Some(43)
        );
        assert_eq!(
            s.scheduler_position::<ScanRange>(monero::Network::Mainnet)
                .unwrap()
                .as_deref(),
            Some("tn_x")
        );
        assert_eq!(
            s.scheduler_position::<CatchUpGroup>(monero::Network::Stagenet)
                .unwrap(),
            None
        );
        assert_eq!(
            s.scheduler_position::<VoidRecheck>(monero::Network::Mainnet)
                .unwrap(),
            None
        );
        // A hand-edited, unreadable value starts that rotation over.
        s.execute_raw_for_test(
            "UPDATE scheduler_positions SET value = 'x' WHERE position = 'catch_up_group'",
        )
        .unwrap();
        assert_eq!(
            s.scheduler_position::<CatchUpGroup>(monero::Network::Mainnet)
                .unwrap(),
            None
        );
        drop(s);
        cleanup(&path);
    }

    /// Runs `op` with each of its SQL statements failed in turn: every
    /// failure is reported (never a panic, never swallowed) and leaves the
    /// database exactly as it was; then `op` runs clean, and its result is
    /// returned. An operation its callers run inside their transaction is
    /// given one here too.
    fn sweep<T>(store: &Store, op: impl Fn(&Store) -> Result<T>) -> T {
        for fault in 0.. {
            let before = store.dump_for_test();
            let seen = store.fail_nth_access(Some(fault));
            let result = op(store);
            store.fail_nth_access(None);
            if seen.load(std::sync::atomic::Ordering::Relaxed) <= fault {
                return result.unwrap();
            }
            assert!(
                result.is_err(),
                "the failure of statement {fault} was swallowed"
            );
            assert_eq!(
                store.dump_for_test(),
                before,
                "the failure of statement {fault} left a partial write"
            );
        }
        unreachable!()
    }

    /// Every durable operation of the scheduler, failed statement by
    /// statement, along one reorg's life and a block scan's.
    #[test]
    fn every_store_operation_fails_whole_and_then_works() {
        let store = Store::open_in_memory().unwrap();
        let tenant_id = tenant(&store, "mainnet");
        let other = tenant(&store, "mainnet");
        let order_id = order(&store, &tenant_id, 10_000);
        // `other` has no order: nothing in scope, ever.
        for h in 1..=12u64 {
            store
                .set_scanned_block(monero::Network::Mainnet, h, &format!("a{h}"))
                .unwrap();
        }
        store
            .execute_raw_for_test("UPDATE tenants SET scanned_through_height = 12")
            .unwrap();
        store
            .record_payment_match(
                &shared::ids::OrderId::new(order_id.to_string()),
                "tx_confirmed",
                0,
                50,
                "[\"ki1\"]",
                150,
                Some(11),
                None,
            )
            .unwrap();
        store
            .record_payment_match(
                &shared::ids::OrderId::new(order_id.to_string()),
                "tx_pool",
                0,
                50,
                "[\"ki2\"]",
                150,
                None,
                None,
            )
            .unwrap();
        store
            .record_payment_match(
                &shared::ids::OrderId::new(order_id.to_string()),
                "tx_voided",
                0,
                50,
                "[\"ki3\"]",
                150,
                Some(10),
                None,
            )
            .unwrap();
        store
            .void_payment(
                &shared::ids::OrderId::new(order_id.to_string()),
                "tx_voided",
                0,
                160,
            )
            .unwrap();
        let voided_id = store
            .get_all_payments(&shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .iter()
            .find(|p| p.txid == "tx_voided")
            .unwrap()
            .id;

        // A reorg's life.
        sweep(&store, |s| {
            s.open_reorg_job(monero::Network::Mainnet, 11, 200)
        });
        assert!(sweep(&store, |s| s.settlement_frozen(monero::Network::Mainnet)));
        assert_eq!(
            sweep(&store, |s| s.collect_reorg_candidates(
                monero::Network::Mainnet,
                1,
                200
            )),
            ReorgPhase::CollectConfirmed {
                after_height: 11,
                after_id: 1
            }
        );
        sweep(&store, |s| {
            s.collect_reorg_candidates(monero::Network::Mainnet, 1, 200)
        });
        sweep(&store, |s| {
            s.collect_reorg_candidates(monero::Network::Mainnet, 1, 200)
        });
        assert_eq!(
            sweep(&store, |s| s.collect_reorg_candidates(
                monero::Network::Mainnet,
                64,
                200
            )),
            ReorgPhase::Process
        );
        let due = sweep(&store, |s| {
            s.due_reorg_candidates(monero::Network::Mainnet, 200, 10)
        });
        assert_eq!(due.len(), 2);
        sweep(&store, |s| {
            s.defer_reorg_candidate(monero::Network::Mainnet, due[0].payment.id, 200)
        });
        assert_eq!(
            sweep(&store, |s| s.reorg_work_remaining(monero::Network::Mainnet)).0,
            2
        );
        sweep(&store, |s| {
            s.complete_reorg_candidate(monero::Network::Mainnet, due[0].payment.id)
        });
        sweep(&store, |s| {
            s.complete_reorg_candidate(monero::Network::Mainnet, due[1].payment.id)
        });
        sweep(&store, |s| {
            s.finish_reorg(monero::Network::Mainnet, 11, Some((10, "a10")))
        });
        assert!(sweep(&store, |s| s.reorg_job(monero::Network::Mainnet)).is_none());

        // Positions, pages and lookups.
        sweep(&store, |s| {
            s.set_scheduler_position::<position::VoidRecheck>(monero::Network::Mainnet, &5)
        });
        assert_eq!(
            sweep(&store, |s| s.scheduler_position::<position::VoidRecheck>(
                monero::Network::Mainnet
            )),
            Some(5)
        );
        assert_eq!(
            sweep(&store, |s| s.scanned_blocks_between(
                monero::Network::Mainnet,
                9,
                10
            ))
            .len(),
            2
        );
        sweep(&store, |s| {
            s.due_order_ids(monero::Network::Mainnet, 20_000, 12, 10)
        });
        assert_eq!(
            sweep(&store, |s| s.voided_payments_page(
                monero::Network::Mainnet,
                0,
                0,
                10
            ))
            .len(),
            1
        );
        assert!(sweep(&store, |s| s.payment_by_id(voided_id)).is_some());
        assert_eq!(
            sweep(&store, |s| s.scan_group_cursors(
                monero::Network::Mainnet,
                20,
                None,
                10
            )),
            vec![10]
        );
        assert_eq!(
            sweep(&store, |s| s.tenants_at_cursor(
                monero::Network::Mainnet,
                10,
                &[],
                10
            ))
            .len(),
            2
        );
        assert_eq!(
            sweep(&store, |s| s.scan_windows(
                &[
                    shared::ids::TenantId::new(tenant_id.clone()),
                    shared::ids::TenantId::new(other.clone())
                ],
                150,
                0
            ))
            .len(),
            1
        );
        assert_eq!(
            sweep(&store, |s| s.active_tenants_page(
                monero::Network::Mainnet,
                150,
                0,
                "",
                10
            ))
            .len(),
            1
        );

        // A block scan: checkpointed, staged, replaced, taken, committed.
        let checkpoint = BlockCheckpoint {
            height: 11,
            hash: "b11".into(),
            next_tx: 3,
        };
        sweep(&store, |s| {
            s.in_transaction(|s| {
                s.save_block_checkpoint(
                    monero::Network::Mainnet,
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &checkpoint,
                )
            })
        });
        sweep(&store, |s| {
            s.stage_partial_match(crate::store::StagedMatch {
                network: monero::Network::Mainnet,
                tenant_id: &shared::ids::TenantId::new(tenant_id.to_string()),
                order_id: &shared::ids::OrderId::new(order_id.to_string()),
                txid: "tx_staged",
                output_index: 0,
                amount: 70,
                key_images_json: "[]",
                seen_at: 170,
                output_key: None,
            })
        });
        let replaced = BlockCheckpoint {
            height: 11,
            hash: "c11".into(),
            next_tx: 1,
        };
        sweep(&store, |s| {
            s.in_transaction(|s| {
                s.save_block_checkpoint(
                    monero::Network::Mainnet,
                    &shared::ids::TenantId::new(tenant_id.to_string()),
                    &replaced,
                )
            })
        });
        assert!(
            sweep(&store, |s| s.in_transaction(|s| s.take_staged_payments(
                monero::Network::Mainnet,
                &shared::ids::TenantId::new(tenant_id.to_string()),
                "c11"
            )))
            .is_empty(),
            "the staged match went with the old block"
        );
        assert_eq!(
            sweep(&store, |s| s.block_checkpoint(
                monero::Network::Mainnet,
                &shared::ids::TenantId::new(tenant_id.to_string())
            )),
            None
        );
        let scanned = [crate::work::ScannedBlock::for_test(
            &shared::ids::TenantId::new(tenant_id.to_string()),
            11,
        )];
        assert_eq!(
            sweep(&store, |s| s.advance_scanned_cursors(
                monero::Network::Mainnet,
                11,
                &scanned
            ))
            .len(),
            1
        );
        assert_eq!(
            sweep(&store, |s| s.advance_idle_cursors(
                monero::Network::Mainnet,
                10,
                11,
                i64::MAX / 2,
                0
            )),
            1
        );
    }

    /// A value past SQLite's range is refused before it reaches the query,
    /// and a corrupted row fails its page: both errors, never a wrapped
    /// number read as a height.
    #[test]
    fn out_of_range_values_and_corrupt_rows_are_errors() {
        let store = Store::open_in_memory().unwrap();
        let tenant_id = tenant(&store, "mainnet");
        assert!(matches!(
            store.scan_group_cursors(monero::Network::Mainnet, u64::MAX, None, 10),
            Err(StoreError::Sqlite(rusqlite::Error::ToSqlConversionFailure(
                _
            )))
        ));
        let order_id = order(&store, &tenant_id, 10_000);
        store
            .record_payment_match(
                &shared::ids::OrderId::new(order_id.to_string()),
                "tx",
                0,
                1,
                "[]",
                100,
                Some(7),
                None,
            )
            .unwrap();
        store
            .open_reorg_job(monero::Network::Mainnet, 5, 100)
            .unwrap();
        while store
            .collect_reorg_candidates(monero::Network::Mainnet, 10, 100)
            .unwrap()
            != ReorgPhase::Process
        {}
        store
            .execute_raw_for_test("UPDATE reorg_work SET attempts = -3")
            .unwrap();
        assert!(matches!(
            store.due_reorg_candidates(monero::Network::Mainnet, 100, 10),
            Err(StoreError::Sqlite(
                rusqlite::Error::IntegralValueOutOfRange(_, -3)
            ))
        ));
    }

    /// A job row whose phase isn't one this build knows (a hand-edited or
    /// newer row) is an error, not a guess.
    #[test]
    fn an_unknown_reorg_phase_is_an_error() {
        let store = Store::open_in_memory().unwrap();
        store
            .open_reorg_job(monero::Network::Mainnet, 5, 100)
            .unwrap();
        store.execute_raw_for_test("PRAGMA ignore_check_constraints = ON; UPDATE reorg_jobs SET phase = 'later'; PRAGMA ignore_check_constraints = OFF").unwrap();
        let error = store.reorg_job(monero::Network::Mainnet).unwrap_err();
        assert!(error.to_string().contains("unknown reorg phase"), "{error}");
    }

    /// Collecting for a job that has finished collecting changes nothing;
    /// deferring a candidate that is already gone changes nothing.
    #[test]
    fn late_collects_and_defers_are_no_ops() {
        let store = Store::open_in_memory().unwrap();
        store
            .open_reorg_job(monero::Network::Mainnet, 5, 100)
            .unwrap();
        while store
            .collect_reorg_candidates(monero::Network::Mainnet, 10, 100)
            .unwrap()
            != ReorgPhase::Process
        {}
        let before = store.dump_for_test();
        assert_eq!(
            store
                .collect_reorg_candidates(monero::Network::Mainnet, 10, 100)
                .unwrap(),
            ReorgPhase::Process
        );
        store
            .defer_reorg_candidate(monero::Network::Mainnet, 12345, 100)
            .unwrap();
        assert_eq!(store.dump_for_test(), before);
    }

    /// An order due by both time and height is listed once, and the list
    /// stops at the limit.
    #[test]
    fn due_orders_are_listed_once_up_to_the_limit() {
        let store = Store::open_in_memory().unwrap();
        let tenant_id = tenant(&store, "mainnet");
        let orders: Vec<String> = (0..3).map(|_| order(&store, &tenant_id, 10_000)).collect();
        store
            .execute_raw_for_test("UPDATE orders SET next_due_at_utc = 50, next_due_height = 7")
            .unwrap();
        let due = store
            .due_order_ids(monero::Network::Mainnet, 100, 10, 10)
            .unwrap();
        assert_eq!(due.len(), 3, "each once: {due:?}");
        assert!(orders
            .iter()
            .all(|o| due.contains(&shared::ids::OrderId::new(o.to_string()))));
        assert_eq!(
            store
                .due_order_ids(monero::Network::Mainnet, 100, 10, 2)
                .unwrap()
                .len(),
            2
        );
    }

    /// More time-due orders than a page: the height-due ones still get
    /// their half of it, so confirming orders are recomputed every block
    /// however many orders are held back by time.
    #[test]
    fn height_due_orders_get_half_the_page_whatever_the_time_due_backlog() {
        let store = Store::open_in_memory().unwrap();
        let tenant_id = tenant(&store, "mainnet");
        let by_time: Vec<String> = (0..8).map(|_| order(&store, &tenant_id, 10_000)).collect();
        let by_height: Vec<String> = (0..3).map(|_| order(&store, &tenant_id, 10_000)).collect();
        for id in &by_time {
            store
                .execute_raw_for_test(&format!(
                    "UPDATE orders SET next_due_at_utc = 50 WHERE id = '{id}'"
                ))
                .unwrap();
        }
        for id in &by_height {
            store
                .execute_raw_for_test(&format!(
                    "UPDATE orders SET next_due_height = 7 WHERE id = '{id}'"
                ))
                .unwrap();
        }
        let due = store
            .due_order_ids(monero::Network::Mainnet, 100, 10, 6)
            .unwrap();
        assert_eq!(due.len(), 6);
        let heights = due
            .iter()
            .filter(|id| by_height.contains(&id.to_string()))
            .count();
        assert_eq!(
            heights, 3,
            "every height-due order, within its half: {due:?}"
        );
        // With room to spare, the time-due ones take what the others left.
        let due = store
            .due_order_ids(monero::Network::Mainnet, 100, 10, 20)
            .unwrap();
        assert_eq!(due.len(), 11);
    }

    /// A reorg is only finished once it is processing and has nothing
    /// left, at the fork it was opened for.
    #[test]
    fn finishing_a_reorg_early_or_at_another_fork_is_refused() {
        let store = Store::open_in_memory().unwrap();
        store
            .open_reorg_job(monero::Network::Mainnet, 5, 100)
            .unwrap();
        assert!(
            matches!(
                store.finish_reorg(monero::Network::Mainnet, 5, None),
                Err(StoreError::NotFound)
            ),
            "still collecting"
        );
        while store
            .collect_reorg_candidates(monero::Network::Mainnet, 10, 100)
            .unwrap()
            != ReorgPhase::Process
        {}
        assert!(
            matches!(
                store.finish_reorg(monero::Network::Mainnet, 4, None),
                Err(StoreError::NotFound)
            ),
            "another fork"
        );
        store
            .finish_reorg(monero::Network::Mainnet, 5, None)
            .unwrap();
        assert!(
            matches!(
                store.finish_reorg(monero::Network::Mainnet, 5, None),
                Err(StoreError::NotFound)
            ),
            "no job"
        );
    }

    // -- What is scanned for, and what is looked at again: the tests of the
    // queries these pages replaced, held to the pages themselves. -----------

    fn tenant_id(id: &str) -> TenantId {
        TenantId::new(id.to_string())
    }

    fn order_id(id: &str) -> OrderId {
        OrderId::new(id.to_string())
    }

    /// The stores in scope on `network`, by id.
    fn in_scope(store: &Store, network: monero::Network, now: i64, grace: i64) -> Vec<String> {
        store
            .active_tenants_page(network, now, grace, "", 1000)
            .unwrap()
            .into_iter()
            .map(|(id, _)| id.into_string())
            .collect()
    }

    fn set_status(store: &Store, order: &str, status: &str) {
        store
            .conn
            .execute(
                "UPDATE orders SET status = ?2 WHERE id = ?1",
                params![order, status],
            )
            .unwrap();
    }

    /// A store is scanned for while it has an order that can still receive
    /// a payment: in any open status, and in none of the settled ones. One
    /// store per status, so its order's status is the only variable.
    #[test]
    fn a_store_is_in_scope_for_each_open_order_status_and_no_settled_one() {
        let store = Store::open_in_memory().unwrap();
        let mut stores = Vec::new();
        for (status, open) in [
            ("pending", true),
            ("unconfirmed", true),
            ("confirming", true),
            ("partial", true),
            ("paid", false),
            ("overpaid", false),
            ("expired", false),
        ] {
            let tenant = tenant(&store, "mainnet");
            let order = order(&store, &tenant, 2_000);
            set_status(&store, &order, status);
            stores.push((status, open, tenant));
        }
        let active = in_scope(&store, monero::Network::Mainnet, i64::MAX, 0);
        for (status, open, tenant) in &stores {
            assert_eq!(active.contains(tenant), *open, "{status}");
        }
    }

    /// An order that expired keeps its store in scope, and stays in the
    /// store's scan window, for the grace period after it closed and no
    /// longer: a payment that lands just after the deadline is still found.
    #[test]
    fn an_expired_order_stays_in_scope_for_the_grace_period_and_no_longer() {
        let store = Store::open_in_memory().unwrap();
        let tenant = tenant(&store, "mainnet");
        let order = order(&store, &tenant, 2_000);
        // Closed at its deadline, as `recompute_order_status` records it.
        store
            .conn
            .execute(
                "UPDATE orders SET status = 'expired', closed_at_utc = expires_at_utc WHERE id = ?1",
                params![order],
            )
            .unwrap();
        let minor: u32 = store
            .conn
            .query_row(
                "SELECT minor_index FROM orders WHERE id = ?1",
                params![order],
                |row| row.get(0),
            )
            .unwrap();
        // The store's page and its window say the same.
        let scanned_for = |now: i64, grace: i64| {
            let listed = in_scope(&store, monero::Network::Mainnet, now, grace).contains(&tenant);
            let windows = store
                .scan_windows(&[tenant_id(&tenant)], now, grace)
                .unwrap();
            match windows.get(&tenant_id(&tenant)) {
                Some(window) => assert_eq!(window, &vec![minor]),
                None => assert!(windows.is_empty()),
            }
            assert_eq!(
                listed,
                !windows.is_empty(),
                "at {now} with {grace}s of grace"
            );
            listed
        };
        // Exactly at the boundary (closed at `now - grace`): inclusive.
        assert!(scanned_for(2_000, 0));
        // One second past, with no grace at all.
        assert!(!scanned_for(2_001, 0));
        // A real grace window: still within it, then past it.
        assert!(scanned_for(2_500, 600));
        assert!(!scanned_for(2_601, 600));
    }

    /// A store with no orders isn't scanned for. One drops out once its
    /// only order settles, and a fresh order brings it straight back: the
    /// page is read fresh each time, so there is no "inactive" left over to
    /// undo. One of several orders still open keeps it in.
    #[test]
    fn a_store_leaves_scope_when_its_orders_settle_and_returns_with_a_new_one() {
        let store = Store::open_in_memory().unwrap();
        let active = |tenant: &String| {
            in_scope(&store, monero::Network::Mainnet, i64::MAX, 0).contains(tenant)
        };
        let settle = |order: &str, txid: &str| {
            store
                .record_payment_match(&order_id(order), txid, 0, 100, "[]", 1_500, Some(50), None)
                .unwrap();
            let (_, status) = store
                .recompute_order_status(&order_id(order), 59, 1_600)
                .unwrap();
            assert_eq!(status, crate::status::OrderStatus::Paid);
        };

        let tenant_a = tenant(&store, "mainnet");
        assert!(!active(&tenant_a), "no orders at all");
        let first = order(&store, &tenant_a, 100_000);
        assert!(active(&tenant_a));
        settle(&first, "tx_a");
        assert!(!active(&tenant_a), "its only order is settled");
        order(&store, &tenant_a, 100_000);
        assert!(active(&tenant_a), "a fresh order");

        let tenant_b = tenant(&store, "mainnet");
        let settled = order(&store, &tenant_b, 100_000);
        order(&store, &tenant_b, 100_000);
        settle(&settled, "tx_b");
        assert!(active(&tenant_b), "its other order is still open");
    }

    /// Each network's scan sees its own stores and orders only: another
    /// chain's heights and transactions are unrelated to it.
    #[test]
    fn stores_in_scope_and_due_orders_are_those_of_one_network() {
        let store = Store::open_in_memory().unwrap();
        let main_tenant = tenant(&store, "mainnet");
        let stage_tenant = tenant(&store, "stagenet");
        let main_order = order(&store, &main_tenant, 5_000);
        let stage_order = order(&store, &stage_tenant, 5_000);

        assert_eq!(
            in_scope(&store, monero::Network::Mainnet, i64::MAX, 0),
            vec![main_tenant]
        );
        assert_eq!(
            in_scope(&store, monero::Network::Stagenet, i64::MAX, 0),
            vec![stage_tenant]
        );
        // Both orders reach their deadline at once; each is due on its own
        // network.
        let due = |network| {
            store
                .due_order_ids(network, 5_000, 0, 10)
                .unwrap()
                .into_iter()
                .map(|id| id.into_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(due(monero::Network::Mainnet), vec![main_order]);
        assert_eq!(due(monero::Network::Stagenet), vec![stage_order]);
    }

    /// A payment a reorg pushed back into the pool has no height, and SQL
    /// makes `NULL >= n` false: collecting by height alone would never look
    /// at it again, and it could be proven double-spent and still never be
    /// voided. It is collected, as something above every height. So is a
    /// voided payment, mined or not: the transaction that proved it
    /// double-spent can itself be reorged out, and un-voiding depends on
    /// looking again. Never another network's.
    #[test]
    fn a_reorg_collects_payments_back_in_the_pool_and_voided_ones() {
        let store = Store::open_in_memory().unwrap();
        let tenant = tenant(&store, "mainnet");
        let order = order(&store, &tenant, 100_000);
        let other_tenant = self::tenant(&store, "stagenet");
        let other_order = self::order(&store, &other_tenant, 100_000);

        let below = pay(&store, &order, "below", Some(49));
        let pooled_again = pay(&store, &order, "pooled_again", Some(50));
        store
            .update_payment_block_height(&order_id(&order), "pooled_again", 0, None)
            .unwrap();
        let voided_mined = pay(&store, &order, "voided_mined", Some(60));
        let voided_pooled = pay(&store, &order, "voided_pooled", None);
        for txid in ["voided_mined", "voided_pooled"] {
            store
                .void_payment(&order_id(&order), txid, 0, 1_600)
                .unwrap();
        }
        pay(&store, &other_order, "other_mined", Some(60));
        pay(&store, &other_order, "other_pooled", None);

        store
            .open_reorg_job(monero::Network::Mainnet, 50, 2_000)
            .unwrap();
        while store
            .collect_reorg_candidates(monero::Network::Mainnet, 2, 2_001)
            .unwrap()
            != ReorgPhase::Process
        {}
        let mut collected = work(&store, "mainnet");
        collected.sort();
        let mut expected = vec![pooled_again, voided_mined, voided_pooled];
        expected.sort();
        assert_eq!(collected, expected);
        assert!(!collected.contains(&below));
        assert!(work(&store, "stagenet").is_empty());
    }

    /// The recheck of voided payments reads them from a cutoff: bounded by
    /// how recently a payment was voided, not by every void ever. Pages move
    /// on by payment id, and keep to their network.
    #[test]
    fn voided_payments_are_paged_from_a_cutoff_on_their_own_network() {
        let store = Store::open_in_memory().unwrap();
        let tenant = tenant(&store, "mainnet");
        let order = order(&store, &tenant, 100_000);
        let old = pay(&store, &order, "tx_old", Some(50));
        let recent = pay(&store, &order, "tx_recent", Some(50));
        pay(&store, &order, "tx_never_voided", Some(50));
        store
            .void_payment(&order_id(&order), "tx_old", 0, 1_000)
            .unwrap();
        store
            .void_payment(&order_id(&order), "tx_recent", 0, 5_000)
            .unwrap();
        let page = |network, cutoff: i64, after: i64, limit: usize| {
            store
                .voided_payments_page(network, cutoff, after, limit)
                .unwrap()
                .into_iter()
                .map(|payment| payment.id)
                .collect::<Vec<_>>()
        };
        let mainnet = monero::Network::Mainnet;
        assert_eq!(page(mainnet, 3_000, 0, 10), vec![recent]);
        assert_eq!(
            page(mainnet, 0, 0, 10),
            vec![old, recent],
            "a cutoff at or before every void returns all of them"
        );
        assert!(
            page(mainnet, 5_001, 0, 10).is_empty(),
            "a cutoff after every void returns nothing"
        );
        // One at a time, each page after the last payment of the one before.
        assert_eq!(page(mainnet, 0, 0, 1), vec![old]);
        assert_eq!(page(mainnet, 0, old, 1), vec![recent]);
        assert!(page(mainnet, 0, recent, 1).is_empty());
        assert!(page(monero::Network::Stagenet, 0, 0, 10).is_empty());
    }

    /// (store, mainnet order, stagenet order, mainnet tenant)
    fn fixture() -> ((Store, String, String, String), String) {
        let (store, path) = file_store();
        let main_tenant = tenant(&store, "mainnet");
        let other_tenant = tenant(&store, "stagenet");
        let main_order = order(&store, &main_tenant, 100_000);
        let other_order = order(&store, &other_tenant, 100_000);
        ((store, main_order, other_order, main_tenant), path)
    }

    fn reopen(store: (Store, String, String, String), path: &str) -> Store {
        drop(store);
        Store::open_file(path).unwrap()
    }

    #[test]
    fn unsigned_values_cross_into_sqlite_checked_both_ways() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let read = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, Unsigned<u64>>(0));
        assert_eq!(read("SELECT 42").unwrap(), Unsigned(42));
        // A negative value read back is an error, not a wrapped huge height.
        assert!(matches!(
            read("SELECT -1"),
            Err(rusqlite::Error::IntegralValueOutOfRange(0, -1))
        ));
        // A value too big for SQLite's signed integers is refused on the way in.
        let wrote = conn.query_row("SELECT ?1", [Unsigned(u64::MAX)], |row| {
            row.get::<_, i64>(0)
        });
        assert!(matches!(
            wrote,
            Err(rusqlite::Error::ToSqlConversionFailure(_))
        ));
        let wrote = conn.query_row("SELECT ?1", [Unsigned(7usize)], |row| row.get::<_, i64>(0));
        assert_eq!(wrote.unwrap(), 7);
    }
}
