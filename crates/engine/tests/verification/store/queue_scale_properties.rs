//! Repeated saturation uses the real worker, accepted jobs, and admission
//! barriers. Timings are observations; fairness is measured in served jobs.
use super::*;

async fn waves(count: usize) {
    let path = TempFile::new();
    let store = Store::create_file(&path.0).unwrap();
    let db = Db::open(&path.0, &store).unwrap();
    let trace = Arc::new(parking_lot::Mutex::new(Vec::new()));
    for wave in 0..count {
        let (mut release, blocker) = held(&db).await;
        let mut callers = Vec::new();
        for (c, class) in Class::ALL.into_iter().enumerate() {
            for index in 0..QUEUE_CAPACITY {
                let (job_db, trace) = (db.clone(), Arc::clone(&trace));
                let caller = tokio::spawn(async move {
                    job_db
                        .run(class, move |s| {
                            trace.lock().push((wave, c, index));
                            s.set_setting(&format!("saturation-{wave}-{c}-{index}"), "once")
                        })
                        .await
                });
                queued(&db, class, index + 1).await;
                if (index + wave) % 7 == 0 {
                    caller.abort();
                    let _ = caller.await;
                } else {
                    callers.push(caller);
                }
            }
            let db_wait = db.clone();
            let mut waiting = Box::pin(db_wait.run(class, move |s| {
                s.set_setting(&format!("unaccepted-wave-{wave}-{c}"), "bad")
            }));
            tokio::select! { biased; result=&mut waiting => panic!("full queue accepted a job: {result:?}"), ()=tokio::task::yield_now()=>{} }
            assert_eq!(db.queued(class), QUEUE_CAPACITY);
            drop(waiting);
        }
        release.release();
        blocker.await.unwrap().unwrap();
        for caller in callers {
            caller.await.unwrap().unwrap();
        }
        for class in Class::ALL {
            db.run(class, Store::count_tenants).await.unwrap();
        }
        assert!(Class::ALL.iter().all(|&c| db.queued(c) == 0));
    }
    let expected: Vec<_> = (0..count)
        .flat_map(|wave| {
            (0..QUEUE_CAPACITY).flat_map(move |i| (0..Class::COUNT).map(move |c| (wave, c, i)))
        })
        .collect();
    assert_eq!(*trace.lock(),expected,"every continuously queued class must get one turn per round of classes, with FIFO preserved");
    for (wave, c, i) in expected {
        assert_eq!(
            store
                .get_setting(&format!("saturation-{wave}-{c}-{i}"))
                .unwrap()
                .as_deref(),
            Some("once")
        );
    }
    for wave in 0..count {
        for c in 0..Class::COUNT {
            assert!(store
                .get_setting(&format!("unaccepted-wave-{wave}-{c}"))
                .unwrap()
                .is_none());
        }
    }
    println!(
        "ENGINE_SCALE_RESULT queue_waves={count} committed_jobs={}",
        count * Class::COUNT * QUEUE_CAPACITY
    );
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn repeated_saturation_retains_fifo_fairness_and_cancellation(wave_count in 1usize..9) {
        runtime().block_on(waves(wave_count));
    }
}
#[test]
#[ignore = "large deterministic scale package; `cargo xtask engine scale` runs it"]
fn twelve_thousand_accepted_jobs_survive_sustained_saturation() {
    runtime().block_on(waves(64));
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/store/queue_scale_properties.txt"
        ),
    )
}
