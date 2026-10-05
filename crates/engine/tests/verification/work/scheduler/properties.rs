use super::*;
use crate::work::{retry::Retry, Wait};
use proptest::prelude::*;

fn outcome(code: u8) -> Option<TierOutcome> {
    match code % 4 {
        0 => None,
        1 => Some(TierOutcome::Idle),
        2 => Some(TierOutcome::Blocked(Wait::NodeFailed)),
        _ => Some(TierOutcome::Failed),
    }
}

// A reference interpreter of the documented two-pass contract. It retains the
// pre-extraction loop, never calls Scheduler, and supplies independently timed
// units. Comparing the full trace also protects runner behavior during refactors.
type Reference = (Vec<(Tier, u8, Duration)>, [u32; 5], [TierOutcome; 5]);

fn reference(
    budget: Duration,
    opening: Duration,
    shares: [u32; 5],
    script: &[(u16, u8)],
) -> Reference {
    let mut now = opening;
    let mut steps = [0; 5];
    let mut outcomes = [TierOutcome::Backlogged; 5];
    let mut trace = Vec::new();
    for pass in 1..=2 {
        for (i, tier) in Tier::ALL.into_iter().enumerate() {
            let until = if pass == 1 {
                now.saturating_add(Duration::from_nanos(
                    (budget.as_nanos() * u128::from(shares[i]) / 100) as u64,
                ))
                .min(budget)
            } else {
                budget
            };
            while outcomes[i] == TierOutcome::Backlogged && (steps[i] == 0 || now < until) {
                let (cost, result) = script.get(trace.len()).copied().unwrap_or((1, 1));
                trace.push((tier, pass, until));
                now = now.saturating_add(Duration::from_nanos(u64::from(cost)));
                steps[i] += 1;
                if let Some(result) = outcome(result) {
                    outcomes[i] = result;
                }
            }
        }
    }
    (trace, steps, outcomes)
}

proptest! {
    #![proptest_config(persisted_config(crate::property_support::config()))]
    #[test]
    fn scheduler_matches_the_two_pass_contract(
        budget in 0u64..100_000, opening in 0u64..150_000,
        script in prop::collection::vec((0u16..1000, any::<u8>()), 0..256),
        weights in prop::collection::vec(1u32..20, 5),
    ) {
        let mut shares = [0; 5];
        let total: u32 = weights.iter().sum();
        for i in 0..4 { shares[i] = weights[i] * 100 / total; }
        shares[4] = 100 - shares[..4].iter().sum::<u32>();
        let tuning = ScanTuning { shares: super::super::TierShares {
            chain: shares[0], blocks: shares[1], mempool: shares[2], settlement: shares[3], upkeep: shares[4],
        }, ..ScanTuning::DEFAULT };
        let budget = Duration::from_nanos(budget);
        let opening = Duration::from_nanos(opening);
        let (expected, steps, outcomes) = reference(budget, opening, shares, &script);
        let mut scheduler = Scheduler::new(&tuning, budget).unwrap();
        let mut now = opening;
        let mut trace = Vec::new();
        while let Some(effect) = scheduler.request(now) {
            let (cost, result) = script.get(trace.len()).copied().unwrap_or((1, 1));
            trace.push((effect.tier, effect.pass, effect.until));
            now = now.saturating_add(Duration::from_nanos(u64::from(cost)));
            prop_assert!(scheduler.complete(effect, outcome(result)));
        }
        prop_assert_eq!(trace, expected);
        for (i, tier) in Tier::ALL.into_iter().enumerate() {
            prop_assert_eq!(scheduler.steps()[tier], steps[i]);
            prop_assert_eq!(scheduler.outcomes()[tier], outcomes[i]);
            prop_assert!(steps[i] >= 1);
        }
        prop_assert!(scheduler.request(Duration::ZERO).is_none());
    }

    #[test]
    fn outstanding_and_duplicate_completions_cannot_change_progress(script in prop::collection::vec(any::<u8>(), 1..100)) {
        let mut scheduler = Scheduler::new(&ScanTuning::DEFAULT, Duration::ZERO).unwrap();
        let mut previous = None;
        for code in script {
            let Some(effect) = scheduler.request(Duration::ZERO) else { break };
            let before = scheduler.steps();
            prop_assert!(scheduler.request(Duration::ZERO).is_none());
            if let Some(old) = previous { prop_assert!(!scheduler.complete(old, Some(TierOutcome::Idle))); }
            prop_assert_eq!(scheduler.steps(), before);
            prop_assert!(scheduler.complete(effect, outcome(code)));
            let after = scheduler.steps();
            prop_assert!(!scheduler.complete(effect, None));
            prop_assert_eq!(scheduler.steps(), after);
            previous = Some(effect);
        }
    }

    #[test]
    fn retry_boundaries_saturate_and_never_wrap(count in any::<u32>(), secs in any::<u64>()) {
        let now = Duration::from_secs(secs);
        let previous = Retry { failures: count, last_failure: now, retry_at: now };
        let next = Retry::failed(Some(previous), now);
        let expected_count = count.saturating_add(1);
        let expected_delay = if expected_count <= 2 { 0 }
            else { (2u64.pow(expected_count.saturating_sub(2).min(6))).min(60) };
        prop_assert_eq!(next.failures, expected_count);
        prop_assert_eq!(next.retry_at, now.saturating_add(Duration::from_secs(expected_delay)));
        prop_assert!(Retry::delay(count) <= Duration::from_secs(60));
        prop_assert!(!next.waiting(next.retry_at));
        prop_assert!(!next.forgotten(now));
    }

    #[test]
    fn retry_histories_reset_and_expire_independently(events in prop::collection::vec((0usize..8, 0u8..4, 0u16..4000), 1..256)) {
        crate::property_support::runtime().block_on(retry_history(&events));
    }
}

async fn retry_history(events: &[(usize, u8, u16)]) {
    tokio::time::pause();
    let retries = crate::work::Backoff::<usize>::default();
    let mut counts = [0u32; 8];
    let mut last = [Duration::ZERO; 8];
    let mut now = Duration::ZERO;
    for &(key, action, elapsed) in events {
        let elapsed = Duration::from_secs(u64::from(elapsed));
        now += elapsed;
        tokio::time::advance(elapsed).await;
        match action {
            0 => {
                retries.succeeded(&key);
                counts[key] = 0;
            }
            1 => {
                counts[key] += 1;
                last[key] = now;
                retries.failed(&key);
            }
            _ => {
                retries.waiting(); // Production upkeep owns expiry of the SUT.
                for i in 0..8 {
                    if now.saturating_sub(last[i]) >= Duration::from_secs(3600) {
                        counts[i] = 0;
                    }
                }
            }
        }
        let actual = retries.failures.lock();
        for (key, &expected) in counts.iter().enumerate() {
            assert_eq!(
                actual.get(&key).map_or(0, |retry| retry.failures),
                expected,
                "BOUNDARY: retry-expiry; key={key} now={now:?} action={action}"
            );
        }
    }
}

#[test]
fn retry_expiry_boundaries_use_production_upkeep() {
    for elapsed in [0, 1, 3599, 3600, 3601, 3999] {
        crate::property_support::runtime().block_on(retry_history(&[
            (0, 1, 0),
            (1, 1, 1),
            (0, 2, elapsed),
            (1, 0, 0),
            (0, 1, 0),
            (2, 1, 0),
            (0, 0, 0),
            (2, 2, 3600),
        ]));
    }
}

#[test]
fn every_combination_of_tier_results_preserves_the_progress_floor() {
    for encoded in 0..1024u32 {
        for opening in [0, 1, 100] {
            let mut scheduler = Scheduler::new(&ScanTuning::DEFAULT, Duration::ZERO).unwrap();
            let mut count = 0;
            while let Some(effect) = scheduler.request(Duration::from_secs(opening)) {
                assert_eq!(effect.tier, Tier::ALL[count]);
                assert_eq!(effect.pass, 1);
                assert!(scheduler.complete(effect, outcome(((encoded >> (2 * count)) & 3) as u8)));
                count += 1;
            }
            assert_eq!(count, 5);
        }
    }
}

#[test]
fn retry_count_at_u32_max_stays_capped() {
    let now = Duration::from_secs(42);
    let next = Retry::failed(
        Some(Retry {
            failures: u32::MAX,
            last_failure: now,
            retry_at: now,
        }),
        now,
    );
    assert_eq!(next.failures, u32::MAX);
    assert_eq!(next.retry_at, Duration::from_secs(102));
}

proptest! {
    #![proptest_config(persisted_config(crate::property_support::config()))]
    #[test]
    fn late_completions_from_another_round_are_rejected(generation in any::<u64>()) {
        let mut old = Scheduler::with_generation(&ScanTuning::DEFAULT,Duration::ZERO,generation).unwrap();
        let stale = old.request(Duration::ZERO).unwrap();
        let mut current = Scheduler::with_generation(&ScanTuning::DEFAULT,Duration::ZERO,generation.wrapping_add(1)).unwrap();
        let effect = current.request(Duration::ZERO).unwrap();
        prop_assert!(!current.complete(stale,Some(TierOutcome::Failed)), "BOUNDARY: stale-round-completion");
        prop_assert!(current.complete(effect,Some(TierOutcome::Idle)));
        prop_assert_eq!(current.steps()[Tier::Chain],1);
        prop_assert_eq!(current.outcomes()[Tier::Chain],TierOutcome::Idle);
    }
}

proptest! {
    #![proptest_config(persisted_config(crate::property_support::config()))]
    #[test]
    fn full_width_round_budgets_preserve_share_deadlines(
        seconds in any::<u64>(), nanos in 0u32..1_000_000_000,
        opening_seconds in any::<u64>(), opening_nanos in 0u32..1_000_000_000,
    ) {
        let budget = Duration::new(seconds,nanos);
        let opening = Duration::new(opening_seconds,opening_nanos);
        let tuning = ScanTuning::DEFAULT;
        let mut machine = Scheduler::new(&tuning,budget).unwrap();
        for tier in Tier::ALL {
            let effect = machine.request(opening).unwrap();
            let expected_ns = (opening.as_nanos()+budget.as_nanos()*u128::from(tuning.shares.percent(tier))/100).min(budget.as_nanos());
            let expected = Duration::new((expected_ns/1_000_000_000) as u64,(expected_ns%1_000_000_000) as u32);
            prop_assert_eq!(effect.tier(),tier);
            prop_assert_eq!(effect.until(),expected);
            prop_assert!(machine.complete(effect,Some(TierOutcome::Idle)));
        }
        prop_assert!(machine.request(Duration::MAX).is_none());
    }
}

#[test]
fn backwards_clock_observations_cannot_extend_an_expired_share() {
    let mut machine = Scheduler::new(&ScanTuning::DEFAULT, Duration::from_secs(100)).unwrap();
    let first = machine.request(Duration::ZERO).unwrap();
    assert!(machine.complete(first, None));
    let second = machine.request(Duration::from_secs(100)).unwrap();
    assert_eq!(second.tier(), Tier::Blocks);
    assert!(machine.complete(second, Some(TierOutcome::Idle)));
    while let Some(effect) = machine.request(Duration::ZERO) {
        assert_ne!(
            effect.tier(),
            Tier::Chain,
            "backwards observation reopened an expired tier"
        );
        assert!(machine.complete(effect, Some(TierOutcome::Idle)));
    }
    assert_eq!(machine.steps()[Tier::Chain], 1);
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/work/scheduler_properties.txt"
        ),
    )
}
