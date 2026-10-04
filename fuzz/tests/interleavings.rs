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
