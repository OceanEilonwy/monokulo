//! The engine's database worker (`docs/scanner_microtasks.md`, "Database
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
    pub const ALL: [Self; 3] = [Self::Scanner, Self::Webhook, Self::Admin];

    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// Longest an inline job waits for the shared store's lock.
const INLINE_LOCK_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(30)
};

/// Faults a test injects into one worker; production starts with none.
#[derive(Clone, Copy, Default)]
struct Faults {
    /// The thread can't be started (asks for an impossible stack).
    fail_spawn: bool,
    /// The loop panics once, between jobs.
    panic_loop_once: bool,
    /// The loop ends at once, as if every handle had gone.
    exit_loop: bool,
}

/// Jobs queued per class before callers wait for room.
pub const QUEUE_CAPACITY: usize = 64;

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
    Worker {
        senders: Arc<[tokio::sync::mpsc::Sender<Job>; 3]>,
        wake: std::sync::mpsc::SyncSender<()>,
    },
    /// Jobs run on the caller, on the store everything else shares. With
    /// `yields`, the caller first yields to the executor, as it does when
    /// it sends a job to the worker.
    Inline { store: SharedStore, yields: bool },
}

impl Db {
    /// A worker with its own connection to the database file at `path`,
    /// sharing `store`'s order-change notifications (so a subscriber sees
    /// changes made through either).
    pub fn open(path: &str, store: &Store) -> Result<Self> {
        Self::start(store.connect_again(path)?, Faults::default())
    }

    /// Runs each job on the calling task, on `store`, locked for the job:
    /// the behaviour of the shared store itself, with no worker thread. For
    /// tests (a paused test clock would otherwise jump ahead while a task
    /// waits on another thread) and in-memory databases, which can't be
    /// opened twice. Production uses [`Db::open`].
    pub fn over_shared(store: SharedStore) -> Self {
        Self {
            inner: Inner::Inline {
                store,
                yields: false,
            },
            counters: Arc::new(Counters::default()),
        }
    }

    /// [`Db::over_shared`], with every job an await point, as it is with
    /// the worker: a test that drops a future part-way can stop it between
    /// two jobs, where a crash or a cancellation can stop the real thing.
    #[cfg(any(test, feature = "fuzzing"))]
    pub fn over_shared_yielding(store: SharedStore) -> Self {
        Self {
            inner: Inner::Inline {
                store,
                yields: true,
            },
            counters: Arc::new(Counters::default()),
        }
    }

    fn start(store: Store, faults: Faults) -> Result<Self> {
        let mut receivers = Vec::new();
        let senders: [tokio::sync::mpsc::Sender<Job>; 3] = std::array::from_fn(|_| {
            let (sender, receiver) = tokio::sync::mpsc::channel(QUEUE_CAPACITY);
            receivers.push(receiver);
            sender
        });
        // A token per submitted job, at most one waiting: the worker sleeps
        // on this when every queue is empty.
        let (wake, woken) = std::sync::mpsc::sync_channel::<()>(1);
        let counters = Arc::new(Counters::default());
        let mut builder = std::thread::Builder::new().name("engine-db".into());
        if faults.fail_spawn {
            builder = builder.stack_size(usize::MAX);
        }
        builder
            .spawn(move || {
                let mut receivers = receivers;
                let mut faults = faults;
                // Jobs catch their own panics; this only restarts the loop
                // itself if it ever panics, so the worker never silently dies
                // with its queues open.
                while let Err(panic) =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve(&store, &mut receivers, &woken, &mut faults)))
                {
                    let message = panic
                        .downcast_ref::<&str>()
                        .map(ToString::to_string)
                        .or_else(|| panic.downcast_ref::<String>().cloned());
                    tracing::error!(panic = ?message, "the database worker's loop panicked; restarting it");
                }
            })
            .map_err(|e| StoreError::WorkerUnavailable(e.to_string()))?;
        Ok(Self {
            inner: Inner::Worker {
                senders: Arc::new(senders),
                wake,
            },
            counters,
        })
    }

    /// Runs `f` on the worker and returns its result. Waits for room in the
    /// class's queue if it is full. If the caller stops waiting, a job that
    /// has been queued still runs.
    pub async fn run<T, E>(
        &self,
        class: Class,
        f: impl FnOnce(&Store) -> std::result::Result<T, E> + Send + 'static,
    ) -> std::result::Result<T, E>
    where
        T: Send + 'static,
        E: From<StoreError> + Send + 'static,
    {
        use Inner::Inline;
        let (senders, wake) = match &self.inner {
            Inline { store, yields } => {
                if *yields {
                    tokio::task::yield_now().await;
                }
                let guard = lock_inline(store)?;
                let started = Instant::now();
                let result = f(&guard);
                record(&self.counters, started, started);
                return result;
            }
            Inner::Worker { senders, wake } => (senders, wake),
        };
        let (reply, answer) = tokio::sync::oneshot::channel();
        let (counters, queued_at) = (Arc::clone(&self.counters), Instant::now());
        // The job records its own timing before it replies, so a caller that
        // has its answer also sees it in the metrics.
        let job: Job = Box::new(move |store| {
            let started = Instant::now();
            let result = f(store);
            record(&counters, queued_at, started);
            let _ = reply.send(result);
        });
        senders[class.index()].send(job).await.map_err(|e| {
            StoreError::WorkerUnavailable(format!("the database worker stopped: {e}"))
        })?;
        let _ = wake.try_send(());
        answer.await.map_err(|e| {
            E::from(StoreError::WorkerUnavailable(format!(
                "the database worker dropped a job: {e}"
            )))
        })?
    }

    /// How many jobs of `class` are queued and not yet taken by the worker
    /// (always 0 for an inline handle).
    pub fn queued(&self, class: Class) -> usize {
        match &self.inner {
            Inner::Worker { senders, wake: _ } => {
                let sender = &senders[class.index()];
                sender.max_capacity() - sender.capacity()
            }
            Inner::Inline {
                store: _,
                yields: _,
            } => 0,
        }
    }

    pub fn metrics(&self) -> DbMetrics {
        DbMetrics {
            completed: self.counters.completed.load(Ordering::Relaxed),
            max_queue_wait_us: self.counters.max_queue_wait_us.load(Ordering::Relaxed),
            max_run_us: self.counters.max_run_us.load(Ordering::Relaxed),
        }
    }
}

/// The shared store's lock, for an inline job. Bounded: a caller that
/// (wrongly) holds the lock while calling gets an error instead of a
/// deadlock.
fn lock_inline(store: &SharedStore) -> Result<parking_lot::MutexGuard<'_, Store>> {
    store.try_lock_for(INLINE_LOCK_WAIT).ok_or_else(|| {
        StoreError::WorkerUnavailable(format!(
            "the store stayed locked for {INLINE_LOCK_WAIT:?} (is the caller holding it?)"
        ))
    })
}

fn record(counters: &Counters, queued_at: Instant, started: Instant) {
    counters.max_queue_wait_us.fetch_max(
        started.duration_since(queued_at).as_micros() as u64,
        Ordering::Relaxed,
    );
    counters
        .max_run_us
        .fetch_max(started.elapsed().as_micros() as u64, Ordering::Relaxed);
    counters.completed.fetch_add(1, Ordering::Relaxed);
}

fn serve(
    store: &Store,
    receivers: &mut [tokio::sync::mpsc::Receiver<Job>],
    woken: &std::sync::mpsc::Receiver<()>,
    faults: &mut Faults,
) {
    let mut dispatch = super::dispatch::Dispatch::default();
    loop {
        if faults.exit_loop {
            return;
        }
        // Round-robin: the first non-empty queue after the last one served.
        let mut taken = None;
        let mut open = 0;
        for class in dispatch.order() {
            let index = class.index();
            match receivers[index].try_recv() {
                Ok(job) => {
                    taken = Some((index, job));
                    break;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => open += 1,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {}
            }
        }
        let Some((index, job)) = taken else {
            if open == 0 {
                return; // every handle dropped and every queue drained
            }
            // Nothing queued: sleep until a job arrives. The timeout covers a
            // wake token lost to the race between the check and the sleep.
            let _ = woken.recv_timeout(Duration::from_millis(50));
            continue;
        };
        dispatch.served(Class::ALL[index]);
        if std::mem::take(&mut faults.panic_loop_once) {
            // The job runs first, as a real bug's panic might not let it;
            // the test checks the worker carries on serving.
            job(store);
            #[expect(clippy::panic, reason = "a fault only a test injects")]
            {
                panic!("injected worker loop panic");
            }
        }
        // A panicking job loses its own reply (its caller gets an error), not
        // the worker.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(store)));
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// An inline handle whose store is already locked (by a caller that
    /// shouldn't be holding it) fails the job instead of deadlocking.
    #[tokio::test]
    #[expect(clippy::await_holding_lock, reason = "holding it is the point")]
    async fn an_inline_job_on_a_held_store_fails_instead_of_deadlocking() {
        let shared = Store::open_in_memory().unwrap().into_shared();
        let db = Db::over_shared(Arc::clone(&shared));
        let held = shared.lock();
        let result = db.run(Class::Admin, Store::count_tenants).await;
        assert!(
            matches!(result, Err(StoreError::WorkerUnavailable(_))),
            "{result:?}"
        );
        drop(held);
        assert_eq!(db.run(Class::Admin, Store::count_tenants).await.unwrap(), 0);
    }

    /// If the worker's own loop panics, it restarts and keeps serving.
    #[tokio::test]
    async fn the_worker_loop_restarts_after_a_panic() {
        let (store, path) = file_store();
        let faults = Faults {
            panic_loop_once: true,
            ..Faults::default()
        };
        let db = Db::start(store.connect_again(&path).unwrap(), faults).unwrap();
        db.run(Class::Admin, |s| s.set_setting("first", "1"))
            .await
            .unwrap();
        db.run(Class::Admin, |s| s.set_setting("second", "2"))
            .await
            .unwrap();
        assert_eq!(store.get_setting("second").unwrap().as_deref(), Some("2"));
        drop(db);
        cleanup(&path);
    }

    /// A worker thread that can't be started is an error at startup, not a
    /// handle whose jobs never run.
    #[test]
    fn a_worker_that_cannot_start_is_an_error() {
        let (store, path) = file_store();
        let faults = Faults {
            fail_spawn: true,
            ..Faults::default()
        };
        let result = Db::start(store.connect_again(&path).unwrap(), faults);
        assert!(matches!(result, Err(StoreError::WorkerUnavailable(_))));
        cleanup(&path);
    }

    /// A worker that has stopped fails each job at once, rather than leaving
    /// its caller waiting forever.
    #[tokio::test]
    async fn a_stopped_worker_fails_jobs_instead_of_hanging() {
        let (store, path) = file_store();
        let faults = Faults {
            exit_loop: true,
            ..Faults::default()
        };
        let db = Db::start(store.connect_again(&path).unwrap(), faults).unwrap();
        // The loop exits at once and its queues close.
        let Inner::Worker { senders, wake: _ } = &db.inner else {
            unreachable!()
        };
        while !senders[Class::Admin.index()].is_closed() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let result = db.run(Class::Admin, Store::count_tenants).await;
        assert!(
            matches!(&result, Err(StoreError::WorkerUnavailable(m)) if m.contains("stopped")),
            "{result:?}"
        );
        drop(db);
        cleanup(&path);
    }

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
        // Stalled until released, not for a fixed time: the test proves the
        // runtime ran meanwhile, not how fast it did.
        let (release, hold) = std::sync::mpsc::channel::<()>();
        let (running, started) = std::sync::mpsc::channel::<()>();
        let job = tokio::spawn(async move {
            stalled
                .run(Class::Scanner, move |_| -> Result<()> {
                    running.send(()).unwrap();
                    let _ = hold.recv();
                    Ok(())
                })
                .await
        });
        tokio::task::spawn_blocking(move || started.recv().unwrap())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !job.is_finished(),
            "the timer fired, on this one thread, while the job was stalled on the worker's"
        );
        release.send(()).unwrap();
        job.await.unwrap().unwrap();
        assert!(db.metrics().max_run_us > 0);
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
        // Each step waits for the queues to show the jobs, not for time to
        // pass: the blocker is taken by the worker (its queue empties), the
        // scanner backlog is queued behind it, then the admin job.
        let queued = |class: Class, count: usize| {
            let db = db.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(10), async {
                    while db.queued(class) != count {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap_or_else(|_| panic!("{count} jobs never queued for {class:?}"));
            }
        };
        queued(Class::Scanner, 0).await;
        let mut jobs = Vec::new();
        for i in 0..20 {
            let (db, order) = (db.clone(), Arc::clone(&order));
            jobs.push(tokio::spawn(async move {
                db.run(Class::Scanner, move |_| -> Result<()> {
                    order.lock().push(format!("scanner{i}"));
                    Ok(())
                })
                .await
            }));
        }
        queued(Class::Scanner, 20).await;
        let admin = {
            let (db, order) = (db.clone(), Arc::clone(&order));
            tokio::spawn(async move {
                db.run(Class::Admin, move |_| -> Result<()> {
                    order.lock().push("admin".into());
                    Ok(())
                })
                .await
            })
        };
        queued(Class::Admin, 1).await;
        release.send(()).unwrap();
        blocker.await.unwrap().unwrap();
        admin.await.unwrap().unwrap();
        for job in jobs {
            job.await.unwrap().unwrap();
        }
        let order = order.lock();
        let admin_at = order.iter().position(|j| j == "admin").unwrap();
        assert!(
            admin_at <= 1,
            "admin ran at position {admin_at} of {order:?}"
        );
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
                &crate::store::NewTenant {
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
            .create_order(&crate::store::NewOrder {
                idempotency_key: None,
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
                s.record_payment_match(&order_id, "tx", 0, 10, "[]", 200, Some(5), None)?;
                s.recompute_order_status(&order_id, 10, 200)?;
                Ok(())
            })
        })
        .await
        .unwrap();
        let change = tokio::time::timeout(Duration::from_secs(2), changes.recv())
            .await
            .unwrap()
            .unwrap();
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
        let panicked = db
            .run(Class::Admin, |_| -> Result<()> {
                panic!("a bug in one job")
            })
            .await;
        assert!(matches!(panicked, Err(StoreError::WorkerUnavailable(_))));
        db.run(Class::Admin, |s| s.set_setting("after", "ok"))
            .await
            .unwrap();
        assert_eq!(
            store.get_setting("after").unwrap().as_deref(),
            Some("ok"),
            "the worker survived"
        );
        assert_eq!(
            store.get_setting("abandoned").unwrap().as_deref(),
            Some("ran"),
            "queued before it, so it ran first"
        );
        drop(db);
        cleanup(&path);
    }
}

#[cfg(test)]
#[path = "../../tests/internal/store/queue_properties.rs"]
#[cfg_attr(coverage_nightly, coverage(off))]
mod properties;
