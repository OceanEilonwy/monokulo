//! SQLite access for both services: how connections are configured, and
//! [`Pool`], a set of threads that each own one connection and run the jobs
//! sent to them.
//!
//! A `rusqlite::Connection` serves one caller at a time, so sharing one
//! connection means sharing a lock. What SQLite allows (in WAL mode) is many
//! connections: any number of readers alongside one writer. So each service
//! opens a pool of read-only connections and one writing connection (a pool
//! of one), and callers send closures to them. Running every query on a
//! pool thread also keeps SQLite work, and any disk stall, off the Tokio
//! worker threads.
//!
//! Every thread of a pool takes jobs from one shared queue: a job waits only
//! until *some* connection is free, never behind a slow job on a particular
//! one. The queue is bounded, so a flood of requests makes callers wait
//! (backpressure) instead of growing without bound.
//!
//! A job runs to completion once taken, even if its caller stopped waiting:
//! SQLite work can't be interrupted. A job that panics loses its own answer
//! (its caller gets [`PoolError::Panicked`]), not the thread.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;

/// How long a connection waits for another connection's lock before
/// reporting the database busy. Transactions are short, so this is only
/// reached if something is badly wrong.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Prepared statements kept per connection (SQLite's default, 16, is fewer
/// than either service's hot queries).
pub const STATEMENT_CACHE: usize = 128;

/// The read connections a service opens unless configured otherwise
/// (`database.read_connections`). Readers don't block each other, so more
/// only helps up to the number of CPU cores, and each keeps its own page
/// cache; a small instance gains nothing past a handful.
pub const DEFAULT_READ_CONNECTIONS: usize = 4;

/// Settings for the writing connection, applied every time it is opened
/// (only `journal_mode` is kept in the file; `foreign_keys` defaults to off
/// on every new connection).
///
/// WAL lets readers run while the writer commits. `synchronous = NORMAL`
/// is durable against a process crash under WAL; only an OS crash or power
/// loss can lose the last few commits.
///
/// Must run before migrations: `PRAGMA foreign_keys` does nothing inside a
/// transaction, and each migration runs in one.
pub fn configure_writer(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA journal_size_limit = 67108864;",
    )?;
    tune(conn)
}

/// Opens a read-only connection to the database file at `path`. It can't
/// write even by mistake: the file is opened read-only and `query_only` is
/// on.
pub fn open_reader(path: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA query_only = ON;")?;
    tune(&conn)?;
    Ok(conn)
}

/// Settings every connection gets, writer and readers alike.
pub fn tune(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
    Ok(())
}

/// Runs `f` with `query_only` on, so a write inside it fails as it would on
/// a pool's read-only connection. For [`Pool::Inline`] reads in tests, where
/// the one connection could otherwise write, and a read that (wrongly)
/// writes would pass its tests and fail in production.
pub fn read_only<T>(conn: &Connection, f: impl FnOnce() -> T) -> T {
    struct Restore<'a>(&'a Connection, bool);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            let _ = self.0.pragma_update(None, "query_only", self.1);
        }
    }
    let was: bool = conn
        .pragma_query_value(None, "query_only", |row| row.get(0))
        .unwrap_or(false);
    let _ = conn.pragma_update(None, "query_only", true);
    let _restore = Restore(conn, was);
    f()
}

/// An unsigned value (a height, count, index, limit or amount) crossing
/// into or out of SQLite, which stores only signed 64-bit integers. Both
/// directions are checked: a value that doesn't fit, or a negative one read
/// back, is an error rather than a silent wrap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unsigned<T>(pub T);

impl<T: Copy + TryInto<i64>> rusqlite::ToSql for Unsigned<T>
where
    <T as TryInto<i64>>::Error: std::error::Error + Send + Sync + 'static,
{
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        let value: i64 = self
            .0
            .try_into()
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        Ok(value.into())
    }
}

impl<T: TryFrom<i64>> rusqlite::types::FromSql for Unsigned<T> {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let raw = value.as_i64()?;
        T::try_from(raw)
            .map(Unsigned)
            .map_err(|_| rusqlite::types::FromSqlError::OutOfRange(raw))
    }
}

/// Why a job didn't run, or didn't answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PoolError {
    #[error("the database {0} threads stopped")]
    Stopped(&'static str),
    #[error("a database job panicked")]
    Panicked,
    #[error("the shared test connection stayed locked (is the caller holding it?)")]
    Locked,
    #[error("the database threads could not start: {0}")]
    Unavailable(String),
}

/// Jobs queued before callers wait for room.
const QUEUE_CAPACITY: usize = 256;

/// Longest an inline job waits for the shared connection's lock: a caller
/// that (wrongly) holds it gets an error instead of a deadlock.
const INLINE_LOCK_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(30)
};

type Job<T> = Box<dyn FnOnce(&T) + Send + 'static>;

/// Threads that each own a `T` (a connection, or a type wrapping one) and
/// run the jobs sent to them. Cheap to clone; the threads stop once every
/// handle is dropped and the queue is empty.
pub enum Pool<T> {
    Threads {
        name: &'static str,
        jobs: tokio::sync::mpsc::Sender<Job<T>>,
    },
    /// Jobs run on the caller, on one shared `T`, locked for the job. For
    /// in-memory databases, which can't be opened twice, and tests (a paused
    /// test clock would jump ahead while a task waits on another thread).
    Inline(Arc<parking_lot::Mutex<T>>),
}

impl<T> Clone for Pool<T> {
    fn clone(&self) -> Self {
        match self {
            Pool::Threads { name, jobs } => Pool::Threads {
                name,
                jobs: jobs.clone(),
            },
            Pool::Inline(shared) => Pool::Inline(shared.clone()),
        }
    }
}

impl<T: Send + 'static> Pool<T> {
    /// Starts one thread per value in `values`, named `{name}-{n}`.
    pub fn start(name: &'static str, values: Vec<T>) -> std::io::Result<Self> {
        let (jobs, receiver) = tokio::sync::mpsc::channel::<Job<T>>(QUEUE_CAPACITY);
        let receiver = Arc::new(tokio::sync::Mutex::new(receiver));
        for (n, value) in values.into_iter().enumerate() {
            let receiver = receiver.clone();
            std::thread::Builder::new()
                .name(format!("{name}-{n}"))
                .spawn(move || serve(&value, &receiver))?;
        }
        Ok(Pool::Threads { name, jobs })
    }

    /// Runs `f` on the pool and returns its result, waiting for room in the
    /// queue if it is full.
    pub async fn run<R, E>(
        &self,
        f: impl FnOnce(&T) -> Result<R, E> + Send + 'static,
    ) -> Result<R, E>
    where
        R: Send + 'static,
        E: From<PoolError> + Send + 'static,
    {
        let (name, jobs) = match self {
            Pool::Inline(shared) => {
                let guard = shared
                    .try_lock_for(INLINE_LOCK_WAIT)
                    .ok_or(PoolError::Locked)?;
                return f(&guard);
            }
            Pool::Threads { name, jobs } => (*name, jobs),
        };
        let (reply, answer) = tokio::sync::oneshot::channel();
        let job: Job<T> = Box::new(move |value| {
            let _ = reply.send(f(value));
        });
        jobs.send(job).await.map_err(|_| PoolError::Stopped(name))?;
        // The reply is dropped unsent only if the job panicked.
        answer.await.map_err(|_| PoolError::Panicked)?
    }
}

fn serve<T>(value: &T, receiver: &tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Job<T>>>) {
    loop {
        // One idle thread waits on the queue while holding its lock; the
        // rest wait for the lock, so each job goes to the next free thread.
        let job = receiver.blocking_lock().blocking_recv();
        let Some(job) = job else {
            return; // every handle dropped and the queue drained
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(value)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_db() -> String {
        std::env::temp_dir()
            .join(format!("shared_sqlite_{}.db", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .into_owned()
    }

    fn writer(path: &str) -> Connection {
        let conn = Connection::open(path).unwrap();
        configure_writer(&conn).unwrap();
        conn.execute_batch("CREATE TABLE t (v INTEGER NOT NULL);")
            .unwrap();
        conn
    }

    #[tokio::test]
    async fn readers_see_the_writers_commits_and_cannot_write() {
        let path = temp_db();
        let writes = Pool::start("test-write", vec![writer(&path)]).unwrap();
        let reads = Pool::start(
            "test-read",
            (0..2).map(|_| open_reader(&path).unwrap()).collect(),
        )
        .unwrap();
        writes
            .run(|c| {
                c.execute("INSERT INTO t VALUES (7)", [])
                    .map_err(Error::from)
            })
            .await
            .unwrap();
        let v: i64 = reads
            .run(|c| {
                c.query_row("SELECT v FROM t", [], |r| r.get(0))
                    .map_err(Error::from)
            })
            .await
            .unwrap();
        assert_eq!(v, 7);
        let refused = reads
            .run(|c| {
                c.execute("INSERT INTO t VALUES (8)", [])
                    .map_err(Error::from)
            })
            .await;
        assert!(matches!(refused, Err(Error::Sqlite(_))), "{refused:?}");
    }

    /// A slow job holds one connection; the next job goes to another one
    /// instead of queueing behind it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_slow_job_does_not_hold_up_the_next() {
        let path = temp_db();
        drop(writer(&path));
        let reads = Pool::start(
            "test-read",
            (0..2).map(|_| open_reader(&path).unwrap()).collect(),
        )
        .unwrap();
        // The slow job holds its connection until the fast one has run: if
        // the pool ran them one after the other, the fast one would never
        // start and the timeout below would fire. Nothing is timed.
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let slow = {
            let reads = reads.clone();
            tokio::spawn(async move {
                reads
                    .run(move |_| {
                        let _ = started_tx.send(());
                        let _ = release_rx.recv_timeout(Duration::from_secs(30));
                        Ok::<_, Error>(())
                    })
                    .await
            })
        };
        tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), reads.run(|_| Ok::<_, Error>(())))
            .await
            .expect("the fast job ran while the slow one held its connection")
            .unwrap();
        release_tx.send(()).unwrap();
        slow.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_panicking_job_fails_alone_and_the_thread_carries_on() {
        let path = temp_db();
        let reads = Pool::start("test-read", vec![writer(&path)]).unwrap();
        let result = reads
            .run(|_| -> Result<(), Error> { panic!("injected") })
            .await;
        assert_eq!(result, Err(Error::Pool(PoolError::Panicked)));
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = ran.clone();
        reads
            .run(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Error>(())
            })
            .await
            .unwrap();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_pool_whose_threads_stopped_says_so() {
        let reads: Pool<()> = Pool::start("test-read", vec![]).unwrap();
        // No threads: the receiver is dropped, so the queue is closed.
        let result = reads.run(|_| Ok::<_, Error>(())).await;
        assert_eq!(result, Err(Error::Pool(PoolError::Stopped("test-read"))));
    }

    #[tokio::test]
    #[expect(
        clippy::await_holding_lock,
        reason = "the test holds the lock on purpose; an inline run never awaits"
    )]
    async fn inline_runs_on_the_shared_value_and_refuses_a_held_lock() {
        let shared = Arc::new(parking_lot::Mutex::new(5));
        let pool = Pool::Inline(shared.clone());
        assert_eq!(pool.run(|v| Ok::<_, Error>(*v)).await, Ok(5));
        let _held = shared.lock();
        assert_eq!(
            pool.run(|v| Ok::<_, Error>(*v)).await,
            Err(Error::Pool(PoolError::Locked))
        );
    }

    #[test]
    fn read_only_refuses_writes_and_restores_the_connection() {
        let conn = writer(&temp_db());
        let refused = read_only(&conn, || conn.execute("INSERT INTO t VALUES (1)", []));
        assert!(refused.is_err());
        conn.execute("INSERT INTO t VALUES (1)", []).unwrap();
    }

    #[derive(Debug, PartialEq, thiserror::Error)]
    enum Error {
        #[error(transparent)]
        Pool(#[from] PoolError),
        #[error("{0}")]
        Sqlite(String),
    }
    impl From<rusqlite::Error> for Error {
        fn from(e: rusqlite::Error) -> Self {
            Error::Sqlite(e.to_string())
        }
    }
}
