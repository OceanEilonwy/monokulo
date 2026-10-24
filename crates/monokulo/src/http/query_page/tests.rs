//! The Query page as an admin uses it, in-process through the router; one
//! test reads a real engine over HTTP.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::ServiceExt as _;
use tracing::instrument::WithSubscriber as _;

use super::*;
use crate::http::test_support::{
    admin_session_cookie, body_text, signed_up_and_logged_in_session_token, urlencoding_encode,
};
use crate::http::{build_router, AppState};

fn get(uri: &str, cookie: &str) -> Request<Body> {
    Request::get(uri)
        .header("cookie", cookie)
        .header("accept", "text/html")
        .body(Body::empty())
        .unwrap()
}

fn query(db: &str, sql: &str) -> String {
    format!("/admin/query?db={db}&sql={}", urlencoding_encode(sql))
}

async fn admin(state: AppState) -> (Router, String) {
    let router = build_router(state);
    let cookie = admin_session_cookie(&router).await;
    (router, cookie)
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, String) {
    let response = router.clone().oneshot(request).await.unwrap();
    (response.status(), body_text(response).await)
}

/// Every column of monokulo's tables is classified, and nothing classified
/// is gone: a migration that adds a column fails here until someone
/// decides whether the Query page may show it.
#[test]
fn every_column_is_classified() {
    let db = crate::db::Db::open_in_memory().unwrap();
    let (missing, stale) =
        shared::sqlite_query::unclassified(db.conn_for_test(), &columns::COLUMNS).unwrap();
    assert!(
        missing.is_empty(),
        "classify these in http/query_page/columns.rs: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "these are gone, remove them from http/query_page/columns.rs: {stale:?}"
    );
}

#[tokio::test]
async fn an_admin_reads_monokulos_database_with_secrets_hidden() {
    let (router, cookie) = admin(AppState::for_tests()).await;
    let (status, html) = send(
        &router,
        get(
            &query("monokulo", "SELECT id, email, password_hash FROM users"),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("<title>Query - Monokulo</title>"), "{html}");
    assert!(html.contains(crate::db::TEST_ADMIN_EMAIL), "{html}");
    assert!(!html.contains("$argon2"), "a password hash shown: {html}");
    assert!(html.contains(r#"data-kind="hidden">hidden</td>"#), "{html}");
    assert!(html.contains("1 column hidden"), "{html}");
    // The tables beside the editor, secret columns marked.
    assert!(html.contains(r#"data-table="users""#), "{html}");
    // A wide page, the nav lined up with it, and the nav's Query link.
    assert!(html.contains("wrap query-page wrap-wide"), "{html}");
    assert!(
        html.contains(r#"<nav class="site-nav" data-width="full">"#),
        "{html}"
    );
    assert!(
        html.contains(r#"<a href="/admin/query">Query</a>"#),
        "{html}"
    );
}

#[tokio::test]
async fn a_write_is_refused_and_changes_nothing() {
    let (router, cookie) = admin(AppState::for_tests()).await;
    let (status, html) = send(
        &router,
        get(
            &query("monokulo", "UPDATE users SET email = 'x@example.org'"),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Not run: this page only reads."), "{html}");
    assert!(html.contains("<code>UPDATE</code>"), "{html}");
    let (_, html) = send(
        &router,
        get(&query("monokulo", "SELECT email FROM users"), &cookie),
    )
    .await;
    assert!(html.contains(crate::db::TEST_ADMIN_EMAIL), "{html}");
}

#[tokio::test]
async fn the_rows_download_as_csv_and_json() {
    let (router, cookie) = admin(AppState::for_tests()).await;
    let sql = urlencoding_encode("SELECT email, password_hash, 'a,b' AS odd FROM users");
    let response = router
        .clone()
        .oneshot(get(
            &format!("/admin/query/export?db=monokulo&sql={sql}&format=csv"),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=\"monokulo-query.csv\""
    );
    let csv = body_text(response).await;
    assert_eq!(
        csv,
        format!(
            "email,password_hash,odd\n{},,\"a,b\"\n",
            crate::db::TEST_ADMIN_EMAIL
        )
    );

    let (status, json) = send(
        &router,
        get(
            &format!("/admin/query/export?db=monokulo&sql={sql}&format=json"),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(rows[0]["email"], crate::db::TEST_ADMIN_EMAIL);
    assert_eq!(rows[0]["password_hash"], serde_json::Value::Null);
}

/// Each of the Query page's routes checks for itself: an admin's read
/// runs, a merchant is refused, a write never runs, and switched off the
/// route is gone.
#[tokio::test]
async fn every_query_route_reads_for_an_admin_only_and_never_writes() {
    let read = urlencoding_encode("SELECT email FROM users");
    let write = urlencoding_encode("UPDATE users SET email = 'x@example.org'");
    // Each route, and what it answers an admin's write.
    let routes = [
        ("/admin/query?db=monokulo", StatusCode::OK),
        ("/admin/query/schema?db=monokulo", StatusCode::OK),
        (
            "/admin/query/value?db=monokulo&row=0&col=0",
            StatusCode::NOT_FOUND,
        ),
        (
            "/admin/query/export?db=monokulo&format=csv",
            StatusCode::BAD_REQUEST,
        ),
    ];
    let state = AppState::for_tests();
    let (router, cookie) = admin(state.clone()).await;
    let merchant = format!(
        "session={}",
        signed_up_and_logged_in_session_token(&router, "merchant@example.org", "a long password")
            .await
    );
    let off = AppState::for_tests().with_options("").await;
    off.save_setting("admin.query_enabled", "false").await;
    let (off, off_cookie) = admin(off).await;
    for (route, written) in routes {
        let (status, body) = send(&router, get(&format!("{route}&sql={read}"), &cookie)).await;
        assert_eq!(status, StatusCode::OK, "{route}");
        if !route.contains("schema") {
            assert!(
                body.contains(crate::db::TEST_ADMIN_EMAIL),
                "{route}: {body}"
            );
        }
        let (status, _) = send(&router, get(&format!("{route}&sql={read}"), &merchant)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{route}");
        let (status, _) = send(&router, get(&format!("{route}&sql={write}"), &cookie)).await;
        assert_eq!(status, written, "{route}");
        let (_, emails) = send(
            &router,
            get(
                &format!("/admin/query/export?db=monokulo&format=csv&sql={read}"),
                &cookie,
            ),
        )
        .await;
        assert!(!emails.contains("x@example.org"), "{route} ran a write");
        let (status, _) = send(&off, get(&format!("{route}&sql={read}"), &off_cookie)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{route}");
    }
    // A value outside the result isn't there.
    let (status, _) = send(
        &router,
        get(
            &format!("/admin/query/value?db=monokulo&row=5&col=0&sql={read}"),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn only_an_admin_gets_the_page() {
    let state = AppState::for_tests();
    let router = build_router(state);
    let session =
        signed_up_and_logged_in_session_token(&router, "merchant@example.org", "a long password")
            .await;
    let response = router
        .clone()
        .oneshot(
            Request::get("/admin/query")
                .header("cookie", format!("session={session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = router
        .clone()
        .oneshot(
            Request::get("/admin/query?db=monokulo")
                .header("accept", "text/html")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers()["location"],
        "/login?next=%2Fadmin%2Fquery%3Fdb%3Dmonokulo"
    );
}

#[tokio::test]
async fn while_a_query_runs_the_next_is_refused_with_a_429() {
    let state = AppState::for_tests();
    let admin = state
        .db
        .read(|db| db.get_user_by_email(crate::db::TEST_ADMIN_EMAIL))
        .await
        .unwrap()
        .unwrap();
    let (router, cookie) = self::admin(state).await;
    let held = Slot::take(&admin).expect("nothing running yet");
    let (status, html) = send(&router, get(&query("monokulo", "SELECT 1"), &cookie)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(html.contains("A query is already running."), "{html}");
    let mut request = get(&query("monokulo", "SELECT 1"), &cookie);
    request
        .headers_mut()
        .insert("fx-request", "true".parse().unwrap());
    let (status, _) = send(&router, request).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "swapped like a success"
    );
    drop(held);
    let (status, _) = send(&router, get(&query("monokulo", "SELECT 1"), &cookie)).await;
    assert_eq!(status, StatusCode::OK);
}

/// What a test subscriber wrote.
#[derive(Clone, Default)]
struct Written(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Written {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn every_query_is_logged_with_who_ran_it() {
    let written = Written::default();
    let writer = written.clone();
    let (_, subscriber) = telemetry::build(
        "monokulo",
        telemetry::Format::Json,
        false,
        "info",
        move || writer.clone(),
    );
    let dispatch = tracing::Dispatch::new(subscriber);
    let router = build_router(AppState::for_tests());
    let cookie = admin_session_cookie(&router)
        .with_subscriber(dispatch.clone())
        .await;
    router
        .clone()
        .oneshot(get(
            &query("monokulo", "SELECT password_hash FROM users"),
            &cookie,
        ))
        .with_subscriber(dispatch)
        .await
        .unwrap();
    let lines = String::from_utf8(written.0.lock().unwrap().clone()).unwrap();
    let line = lines
        .lines()
        .find(|line| line.contains("admin.query"))
        .unwrap_or_else(|| panic!("no admin.query event in {lines}"));
    for part in [
        "\"query.database\":\"monokulo\"",
        "\"query.sql\":\"SELECT password_hash FROM users\"",
        "\"query.outcome\":\"ran\"",
        "\"query.hidden\":\"users.password_hash\"",
        "\"user.id\"",
    ] {
        assert!(line.contains(part), "{part} in {line}");
    }
}

#[tokio::test]
async fn switched_off_the_page_is_gone_and_so_is_its_link() {
    let state = AppState::for_tests().with_options("").await;
    state.save_setting("admin.query_enabled", "false").await;
    let (router, cookie) = admin(state).await;
    let (status, _) = send(&router, get(&query("monokulo", "SELECT 1"), &cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, html) = send(&router, get("/admin/logs", &cookie)).await;
    assert!(!html.contains("/admin/query"), "{html}");
}

#[tokio::test]
async fn the_engines_database_is_read_over_http() {
    let engine = engine_test_support::TestEngineConfig::new()
        .with_networks(&[monero::Network::Mainnet])
        .spawn()
        .await;
    let state = AppState {
        engine: crate::http::Engine::new(crate::engine_client::EngineClient::for_tests(format!(
            "http://{}",
            engine.addr
        ))),
        ..AppState::for_tests()
    };
    let (router, cookie) = admin(state).await;
    let (status, html) = send(
        &router,
        get(
            &query(
                "engine",
                "SELECT id, secret_token_hash FROM tenants LIMIT 1",
            ),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(" ms · engine"), "{html}");
    assert!(html.contains(r#"data-table="tenants""#), "{html}");
    assert!(
        html.contains(r#"<option value="engine" selected"#),
        "{html}"
    );
}

#[tokio::test]
async fn an_unreachable_engine_says_so() {
    let (router, cookie) = admin(AppState::for_tests()).await;
    let (status, html) = send(&router, get(&query("engine", "SELECT 1"), &cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("The engine can't be reached."), "{html}");
    assert!(html.contains("unreachable"), "{html}");
}
