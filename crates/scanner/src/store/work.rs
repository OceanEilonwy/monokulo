//! Durable state for the scanner's work units (docs/scanner_microtasks.md):
//! the reorg job, the recompute schedule, and rotation positions. Every
//! read here is a bounded page; every multi-row write is one transaction.

use rusqlite::{params, OptionalExtension};

use super::{OrderPaymentRow, Result, Store, StoreError};

/// An unsigned value (a height, count, index or limit) crossing into or out
/// of SQLite, which stores only signed 64-bit integers. Both directions are
/// checked: a value that doesn't fit, or a negative one read back, is an
/// error rather than a silent wrap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Unsigned<T>(pub T);

impl<T: Copy + TryInto<i64>> rusqlite::ToSql for Unsigned<T>
where
    <T as TryInto<i64>>::Error: std::error::Error + Send + Sync + 'static,
{
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        let value: i64 = self.0.try_into().map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        Ok(value.into())
    }
}

impl<T: TryFrom<i64>> rusqlite::types::FromSql for Unsigned<T> {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let raw = value.as_i64()?;
        T::try_from(raw).map(Unsigned).map_err(|_| rusqlite::types::FromSqlError::OutOfRange(raw))
    }
}

/// A block height as SQLite stores it. Heights never come near `i64::MAX`;
/// one that did is refused rather than wrapped negative.
pub fn sql_height(height: u64) -> Result<i64> {
    i64::try_from(height).map_err(|e| StoreError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e))))
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
    Deepened { from: u64 },
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
        "collect_confirmed" => ReorgPhase::CollectConfirmed { after_height, after_id },
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
    pub fn reorg_job(&self, network: &str) -> Result<Option<ReorgJob>> {
        self.conn
            .query_row(
                "SELECT network, fork_height, phase, candidate_max_id, collect_after_height, collect_after_id,
                        created_at_utc
                 FROM reorg_jobs WHERE network = ?1",
                [network],
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
    pub fn open_reorg_job(&self, network: &str, fork_height: u64, now: i64) -> Result<OpenedReorg> {
        self.in_transaction(|s| {
            let max_id: i64 = s.conn.query_row("SELECT COALESCE(MAX(id), 0) FROM order_payments", [], |r| r.get(0))?;
            match s.reorg_job(network)? {
                None => {
                    s.conn.execute(
                        "INSERT INTO reorg_jobs (network, fork_height, phase, candidate_max_id,
                             collect_after_height, collect_after_id, created_at_utc, updated_at_utc)
                         VALUES (?1, ?2, 'collect_confirmed', ?3, ?2, 0, ?4, ?4)",
                        params![network, Unsigned(fork_height), max_id, now],
                    )?;
                    Ok(OpenedReorg::Created)
                }
                Some(job) if fork_height < job.fork_height => {
                    s.conn.execute(
                        "UPDATE reorg_jobs SET fork_height = ?2, phase = 'collect_confirmed', candidate_max_id = ?3,
                             collect_after_height = ?2, collect_after_id = 0, updated_at_utc = ?4
                         WHERE network = ?1",
                        params![network, Unsigned(fork_height), max_id.max(job.candidate_max_id), now],
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
    pub fn collect_reorg_candidates(&self, network: &str, limit: usize, now: i64) -> Result<ReorgPhase> {
        self.in_transaction(|s| {
            let job = s.reorg_job(network)?.ok_or(StoreError::NotFound)?;
            let (ids, next) = match job.phase {
                ReorgPhase::CollectConfirmed { after_height, after_id } => {
                    let rows: Vec<(i64, u64)> = s
                        .conn
                        .prepare(
                            "SELECT op.id, op.block_height FROM order_payments op
                             JOIN orders o ON o.id = op.order_id JOIN tenants t ON t.id = o.tenant_id
                             WHERE op.block_height IS NOT NULL AND op.block_height >= ?2
                               AND (op.block_height > ?3 OR (op.block_height = ?3 AND op.id > ?4))
                               AND op.id <= ?5 AND t.network = ?1
                             ORDER BY op.block_height, op.id LIMIT ?6",
                        )?
                        .query_map(
                            params![network, Unsigned(job.fork_height), Unsigned(after_height), after_id,
                                job.candidate_max_id, Unsigned(limit)],
                            |row| Ok((row.get(0)?, unsigned(row, 1)?)),
                        )?
                        .collect::<rusqlite::Result<_>>()?;
                    let next = match rows.last() {
                        Some(&(id, height)) if rows.len() == limit => {
                            ReorgPhase::CollectConfirmed { after_height: height, after_id: id }
                        }
                        _ => ReorgPhase::CollectUnconfirmed { after_id: 0 },
                    };
                    (rows.into_iter().map(|(id, _)| id).collect::<Vec<_>>(), next)
                }
                ReorgPhase::CollectUnconfirmed { after_id } => {
                    let ids: Vec<i64> = s
                        .conn
                        .prepare(
                            "SELECT op.id FROM order_payments op
                             JOIN orders o ON o.id = op.order_id JOIN tenants t ON t.id = o.tenant_id
                             WHERE op.block_height IS NULL AND op.id > ?2 AND op.id <= ?3 AND t.network = ?1
                             ORDER BY op.id LIMIT ?4",
                        )?
                        .query_map(params![network, after_id, job.candidate_max_id, Unsigned(limit)], |row| row.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
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
                    params![network, id],
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
                params![network, phase, Unsigned(after_height), after_id, now],
            )?;
            Ok(next)
        })
    }

    /// Up to `limit` candidates whose retry time has come, oldest retry first.
    pub fn due_reorg_candidates(&self, network: &str, now: i64, limit: usize) -> Result<Vec<ReorgCandidate>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT op.*, w.attempts AS reorg_attempts FROM reorg_work w
             JOIN order_payments op ON op.id = w.payment_id
             WHERE w.network = ?1 AND w.next_attempt_at_utc <= ?2
             ORDER BY w.next_attempt_at_utc, w.payment_id LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![network, now, Unsigned(limit)], |row| {
                Ok(ReorgCandidate {
                    payment: Self::row_to_payment(row)?,
                    attempts: row.get::<_, Unsigned<u32>>("reorg_attempts")?.0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// How many candidates are left, and when the soonest one is due.
    pub fn reorg_work_remaining(&self, network: &str) -> Result<(u64, Option<i64>)> {
        self.conn
            .query_row(
                "SELECT COUNT(*), MIN(next_attempt_at_utc) FROM reorg_work WHERE network = ?1",
                [network],
                |row| Ok((unsigned(row, 0)?, row.get(1)?)),
            )
            .map_err(Into::into)
    }

    /// Removes a candidate. Call inside the transaction that applies its
    /// outcome, so the two can't come apart.
    pub fn complete_reorg_candidate(&self, network: &str, payment_id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM reorg_work WHERE network = ?1 AND payment_id = ?2", params![network, payment_id])?;
        Ok(())
    }

    /// A candidate whose lookup failed: retried later, after the others.
    pub fn defer_reorg_candidate(&self, network: &str, payment_id: i64, now: i64) -> Result<()> {
        let attempts: Option<u32> = self
            .conn
            .query_row(
                "SELECT attempts FROM reorg_work WHERE network = ?1 AND payment_id = ?2",
                params![network, payment_id],
                |row| unsigned(row, 0),
            )
            .optional()?;
        if let Some(attempts) = attempts {
            let attempts = attempts.saturating_add(1);
            self.conn.execute(
                "UPDATE reorg_work SET attempts = ?3, next_attempt_at_utc = ?4 WHERE network = ?1 AND payment_id = ?2",
                params![network, payment_id, attempts, now + reorg_retry_delay(attempts)],
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
    pub fn finish_reorg(&self, network: &str, fork_height: u64, ancestor: Option<(u64, &str)>) -> Result<()> {
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
                params![network, Unsigned(fork_height)],
            )?;
            s.conn.execute(
                "DELETE FROM partial_block_progress WHERE network = ?1 AND height >= ?2",
                params![network, Unsigned(fork_height)],
            )?;
            s.conn.execute("DELETE FROM reorg_jobs WHERE network = ?1", [network])?;
            Ok(())
        })
    }

    /// Whether new settlements on `network` must wait: a reorg is being
    /// reconciled there, so confirmations may be counted on a losing chain.
    pub fn settlement_frozen(&self, network: &str) -> Result<bool> {
        self.conn
            .query_row("SELECT EXISTS (SELECT 1 FROM reorg_jobs WHERE network = ?1)", [network], |row| row.get(0))
            .map_err(Into::into)
    }

    /// Stored block hashes on `network` from `from` to `to` inclusive,
    /// lowest first. Bounded by the retained window.
    pub fn scanned_blocks_between(&self, network: &str, from: u64, to: u64) -> Result<Vec<(u64, String)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT height, block_hash FROM scanned_blocks WHERE network = ?1 AND height BETWEEN ?2 AND ?3 ORDER BY height",
        )?;
        let rows = stmt
            .query_map(params![network, Unsigned(from), Unsigned(to)], |row| {
                Ok((unsigned(row, 0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Up to `limit` orders on `network` whose status may have changed with
    /// time (`next_due_at_utc <= now`) or height (`next_due_height <= tip`),
    /// earliest due first.
    pub fn due_order_ids(&self, network: &str, now: i64, tip: u64, limit: usize) -> Result<Vec<String>> {
        let mut ids: Vec<String> = Vec::new();
        let tip = i64::try_from(tip).unwrap_or(i64::MAX);
        for (sql, due) in [
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
        ] {
            let mut stmt = self.conn.prepare_cached(sql)?;
            let rows = stmt
                .query_map(params![network, due, Unsigned(limit)], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for id in rows {
                if ids.len() < limit && !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        Ok(ids)
    }

    /// A rotation position, if one was recorded. A value that doesn't parse
    /// (a hand-edited row) is logged and treated as absent: the rotation
    /// starts over, which costs repeated work, never skipped work.
    pub fn scheduler_position<P: Position>(&self, network: &str) -> Result<Option<P::Value>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM scheduler_positions WHERE network = ?1 AND position = ?2",
                params![network, P::KEY],
                |row| row.get(0),
            )
            .optional()?;
        Ok(raw.and_then(|raw| match raw.parse() {
            Ok(value) => Some(value),
            Err(_) => {
                tracing::warn!(network, position = P::KEY, value = %raw, "an unreadable scheduler position; starting that rotation over");
                None
            }
        }))
    }

    pub fn set_scheduler_position<P: Position>(&self, network: &str, value: &P::Value) -> Result<()> {
        self.conn.execute(
            "INSERT INTO scheduler_positions (network, position, value) VALUES (?1, ?2, ?3)
             ON CONFLICT (network, position) DO UPDATE SET value = excluded.value",
            params![network, P::KEY, value.to_string()],
        )?;
        Ok(())
    }

    /// A payment by its row id, voided or not.
    pub fn payment_by_id(&self, payment_id: i64) -> Result<Option<OrderPaymentRow>> {
        self.conn
            .query_row("SELECT * FROM order_payments WHERE id = ?1", [payment_id], Self::row_to_payment)
            .optional()
            .map_err(Into::into)
    }

    /// Up to `limit` payments on `network` voided no earlier than `cutoff`,
    /// after payment id `after`, in id order: one page of the slow recheck
    /// for false double-spend accusations.
    pub fn voided_payments_page(&self, network: &str, cutoff: i64, after: i64, limit: usize) -> Result<Vec<OrderPaymentRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT op.* FROM order_payments op
             JOIN orders o ON o.id = op.order_id JOIN tenants t ON t.id = o.tenant_id
             WHERE op.voided_at_utc IS NOT NULL AND op.voided_at_utc >= ?2 AND op.id > ?3 AND t.network = ?1
             ORDER BY op.id LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![network, cutoff, after, Unsigned(limit)], Self::row_to_payment)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
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
    pub order_id: String,
    pub txid: String,
    pub output_index: i64,
    pub amount_piconero: u64,
    pub key_images_json: String,
    pub seen_at: i64,
}

impl Store {
    pub fn block_checkpoint(&self, network: &str, tenant_id: &str) -> Result<Option<BlockCheckpoint>> {
        self.conn
            .query_row(
                "SELECT height, block_hash, next_tx_index FROM partial_block_progress WHERE network = ?1 AND tenant_id = ?2",
                params![network, tenant_id],
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
    /// other block (a different height or hash) is replaced, with its staged
    /// matches. Call in the transaction that stages this block's matches.
    pub fn save_block_checkpoint(&self, network: &str, tenant_id: &str, checkpoint: &BlockCheckpoint) -> Result<()> {
        if let Some(old) = self.block_checkpoint(network, tenant_id)? {
            if old.height != checkpoint.height || old.hash != checkpoint.hash {
                self.clear_partial_block(network, tenant_id)?;
            }
        }
        self.conn.execute(
            "INSERT INTO partial_block_progress (network, tenant_id, height, block_hash, window_generation, next_tx_index)
             VALUES (?1, ?2, ?3, ?4, '', ?5)
             ON CONFLICT (network, tenant_id) DO UPDATE SET
                 height = excluded.height, block_hash = excluded.block_hash, next_tx_index = excluded.next_tx_index",
            params![network, tenant_id, Unsigned(checkpoint.height), checkpoint.hash, Unsigned(checkpoint.next_tx)],
        )?;
        Ok(())
    }

    /// Removes a tenant's checkpoint and returns its staged matches if it was
    /// for this block (height and hash); a stale one is dropped.
    pub fn take_staged_payments(&self, network: &str, tenant_id: &str, height: u64, hash: &str) -> Result<Vec<StagedPayment>> {
        let current = self.block_checkpoint(network, tenant_id)?.is_some_and(|c| c.height == height && c.hash == hash);
        let staged = if current {
            let mut stmt = self.conn.prepare_cached(
                "SELECT order_id, txid, output_index, amount_piconero, key_images_json, seen_at_utc
                 FROM partial_block_matches WHERE network = ?1 AND tenant_id = ?2",
            )?;
            let rows = stmt
                .query_map(params![network, tenant_id], |row| {
                    Ok(StagedPayment {
                        order_id: row.get(0)?,
                        txid: row.get(1)?,
                        output_index: row.get(2)?,
                        amount_piconero: unsigned(row, 3)?,
                        key_images_json: row.get(4)?,
                        seen_at: row.get(5)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        } else {
            Vec::new()
        };
        self.clear_partial_block(network, tenant_id)?;
        Ok(staged)
    }

    /// Up to `limit` distinct cursor heights below `below` held by enabled
    /// tenants on `network`, after `after` (all, from the lowest, for
    /// `None`): the catch-up groups, in rotation order.
    pub fn scan_group_cursors(&self, network: &str, below: u64, after: Option<u64>, limit: usize) -> Result<Vec<u64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT scanned_through_height FROM tenants
             WHERE network = ?1 AND disabled_at_utc IS NULL AND scanned_through_height IS NOT NULL
               AND scanned_through_height < ?2 AND scanned_through_height >= ?3
             ORDER BY scanned_through_height LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(
                params![network, Unsigned(below), Unsigned(after.map_or(0, |a| a.saturating_add(1))), Unsigned(limit)],
                |row| unsigned(row, 0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Up to `limit` enabled tenants on `network` whose cursor is `cursor`,
    /// leaving out `excluding` (tenants waiting out a retry delay), in id
    /// order.
    pub fn tenants_at_cursor(&self, network: &str, cursor: u64, excluding: &[String], limit: usize) -> Result<Vec<String>> {
        let excluding = serde_json::to_string(excluding)
            .map_err(|e| StoreError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e))))?;
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM tenants
             WHERE network = ?1 AND disabled_at_utc IS NULL AND scanned_through_height = ?2
               AND id NOT IN (SELECT value FROM json_each(?3))
             ORDER BY id LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![network, Unsigned(cursor), excluding, Unsigned(limit)], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The scan windows of several tenants at once (see `scan_window`), in
    /// one query: minor indices by tenant, each list ascending. A tenant with
    /// nothing in scope is absent.
    pub fn scan_windows(
        &self, tenant_ids: &[String], since: i64, grace_period_seconds: i64,
    ) -> Result<std::collections::HashMap<String, Vec<u32>>> {
        let ids = serde_json::to_string(tenant_ids)
            .map_err(|e| StoreError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e))))?;
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT tenant_id, minor_index FROM orders WHERE id IN ({}) ORDER BY tenant_id, minor_index",
            super::scan_window_orders("o.tenant_id IN (SELECT value FROM json_each(:ids))")
        ))?;
        let mut windows: std::collections::HashMap<String, Vec<u32>> = std::collections::HashMap::new();
        let rows = stmt.query_map(
            rusqlite::named_params! { ":ids": ids, ":since_minus_grace": since.saturating_sub(grace_period_seconds) },
            |row| Ok((row.get::<_, String>(0)?, unsigned(row, 1)?)),
        )?;
        for row in rows {
            let (tenant_id, minor) = row?;
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
    pub fn advance_idle_cursors(&self, network: &str, from: u64, to: u64, since: i64, grace_period_seconds: i64) -> Result<usize> {
        let moved = self.conn.execute(
            &format!(
                "UPDATE tenants SET scanned_through_height = :to
                 WHERE network = :network AND disabled_at_utc IS NULL AND scanned_through_height = :from
                   AND NOT {}",
                super::tenant_in_scope("tenants.id")
            ),
            rusqlite::named_params! {
                ":network": network,
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
        &self, network: &str, height: u64, scanned: &[crate::work::ScannedBlock],
    ) -> Result<std::collections::HashSet<String>> {
        let ids: Vec<&str> = scanned.iter().filter(|b| b.height() == height).map(|b| b.tenant_id()).collect();
        if ids.is_empty() {
            return Ok(Default::default());
        }
        let ids = serde_json::to_string(&ids)
            .map_err(|e| StoreError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e))))?;
        let mut stmt = self.conn.prepare_cached(
            "UPDATE tenants SET scanned_through_height = ?2
             WHERE network = ?1 AND scanned_through_height = ?2 - 1 AND id IN (SELECT value FROM json_each(?3))
             RETURNING id",
        )?;
        let moved = stmt
            .query_map(params![network, Unsigned(height), ids], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(moved)
    }

    /// Up to `limit` enabled tenants on `network` with an order in scope,
    /// after `after` in id order, with their cursors: one page of the
    /// scanned-range bookkeeping.
    pub fn active_tenants_page(
        &self, network: &str, now: i64, grace_period_seconds: i64, after: &str, limit: usize,
    ) -> Result<Vec<(String, Option<u64>)>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT t.id, t.scanned_through_height FROM tenants t
             WHERE t.network = :network AND t.disabled_at_utc IS NULL AND t.id > :after AND {}
             ORDER BY t.id LIMIT :limit",
            super::tenant_in_scope("t.id")
        ))?;
        let rows = stmt
            .query_map(
                rusqlite::named_params! {
                    ":network": network,
                    ":after": after,
                    ":limit": Unsigned(limit),
                    ":since_minus_grace": now - grace_period_seconds,
                },
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<Unsigned<u64>>>(1)?.map(|h| h.0))),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
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
    }

    fn order(store: &Store, tenant_id: &str, expires_at: i64) -> String {
        let index = store.allocate_minor_index(tenant_id).unwrap();
        store
            .create_order(NewOrder {
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
    }

    fn pay(store: &Store, order_id: &str, txid: &str, height: Option<i64>) -> i64 {
        store.record_payment_match(order_id, txid, 0, 10, "[\"ki\"]", 100, height).unwrap();
        store.get_all_payments(order_id).unwrap().into_iter().find(|p| p.txid == txid).unwrap().id
    }

    fn work(store: &Store, network: &str) -> Vec<i64> {
        store.due_reorg_candidates(network, i64::MAX, 1000).unwrap().into_iter().map(|c| c.payment.id).collect()
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
        let above: Vec<i64> = (0..5).map(|i| pay(s, main_order, &format!("above{i}"), Some(11 + i))).collect();
        let unconfirmed = pay(s, main_order, "pool", None);
        let _other_network = pay(s, other_order, "other", Some(12));
        assert_eq!(s.open_reorg_job("mainnet", 10, 1000).unwrap(), OpenedReorg::Created);
        let late = pay(s, main_order, "late", Some(12));

        // Two candidates per page, restarting the process between pages.
        assert!(matches!(s.collect_reorg_candidates("mainnet", 2, 1001).unwrap(), ReorgPhase::CollectConfirmed { .. }));
        let s = reopen(store, &path);
        let mut phase = s.collect_reorg_candidates("mainnet", 2, 1002).unwrap();
        while phase != ReorgPhase::Process {
            phase = s.collect_reorg_candidates("mainnet", 2, 1003).unwrap();
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
        s.open_reorg_job("mainnet", 8, 1000).unwrap();
        while s.collect_reorg_candidates("mainnet", 10, 1000).unwrap() != ReorgPhase::Process {}
        assert_eq!(work(s, "mainnet"), vec![shallow]);
        assert_eq!(s.open_reorg_job("mainnet", 9, 1001).unwrap(), OpenedReorg::Covered);
        assert_eq!(s.open_reorg_job("mainnet", 4, 1002).unwrap(), OpenedReorg::Deepened { from: 8 });
        while s.collect_reorg_candidates("mainnet", 10, 1003).unwrap() != ReorgPhase::Process {}
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
            s.set_scanned_block("mainnet", h, &format!("old{h}")).unwrap();
        }
        s.open_reorg_job("mainnet", 10, 1000).unwrap();
        while s.collect_reorg_candidates("mainnet", 10, 1000).unwrap() != ReorgPhase::Process {}
        assert!(s.settlement_frozen("mainnet").unwrap());
        assert!(!s.settlement_frozen("stagenet").unwrap());

        // The first retries are due at once; the third failure waits.
        for _ in 0..3 {
            s.defer_reorg_candidate("mainnet", first, 1000).unwrap();
        }
        assert_eq!((reorg_retry_delay(1), reorg_retry_delay(2), reorg_retry_delay(3)), (0, 0, 1));
        let due: Vec<i64> = s.due_reorg_candidates("mainnet", 1000, 10).unwrap().iter().map(|c| c.payment.id).collect();
        assert_eq!(due, vec![second], "the failed one waits");
        assert!(matches!(s.finish_reorg("mainnet", 10, Some((9, "old9"))), Err(StoreError::NotFound)));
        s.complete_reorg_candidate("mainnet", second).unwrap();
        let later = s.due_reorg_candidates("mainnet", 1000 + reorg_retry_delay(3), 10).unwrap();
        assert_eq!(later.len(), 1);
        assert_eq!((later[0].payment.id, later[0].attempts), (first, 3));
        s.complete_reorg_candidate("mainnet", first).unwrap();

        assert!(matches!(s.finish_reorg("mainnet", 9, Some((8, "old8"))), Err(StoreError::NotFound)), "wrong fork");
        s.finish_reorg("mainnet", 10, Some((9, "old9"))).unwrap();
        assert_eq!(s.max_scanned_height("mainnet").unwrap(), Some(9));
        assert!(s.reorg_job("mainnet").unwrap().is_none());
        assert!(!s.settlement_frozen("mainnet").unwrap());
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
        s.set_scanned_block("mainnet", 20, "old20").unwrap();
        s.set_scanned_block("mainnet", 21, "old21").unwrap();
        s.execute_raw_for_test("UPDATE tenants SET scanned_through_height = 21").unwrap();
        s.open_reorg_job("mainnet", 20, 1000).unwrap();
        while s.collect_reorg_candidates("mainnet", 10, 1000).unwrap() != ReorgPhase::Process {}
        s.finish_reorg("mainnet", 20, Some((19, "new19"))).unwrap();
        assert_eq!(s.scanned_blocks_between("mainnet", 0, 100).unwrap(), vec![(19, "new19".to_string())]);
        let cursors: Vec<Option<u64>> = s.list_active_tenants().unwrap().into_iter()
            .filter(|t| t.network == "mainnet").map(|t| t.scanned_through_height).collect();
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
        assert_eq!(s.due_order_ids("mainnet", 4_999, 0, 10).unwrap(), Vec::<String>::new());
        assert_eq!(s.due_order_ids("mainnet", 5_000, 0, 10).unwrap(), vec![expiring.clone()]);

        // Fully paid at height 50 with ten confirmations needed: due again
        // each block until it settles, then never.
        s.record_payment_match(&expiring, "tx", 0, 100, "[\"ki\"]", 1_000, Some(50)).unwrap();
        s.recompute_order_status(&expiring, 52, 1_000).unwrap();
        assert!(s.due_order_ids("mainnet", 1_000, 52, 10).unwrap().is_empty());
        assert_eq!(s.due_order_ids("mainnet", 1_000, 53, 10).unwrap(), vec![expiring.clone()]);
        assert!(s.due_order_ids("mainnet", 9_999, 52, 10).unwrap().is_empty(), "no deadline once fully paid");
        s.recompute_order_status(&expiring, 59, 1_000).unwrap();
        assert!(!s.due_order_ids("mainnet", i64::MAX, u64::MAX, 10).unwrap().contains(&expiring), "settled");
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
        s.record_payment_match(&o, "tx", 0, 100, "[\"ki\"]", 1_000, Some(50)).unwrap();
        s.open_reorg_job("mainnet", 70, 1_000).unwrap();
        let (_, frozen) = s.recompute_order_status(&o, 59, 1_000).unwrap();
        assert_eq!(frozen, crate::status::OrderStatus::Confirming);
        assert_eq!(s.pending_payment_recomputes("mainnet").unwrap(), vec![o.clone()]);
        assert_eq!(s.due_order_ids("mainnet", 1_000, 0, 10).unwrap(), vec![o.clone()], "due again at once");

        while s.collect_reorg_candidates("mainnet", 10, 1000).unwrap() != ReorgPhase::Process {}
        s.finish_reorg("mainnet", 70, Some((69, "h69"))).unwrap();
        let (_, settled) = s.recompute_order_status(&o, 59, 1_000).unwrap();
        assert_eq!(settled, crate::status::OrderStatus::Paid);
        assert!(s.pending_payment_recomputes("mainnet").unwrap().is_empty());
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
            let mut stmt = store.conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
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
        store.0.set_scheduler_position::<CatchUpGroup>("mainnet", &42).unwrap();
        store.0.set_scheduler_position::<CatchUpGroup>("mainnet", &43).unwrap();
        store.0.set_scheduler_position::<ScanRange>("mainnet", &"tn_x".to_string()).unwrap();
        let s = reopen(store, &path);
        assert_eq!(s.scheduler_position::<CatchUpGroup>("mainnet").unwrap(), Some(43));
        assert_eq!(s.scheduler_position::<ScanRange>("mainnet").unwrap().as_deref(), Some("tn_x"));
        assert_eq!(s.scheduler_position::<CatchUpGroup>("stagenet").unwrap(), None);
        assert_eq!(s.scheduler_position::<VoidRecheck>("mainnet").unwrap(), None);
        // A hand-edited, unreadable value starts that rotation over.
        s.execute_raw_for_test("UPDATE scheduler_positions SET value = 'x' WHERE position = 'catch_up_group'").unwrap();
        assert_eq!(s.scheduler_position::<CatchUpGroup>("mainnet").unwrap(), None);
        drop(s);
        cleanup(&path);
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
        assert!(matches!(read("SELECT -1"), Err(rusqlite::Error::IntegralValueOutOfRange(0, -1))));
        // A value too big for SQLite's signed integers is refused on the way in.
        let wrote = conn.query_row("SELECT ?1", [Unsigned(u64::MAX)], |row| row.get::<_, i64>(0));
        assert!(matches!(wrote, Err(rusqlite::Error::ToSqlConversionFailure(_))));
        let wrote = conn.query_row("SELECT ?1", [Unsigned(7usize)], |row| row.get::<_, i64>(0));
        assert_eq!(wrote.unwrap(), 7);
    }
}
