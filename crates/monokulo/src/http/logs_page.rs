//! The Logs page's handlers (structured_logging.md part 5), admin only.
//!
//! - `GET /dashboard/admin/logs`: the page. For fixi, only `#log-results`;
//!   with `part=more`, only the next page's lines ("Older").
//! - `GET /dashboard/admin/logs/tail`: new lines as server-sent events
//!   (Live; JavaScript only).
//! - `GET /dashboard/admin/logs/trace/{trace_id}`: one trace's waterfall.
//! - `GET /dashboard/admin/logs/export?format=ndjson|csv`: a download.
//! - `POST /dashboard/admin/logs/saved`, `.../saved/{id}/delete`: saved
//!   searches.
//!
//! Lines come from monokulo's own store and the engine's
//! (`crate::logs`). The search is the filter language
//! (`telemetry::query`) plus the level, service and time range choices,
//! all in the URL, so any view can be bookmarked or shared.

use std::time::Duration;

use axum::extract::{Form, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use telemetry::query::{and_also, parse, Expr, Op, ParseError, Severity, Value as QValue};
use telemetry::store::api::{is_trace_id, HistogramRequest, LogsRequest};
use telemetry::store::{Cursor, LogRow};

use super::fx::{FxRequest, Timezone};
use super::{AppState, AuthedAdmin};
use crate::logs::Sources;
use crate::views::logs::{self as view, BarView, FormView, HistogramView, LogsViewModel, PropertyView, QueryErrorView, RowView};

/// Lines per page.
const PAGE_SIZE: u32 = 100;
/// Bars in the histogram strip.
const BARS: u32 = 60;
/// Most lines one download holds.
const EXPORT_MAX: usize = 10_000;
/// Most saved searches per admin.
const MAX_SAVED: usize = 50;
const NANOS_PER_SECOND: i64 = 1_000_000_000;

/// The page's URL parameters. Empty values mean "not set".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LogsParams {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub q: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub level: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub service: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub range: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub to: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub before: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub after: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub part: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
}

impl LogsParams {
    /// The same search, without paging or fragment choices.
    fn search_only(&self) -> LogsParams {
        LogsParams {
            q: self.q.clone(),
            level: self.level.clone(),
            service: self.service.clone(),
            range: self.range.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
            ..LogsParams::default()
        }
    }

    fn query_string(&self) -> String {
        serde_urlencoded::to_string(self).unwrap_or_default()
    }

    fn url(&self, path: &str) -> String {
        let query = self.query_string();
        if query.is_empty() { path.to_string() } else { format!("{path}?{query}") }
    }

    fn range(&self) -> &str {
        if self.range.is_empty() { "24h" } else { &self.range }
    }
}

const LOGS: &str = "/dashboard/admin/logs";

fn now_nanos() -> i64 {
    jiff::Timestamp::now().as_nanosecond().try_into().unwrap_or(i64::MAX)
}

/// Where times are shown: the browser's zone when it said one this server
/// knows, UTC otherwise.
fn zone(tz: &Timezone) -> (jiff::tz::TimeZone, String) {
    match tz.0.as_deref().and_then(|name| jiff::tz::TimeZone::get(name).ok().map(|zone| (zone, name.to_string()))) {
        Some(found) => found,
        None => (jiff::tz::TimeZone::UTC, "UTC".to_string()),
    }
}

fn zoned(ts: i64, zone: &jiff::tz::TimeZone) -> Option<jiff::Zoned> {
    jiff::Timestamp::from_nanosecond(i128::from(ts)).ok().map(|t| t.to_zoned(zone.clone()))
}

fn display_time(ts: i64, zone: &jiff::tz::TimeZone) -> String {
    zoned(ts, zone).map(|z| z.strftime("%Y-%m-%d %H:%M:%S%.3f").to_string()).unwrap_or_default()
}

fn iso_time(ts: i64) -> String {
    jiff::Timestamp::from_nanosecond(i128::from(ts)).map(|t| t.to_string()).unwrap_or_default()
}

/// A time as a `datetime-local` value in `zone`.
fn local_input(ts: i64, zone: &jiff::tz::TimeZone) -> String {
    zoned(ts, zone).map(|z| z.strftime("%Y-%m-%dT%H:%M:%S").to_string()).unwrap_or_default()
}

/// Reads a `datetime-local` value (with or without seconds) in `zone`.
fn parse_local(text: &str, zone: &jiff::tz::TimeZone) -> Option<i64> {
    let civil: jiff::civil::DateTime = text.trim().parse().ok()?;
    let zoned = civil.to_zoned(zone.clone()).ok()?;
    zoned.timestamp().as_nanosecond().try_into().ok()
}

/// The time range in Unix nanoseconds, `[from, to)`.
fn time_range(params: &LogsParams, zone: &jiff::tz::TimeZone, now: i64) -> (Option<i64>, Option<i64>) {
    let back = |seconds: i64| (Some(now - seconds * NANOS_PER_SECOND), None);
    match params.range() {
        "15m" => back(15 * 60),
        "1h" => back(3600),
        "6h" => back(6 * 3600),
        "7d" => back(7 * 86_400),
        "14d" => back(14 * 86_400),
        "all" => (None, None),
        "custom" => (parse_local(&params.from, zone), parse_local(&params.to, zone)),
        _ => back(86_400),
    }
}

/// The user's own query, and the whole filter with the level and service
/// choices added.
fn filters(params: &LogsParams) -> Result<(Option<Expr>, Option<Expr>), ParseError> {
    let user = parse(&params.q)?;
    let mut combined = user.clone();
    if let Some(level) = Severity::from_name(&params.level) {
        combined = Some(and_also(combined.as_ref(), Expr::Compare { field: "level".into(), op: Op::Ge, value: QValue::Level(level) }));
    }
    if !params.service.is_empty() {
        combined =
            Some(and_also(combined.as_ref(), Expr::Compare { field: "service".into(), op: Op::Eq, value: QValue::Text(params.service.clone()) }));
    }
    Ok((user, combined))
}

fn query_error(params: &LogsParams, e: &ParseError) -> QueryErrorView {
    let chars: Vec<char> = params.q.chars().collect();
    let start = e.start.min(chars.len());
    let end = e.end.clamp(start, chars.len());
    let marked: String = if start == end { " ".into() } else { chars[start..end].iter().collect() };
    QueryErrorView {
        message: e.message.clone(),
        before: chars[..start].iter().collect(),
        marked,
        after: chars[end..].iter().collect(),
    }
}

/// A property's value as the filter language writes it, when it can be
/// searched for.
fn query_value(value: &Value) -> Option<QValue> {
    match value {
        Value::String(s) => Some(QValue::Text(s.clone())),
        Value::Number(n) => n.as_i64().map(QValue::Integer).or_else(|| n.as_f64().map(QValue::Real)),
        Value::Bool(b) => Some(QValue::Bool(*b)),
        Value::Null => Some(QValue::Null),
        _ => None,
    }
}

/// Find and exclude links for `field = value`, added to the user's query.
fn find_links(params: &LogsParams, user: Option<&Expr>, field: &str, value: Option<QValue>) -> (Option<String>, Option<String>) {
    let Some(value) = value else { return (None, None) };
    let condition = Expr::Compare { field: field.to_string(), op: Op::Eq, value };
    let with = |extra: Expr| LogsParams { q: and_also(user, extra).to_string(), ..params.search_only() }.url(LOGS);
    (Some(with(condition.clone())), Some(with(Expr::Not(Box::new(condition)))))
}

fn row_view(row: &LogRow, params: &LogsParams, user: Option<&Expr>, zone: &jiff::tz::TimeZone) -> RowView {
    let severity = row.severity();
    let mut properties = Vec::new();
    let mut push = |name: &str, shown: String, value: Option<QValue>| {
        let (find_url, exclude_url) = find_links(params, user, name, value);
        properties.push(PropertyView { name: name.to_string(), value: shown, find_url, exclude_url });
    };
    push("level", severity.name().to_string(), Some(QValue::Level(severity)));
    push("service", row.service.clone(), Some(QValue::Text(row.service.clone())));
    for (name, value) in &row.attributes {
        let shown = match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        push(name, shown, query_value(value));
    }
    if let Some(trace_id) = &row.trace_id {
        push("trace_id", trace_id.clone(), Some(QValue::Text(trace_id.clone())));
    }
    if !row.spans.is_empty() {
        push("spans", row.spans.join(" > "), None);
    }
    RowView {
        dom_id: format!("log-{}-{}", row.service, row.id),
        time_display: display_time(row.ts, zone),
        time_iso: iso_time(row.ts),
        severity,
        service: row.service.clone(),
        target: row.target.clone(),
        message: row.message.clone(),
        trace_url: row.trace_id.as_ref().map(|id| format!("{LOGS}/trace/{id}")),
        properties,
    }
}

fn request(combined: Option<&Expr>, from: Option<i64>, to: Option<i64>, before: &str, after: &str, limit: u32) -> LogsRequest {
    LogsRequest {
        q: combined.map(Expr::to_string),
        from,
        to,
        before: Some(before.to_string()).filter(|c| !c.is_empty()),
        after: Some(after.to_string()).filter(|c| !c.is_empty()),
        limit: Some(limit),
    }
}

fn histogram_view(counts: &[u64], params: &LogsParams, from: i64, to: i64, zone: &jiff::tz::TimeZone) -> Option<HistogramView> {
    let max = counts.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return None;
    }
    let width = (to - from) / i64::from(BARS.max(1));
    let bars = counts
        .iter()
        .enumerate()
        .map(|(i, &count)| {
            let start = from + width * i as i64;
            let end = start + width;
            let slice = LogsParams {
                range: "custom".into(),
                from: local_input(start, zone),
                to: local_input(end, zone),
                ..params.search_only()
            };
            BarView {
                count,
                height_pct: u32::try_from(count * 100 / max).unwrap_or(100),
                href: slice.url(LOGS),
                label: format!("{count} lines from {}", display_time(start, zone)),
            }
        })
        .collect();
    Some(HistogramView { bars, from_label: display_time(from, zone), to_label: display_time(to, zone) })
}

async fn build(state: &AppState, admin: &crate::db::UserRow, params: &LogsParams, tz: &Timezone) -> LogsViewModel {
    let (zone, zone_label) = zone(tz);
    let now = now_nanos();
    let (from, to) = time_range(params, &zone, now);
    let search = params.search_only();
    let sources = Sources::from_state(state);
    let saved = state.db.lock().list_saved_log_searches(&admin.id).unwrap_or_default();

    let mut vm = LogsViewModel {
        form: FormView {
            q: params.q.clone(),
            level: params.level.clone(),
            service: params.service.clone(),
            range: params.range().to_string(),
            from: params.from.clone(),
            to: params.to.clone(),
        },
        query_error: None,
        problems: Vec::new(),
        rows: Vec::new(),
        histogram: None,
        older_url: None,
        more_url: None,
        newer_url: None,
        refresh_url: search.url(LOGS),
        tail_url: search.url(&format!("{LOGS}/tail")),
        export_ndjson_url: LogsParams { format: "ndjson".into(), ..search.clone() }.url(&format!("{LOGS}/export")),
        export_csv_url: LogsParams { format: "csv".into(), ..search.clone() }.url(&format!("{LOGS}/export")),
        query_string: search.query_string(),
        saved,
        saved_error: None,
        attribute_names: Vec::new(),
        zone_label,
    };

    let (user, combined) = match filters(params) {
        Ok(filters) => filters,
        Err(e) => {
            vm.query_error = Some(query_error(params, &e));
            vm.attribute_names = attribute_names(&sources).await;
            return vm;
        }
    };
    let page_request = request(combined.as_ref(), from, to, &params.before, &params.after, PAGE_SIZE);
    let histogram_from = from.unwrap_or(now - 14 * 86_400 * NANOS_PER_SECOND);
    let histogram_to = to.unwrap_or(now);
    let histogram_request =
        HistogramRequest { q: combined.as_ref().map(Expr::to_string), from: histogram_from, to: histogram_to, buckets: BARS };
    let (page, counts, names) = tokio::join!(
        crate::logs::read(&sources, &page_request),
        crate::logs::histogram(&sources, &histogram_request),
        attribute_names(&sources)
    );
    vm.attribute_names = names;
    let page = match page {
        Ok(page) => page,
        Err(e) => {
            vm.query_error = Some(query_error(params, &e));
            return vm;
        }
    };
    vm.problems.extend(page.local_problem);
    vm.problems.extend(page.engine_problem);
    vm.rows = page.rows.iter().map(|row| row_view(row, params, user.as_ref(), &zone)).collect();
    if params.before.is_empty() && params.after.is_empty() {
        vm.histogram = histogram_view(&counts, params, histogram_from, histogram_to, &zone);
    }
    if page.rows.len() == PAGE_SIZE as usize {
        if let Some(last) = page.rows.last() {
            let older = LogsParams { before: last.cursor().encode(), ..search.clone() };
            vm.older_url = Some(older.url(LOGS));
            vm.more_url = Some(LogsParams { part: "more".into(), ..older }.url(LOGS));
        }
    }
    if !params.before.is_empty() || !params.after.is_empty() {
        if let Some(first) = page.rows.first() {
            vm.newer_url = Some(LogsParams { after: first.cursor().encode(), ..search.clone() }.url(LOGS));
        }
    }
    // Live starts after the newest line shown (or now).
    let start = page.rows.first().map(LogRow::cursor).unwrap_or(Cursor { ts: now, service: String::new(), id: 0 });
    vm.tail_url = LogsParams { after: start.encode(), ..search }.url(&format!("{LOGS}/tail"));
    vm
}

async fn attribute_names(sources: &Sources) -> Vec<String> {
    let mut names: Vec<String> = ["level", "service", "target", "message", "trace_id", "span_id"].map(String::from).to_vec();
    names.extend(crate::logs::attribute_names(sources).await);
    names.dedup();
    names
}

/// `GET /dashboard/admin/logs`.
pub async fn page(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    fx: FxRequest,
    tz: Timezone,
    Query(params): Query<LogsParams>,
) -> Response {
    let vm = build(&state, &admin, &params, &tz).await;
    if params.part == "more" {
        return Html(view::more_rows(&vm.rows, vm.more_url.as_deref(), vm.older_url.as_deref()).into_string()).into_response();
    }
    if fx.0 {
        return Html(view::results(&vm).into_string()).into_response();
    }
    let chrome = super::page_chrome(&state, Some(&admin), vm.refresh_url.clone());
    Html(view::page(&chrome, &vm).into_string()).into_response()
}

/// `GET /dashboard/admin/logs/tail`: lines newer than `after` (or the
/// `Last-Event-ID` a reconnecting stream sends), as they arrive. Each event
/// carries rendered rows routed to the top of `#log-rows` (ssexi's JSON
/// event form), with the newest row's cursor as its id.
pub async fn tail(
    State(state): State<AppState>,
    AuthedAdmin(..): AuthedAdmin,
    tz: Timezone,
    headers: HeaderMap,
    Query(params): Query<LogsParams>,
) -> Response {
    let (_, combined) = match filters(&params) {
        Ok(filters) => filters,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let resume = headers.get("last-event-id").and_then(|v| v.to_str().ok()).and_then(Cursor::parse);
    let cursor = resume.or_else(|| Cursor::parse(&params.after)).unwrap_or(Cursor { ts: now_nanos(), service: String::new(), id: 0 });
    let sources = Sources::from_state(&state);
    let changed = sources.local.as_ref().map(telemetry::store::LogStore::subscribe);
    let (zone, _) = zone(&tz);
    let user = parse(&params.q).ok().flatten();

    struct Tail {
        sources: Sources,
        combined: Option<Expr>,
        cursor: Cursor,
        changed: Option<tokio::sync::watch::Receiver<i64>>,
        first: bool,
    }
    let state = Tail { sources, combined, cursor, changed, first: true };
    let stream = futures_util::stream::unfold(state, move |mut tail| {
        let (params, user, zone) = (params.clone(), user.clone(), zone.clone());
        async move {
            loop {
                if !tail.first {
                    // New local lines wake it at once; the engine's are
                    // polled.
                    match tail.changed.as_mut() {
                        Some(changed) => {
                            let _ = tokio::time::timeout(Duration::from_secs(2), changed.changed()).await;
                        }
                        None => tokio::time::sleep(Duration::from_secs(2)).await,
                    }
                }
                tail.first = false;
                let request = request(tail.combined.as_ref(), None, None, "", &tail.cursor.encode(), 200);
                let Ok(page) = crate::logs::read(&tail.sources, &request).await else { continue };
                let Some(newest) = page.rows.first() else { continue };
                tail.cursor = newest.cursor();
                let rows: Vec<RowView> = page.rows.iter().map(|row| row_view(row, &params, user.as_ref(), &zone)).collect();
                let event = axum::response::sse::Event::default()
                    .event(r##"{"target":"#log-rows","swap":"afterbegin"}"##)
                    .id(tail.cursor.encode())
                    .data(view::live_rows(&rows).into_string());
                return Some((Ok::<_, std::convert::Infallible>(event), tail));
            }
        }
    });
    crate::live::sse(stream)
}

/// `GET /dashboard/admin/logs/trace/{trace_id}`.
pub async fn trace_page(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    tz: Timezone,
    Path(trace_id): Path<String>,
) -> Response {
    if !is_trace_id(&trace_id) {
        return (StatusCode::NOT_FOUND, "No such trace.").into_response();
    }
    let (zone, zone_label) = zone(&tz);
    let sources = Sources::from_state(&state);
    let (trace, engine_problem) = crate::logs::trace(&sources, &trace_id).await;
    let search = LogsParams { q: format!("trace_id = '{trace_id}'"), range: "all".into(), ..LogsParams::default() };
    let user = parse(&search.q).ok().flatten();

    let start = trace.spans.iter().map(|s| s.start).min().unwrap_or(0);
    let end = trace.spans.iter().map(|s| s.end).max().unwrap_or(start).max(start + 1);
    let total = (end - start) as f64;
    let depth_of = |span: &telemetry::store::SpanRow| {
        let mut depth = 0;
        let mut parent = span.parent_span_id.clone();
        while let Some(id) = parent {
            match trace.spans.iter().find(|s| s.span_id == id) {
                Some(p) if depth < 32 => {
                    depth += 1;
                    parent = p.parent_span_id.clone();
                }
                _ => break,
            }
        }
        depth
    };
    let spans = trace
        .spans
        .iter()
        .map(|span| view::SpanView {
            name: span.attributes.get("otel.name").and_then(Value::as_str).unwrap_or(&span.name).to_string(),
            service: span.service.clone(),
            depth: depth_of(span),
            left_pct: (span.start - start) as f64 * 100.0 / total,
            width_pct: ((span.end - span.start) as f64 * 100.0 / total).max(0.2),
            duration: format_duration(span.end - span.start),
            error: span.status == "error",
            attributes: span
                .attributes
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
                .collect(),
        })
        .collect();
    let vm = view::TraceViewModel {
        trace_id: trace_id.clone(),
        spans,
        rows: trace.logs.iter().map(|row| row_view(row, &search, user.as_ref(), &zone)).collect(),
        problems: engine_problem.into_iter().collect(),
        logs_url: search.url(LOGS),
        zone_label,
    };
    let chrome = super::page_chrome(&state, Some(&admin), format!("{LOGS}/trace/{trace_id}"));
    Html(view::trace_page(&chrome, &vm).into_string()).into_response()
}

fn format_duration(nanos: i64) -> String {
    let ms = nanos as f64 / 1_000_000.0;
    if ms < 1.0 { format!("{:.0} µs", ms * 1000.0) } else if ms < 1000.0 { format!("{ms:.1} ms") } else { format!("{:.2} s", ms / 1000.0) }
}

fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) { format!("\"{}\"", text.replace('"', "\"\"")) } else { text.to_string() }
}

/// `GET /dashboard/admin/logs/export?format=ndjson|csv`: this search's
/// lines, newest first, up to `EXPORT_MAX`.
pub async fn export(State(state): State<AppState>, AuthedAdmin(..): AuthedAdmin, tz: Timezone, Query(params): Query<LogsParams>) -> Response {
    let (zone, _) = zone(&tz);
    let (from, to) = time_range(&params, &zone, now_nanos());
    let (_, combined) = match filters(&params) {
        Ok(filters) => filters,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let sources = Sources::from_state(&state);
    let mut rows: Vec<LogRow> = Vec::new();
    let mut before = String::new();
    while rows.len() < EXPORT_MAX {
        let page = match crate::logs::read(&sources, &request(combined.as_ref(), from, to, &before, "", 500)).await {
            Ok(page) => page,
            Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        };
        let done = page.rows.len() < 500;
        let Some(last) = page.rows.last() else { break };
        before = last.cursor().encode();
        rows.extend(page.rows);
        if done {
            break;
        }
    }
    rows.truncate(EXPORT_MAX);
    let (body, content_type, extension) = if params.format == "csv" {
        let mut out = String::from("time,level,service,target,message,trace_id,attributes\n");
        for row in &rows {
            out.push_str(&format!(
                "{},{},{},{},{},{},{}\n",
                iso_time(row.ts),
                row.severity().name(),
                csv_field(&row.service),
                csv_field(&row.target),
                csv_field(&row.message),
                row.trace_id.as_deref().unwrap_or(""),
                csv_field(&Value::Object(row.attributes.clone()).to_string()),
            ));
        }
        (out, "text/csv; charset=utf-8", "csv")
    } else {
        let mut out = String::new();
        for row in &rows {
            let line = serde_json::json!({
                "timestamp": iso_time(row.ts),
                "level": row.severity().upper(),
                "service": row.service,
                "target": row.target,
                "trace_id": row.trace_id,
                "span_id": row.span_id,
                "message": row.message,
                "attributes": row.attributes,
            });
            out.push_str(&line.to_string());
            out.push('\n');
        }
        (out, "application/x-ndjson", "ndjson")
    };
    (
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"monokulo-logs.{extension}\"")),
        ],
        body,
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct SaveSearchForm {
    name: String,
    query_string: String,
}

fn saved_fragment(state: &AppState, admin: &crate::db::UserRow, query_string: &str, error: Option<&str>) -> maud::Markup {
    let saved = state.db.lock().list_saved_log_searches(&admin.id).unwrap_or_default();
    view::saved_searches(&saved, query_string, error)
}

/// `POST /dashboard/admin/logs/saved`.
pub async fn save_search(State(state): State<AppState>, AuthedAdmin(admin, _): AuthedAdmin, fx: FxRequest, Form(form): Form<SaveSearchForm>) -> Response {
    let name = form.name.trim();
    // Only the page's own parameters are kept, whatever was posted.
    let query_string = serde_urlencoded::from_str::<LogsParams>(&form.query_string).unwrap_or_default().search_only().query_string();
    let back = LogsParams::default().url(LOGS) + if query_string.is_empty() { "" } else { "?" } + &query_string;
    if name.is_empty() || name.chars().count() > 80 {
        let error = "Give the search a name of up to 80 characters.";
        return if fx.0 { super::fx::invalid(saved_fragment(&state, &admin, &query_string, Some(error))) } else { super::dashboard::redirect_302(&back) };
    }
    let created = state.db.lock().create_saved_log_search(&uuid::Uuid::new_v4().to_string(), &admin.id, name, &query_string, crate::now_unix(), MAX_SAVED);
    let error = match created {
        Ok(true) => None,
        Ok(false) => Some("You have 50 saved searches already; remove one first."),
        Err(e) => {
            tracing::error!(error = %e, "saving a log search failed");
            Some("The search couldn't be saved.")
        }
    };
    super::fx::respond(fx, &back, || saved_fragment(&state, &admin, &query_string, error))
}

#[derive(Deserialize)]
pub struct DeleteSearchForm {
    #[serde(default)]
    query_string: String,
}

/// `POST /dashboard/admin/logs/saved/{id}/delete`.
pub async fn delete_search(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    fx: FxRequest,
    Path(id): Path<String>,
    Form(form): Form<DeleteSearchForm>,
) -> Response {
    if let Err(e) = state.db.lock().delete_saved_log_search(&admin.id, &id) {
        tracing::error!(error = %e, "removing a saved log search failed");
    }
    let query_string = serde_urlencoded::from_str::<LogsParams>(&form.query_string).unwrap_or_default().search_only().query_string();
    let back = if query_string.is_empty() { LOGS.to_string() } else { format!("{LOGS}?{query_string}") };
    super::fx::respond(fx, &back, || saved_fragment(&state, &admin, &query_string, None))
}

#[cfg(test)]
mod tests;
