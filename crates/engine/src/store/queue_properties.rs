use super::*;
use crate::property_support::{config, runtime, TempFile};
use crate::store::dispatch::Dispatch;
use proptest::prelude::*;

struct Release(Option<std::sync::mpsc::Sender<()>>);
impl Release {
    fn release(&mut self) {
        if let Some(s) = self.0.take() {
            let _ = s.send(());
        }
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        self.release();
    }
}

async fn queued(db: &Db, class: Class, count: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while db.queued(class) != count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn held(db: &Db) -> (Release, tokio::task::JoinHandle<Result<()>>) {
    let (release, hold) = std::sync::mpsc::channel();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let db = db.clone();
    let task = tokio::spawn(async move {
        db.run(Class::Admin, move |_| -> Result<()> {
            let _ = entered.send(());
            let _ = hold.recv();
            Ok(())
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), ready)
        .await
        .unwrap()
        .unwrap();
    (Release(Some(release)), task)
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn class_selection_matches_independent_round_robin_model(masks in prop::collection::vec(0u8..8, 1..512)) {
        let mut policy = Dispatch::default();
        let mut previous = 2usize;
        for mask in masks {
            let expected = (1..=3).map(|offset| (previous + offset) % 3).find(|&i| mask & (1 << i) != 0);
            let actual = policy.order().into_iter().find(|c| mask & (1 << c.index()) != 0);
            prop_assert_eq!(actual.map(Class::index), expected);
            if let Some(class) = actual { policy.served(class); previous = class.index(); }
        }
    }

    #[test]
    fn bounded_queues_preserve_fifo_fairness_and_accepted_work(
        lengths in prop::array::uniform3(1usize..=66), cancelled in prop::collection::vec(any::<bool>(), 3*66),
        panic_class in 0usize..4,
    ) {
        runtime().block_on(async {
            let path = TempFile::new();
            let store = Store::open_file(&path.0).unwrap();
            let db = Db::open(&path.0, &store).unwrap();
            let (mut release, blocker) = held(&db).await;
            let trace = Arc::new(parking_lot::Mutex::new(Vec::new()));
            let mut jobs = Vec::new();
            // Enqueue deterministically, without relying on sleeps or which
            // spawning task the OS happens to poll first.
            for (class_index, class) in Class::ALL.into_iter().enumerate() {
                for i in 0..lengths[class_index].min(QUEUE_CAPACITY) {
                    let (job_db, trace) = (db.clone(), Arc::clone(&trace));
                    let task = tokio::spawn(async move {
                        job_db.run(class, move |s| -> Result<()> {
                            trace.lock().push((class_index, i));
                            s.set_setting(&format!("queue-{class_index}-{i}"), "committed")?;
                            assert!(!(panic_class == class_index && i == 0), "injected job failure after commit");
                            Ok(())
                        }).await
                    });
                    queued(&db, class, i+1).await;
                    if cancelled[class_index*66+i] { task.abort(); let _ = task.await; }
                    else { jobs.push((task, panic_class == class_index && i == 0)); }
                }
                if lengths[class_index] > QUEUE_CAPACITY {
                    for extra in QUEUE_CAPACITY..lengths[class_index] {
                        let job_db = db.clone();
                        let (entered, ready) = tokio::sync::oneshot::channel();
                        let waiting = tokio::spawn(async move {
                            let _ = entered.send(());
                            job_db.run(class, move |s| s.set_setting(&format!("unaccepted-{class_index}-{extra}"), "bad")).await
                        });
                        ready.await.unwrap();
                        tokio::task::yield_now().await;
                        assert_eq!(db.queued(class), QUEUE_CAPACITY);
                        waiting.abort(); let _ = waiting.await;
                    }
                }
            }
            release.release(); blocker.await.unwrap().unwrap();
            for (job, panicked) in jobs {
                let result = tokio::time::timeout(Duration::from_secs(10), job).await.unwrap().unwrap();
                assert_eq!(result.is_err(), panicked);
            }
            // A barrier in every class proves even abandoned trailing jobs ran.
            for class in Class::ALL { db.run(class, Store::count_tenants).await.unwrap(); }
            let actual = trace.lock().clone();
            let mut expected = Vec::new();
            for i in 0..QUEUE_CAPACITY {
                for (class, &len) in lengths.iter().enumerate() {
                    if i < len { expected.push((class, i)); }
                }
            }
            assert_eq!(actual, expected);
            for (class, i) in expected {
                assert_eq!(store.get_setting(&format!("queue-{class}-{i}")).unwrap().as_deref(), Some("committed"));
            }
            for (class, &len) in lengths.iter().enumerate() { for i in QUEUE_CAPACITY..len {
                assert!(store.get_setting(&format!("unaccepted-{class}-{i}")).unwrap().is_none());
            } }
        });
    }

    #[test]
    fn closing_all_senders_drains_accepted_jobs(lengths in prop::array::uniform3(1usize..=64)) {
        runtime().block_on(async {
            let path = TempFile::new();
            let store = Store::open_file(&path.0).unwrap();
            let db = Db::open(&path.0, &store).unwrap();
            let (mut release, blocker) = held(&db).await;
            let total: usize = lengths.iter().sum();
            let (finished, mut completions) = tokio::sync::mpsc::unbounded_channel();
            for (c, class) in Class::ALL.into_iter().enumerate() {
                for i in 0..lengths[c] {
                    let (job_db, finished) = (db.clone(), finished.clone());
                    let task = tokio::spawn(async move {
                        job_db.run(class, move |s| -> Result<()> {
                            s.set_setting(&format!("drain-{c}-{i}"), "yes")?;
                            finished.send((c,i)).unwrap();
                            Ok(())
                        }).await
                    });
                    queued(&db, class, i+1).await;
                    task.abort(); let _ = task.await;
                }
            }
            blocker.abort(); let _ = blocker.await;
            drop(db); drop(finished);
            release.release();
            let mut seen = std::collections::BTreeSet::new();
            for _ in 0..total {
                seen.insert(tokio::time::timeout(Duration::from_secs(10), completions.recv()).await.unwrap().unwrap());
            }
            assert_eq!(seen.len(), total);
            for (c,i) in seen { assert_eq!(store.get_setting(&format!("drain-{c}-{i}")).unwrap().as_deref(), Some("yes")); }
        });
    }
}

#[test]
fn continuously_ready_classes_are_served_within_three_turns() {
    // Exhaust every readiness history of length six (8^6). Each continuously
    // ready class gets a turn within three accepted jobs, whatever peers do.
    for encoded in 0..(1u32 << 18) {
        for protected in 0..3 {
            let mut policy = Dispatch::default();
            let mut age = 0;
            for step in 0..6 {
                let mask = ((encoded >> (step * 3)) & 7) | (1 << protected);
                let class = policy
                    .order()
                    .into_iter()
                    .find(|c| mask & (1 << c.index()) != 0)
                    .unwrap();
                policy.served(class);
                age = if class.index() == protected {
                    0
                } else {
                    age + 1
                };
                assert!(age < 3);
            }
        }
    }
}
