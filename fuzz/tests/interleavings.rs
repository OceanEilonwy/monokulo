//! Exhaustive serialized policy event orders, not a memory-race model.
//! Real executor overlap is verified by the engine's rendezvous scenarios.
use engine::work::{scheduler::Scheduler, ScanTuning, Tier, TierOutcome};
use std::time::Duration;

fn permutations<const N: usize>() -> Vec<[usize; N]> {
    fn visit<const N: usize>(order: &mut [usize; N], from: usize, out: &mut Vec<[usize; N]>) {
        if from == N {
            out.push(*order);
            return;
        }
        for at in from..N {
            order.swap(from, at);
            visit(order, from + 1, out);
            order.swap(from, at);
        }
    }
    let mut out = Vec::new();
    visit(&mut std::array::from_fn(|i| i), 0, &mut out);
    out
}

#[test]
fn duplicate_completions_are_applied_once_under_every_event_ordering() {
    let orders = permutations::<2>();
    assert_eq!(orders.len(), 2);
    for order in orders {
        let mut policy = Scheduler::new(&ScanTuning::DEFAULT, Duration::ZERO).unwrap();
        let effect = policy.request(Duration::ZERO).unwrap();
        let accepted = order
            .into_iter()
            .filter(|_| policy.complete(effect, Some(TierOutcome::Idle)))
            .count();
        assert_eq!(accepted, 1);
        assert_eq!(policy.steps()[Tier::Chain], 1);
    }
}

#[test]
fn an_old_completion_cannot_close_a_new_outstanding_effect() {
    for order in permutations::<2>() {
        let mut policy = Scheduler::new(&ScanTuning::DEFAULT, Duration::ZERO).unwrap();
        let old = policy.request(Duration::ZERO).unwrap();
        policy.complete(old, Some(TierOutcome::Idle));
        let current = policy.request(Duration::ZERO).unwrap();
        for event in order {
            if event == 0 {
                assert!(!policy.complete(old, Some(TierOutcome::Failed)));
            } else {
                assert!(policy.complete(current, Some(TierOutcome::Idle)));
            }
        }
        assert_eq!(policy.steps()[Tier::Blocks], 1);
        assert_eq!(policy.outcomes()[Tier::Blocks], TierOutcome::Idle);
    }
}

#[test]
fn reservation_cancellation_and_cache_reset_never_admit_two_owners() {
    use engine::exploration::Reservations;
    let orders = permutations::<3>();
    assert_eq!(orders.len(), 6);
    for order in orders {
        let tenant = engine::store::TenantId::new("tenant");
        let mut policy = Reservations::default();
        let mut old = Some(policy.claim("tx", &tenant, 1).unwrap());
        for event in order {
            match event {
                0 => policy.release(old.take().unwrap()),
                1 => policy.forget_completed(),
                _ => {
                    if let Some(lease) = policy.claim("tx", &tenant, 2) {
                        assert_eq!(policy.pending(), 1);
                        assert!(policy.claim("tx", &tenant, 3).is_none());
                        policy.complete(lease);
                    }
                }
            }
        }
        assert_eq!(policy.pending(), 0);
        if let Some(lease) = policy.claim("tx", &tenant, 2) {
            policy.complete(lease);
        }
        assert_eq!(policy.completed("tx", &tenant), Some(2));
        assert!(policy.claim("tx", &tenant, 2).is_none());
    }
}

#[test]
fn changed_window_after_old_completion_remains_due_in_every_event_order() {
    use engine::exploration::Reservations;
    for order in permutations::<2>() {
        let tenant = engine::store::TenantId::new("tenant");
        let mut policy = Reservations::default();
        let mut old = Some(policy.claim("tx", &tenant, 1).unwrap());
        for event in order {
            if event == 0 {
                policy.complete(old.take().unwrap());
            } else if let Some(lease) = policy.claim("tx", &tenant, 2) {
                policy.complete(lease);
            }
        }
        if let Some(lease) = policy.claim("tx", &tenant, 2) {
            policy.complete(lease);
        }
        assert_eq!(policy.completed("tx", &tenant), Some(2));
        assert_eq!(policy.pending(), 0);
    }
}
