use std::time::Duration;

use super::*;
use crate::query::parse;
use crate::{build, Format, LogConfig};

struct Setup {
    telemetry: Arc<crate::Telemetry>,
    store: LogStore,
    _dir: TempDir,
    _guard: tracing::subscriber::DefaultGuard,
}

/// A directory removed when dropped (no extra dev-dependency for one).
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "telemetry-store-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn setup(level: &str) -> Setup {
    let dir = TempDir::new();
    let (telemetry, subscriber) = build("engine", Format::Json, false, level, std::io::sink);
    let guard = tracing::subscriber::set_default(subscriber);
    let telemetry = Arc::new(telemetry);
    let store = telemetry.open_store(&dir.0.join("logs.db")).unwrap();
    Setup {
        telemetry,
        store,
        _dir: dir,
        _guard: guard,
    }
}

fn all(store: &LogStore) -> Vec<LogRow> {
    store
        .query(&LogQuery {
            limit: 1000,
            ..LogQuery::default()
        })
        .unwrap()
}

/// Waits for the writer thread to have stored `count` lines.
fn wait_for(store: &LogStore, count: usize) -> Vec<LogRow> {
    // Bounds only a hung writer, not how fast it is on a loaded machine.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let rows = all(store);
        if rows.len() >= count || Instant::now() > deadline {
            return rows;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn filtered(store: &LogStore, filter: &str) -> Vec<String> {
    let query = LogQuery {
        filter: parse(filter).unwrap(),
        limit: 100,
        ..LogQuery::default()
    };
    store
        .query(&query)
        .unwrap()
        .into_iter()
        .map(|r| r.message)
        .collect()
}

#[test]
fn lines_are_stored_with_their_attributes_and_trace_and_found_by_filter() {
    let s = setup("info");
    let request = tracing::info_span!("HTTP request", store.id = "s_1");
    request.in_scope(|| {
        tracing::info!(order.id = "o_1", attempts = 3, view_key = "secret", "first");
        tracing::warn!(order.id = "o_2", "second");
    });
    tracing::debug!("below the level");
    tracing::error!(store.id = "s_2", "third");

    let rows = wait_for(&s.store, 3);
    assert_eq!(
        rows.iter().map(|r| r.message.as_str()).collect::<Vec<_>>(),
        ["third", "second", "first"],
        "newest first"
    );
    let first = &rows[2];
    assert_eq!(first.service, "engine");
    assert_eq!(first.severity(), Severity::Info);
    assert_eq!(first.attributes["store.id"], "s_1");
    assert_eq!(first.attributes["attempts"], 3);
    assert_eq!(first.attributes["view_key"], redact::REDACTED);
    assert_eq!(first.spans, ["HTTP request"]);
    let trace_id = first.trace_id.clone().unwrap();
    assert_eq!(rows[1].trace_id.as_deref(), Some(trace_id.as_str()));
    assert!(rows[0].trace_id.is_none());

    assert_eq!(filtered(&s.store, "store.id = 's_1'"), ["second", "first"]);
    assert_eq!(filtered(&s.store, "level >= warn"), ["third", "second"]);
    assert_eq!(
        filtered(&s.store, "order.id = 'o_1' or store.id = 's_2'"),
        ["third", "first"]
    );
    assert_eq!(filtered(&s.store, "attempts > 2"), ["first"]);
    assert_eq!(filtered(&s.store, "not has order.id"), ["third"]);
    assert_eq!(
        filtered(&s.store, "'SEC'"),
        ["second"],
        "text search is case-insensitive"
    );
    assert_eq!(
        filtered(&s.store, &format!("trace_id = '{trace_id}'")),
        ["second", "first"]
    );
}

#[test]
fn pages_follow_the_cursor_both_ways_and_time_ranges_apply() {
    let s = setup("info");
    for n in 0..7 {
        // Each line on a later clock reading than the one before: the time
        // ranges below select by timestamp, and on a coarse clock two lines
        // logged back to back could share one.
        let before = std::time::SystemTime::now();
        while std::time::SystemTime::now() <= before {
            std::hint::spin_loop();
        }
        tracing::info!("line {n}");
    }
    let rows = wait_for(&s.store, 7);
    let page = |before: Option<Cursor>, after: Option<Cursor>| -> Vec<String> {
        s.store
            .query(&LogQuery {
                before,
                after,
                limit: 3,
                ..LogQuery::default()
            })
            .unwrap()
            .into_iter()
            .map(|r| r.message)
            .collect()
    };
    assert_eq!(page(None, None), ["line 6", "line 5", "line 4"]);
    assert_eq!(
        page(Some(rows[2].cursor()), None),
        ["line 3", "line 2", "line 1"]
    );
    assert_eq!(
        page(None, Some(rows[4].cursor())),
        ["line 5", "line 4", "line 3"],
        "the page just newer, still newest first"
    );
    assert_eq!(page(None, Some(rows[0].cursor())), Vec::<String>::new());

    let middle = s
        .store
        .query(&LogQuery {
            from: Some(rows[4].ts),
            to: Some(rows[1].ts),
            limit: 10,
            ..LogQuery::default()
        })
        .unwrap();
    assert_eq!(
        middle
            .iter()
            .map(|r| r.message.as_str())
            .collect::<Vec<_>>(),
        ["line 4", "line 3", "line 2"]
    );

    let cursor = rows[3].cursor();
    assert_eq!(Cursor::parse(&cursor.encode()), Some(cursor));
    assert_eq!(
        Cursor::parse("1.2.key-custody-server").unwrap().service,
        "key-custody-server"
    );
    assert_eq!(Cursor::parse("x.2.a"), None);
}

/// A process's last lines, logged just before `main` returns, are stored
/// once `flush` returns: nothing is left in the channel or the writer's
/// batch to die with the process.
#[tokio::test]
async fn flush_returns_once_every_line_logged_so_far_is_stored() {
    let s = setup("info");
    for i in 0..300 {
        tracing::info!(i, "shutting down");
    }
    assert!(s.telemetry.flush(Duration::from_secs(30)).await);
    assert_eq!(all(&s.store).len(), 300);
    // Nothing new: returns straight away.
    assert!(s.telemetry.flush(Duration::ZERO).await);
}

#[test]
fn lines_logged_before_the_store_opens_are_kept() {
    let dir = TempDir::new();
    let (telemetry, subscriber) = build("monokulo", Format::Json, false, "info", std::io::sink);
    let _guard = tracing::subscriber::set_default(subscriber);
    tracing::info!("starting");
    let store = telemetry.open_store(&dir.0.join("logs.db")).unwrap();
    tracing::info!("started");
    let rows = wait_for(&store, 2);
    assert_eq!(
        rows.iter().map(|r| r.message.as_str()).collect::<Vec<_>>(),
        ["started", "starting"]
    );
}

#[test]
fn spans_are_stored_redacted_with_their_parents_and_a_trace_reads_back_whole() {
    let s = setup("info");
    let outer = tracing::info_span!(
        "HTTP request",
        otel.kind = "server",
        secret_token = "sk_abc",
        store.id = "s_1"
    );
    let trace_id = crate::trace::span_context(&outer)
        .unwrap()
        .trace_id()
        .to_string();
    outer.in_scope(|| {
        let inner = tracing::info_span!("engine call");
        inner.in_scope(|| tracing::info!("inside"));
    });
    drop(outer);

    // Spans and lines can reach the writer in different batches: wait for
    // both. The deadline bounds only a hung writer.
    let deadline = Instant::now() + Duration::from_secs(30);
    let trace = loop {
        let trace = s.store.trace(&trace_id).unwrap();
        if (trace.spans.len() == 2 && trace.logs.len() == 1) || Instant::now() > deadline {
            break trace;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(trace.spans.len(), 2, "{trace:?}");
    let (request, call) = (&trace.spans[0], &trace.spans[1]);
    assert_eq!(request.name, "HTTP request");
    assert_eq!(request.kind, "server");
    assert_eq!(request.parent_span_id, None);
    assert_eq!(
        call.parent_span_id.as_deref(),
        Some(request.span_id.as_str())
    );
    assert!(request.end >= request.start);
    assert_eq!(request.attributes["secret_token"], redact::REDACTED);
    assert_eq!(request.attributes["store.id"], "s_1");
    assert_eq!(trace.logs.len(), 1);
    assert_eq!(
        trace.logs[0].span_id.as_deref(),
        Some(call.span_id.as_str())
    );
}

#[test]
fn retention_removes_old_lines_then_the_oldest_until_under_the_size_limit() {
    let s = setup("info");
    for n in 0..500 {
        tracing::info!(padding = %"x".repeat(2000), "line {n}");
    }
    let rows = wait_for(&s.store, 500);
    assert_eq!(rows.len(), 500);
    let conn = open_writer(s.store.path()).unwrap();

    // By size: 1 MB holds far fewer than 500 lines of 2 KB.
    s.store.set_limits(14, 1);
    s.store.maintain(&conn, now_nanos()).unwrap();
    let left = all(&s.store);
    assert!(left.len() < 500 && !left.is_empty(), "{}", left.len());
    assert_eq!(left[0].message, "line 499", "the newest are kept");
    assert!(used_bytes(&conn).unwrap() <= 1024 * 1024);

    // By age: everything is older than a day, a day from now.
    s.store.set_limits(1, 500);
    s.store
        .maintain(&conn, now_nanos() + 2 * 86_400_000_000_000)
        .unwrap();
    assert!(all(&s.store).is_empty());
}

#[test]
fn the_retention_settings_reach_the_store() {
    let s = setup("info");
    s.telemetry.apply(&LogConfig {
        retention_days: 3,
        max_mb: 20,
        ..LogConfig::default()
    });
    assert_eq!(
        s.store.inner.limits.retention_days.load(Ordering::Relaxed),
        3
    );
    assert_eq!(
        s.store.inner.limits.max_bytes.load(Ordering::Relaxed),
        20 * 1024 * 1024
    );
}

#[test]
fn a_histogram_counts_lines_per_slice_and_attribute_names_are_listed() {
    let s = setup("info");
    tracing::info!(network = "stagenet", "a");
    tracing::warn!(order.id = "o_1", "b");
    tracing::warn!("c");
    let rows = wait_for(&s.store, 3);
    let from = rows[2].ts;
    let to = rows[0].ts + 1;
    let counts = s.store.histogram(None, from, to, 1).unwrap();
    assert_eq!(counts, [3]);
    let warnings = s
        .store
        .histogram(parse("level = warn").unwrap().as_ref(), from, to, 4)
        .unwrap();
    assert_eq!(warnings.iter().sum::<u64>(), 2);
    assert_eq!(warnings.len(), 4);
    assert_eq!(s.store.attribute_names().unwrap(), ["network", "order.id"]);
    // Bounds are the caller's: a backwards range is empty, and the widest
    // possible one is counted, not overflowed.
    assert_eq!(s.store.histogram(None, to, from, 3).unwrap(), [0, 0, 0]);
    assert_eq!(
        s.store
            .histogram(None, 0, i64::MAX, 2)
            .unwrap()
            .iter()
            .sum::<u64>(),
        3
    );
    // ...and the widest range there is doesn't panic (SQLite answers it in
    // its own way).
    let _ = s.store.histogram(None, i64::MIN, i64::MAX, 2);
}

#[test]
fn watching_the_latest_id_sees_new_lines() {
    let s = setup("info");
    let mut latest = s.store.subscribe();
    let before = *latest.borrow_and_update();
    tracing::info!("new");
    wait_for(&s.store, 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !latest.has_changed().unwrap() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(*latest.borrow() > before);
}

#[test]
fn a_store_made_before_session_ids_were_indexed_gets_the_column_and_finds_by_it() {
    let dir = TempDir::new();
    let path = dir.0.join("logs.db");
    {
        // The table as it was: no session_id column.
        let conn = Connection::open(&path).unwrap();
        let old = SCHEMA.replace(",\n    session_id TEXT GENERATED ALWAYS AS (json_extract(attributes, '$.\"session.id\"')) VIRTUAL", "");
        assert_ne!(old, SCHEMA);
        conn.execute_batch(&old).unwrap();
        conn.execute(
            "INSERT INTO logs (ts, level, service, target, message, attributes, spans) VALUES (1, 9, 'monokulo', 't', 'signed in', ?1, '[]')",
            [r#"{"session.id":"5e55"}"#],
        )
        .unwrap();
        let has: i64 = conn
            .query_row(
                "SELECT count(*) FROM pragma_table_xinfo('logs') WHERE name = 'session_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has, 0, "the old table really lacks it");
    }
    let conn = open_writer(&path).unwrap();
    let plan: String = conn
        .query_row(
            "EXPLAIN QUERY PLAN SELECT id FROM logs WHERE session_id = '5e55'",
            [],
            |r| r.get(3),
        )
        .unwrap();
    assert!(plan.contains("logs_session"), "{plan}");
    drop(conn);
    let (telemetry, subscriber) = build("monokulo", Format::Json, false, "info", std::io::sink);
    let _guard = tracing::subscriber::set_default(subscriber);
    let store = telemetry.open_store(&path).unwrap();
    assert_eq!(
        filtered(&store, "session.id = '5e55'"),
        vec!["signed in".to_string()]
    );
    assert!(filtered(&store, "session.id = 'other'").is_empty());
}
