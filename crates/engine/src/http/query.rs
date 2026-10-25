//! The engine's database, read for monokulo's admin Query page. The engine
//! token only, like the logs: the engine is private, and monokulo has
//! already checked the person is an admin.
//!
//! - `POST /api/v1/admin/query`: one read-only statement
//!   (`sqlite::query::QueryRequest`), on a read connection, with the
//!   engine's secret columns read as NULL (`store::query_columns`). A
//!   statement that is refused or fails is still a `200`, carrying why.
//! - `GET /api/v1/admin/query/schema`: the tables, their columns and rows.
//!
//! At most [`MAX_RUNNING`] statements run at once, whatever asks: past
//! that, a `429`.

use std::sync::atomic::AtomicUsize;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Json, Response};
use sqlite::query::{Limits, QueryRequest, QueryResponse, Running, SchemaResponse};

use super::ApiError;
use crate::store::Database;

/// Statements running at once: each holds one of the read connections for
/// up to its time limit.
const MAX_RUNNING: usize = 2;
static RUNNING: AtomicUsize = AtomicUsize::new(0);

fn busy() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({ "error": "A query is already running." })),
    )
        .into_response()
}

pub(super) async fn run(State(db): State<Database>, Json(request): Json<QueryRequest>) -> Response {
    let Some(running) = Running::take(&RUNNING, MAX_RUNNING) else {
        return busy();
    };
    let limits = request.limits.within(Limits::EXPORT);
    let sql = request.sql;
    let answer = db
        .read(move |store| Ok(store.admin_query(&sql, limits)))
        .await;
    drop(running);
    match answer {
        Ok(Ok(result)) => Json(QueryResponse::Result(result)).into_response(),
        Ok(Err(error)) => Json(QueryResponse::Error(error)).into_response(),
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

pub(super) async fn schema(State(db): State<Database>) -> Response {
    match db.read(crate::store::Store::admin_query_schema).await {
        Ok(tables) => Json(SchemaResponse { tables }).into_response(),
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use sqlite::query::{Cell, QueryError};
    use tower::ServiceExt as _;

    use super::*;
    use crate::http::{build_router, AppState, TEST_ENGINE_TOKEN};

    fn router() -> axum::Router {
        build_router(AppState::for_tests(), 1024 * 1024)
    }

    async fn ask(router: axum::Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    fn query(sql: &str) -> Request<Body> {
        Request::post("/api/v1/admin/query")
            .header(shared::auth::ENGINE_TOKEN_HEADER, TEST_ENGINE_TOKEN)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&QueryRequest {
                    sql: sql.into(),
                    limits: Limits::PAGE,
                })
                .unwrap(),
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn a_select_answers_with_rows_and_a_write_with_why_not() {
        let (status, body) = ask(
            router(),
            query("SELECT version FROM schema_migrations ORDER BY version LIMIT 2"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let QueryResponse::Result(result) = serde_json::from_value(body).unwrap() else {
            panic!("a result")
        };
        assert_eq!(result.rows[0], [Cell::Integer { value: 1 }]);

        let (status, body) = ask(router(), query("DELETE FROM orders")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_value::<QueryResponse>(body).unwrap(),
            QueryResponse::Error(QueryError::Refused {
                action: "DELETE".into()
            })
        );
    }

    #[tokio::test]
    async fn the_schema_lists_the_engines_tables_with_secrets_hidden() {
        let request = Request::get("/api/v1/admin/query/schema")
            .header(shared::auth::ENGINE_TOKEN_HEADER, TEST_ENGINE_TOKEN)
            .body(Body::empty())
            .unwrap();
        let (status, body) = ask(router(), request).await;
        assert_eq!(status, StatusCode::OK);
        let schema: SchemaResponse = serde_json::from_value(body).unwrap();
        let tenants = schema.tables.iter().find(|t| t.name == "tenants").unwrap();
        let hidden: Vec<&str> = tenants
            .columns
            .iter()
            .filter(|c| c.hidden)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(hidden, ["secret_token_hash", "sealed_key_material"]);
    }

    #[tokio::test]
    async fn without_the_engine_token_nothing_is_read() {
        let request = Request::post("/api/v1/admin/query")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"sql":"SELECT 1","limits":{"max_rows":1,"max_cell_bytes":1,"max_result_bytes":1,"timeout":1}}"#))
            .unwrap();
        let (status, _) = ask(router(), request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// Every column of the engine's tables is classified, and nothing
    /// classified is gone: a migration that adds a column fails here until
    /// someone decides whether the Query page may show it.
    #[test]
    fn every_column_is_classified() {
        let store = crate::store::Store::open_in_memory().unwrap();
        let (missing, stale) = sqlite::query::unclassified(
            store.conn_for_test(),
            &crate::store::query_columns::COLUMNS,
        )
        .unwrap();
        assert!(
            missing.is_empty(),
            "classify these in store/query_columns.rs: {missing:?}"
        );
        assert!(
            stale.is_empty(),
            "these are gone, remove them from store/query_columns.rs: {stale:?}"
        );
    }
}
