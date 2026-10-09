//! The Logs page's handlers (structured_logging.md part 5), admin only.
//!
//! - `GET /dashboard/admin/logs`: the page. For fixi, only `#log-results`;
//!   with `part=more`, only the next page's lines ("Older").
//! - `GET /dashboard/admin/logs/tail`: new lines as server-sent events
//!   (Live; JavaScript only).
//! - `GET /dashboard/admin/logs/trace/{trace_id}`: one trace's waterfall.
//! - `GET /dashboard/admin/logs/row/{cursor}`: one line's properties, which
//!   a line in a list loads when it's first opened.
//! - `GET /dashboard/admin/logs/pos/{session}`: one POS session's
//!   timeline; `GET /dashboard/admin/logs/pos?order=...` finds the session
//!   that created an order.
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
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use telemetry::query::{and_also, parse, Expr, Op, ParseError, Severity, Value as QValue};
use telemetry::store::api::{is_trace_id, HistogramRequest, LogsRequest};
use telemetry::store::{Cursor, LogRow};

use super::fx::{FxRequest, Timezone};
use super::{AppState, AuthedAdmin};
use crate::logs::Sources;
use crate::views::logs::{
    self as view, BarView, FormView, HistogramView, LogsViewModel, PropertyView, QueryErrorView,
    RowView,
};

/// Lines per page.
const PAGE_SIZE: u32 = 100;
/// Bars in the histogram strip.
const BARS: u32 = 60;
/// Most lines one download holds.
const EXPORT_MAX: usize = 10_000;
/// Most saved searches per admin.
const MAX_SAVED: usize = 50;
/// How often Live asks the engine for new lines (monokulo's own wake it).
const ENGINE_POLL: Duration = Duration::from_secs(1);

/// Most Live streams open at once. Each reads the engine once per
/// [`ENGINE_POLL`] against the engine's rate limit, so a few
/// is all there is room for.
const MAX_TAILS: usize = 4;
static OPEN_TAILS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One open Live stream, counted against [`MAX_TAILS`] until dropped.
struct TailSlot;

impl TailSlot {
    fn take() -> Option<TailSlot> {
        use std::sync::atomic::Ordering;
        let mut open = OPEN_TAILS.load(Ordering::SeqCst);
        while open < MAX_TAILS {
            match OPEN_TAILS.compare_exchange_weak(
                open,
                open + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Some(TailSlot),
                Err(now) => open = now,
            }
        }
        None
    }
}

impl Drop for TailSlot {
    fn drop(&mut self) {
        OPEN_TAILS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
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
    /// `show` includes the Logs page's own requests, hidden otherwise
    /// ([`OWN_REQUESTS`]).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub logs_requests: String,
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
            logs_requests: self.logs_requests.clone(),
            ..LogsParams::default()
        }
    }

    fn query_string(&self) -> String {
        serde_urlencoded::to_string(self).unwrap_or_default()
    }

    fn url(&self, path: &str) -> String {
        let query = self.query_string();
        if query.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{query}")
        }
    }

    fn range(&self) -> &str {
        if self.range.is_empty() {
            "24h"
        } else {
            &self.range
        }
    }
}

const LOGS: &str = "/dashboard/admin/logs";

use shared::time::now_unix_nanos as now_nanos;

/// Where times are shown: the browser's zone when it said one this server
/// knows, UTC otherwise.
/// The zone the admin chose (`users.timezone`), else the browser's, else UTC.
fn zone(tz: &Timezone, admin: &crate::db::UserRow) -> (jiff::tz::TimeZone, String) {
    match admin
        .timezone
        .as_deref()
        .or(tz.0.as_deref())
        .and_then(|name| {
            jiff::tz::TimeZone::get(name)
                .ok()
                .map(|zone| (zone, name.to_string()))
        }) {
        Some(found) => found,
        None => (jiff::tz::TimeZone::UTC, "UTC".to_string()),
    }
}

fn zoned(ts: i64, zone: &jiff::tz::TimeZone) -> Option<jiff::Zoned> {
    jiff::Timestamp::from_nanosecond(i128::from(ts))
        .ok()
        .map(|t| t.to_zoned(zone.clone()))
}

fn display_time(ts: i64, zone: &jiff::tz::TimeZone) -> String {
    zoned(ts, zone)
        .map(|z| z.strftime("%Y-%m-%d %H:%M:%S%.3f").to_string())
        .unwrap_or_default()
}

fn iso_time(ts: i64) -> String {
    jiff::Timestamp::from_nanosecond(i128::from(ts))
        .map(|t| t.to_string())
        .unwrap_or_default()
}

/// A time as a `datetime-local` value in `zone`.
fn local_input(ts: i64, zone: &jiff::tz::TimeZone) -> String {
    zoned(ts, zone)
        .map(|z| z.strftime("%Y-%m-%dT%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Reads a `datetime-local` value (with or without seconds) in `zone`.
fn parse_local(text: &str, zone: &jiff::tz::TimeZone) -> Option<i64> {
    let civil: jiff::civil::DateTime = text.trim().parse().ok()?;
    let zoned = civil.to_zoned(zone.clone()).ok()?;
    zoned.timestamp().as_nanosecond().try_into().ok()
}

/// The time range in Unix nanoseconds, `[from, to)`. A custom range's
/// empty end is open; one that isn't a date and time is refused, not read
/// as "all time".
fn time_range(
    params: &LogsParams,
    zone: &jiff::tz::TimeZone,
    now: i64,
) -> Result<(Option<i64>, Option<i64>), String> {
    let back = |seconds: i64| Ok((Some(now - seconds * NANOS_PER_SECOND), None));
    let end = |text: &str, which: &str| {
        if text.trim().is_empty() {
            return Ok(None);
        }
        parse_local(text, zone).map(Some).ok_or_else(|| {
            format!(
                "The range's {which} \"{}\" isn't a date and time.",
                text.trim()
            )
        })
    };
    match params.range() {
        "15m" => back(15 * 60),
        "1h" => back(3600),
        "6h" => back(6 * 3600),
        "7d" => back(7 * 86_400),
        "14d" => back(14 * 86_400),
        "all" => Ok((None, None)),
        "custom" => Ok((end(&params.from, "start")?, end(&params.to, "end")?)),
        _ => back(86_400),
    }
}

/// Routes whose lines only say someone was reading the logs: this page's
/// own requests, and the engine's that serve them. Hidden unless asked for,
/// or every look at the page would fill it.
const OWN_REQUESTS: [&str; 2] = ["/dashboard/admin/logs%", "/api/v1/admin/logs%"];

/// The user's own query, and the whole filter with the level, service and
/// own-requests choices added.
fn filters(params: &LogsParams) -> Result<(Option<Expr>, Option<Expr>), ParseError> {
    let user = parse(&params.q)?;
    let mut combined = user.clone();
    if params.logs_requests != "show" {
        let own = OWN_REQUESTS
            .map(|pattern| Expr::Like {
                field: "http.route".into(),
                pattern: pattern.into(),
            })
            .into_iter()
            .reduce(|a, b| Expr::Or(Box::new(a), Box::new(b)));
        if let Some(own) = own {
            combined = Some(and_also(combined.as_ref(), Expr::Not(Box::new(own))));
        }
    }
    if let Some(level) = Severity::from_name(&params.level) {
        combined = Some(and_also(
            combined.as_ref(),
            Expr::Compare {
                field: "level".into(),
                op: Op::Ge,
                value: QValue::Level(level),
            },
        ));
    }
    if !params.service.is_empty() {
        combined = Some(and_also(
            combined.as_ref(),
            Expr::Compare {
                field: "service".into(),
                op: Op::Eq,
                value: QValue::Text(params.service.clone()),
            },
        ));
    }
    Ok((user, combined))
}

fn query_error(params: &LogsParams, e: &ParseError) -> QueryErrorView {
    let chars: Vec<char> = params.q.chars().collect();
    let start = e.start.min(chars.len());
    let end = e.end.clamp(start, chars.len());
    let marked: String = if start == end {
        " ".into()
    } else {
        chars[start..end].iter().collect()
    };
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
        Value::Number(n) => n
            .as_i64()
            .map(QValue::Integer)
            .or_else(|| n.as_f64().map(QValue::Real)),
        Value::Bool(b) => Some(QValue::Bool(*b)),
        Value::Null => Some(QValue::Null),
        _ => None,
    }
}

/// Find and exclude links for `field = value`, added to the user's query.
fn find_links(
    params: &LogsParams,
    user: Option<&Expr>,
    field: &str,
    value: Option<QValue>,
) -> (Option<String>, Option<String>) {
    let Some(value) = value else {
        return (None, None);
    };
    let condition = Expr::Compare {
        field: field.to_string(),
        op: Op::Eq,
        value,
    };
    let with = |extra: Expr| {
        LogsParams {
            q: and_also(user, extra).to_string(),
            ..params.search_only()
        }
        .url(LOGS)
    };
    (
        Some(with(condition.clone())),
        Some(with(Expr::Not(Box::new(condition)))),
    )
}

/// Properties shown first when a line has them: who it was for.
const WHO: [&str; 3] = ["session.id", "user.id", "store.id"];

/// Names beside ids a person can't read: a user's email, a store's site
/// (monokulo's own stores; the engine's ids are its own).
async fn add_notes(state: &AppState, rows: &mut [RowView]) {
    use std::collections::HashMap;
    // Every id on the page, looked up in one read.
    let wanted: Vec<(bool, String)> = rows
        .iter()
        .flat_map(|row| {
            row.properties
                .iter()
                .filter_map(|property| match property.name.as_str() {
                    "user.id" => Some((true, property.value.clone())),
                    "store.id" if row.service == "monokulo" => {
                        Some((false, property.value.clone()))
                    }
                    _ => None,
                })
        })
        .collect();
    if wanted.is_empty() {
        return;
    }
    let notes: HashMap<(bool, String), String> = state
        .db
        .read(move |db| {
            let mut notes = HashMap::new();
            for (is_user, id) in wanted {
                let note = if is_user {
                    db.get_user_by_id(&crate::db::UserId::new(id.clone()))
                        .ok()
                        .flatten()
                        .map(|user| user.email)
                } else {
                    db.get_store_connection_by_id(&crate::db::ConnectionId::new(id.clone()))
                        .ok()
                        .flatten()
                        .map(|store| super::orders::display_name_for(&store.site_url))
                };
                if let Some(note) = note {
                    notes.insert((is_user, id), note);
                }
            }
            Ok::<_, crate::db::DbError>(notes)
        })
        .await
        .unwrap_or_default();
    for row in rows {
        let monokulo = row.service == "monokulo";
        for property in &mut row.properties {
            let key = match property.name.as_str() {
                "user.id" => (true, property.value.clone()),
                "store.id" if monokulo => (false, property.value.clone()),
                _ => continue,
            };
            property.note = notes.get(&key).cloned();
        }
    }
}

/// A line as shown. `lazy`: its properties load when it opens (lists),
/// rather than coming with the page.
fn row_view(
    row: &LogRow,
    params: &LogsParams,
    user: Option<&Expr>,
    zone: &jiff::tz::TimeZone,
    lazy: bool,
) -> RowView {
    let severity = row.severity();
    let mut properties = Vec::new();
    let mut push = |name: &str, shown: String, value: Option<QValue>| {
        let (find_url, exclude_url) = find_links(params, user, name, value);
        properties.push(PropertyView {
            name: name.to_string(),
            value: shown,
            find_url,
            exclude_url,
            note: None,
        });
    };
    push(
        "level",
        severity.name().to_string(),
        Some(QValue::Level(severity)),
    );
    push(
        "service",
        row.service.clone(),
        Some(QValue::Text(row.service.clone())),
    );
    // Who and what the line is about first, in the same place on every
    // line; then the rest by name.
    let pinned = |name: &str| WHO.iter().position(|who| *who == name).unwrap_or(WHO.len());
    let mut attributes: Vec<_> = row.attributes.iter().collect();
    attributes.sort_by_key(|(name, _)| pinned(name));
    for (name, value) in attributes {
        let shown = match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        push(name, shown, query_value(value));
    }
    if let Some(trace_id) = &row.trace_id {
        push(
            "trace_id",
            trace_id.clone(),
            Some(QValue::Text(trace_id.clone())),
        );
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
        session_url: row
            .attributes
            .get("session.id")
            .and_then(Value::as_str)
            .map(session_url),
        pos_session_url: row
            .attributes
            .get("pos.session")
            .and_then(Value::as_str)
            .filter(|s| super::pos_logs::is_session_id(s))
            .map(|s| format!("{LOGS}/pos/{s}")),
        properties_url: lazy.then(|| {
            params
                .search_only()
                .url(&format!("{LOGS}/row/{}", row.cursor().encode()))
        }),
        open: false,
        properties,
    }
}

/// Every line of one signed-in session, whenever it was.
fn session_url(session: &str) -> String {
    let condition = Expr::Compare {
        field: "session.id".into(),
        op: Op::Eq,
        value: QValue::Text(session.to_string()),
    };
    LogsParams {
        q: condition.to_string(),
        range: "all".into(),
        ..LogsParams::default()
    }
    .url(LOGS)
}

fn request(
    combined: Option<&Expr>,
    from: Option<i64>,
    to: Option<i64>,
    before: &str,
    after: &str,
    limit: u32,
) -> LogsRequest {
    LogsRequest {
        q: combined.map(Expr::to_string),
        from,
        to,
        before: Some(before.to_string()).filter(|c| !c.is_empty()),
        after: Some(after.to_string()).filter(|c| !c.is_empty()),
        limit: Some(limit),
    }
}

fn histogram_view(
    counts: &[u64],
    params: &LogsParams,
    from: i64,
    to: i64,
    zone: &jiff::tz::TimeZone,
) -> Option<HistogramView> {
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
    Some(HistogramView {
        bars,
        from_label: display_time(from, zone),
        to_label: display_time(to, zone),
    })
}

async fn build(
    state: &AppState,
    admin: &crate::db::UserRow,
    params: &LogsParams,
    tz: &Timezone,
) -> LogsViewModel {
    let (zone, _) = zone(tz, admin);
    let now = now_nanos();
    let range = time_range(params, &zone, now);
    let search = params.search_only();
    let sources = Sources::from_state(state).await;
    let admin_id = admin.id.clone();
    let saved = state
        .db
        .read(move |db| db.list_saved_log_searches(&admin_id))
        .await
        .unwrap_or_default();

    let mut vm = LogsViewModel {
        form: FormView {
            q: params.q.clone(),
            level: params.level.clone(),
            service: params.service.clone(),
            range: params.range().to_string(),
            from: params.from.clone(),
            to: params.to.clone(),
            logs_requests: params.logs_requests == "show",
        },
        query_error: None,
        problems: Vec::new(),
        rows: Vec::new(),
        histogram: None,
        older_url: None,
        more_url: None,
        newer_url: None,
        refresh_url: search.url(LOGS),
        tail_url: None,
        export_ndjson_url: LogsParams {
            format: "ndjson".into(),
            ..search.clone()
        }
        .url(&format!("{LOGS}/export")),
        export_csv_url: LogsParams {
            format: "csv".into(),
            ..search.clone()
        }
        .url(&format!("{LOGS}/export")),
        query_string: search.query_string(),
        saved,
        saved_error: None,
        attribute_names: Vec::new(),
    };

    let (from, to) = match range {
        Ok(range) => range,
        Err(problem) => {
            vm.problems.push(problem);
            vm.attribute_names = attribute_names(&sources).await;
            return vm;
        }
    };
    let (user, combined) = match filters(params) {
        Ok(filters) => filters,
        Err(e) => {
            vm.query_error = Some(query_error(params, &e));
            vm.attribute_names = attribute_names(&sources).await;
            return vm;
        }
    };
    let page_request = request(
        combined.as_ref(),
        from,
        to,
        &params.before,
        &params.after,
        PAGE_SIZE,
    );
    let histogram_from = from.unwrap_or(now - 14 * 86_400 * NANOS_PER_SECOND);
    let histogram_to = to.unwrap_or(now);
    let histogram_request = HistogramRequest {
        q: combined.as_ref().map(Expr::to_string),
        from: histogram_from,
        to: histogram_to,
        buckets: BARS,
    };
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
    vm.rows = page
        .rows
        .iter()
        .map(|row| row_view(row, params, user.as_ref(), &zone, true))
        .collect();
    if params.before.is_empty() && params.after.is_empty() {
        vm.histogram = histogram_view(&counts, params, histogram_from, histogram_to, &zone);
    }
    if page.rows.len() == PAGE_SIZE as usize {
        if let Some(last) = page.rows.last() {
            let older = LogsParams {
                before: last.cursor().encode(),
                ..search.clone()
            };
            vm.older_url = Some(older.url(LOGS));
            vm.more_url = Some(
                LogsParams {
                    part: "more".into(),
                    ..older
                }
                .url(LOGS),
            );
        }
    }
    if !params.before.is_empty() || !params.after.is_empty() {
        if let Some(first) = page.rows.first() {
            vm.newer_url = Some(
                LogsParams {
                    after: first.cursor().encode(),
                    ..search.clone()
                }
                .url(LOGS),
            );
        }
    }
    // Live starts after the newest line shown (or now), and only from the
    // newest page.
    if params.before.is_empty() && params.after.is_empty() {
        let start = page.rows.first().map(LogRow::cursor).unwrap_or(Cursor {
            ts: now,
            service: String::new(),
            id: 0,
        });
        vm.tail_url = Some(
            LogsParams {
                after: start.encode(),
                ..search
            }
            .url(&format!("{LOGS}/tail")),
        );
    }
    vm
}

async fn attribute_names(sources: &Sources) -> Vec<String> {
    let mut names: Vec<String> = [
        "level", "service", "target", "message", "trace_id", "span_id",
    ]
    .map(String::from)
    .to_vec();
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
        return Html(
            view::more_rows(&vm.rows, vm.more_url.as_deref(), vm.older_url.as_deref())
                .into_string(),
        )
        .into_response();
    }
    if fx.0 {
        return Html(view::results(&vm).into_string()).into_response();
    }
    let chrome = super::page_chrome(&state, Some(&admin), vm.refresh_url.clone()).await;
    Html(view::page(&chrome, &vm).into_string()).into_response()
}

/// `GET /dashboard/admin/logs/syntax`: the search language, as a page. The
/// Logs page opens the same help as a dialog when script runs.
pub async fn syntax_page(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
) -> Response {
    let names = attribute_names(&Sources::from_state(&state).await).await;
    let chrome = super::page_chrome(&state, Some(&admin), view::SYNTAX_PAGE).await;
    Html(view::syntax_page(&chrome, &names).into_string()).into_response()
}

/// `GET /dashboard/admin/logs/tail`: lines newer than `after` (or the
/// `Last-Event-ID` a reconnecting stream sends), as they arrive. Each event
/// carries rendered rows routed to the top of `#log-rows` (ssexi's JSON
/// event form), with the newest row's cursor as its id.
pub async fn tail(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    tz: Timezone,
    headers: HeaderMap,
    Query(params): Query<LogsParams>,
) -> Response {
    let (_, combined) = match filters(&params) {
        Ok(filters) => filters,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let resume = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(Cursor::parse);
    let cursor = resume
        .or_else(|| Cursor::parse(&params.after))
        .unwrap_or(Cursor {
            ts: now_nanos(),
            service: String::new(),
            id: 0,
        });
    let Some(slot) = TailSlot::take() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many Live views are open; close one and try again.",
        )
            .into_response();
    };
    let sources = Sources::from_state(&state).await;
    let changed = sources
        .local
        .as_ref()
        .map(telemetry::store::LogStore::subscribe);
    let (zone, _) = zone(&tz, &admin);
    let user = parse(&params.q).ok().flatten();

    // Local lines are read on every wake; the engine's at most once per
    // ENGINE_POLL however busy this server is, each from its own cursor so
    // neither skips the other's lines.
    struct Tail {
        local: Sources,
        engine: Option<Sources>,
        combined: Option<Expr>,
        cursor: Cursor,
        engine_cursor: Cursor,
        last_engine_read: Option<tokio::time::Instant>,
        engine_problem: Option<String>,
        changed: Option<tokio::sync::watch::Receiver<i64>>,
        first: bool,
        _slot: TailSlot,
    }
    let engine = match &sources.engine {
        crate::logs::EngineSource::Api { .. } => Some(Sources {
            local: None,
            engine: sources.engine.clone(),
        }),
        // Embedded, the local store holds the engine's lines too.
        crate::logs::EngineSource::Unavailable(_) | crate::logs::EngineSource::Local => None,
    };
    let state = Tail {
        local: Sources {
            local: sources.local.clone(),
            engine: crate::logs::EngineSource::Unavailable(String::new()),
        },
        engine,
        combined,
        engine_cursor: cursor.clone(),
        cursor,
        last_engine_read: None,
        engine_problem: None,
        changed,
        first: true,
        _slot: slot,
    };
    let stream = futures_util::stream::unfold(state, move |mut tail| {
        let (params, user, zone) = (params.clone(), user.clone(), zone.clone());
        async move {
            loop {
                if !tail.first {
                    // New local lines wake it at once; the engine's are
                    // polled.
                    match tail.changed.as_mut() {
                        Some(changed) => {
                            let _ = tokio::time::timeout(ENGINE_POLL, changed.changed()).await;
                        }
                        None => tokio::time::sleep(ENGINE_POLL).await,
                    }
                }
                tail.first = false;
                let mut rows = Vec::new();
                if tail.local.local.is_some() {
                    let request = request(
                        tail.combined.as_ref(),
                        None,
                        None,
                        "",
                        &tail.cursor.encode(),
                        200,
                    );
                    if let Ok(page) = crate::logs::read(&tail.local, &request).await {
                        if let Some(newest) = page.rows.first() {
                            tail.cursor = newest.cursor();
                        }
                        rows.extend(page.rows);
                    }
                }
                let mut problem_event = None;
                let engine_due = tail
                    .last_engine_read
                    .is_none_or(|at| at.elapsed() >= ENGINE_POLL);
                if let (Some(engine), true) = (&tail.engine, engine_due) {
                    tail.last_engine_read = Some(tokio::time::Instant::now());
                    let request = request(
                        tail.combined.as_ref(),
                        None,
                        None,
                        "",
                        &tail.engine_cursor.encode(),
                        200,
                    );
                    if let Ok(page) = crate::logs::read(engine, &request).await {
                        if let Some(newest) = page.rows.first() {
                            tail.engine_cursor = newest.cursor();
                        }
                        rows.extend(page.rows);
                        // Said once when the engine's lines stop (a 429
                        // from its rate limit, say), and cleared when they
                        // come back.
                        if page.engine_problem != tail.engine_problem {
                            tail.engine_problem = page.engine_problem;
                            problem_event = Some(
                                axum::response::sse::Event::default()
                                    .event(r##"{"target":"#log-tail-problem","swap":"innerHTML"}"##)
                                    .data(
                                        view::tail_problem(tail.engine_problem.as_deref())
                                            .into_string(),
                                    ),
                            );
                        }
                    }
                }
                rows.sort_by_key(|row| std::cmp::Reverse(row.cursor()));
                let mut events = Vec::new();
                events.extend(problem_event);
                if !rows.is_empty() {
                    let views: Vec<RowView> = rows
                        .iter()
                        .map(|row| row_view(row, &params, user.as_ref(), &zone, true))
                        .collect();
                    // The id resumes the local stream after a reconnect.
                    events.push(
                        axum::response::sse::Event::default()
                            .event(r##"{"target":"#log-rows","swap":"afterbegin"}"##)
                            .id(tail.cursor.clone().max(tail.engine_cursor.clone()).encode())
                            .data(view::live_rows(&views).into_string()),
                    );
                }
                if events.is_empty() {
                    continue;
                }
                return Some((events, tail));
            }
        }
    })
    .flat_map(|events| {
        futures_util::stream::iter(events.into_iter().map(Ok::<_, std::convert::Infallible>))
    });
    // A comment first, so the stream is open (and Live shows it) at once,
    // before there's a line to send.
    let opened = futures_util::stream::once(async {
        Ok(axum::response::sse::Event::default().comment("live"))
    });
    crate::live::sse(opened.chain(stream))
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
    let (zone, _) = zone(&tz, &admin);
    let sources = Sources::from_state(&state).await;
    let (trace, engine_problem) = crate::logs::trace(&sources, &trace_id).await;
    let search = LogsParams {
        q: format!("trace_id = '{trace_id}'"),
        range: "all".into(),
        ..LogsParams::default()
    };
    let user = parse(&search.q).ok().flatten();

    let start = trace.spans.iter().map(|s| s.start).min().unwrap_or(0);
    let end = trace
        .spans
        .iter()
        .map(|s| s.end)
        .max()
        .unwrap_or(start)
        .max(start + 1);
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
            name: span
                .attributes
                .get("otel.name")
                .and_then(Value::as_str)
                .unwrap_or(&span.name)
                .to_string(),
            service: span.service.clone(),
            depth: depth_of(span),
            left_pct: (span.start - start) as f64 * 100.0 / total,
            width_pct: ((span.end - span.start) as f64 * 100.0 / total).max(0.2),
            duration: format_duration(span.end - span.start),
            error: span.status == "error",
            attributes: span
                .attributes
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| v.to_string()),
                    )
                })
                .collect(),
        })
        .collect();
    let mut rows: Vec<RowView> = trace
        .logs
        .iter()
        .map(|row| row_view(row, &search, user.as_ref(), &zone, false))
        .collect();
    add_notes(&state, &mut rows).await;
    let vm = view::TraceViewModel {
        trace_id: trace_id.clone(),
        spans,
        rows,
        problems: engine_problem.into_iter().collect(),
        logs_url: search.url(LOGS),
    };
    let chrome = super::page_chrome(&state, Some(&admin), format!("{LOGS}/trace/{trace_id}")).await;
    Html(view::trace_page(&chrome, &vm).into_string()).into_response()
}

/// `GET /dashboard/admin/logs/row/{cursor}`: one line's properties. For
/// fixi (a line opening in a list), just the properties; otherwise a page
/// with the line opened. The search's parameters come along, so the Find
/// and Exclude links add to it.
pub async fn row_page(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    fx: FxRequest,
    tz: Timezone,
    Path(cursor): Path<String>,
    Query(params): Query<LogsParams>,
) -> Response {
    let (zone, _) = zone(&tz, &admin);
    let search = params.search_only();
    let back_url = search.url(LOGS);
    // An id at the very top of the range has no "just after": not found.
    let found = match Cursor::parse(&cursor).filter(|wanted| wanted.id.checked_add(1).is_some()) {
        // The first line before one just after it, in the order every
        // store pages in, is the line itself when it is still kept.
        Some(wanted) => {
            let just_after = Cursor {
                id: wanted.id + 1,
                ..wanted.clone()
            };
            let page = crate::logs::read(
                &Sources::from_state(&state).await,
                &request(None, None, None, &just_after.encode(), "", 1),
            )
            .await
            .ok();
            page.and_then(|page| page.rows.into_iter().next())
                .filter(|row| row.cursor() == wanted)
        }
        None => None,
    };
    let user = parse(&search.q).ok().flatten();
    let mut row = found.map(|row| RowView {
        open: true,
        ..row_view(&row, &search, user.as_ref(), &zone, false)
    });
    if let Some(row) = &mut row {
        add_notes(&state, std::slice::from_mut(row)).await;
    }
    if fx.0 {
        return match &row {
            Some(row) => Html(view::properties(row).into_string()).into_response(),
            None => (
                StatusCode::NOT_FOUND,
                Html(
                    r#"<div class="props"><p class="muted">This line is no longer kept.</p></div>"#,
                ),
            )
                .into_response(),
        };
    }
    let chrome = super::page_chrome(&state, Some(&admin), format!("{LOGS}/row/{cursor}")).await;
    let status = if row.is_some() {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    };
    (
        status,
        Html(view::row_page(&chrome, row.as_ref(), &back_url).into_string()),
    )
        .into_response()
}

/// Most events one POS timeline shows.
const TIMELINE_MAX: usize = 5_000;

/// "42 s", "4 min 12 s", "2 h 5 min".
fn human_duration(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    match seconds {
        0 => format!("{:.1} s", ms.max(0) as f64 / 1000.0),
        1..=59 => format!("{seconds} s"),
        60..=3599 => match seconds % 60 {
            0 => format!("{} min", seconds / 60),
            s => format!("{} min {s} s", seconds / 60),
        },
        _ => match (seconds % 3600) / 60 {
            0 => format!("{} h", seconds / 3600),
            m => format!("{} h {m} min", seconds / 3600),
        },
    }
}

/// Every line of one POS session, oldest page last, from monokulo's own
/// store (the engine never has them).
async fn session_rows(state: &AppState, session: &str) -> (Vec<LogRow>, Option<String>, bool) {
    let sources = Sources {
        local: state.log_store.clone(),
        engine: crate::logs::EngineSource::Unavailable(String::new()),
    };
    let q = format!("pos.session = '{session}'");
    let (mut rows, mut before) = (Vec::new(), String::new());
    loop {
        let page = match crate::logs::read(
            &sources,
            &request(
                parse(&q).ok().flatten().as_ref(),
                None,
                None,
                &before,
                "",
                telemetry::store::api::MAX_LIMIT,
            ),
        )
        .await
        {
            Ok(page) => page,
            Err(e) => return (rows, Some(e.to_string()), false),
        };
        if let Some(problem) = page.local_problem {
            return (rows, Some(problem), false);
        }
        let Some(last) = page.rows.last() else { break };
        before = last.cursor().encode();
        let full = page.rows.len() == telemetry::store::api::MAX_LIMIT as usize;
        rows.extend(page.rows);
        if rows.len() >= TIMELINE_MAX {
            return (rows, None, true);
        }
        if !full {
            break;
        }
    }
    (rows, None, false)
}

fn attr_i64(row: &LogRow, name: &str) -> Option<i64> {
    row.attributes.get(name).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

fn attr_str<'a>(row: &'a LogRow, name: &str) -> Option<&'a str> {
    row.attributes.get(name).and_then(Value::as_str)
}

/// `GET /dashboard/admin/logs/pos/{session}`.
pub async fn pos_timeline(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    tz: Timezone,
    Path(session): Path<String>,
) -> Response {
    if !super::pos_logs::is_session_id(&session) {
        return (StatusCode::NOT_FOUND, "No such POS session.").into_response();
    }
    let (zone, zone_label) = zone(&tz, &admin);
    let (mut rows, problem, truncated) = session_rows(&state, &session).await;
    rows.sort_by_key(|row| (attr_i64(row, "pos.seq").unwrap_or(i64::MAX), row.ts));
    // Delivery is at least once (a batch whose answer was lost is sent
    // again): one line per event.
    rows.dedup_by_key(|row| attr_i64(row, "pos.seq"));

    let store = rows
        .iter()
        .find_map(|row| attr_str(row, "store.id"))
        .map(str::to_string);
    let store_link = match &store {
        Some(id) => {
            let lookup = crate::db::ConnectionId::new(id.clone());
            let name = state
                .db
                .read(move |db| db.get_store_connection_by_id(&lookup))
                .await
                .ok()
                .flatten()
                .map(|row| super::orders::display_name_for(&row.site_url))
                .unwrap_or_else(|| id.clone());
            Some((name, format!("/dashboard/stores/{id}")))
        }
        None => None,
    };
    let device = rows
        .iter()
        .find(|row| attr_str(row, "pos.kind") == Some("pos.opened"))
        .and_then(|row| attr_str(row, "pos.detail"))
        .and_then(|detail| {
            super::pos_logs::detail_pairs(detail)
                .into_iter()
                .find(|(k, _)| k == "agent")
                .map(|(_, v)| v)
        });

    let (mut offline_ms, mut hidden_ms, mut stream_drops, mut orders_created, mut problems) =
        (0i64, 0i64, 0usize, 0usize, 0usize);
    let mut previous: Option<i64> = None;
    let mut entries = Vec::with_capacity(rows.len());
    for row in &rows {
        // The tablet's own clock, when it is a plausible one (milliseconds
        // since 1970, before 2286): the value is the client's, and one
        // out of range would overflow the arithmetic below.
        let client_ms = attr_i64(row, "pos.client_ts")
            .filter(|ms| (0..10_000_000_000_000).contains(ms))
            .unwrap_or(row.ts / 1_000_000);
        let kind = attr_str(row, "pos.kind").unwrap_or("unknown").to_string();
        let detail = attr_str(row, "pos.detail")
            .map(super::pos_logs::detail_pairs)
            .unwrap_or_default();
        let number = |name: &str| {
            detail
                .iter()
                .find(|(k, _)| k == name)
                .and_then(|(_, v)| v.parse::<i64>().ok())
        };
        let period = if let Some(ms) = number("offline_ms") {
            offline_ms += ms;
            Some(format!("offline for {}", human_duration(ms)))
        } else if let Some(ms) = number("hidden_ms") {
            hidden_ms += ms;
            Some(format!("hidden for {}", human_duration(ms)))
        } else {
            number("down_ms").map(|ms| format!("live updates down for {}", human_duration(ms)))
        };
        match kind.as_str() {
            "stream.error" => stream_drops += 1,
            "order.created" => orders_created += 1,
            _ => {}
        }
        let severity = row.severity();
        if severity >= Severity::Warn {
            problems += 1;
        }
        let since = previous.map(|p| client_ms.saturating_sub(p));
        let gap_before = since
            .filter(|ms| *ms >= 60_000)
            .map(|ms| format!("Nothing recorded for {}", human_duration(ms)));
        let late_ms = (row.ts / 1_000_000).saturating_sub(client_ms);
        let order = attr_str(row, "order.id").map(|order| {
            let href = match &store {
                Some(store) => format!("/dashboard/stores/{store}/orders/{order}"),
                None => format!(
                    "{LOGS}?q={}",
                    url::form_urlencoded::byte_serialize(
                        format!("order.id = '{order}'").as_bytes()
                    )
                    .collect::<String>()
                ),
            };
            (order.to_string(), href)
        });
        let shown_detail = detail
            .into_iter()
            .filter(|(k, _)| {
                !matches!(k.as_str(), "offline_ms" | "hidden_ms" | "down_ms" | "agent")
            })
            .collect();
        entries.push(view::TimelineEntryView {
            gap_before,
            time_display: display_time(client_ms * 1_000_000, &zone),
            time_iso: iso_time(client_ms * 1_000_000),
            since_previous: since
                .map(|ms| format!("+{}", human_duration(ms)))
                .unwrap_or_default(),
            severity,
            kind,
            order,
            period,
            detail: shown_detail,
            late: (late_ms >= 30_000).then(|| format!("sent {} later", human_duration(late_ms))),
        });
        previous = Some(client_ms);
    }
    let first = rows
        .first()
        .map(|row| attr_i64(row, "pos.client_ts").unwrap_or(row.ts / 1_000_000));
    let span = match (first, previous) {
        (Some(first), Some(last)) => human_duration(last - first),
        _ => String::new(),
    };
    let search = LogsParams {
        q: format!("pos.session = '{session}'"),
        range: "all".into(),
        ..LogsParams::default()
    };
    let vm = view::PosTimelineViewModel {
        session: session.clone(),
        store: store_link,
        device,
        summary: view::TimelineSummaryView {
            events: entries.len(),
            span,
            offline: human_duration(offline_ms),
            hidden: human_duration(hidden_ms),
            stream_drops,
            orders_created,
            problems,
        },
        entries,
        problems: problem.into_iter().collect(),
        truncated,
        logs_url: search.url(LOGS),
        zone_label,
    };
    let chrome = super::page_chrome(&state, Some(&admin), format!("{LOGS}/pos/{session}")).await;
    Html(view::pos_timeline_page(&chrome, &vm).into_string()).into_response()
}

#[derive(Deserialize)]
pub struct PosSessionForOrder {
    order: String,
}

/// `GET /dashboard/admin/logs/pos?order=...`: the POS session that created
/// an order, or a page saying there isn't one.
pub async fn pos_session_for_order(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    Query(query): Query<PosSessionForOrder>,
) -> Response {
    // Built as an expression, not as query text: no quoting rules to keep
    // in step with the query language's.
    let compare = |field: &str, value: String| Expr::Compare {
        field: field.into(),
        op: Op::Eq,
        value: QValue::Text(value),
    };
    let filter = Expr::And(
        Box::new(compare("order.id", query.order)),
        Box::new(compare("pos.kind", "order.created".to_string())),
    );
    let sources = Sources {
        local: state.log_store.clone(),
        engine: crate::logs::EngineSource::Unavailable(String::new()),
    };
    if let Ok(page) =
        crate::logs::read(&sources, &request(Some(&filter), None, None, "", "", 1)).await
    {
        if let Some(session) = page
            .rows
            .first()
            .and_then(|row| attr_str(row, "pos.session"))
            .filter(|s| super::pos_logs::is_session_id(s))
        {
            return axum::response::Redirect::to(&format!("{LOGS}/pos/{session}")).into_response();
        }
    }
    let chrome = super::page_chrome(&state, Some(&admin), format!("{LOGS}/pos")).await;
    let body = maud::html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" {
                a href="/" { "Dashboard" } " / " a href=(LOGS) { "Logs" }
            }
            h1 { "No POS session recorded" }
            p {
                "No POS session timeline mentions creating this order. Only orders taken on the POS of a store with "
                "Diagnostics turned on have one, and only until log retention deletes it."
            }
        }
    };
    (
        StatusCode::NOT_FOUND,
        crate::views::layout(&chrome, "No POS session - Monokulo", body),
    )
        .into_response()
}

fn format_duration(nanos: i64) -> String {
    let ms = nanos as f64 / 1_000_000.0;
    if ms < 1.0 {
        format!("{:.0} µs", ms * 1000.0)
    } else if ms < 1000.0 {
        format!("{ms:.1} ms")
    } else {
        format!("{:.2} s", ms / 1000.0)
    }
}

/// One CSV field. A value a spreadsheet would read as a formula (`=`, `+`,
/// `-`, `@`, a tab or a carriage return first) is prefixed with `'` and
/// quoted: log lines carry text from anyone (a URL, a header), and an
/// exported `=HYPERLINK(...)` must not run when the file is opened.
fn csv_field(text: &str) -> String {
    let formula = text.starts_with(['=', '+', '-', '@', '\t', '\r']);
    if formula {
        format!("\"'{}\"", text.replace('"', "\"\""))
    } else if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

/// `GET /dashboard/admin/logs/export?format=ndjson|csv`: this search's
/// lines, newest first, up to `EXPORT_MAX`.
pub async fn export(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    tz: Timezone,
    Query(params): Query<LogsParams>,
) -> Response {
    let (zone, _) = zone(&tz, &admin);
    let (from, to) = match time_range(&params, &zone, now_nanos()) {
        Ok(range) => range,
        Err(problem) => return (StatusCode::BAD_REQUEST, problem).into_response(),
    };
    let (_, combined) = match filters(&params) {
        Ok(filters) => filters,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let sources = Sources::from_state(&state).await;
    let mut rows: Vec<LogRow> = Vec::new();
    let mut before = String::new();
    while rows.len() < EXPORT_MAX {
        let page = match crate::logs::read(
            &sources,
            &request(combined.as_ref(), from, to, &before, "", 500),
        )
        .await
        {
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
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"monokulo-logs.{extension}\""),
            ),
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

async fn saved_fragment(
    state: &AppState,
    admin: &crate::db::UserRow,
    query_string: &str,
    error: Option<&str>,
) -> maud::Markup {
    let admin_id = admin.id.clone();
    let saved = state
        .db
        .read(move |db| db.list_saved_log_searches(&admin_id))
        .await
        .unwrap_or_default();
    view::saved_searches(&saved, query_string, error)
}

/// `POST /dashboard/admin/logs/saved`.
pub async fn save_search(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    fx: FxRequest,
    Form(form): Form<SaveSearchForm>,
) -> Response {
    let name = form.name.trim();
    // Only the page's own parameters are kept, whatever was posted.
    let query_string = serde_urlencoded::from_str::<LogsParams>(&form.query_string)
        .unwrap_or_default()
        .search_only()
        .query_string();
    let back = LogsParams::default().url(LOGS)
        + if query_string.is_empty() { "" } else { "?" }
        + &query_string;
    if name.is_empty() || name.chars().count() > 80 {
        let error = "Give the search a name of up to 80 characters.";
        return if fx.0 {
            super::fx::invalid(saved_fragment(&state, &admin, &query_string, Some(error)).await)
        } else {
            super::dashboard::redirect_302(&back)
        };
    }
    let (admin_id, name, saved_query) = (admin.id.clone(), name.to_string(), query_string.clone());
    let created = state
        .db
        .write(move |db| {
            db.create_saved_log_search(
                &uuid::Uuid::new_v4().to_string(),
                &admin_id,
                &name,
                &saved_query,
                crate::now_unix(),
                MAX_SAVED,
            )
        })
        .await;
    let error = match created {
        Ok(true) => None,
        Ok(false) => Some("You have 50 saved searches already; remove one first."),
        Err(e) => {
            tracing::error!(error = %e, "saving a log search failed");
            Some("The search couldn't be saved.")
        }
    };
    saved_response(
        fx,
        &back,
        saved_fragment(&state, &admin, &query_string, error),
    )
    .await
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
    let admin_id = admin.id.clone();
    let deleted = state
        .db
        .write(move |db| db.delete_saved_log_search(&admin_id, &id))
        .await;
    if let Err(e) = deleted {
        tracing::error!(error = %e, "removing a saved log search failed");
    }
    let query_string = serde_urlencoded::from_str::<LogsParams>(&form.query_string)
        .unwrap_or_default()
        .search_only()
        .query_string();
    let back = if query_string.is_empty() {
        LOGS.to_string()
    } else {
        format!("{LOGS}?{query_string}")
    };
    saved_response(
        fx,
        &back,
        saved_fragment(&state, &admin, &query_string, None),
    )
    .await
}

/// `fx::respond` for the saved-searches fragment, which is read from the
/// database, so only rendered (awaited) when fixi asked for it.
async fn saved_response(
    fx: FxRequest,
    back: &str,
    fragment: impl std::future::Future<Output = maud::Markup>,
) -> Response {
    if fx.0 {
        Html(fragment.await.into_string()).into_response()
    } else {
        super::dashboard::redirect_302(back)
    }
}

#[cfg(test)]
mod tests;
