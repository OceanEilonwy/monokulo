//! Deterministic round policy.
//!
//! The runner owns I/O; this machine issues one
//! unit at a time and accepts only its matching completion. Times are elapsed
//! monotonic durations from the round's start, including its opening RPCs.
use std::time::Duration;

use super::{PerTier, ScanTuning, Tier, TierOutcome, TuningError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunUnit {
    tier: Tier,
    pass: u8,
    until: Duration,
    sequence: u64,
    generation: u64,
}

impl RunUnit {
    pub fn tier(self) -> Tier {
        self.tier
    }
    pub fn pass(self) -> u8 {
        self.pass
    }
    pub fn until(self) -> Duration {
        self.until
    }
}

/// `None` as a completion outcome means the unit advanced and remains open.
/// Idle, blocked and failed outcomes close only that tier for this round.
pub struct Scheduler {
    budget: Duration,
    shares: PerTier<Duration>,
    steps: PerTier<u32>,
    outcomes: PerTier<TierOutcome>,
    pass: u8,
    index: usize,
    until: Option<Duration>,
    pending: Option<RunUnit>,
    sequence: u64,
    clock: Duration,
    generation: u64,
}

impl Scheduler {
    pub fn new(tuning: &ScanTuning, budget: Duration) -> Result<Self, TuningError> {
        tuning.validate()?;
        let mut shares = PerTier::filled(Duration::ZERO);
        for tier in Tier::ALL {
            // Duration multiplication by a percentage without narrowing nanoseconds.
            shares[tier] = budget / 100 * tuning.shares.percent(tier)
                + Duration::from_nanos(
                    (budget.as_nanos() % 100) as u64 * u64::from(tuning.shares.percent(tier)) / 100,
                );
        }
        Ok(Self {
            budget,
            shares,
            steps: PerTier::filled(0),
            outcomes: PerTier::filled(TierOutcome::Backlogged),
            pass: 1,
            index: 0,
            until: None,
            pending: None,
            sequence: 0,
            clock: Duration::ZERO,
            generation: 0,
        })
    }

    /// Distinguish completions across rounds in the same network runner.
    pub fn with_generation(
        tuning: &ScanTuning,
        budget: Duration,
        generation: u64,
    ) -> Result<Self, TuningError> {
        let mut machine = Self::new(tuning, budget)?;
        machine.generation = generation;
        Ok(machine)
    }

    /// No second effect is issued until the first completes. Backwards clock
    /// observations cannot reopen an already expired share.
    pub fn request(&mut self, elapsed: Duration) -> Option<RunUnit> {
        self.clock = self.clock.max(elapsed);
        if self.pending.is_some() {
            return None;
        }
        while self.pass <= 2 {
            if self.index == Tier::ALL.len() {
                self.index = 0;
                self.pass += 1;
                self.until = None;
                continue;
            }
            let tier = Tier::ALL[self.index];
            let until = *self.until.get_or_insert_with(|| {
                if self.pass == 1 {
                    self.clock
                        .saturating_add(self.shares[tier])
                        .min(self.budget)
                } else {
                    self.budget
                }
            });
            if self.outcomes[tier] != TierOutcome::Backlogged
                || (self.steps[tier] > 0 && self.clock >= until)
            {
                self.index += 1;
                self.until = None;
                continue;
            }
            self.sequence = self.sequence.saturating_add(1);
            let effect = RunUnit {
                tier,
                pass: self.pass,
                until,
                sequence: self.sequence,
                generation: self.generation,
            };
            self.pending = Some(effect);
            return Some(effect);
        }
        None
    }

    /// Duplicate or obsolete completions have no effect. A Backlogged outcome
    /// has the same meaning as an advancing unit.
    pub fn complete(&mut self, effect: RunUnit, outcome: Option<TierOutcome>) -> bool {
        if self.pending != Some(effect) {
            return false;
        }
        self.pending = None;
        self.steps[effect.tier] = self.steps[effect.tier].saturating_add(1);
        if let Some(outcome) = outcome {
            self.outcomes[effect.tier] = outcome;
        }
        true
    }

    pub fn steps(&self) -> PerTier<u32> {
        self.steps
    }
    pub fn outcomes(&self) -> PerTier<TierOutcome> {
        self.outcomes
    }
}

#[cfg(test)]
#[path = "../../tests/verification/work/scheduler/properties.rs"]
#[cfg_attr(coverage_nightly, coverage(off))]
mod properties;
