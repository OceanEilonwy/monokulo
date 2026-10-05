//! The scanner as small, durable work units chosen by one scheduler per
//! network (`docs/scanner_microtasks.md`).
//!
//! A *round* runs the five tiers in priority order. Each tier has a reserved
//! share of the round's time and completes at least one unit per round when
//! it has work, so a slow node, a throttled CPU or a flood of one kind of work
//! slows every tier but starves none. Units keep their progress in SQLite, so
//! a round cut short, a crash or a restart repeats at most the unit in flight.

mod blocks;
pub(crate) mod chain;
mod mempool;
mod observe;
pub mod retry;
pub mod scheduler;
mod settlement;
mod tuning;
mod upkeep;

use std::collections::HashMap;
use std::time::Duration;

use tokio::time::Instant;

use crate::daemon::MoneroDaemonClient;
use crate::key_custody::{KeyCustody, WalletHandle};
use crate::scanner::ScannerError;
use crate::store::db::Class;
use crate::store::{Db, Store};

pub use blocks::ScannedBlock;
pub use mempool::{fast_pass, FastReport};
pub use observe::snapshot;
use shared::activity::{Event, UnitProgress, Work};
pub use shared::activity::{Tier, TierOutcome, Wait};
pub use tuning::{ScanTuning, TierShares, TuningError};

/// One value per tier, indexed by [`Tier`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PerTier<T>([T; 5]);

impl<T: Copy> PerTier<T> {
    fn filled(value: T) -> Self {
        Self([value; 5])
    }

    pub fn iter(&self) -> impl Iterator<Item = (Tier, T)> + '_ {
        Tier::ALL
            .into_iter()
            .map(|tier| (tier, self.0[tier.index()]))
    }
}

impl<T> std::ops::Index<Tier> for PerTier<T> {
    type Output = T;
    fn index(&self, index: Tier) -> &T {
        &self.0[index.index()]
    }
}

impl<T> std::ops::IndexMut<Tier> for PerTier<T> {
    fn index_mut(&mut self, index: Tier) -> &mut T {
        &mut self.0[index.index()]
    }
}

/// What one unit of work did. The executor can't mistake "nothing to do"
/// for "failed".
#[derive(Debug)]
pub(crate) enum Progress {
    /// Did something; more may be due.
    Advanced,
    /// Nothing is due for this tier this round.
    Idle,
    /// Work exists but must wait.
    Blocked(Wait),
    /// A unit failed; the tier stops for this round and the error is
    /// reported. Its durable state is unchanged, so the next round retries.
    Failed(ScannerError),
}

/// What a round did, for the loop and for status reporting.
#[derive(Debug)]
pub struct RoundReport {
    /// Units each tier ran.
    pub steps: PerTier<u32>,
    pub outcomes: PerTier<TierOutcome>,
    /// The first unit failure, if any. Other tiers still ran.
    pub error: Option<ScannerError>,
}

impl RoundReport {
    /// Whether any tier stopped with work left: the loop starts the next
    /// round at once instead of waiting for the poll interval.
    pub fn backlogged(&self) -> bool {
        self.outcomes
            .iter()
            .any(|(_, outcome)| outcome == TierOutcome::Backlogged)
    }

    pub fn outcome(&self, tier: Tier) -> TierOutcome {
        self.outcomes[tier]
    }

    pub fn into_result(self) -> Result<(), ScannerError> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// [`Self::into_result`], and a tier the node blocked counts as a
    /// failure too: for the status page, a round in which no block (or the
    /// pool) could be read is not a round that went well, however cleanly
    /// the tier stopped and waited.
    pub fn into_status_result(self) -> Result<(), ScannerError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        for (tier, outcome) in self.outcomes.iter() {
            if let TierOutcome::Blocked(wait @ (Wait::NodeFailed | Wait::MempoolUnreadable)) =
                outcome
            {
                return Err(ScannerError::Daemon(crate::daemon::DaemonError::Request(
                    format!("the {tier:?} tier was blocked: {wait:?}"),
                )));
            }
        }
        Ok(())
    }
}

/// Everything a round needs from outside. Borrowed for the round only.
pub struct RoundInputs<'a> {
    /// All database work goes through the worker: SQLite never runs on a
    /// Tokio worker thread.
    pub db: &'a Db,
    pub custody: &'a dyn KeyCustody,
    pub daemon: &'a dyn MoneroDaemonClient,
    pub network: monero::Network,
    /// Tenants with registered keys. A tenant missing here isn't scanned,
    /// and its cursor stays where it is until it is registered.
    pub tenants: &'a [(crate::store::TenantId, WalletHandle)],
    pub reorg_check_depth: u64,
    pub grace_period_seconds: i64,
    pub scan_chunk_memory_budget_mb: u32,
}

/// How long one daemon call inside a unit may take before the unit treats
/// it as failed: the client's own request timeout for small calls, and a
/// margin. The client's timer starts a moment after this one, so without
/// the margin this deadline could fire first and drop the call before the
/// node's own error (naming the node) came back and its failure was
/// recorded: the hung node would be pinned again next round.
pub(crate) const CALL_DEADLINE: Duration =
    crate::daemon_rpc::REQUEST_TIMEOUT.saturating_add(crate::daemon_fallback::DEADLINE_MARGIN);

/// A daemon (or other) call with [`CALL_DEADLINE`].
pub(crate) async fn bounded<T, E>(
    call: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, ScannerError>
where
    ScannerError: From<E>,
{
    bounded_by(CALL_DEADLINE, call).await
}

/// A call with `deadline`: for a block request, what the node's link needs
/// (`docs/engine_scaling.md` section 2).
pub(crate) async fn bounded_by<T, E>(
    deadline: Duration,
    call: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, ScannerError>
where
    ScannerError: From<E>,
{
    match tokio::time::timeout(deadline, call).await {
        Ok(result) => result.map_err(ScannerError::from),
        Err(_) => Err(ScannerError::Daemon(no_answer(deadline))),
    }
}

/// What a call that outlasted its deadline failed with.
fn no_answer(deadline: Duration) -> crate::daemon::DaemonError {
    crate::daemon::DaemonError::TimedOut(format!("no answer within {deadline:?}"))
}

/// Retry delays for keys whose work keeps failing (a key-custody backend
/// that is down, an order whose recompute fails). The first failures retry
/// at once, so a blip costs nothing; after that each retry waits twice as
/// long, up to a minute, so a dead backend costs one attempt a minute
/// instead of a deadline every round. Keyed by an id type
/// (`Backoff<TenantId>` for scans, `Backoff<OrderId>` for recomputes), so a
/// tenant's retry state can't be consulted or recorded for an order, or the
/// other way round.
pub(crate) struct Backoff<K> {
    /// By key: consecutive failures, retry-not-before, last failure.
    failures: parking_lot::Mutex<HashMap<K, retry::Retry>>,
    started: Instant,
}

impl<K> Default for Backoff<K> {
    fn default() -> Self {
        Self {
            failures: parking_lot::Mutex::default(),
            started: Instant::now(),
        }
    }
}

impl<K: Clone + Eq + std::hash::Hash> Backoff<K> {
    pub(crate) fn failed(&self, key: &K) {
        let mut failures = self.failures.lock();
        let next = retry::Retry::failed(failures.get(key).copied(), self.started.elapsed());
        failures.insert(key.clone(), next);
    }

    pub(crate) fn succeeded(&self, key: &K) {
        self.failures.lock().remove(key);
    }

    /// Keys still waiting out their delay. A key stays counted until it
    /// succeeds (or stops failing for `FORGET_AFTER`), so repeated failures
    /// keep lengthening its delay.
    pub(crate) fn waiting(&self) -> Vec<K> {
        let now = self.started.elapsed();
        let mut failures = self.failures.lock();
        failures.retain(|_, retry| !retry.forgotten(now));
        failures
            .iter()
            .filter(|(_, retry)| retry.waiting(now))
            .map(|(key, _)| key.clone())
            .collect()
    }

    pub(crate) fn is_waiting(&self, key: &K) -> bool {
        self.failures
            .lock()
            .get(key)
            .is_some_and(|retry| retry.waiting(self.started.elapsed()))
    }
}

/// What a network's scheduler keeps in memory between rounds. Everything
/// here can be lost (a restart, a panic) at the cost of some repeated work:
/// the durable state is in SQLite.
#[derive(Default)]
pub struct ScanState {
    /// Wakes webhook delivery when something was just enqueued, so a
    /// settlement's webhook goes out at once rather than on the next poll.
    webhooks: std::sync::Arc<tokio::sync::Notify>,
    /// Wakes the network's loops when its node announces a block or a pool
    /// transaction (`docs/monero_zmq.md`).
    node_wakes: std::sync::Arc<crate::node_events::NodeWakes>,
    mempool: mempool::MempoolState,
    blocks: blocks::BlockState,
    settlement: settlement::SettlementState,
    upkeep: upkeep::UpkeepState,
    /// Tenants whose scans keep failing.
    backoff: Backoff<crate::store::TenantId>,
    /// Orders whose status recompute keeps failing.
    order_backoff: Backoff<crate::store::OrderId>,
    /// How rounds, units and calls are sized: [`ScanTuning::DEFAULT`] but
    /// in tests and the round length sweep.
    tuning: ScanTuning,
    /// What the scanner does, recorded for the engine page
    /// (`docs/engine_visualizer.md`).
    activity: std::sync::Arc<crate::activity::Activity>,
}

impl ScanState {
    /// State that wakes `webhooks` whenever it enqueues webhook deliveries.
    pub fn waking(webhooks: std::sync::Arc<tokio::sync::Notify>) -> Self {
        Self {
            webhooks,
            ..Self::default()
        }
    }

    /// This state, keeping its block scan's progress in `progress`, which
    /// `/status` reads (`docs/engine_scaling.md` section 6).
    #[must_use = "the state with progress reporting is returned, not changed in place"]
    pub fn with_progress(mut self, progress: crate::scaling::SharedProgress) -> Self {
        self.blocks = blocks::BlockState::with_progress(progress);
        self
    }

    /// The time the next round may take: the tuning's round, unless a
    /// large block's smallest unit needs more (`docs/engine_scaling.md`
    /// section 4).
    pub fn round_budget(&self, daemon: &dyn MoneroDaemonClient, stores: usize) -> Duration {
        self.blocks.round_budget(daemon, stores, &self.tuning)
    }

    /// This state, scanning with `tuning` in place of
    /// [`ScanTuning::DEFAULT`]: for tests and the round length sweep. A
    /// tuning the scanner can't run is refused.
    pub fn with_tuning(mut self, tuning: ScanTuning) -> Result<Self, TuningError> {
        tuning.validate()?;
        self.tuning = tuning;
        Ok(self)
    }

    /// How rounds, units and calls are sized.
    pub fn tuning(&self) -> &ScanTuning {
        &self.tuning
    }

    /// This state, recording what it does in `activity` (which the admin
    /// API serves).
    #[must_use = "the state with an activity record is returned, not changed in place"]
    pub fn with_activity(mut self, activity: std::sync::Arc<crate::activity::Activity>) -> Self {
        self.activity = activity;
        self
    }

    /// Where this network's scanner records what it does.
    pub fn activity(&self) -> &std::sync::Arc<crate::activity::Activity> {
        &self.activity
    }

    /// This state, its loops woken by `wakes` (shared with `/status`).
    #[must_use = "the state with node wakes is returned, not changed in place"]
    pub fn with_wakes(mut self, wakes: std::sync::Arc<crate::node_events::NodeWakes>) -> Self {
        self.node_wakes = wakes;
        self
    }

    /// What wakes this network's loops early: its loops wait on it, and a
    /// node subscriber pokes it.
    pub fn node_wakes(&self) -> &std::sync::Arc<crate::node_events::NodeWakes> {
        &self.node_wakes
    }

    pub(crate) fn wake_webhooks(&self) {
        self.webhooks.notify_one();
    }
}

/// One round in progress: the inputs, the facts read at its start, and what
/// each tier has done so far.
pub(crate) struct Round<'a> {
    pub inputs: &'a RoundInputs<'a>,
    pub state: &'a ScanState,
    pub now: i64,
    /// The node's chain height. `None` if it couldn't be read: nothing that
    /// depends on the chain runs this round.
    pub tip: Option<u64>,
    /// The tip block's id, when the node gave it with the height: reorg
    /// detection then costs no lookup while the recorded chain ends there.
    pub tip_hash: Option<String>,
    pub handles: HashMap<&'a str, WalletHandle>,
    /// The mempool's transaction ids, when this round's poll succeeded. The
    /// vanished-payment check only runs on a real answer, never on "we
    /// couldn't look".
    pub pool_txids: Option<std::collections::HashSet<String>>,
    pub chain: chain::ChainRound,
    pub blocks: blocks::BlocksRound,
    pub mempool: mempool::MempoolRound,
    pub settlement: settlement::SettlementRound,
    pub upkeep: upkeep::UpkeepRound,
}

impl Round<'_> {
    pub(crate) fn network(&self) -> monero::Network {
        self.inputs.network
    }

    /// Runs `f` on the database worker, with this round's network:
    /// `round.db(|s, network| s.reorg_job(network))`. `f` may fail with a
    /// store error or a scanner error.
    pub(crate) async fn db<T, E>(
        &self,
        f: impl FnOnce(&Store, monero::Network) -> Result<T, E> + Send + 'static,
    ) -> Result<T, ScannerError>
    where
        T: Send + 'static,
        E: Into<ScannerError> + Send + 'static,
    {
        let network = self.network();
        self.inputs
            .db
            .run(Class::Scanner, move |s| f(s, network).map_err(Into::into))
            .await
    }
}

async fn step(tier: Tier, round: &mut Round<'_>, until: Instant) -> Progress {
    match tier {
        Tier::Chain => chain::step(round, until).await,
        Tier::Blocks => blocks::step(round, until).await,
        Tier::Mempool => mempool::step(round, until).await,
        Tier::Settlement => settlement::step(round, until).await,
        Tier::Upkeep => upkeep::step(round, until).await,
    }
}

/// Runs one round for one network within `budget`.
///
/// Pass 1 gives each tier, in priority order, its reserved share (time a tier
/// doesn't use goes to the tiers after it). Pass 2 gives whatever is left to
/// the tiers that still have work, again in priority order. A tier with work
/// always completes at least one unit, even over budget, so every kind of
/// work advances every round.
pub async fn run_round(
    state: &ScanState,
    inputs: &RoundInputs<'_>,
    budget: Duration,
) -> RoundReport {
    run_round_at(state, inputs, budget, crate::now_unix()).await
}

// Explicit Unix time lets generated histories advance persisted retry deadlines
// together with Tokio's clock, without sleeping or changing process-global time.
async fn run_round_at(
    state: &ScanState,
    inputs: &RoundInputs<'_>,
    budget: Duration,
    now: i64,
) -> RoundReport {
    let started = Instant::now();
    let mut laps = Laps::new(started);
    // A round that will look at the pool asks for the tip and the pool
    // together: one request while the chain hasn't moved.
    let watching = mempool::watching(inputs, now).await;
    let (tip_answer, polled) = if matches!(watching, Ok(true)) {
        match tokio::time::timeout(CALL_DEADLINE, inputs.daemon.get_tip_and_mempool()).await {
            Ok((tip, pool)) => (tip.map_err(ScannerError::from), Some(pool)),
            Err(_) => (
                Err(no_answer(CALL_DEADLINE).into()),
                Some(Err(no_answer(CALL_DEADLINE))),
            ),
        }
    } else {
        (bounded(inputs.daemon.get_tip()).await, None)
    };
    let tip_request = laps.lap();
    let (tip, tip_hash, tip_error) = match tip_answer {
        Ok(tip) => (Some(tip.height), tip.hash, None),
        Err(error) => {
            shared::throttled!(
                format!("round-height:{:?}", inputs.network),
                warn,
                network = ?inputs.network,
                error = %error,
                "reading the chain height failed - only the mempool is scanned this round"
            );
            (None, None, Some(error))
        }
    };
    if let Some(tip) = tip {
        state
            .mempool
            .last_tip
            .store(tip, std::sync::atomic::Ordering::Relaxed);
    }
    let activity = state.activity();
    let round_number = activity.next_round();
    activity.record(Event::RoundStarted {
        round: round_number,
        budget_ms: millis(budget),
        tip,
    });
    // The opening: the pool check and the tip request, one span, as they
    // make one request to the node.
    let (start_ms, ms) = tip_request;
    activity.record(Event::Work {
        tier: Tier::Chain,
        start_ms,
        ms,
        what: Work::TipRequest,
    });
    let mut round = Round {
        inputs,
        state,
        now,
        tip,
        tip_hash,
        handles: inputs
            .tenants
            .iter()
            .map(|(id, handle)| (id.as_str(), *handle))
            .collect(),
        pool_txids: None,
        chain: chain::ChainRound::default(),
        blocks: blocks::BlocksRound::resume(&state.blocks, inputs),
        mempool: mempool::MempoolRound::starting(watching, polled),
        settlement: settlement::SettlementRound::default(),
        upkeep: upkeep::UpkeepRound::default(),
    };
    let mut report = RoundReport {
        steps: PerTier::filled(0),
        outcomes: PerTier::filled(TierOutcome::Backlogged),
        error: tip_error,
    };
    // The validated tuning is held by ScanState. Keep the policy independently
    // callable, while the runner executes and records each requested effect.
    let mut scheduler =
        match scheduler::Scheduler::with_generation(&state.tuning, budget, round_number) {
            Ok(scheduler) => scheduler,
            Err(error) => {
                report.error = Some(ScannerError::Internal(format!(
                    "invalid scanner tuning: {error}"
                )));
                return report;
            }
        };
    while let Some(effect) = scheduler.request(started.elapsed()) {
        let tier = effect.tier();
        let progress = step(tier, &mut round, started + effect.until()).await;
        let (start_ms, ms) = laps.lap();
        activity.record(Event::Unit {
            tier,
            pass: effect.pass(),
            start_ms,
            ms,
            progress: UnitProgress::from(&progress),
        });
        let outcome = match progress {
            Progress::Advanced => None,
            Progress::Idle => Some(TierOutcome::Idle),
            Progress::Blocked(reason) => {
                tracing::debug!(network = ?inputs.network, %tier, %reason, "tier waiting");
                Some(TierOutcome::Blocked(reason))
            }
            Progress::Failed(error) => {
                tracing::warn!(network = ?inputs.network, %tier, error = %error, "work unit failed (retried next round)");
                report.error.get_or_insert(error);
                Some(TierOutcome::Failed)
            }
        };
        scheduler.complete(effect, outcome);
        if let Some(outcome) = outcome {
            activity.record(Event::TierEnded { tier, outcome });
        }
    }
    report.steps = scheduler.steps();
    report.outcomes = scheduler.outcomes();
    blocks::carry(&mut round).await;
    let (start_ms, ms) = laps.lap();
    activity.record(Event::Work {
        tier: Tier::Blocks,
        start_ms,
        ms,
        what: Work::CacheCarry,
    });
    activity.record(Event::RoundFinished {
        round: round_number,
        ms: laps.total(),
        backlogged: report.backlogged(),
    });
    report
}

/// A round's time cut into back-to-back spans, in whole milliseconds since
/// its start: each span starts where the last ended, so the spans add up
/// to the round exactly.
struct Laps {
    started: Instant,
    last_ms: u64,
}

impl Laps {
    const fn new(started: Instant) -> Self {
        Self {
            started,
            last_ms: 0,
        }
    }

    /// The span since the last lap: its start and length.
    fn lap(&mut self) -> (u64, u64) {
        let now = millis(self.started.elapsed()).max(self.last_ms);
        let span = (self.last_ms, now - self.last_ms);
        self.last_ms = now;
        span
    }

    /// Where the last lap ended.
    const fn total(&self) -> u64 {
        self.last_ms
    }
}

/// `n` as a count in the activity record.
pub(crate) fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// `duration` in whole milliseconds, for the activity record.
pub fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl From<&Progress> for UnitProgress {
    fn from(progress: &Progress) -> Self {
        match progress {
            Progress::Advanced => Self::Advanced,
            Progress::Idle => Self::Idle,
            Progress::Blocked(wait) => Self::Blocked(*wait),
            Progress::Failed(_) => Self::Failed,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;

#[cfg(any(test, feature = "fuzzing"))]
pub(crate) fn explore_mempool(data: &[u8]) {
    mempool::explore(data);
}
