//! The engine's log store, for monokulo's Logs page (structured_logging.md
//! 3.3). The engine token only, like the settings API: the engine never
//! pushes its logs anywhere, monokulo asks.
//!
//! - `GET /api/v1/admin/logs?q=&from=&to=&before=&after=&limit=`: lines,
//!   newest first (`telemetry::store::api::LogsRequest`).
//! - `GET /api/v1/admin/logs/trace/{trace_id}`: one trace's spans and lines.
//! - `GET /api/v1/admin/logs/histogram?q=&from=&to=&buckets=`
//! - `GET /api/v1/admin/logs/attributes`: recent attribute names.
//!
//! A query that doesn't parse is a `400` carrying the parser's error and
//! position (`QueryErrorResponse`), so monokulo can show it in place.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use telemetry::store::api::{
    is_trace_id, AttributesResponse, HistogramRequest, HistogramResponse, LogsRequest,
    LogsResponse, QueryErrorResponse,
};
use telemetry::store::{LogStore, StoreError};

use super::ApiError;

fn store(log_store: Option<LogStore>) -> Result<LogStore, ApiError> {
    log_store.ok_or_else(|| ApiError::Unavailable("this engine has no log store open".into()))
}

/// Runs a store read off the async threads.
async fn read<T: Send + 'static>(
    store: LogStore,
    read: impl FnOnce(&LogStore) -> Result<T, StoreError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(move || read(&store))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map_err(|e| ApiError::Internal(e.to_string()))
}

fn bad_query(e: telemetry::query::ParseError) -> Response {
    (StatusCode::BAD_REQUEST, Json(QueryErrorResponse::from(e))).into_response()
}

pub async fn list(
    State(log_store): State<Option<LogStore>>,
    Query(request): Query<LogsRequest>,
) -> Response {
    let query = match request.to_query() {
        Ok(query) => query,
        Err(e) => return bad_query(e),
    };
    let result = match store(log_store) {
        Ok(store) => read(store, move |s| s.query(&query)).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(rows) => Json(LogsResponse { rows }).into_response(),
        Err(e) => e.into_response(),
    }
}

pub async fn trace(
    State(log_store): State<Option<LogStore>>,
    Path(trace_id): Path<String>,
) -> Response {
    if !is_trace_id(&trace_id) {
        return ApiError::BadRequest("a trace id is 32 lowercase hex characters".into())
            .into_response();
    }
    let result = match store(log_store) {
        Ok(store) => read(store, move |s| s.trace(&trace_id)).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(trace) => Json(trace).into_response(),
        Err(e) => e.into_response(),
    }
}

pub async fn histogram(
    State(log_store): State<Option<LogStore>>,
    Query(request): Query<HistogramRequest>,
) -> Response {
    let filter = match telemetry::query::parse(request.q.as_deref().unwrap_or("")) {
        Ok(filter) => filter,
        Err(e) => return bad_query(e),
    };
    if request.from > request.to {
        return ApiError::BadRequest("from must not be after to".into()).into_response();
    }
    let result = match store(log_store) {
        Ok(store) => {
            read(store, move |s| {
                s.histogram(filter.as_ref(), request.from, request.to, request.buckets)
            })
            .await
        }
        Err(e) => Err(e),
    };
    match result {
        Ok(counts) => Json(HistogramResponse { counts }).into_response(),
        Err(e) => e.into_response(),
    }
}

pub async fn attributes(State(log_store): State<Option<LogStore>>) -> Response {
    let result = match store(log_store) {
        Ok(store) => read(store, |s| s.attribute_names()).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(names) => Json(AttributesResponse { names }).into_response(),
        Err(e) => e.into_response(),
    }
}
