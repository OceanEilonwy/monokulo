//! Manual contention comparison, not a timing assertion. Uses the same cache
//! data and critical sections for each mutex, on two Tokio worker threads.
use super::*;

enum CacheLock {
    Blocking(parking_lot::Mutex<Remembered>),
    Yielding(tokio::sync::Mutex<Remembered>),
}

fn operation(
    cache: &mut Remembered,
    step: usize,
    tx: &Arc<Transaction>,
    size: usize,
) -> Vec<String> {
    match step % 3 {
        0 => {
            cache
                .bodies
                .remember("fetched", tx, size, MAX_BODIES, MAX_BODY_BYTES);
            Vec::new()
        }
        1 => {
            cache.bodies.retain(|_| true);
            cache.scanned.retain(|_, _| true);
            Vec::new()
        }
        _ => cache.scanned.keys().cloned().collect(),
    }
}

#[test]
#[ignore = "manual contention profile; latency depends on hardware and load"]
fn compare_blocking_and_yielding_cache_mutexes() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for entries in [256,20_000] {
            for yielding in [false,true] {
                let mut cache = Remembered::default();
                let tx = Arc::new(crate::scanner::tests::fixture_tx());
                let size = monero::consensus::encode::serialize(tx.as_ref()).len();
                for i in 0..entries {
                    let txid = format!("{i:064x}");
                    cache.scanned.insert(txid.clone(),HashMap::new());
                    cache.bodies.remember(&txid,&tx,size,MAX_BODIES,MAX_BODY_BYTES);
                }
                let lock = Arc::new(if yielding { CacheLock::Yielding(tokio::sync::Mutex::new(cache)) }
                    else { CacheLock::Blocking(parking_lot::Mutex::new(cache)) });
                let mut tasks = Vec::new();
                for task in 0..2 {
                    let (lock,tx) = (Arc::clone(&lock),Arc::clone(&tx));
                    tasks.push(tokio::spawn(async move {
                        let mut samples = Vec::new();
                        for step in 0..256 {
                            let requested = std::time::Instant::now();
                            let (mut ids,wait,hold) = match lock.as_ref() {
                                CacheLock::Blocking(lock) => {
                                    let mut guard = lock.lock();
                                    let acquired = std::time::Instant::now();
                                    let ids = operation(&mut guard,step+task,&tx,size);
                                    (ids,acquired.duration_since(requested),acquired.elapsed())
                                }
                                CacheLock::Yielding(lock) => {
                                    let mut guard = lock.lock().await;
                                    let acquired = std::time::Instant::now();
                                    let ids = operation(&mut guard,step+task,&tx,size);
                                    (ids,acquired.duration_since(requested),acquired.elapsed())
                                }
                            };
                            // Production sorts the snapshot after releasing its lock.
                            ids.sort_unstable();
                            std::hint::black_box(ids);
                            samples.push((wait.as_micros(),hold.as_micros()));
                            tokio::task::yield_now().await;
                        }
                        samples
                    }));
                }
                let mut samples = Vec::new();
                for task in tasks { samples.extend(task.await.unwrap()); }
                let mut waits:Vec<_> = samples.iter().map(|s| s.0).collect();
                let mut holds:Vec<_> = samples.iter().map(|s| s.1).collect();
                waits.sort_unstable(); holds.sort_unstable();
                eprintln!("cache entries={entries}, mutex={}, wait p50/p99/max={} / {} / {} us, hold p50/p99/max={} / {} / {} us",
                    if yielding {"tokio"} else {"parking_lot"},waits[256],waits[506],waits[511],holds[256],holds[506],holds[511]);
            }
        }
    });
}
