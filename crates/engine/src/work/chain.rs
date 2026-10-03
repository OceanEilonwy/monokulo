//! The chain tier: reorg detection, and the durable reorg job that
//! reconciles the payments a reorg touched (docs/scanner_microtasks.md,
//! "Reorgs").

use std::collections::HashSet;

use tokio::time::Instant;

use crate::daemon::{KeyImageStatus, MoneroDaemonClient, TxLocation};
use crate::scanner::{void_and_notify_in_tx, ScannerError};
use crate::store::db::Class;
use crate::store::{Db, OpenedReorg, OrderPaymentRow, ReorgPhase, Store};

use super::{bounded, Progress, Round, Wait};

/// The id of block `height`, if its own list of transactions holds `txid`
/// (`get_block`: the id is computed from the block, the list is what the id
/// commits to). A payment moved to a height on a node's word is stamped
/// with it, and settles under proof-of-work checking only if that block is
/// the proven one (docs/proof_of_work.md). `None` if the node can't show
/// it: the payment then waits for a scan of that block.
pub(crate) async fn block_holding(
    daemon: &dyn MoneroDaemonClient,
    txid: &str,
    height: u64,
) -> Option<String> {
    match bounded(daemon.get_block_outline(height, None)).await {
        Ok(outline) if outline.height == height && outline.txids.iter().any(|t| t == txid) => {
            Some(outline.hash)
        }
        _ => None,
    }
}

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
    pub(crate) dirty_orders: HashSet<crate::store::OrderId>,
    pub(crate) double_spent_orders: HashSet<crate::store::OrderId>,
}

/// The chain work for one network, usable from a round or on its own.
pub(crate) struct Chain<'a> {
    db: &'a Db,
    daemon: &'a dyn MoneroDaemonClient,
    network: monero::Network,
    reorg_check_depth: u64,
    now: i64,
    /// The id of the node's tip block, if it came with the tip's height.
    tip_hash: Option<String>,
}

impl<'a> Chain<'a> {
    pub fn new(
        db: &'a Db,
        daemon: &'a dyn MoneroDaemonClient,
        network: monero::Network,
        reorg_check_depth: u64,
        now: i64,
    ) -> Self {
        Self {
            db,
            daemon,
            network,
            reorg_check_depth,
            now,
            tip_hash: None,
        }
    }

    /// With the id the node gave for its tip block along with the height
    /// passed to [`Self::detect`]: a comparison at that height then needs
    /// no lookup.
    pub fn with_tip_hash(mut self, tip_hash: Option<String>) -> Self {
        self.tip_hash = tip_hash;
        self
    }

    /// Runs `f` on the database worker, with this network. `f` may fail
    /// with a store error or a scanner error.
    async fn db<T, E>(
        &self,
        f: impl FnOnce(&Store, monero::Network) -> Result<T, E> + Send + 'static,
    ) -> Result<T, ScannerError>
    where
        T: Send + 'static,
        E: Into<ScannerError> + Send + 'static,
    {
        let network = self.network;
        self.db
            .run(Class::Scanner, move |s| f(s, network).map_err(Into::into))
            .await
    }

    /// The lowest height where the stored chain and the node's differ, if
    /// they do within the reorg window.
    ///
    /// Block hashes chain: if the stored hash at the highest height both
    /// sides have matches the node's, every block below matches too. So one
    /// comparison settles the common case; a mismatch is narrowed down by
    /// binary search over the stored window, O(log depth) lookups. When the
    /// recorded chain ends at the node's tip and the node gave the tip's id
    /// with its height, that one comparison costs no lookup at all.
    pub async fn detect(&self, tip: u64) -> Result<Option<u64>, ScannerError> {
        let depth = self.reorg_check_depth;
        let rows = self
            .db(move |s, network| -> Result<_, crate::store::StoreError> {
                let Some(high_water) = s.max_scanned_height(network)? else {
                    return Ok(Vec::new());
                };
                let check = high_water.min(tip);
                s.scanned_blocks_between(network, check.saturating_sub(depth), check)
            })
            .await?;
        let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
            return Ok(None);
        };
        let at_tip = match &self.tip_hash {
            Some(tip_hash) if last.0 == tip => Some(*tip_hash == last.1),
            _ => None,
        };
        let agrees = match at_tip {
            Some(agrees) => agrees,
            None => self.node_agrees(last).await?,
        };
        if agrees {
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
        match self
            .db(move |s, network| s.open_reorg_job(network, fork, now))
            .await?
        {
            OpenedReorg::Created => {
                tracing::warn!(
                    network = crate::network::network_str(self.network),
                    fork,
                    "chain reorganisation detected - reconciling payments from this height"
                )
            }
            OpenedReorg::Deepened { from } => {
                tracing::warn!(
                    network = crate::network::network_str(self.network),
                    fork,
                    previous_fork = from,
                    "the reorganisation being reconciled goes deeper"
                )
            }
            OpenedReorg::Covered => {}
        }
        Ok(())
    }

    /// Re-examines up to `PROCESS_PAGE` due candidates not in `skip`, until
    /// `until` (at least one). Each outcome and the removal of its candidate
    /// commit together. Stops at the first node failure: a node that fails
    /// (or hangs until the call deadline) for one payment would for the next,
    /// and waiting on it payment after payment would stall the round.
    /// Returns how many were re-examined, what changed, and the first
    /// failure (a failed candidate is deferred; the others still count).
    pub async fn process_page(
        &self,
        tip: u64,
        skip: &mut HashSet<i64>,
        until: Instant,
    ) -> Result<(usize, Reconciled, Option<ScannerError>), ScannerError> {
        let (now, limit) = (self.now, PROCESS_PAGE + skip.len());
        let due = self
            .db(move |s, network| s.due_reorg_candidates(network, now, limit))
            .await?;
        let mut done = Reconciled::default();
        let mut failure = None;
        let mut processed = 0;
        let page: Vec<_> = due
            .into_iter()
            .filter(|c| !skip.contains(&c.payment.id))
            .take(PROCESS_PAGE)
            .collect();
        // Where the page's transactions are, in one round trip where the
        // node client can; a transaction left out of the answer (or all of
        // them, if the call fails) is asked about on its own.
        let located = {
            let mut txids: Vec<String> = page.iter().map(|c| c.payment.txid.clone()).collect();
            txids.sort_unstable();
            txids.dedup();
            bounded(self.daemon.locate_transactions(&txids))
                .await
                .unwrap_or_default()
        };
        for candidate in page {
            if processed > 0 && Instant::now() >= until {
                break;
            }
            skip.insert(candidate.payment.id);
            processed += 1;
            match self
                .reexamine(&candidate.payment, tip, &mut done, &located)
                .await
            {
                Ok(()) => {}
                Err(error) if candidate.attempts + 1 >= MAX_CANDIDATE_ATTEMPTS => {
                    // A confirmed, unvoided candidate is moved out of its
                    // block: its height is from the discarded chain, and
                    // left there it would keep counting confirmations and
                    // could settle the order. As an unconfirmed payment the
                    // vanished-payment check follows it and records where
                    // it is once a node answers. A voided one is left as it
                    // is; nothing counts its height.
                    let unconfirm = candidate.payment.voided_at.is_none()
                        && candidate.payment.block_height.is_some();
                    tracing::error!(
                        network = crate::network::network_str(self.network), payment.id = candidate.payment.id, order.id = %candidate.payment.order_id,
                        error = %error, unconfirmed = unconfirm,
                        "reorg: giving up re-examining a payment the node keeps failing to answer about"
                    );
                    let (id, order_id, txid, output) = (
                        candidate.payment.id,
                        candidate.payment.order_id.clone(),
                        candidate.payment.txid.clone(),
                        candidate.payment.output_index,
                    );
                    self.db(move |s, network| {
                        s.in_transaction(|s| -> Result<(), ScannerError> {
                            if unconfirm {
                                s.update_payment_block_height(&order_id, &txid, output, None)?;
                            }
                            s.complete_reorg_candidate(network, id)?;
                            Ok(())
                        })
                    })
                    .await?;
                    if unconfirm {
                        done.dirty_orders.insert(candidate.payment.order_id.clone());
                    }
                }
                Err(error) => {
                    tracing::warn!(network = crate::network::network_str(self.network), payment.id = candidate.payment.id, error = %error, "reorg: re-examining a payment failed (retried)");
                    let (id, now) = (candidate.payment.id, self.now);
                    self.db(move |s, network| s.defer_reorg_candidate(network, id, now))
                        .await?;
                    let node_failed = matches!(error, ScannerError::Daemon(_));
                    failure.get_or_insert(error);
                    if node_failed {
                        break;
                    }
                }
            }
        }
        Ok((processed, done, failure))
    }

    /// Re-examines one payment against the chain as the node has it now
    /// ([`decide`]), and applies the outcome and completes the candidate in
    /// one transaction.
    async fn reexamine(
        &self,
        candidate: &OrderPaymentRow,
        tip: u64,
        done: &mut Reconciled,
        located: &std::collections::HashMap<String, TxLocation>,
    ) -> Result<(), ScannerError> {
        // The row as it is now: something else may have changed it since
        // it was collected.
        let id = candidate.id;
        let current = self
            .db(move |s, network| -> Result<_, ScannerError> {
                let payment = s.payment_by_id(id)?;
                if payment.is_none() {
                    s.complete_reorg_candidate(network, id)?;
                }
                Ok(payment)
            })
            .await?;
        let Some(payment) = current else {
            return Ok(());
        };
        let voided = payment.voided_at.is_some();
        let mut location = match located.get(&payment.txid) {
            Some(location) => *location,
            None => bounded(self.daemon.locate_transaction(&payment.txid)).await?,
        };
        // Only a transaction that is nowhere to be found needs the key-image
        // evidence: dropped, evicted and double-spent look the same otherwise.
        // And only once every node agrees it is nowhere: one node's absence
        // is what a real payment would be voided on, and its own inputs are
        // spent on every node regardless (see
        // `MoneroDaemonClient::locate_transaction_corroborated`).
        if !voided && location == TxLocation::NotFound {
            if let Some(agreed) =
                bounded(self.daemon.locate_transaction_corroborated(&payment.txid)).await?
            {
                location = agreed;
            }
        }
        // A payment voided for another with its output key
        // (`store::conflicts`) only has its height followed: which of them
        // is credited is settled at recompute, never restored or voided as
        // a double spend here.
        let decision = if payment.superseded_by.is_some() {
            Decision::Move(match location {
                TxLocation::InBlock(height) => Some(height),
                TxLocation::InPool | TxLocation::NotFound => None,
            })
        } else {
            let proven = !voided
                && location == TxLocation::NotFound
                && self.double_spend_proven(&payment).await?;
            decide(voided, location, proven)
        };
        let moved_to = match decision {
            Decision::Move(Some(height)) | Decision::Restore(height) => Some(height),
            _ => None,
        };
        let found_in = match moved_to {
            Some(height) => block_holding(self.daemon, &payment.txid, height).await,
            None => None,
        };

        let (order_id, txid, output, now) = (
            payment.order_id.clone(),
            payment.txid.clone(),
            payment.output_index,
            self.now,
        );
        let changed = self
            .db(move |s, network| {
                s.in_transaction(|s| -> Result<bool, ScannerError> {
                    let changed = match decision {
                        Decision::Keep => false,
                        Decision::Move(height) => {
                            let height = height.map(crate::store::sql_height).transpose()?;
                            s.update_payment_block_height(&order_id, &txid, output, height)?;
                            height != payment.block_height
                        }
                        Decision::Restore(height) => {
                            // Already restored since it was read (the void
                            // recheck got there first) or not, the node
                            // has it in this block: its height is recorded
                            // either way.
                            let restored = s.unvoid_payment(&order_id, &txid, output)?;
                            let height = Some(crate::store::sql_height(height)?);
                            s.update_payment_block_height(&order_id, &txid, output, height)?;
                            restored || height != payment.block_height
                        }
                        Decision::Void => {
                            void_and_notify_in_tx(s, &order_id, &txid, output, tip, now)?;
                            true
                        }
                    };
                    if let (Some(height), Some(hash)) = (moved_to, &found_in) {
                        s.attest_payment_block(&txid, height, hash)?;
                    }
                    s.complete_reorg_candidate(network, id)?;
                    Ok(changed)
                })
            })
            .await?;
        if changed {
            done.dirty_orders.insert(candidate.order_id.clone());
        }
        if decision == Decision::Void {
            done.double_spent_orders.insert(candidate.order_id.clone());
        }
        Ok(())
    }

    /// Whether a different transaction provably spent `payment`'s inputs,
    /// asked of every node that can answer (corroborated). Invalid stored
    /// evidence is never proof.
    async fn double_spend_proven(&self, payment: &OrderPaymentRow) -> Result<bool, ScannerError> {
        match crate::scanner::parse_payment_key_images(&payment.key_images_json) {
            Ok(images) => Ok(
                bounded(self.daemon.is_key_image_spent_corroborated(&images))
                    .await?
                    .contains(&KeyImageStatus::SpentInBlockchain),
            ),
            Err(error) => {
                tracing::warn!(payment.id = payment.id, error = %error, "reorg: a vanished payment's stored key images are invalid - never voiding it on that");
                Ok(false)
            }
        }
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
        self.db(move |s, network| {
            s.finish_reorg(
                network,
                fork,
                ancestor.as_ref().map(|(h, hash)| (*h, hash.as_str())),
            )
        })
        .await?;
        tracing::info!(
            network = crate::network::network_str(self.network),
            fork,
            "reorganisation reconciled - replacement blocks will be scanned"
        );
        Ok(())
    }

    /// One unit of the open job, if any: collect a page, re-examine a page,
    /// or rewind. `Ok(None)` when there is no job.
    pub async fn advance_job(
        &self,
        tip: u64,
        skip: &mut HashSet<i64>,
        until: Instant,
    ) -> Result<Option<JobStep>, ScannerError> {
        let Some(job) = self.db(|s, network| s.reorg_job(network)).await? else {
            return Ok(None);
        };
        match job.phase {
            ReorgPhase::CollectConfirmed { .. } | ReorgPhase::CollectUnconfirmed { .. } => {
                let now = self.now;
                self.db(move |s, network| s.collect_reorg_candidates(network, COLLECT_PAGE, now))
                    .await?;
                Ok(Some(JobStep::Collected))
            }
            ReorgPhase::Process => {
                let (processed, reconciled, failure) = self.process_page(tip, skip, until).await?;
                if processed > 0 {
                    return Ok(Some(JobStep::Processed {
                        reconciled,
                        failure,
                    }));
                }
                let (remaining, _) = self
                    .db(|s, network| s.reorg_work_remaining(network))
                    .await?;
                if remaining > 0 {
                    return Ok(Some(JobStep::Waiting));
                }
                self.rewind(job.fork_height).await?;
                Ok(Some(JobStep::Rewound))
            }
        }
    }
}

/// What reconciliation does to one payment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Leave it as recorded.
    Keep,
    /// Record where its transaction is now: in a block, or (`None`) not in
    /// any block.
    Move(Option<u64>),
    /// It was voided, and its transaction is back on the chain here.
    Restore(u64),
    /// A different transaction provably spent its inputs.
    Void,
}

/// The reconciliation rules for one payment, given whether it was voided,
/// where the node now places its transaction, and whether a double spend is
/// proven. No I/O, so the whole table is tested directly.
///
/// A payment whose transaction can't be found and isn't provably
/// double-spent is moved *out of any block*: it isn't on this chain, so no
/// confirmations may count towards it. As an unconfirmed payment it is
/// followed by the vanished-payment check, which records its height when it
/// is mined again and voids it on proof. (Leaving its old height would keep
/// counting confirmations on a discarded block and could settle the order.)
pub(crate) fn decide(voided: bool, location: TxLocation, double_spend_proven: bool) -> Decision {
    match (voided, location) {
        (true, TxLocation::InBlock(height)) => Decision::Restore(height),
        (true, TxLocation::InPool | TxLocation::NotFound) => Decision::Keep,
        (false, TxLocation::InBlock(height)) => Decision::Move(Some(height)),
        (false, TxLocation::InPool) => Decision::Move(None),
        (false, TxLocation::NotFound) if double_spend_proven => Decision::Void,
        (false, TxLocation::NotFound) => Decision::Move(None),
    }
}

/// What one unit of the reorg job did. Each variant is a different next
/// move for the caller: after `Collected` there is nothing to apply (the
/// candidates are only queued); `Processed` carries changes to act on (and
/// wake webhooks for); `Waiting` means stop for now; `Rewound` means the
/// job is over and blocks must wait for a fresh chain read.
pub(crate) enum JobStep {
    /// A page of candidates was queued.
    Collected,
    /// A page of candidates was re-examined; a failed one is deferred.
    Processed {
        reconciled: Reconciled,
        failure: Option<ScannerError>,
    },
    /// Candidates remain, but each is waiting out a retry.
    Waiting,
    /// Every candidate was handled and the chain was rewound.
    Rewound,
}

pub(super) async fn step(round: &mut Round<'_>, until: Instant) -> Progress {
    match run(round, until).await {
        // A node that fails partway leaves the job (and the losing chain's
        // hashes) exactly where they were; the next round carries on.
        Progress::Failed(ScannerError::Daemon(error)) => {
            shared::throttled!(
                format!("chain-node:{}", crate::network::network_str(round.network())),
                warn,
                network = crate::network::network_str(round.network()),
                error = %error,
                "reorg work stopped: the node failed (retried next round)"
            );
            Progress::Blocked(Wait::NodeFailed)
        }
        progress => progress,
    }
}

async fn run(round: &mut Round<'_>, until: Instant) -> Progress {
    let Some(tip) = round.tip else {
        return Progress::Blocked(Wait::ChainHeightUnknown);
    };
    let chain = Chain::new(
        round.inputs.db,
        round.inputs.daemon,
        round.inputs.network,
        round.inputs.reorg_check_depth,
        round.now,
    )
    .with_tip_hash(round.tip_hash.clone());
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
    match chain
        .advance_job(tip, &mut round.chain.attempted, until)
        .await
    {
        Ok(None) => Progress::Idle,
        Ok(Some(JobStep::Processed {
            reconciled,
            failure,
        })) => {
            // A void enqueues its webhook in the same transaction.
            if !reconciled.double_spent_orders.is_empty() {
                round.state.wake_webhooks();
            }
            match failure {
                Some(error) => Progress::Failed(error),
                None => Progress::Advanced,
            }
        }
        Ok(Some(JobStep::Waiting)) => Progress::Blocked(Wait::ReorgCandidatesRetrying),
        Ok(Some(JobStep::Rewound)) => {
            round.chain.rewound = true;
            Progress::Advanced
        }
        Ok(Some(_)) => Progress::Advanced,
        Err(error) => Progress::Failed(error),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod decide_tests {
    use super::*;

    #[test]
    fn reconciliation_follows_the_transaction_and_voids_only_on_proof() {
        use Decision::*;
        use TxLocation::*;
        for (voided, location, proven, expected) in [
            (false, InBlock(7), false, Move(Some(7))),
            (false, InPool, false, Move(None)),
            (false, NotFound, true, Void),
            (false, NotFound, false, Move(None)),
            (true, InBlock(7), false, Restore(7)),
            (true, InPool, false, Keep),
            (true, NotFound, false, Keep),
            (true, NotFound, true, Keep),
        ] {
            assert_eq!(
                decide(voided, location, proven),
                expected,
                "voided={voided} {location:?} proven={proven}"
            );
        }
    }
}
