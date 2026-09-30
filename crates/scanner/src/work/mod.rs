//! The scanner as small, durable work units chosen by one scheduler per
//! network (docs/scanner_microtasks.md).
//!
//! A *round* runs the five tiers in priority order. Each tier has a reserved
//! share of the round's time and completes at least one unit per round when
//! it has work, so a slow node, a throttled CPU or a flood of one kind of work
//! slows every tier but starves none. Units keep their progress in SQLite, so
//! a round cut short, a crash or a restart repeats at most the unit in flight.

mod blocks;
pub(crate) mod chain;
mod mempool;
mod settlement;
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

/// The kinds of work, in priority order within a round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Reorg detection and reconciliation.
    Chain,
    /// Scanning blocks for tenants: the new ones first, then catch-up.
    Blocks,
    /// Scanning the mempool for zero-confirmation payments.
    Mempool,
    /// Payments that left the pool, and order status recomputes.
    Settlement,
    /// Bookkeeping that can lag: scanned ranges, void rechecks, pruning.
    Upkeep,
}

impl Tier {
    pub const ALL: [Tier; 5] = [Tier::Chain, Tier::Blocks, Tier::Mempool, Tier::Settlement, Tier::Upkeep];

    /// The share of a round's time reserved for this tier, in percent.
    const fn reserved_percent(self) -> u32 {
        match self {
            Tier::Chain => 20,
            Tier::Blocks => 40,
            Tier::Mempool => 15,
            Tier::Settlement => 20,
            Tier::Upkeep => 5,
        }
    }

    const fn index(self) -> usize {
        self as usize
    }

}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Tier::Chain => "chain",
            Tier::Blocks => "blocks",
            Tier::Mempool => "mempool",
            Tier::Settlement => "settlement",
            Tier::Upkeep => "upkeep",
        })
    }
}

const _: () = {
    let mut total = 0;
    let mut i = 0;
    while i < Tier::ALL.len() {
        total += Tier::ALL[i].reserved_percent();
        i += 1;
    }
    assert!(total == 100, "tier shares must cover the whole round");
};

/// One value per tier, indexed by [`Tier`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PerTier<T>([T; 5]);

impl<T: Copy> PerTier<T> {
    fn filled(value: T) -> Self {
        Self([value; 5])
    }

    pub fn iter(&self) -> impl Iterator<Item = (Tier, T)> + '_ {
        Tier::ALL.into_iter().map(|tier| (tier, self.0[tier.index()]))
    }
}

impl<T> std::ops::Index<Tier> for PerTier<T> {
    type Output = T;
    fn index(&self, tier: Tier) -> &T {
        &self.0[tier.index()]
    }
}

impl<T> std::ops::IndexMut<Tier> for PerTier<T> {
    fn index_mut(&mut self, tier: Tier) -> &mut T {
        &mut self.0[tier.index()]
    }
}

/// What a tier is waiting for when it has work it can't do yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// The node's chain height couldn't be read this round.
    ChainHeightUnknown,
    /// A reorg is being reconciled: blocks wait for the rewind.
    ReorgBeingReconciled,
    /// A rewind happened this round: replacement blocks are scanned from
    /// the next, against a freshly read chain.
    RewoundThisRound,
    /// The node failed or didn't answer; retried next round.
    NodeFailed,
    /// The node reports a tip it can't serve yet (first run).
    NodeCannotServeTip,
    /// The mempool couldn't be read.
    MempoolUnreadable,
    /// Every remaining reorg candidate is waiting out a retry delay.
    ReorgCandidatesRetrying,
    /// The node's next block doesn't extend the recorded chain: a reorg the
    /// chain tier hasn't opened a job for yet (it failed this round, or the
    /// fork happened since it looked).
    ChainDiverged,
}

impl std::fmt::Display for Wait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Wait::ChainHeightUnknown => "the chain height is unknown",
            Wait::ReorgBeingReconciled => "a reorganisation is being reconciled",
            Wait::RewoundThisRound => "rewound this round; replacement blocks are scanned from the next",
            Wait::NodeFailed => "the node failed",
            Wait::NodeCannotServeTip => "the node can't serve its own tip yet",
            Wait::MempoolUnreadable => "the mempool couldn't be read",
            Wait::ReorgCandidatesRetrying => "reorg candidates are waiting to be retried",
            Wait::ChainDiverged => "the node's chain differs from the recorded one; waiting for reorg reconciliation",
        })
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

/// How a tier ended its round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TierOutcome {
    /// Ran out of work.
    Idle,
    /// Ran out of time with work left.
    Backlogged,
    Blocked(Wait),
    Failed,
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
        self.outcomes.iter().any(|(_, outcome)| outcome == TierOutcome::Backlogged)
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
    pub tenants: &'a [(String, WalletHandle)],
    pub reorg_check_depth: u64,
    pub grace_period_seconds: i64,
    pub scan_chunk_memory_budget_mb: u32,
}

/// The time one round may take. A round ends sooner when every tier runs out
/// of work; a round that ends with work left is followed at once by the next.
pub const ROUND_BUDGET: Duration = Duration::from_secs(10);

/// How long one daemon call inside a unit may take before the unit treats
/// it as failed.
pub(crate) const CALL_DEADLINE: Duration = Duration::from_secs(15);

/// A daemon (or other) call with [`CALL_DEADLINE`].
pub(crate) async fn bounded<T, E>(call: impl std::future::Future<Output = Result<T, E>>) -> Result<T, ScannerError>
where
    ScannerError: From<E>,
{
    match tokio::time::timeout(CALL_DEADLINE, call).await {
        Ok(result) => result.map_err(ScannerError::from),
        Err(_) => Err(ScannerError::Daemon(crate::daemon::DaemonError::Request(format!(
            "no answer within {CALL_DEADLINE:?}"
        )))),
    }
}

/// What a [`Backoff`] is keyed by. The kind is part of the type, so a
/// tenant's retry state can't be consulted or recorded for an order, or the
/// other way round.
pub(crate) trait BackoffKind {}

/// Keys are tenant (store) ids: scans that keep failing.
pub(crate) enum TenantKey {}
impl BackoffKind for TenantKey {}

/// Keys are order ids: status recomputes that keep failing.
pub(crate) enum OrderKey {}
impl BackoffKind for OrderKey {}

/// Retry delays for keys whose work keeps failing (a key-custody backend
/// that is down, an order whose recompute fails). The first failures retry
/// at once, so a blip costs nothing; after that each retry waits twice as
/// long, up to a minute, so a dead backend costs one attempt a minute
/// instead of a deadline every round.
pub(crate) struct Backoff<K: BackoffKind> {
    /// By key: consecutive failures, retry-not-before, last failure.
    failures: parking_lot::Mutex<HashMap<String, (u32, Instant, Instant)>>,
    kind: std::marker::PhantomData<fn() -> K>,
}

impl<K: BackoffKind> Default for Backoff<K> {
    fn default() -> Self {
        Self { failures: Default::default(), kind: std::marker::PhantomData }
    }
}

impl<K: BackoffKind> Backoff<K> {
    const FREE_RETRIES: u32 = 2;
    const MAX_DELAY: Duration = Duration::from_secs(60);
    /// A key that hasn't failed for this long is forgotten: it is no longer
    /// being tried (a store with nothing in scope, an order that settled).
    const FORGET_AFTER: Duration = Duration::from_secs(60 * 60);

    pub(crate) fn failed(&self, key: &str) {
        let mut failures = self.failures.lock();
        let count = failures.get(key).map_or(0, |(n, _, _)| *n) + 1;
        let delay = if count <= Self::FREE_RETRIES {
            Duration::ZERO
        } else {
            Duration::from_secs(1u64 << (count - Self::FREE_RETRIES).min(6)).min(Self::MAX_DELAY)
        };
        let now = Instant::now();
        failures.insert(key.to_string(), (count, now + delay, now));
    }

    pub(crate) fn succeeded(&self, key: &str) {
        self.failures.lock().remove(key);
    }

    /// Keys still waiting out their delay. A key stays counted until it
    /// succeeds (or stops failing for `FORGET_AFTER`), so repeated failures
    /// keep lengthening its delay.
    pub(crate) fn waiting(&self) -> Vec<String> {
        let now = Instant::now();
        let mut failures = self.failures.lock();
        failures.retain(|_, (_, _, last)| now.saturating_duration_since(*last) < Self::FORGET_AFTER);
        failures.iter().filter(|(_, (_, until, _))| *until > now).map(|(key, _)| key.clone()).collect()
    }

    pub(crate) fn is_waiting(&self, key: &str) -> bool {
        self.failures.lock().get(key).is_some_and(|(_, until, _)| *until > Instant::now())
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
    mempool: mempool::MempoolState,
    blocks: blocks::BlockState,
    settlement: settlement::SettlementState,
    upkeep: upkeep::UpkeepState,
    /// Tenants whose scans keep failing.
    backoff: Backoff<TenantKey>,
    /// Orders whose status recompute keeps failing.
    order_backoff: Backoff<OrderKey>,
}

impl ScanState {
    /// State that wakes `webhooks` whenever it enqueues webhook deliveries.
    pub fn waking(webhooks: std::sync::Arc<tokio::sync::Notify>) -> Self {
        Self { webhooks, ..Self::default() }
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

impl<'a> Round<'a> {
    pub(crate) fn network(&self) -> &'static str {
        crate::network::network_str(self.inputs.network)
    }

    /// Runs `f` on the database worker, with this round's network name:
    /// `round.db(|s, network| s.reorg_job(network))`. `f` may fail with a
    /// store error or a scanner error.
    pub(crate) async fn db<T, E>(&self, f: impl FnOnce(&Store, &str) -> Result<T, E> + Send + 'static) -> Result<T, ScannerError>
    where
        T: Send + 'static,
        E: Into<ScannerError> + Send + 'static,
    {
        let network = self.network();
        self.inputs.db.run(Class::Scanner, move |s| f(s, network).map_err(Into::into)).await
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
pub async fn run_round(state: &ScanState, inputs: &RoundInputs<'_>, budget: Duration) -> RoundReport {
    let started = Instant::now();
    let round_end = started + budget;
    let (tip, tip_error) = match bounded(inputs.daemon.get_height()).await {
        Ok(tip) => (Some(tip), None),
        Err(error) => {
            shared::throttled!(
                format!("round-height:{:?}", inputs.network),
                warn,
                network = ?inputs.network,
                error = %error,
                "reading the chain height failed - only the mempool is scanned this round"
            );
            (None, Some(error))
        }
    };
    if let Some(tip) = tip {
        state.mempool.last_tip.store(tip, std::sync::atomic::Ordering::Relaxed);
    }
    let mut round = Round {
        inputs,
        state,
        now: crate::now_unix(),
        tip,
        handles: inputs.tenants.iter().map(|(id, handle)| (id.as_str(), *handle)).collect(),
        pool_txids: None,
        chain: Default::default(),
        blocks: Default::default(),
        mempool: Default::default(),
        settlement: Default::default(),
        upkeep: Default::default(),
    };
    let mut report = RoundReport { steps: PerTier::filled(0), outcomes: PerTier::filled(TierOutcome::Backlogged), error: tip_error };
    let mut open = PerTier::filled(true);

    for pass_end in [None, Some(round_end)] {
        for tier in Tier::ALL {
            let until = pass_end.unwrap_or_else(|| {
                (Instant::now() + budget * tier.reserved_percent() / 100).min(round_end)
            });
                        while open[tier] {
                if report.steps[tier] > 0 && Instant::now() >= until {
                    break;
                }
                let progress = step(tier, &mut round, until).await;
                report.steps[tier] += 1;
                match progress {
                    Progress::Advanced => {}
                    Progress::Idle => {
                        open[tier] = false;
                        report.outcomes[tier] = TierOutcome::Idle;
                    }
                    Progress::Blocked(reason) => {
                        open[tier] = false;
                        report.outcomes[tier] = TierOutcome::Blocked(reason);
                        tracing::debug!(network = ?inputs.network, %tier, %reason, "tier waiting");
                    }
                    Progress::Failed(error) => {
                        open[tier] = false;
                        report.outcomes[tier] = TierOutcome::Failed;
                        tracing::warn!(network = ?inputs.network, %tier, error = %error, "work unit failed (retried next round)");
                        report.error.get_or_insert(error);
                    }
                }
            }
        }
    }
    report
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
