//! The admin Query page's handlers (`views::query`): read-only SQL against
//! monokulo's own database or the engine's, admin only, and only while
//! `admin.query_enabled` is on (otherwise every route is a 404 and the nav
//! leaves the link out).
//!
//! - `GET /admin/query?db=&sql=`: the page; a `sql` runs. For
//!   fixi, only `#query-results`.
//! - `GET /admin/query/schema?db=`: the tables, the editor and
//!   the results for the other database (fixi swaps them in when the
//!   database changes; without JavaScript, the page).
//! - `GET /admin/query/value?db=&sql=&row=&col=`: one long value
//!   whole, on a page of its own, or with `part=dialog` the dialog's
//!   content.
//! - `GET /admin/query/export?db=&sql=&format=csv|json`: the
//!   rows as a download, up to 10,000.
//!
//! The statement runs through `sqlite::query` (on one of monokulo's
//! read connections, or the engine's): it can't change anything, and
//! secret columns come back hidden. One query per admin, and two in all,
//! run at once; past that, a 429. Every query is written to the logs as
//! an `admin.query` event, so the Logs page shows who ran what.

pub(crate) mod columns;

use std::collections::HashSet;
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use sqlite::query::{
    Cell, Limits, QueryError, QueryRequest, QueryResponse, QueryResult, Running, TableInfo,
};

use super::fx::FxRequest;
use super::{AppState, AuthedAdmin};
use crate::db::UserRow;
use crate::views::query::{self as view, Outcome, QueryDb, QueryViewModel};

/// Queries running at once across every admin.
const MAX_RUNNING: usize = 2;
static RUNNING: AtomicUsize = AtomicUsize::new(0);
/// The admins with a query running.
static ADMINS_RUNNING: Mutex<Option<HashSet<String>>> = Mutex::new(None);
/// How much of a statement the log keeps.
const LOGGED_SQL: usize = 2048;

#[derive(Deserialize, Default)]
pub struct QueryParams {
    #[serde(default)]
    db: String,
    #[serde(default)]
    sql: String,
    /// `value`: which cell.
    row: Option<usize>,
    col: Option<usize>,
    /// `value`: `dialog` for the dialog's content.
    #[serde(default)]
    part: String,
    /// `export`: `csv` or `json`.
    #[serde(default)]
    format: String,
}

/// One admin's query, counted against the limits while it lives.
struct Slot {
    _running: Running,
    admin: String,
}

impl Slot {
    fn take(admin: &UserRow) -> Option<Slot> {
        let mut admins = ADMINS_RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        let admins = admins.get_or_insert_with(HashSet::new);
        if admins.contains(admin.id.as_str()) {
            return None;
        }
        let running = Running::take(&RUNNING, MAX_RUNNING)?;
        admins.insert(admin.id.to_string());
        Some(Slot {
            _running: running,
            admin: admin.id.to_string(),
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(admins) = ADMINS_RUNNING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            admins.remove(&self.admin);
        }
    }
}

/// The page's routes are there only while the Query page is on.
fn switched_off(state: &AppState) -> Option<Response> {
    (!state.settings.query_enabled()).then(|| StatusCode::NOT_FOUND.into_response())
}

/// Runs `sql` on `db`, once a slot is free, and logs it.
async fn execute(
    state: &AppState,
    admin: &UserRow,
    db: QueryDb,
    sql: &str,
    limits: Limits,
) -> Outcome {
    let Some(slot) = Slot::take(admin) else {
        log(admin, db, sql, &Outcome::Busy);
        return Outcome::Busy;
    };
    let outcome =
        match db {
            QueryDb::Monokulo => {
                let statement = sql.to_string();
                match state
                    .db
                    .read(move |db| Ok::<_, crate::db::DbError>(db.admin_query(&statement, limits)))
                    .await
                {
                    Ok(Ok(result)) => Outcome::Ran(result),
                    Ok(Err(error)) => Outcome::Refused(error),
                    Err(e) => Outcome::Refused(QueryError::Failed {
                        message: e.to_string(),
                    }),
                }
            }
            QueryDb::Engine => {
                let request = QueryRequest {
                    sql: sql.to_string(),
                    limits,
                };
                match state.engine.client.admin_query(&request).await {
                    Ok(QueryResponse::Result(result)) => Outcome::Ran(result),
                    Ok(QueryResponse::Error(error)) => Outcome::Refused(error),
                    Err(crate::engine_client::EngineClientError::EngineError {
                        status, ..
                    }) if status == StatusCode::TOO_MANY_REQUESTS => Outcome::Busy,
                    Err(e) => Outcome::Unreachable(e.to_string()),
                }
            }
        };
    drop(slot);
    log(admin, db, sql, &outcome);
    outcome
}

/// The `admin.query` event: who ran what, and what came of it.
fn log(admin: &UserRow, db: QueryDb, sql: &str, outcome: &Outcome) {
    let mut end = sql.len().min(LOGGED_SQL);
    while !sql.is_char_boundary(end) {
        end -= 1;
    }
    let (name, rows, ms, cut, hidden) = match outcome {
        Outcome::Ran(result) => (
            "ran",
            result.rows.len(),
            result.elapsed_ms,
            result.cut,
            result.hidden.join(","),
        ),
        Outcome::Refused(QueryError::Refused { .. }) => ("refused", 0, 0, false, String::new()),
        Outcome::Refused(QueryError::Timeout { .. }) => ("timed_out", 0, 0, false, String::new()),
        Outcome::Refused(_) => ("failed", 0, 0, false, String::new()),
        Outcome::Busy => ("busy", 0, 0, false, String::new()),
        Outcome::Unreachable(_) => ("unreachable", 0, 0, false, String::new()),
        Outcome::Nothing => return,
    };
    tracing::info!(
        event = "admin.query",
        user.id = %admin.id,
        query.database = db.id(),
        query.sql = &sql[..end],
        query.outcome = name,
        query.rows = rows,
        query.ms = ms,
        query.cut = cut,
        query.hidden = %hidden,
        "an admin ran a query"
    );
}

/// The tables of `db`, and whether the engine answered.
async fn tables(state: &AppState, db: QueryDb) -> (Vec<TableInfo>, bool) {
    match db {
        QueryDb::Monokulo => (
            state
                .db
                .read(|db| db.admin_query_schema())
                .await
                .unwrap_or_default(),
            false,
        ),
        QueryDb::Engine => match state.engine.client.admin_query_schema().await {
            Ok(tables) => (tables, false),
            Err(_) => (Vec::new(), true),
        },
    }
}

async fn view_model(
    state: &AppState,
    admin: &UserRow,
    params: &QueryParams,
    run: bool,
) -> QueryViewModel {
    let db = QueryDb::from_id(&params.db);
    let (tables, engine_unreachable) = tables(state, db).await;
    let outcome = if engine_unreachable {
        Outcome::Unreachable(
            "Its tables couldn't be read. Pick Monokulo, or try again once the engine is back."
                .into(),
        )
    } else if run && !params.sql.trim().is_empty() {
        execute(state, admin, db, &params.sql, Limits::PAGE).await
    } else {
        Outcome::Nothing
    };
    QueryViewModel {
        db,
        sql: params.sql.clone(),
        tables,
        engine_unreachable,
        outcome,
    }
}

/// A full page or a fragment, with a 429 while another query runs (a
/// fragment says so with a 422, which fixi swaps in like a success).
fn answer(fx: FxRequest, busy: bool, html: maud::Markup) -> Response {
    let status = match (busy, fx.0) {
        (true, true) => StatusCode::UNPROCESSABLE_ENTITY,
        (true, false) => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::OK,
    };
    (status, Html(html.into_string())).into_response()
}

/// `GET /admin/query`.
pub async fn page(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    fx: FxRequest,
    Query(params): Query<QueryParams>,
) -> Response {
    if let Some(off) = switched_off(&state) {
        return off;
    }
    let vm = view_model(&state, &admin, &params, true).await;
    let busy = vm.outcome == Outcome::Busy;
    if fx.0 {
        return answer(fx, busy, view::results(&vm));
    }
    let current = view::href(vm.db, &vm.sql);
    let chrome = super::page_chrome(&state, Some(&admin), current).await;
    answer(fx, busy, view::page(&chrome, &vm))
}

/// `GET /admin/query/schema`: another database's tables, with an
/// empty editor.
pub async fn schema(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    fx: FxRequest,
    Query(params): Query<QueryParams>,
) -> Response {
    if let Some(off) = switched_off(&state) {
        return off;
    }
    let params = QueryParams {
        sql: String::new(),
        ..params
    };
    let vm = view_model(&state, &admin, &params, false).await;
    if fx.0 {
        return Html(view::workspace(&vm).into_string()).into_response();
    }
    let chrome = super::page_chrome(&state, Some(&admin), view::href(vm.db, "")).await;
    Html(view::page(&chrome, &vm).into_string()).into_response()
}

/// The value at `row`, `col` of `result`, and the first column of its row
/// (what the value is shown as belonging to).
fn cell_of(result: &QueryResult, row: usize, col: usize) -> Option<(&str, String, &Cell)> {
    let cells = result.rows.get(row)?;
    let cell = cells.get(col)?;
    let row_id = cells
        .first()
        .and_then(Cell::to_text)
        .unwrap_or_else(|| format!("row {}", row + 1));
    Some((result.columns.get(col)?.name.as_str(), row_id, cell))
}

/// `GET /admin/query/value`: runs the statement again and shows
/// one value whole.
pub async fn value(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    Query(params): Query<QueryParams>,
) -> Response {
    if let Some(off) = switched_off(&state) {
        return off;
    }
    let (Some(row), Some(col)) = (params.row, params.col) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let db = QueryDb::from_id(&params.db);
    let result = match execute(&state, &admin, db, &params.sql, Limits::PAGE).await {
        Outcome::Ran(result) => result,
        Outcome::Busy => {
            return (StatusCode::TOO_MANY_REQUESTS, "A query is already running.").into_response()
        }
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let Some((column, row_id, cell)) = cell_of(&result, row, col) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content = view::value(column, &row_id, cell);
    if params.part == "dialog" {
        return Html(view::value_dialog(content).into_string()).into_response();
    }
    let back = view::href(db, &params.sql);
    let chrome = super::page_chrome(&state, Some(&admin), back.clone()).await;
    Html(view::value_page(&chrome, &back, content).into_string()).into_response()
}

/// A CSV field, quoted when it has to be.
fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// `GET /admin/query/export?format=csv|json`.
pub async fn export(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    Query(params): Query<QueryParams>,
) -> Response {
    if let Some(off) = switched_off(&state) {
        return off;
    }
    let db = QueryDb::from_id(&params.db);
    let result = match execute(&state, &admin, db, &params.sql, Limits::EXPORT).await {
        Outcome::Ran(result) => result,
        Outcome::Busy => {
            return (StatusCode::TOO_MANY_REQUESTS, "A query is already running.").into_response()
        }
        Outcome::Refused(error) => {
            return (StatusCode::BAD_REQUEST, error.to_string()).into_response()
        }
        Outcome::Unreachable(why) => return (StatusCode::BAD_GATEWAY, why).into_response(),
        Outcome::Nothing => return StatusCode::BAD_REQUEST.into_response(),
    };
    let value = |column: usize, cell: &Cell| {
        if result.columns[column].hidden {
            None
        } else {
            cell.to_text()
        }
    };
    let (body, content_type, extension) = if params.format == "json" {
        let rows: Vec<serde_json::Value> = result
            .rows
            .iter()
            .map(|row| {
                serde_json::Value::Object(
                    row.iter()
                        .enumerate()
                        .map(|(i, cell)| {
                            let json = match cell {
                                _ if result.columns[i].hidden => serde_json::Value::Null,
                                Cell::Integer { value } => (*value).into(),
                                Cell::Real { value } => (*value).into(),
                                other => value(i, other).into(),
                            };
                            (result.columns[i].name.clone(), json)
                        })
                        .collect(),
                )
            })
            .collect();
        (
            serde_json::to_string_pretty(&rows).unwrap_or_default(),
            "application/json",
            "json",
        )
    } else {
        let mut out = result
            .columns
            .iter()
            .map(|c| csv_field(&c.name))
            .collect::<Vec<_>>()
            .join(",");
        out.push('\n');
        for row in &result.rows {
            let line = row
                .iter()
                .enumerate()
                .map(|(i, cell)| value(i, cell).map(|v| csv_field(&v)).unwrap_or_default())
                .collect::<Vec<_>>()
                .join(",");
            out.push_str(&line);
            out.push('\n');
        }
        (out, "text/csv; charset=utf-8", "csv")
    };
    (
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}-query.{extension}\"", db.id()),
            ),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests;
