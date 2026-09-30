//! The chain tier: reorg detection, and the durable reorg job that
//! reconciles the payments a reorg touched (docs/scanner_microtasks.md,
//! "Reorgs").

use std::collections::HashSet;

use crate::daemon::{KeyImageStatus, MoneroDaemonClient, TxLocation};
use crate::scanner::{void_and_notify_in_tx, ScannerError};
use crate::store::db::Class;
use crate::store::{Db, OpenedReorg, OrderPaymentRow, ReorgPhase, Store};

use super::{bounded, Progress, Round};

/// Candidates collected into the job per unit.
const COLLECT_PAGE: usize = 256;
/// Candidates re-examined per unit: each costs a daemon lookup or two.
const PROCESS_PAGE: usize = 16;
/// After this many failed lookups a candidate is left as it is (the rule
/// for ambiguous evidence: never void on absence alone) so one payment the
/// node can't answer about can't hold the network's block scanning forever.
const MAX_CANDIDATE_ATTEMPTS: u32 = 12;

#[derive(Default)]
pub(crate) struct ChainRound {
    detected: bool,
    /// A rewind happened this round: the replacement blocks are scanned
    /// from the next round, against a freshly read chain.
    pub rewound: bool,
    /// Candidates tried this round: one that failed isn't retried until the
    /// next round, so a failing node can't spin the tier.
    attempted: HashSet<i64>,
}

/// What reconciling some candidates changed, for callers that report it.
#[derive(Default)]
pub(crate) struct Reconciled {
    pub dirty_orders: HashSet<String>,
    pub double_spent_orders: HashSet<String>,
    pub failure: Option<ScannerError>,
}

/// The chain work for one network, usable from a round or on its own.
pub(crate) struct Chain<'a> {
    pub db: &'a Db,
    pub daemon: &'a dyn MoneroDaemonClient,
    pub network: &'a str,
    pub reorg_check_depth: u64,
    pub now: i64,
}

impl Chain<'_> {
    /// Runs `f` on the database worker, with this network's name.
    async fn db<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Store, &str) -> Result<T, ScannerError> + Send + 'static,
    ) -> Result<T, ScannerError> {
        let network = self.network.to_string();
        self.db.run(Class::Scanner, move |s| f(s, &network)).await
    }

    /// The lowest height where the stored chain and the node's differ, if
    /// they do within the reorg window.
    ///
    /// Block hashes chain: if the stored hash at the highest height both
    /// sides have matches the node's, every block below matches too. So one
    /// comparison settles the common case; a mismatch is narrowed down by
    /// binary search over the stored window, O(log depth) lookups.
    pub async fn detect(&self, tip: u64) -> Result<Option<u64>, ScannerError> {
        let depth = self.reorg_check_depth;
        let rows = self
            .db(move |s, network| {
                let Some(high_water) = s.max_scanned_height(network)? else { return Ok(Vec::new()) };
                let check = high_water.min(tip);
                Ok(s.scanned_blocks_between(network, check.saturating_sub(depth), check)?)
            })
            .await?;
        let (Some(first), Some(last)) = (rows.first(), rows.last()) else { return Ok(None) };
        if self.node_agrees(last).await? {
            return Ok(None);
        }
        if !self.node_agrees(first).await? {
            // Diverged at or below the window's edge: reconcile from there,
            // and leave older payments alone.
            return Ok(Some(first.0));
        }
        // rows[lo] agrees, rows[hi] doesn't.
        let (mut lo, mut hi) = (0, rows.len() - 1);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if self.node_agrees(&rows[mid]).await? {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Ok(Some(rows[lo].0 + 1))
    }

    async fn node_agrees(&self, (height, stored): &(u64, String)) -> Result<bool, ScannerError> {
        Ok(bounded(self.daemon.get_block_hash(*height)).await? == *stored)
    }

    /// Records a detected fork (or deepens the open job). Logged, because a
    /// reorg is rare and worth an operator knowing about.
    pub async fn open(&self, fork: u64) -> Result<(), ScannerError> {
        let now = self.now;
        match self.db(move |s, network| Ok(s.open_reorg_job(network, fork, now)?)).await? {
            OpenedReorg::Created => {
                tracing::warn!(network = %self.network, fork, "chain reorganisation detected - reconciling payments from this height")
            }
            OpenedReorg::Deepened { from } => {
                tracing::warn!(network = %self.network, fork, previous_fork = from, "the reorganisation being reconciled goes deeper")
            }
            OpenedReorg::Covered => {}
        }
        Ok(())
    }

    /// Re-examines up to `PROCESS_PAGE` due candidates not in `skip`.
    /// Each outcome and the removal of its candidate commit together.
    pub async fn process_page(&self, tip: u64, skip: &mut HashSet<i64>) -> Result<(usize, Reconciled), ScannerError> {
        let (now, limit) = (self.now, PROCESS_PAGE + skip.len());
        let due = self.db(move |s, network| Ok(s.due_reorg_candidates(network, now, limit)?)).await?;
        let mut done = Reconciled::default();
        let mut processed = 0;
        let page: Vec<_> = due.into_iter().filter(|c| !skip.contains(&c.payment.id)).take(PROCESS_PAGE).collect();
        for candidate in page {
            skip.insert(candidate.payment.id);
            processed += 1;
            match self.reexamine(&candidate.payment, tip, &mut done).await {
                Ok(()) => {}
                Err(error) if candidate.attempts + 1 >= MAX_CANDIDATE_ATTEMPTS => {
                    tracing::error!(
                        network = %self.network, payment.id = candidate.payment.id, order.id = %candidate.payment.order_id,
                        error = %error,
                        "reorg: giving up re-examining a payment the node keeps failing to answer about - leaving it as recorded"
                    );
                    let id = candidate.payment.id;
                    self.db(move |s, network| Ok(s.complete_reorg_candidate(network, id)?)).await?;
                }
                Err(error) => {
                    tracing::warn!(network = %self.network, payment.id = candidate.payment.id, error = %error, "reorg: re-examining a payment failed (retried)");
                    let (id, now) = (candidate.payment.id, self.now);
                    self.db(move |s, network| Ok(s.defer_reorg_candidate(network, id, now)?)).await?;
                    done.failure.get_or_insert(error);
                }
            }
        }
        Ok((processed, done))
    }

    /// The existing reconciliation rules, for one payment:
    /// - not voided: follow its transaction (new height, back to the pool),
    ///   or void it if and only if a different transaction provably spent
    ///   its key images;
    /// - voided: restore it if its transaction is back on the chain.
    async fn reexamine(&self, candidate: &OrderPaymentRow, tip: u64, done: &mut Reconciled) -> Result<(), ScannerError> {
        // The row as it is now: something else may have changed it since
        // it was collected.
        let id = candidate.id;
        let current = self
            .db(move |s, network| {
                let payment = s.payment_by_id(id)?;
                if payment.is_none() {
                    s.complete_reorg_candidate(network, id)?;
                }
                Ok(payment)
            })
            .await?;
        let Some(payment) = current else { return Ok(()) };
        let location = bounded(self.daemon.locate_transaction(&payment.txid)).await?;
        let (order_id, txid, output) = (payment.order_id.clone(), payment.txid.clone(), payment.output_index);
        if payment.voided_at.is_some() {
            let (o, t) = (order_id.clone(), txid.clone());
            let restored = self
                .db(move |s, network| {
                    s.in_transaction(|s| -> Result<bool, ScannerError> {
                        let mut restored = false;
                        if let TxLocation::InBlock(height) = location {
                            if s.unvoid_payment(&o, &t, output)? {
                                s.update_payment_block_height(&o, &t, output, Some(height as i64))?;
                                restored = true;
                            }
                        }
                        s.complete_reorg_candidate(network, id)?;
                        Ok(restored)
                    })
                })
                .await?;
            if restored {
                done.dirty_orders.insert(order_id);
            }
            return Ok(());
        }
        let new_height = match location {
            TxLocation::InBlock(height) => Some(Some(height as i64)),
            TxLocation::InPool => Some(None),
            TxLocation::NotFound => None,
        };
        if let Some(height) = new_height {
            let (o, t) = (order_id.clone(), txid.clone());
            self.db(move |s, network| {
                s.in_transaction(|s| -> Result<(), ScannerError> {
                    s.update_payment_block_height(&o, &t, output, height)?;
                    Ok(s.complete_reorg_candidate(network, id)?)
                })
            })
            .await?;
            if height != payment.block_height {
                done.dirty_orders.insert(order_id);
            }
            return Ok(());
        }
        // Nowhere to be found. Dropped, evicted and double-spent look the
        // same from here, except by the key images.
        let proven = match crate::scanner::parse_payment_key_images(&payment.key_images_json) {
            Ok(images) => bounded(self.daemon.is_key_image_spent_corroborated(&images))
                .await?
                .contains(&KeyImageStatus::SpentInBlockchain),
            Err(error) => {
                tracing::warn!(payment.id = payment.id, error = %error, "reorg: a vanished payment's stored key images are invalid - never voiding it on that");
                false
            }
        };
        let (o, t, now) = (order_id.clone(), txid.clone(), self.now);
        self.db(move |s, network| {
            s.in_transaction(|s| -> Result<(), ScannerError> {
                if proven {
                    void_and_notify_in_tx(s, &o, &t, output, tip, now)?;
                }
                Ok(s.complete_reorg_candidate(network, id)?)
            })
        })
        .await?;
        if proven {
            done.dirty_orders.insert(order_id.clone());
            done.double_spent_orders.insert(order_id);
        }
        Ok(())
    }

    /// The job's last step, once every candidate is done: rewind to the
    /// common ancestor. The ancestor's hash is read first; without it the
    /// losing hashes and the job stay, and this is retried.
    pub async fn rewind(&self, fork: u64) -> Result<(), ScannerError> {
        let ancestor = match fork.checked_sub(1) {
            Some(height) => Some((height, bounded(self.daemon.get_block_hash(height)).await?)),
            // The genesis block changed: impossible on a real chain, and
            // there is nothing to anchor to.
            None => None,
        };
        self.db(move |s, network| Ok(s.finish_reorg(network, fork, ancestor.as_ref().map(|(h, hash)| (*h, hash.as_str())))?))
            .await?;
        tracing::info!(network = %self.network, fork, "reorganisation reconciled - replacement blocks will be scanned");
        Ok(())
    }

    /// One unit of the open job, if any: collect a page, re-examine a page,
    /// or rewind. `Ok(None)` when there is no job.
    pub async fn advance_job(&self, tip: u64, skip: &mut HashSet<i64>) -> Result<Option<JobStep>, ScannerError> {
        let Some(job) = self.db(|s, network| Ok(s.reorg_job(network)?)).await? else { return Ok(None) };
        match job.phase {
            ReorgPhase::CollectConfirmed { .. } | ReorgPhase::CollectUnconfirmed { .. } => {
                let now = self.now;
                self.db(move |s, network| Ok(s.collect_reorg_candidates(network, COLLECT_PAGE, now)?)).await?;
                Ok(Some(JobStep::Collected))
            }
            ReorgPhase::Process => {
                let (processed, reconciled) = self.process_page(tip, skip).await?;
                if processed > 0 {
                    return Ok(Some(JobStep::Processed(reconciled)));
                }
                let (remaining, _) = self.db(|s, network| Ok(s.reorg_work_remaining(network)?)).await?;
                if remaining > 0 {
                    return Ok(Some(JobStep::Waiting));
                }
                self.rewind(job.fork_height).await?;
                Ok(Some(JobStep::Rewound))
            }
        }
    }
}

pub(crate) enum JobStep {
    Collected,
    Processed(Reconciled),
    /// Candidates remain, but each is waiting out a retry.
    Waiting,
    Rewound,
}

pub(super) async fn step(round: &mut Round<'_>) -> Progress {
    match run(round).await {
        // A node that fails partway leaves the job (and the losing chain's
        // hashes) exactly where they were; the next round carries on.
        Progress::Failed(ScannerError::Daemon(error)) => {
            shared::throttled!(
                format!("chain-node:{}", round.network()),
                warn,
                network = %round.network(),
                error = %error,
                "reorg work stopped: the node failed (retried next round)"
            );
            Progress::Blocked("the node failed")
        }
        progress => progress,
    }
}

async fn run(round: &mut Round<'_>) -> Progress {
    let Some(tip) = round.tip else { return Progress::Blocked("chain height unknown") };
    let chain = Chain {
        db: round.inputs.db,
        daemon: round.inputs.daemon,
        network: round.inputs.network,
        reorg_check_depth: round.inputs.reorg_check_depth,
        now: round.now,
    };
    // Detection and a step of the job share one unit: even a round with no
    // time to spare moves an open job forward.
    if !round.chain.detected {
        round.chain.detected = true;
        match chain.detect(tip).await {
            Ok(Some(fork)) => {
                if let Err(error) = chain.open(fork).await {
                    return Progress::Failed(error);
                }
            }
            Ok(None) => {}
            Err(error) => return Progress::Failed(error),
        }
    }
    match chain.advance_job(tip, &mut round.chain.attempted).await {
        Ok(None) => Progress::Idle,
        Ok(Some(JobStep::Processed(Reconciled { failure: Some(error), .. }))) => Progress::Failed(error),
        Ok(Some(JobStep::Waiting)) => Progress::Blocked("reorg candidates are waiting to be retried"),
        Ok(Some(JobStep::Rewound)) => {
            round.chain.rewound = true;
            Progress::Advanced
        }
        Ok(Some(_)) => Progress::Advanced,
        Err(error) => Progress::Failed(error),
    }
}
