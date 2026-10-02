//! The scanner's tuning: how long a round is, how it is shared out, and how
//! much work one unit, request or call takes on (docs/scanner_microtasks.md,
//! docs/engine_scaling.md).
//!
//! These are design values, not settings. The engine runs
//! [`ScanTuning::DEFAULT`], checked at build time; nothing reads them from
//! a file or the database. A test, or the round length sweep, can run other
//! values explicitly ([`super::ScanState::with_tuning`]), through the same
//! [`ScanTuning::validate`] the default passes, so no combination production
//! would refuse can be run anywhere.
//!
//! What isn't the scanner's to tune stays a plain constant where it is
//! used: monerod's limits (`daemon_rpc::TXS_PER_REQUEST`), network timeouts
//! (`daemon_rpc::REQUEST_TIMEOUT`, `work::CALL_DEADLINE`), consensus rules,
//! and a link's cold-start guesses (`link`).

use std::time::Duration;

use super::Tier;

/// Each tier's share of a round, in percent; together, the whole round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TierShares {
    pub chain: u32,
    pub blocks: u32,
    pub mempool: u32,
    pub settlement: u32,
    pub upkeep: u32,
}

impl TierShares {
    /// `tier`'s share, in percent.
    pub const fn percent(&self, tier: Tier) -> u32 {
        match tier {
            Tier::Chain => self.chain,
            Tier::Blocks => self.blocks,
            Tier::Mempool => self.mempool,
            Tier::Settlement => self.settlement,
            Tier::Upkeep => self.upkeep,
        }
    }

    const fn total(&self) -> u32 {
        self.chain + self.blocks + self.mempool + self.settlement + self.upkeep
    }
}

/// The scanner's tuning. See the module docs, and each field's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScanTuning {
    /// The time one round may take. A round ends sooner when every tier
    /// runs out of work; a round that ends with work left is followed at
    /// once by the next.
    ///
    /// It only matters while there is a backlog: a caught-up round ends in
    /// well under a second and the loop sleeps for the poll interval. Then
    /// it sets two things. Each tier's per-call times are a share of it
    /// ([`Self::reserved`]), so it sets how much of a node's link one block
    /// request uses. And the mempool and settlement tiers get a turn once a
    /// round, so it is about the longest they wait while blocks catch up
    /// (zero-confirmation detection doesn't wait: the fast mempool path
    /// runs every 250 ms).
    ///
    /// 10 s is measured, not guessed (`cargo xtask stress rounds`,
    /// docs/engine_stress.md, round length sweep). Each round pays about
    /// one round trip of its own, and each block request one more on top of
    /// its share of the Blocks tier's time:
    ///
    /// - Over a nearby node (50 ms), throughput barely changes from 5 to
    ///   20 s (2 Mbit/s: 17.8 to 18.3 blocks a second). Only the wait
    ///   changes.
    /// - Over a Tor-like node (800 ms), rounds shorter than 10 s lose 28 %
    ///   at 5 s and 12 to 14 % at 7 s; 15 and 20 s gain 6 to 22 %.
    /// - Longer rounds cost: the wait grows from about 13 s to 17 to 26 s,
    ///   catch-up groups sharing the cache fetch more blocks twice as their
    ///   runs grow (16 groups: 1.45 times at 10 s, 2.04 at 20 s), and past
    ///   12.5 s a block request sized to the Blocks share no longer fits
    ///   three times within the 15 s minimum timeout (checked for the
    ///   default at build time).
    ///
    /// 10 s keeps a nearby node within 1 % of the longest round tried, a
    /// Tor-like one within 7 to 18 %, and the wait near 13 s.
    pub round_budget: Duration,
    /// Each tier's share of a round. The tier always runs at least one
    /// unit, so a unit that takes longer than its share delays every tier
    /// after it in the round: what one of its calls may take is set from
    /// the share ([`Self::reserved`]), not written down beside it.
    pub shares: TierShares,
    /// The most a round may be raised to for one page of a large block
    /// (docs/engine_scaling.md section 4).
    pub max_round_budget: Duration,
    /// Most tenants one block scan covers and one commit moves. A larger
    /// group is given the same block a page at a time before it moves on,
    /// within [`Self::blocks_per_unit`] scans a unit; past that the rest of
    /// the group stays at its cursor and becomes its own catch-up group.
    pub group_page: usize,
    /// Most block scans (a block for one page of its group's tenants) one
    /// unit makes before yielding the tier.
    pub blocks_per_unit: usize,
    /// Transactions of a block scanned for a tenant in one key-custody
    /// call. A call costs a hop to a worker thread or a round trip to
    /// another process, which a run of transactions shares. It is also how
    /// far a unit gets between looks at the clock, and how much of a block
    /// a tenant whose call fails has to be scanned for again.
    pub txs_per_scan: usize,
    /// Most tenants scanned at the same time.
    pub scan_concurrency: usize,
    /// Most headers fetched at once for blocks recorded without being
    /// scanned; one request ([`crate::daemon_rpc::MAX_HEADERS_PER_REQUEST`]).
    pub headers_per_fetch: u64,
    /// Most blocks one block request asks for, however small blocks have
    /// been (a long run of near-empty blocks): the scan memory budget alone
    /// would allow an enormous request then, and an older monerod ignoring
    /// `get_blocks.bin`'s own `max_block_count` hint has no other backstop.
    pub chunk_max_blocks: u64,
    /// How strongly a fetched run's own bytes a block move the running
    /// average: 0.3 follows a real shift in block size within a handful of
    /// runs without one odd run (a giant consolidation transaction, a run of
    /// empty blocks) swinging the next request.
    pub chunk_ewma_alpha: f64,
    /// One response's share of the scan memory budget is one in this many:
    /// the raw answer and its parse copies are held at once, so each answer
    /// is kept to a fraction of what the block cache may hold
    /// (docs/engine_scaling.md section 3).
    pub response_share_of_budget: u64,
    /// A block that would take longer than this to fetch whole over its
    /// node's link is scanned a page of transactions at a time instead
    /// (docs/engine_scaling.md section 4): well inside the two minutes
    /// after which a block counts as slow.
    pub whole_block_max_secs: f64,
}

/// A tuning the scanner refuses, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuningError {
    SharesNotWholeRound,
    NoRound,
    RoundOverMaximum,
    RaisedRoundSlow,
    HeadersOverOneRequest,
    WholeBlockSlow,
    ZeroCount,
    AlphaOutOfRange,
}

impl TuningError {
    pub const fn message(self) -> &'static str {
        match self {
            TuningError::SharesNotWholeRound => "tier shares must cover the whole round",
            TuningError::NoRound => "a round must have some time",
            TuningError::RoundOverMaximum => {
                "the round must not exceed the most it may be raised to"
            }
            TuningError::RaisedRoundSlow => {
                "a raised round must not by itself make a block count as slow"
            }
            TuningError::HeadersOverOneRequest => "a headers fetch must be one request",
            TuningError::WholeBlockSlow => {
                "a block fetched whole must be able to finish before it counts as slow"
            }
            TuningError::ZeroCount => {
                "page, unit, batch, concurrency and request sizes must be at least one"
            }
            TuningError::AlphaOutOfRange => {
                "the running average's weight must be above 0 and at most 1"
            }
        }
    }
}

impl std::fmt::Display for TuningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for TuningError {}

impl ScanTuning {
    /// What the engine runs.
    pub const DEFAULT: ScanTuning = ScanTuning {
        round_budget: Duration::from_secs(10),
        shares: TierShares {
            chain: 20,
            blocks: 40,
            mempool: 15,
            settlement: 20,
            upkeep: 5,
        },
        max_round_budget: Duration::from_secs(120),
        group_page: 256,
        blocks_per_unit: 8,
        txs_per_scan: 32,
        scan_concurrency: 32,
        headers_per_fetch: 256,
        chunk_max_blocks: 500,
        chunk_ewma_alpha: 0.3,
        response_share_of_budget: 8,
        whole_block_max_secs: 30.0,
    };

    /// `tier`'s share of a round of `budget`: the round's live length,
    /// which is raised while a large block is scanned in pages.
    pub const fn share_of(&self, tier: Tier, budget: Duration) -> Duration {
        // Whole nanoseconds; a round would need to run for centuries to
        // overflow the `u64`.
        Duration::from_nanos((budget.as_nanos() * self.shares.percent(tier) as u128 / 100) as u64)
    }

    /// `tier`'s share of a round of [`Self::round_budget`]: what one of its
    /// calls may take. The Blocks share is also the target for one block
    /// request ([`Self::target_call_secs`]).
    pub const fn reserved(&self, tier: Tier) -> Duration {
        self.share_of(tier, self.round_budget)
    }

    /// How long one block request should take over the node's measured link
    /// (docs/engine_scaling.md section 2): the Blocks tier's share of a
    /// round, so 4 s by default.
    ///
    /// It is a time and not a size because the scheduler divides time, not
    /// bytes. A round gives each tier a share of its seconds, and a tier
    /// always runs at least one unit, which can't be stopped part-way
    /// through a node request. One block request is therefore the smallest
    /// delay the Blocks tier can cause the tiers after it: the mempool tier
    /// (zero-confirmation payments), settlement, and upkeep. A request sized
    /// in bytes alone takes milliseconds on a LAN node and minutes over
    /// Tor. Sized by the link, it fits the Blocks tier's share on any link.
    ///
    /// It doesn't limit throughput. A round that ends with blocks left is
    /// followed at once by the next, so a catch-up walk keeps the link
    /// about as busy as it would be with larger requests. Larger requests
    /// would only spread the fixed round trip over more bytes.
    ///
    /// On a fast link this limit rarely applies: the response cap (an
    /// eighth of `payment.scan_chunk_memory_budget_mb`, 1 MB at the default
    /// 8 MB) binds first. The link limit takes over below the cap divided by
    /// this many seconds, about 2 Mbit/s at the default budget. Slow nodes,
    /// Tor nodes and large budgets all fall below that.
    ///
    /// A request's timeout is three times what the link says it needs, and
    /// never under 15 s ([`crate::link::timeout_for`]). By default three
    /// times this target fits within that floor (checked at build time;
    /// a longer round only loses the extra headroom), so a request
    /// sized to it gets nearly four times as long as it should need. If the
    /// rate estimate is out of date, the request runs slow but doesn't
    /// fail. A request sized to it counts every term its timeout counts
    /// ([`crate::link::LinkCost`]): the round trip, the node's time to first
    /// byte for each block, and the bytes at the link's rate.
    pub const fn target_call_secs(&self) -> f64 {
        self.reserved(Tier::Blocks).as_secs_f64()
    }

    /// The largest block response to ask for under a `budget_mb` scan
    /// budget: its share of it, and never less than 256 kB, so a tiny
    /// budget still fetches whole blocks.
    pub const fn response_cap_bytes(&self, budget_mb: u32) -> u64 {
        let share = budget_mb as u64 * 1024 * 1024 / self.response_share_of_budget;
        if share > 256 * 1024 {
            share
        } else {
            256 * 1024
        }
    }

    /// The largest block response one group may ask for when `groups`
    /// groups (the catch-up groups and the frontier) share the block cache:
    /// its share of the `budget_mb` scan budget, within
    /// [`Self::response_cap_bytes`]. Every group's run fetched ahead then
    /// fits at once, so no group's run is evicted unscanned by another's;
    /// with many groups, each asks for fewer blocks at a time (at least
    /// one) instead of the cache going over budget.
    pub const fn group_response_cap_bytes(&self, budget_mb: u32, groups: u64) -> u64 {
        let groups = if groups == 0 { 1 } else { groups };
        let share = budget_mb as u64 * 1024 * 1024 / groups;
        let cap = self.response_cap_bytes(budget_mb);
        if share < cap {
            share
        } else {
            cap
        }
    }

    /// The time a round needs when the smallest unit of a large block takes
    /// `unit_secs`: the base round while that fits the Blocks share, else
    /// half as much again as the unit, within [`Self::max_round_budget`].
    pub fn round_budget_for(&self, unit_secs: f64) -> Duration {
        if unit_secs > self.target_call_secs() {
            Duration::from_secs_f64((unit_secs * 1.5).min(self.max_round_budget.as_secs_f64()))
                .max(self.round_budget)
        } else {
            self.round_budget
        }
    }

    /// Whether the scanner can run this tuning: the relations the rest of
    /// the engine relies on. [`Self::DEFAULT`] is checked at build time.
    pub const fn validate(&self) -> Result<(), TuningError> {
        let slow_secs = shared::scaling::SLOW_BLOCK_SECS as u64;
        if self.shares.total() != 100 {
            Err(TuningError::SharesNotWholeRound)
        } else if self.round_budget.is_zero() {
            Err(TuningError::NoRound)
        } else if self.round_budget.as_nanos() > self.max_round_budget.as_nanos() {
            Err(TuningError::RoundOverMaximum)
        } else if self.max_round_budget.as_secs() > slow_secs
            || (self.max_round_budget.as_secs() == slow_secs
                && self.max_round_budget.subsec_nanos() > 0)
        {
            Err(TuningError::RaisedRoundSlow)
        } else if self.headers_per_fetch > crate::daemon_rpc::MAX_HEADERS_PER_REQUEST {
            Err(TuningError::HeadersOverOneRequest)
        } else if self.whole_block_max_secs.is_nan()
            || self.whole_block_max_secs >= slow_secs as f64
        {
            Err(TuningError::WholeBlockSlow)
        } else if self.group_page == 0
            || self.blocks_per_unit == 0
            || self.txs_per_scan == 0
            || self.scan_concurrency == 0
            || self.headers_per_fetch == 0
            || self.chunk_max_blocks == 0
            || self.response_share_of_budget == 0
        {
            Err(TuningError::ZeroCount)
        } else if self.chunk_ewma_alpha.is_nan()
            || self.chunk_ewma_alpha <= 0.0
            || self.chunk_ewma_alpha > 1.0
        {
            Err(TuningError::AlphaOutOfRange)
        } else {
            Ok(())
        }
    }
}

impl Default for ScanTuning {
    fn default() -> Self {
        ScanTuning::DEFAULT
    }
}

const _: () = match ScanTuning::DEFAULT.validate() {
    Ok(()) => (),
    Err(error) => panic!("{}", error.message()),
};

// Headroom the default keeps rather than a rule the scanner needs: a block
// request sized to the Blocks share keeps the 15 s minimum timeout, nearly
// four times what it should take. (A longer round, as the round length
// sweep runs, gets three times instead.)
const _: () = assert!(
    ScanTuning::DEFAULT.target_call_secs() * crate::link::SAFETY
        <= crate::link::MIN_TIMEOUT.as_secs_f64(),
    "a block request sized to the default Blocks share must keep the minimum timeout"
);

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// The default's derived times, as the docs and comments quote them.
    #[test]
    fn the_default_shares_out_a_ten_second_round() {
        let t = ScanTuning::DEFAULT;
        assert_eq!(t.reserved(Tier::Blocks), Duration::from_secs(4));
        assert_eq!(t.reserved(Tier::Mempool), Duration::from_millis(1_500));
        assert_eq!(t.reserved(Tier::Settlement), Duration::from_secs(2));
        assert_eq!(t.target_call_secs(), 4.0);
        assert_eq!(
            t.share_of(Tier::Blocks, Duration::from_secs(120)),
            Duration::from_secs(48)
        );
        for budget in [
            t.round_budget,
            Duration::from_millis(7_777),
            Duration::from_nanos(1_001),
        ] {
            let total: Duration = Tier::ALL.iter().map(|tier| t.share_of(*tier, budget)).sum();
            assert!(
                total <= budget && budget - total < Duration::from_nanos(Tier::ALL.len() as u64)
            );
        }
    }

    /// The response cap is an eighth of the budget and at least 256 kB;
    /// groups sharing the cache each get their share, never more than the
    /// cap, down to one block a request.
    #[test]
    fn response_caps_follow_the_budget_and_the_groups() {
        let t = ScanTuning::DEFAULT;
        let mb = 1024 * 1024;
        assert_eq!(t.response_cap_bytes(8), mb);
        assert_eq!(t.response_cap_bytes(1), 256 * 1024);
        assert_eq!(t.response_cap_bytes(4096), 512 * mb);
        assert_eq!(t.group_response_cap_bytes(8, 1), mb, "the cap binds first");
        assert_eq!(
            t.group_response_cap_bytes(8, 0),
            mb,
            "no groups counts as one"
        );
        assert_eq!(t.group_response_cap_bytes(8, 16), mb / 2);
        assert_eq!(t.group_response_cap_bytes(8, 1_000_000), 8);
    }

    /// A round keeps its base time unless one page of one transaction can't
    /// fit the Blocks share; then it gets half as much again as that page,
    /// never past the maximum.
    #[test]
    fn the_round_grows_only_for_a_page_that_cannot_fit_its_share() {
        let t = ScanTuning::DEFAULT;
        assert_eq!(t.round_budget_for(0.0), Duration::from_secs(10));
        assert_eq!(
            t.round_budget_for(4.0),
            Duration::from_secs(10),
            "the share is 4 s"
        );
        assert_eq!(
            t.round_budget_for(5.0),
            Duration::from_secs(10),
            "1.5 x 5 s is under the base"
        );
        assert_eq!(t.round_budget_for(20.0), Duration::from_secs(30));
        assert_eq!(t.round_budget_for(1_000.0), t.max_round_budget);
    }

    /// Every relation the scanner relies on is refused when broken, with a
    /// reason, and a tuning that keeps them all is accepted.
    #[test]
    fn a_tuning_that_breaks_a_relation_is_refused() {
        let d = ScanTuning::DEFAULT;
        let refused = |t: ScanTuning| t.validate().unwrap_err();
        let shares = TierShares {
            upkeep: 6,
            ..d.shares
        };
        assert_eq!(
            refused(ScanTuning { shares, ..d }),
            TuningError::SharesNotWholeRound
        );
        assert_eq!(
            refused(ScanTuning {
                round_budget: Duration::ZERO,
                ..d
            }),
            TuningError::NoRound
        );
        assert_eq!(
            refused(ScanTuning {
                round_budget: Duration::from_secs(200),
                ..d
            }),
            TuningError::RoundOverMaximum
        );
        assert_eq!(
            refused(ScanTuning {
                max_round_budget: Duration::from_millis(120_001),
                ..d
            }),
            TuningError::RaisedRoundSlow
        );
        assert_eq!(
            ScanTuning {
                round_budget: Duration::from_secs(20),
                ..d
            }
            .validate(),
            Ok(()),
            "a longer round only loses headroom, as the sweep runs it"
        );
        assert_eq!(
            refused(ScanTuning {
                headers_per_fetch: 501,
                ..d
            }),
            TuningError::HeadersOverOneRequest
        );
        assert_eq!(
            refused(ScanTuning {
                whole_block_max_secs: 120.0,
                ..d
            }),
            TuningError::WholeBlockSlow
        );
        assert_eq!(
            refused(ScanTuning {
                whole_block_max_secs: f64::NAN,
                ..d
            }),
            TuningError::WholeBlockSlow
        );
        for t in [
            ScanTuning { group_page: 0, ..d },
            ScanTuning {
                blocks_per_unit: 0,
                ..d
            },
            ScanTuning {
                txs_per_scan: 0,
                ..d
            },
            ScanTuning {
                scan_concurrency: 0,
                ..d
            },
            ScanTuning {
                headers_per_fetch: 0,
                ..d
            },
            ScanTuning {
                chunk_max_blocks: 0,
                ..d
            },
            ScanTuning {
                response_share_of_budget: 0,
                ..d
            },
        ] {
            assert_eq!(refused(t), TuningError::ZeroCount);
        }
        for alpha in [0.0, 1.5, f64::NAN] {
            assert_eq!(
                refused(ScanTuning {
                    chunk_ewma_alpha: alpha,
                    ..d
                }),
                TuningError::AlphaOutOfRange
            );
        }
        assert!(TuningError::ZeroCount.to_string().contains("at least one"));
        let five = ScanTuning {
            round_budget: Duration::from_secs(5),
            group_page: 4,
            ..d
        };
        assert_eq!(five.validate(), Ok(()));
        assert_eq!(five.reserved(Tier::Blocks), Duration::from_secs(2));
    }
}
