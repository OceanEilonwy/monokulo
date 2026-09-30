//! Durable state for the scanner's work units (docs/scanner_microtasks.md):
//! the reorg job, the recompute schedule, and rotation positions. Every
//! read here is a bounded page; every multi-row write is one transaction.

use rusqlite::{params, OptionalExtension};

use super::{OrderPaymentRow, Result, Store, StoreError};

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

/// Rotation positions the scheduler keeps across restarts. A closed set,
/// so the table stays one row per network per variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    /// The next catch-up group to serve (a tenant cursor height).
    CatchUpGroup,
    /// The last unconfirmed payment checked for having left the pool.
    VanishedPayments,
    /// The last voided payment rechecked for a false double-spend.
    VoidRecheck,
    /// When the last full void recheck pass began (unix time).
    VoidRecheckPassStarted,
    /// The last tenant whose orders' scanned range was brought up to date.
    ScanRange,
}

impl Position {
    fn key(self) -> &'static str {
        match self {
            Self::CatchUpGroup => "catch_up_group",
            Self::VanishedPayments => "vanished_payments",
            Self::VoidRecheck => "void_recheck",
            Self::VoidRecheckPassStarted => "void_recheck_pass_started",
            Self::ScanRange => "scan_range",
        }
    }
}

/// How long a failed reorg lookup waits before it is retried: doubling
/// from one second, capped at five minutes.
pub fn reorg_retry_delay(attempts: u32) -> i64 {
    1i64 << attempts.min(8)
}

fn phase_from_row(phase: &str, after_height: i64, after_id: i64) -> rusqlite::Result<ReorgPhase> {
    Ok(match phase {
        "collect_confirmed" => ReorgPhase::CollectConfirmed { after_height: after_height as u64, after_id },
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
                        fork_height: row.get::<_, i64>(1)? as u64,
                        phase: phase_from_row(&row.get::<_, String>(2)?, row.get(4)?, row.get(5)?)?,
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
                        params![network, fork_height as i64, max_id, now],
                    )?;
                    Ok(OpenedReorg::Created)
                }
                Some(job) if fork_height < job.fork_height => {
                    s.conn.execute(
                        "UPDATE reorg_jobs SET fork_height = ?2, phase = 'collect_confirmed', candidate_max_id = ?3,
                             collect_after_height = ?2, collect_after_id = 0, updated_at_utc = ?4
                         WHERE network = ?1",
                        params![network, fork_height as i64, max_id.max(job.candidate_max_id), now],
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
                    let rows: Vec<(i64, i64)> = s
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
                            params![network, job.fork_height as i64, after_height as i64, after_id,
                                job.candidate_max_id, limit as i64],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )?
                        .collect::<rusqlite::Result<_>>()?;
                    let next = match rows.last() {
                        Some(&(id, height)) if rows.len() == limit => {
                            ReorgPhase::CollectConfirmed { after_height: height as u64, after_id: id }
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
                        .query_map(params![network, after_id, job.candidate_max_id, limit as i64], |row| row.get(0))?
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
                ReorgPhase::CollectConfirmed { after_height, after_id } => ("collect_confirmed", after_height as i64, after_id),
                ReorgPhase::CollectUnconfirmed { after_id } => ("collect_unconfirmed", 0, after_id),
                ReorgPhase::Process => ("process", 0, 0),
            };
            s.conn.execute(
                "UPDATE reorg_jobs SET phase = ?2, collect_after_height = ?3, collect_after_id = ?4, updated_at_utc = ?5
                 WHERE network = ?1",
                params![network, phase, after_height, after_id, now],
            )?;
            Ok(next)
        })
    }

    /// Up to `limit` candidates whose retry time has come, oldest retry first.
    pub fn due_reorg_candidates(&self, network: &str, now: i64, limit: usize) -> Result<Vec<ReorgCandidate>> {
        let mut stmt = self.conn.prepare(
            "SELECT op.*, w.attempts AS reorg_attempts FROM reorg_work w
             JOIN order_payments op ON op.id = w.payment_id
             WHERE w.network = ?1 AND w.next_attempt_at_utc <= ?2
             ORDER BY w.next_attempt_at_utc, w.payment_id LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![network, now, limit as i64], |row| {
                Ok(ReorgCandidate {
                    payment: Self::row_to_payment(row)?,
                    attempts: row.get::<_, i64>("reorg_attempts")?.max(0) as u32,
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
                |row| Ok((row.get::<_, i64>(0)?.max(0) as u64, row.get(1)?)),
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
        let attempts: Option<i64> = self
            .conn
            .query_row(
                "SELECT attempts FROM reorg_work WHERE network = ?1 AND payment_id = ?2",
                params![network, payment_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(attempts) = attempts {
            let attempts = attempts.max(0) as u32 + 1;
            self.conn.execute(
                "UPDATE reorg_work SET attempts = ?3, next_attempt_at_utc = ?4 WHERE network = ?1 AND payment_id = ?2",
                params![network, payment_id, attempts as i64, now + reorg_retry_delay(attempts)],
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
                params![network, fork_height as i64],
            )?;
            s.conn.execute(
                "DELETE FROM partial_block_progress WHERE network = ?1 AND height >= ?2",
                params![network, fork_height as i64],
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
        let mut stmt = self.conn.prepare(
            "SELECT height, block_hash FROM scanned_blocks WHERE network = ?1 AND height BETWEEN ?2 AND ?3 ORDER BY height",
        )?;
        let rows = stmt
            .query_map(params![network, from as i64, to as i64], |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
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
            let mut stmt = self.conn.prepare(sql)?;
            let rows = stmt
                .query_map(params![network, due, limit as i64], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for id in rows {
                if ids.len() < limit && !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        Ok(ids)
    }

    pub fn scheduler_position(&self, network: &str, position: Position) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM scheduler_positions WHERE network = ?1 AND position = ?2",
                params![network, position.key()],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_scheduler_position(&self, network: &str, position: Position, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO scheduler_positions (network, position, value) VALUES (?1, ?2, ?3)
             ON CONFLICT (network, position) DO UPDATE SET value = excluded.value",
            params![network, position.key(), value],
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
        let mut stmt = self.conn.prepare(
            "SELECT op.* FROM order_payments op
             JOIN orders o ON o.id = op.order_id JOIN tenants t ON t.id = o.tenant_id
             WHERE op.voided_at_utc IS NOT NULL AND op.voided_at_utc >= ?2 AND op.id > ?3 AND t.network = ?1
             ORDER BY op.id LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![network, cutoff, after, limit as i64], Self::row_to_payment)?
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

        s.defer_reorg_candidate("mainnet", first, 1000).unwrap();
        let due: Vec<i64> = s.due_reorg_candidates("mainnet", 1000, 10).unwrap().iter().map(|c| c.payment.id).collect();
        assert_eq!(due, vec![second], "the failed one waits");
        assert!(matches!(s.finish_reorg("mainnet", 10, Some((9, "old9"))), Err(StoreError::NotFound)));
        s.complete_reorg_candidate("mainnet", second).unwrap();
        let later = s.due_reorg_candidates("mainnet", 1000 + reorg_retry_delay(1), 10).unwrap();
        assert_eq!(later.len(), 1);
        assert_eq!((later[0].payment.id, later[0].attempts), (first, 1));
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

    #[test]
    fn scheduler_positions_are_per_network_and_survive_a_restart() {
        let (store, path) = fixture();
        store.0.set_scheduler_position("mainnet", Position::CatchUpGroup, "42").unwrap();
        store.0.set_scheduler_position("mainnet", Position::CatchUpGroup, "43").unwrap();
        let s = reopen(store, &path);
        assert_eq!(s.scheduler_position("mainnet", Position::CatchUpGroup).unwrap().as_deref(), Some("43"));
        assert_eq!(s.scheduler_position("stagenet", Position::CatchUpGroup).unwrap(), None);
        assert_eq!(s.scheduler_position("mainnet", Position::VoidRecheck).unwrap(), None);
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
}
