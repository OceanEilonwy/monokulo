//! The local log store, `logs.db` (structured_logging.md part 3).
//!
//! Every line the level filter lets through, and every span at `info` or
//! above, is kept in a SQLite file of its own next to the process's main
//! database. Monokulo's Logs page reads it directly; the engine's is read
//! through the engine's admin API.
//!
//! Writing never blocks the code that logs: lines go through a bounded
//! channel to one writer thread, which inserts them in batches. When the
//! channel is full, lines are dropped and counted, and the count is logged
//! once a minute. Lines logged before the store is opened (at start-up,
//! before the database path is known) are held in memory, up to a limit,
//! and stored once it opens.
//!
//! The same writer thread applies retention once a minute: rows older
//! than the retention period go, then the oldest rows until the file is
//! under its size limit (both live settings).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opentelemetry::trace::{SpanId, Status};
use opentelemetry_sdk::trace::{SpanData, SpanProcessor};
use parking_lot::Mutex;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub mod api;

use crate::json::Line;
use crate::query::{Expr, Severity};
use crate::redact;

pub const DEFAULT_RETENTION_DAYS: u64 = 14;
pub const DEFAULT_MAX_MB: u64 = 500;

/// Lines waiting for the writer; more are dropped.
const CHANNEL: usize = 10_000;
/// Lines held before the store opens; more are dropped.
const EARLY: usize = 2_000;
/// Most rows inserted in one transaction.
const BATCH: usize = 1_000;
const MAINTENANCE_EVERY: Duration = Duration::from_secs(60);

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS logs (
    id INTEGER PRIMARY KEY,
    ts INTEGER NOT NULL,
    level INTEGER NOT NULL,
    service TEXT NOT NULL,
    target TEXT NOT NULL,
    message TEXT NOT NULL,
    trace_id TEXT,
    span_id TEXT,
    attributes TEXT NOT NULL,
    spans TEXT NOT NULL,
    store_id TEXT GENERATED ALWAYS AS (json_extract(attributes, '$."store.id"')) VIRTUAL,
    order_id TEXT GENERATED ALWAYS AS (json_extract(attributes, '$."order.id"')) VIRTUAL,
    session_id TEXT GENERATED ALWAYS AS (json_extract(attributes, '$."session.id"')) VIRTUAL
);
CREATE INDEX IF NOT EXISTS logs_ts ON logs (ts, service, id);
CREATE INDEX IF NOT EXISTS logs_trace ON logs (trace_id) WHERE trace_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS logs_store ON logs (store_id, ts) WHERE store_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS logs_order ON logs (order_id, ts) WHERE order_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS spans (
    trace_id TEXT NOT NULL,
    span_id TEXT NOT NULL,
    parent_span_id TEXT,
    service TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    start_ts INTEGER NOT NULL,
    end_ts INTEGER NOT NULL,
    status TEXT NOT NULL,
    attributes TEXT NOT NULL,
    PRIMARY KEY (trace_id, span_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS spans_start ON spans (start_ts);
"#;

fn now_nanos() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

fn nanos(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// One stored line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRow {
    /// Unique within its service's store; 0 before it is stored.
    pub id: i64,
    /// Unix time in nanoseconds.
    pub ts: i64,
    /// OpenTelemetry severity number (9 is info, 13 warn, 17 error).
    pub level: i64,
    pub service: String,
    pub target: String,
    pub message: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub attributes: Map<String, Value>,
    /// The names of the spans the line was written in, outermost first.
    pub spans: Vec<String>,
}

impl LogRow {
    fn from_line(service: &str, line: Line) -> LogRow {
        let (trace_id, span_id) = match line.ids {
            Some((trace_id, span_id)) => (Some(trace_id), Some(span_id)),
            None => (None, None),
        };
        LogRow {
            id: 0,
            ts: i64::try_from(line.timestamp.unix_timestamp_nanos()).unwrap_or(i64::MAX),
            level: Severity::from(&line.level) as i64,
            service: service.to_string(),
            target: line.target,
            message: line.message,
            trace_id,
            span_id,
            attributes: line.attributes,
            spans: line.spans,
        }
    }

    pub fn severity(&self) -> Severity {
        Severity::from_number(self.level)
    }

    pub fn cursor(&self) -> Cursor {
        Cursor { ts: self.ts, service: self.service.clone(), id: self.id }
    }
}

/// One stored span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanRow {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub service: String,
    pub name: String,
    /// `server`, `client` or `internal`.
    pub kind: String,
    /// Unix times in nanoseconds.
    pub start: i64,
    pub end: i64,
    /// `unset`, `ok` or `error`.
    pub status: String,
    pub attributes: Map<String, Value>,
}

/// Everything stored for one trace: its spans by start time and its lines
/// by time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Trace {
    pub spans: Vec<SpanRow>,
    pub logs: Vec<LogRow>,
}

/// A position in the newest-first order of lines: `(ts, service, id)`,
/// which is the same across every store, so lines from monokulo's and the
/// engine's stores can be merged and paged together.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cursor {
    pub ts: i64,
    pub service: String,
    pub id: i64,
}

impl Cursor {
    /// `ts.id.service`, for URLs.
    pub fn encode(&self) -> String {
        format!("{}.{}.{}", self.ts, self.id, self.service)
    }

    pub fn parse(text: &str) -> Option<Cursor> {
        let mut parts = text.splitn(3, '.');
        let ts = parts.next()?.parse().ok()?;
        let id = parts.next()?.parse().ok()?;
        let service = parts.next()?.to_string();
        (!service.is_empty()).then_some(Cursor { ts, service, id })
    }
}

/// Which lines to read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogQuery {
    pub filter: Option<Expr>,
    /// Unix nanoseconds, inclusive.
    pub from: Option<i64>,
    /// Unix nanoseconds, exclusive.
    pub to: Option<i64>,
    /// Only lines older than this (the next page).
    pub before: Option<Cursor>,
    /// Only lines newer than this (the previous page, or live tail).
    pub after: Option<Cursor>,
    pub limit: u32,
}

#[derive(Debug)]
pub struct StoreError(String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "log store: {}", self.0)
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError(e.to_string())
    }
}

#[derive(Clone)]
pub(crate) enum Record {
    Log(LogRow),
    Span(SpanRow),
}

/// Where the subscriber hands lines and spans to the store.
#[derive(Default)]
pub(crate) struct StoreSink {
    sender: OnceLock<SyncSender<Record>>,
    early: Mutex<Vec<Record>>,
    dropped: AtomicU64,
    /// OTLP export, when configured: gets a copy of every record.
    pub(crate) otlp: parking_lot::RwLock<Option<crate::otlp::Exporter>>,
}

impl StoreSink {
    pub(crate) fn log(&self, service: &str, line: Line) {
        self.send(Record::Log(LogRow::from_line(service, line)));
    }

    fn send(&self, record: Record) {
        if let Some(exporter) = self.otlp.read().as_ref() {
            if !exporter.offer(record.clone()) {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
        if let Some(sender) = self.sender.get() {
            self.try_send(sender, record);
            return;
        }
        let mut early = self.early.lock();
        // Checked again under the lock that `attach` takes to drain it.
        if let Some(sender) = self.sender.get() {
            drop(early);
            self.try_send(sender, record);
        } else if early.len() < EARLY {
            early.push(record);
        } else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn try_send(&self, sender: &SyncSender<Record>, record: Record) {
        match sender.try_send(record) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn attach(&self, sender: SyncSender<Record>) {
        let mut early = self.early.lock();
        for record in early.drain(..) {
            self.try_send(&sender, record);
        }
        let _ = self.sender.set(sender);
    }
}

/// Stores the spans OpenTelemetry finishes.
#[derive(Debug)]
pub(crate) struct StoreSpans {
    service: &'static str,
    sink: Arc<StoreSink>,
}

impl std::fmt::Debug for StoreSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreSink")
    }
}

impl StoreSpans {
    pub(crate) fn new(service: &'static str, sink: Arc<StoreSink>) -> Self {
        StoreSpans { service, sink }
    }
}

/// A span's attributes as JSON, redacted the same way as line attributes.
/// `tracing-opentelemetry` copies span fields as they were recorded, so
/// this is the only place they are redacted before storing or export.
pub fn redacted_attributes(attributes: &[opentelemetry::KeyValue]) -> Map<String, Value> {
    use opentelemetry::Value as V;
    let mut map = Map::new();
    for kv in attributes {
        let key = kv.key.as_str();
        let value = match &kv.value {
            _ if redact::is_secret_name(key) => Value::String(redact::REDACTED.to_string()),
            V::Bool(b) => Value::Bool(*b),
            V::I64(n) => Value::from(*n),
            V::F64(n) => serde_json::Number::from_f64(*n).map_or(Value::Null, Value::Number),
            V::String(s) => Value::String(redact::field(key, s.as_str()).into_owned()),
            other => Value::String(redact::field(key, &other.to_string()).into_owned()),
        };
        map.insert(key.to_string(), value);
    }
    map
}

impl SpanRow {
    pub(crate) fn from_span(service: &str, span: &SpanData) -> SpanRow {
        let parent = span.parent_span_id;
        SpanRow {
            trace_id: span.span_context.trace_id().to_string(),
            span_id: span.span_context.span_id().to_string(),
            parent_span_id: (parent != SpanId::INVALID).then(|| parent.to_string()),
            service: service.to_string(),
            name: span.name.to_string(),
            kind: format!("{:?}", span.span_kind).to_ascii_lowercase(),
            start: nanos(span.start_time),
            end: nanos(span.end_time),
            status: match &span.status {
                Status::Unset => "unset",
                Status::Ok => "ok",
                Status::Error { .. } => "error",
            }
            .to_string(),
            attributes: redacted_attributes(&span.attributes),
        }
    }
}

impl SpanProcessor for StoreSpans {
    fn on_start(&self, _span: &mut opentelemetry_sdk::trace::Span, _cx: &opentelemetry::Context) {}

    fn on_end(&self, span: SpanData) {
        self.sink.send(Record::Span(SpanRow::from_span(self.service, &span)));
    }

    fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }
}

struct Limits {
    retention_days: AtomicU64,
    max_bytes: AtomicU64,
}

struct Inner {
    path: PathBuf,
    reader: Mutex<Connection>,
    latest: tokio::sync::watch::Sender<i64>,
    limits: Limits,
}

/// An open log store. Cheap to clone. Reads are synchronous SQLite
/// queries: call them from `spawn_blocking` in async code.
#[derive(Clone)]
pub struct LogStore {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for LogStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogStore").field("path", &self.inner.path).finish()
    }
}

/// Where a process keeps its log store: beside its main database, named
/// after it (`monokulo.db` gets `monokulo.logs.db`), so two processes
/// sharing a directory don't share a store.
pub fn path_beside(database: &Path) -> PathBuf {
    let stem = database.file_stem().and_then(|s| s.to_str()).filter(|s| !s.is_empty()).unwrap_or("service");
    database.with_file_name(format!("{stem}.logs.db"))
}

fn open_writer(path: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open(path)?;
    // Before any table exists, so freed pages can be given back to the
    // file system as retention deletes rows.
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(SCHEMA)?;
    add_session_column(&conn)?;
    Ok(conn)
}

/// `logs.session_id` and its index, for a store made before they were
/// part of [`SCHEMA`]. A virtual column costs nothing to add: no row is
/// rewritten.
fn add_session_column(conn: &Connection) -> Result<(), StoreError> {
    let has: bool = conn.query_row("SELECT count(*) FROM pragma_table_xinfo('logs') WHERE name = 'session_id'", [], |r| r.get::<_, i64>(0))? > 0;
    if !has {
        conn.execute_batch(r#"ALTER TABLE logs ADD COLUMN session_id TEXT GENERATED ALWAYS AS (json_extract(attributes, '$."session.id"')) VIRTUAL;"#)?;
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS logs_session ON logs (session_id, ts) WHERE session_id IS NOT NULL;")?;
    Ok(())
}

impl LogStore {
    /// Opens (creating if needed) the store at `path` and starts its writer
    /// thread, fed by `sink`.
    pub(crate) fn open(path: &Path, sink: Arc<StoreSink>) -> Result<LogStore, StoreError> {
        let writer = open_writer(path)?;
        let reader = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        let latest: i64 = writer.query_row("SELECT coalesce(max(id), 0) FROM logs", [], |r| r.get(0))?;
        let store = LogStore {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                reader: Mutex::new(reader),
                latest: tokio::sync::watch::Sender::new(latest),
                limits: Limits {
                    retention_days: AtomicU64::new(DEFAULT_RETENTION_DAYS),
                    max_bytes: AtomicU64::new(DEFAULT_MAX_MB * 1024 * 1024),
                },
            }),
        };
        let (sender, receiver) = std::sync::mpsc::sync_channel(CHANNEL);
        let thread_store = store.clone();
        let thread_sink = sink.clone();
        std::thread::Builder::new()
            .name("log-store".into())
            .spawn(move || thread_store.write_loop(writer, receiver, thread_sink))
            .map_err(|e| StoreError(e.to_string()))?;
        sink.attach(sender);
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Sets the retention period and size limit (settings
    /// `logging.retention_days` and `logging.max_mb`).
    pub fn set_limits(&self, retention_days: u64, max_mb: u64) {
        self.inner.limits.retention_days.store(retention_days.max(1), Ordering::Relaxed);
        self.inner.limits.max_bytes.store(max_mb.max(1).saturating_mul(1024 * 1024), Ordering::Relaxed);
    }

    /// The id of the newest stored line, updated after every batch: wait on
    /// it for new lines (live tail).
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<i64> {
        self.inner.latest.subscribe()
    }

    fn write_loop(&self, mut conn: Connection, receiver: Receiver<Record>, sink: Arc<StoreSink>) {
        let mut last_maintenance = Instant::now();
        let mut batch = Vec::with_capacity(BATCH);
        loop {
            match receiver.recv_timeout(Duration::from_secs(1)) {
                Ok(record) => {
                    batch.push(record);
                    while batch.len() < BATCH {
                        match receiver.try_recv() {
                            Ok(record) => batch.push(record),
                            Err(_) => break,
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if !batch.is_empty() {
                match insert(&mut conn, &mut batch) {
                    Ok(Some(latest)) => {
                        self.inner.latest.send_replace(latest);
                    }
                    Ok(None) => {}
                    Err(e) => shared_warn(&e),
                }
                batch.clear();
            }
            if last_maintenance.elapsed() >= MAINTENANCE_EVERY {
                last_maintenance = Instant::now();
                if let Err(e) = self.maintain(&conn, now_nanos()) {
                    shared_warn(&e);
                }
                let dropped = sink.dropped.swap(0, Ordering::Relaxed);
                if dropped > 0 {
                    tracing::warn!(dropped, "the log store fell behind; some lines were not stored");
                }
            }
        }
    }

    /// Retention: rows older than the retention period, then the oldest
    /// rows until the file is under its size limit.
    fn maintain(&self, conn: &Connection, now: i64) -> Result<(), StoreError> {
        let days = self.inner.limits.retention_days.load(Ordering::Relaxed);
        let cutoff = now.saturating_sub(i64::try_from(days).unwrap_or(i64::MAX).saturating_mul(86_400_000_000_000));
        conn.execute("DELETE FROM logs WHERE ts < ?1", [cutoff])?;
        conn.execute("DELETE FROM spans WHERE start_ts < ?1", [cutoff])?;
        let max_bytes = self.inner.limits.max_bytes.load(Ordering::Relaxed);
        for _ in 0..20 {
            if used_bytes(conn)? <= max_bytes {
                break;
            }
            let count: i64 = conn.query_row("SELECT count(*) FROM logs", [], |r| r.get(0))?;
            if count == 0 {
                conn.execute("DELETE FROM spans", [])?;
                break;
            }
            // The oldest tenth of the lines, and the spans that started
            // before the oldest line left.
            conn.execute("DELETE FROM logs WHERE id IN (SELECT id FROM logs ORDER BY ts LIMIT ?1)", [(count / 10).max(1)])?;
            conn.execute("DELETE FROM spans WHERE start_ts < (SELECT coalesce(min(ts), 0) FROM logs)", [])?;
        }
        conn.execute_batch("PRAGMA incremental_vacuum;")?;
        Ok(())
    }

    /// Lines matching `query`, newest first.
    pub fn query(&self, query: &LogQuery) -> Result<Vec<LogRow>, StoreError> {
        let mut params: Vec<SqlValue> = Vec::new();
        let mut conditions: Vec<String> = Vec::new();
        if let Some(filter) = &query.filter {
            conditions.push(filter.to_sql(&mut params));
        }
        if let Some(from) = query.from {
            conditions.push("ts >= ?".into());
            params.push(SqlValue::Integer(from));
        }
        if let Some(to) = query.to {
            conditions.push("ts < ?".into());
            params.push(SqlValue::Integer(to));
        }
        if let Some(before) = &query.before {
            conditions.push("(ts, service, id) < (?, ?, ?)".into());
            params.extend([SqlValue::Integer(before.ts), SqlValue::Text(before.service.clone()), SqlValue::Integer(before.id)]);
        }
        if let Some(after) = &query.after {
            conditions.push("(ts, service, id) > (?, ?, ?)".into());
            params.extend([SqlValue::Integer(after.ts), SqlValue::Text(after.service.clone()), SqlValue::Integer(after.id)]);
        }
        let condition = if conditions.is_empty() { "1".to_string() } else { conditions.join(" AND ") };
        // Newer pages are read oldest first from the cursor, then turned round.
        let order = if query.after.is_some() && query.before.is_none() { "ASC" } else { "DESC" };
        params.push(SqlValue::Integer(i64::from(query.limit.clamp(1, 1000))));
        let sql = format!(
            "SELECT {LOG_COLUMNS} FROM logs WHERE {condition} ORDER BY ts {order}, service {order}, id {order} LIMIT ?"
        );
        let conn = self.inner.reader.lock();
        let mut statement = conn.prepare_cached(&sql)?;
        let mut rows: Vec<LogRow> = statement.query_map(params_from_iter(params), log_row)?.collect::<Result<_, _>>()?;
        if order == "ASC" {
            rows.reverse();
        }
        Ok(rows)
    }

    /// One trace's spans and lines.
    pub fn trace(&self, trace_id: &str) -> Result<Trace, StoreError> {
        let conn = self.inner.reader.lock();
        let logs = conn
            .prepare_cached(&format!("SELECT {LOG_COLUMNS} FROM logs WHERE trace_id = ?1 ORDER BY ts, id LIMIT 5000"))?
            .query_map([trace_id], log_row)?
            .collect::<Result<_, _>>()?;
        let spans = conn
            .prepare_cached(
                "SELECT trace_id, span_id, parent_span_id, service, name, kind, start_ts, end_ts, status, attributes
                 FROM spans WHERE trace_id = ?1 ORDER BY start_ts LIMIT 5000",
            )?
            .query_map([trace_id], |row| {
                Ok(SpanRow {
                    trace_id: row.get(0)?,
                    span_id: row.get(1)?,
                    parent_span_id: row.get(2)?,
                    service: row.get(3)?,
                    name: row.get(4)?,
                    kind: row.get(5)?,
                    start: row.get(6)?,
                    end: row.get(7)?,
                    status: row.get(8)?,
                    attributes: json_map(&row.get::<_, String>(9)?),
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(Trace { spans, logs })
    }

    /// How many lines matching `filter` fall in each of `buckets` equal
    /// slices of `[from, to)`.
    pub fn histogram(&self, filter: Option<&Expr>, from: i64, to: i64, buckets: u32) -> Result<Vec<u64>, StoreError> {
        let buckets = buckets.clamp(1, 500);
        // Rounded up, so the last slice reaches `to`.
        let width = ((to - from + i64::from(buckets) - 1) / i64::from(buckets)).max(1);
        // In the order they appear in the SQL: the bucket, the filter, the range.
        let mut params: Vec<SqlValue> = vec![SqlValue::Integer(from), SqlValue::Integer(width)];
        let mut condition = String::new();
        if let Some(filter) = filter {
            condition = format!("{} AND ", filter.to_sql(&mut params));
        }
        condition.push_str("ts >= ? AND ts < ?");
        params.extend([SqlValue::Integer(from), SqlValue::Integer(to)]);
        let sql = format!("SELECT (ts - ?) / ? AS bucket, count(*) FROM logs WHERE {condition} GROUP BY bucket");
        let conn = self.inner.reader.lock();
        let mut counts = vec![0u64; buckets as usize];
        let mut statement = conn.prepare_cached(&sql)?;
        let mut rows = statement.query(params_from_iter(params))?;
        while let Some(row) = rows.next()? {
            let bucket: i64 = row.get(0)?;
            let count: i64 = row.get(1)?;
            if let Some(slot) = usize::try_from(bucket).ok().and_then(|b| counts.get_mut(b)) {
                *slot += u64::try_from(count).unwrap_or(0);
            }
        }
        Ok(counts)
    }

    /// Attribute names seen on recent lines, for the search box's
    /// suggestions.
    pub fn attribute_names(&self) -> Result<Vec<String>, StoreError> {
        let conn = self.inner.reader.lock();
        let names = conn
            .prepare_cached(
                "SELECT DISTINCT key FROM (SELECT attributes FROM logs ORDER BY id DESC LIMIT 2000), json_each(attributes)
                 ORDER BY key LIMIT 500",
            )?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(names)
    }
}

const LOG_COLUMNS: &str = "id, ts, level, service, target, message, trace_id, span_id, attributes, spans";

fn json_map(text: &str) -> Map<String, Value> {
    serde_json::from_str(text).unwrap_or_default()
}

fn log_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LogRow> {
    Ok(LogRow {
        id: row.get(0)?,
        ts: row.get(1)?,
        level: row.get(2)?,
        service: row.get(3)?,
        target: row.get(4)?,
        message: row.get(5)?,
        trace_id: row.get(6)?,
        span_id: row.get(7)?,
        attributes: json_map(&row.get::<_, String>(8)?),
        spans: serde_json::from_str(&row.get::<_, String>(9)?).unwrap_or_default(),
    })
}

fn used_bytes(conn: &Connection) -> Result<u64, StoreError> {
    let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let free: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
    let size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    Ok(u64::try_from((pages - free).max(0) * size).unwrap_or(0))
}

/// Inserts a batch; returns the id of the last line inserted, if any.
fn insert(conn: &mut Connection, batch: &mut Vec<Record>) -> Result<Option<i64>, StoreError> {
    let tx = conn.transaction()?;
    let mut latest = None;
    {
        let mut logs = tx.prepare_cached(
            "INSERT INTO logs (ts, level, service, target, message, trace_id, span_id, attributes, spans)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        let mut spans = tx.prepare_cached(
            "INSERT OR REPLACE INTO spans (trace_id, span_id, parent_span_id, service, name, kind, start_ts, end_ts, status, attributes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for record in batch.drain(..) {
            match record {
                Record::Log(row) => {
                    logs.execute(params![
                        row.ts,
                        row.level,
                        row.service,
                        row.target,
                        row.message,
                        row.trace_id,
                        row.span_id,
                        Value::Object(row.attributes).to_string(),
                        serde_json::to_string(&row.spans).unwrap_or_else(|_| "[]".into()),
                    ])?;
                    latest = Some(tx.last_insert_rowid());
                }
                Record::Span(row) => {
                    spans.execute(params![
                        row.trace_id,
                        row.span_id,
                        row.parent_span_id,
                        row.service,
                        row.name,
                        row.kind,
                        row.start,
                        row.end,
                        row.status,
                        Value::Object(row.attributes).to_string(),
                    ])?;
                }
            }
        }
    }
    tx.commit()?;
    Ok(latest)
}

/// A store problem, logged. (It goes back into the store's own channel,
/// which is fine: the writer thread only ever `try_send`s to it.)
fn shared_warn(e: &StoreError) {
    tracing::warn!(error = %e, "log store write failed");
}

#[cfg(test)]
pub(crate) mod tests;
