//! The engine's database worker (docs/scanner_microtasks.md, "Database
//! access"): one thread owns a connection and runs every job sent to it, so
//! SQLite work never blocks a Tokio worker thread.
//!
//! Callers are sorted into [`Class`]es, each with its own bounded queue,
//! served round-robin: a flood of one kind of work (a scanner catching up,
//! a webhook backlog) delays each other kind by at most one job per turn,
//! and a full queue makes its callers wait (backpressure) instead of
//! growing without bound.
//!
//! A job runs to completion once taken, even if its caller stopped waiting
//! (a timeout, a cancelled task): SQLite work can't be interrupted. Every job
//! is therefore a whole, idempotent step, usually one transaction.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{Result, SharedStore, Store, StoreError};

/// Who a job is for; each has its own queue and an equal turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// The chain scanner's work units.
    Scanner,
    /// Webhook delivery bookkeeping.
    Webhook,
    /// API and admin requests.
    Admin,
}

impl Class {
    const ALL: [Class; 3] = [Class::Scanner, Class::Webhook, Class::Admin];

    fn index(self) -> usize {
        self as usize
    }
}

/// Jobs queued per class before callers wait for room.
const QUEUE_CAPACITY: usize = 64;

type Job = Box<dyn FnOnce(&Store) + Send + 'static>;

/// What the worker has done, for status pages and the stress report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DbMetrics {
    pub completed: u64,
    /// Longest a job waited in its queue before running.
    pub max_queue_wait_us: u64,
    /// Longest a job took to run.
    pub max_run_us: u64,
}

#[derive(Default)]
struct Counters {
    completed: AtomicU64,
    max_queue_wait_us: AtomicU64,
    max_run_us: AtomicU64,
}

/// A handle to the database worker. Cheap to clone; the worker stops when
/// the last handle is dropped, after finishing the jobs already queued.
#[derive(Clone)]
pub struct Db {
    inner: Inner,
    counters: Arc<Counters>,
}

#[derive(Clone)]
enum Inner {
    /// A worker thread with its own connection.
    Worker { senders: Arc<[tokio::sync::mpsc::Sender<(Instant, Job)>; 3]>, wake: std::sync::mpsc::SyncSender<()> },
    /// Jobs run on the caller, on the store everything else shares.
    Inline(SharedStore),
}

impl Db {
    /// A worker with its own connection to the database file at `path`,
    /// sharing `store`'s order-change notifications (so a subscriber sees
    /// changes made through either).
    pub fn open(path: &str, store: &Store) -> Result<Self> {
        Self::start(store.connect_again(path)?)
    }

    /// Runs each job on the calling task, on `store`, locked for the job:
    /// the behaviour of the shared store itself, with no worker thread. For
    /// tests (a paused test clock would otherwise jump ahead while a task
    /// waits on another thread) and in-memory databases, which can't be
    /// opened twice. Production uses [`Db::open`].
    pub fn over_shared(store: SharedStore) -> Self {
        Db { inner: Inner::Inline(store), counters: Arc::new(Counters::default()) }
    }

    fn start(store: Store) -> Result<Self> {
        let mut senders = Vec::new();
        let mut receivers = Vec::new();
        for _ in Class::ALL {
            let (sender, receiver) = tokio::sync::mpsc::channel(QUEUE_CAPACITY);
            senders.push(sender);
            receivers.push(receiver);
        }
        // A token per submitted job, at most one waiting: the worker sleeps
        // on this when every queue is empty.
        let (wake, woken) = std::sync::mpsc::sync_channel::<()>(1);
        let counters = Arc::new(Counters::default());
        let worker_counters = counters.clone();
        std::thread::Builder::new()
            .name("scanner-db".into())
            .spawn(move || serve(store, receivers, woken, worker_counters))
            .map_err(|e| StoreError::WorkerUnavailable(e.to_string()))?;
        let senders: [tokio::sync::mpsc::Sender<(Instant, Job)>; 3] =
            senders.try_into().map_err(|_| StoreError::WorkerUnavailable("queue setup".into()))?;
        Ok(Db { inner: Inner::Worker { senders: Arc::new(senders), wake }, counters })
    }

    /// Runs `f` on the worker and returns its result. Waits for room in the
    /// class's queue if it is full. If the caller stops waiting, a job that
    /// has been queued still runs.
    pub async fn run<T, E>(&self, class: Class, f: impl FnOnce(&Store) -> std::result::Result<T, E> + Send + 'static) -> std::result::Result<T, E>
    where
        T: Send + 'static,
        E: From<StoreError> + Send + 'static,
    {
        let (senders, wake) = match &self.inner {
            Inner::Inline(store) => {
                let started = Instant::now();
                let result = f(&store.lock());
                record(&self.counters, started, started);
                return result;
            }
            Inner::Worker { senders, wake } => (senders, wake),
        };
        let (reply, answer) = tokio::sync::oneshot::channel();
        let job: Job = Box::new(move |store| {
            let _ = reply.send(f(store));
        });
        senders[class.index()]
            .send((Instant::now(), job))
            .await
            .map_err(|_| StoreError::WorkerUnavailable("the database worker stopped".into()))?;
        let _ = wake.try_send(());
        answer.await.map_err(|_| E::from(StoreError::WorkerUnavailable("the database worker dropped a job".into())))?
    }

    pub fn metrics(&self) -> DbMetrics {
        DbMetrics {
            completed: self.counters.completed.load(Ordering::Relaxed),
            max_queue_wait_us: self.counters.max_queue_wait_us.load(Ordering::Relaxed),
            max_run_us: self.counters.max_run_us.load(Ordering::Relaxed),
        }
    }
}

fn record(counters: &Counters, queued_at: Instant, started: Instant) {
    counters.max_queue_wait_us.fetch_max(started.duration_since(queued_at).as_micros() as u64, Ordering::Relaxed);
    counters.max_run_us.fetch_max(started.elapsed().as_micros() as u64, Ordering::Relaxed);
    counters.completed.fetch_add(1, Ordering::Relaxed);
}

fn serve(
    store: Store,
    mut receivers: Vec<tokio::sync::mpsc::Receiver<(Instant, Job)>>,
    woken: std::sync::mpsc::Receiver<()>,
    counters: Arc<Counters>,
) {
    let mut next = 0;
    loop {
        // Round-robin: the first non-empty queue after the last one served.
        let mut taken = None;
        let mut open = 0;
        for offset in 0..receivers.len() {
            let index = (next + offset) % receivers.len();
            match receivers[index].try_recv() {
                Ok(job) => {
                    taken = Some((index, job));
                    break;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => open += 1,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {}
            }
        }
        let Some((index, (queued_at, job))) = taken else {
            if open == 0 {
                return; // every handle dropped and every queue drained
            }
            // Nothing queued: sleep until a job arrives. The timeout covers a
            // wake token lost to the race between the check and the sleep.
            let _ = woken.recv_timeout(Duration::from_millis(50));
            continue;
        };
        next = (index + 1) % receivers.len();
        let started = Instant::now();
        // A panicking job loses its own reply (its caller gets an error), not
        // the worker.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&store)));
        record(&counters, queued_at, started);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_store() -> (Store, String) {
        let path = std::env::temp_dir().join(format!("scanner_db_{}.db", uuid::Uuid::new_v4()));
        let path = path.to_string_lossy().into_owned();
        (Store::open_file(&path).unwrap(), path)
    }

    fn cleanup(path: &str) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path}{suffix}"));
        }
    }

    /// A stalled database job occupies the worker thread, never a Tokio
    /// worker: timers and other tasks keep running on a one-thread runtime.
    #[tokio::test(flavor = "current_thread")]
    async fn a_stalled_job_never_blocks_the_async_runtime() {
        let (store, path) = file_store();
        let db = Db::open(&path, &store).unwrap();
        let stalled = db.clone();
        let job = tokio::spawn(async move {
            stalled
                .run(Class::Scanner, |_| -> Result<()> {
                    std::thread::sleep(Duration::from_millis(300));
                    Ok(())
                })
                .await
        });
        let started = Instant::now();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(started.elapsed() < Duration::from_millis(200), "the timer fired while the job was stalled");
        job.await.unwrap().unwrap();
        assert!(db.metrics().max_run_us >= 300_000);
        drop(db);
        cleanup(&path);
    }

    /// Classes take turns: with a deep scanner backlog queued, an admin job
    /// runs after at most one more scanner job, not after the whole backlog.
    #[tokio::test]
    async fn a_backlog_in_one_class_delays_another_by_one_job_at_most() {
        let (store, path) = file_store();
        let db = Db::open(&path, &store).unwrap();
        let order = Arc::new(parking_lot::Mutex::new(Vec::new()));
        // Hold the worker so the queues fill up behind it.
        let (release, hold) = std::sync::mpsc::channel::<()>();
        let blocker = {
            let db = db.clone();
            tokio::spawn(async move {
                db.run(Class::Scanner, move |_| -> Result<()> {
                    let _ = hold.recv();
                    Ok(())
                })
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut jobs = Vec::new();
        for i in 0..20 {
            let (db, order) = (db.clone(), order.clone());
            jobs.push(tokio::spawn(async move {
                db.run(Class::Scanner, move |_| -> Result<()> {
                    order.lock().push(format!("scanner{i}"));
                    Ok(())
                })
                .await
            }));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        let admin = {
            let (db, order) = (db.clone(), order.clone());
            tokio::spawn(async move {
                db.run(Class::Admin, move |_| -> Result<()> {
                    order.lock().push("admin".into());
                    Ok(())
                })
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        release.send(()).unwrap();
        blocker.await.unwrap().unwrap();
        admin.await.unwrap().unwrap();
        for job in jobs {
            job.await.unwrap().unwrap();
        }
        let order = order.lock();
        let admin_at = order.iter().position(|j| j == "admin").unwrap();
        assert!(admin_at <= 1, "admin ran at position {admin_at} of {order:?}");
        drop(db);
        cleanup(&path);
    }

    /// Writes through the worker's own connection reach the order-change
    /// subscribers of the store it was opened from, after commit.
    #[tokio::test]
    async fn changes_made_through_the_worker_reach_the_stores_subscribers() {
        let (store, path) = file_store();
        let tenant = store
            .create_tenant(
                crate::store::NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![0u8; 64],
                    primary_address: "4db".into(),
                    network: "mainnet".into(),
                    confirmations_required: Some(1),
                    order_expiry_seconds: None,
                },
                100,
            )
            .unwrap()
            .tenant;
        let index = store.allocate_minor_index(&tenant.id).unwrap();
        let order = store
            .create_order(crate::store::NewOrder {
                confirmations_required_override: None,
                tenant_id: tenant.id.clone(),
                merchant_order_id: None,
                minor_index: index,
                address: "addr".into(),
                xmr_amount_piconero: 10,
                description: None,
                created_at: 100,
                expires_at: 10_000,
            })
            .unwrap();
        let mut changes = store.subscribe_order_changes();
        let db = Db::open(&path, &store).unwrap();
        let order_id = order.id.clone();
        db.run(Class::Scanner, move |s| -> Result<()> {
            s.in_transaction(|s| -> Result<()> {
                s.record_payment_match(&order_id, "tx", 0, 10, "[]", 200, Some(5))?;
                s.recompute_order_status(&order_id, 10, 200)?;
                Ok(())
            })
        })
        .await
        .unwrap();
        let change = tokio::time::timeout(Duration::from_secs(2), changes.recv()).await.unwrap().unwrap();
        assert_eq!(change.order_id, order.id);
        drop(db);
        cleanup(&path);
    }

    /// A job whose caller gave up still runs; a panicking job fails only its
    /// own caller.
    #[tokio::test]
    async fn an_abandoned_job_still_runs_and_a_panic_fails_only_its_caller() {
        let (store, path) = file_store();
        let db = Db::open(&path, &store).unwrap();
        let abandoned = db.run(Class::Admin, |s| s.set_setting("abandoned", "ran"));
        drop(tokio::time::timeout(Duration::ZERO, abandoned).await);
        let panicked = db.run(Class::Admin, |_| -> Result<()> { panic!("a bug in one job") }).await;
        assert!(matches!(panicked, Err(StoreError::WorkerUnavailable(_))));
        db.run(Class::Admin, |s| s.set_setting("after", "ok")).await.unwrap();
        assert_eq!(store.get_setting("after").unwrap().as_deref(), Some("ok"), "the worker survived");
        assert_eq!(store.get_setting("abandoned").unwrap().as_deref(), Some("ran"), "queued before it, so it ran first");
        drop(db);
        cleanup(&path);
    }
}
