//! Loom explores effect event orderings against the actual pure policy. This
//! intentionally does not claim to instrument Tokio channels or SQLite.
use engine::work::{scheduler::Scheduler, ScanTuning, Tier, TierOutcome};
use loom::sync::{Arc, Mutex};
use loom::thread;
use std::time::Duration;

#[test]
fn duplicate_completions_are_applied_once_under_every_event_ordering() {
    loom::model(|| {
        let mut policy = Scheduler::new(&ScanTuning::DEFAULT, Duration::ZERO).unwrap();
        let effect = policy.request(Duration::ZERO).unwrap();
        let policy = Arc::new(Mutex::new(policy));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let policy = Arc::clone(&policy);
            threads.push(thread::spawn(move || {
                policy
                    .lock()
                    .unwrap()
                    .complete(effect, Some(TierOutcome::Idle))
            }));
        }
        let accepted = threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .filter(|&accepted| accepted)
            .count();
        assert_eq!(accepted, 1);
        assert_eq!(policy.lock().unwrap().steps()[Tier::Chain], 1);
    });
}

#[test]
fn an_old_completion_cannot_close_a_new_outstanding_effect() {
    loom::model(|| {
        let mut policy = Scheduler::new(&ScanTuning::DEFAULT, Duration::ZERO).unwrap();
        let old = policy.request(Duration::ZERO).unwrap();
        policy.complete(old, Some(TierOutcome::Idle));
        let current = policy.request(Duration::ZERO).unwrap();
        let policy = Arc::new(Mutex::new(policy));
        let a = Arc::clone(&policy);
        let stale = thread::spawn(move || {
            assert!(!a.lock().unwrap().complete(old, Some(TierOutcome::Failed)))
        });
        let b = Arc::clone(&policy);
        let completion = thread::spawn(move || {
            assert!(b.lock().unwrap().complete(current, Some(TierOutcome::Idle)))
        });
        stale.join().unwrap();
        completion.join().unwrap();
        let policy = policy.lock().unwrap();
        assert_eq!(policy.steps()[Tier::Blocks], 1);
        assert_eq!(policy.outcomes()[Tier::Blocks], TierOutcome::Idle);
    });
}

#[test]
fn reservation_cancellation_and_cache_reset_never_admit_two_owners() {
    use engine::exploration::Reservations;
    // Each lock-held policy call is one actual production critical section.
    // Loom explores ordering between those calls, not parking_lot internals.
    loom::model(|| {
        let tenant = engine::store::TenantId::new("tenant");
        let mut policy = Reservations::default();
        let lease = policy.claim("tx", &tenant, 1).unwrap();
        let policy = Arc::new(Mutex::new(policy));
        let owner = Arc::clone(&policy);
        let cancellation = thread::spawn(move || owner.lock().unwrap().release(lease));
        let reset = Arc::clone(&policy);
        let eviction = thread::spawn(move || reset.lock().unwrap().forget_completed());
        let contender = Arc::clone(&policy);
        let other = tenant.clone();
        let scanner = thread::spawn(move || {
            let mut p = contender.lock().unwrap();
            if let Some(lease) = p.claim("tx", &other, 2) {
                assert_eq!(p.pending(), 1);
                assert!(p.claim("tx", &other, 3).is_none());
                p.complete(lease);
            }
        });
        cancellation.join().unwrap();
        eviction.join().unwrap();
        scanner.join().unwrap();
        let mut p = policy.lock().unwrap();
        assert_eq!(p.pending(), 0);
        if let Some(lease) = p.claim("tx", &tenant, 2) {
            p.complete(lease);
        }
        assert_eq!(p.completed("tx", &tenant), Some(2));
        assert!(p.claim("tx", &tenant, 2).is_none());
    });
}

#[test]
fn changed_window_after_old_completion_remains_due_in_every_event_order() {
    use engine::exploration::Reservations;
    loom::model(|| {
        let tenant = engine::store::TenantId::new("tenant");
        let mut policy = Reservations::default();
        let old = policy.claim("tx", &tenant, 1).unwrap();
        let policy = Arc::new(Mutex::new(policy));
        let a = Arc::clone(&policy);
        let completion = thread::spawn(move || a.lock().unwrap().complete(old));
        let b = Arc::clone(&policy);
        let other = tenant.clone();
        let rescan = thread::spawn(move || {
            let mut p = b.lock().unwrap();
            if let Some(lease) = p.claim("tx", &other, 2) {
                p.complete(lease);
            }
        });
        completion.join().unwrap();
        rescan.join().unwrap();
        let mut p = policy.lock().unwrap();
        if let Some(lease) = p.claim("tx", &tenant, 2) {
            p.complete(lease);
        }
        assert_eq!(p.completed("tx", &tenant), Some(2));
        assert_eq!(p.pending(), 0);
    });
}
