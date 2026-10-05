//! Durable state for the scanner's work units (`docs/scanner_microtasks.md)`:
//! the reorg job, the recompute schedule, and rotation positions. Every
//! read here is a bounded page; every multi-row write is one transaction.

use super::{OrderId, TenantId};
use rusqlite::{params, OptionalExtension as _};

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
    serde_json::Value::from(ids.iter().map(AsRef::as_ref).collect::<Vec<&str>>()).to_string()
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

/// A scheduler position kept across restarts, one row per
/// network per position.
///
/// Each is a type with its own value type, so a position can't be read
/// as something it isn't. The set is closed: one type per scheduler role,
/// never one per tenant.
pub trait Position {
    const KEY: &'static str;
    type Value: std::str::FromStr + ToString;
}

/// Positions by name.
pub mod position {
    use super::Position;

    /// Last node height observed by settlement, even above the scanned chain.
    pub struct SettlementTip;
    impl Position for SettlementTip {
        const KEY: &'static str = "settlement_tip";
        type Value = u64;
    }

    /// The fork point covered by the remembered replacement branch.
    pub struct ReorgBranchFork;
    impl Position for ReorgBranchFork {
        const KEY: &'static str = "reorg_branch_fork";
        type Value = u64;
    }

    /// A fixed height on the replacement branch being reconciled. Extensions
    /// don't change its hash, but another fork at or below it does.
    pub struct ReorgBranchHeight;
    impl Position for ReorgBranchHeight {
        const KEY: &'static str = "reorg_branch_height";
        type Value = u64;
    }

    /// The hash at `ReorgBranchHeight`, recorded atomically with it.
    pub struct ReorgBranchHash;
    impl Position for ReorgBranchHash {
        const KEY: &'static str = "reorg_branch_hash";
        type Value = String;
    }

    /// The last catch-up group served (a tenant cursor height).
    pub struct CatchUpGroup;
    impl Position for CatchUpGroup {
        const KEY: &'static str = "catch_up_group";
        type Value = u64;
    }

    /// Last tenant offered a block page, including unsuccessful attempts.
    /// A durable cyclic position prevents a failing first page hiding later tenants.
    pub struct BlockTenantPage;
    impl Position for BlockTenantPage {
        const KEY: &'static str = "block_tenant_page";
        type Value = String;
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
pub(super) fn reorg_retry_delay(attempts: u32) -> i64 {
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
    /// Confirmation schedules normally wake on increasing heights. A shorter
    /// tip can invalidate their depth even if the fork is above every recorded
    /// block, where hash-based reorg detection has nothing to compare. Keep
    /// the height and its recompute obligations in one durable transaction.
    pub fn observe_settlement_tip(&self, network: monero::Network, tip: u64) -> Result<()> {
        self.in_transaction(|s| {
            let previous = s.scheduler_position::<position::SettlementTip>(network)?;
            if previous.is_none_or(|previous| tip < previous) {
                // Also repair existing orders on first use of this position.
                s.enqueue_mined_payment_recomputes(network)?;
            }
            if previous != Some(tip) {
                s.set_scheduler_position::<position::SettlementTip>(network, &tip)?;
            }
            Ok(())
        })
    }

    fn enqueue_mined_payment_recomputes(&self, network: monero::Network) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO pending_payment_recomputes (order_id)
             SELECT DISTINCT p.order_id FROM order_payments p
             JOIN orders o ON o.id = p.order_id
             JOIN tenants t ON t.id = o.tenant_id
             WHERE t.network = ?1 AND p.block_height IS NOT NULL AND p.voided_at_utc IS NULL",
            [shared::network::SqlNetwork(network)],
        )?;
        Ok(())
    }
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
                    s.clear_reorg_branch(network)?;
                    s.conn.execute(
                        "INSERT INTO reorg_jobs (network, fork_height, phase, candidate_max_id,
                             collect_after_height, collect_after_id, created_at_utc, updated_at_utc)
                         VALUES (?1, ?2, 'collect_confirmed', ?3, ?2, 0, ?4, ?4)",
                        params![shared::network::SqlNetwork(network), Unsigned(fork_height), max_id, now],
                    )?;
                    Ok(OpenedReorg::Created)
                }
                Some(job) if fork_height < job.fork_height => {
                    s.clear_reorg_branch(network)?;
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

    /// The replacement branch used by the latest reconciliation, retained until
    /// rescanning reaches it. Older open jobs lack it and are safely recollected.
    pub fn reorg_branch(&self, network: monero::Network) -> Result<Option<(u64, String)>> {
        let height = self.scheduler_position::<position::ReorgBranchHeight>(network)?;
        let hash = self.scheduler_position::<position::ReorgBranchHash>(network)?;
        Ok(height.zip(hash))
    }

    /// Forget branch tracking once that range was rescanned (or genesis changed).
    pub fn clear_reorg_branch(&self, network: monero::Network) -> Result<()> {
        self.conn.execute(
            "DELETE FROM scheduler_positions WHERE network = ?1 AND position IN (?2, ?3, ?4)",
            params![
                shared::network::SqlNetwork(network),
                position::ReorgBranchHeight::KEY,
                position::ReorgBranchHash::KEY,
                position::ReorgBranchFork::KEY
            ],
        )?;
        Ok(())
    }

    /// Extend the remembered branch without restarting collection. This keeps
    /// new candidates reconciled at higher heights covered by the branch check.
    pub fn extend_reorg_branch(
        &self,
        network: monero::Network,
        height: u64,
        hash: &str,
    ) -> Result<()> {
        self.in_transaction(|s| {
            let job = s.reorg_job(network)?.ok_or(StoreError::NotFound)?;
            s.set_scheduler_position::<position::ReorgBranchFork>(network, &job.fork_height)?;
            s.set_scheduler_position::<position::ReorgBranchHeight>(network, &height)?;
            s.set_scheduler_position::<position::ReorgBranchHash>(network, &hash.to_owned())
        })
    }

    /// Revisit all candidates when the replacement branch changes, including
    /// payments already processed on the previous branch. The branch identity,
    /// collection restart and discarded retries commit together.
    pub fn restart_reorg_for_branch(
        &self,
        network: monero::Network,
        height: u64,
        hash: &str,
        now: i64,
    ) -> Result<()> {
        self.in_transaction(|s| {
            let job = s.reorg_job(network)?.ok_or(StoreError::NotFound)?;
            s.conn.execute(
                "UPDATE reorg_jobs SET phase = 'collect_confirmed',
                     candidate_max_id = MAX(candidate_max_id, (SELECT COALESCE(MAX(id), 0) FROM order_payments)),
                     collect_after_height = fork_height, collect_after_id = 0, updated_at_utc = ?2
                 WHERE network = ?1",
                params![shared::network::SqlNetwork(network), now],
            )?;
            s.conn.execute("DELETE FROM reorg_work WHERE network = ?1", [shared::network::SqlNetwork(network)])?;
            s.set_scheduler_position::<position::ReorgBranchHeight>(network, &height)?;
            s.set_scheduler_position::<position::ReorgBranchHash>(network, &hash.to_owned())?;
            s.set_scheduler_position::<position::ReorgBranchFork>(network, &job.fork_height)?;
            // Keep the job's original fork and creation time: settlement stays
            // frozen, and collection still covers every affected payment.
            Ok(())
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
        let result = self.in_transaction(|s| {
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
            // A replacement branch can be shorter. Even payments whose
            // height stayed unchanged (including those below the fork) then
            // have fewer confirmations. Their next-height schedule only
            // notices growing tips, so persist an explicit recompute.
            s.enqueue_mined_payment_recomputes(network)?;
            // Payments may have moved to replacement blocks not yet rescanned.
            // Keep their branch identity until scanning covers it, so another
            // fork in that gap cannot strand a height from the discarded branch.
            if ancestor.is_none() { s.clear_reorg_branch(network)?; }
            #[cfg(test)]
            super::crash_checkpoint("reorg.before_commit");
            Ok(())
        });
        #[cfg(test)]
        if result.is_ok() {
            super::crash_checkpoint("reorg.after_commit");
        }
        result
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
        for list in &mut found {
            take(list, share);
        }
        for list in &mut found {
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
        Ok(raw.and_then(|raw| if let Ok(value) = raw.parse() { Some(value) } else {
            tracing::warn!(network = crate::network::network_str(network), position = P::KEY, value = %raw, "an unreadable scheduler position; starting that rotation over");
            None
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
             WHERE op.voided_at_utc IS NOT NULL AND op.superseded_by IS NULL
               AND op.voided_at_utc >= ?2 AND op.id > ?3 AND t.network = ?1
             ORDER BY op.id LIMIT ?4",
            params![
                shared::network::SqlNetwork(network),
                cutoff,
                after,
                Unsigned(limit)
            ],
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

    /// How many catch-up groups `network` has: distinct cursor heights below
    /// `below` held by enabled tenants, as [`Self::scan_group_cursors`]
    /// lists them.
    pub fn count_scan_groups(&self, network: monero::Network, below: u64) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(DISTINCT scanned_through_height) FROM tenants
             WHERE network = ?1 AND disabled_at_utc IS NULL AND scanned_through_height IS NOT NULL
               AND scanned_through_height < ?2",
            params![shared::network::SqlNetwork(network), Unsigned(below)],
            |row| unsigned(row, 0),
        )?)
    }

    /// Up to `limit` enabled tenants on `network` whose cursor is `cursor`,
    /// leaving out `excluding` (tenants waiting out a retry delay), in id
    /// order, rotated after the last block page offered on this network.
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
             ORDER BY (id <= ?5), id LIMIT ?4",
            params![
                shared::network::SqlNetwork(network),
                Unsigned(cursor),
                excluding,
                Unsigned(limit),
                self.scheduler_position::<position::BlockTenantPage>(network)?
                    .unwrap_or_default()
            ],
            |row| row.get::<_, TenantId>(0),
        )
    }

    /// A catch-up page of tenants whose keys are registered. Filtering in SQL,
    /// before LIMIT, prevents unregistered tenants from hiding later members.
    pub fn registered_tenants_at_cursor(
        &self,
        network: monero::Network,
        cursor: u64,
        excluding: &[TenantId],
        registered: &[TenantId],
        limit: usize,
    ) -> Result<Vec<TenantId>> {
        self.rows(
            "SELECT id FROM tenants
             WHERE network = ?1 AND disabled_at_utc IS NULL AND scanned_through_height = ?2
               AND id NOT IN (SELECT value FROM json_each(?3))
               AND id IN (SELECT value FROM json_each(?4))
             ORDER BY (id <= ?6), id LIMIT ?5",
            params![
                shared::network::SqlNetwork(network),
                Unsigned(cursor),
                json_array(excluding),
                json_array(registered),
                Unsigned(limit),
                self.scheduler_position::<position::BlockTenantPage>(network)?
                    .unwrap_or_default()
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
            .map(crate::work::ScannedBlock::tenant_id)
            .collect();
        if ids.is_empty() {
            return Ok(std::collections::HashSet::default());
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

/// What the engine page's snapshot reads from the database
/// (`docs/engine_visualizer.md`): each read is one indexed query.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ActivityFacts {
    pub high_water: Option<u64>,
    /// `(cursor, stores)`, highest cursor first, at most the limit asked.
    pub groups: Vec<(u64, u64)>,
    /// Distinct cursors in all.
    pub all_groups: u64,
    /// Heights with a block scan saved partway.
    pub checkpoints: Vec<u64>,
    /// The open reorg job: its fork, whether it is still collecting, and
    /// the candidates left to re-examine.
    pub reorg: Option<(u64, bool, u64)>,
    pub recomputes_pending: u64,
    pub orders_due: u64,
    pub webhooks_due: u64,
    /// When each delivery since the time asked for was made.
    pub delivered_at: Vec<i64>,
}

impl Store {
    /// The engine page's database facts for `network`: groups of stores by
    /// cursor (up to `groups`), saved partial scans, the reorg job, what
    /// settlement has waiting, and webhook deliveries due and made since
    /// `delivered_since`. `tip` is the node's height, for orders due by
    /// height.
    pub fn activity_facts(
        &self,
        network: monero::Network,
        now: i64,
        tip: Option<u64>,
        groups: usize,
        delivered_since: i64,
    ) -> Result<ActivityFacts> {
        let net = shared::network::SqlNetwork(network);
        let count = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> Result<u64> {
            Ok(self
                .conn
                .prepare_cached(sql)?
                .query_row(params, |row| unsigned(row, 0))?)
        };
        let tip = i64::try_from(tip.unwrap_or(0)).unwrap_or(i64::MAX);
        let reorg = match self.reorg_job(network)? {
            Some(job) => Some((
                job.fork_height,
                !matches!(job.phase, ReorgPhase::Process),
                self.reorg_work_remaining(network)?.0,
            )),
            None => None,
        };
        Ok(ActivityFacts {
            high_water: self.max_scanned_height(network)?,
            groups: self.rows(
                "SELECT scanned_through_height, COUNT(*) FROM tenants
                 WHERE network = ?1 AND disabled_at_utc IS NULL AND scanned_through_height IS NOT NULL
                 GROUP BY scanned_through_height ORDER BY scanned_through_height DESC LIMIT ?2",
                params![net, Unsigned(groups)],
                |row| Ok((unsigned(row, 0)?, unsigned(row, 1)?)),
            )?,
            all_groups: count(
                "SELECT COUNT(DISTINCT scanned_through_height) FROM tenants
                 WHERE network = ?1 AND disabled_at_utc IS NULL AND scanned_through_height IS NOT NULL",
                &[&net],
            )?,
            checkpoints: self.rows(
                "SELECT DISTINCT height FROM partial_block_progress WHERE network = ?1 ORDER BY height",
                params![net],
                |row| unsigned(row, 0),
            )?,
            reorg,
            recomputes_pending: count(
                "SELECT COUNT(*) FROM pending_payment_recomputes p
                 JOIN orders o ON o.id = p.order_id JOIN tenants t ON t.id = o.tenant_id
                 WHERE t.network = ?1",
                &[&net],
            )?,
            // Due by time, then due by height and not by time: each order
            // once, each count from its own index.
            orders_due: count(
                "SELECT COUNT(*) FROM orders o JOIN tenants t ON t.id = o.tenant_id
                 WHERE o.next_due_at_utc IS NOT NULL AND o.next_due_at_utc <= ?2 AND t.network = ?1",
                &[&net, &now],
            )? + count(
                "SELECT COUNT(*) FROM orders o JOIN tenants t ON t.id = o.tenant_id
                 WHERE o.next_due_height IS NOT NULL AND o.next_due_height <= ?2 AND t.network = ?1
                   AND (o.next_due_at_utc IS NULL OR o.next_due_at_utc > ?3)",
                &[&net, &tip, &now],
            )?,
            webhooks_due: count(
                "SELECT COUNT(*) FROM webhook_deliveries d
                 JOIN orders o ON o.id = d.order_id JOIN tenants t ON t.id = o.tenant_id
                 WHERE d.delivered_at_utc IS NULL AND d.gave_up_at_utc IS NULL
                   AND d.next_attempt_at_utc <= ?2 AND t.network = ?1",
                &[&net, &now],
            )?,
            delivered_at: self.rows(
                "SELECT d.delivered_at_utc FROM webhook_deliveries d
                 JOIN orders o ON o.id = d.order_id JOIN tenants t ON t.id = o.tenant_id
                 WHERE d.delivered_at_utc IS NOT NULL AND d.delivered_at_utc >= ?2 AND t.network = ?1
                 ORDER BY d.delivered_at_utc",
                params![net, delivered_since],
                |row| row.get(0),
            )?,
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/internal/store/work_tests.rs"]
mod tests;
