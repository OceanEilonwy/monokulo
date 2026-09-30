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
use crate::store::SharedStore;

pub use blocks::ScannedBlock;

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

    fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            Tier::Chain => "chain",
            Tier::Blocks => "blocks",
            Tier::Mempool => "mempool",
            Tier::Settlement => "settlement",
            Tier::Upkeep => "upkeep",
        }
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

/// What one unit of work did. The executor can't mistake "nothing to do"
/// for "failed".
#[derive(Debug)]
pub(crate) enum Progress {
    /// Did something; more may be due.
    Advanced,
    /// Nothing is due for this tier this round.
    Idle,
    /// Work exists but must wait for something else (a reorg rewind, a
    /// retry delay, the chain height).
    Blocked(&'static str),
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
    Blocked(&'static str),
    Failed,
}

/// What a round did, for the loop and for status reporting.
#[derive(Debug)]
pub struct RoundReport {
    pub steps: [u32; 5],
    pub outcomes: [TierOutcome; 5],
    /// The first unit failure, if any. Other tiers still ran.
    pub error: Option<ScannerError>,
}

impl RoundReport {
    /// Whether any tier stopped with work left: the loop starts the next
    /// round at once instead of waiting for the poll interval.
    pub fn backlogged(&self) -> bool {
        self.outcomes.contains(&TierOutcome::Backlogged)
    }

    pub fn outcome(&self, tier: Tier) -> TierOutcome {
        self.outcomes[tier.index()]
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
    pub store: &'a SharedStore,
    pub custody: &'a dyn KeyCustody,
    pub daemon: &'a dyn MoneroDaemonClient,
    pub network: &'a str,
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

/// Retry delays for tenants whose scans keep failing (a key-custody backend
/// that is down). The first failures retry at once, so a blip costs nothing;
/// after that each retry waits twice as long, up to a minute, so a dead
/// backend costs one attempt a minute instead of a deadline every round.
#[derive(Default)]
pub(crate) struct Backoff {
    failures: parking_lot::Mutex<HashMap<String, (u32, Instant)>>,
}

impl Backoff {
    const FREE_RETRIES: u32 = 2;
    const MAX_DELAY: Duration = Duration::from_secs(60);

    pub(crate) fn failed(&self, key: &str) {
        let mut failures = self.failures.lock();
        let count = failures.get(key).map_or(0, |(n, _)| *n) + 1;
        let delay = if count <= Self::FREE_RETRIES {
            Duration::ZERO
        } else {
            Duration::from_secs(1u64 << (count - Self::FREE_RETRIES).min(6)).min(Self::MAX_DELAY)
        };
        failures.insert(key.to_string(), (count, Instant::now() + delay));
    }

    pub(crate) fn succeeded(&self, key: &str) {
        self.failures.lock().remove(key);
    }

    /// Keys still waiting out their delay. A key stays counted until it
    /// succeeds, so repeated failures keep lengthening its delay.
    pub(crate) fn waiting(&self) -> Vec<String> {
        let now = Instant::now();
        self.failures.lock().iter().filter(|(_, (_, until))| *until > now).map(|(key, _)| key.clone()).collect()
    }

    pub(crate) fn is_waiting(&self, key: &str) -> bool {
        self.failures.lock().get(key).is_some_and(|(_, until)| *until > Instant::now())
    }
}

/// What a network's scheduler keeps in memory between rounds. Everything
/// here can be lost (a restart, a panic) at the cost of some repeated work:
/// the durable state is in SQLite.
#[derive(Default)]
pub struct ScanState {
    mempool: mempool::MempoolState,
    settlement: settlement::SettlementState,
    backoff: Backoff,
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
    pub(crate) fn network(&self) -> &'a str {
        self.inputs.network
    }
}

async fn step(tier: Tier, round: &mut Round<'_>, until: Instant) -> Progress {
    match tier {
        Tier::Chain => chain::step(round).await,
        Tier::Blocks => blocks::step(round, until).await,
        Tier::Mempool => mempool::step(round, until).await,
        Tier::Settlement => settlement::step(round, until).await,
        Tier::Upkeep => upkeep::step(round).await,
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
                format!("round-height:{}", inputs.network),
                warn,
                network = %inputs.network,
                error = %error,
                "reading the chain height failed - only the mempool is scanned this round"
            );
            (None, Some(error))
        }
    };
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
    let mut report = RoundReport { steps: [0; 5], outcomes: [TierOutcome::Backlogged; 5], error: tip_error };
    let mut open = [true; 5];

    for pass_end in [None, Some(round_end)] {
        for tier in Tier::ALL {
            let until = pass_end.unwrap_or_else(|| {
                (Instant::now() + budget * tier.reserved_percent() / 100).min(round_end)
            });
            let i = tier.index();
            while open[i] {
                if report.steps[i] > 0 && Instant::now() >= until {
                    break;
                }
                let progress = step(tier, &mut round, until).await;
                report.steps[i] += 1;
                match progress {
                    Progress::Advanced => {}
                    Progress::Idle => {
                        open[i] = false;
                        report.outcomes[i] = TierOutcome::Idle;
                    }
                    Progress::Blocked(reason) => {
                        open[i] = false;
                        report.outcomes[i] = TierOutcome::Blocked(reason);
                        tracing::debug!(network = %inputs.network, tier = tier.name(), reason, "tier waiting");
                    }
                    Progress::Failed(error) => {
                        open[i] = false;
                        report.outcomes[i] = TierOutcome::Failed;
                        tracing::warn!(network = %inputs.network, tier = tier.name(), error = %error, "work unit failed (retried next round)");
                        report.error.get_or_insert(error);
                    }
                }
            }
        }
    }
    report
}
