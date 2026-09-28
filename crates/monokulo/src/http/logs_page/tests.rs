use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::db::{Db, TEST_ADMIN_EMAIL, TEST_ADMIN_PASSWORD};
use crate::engine_client::EngineClient;
use crate::http::{build_router, AppState};

struct Setup {
    router: Router,
    state: AppState,
    cookie: String,
    trace_id: String,
    /// Logs into the page's store.
    dispatch: tracing::Dispatch,
    _dir: TempDir,
}

struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A monokulo whose own log store holds `extra + 3` lines: two in one
/// request's trace (an info with `order.id = o_page`, a warning), one error
/// outside it, and `extra` numbered filler lines older than all three.
async fn setup(extra: usize) -> Setup {
    let dir = TempDir(std::env::temp_dir().join(format!("logs-page-{}-{}", std::process::id(), uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&dir.0).unwrap();
    let (telemetry, subscriber) = telemetry::build("monokulo", telemetry::Format::Json, false, "info", std::io::sink);
    let store = telemetry.open_store(&dir.0.join("monokulo.logs.db")).unwrap();
    let dispatch = tracing::Dispatch::new(subscriber);
    let trace_id = tracing::dispatcher::with_default(&dispatch, || {
        for n in 0..extra {
            tracing::info!(n, "filler line {n}");
        }
        let request = tracing::info_span!("logs page test request", http.route = "/pay");
        request.in_scope(|| {
            tracing::info!(order.id = "o_page", attempts = 2, "payment seen for the page test");
            tracing::warn!("the page test is slow");
        });
        tracing::error!(store.id = "s_page", "the page test failed");
        telemetry::trace::span_context(&request).unwrap().trace_id().to_string()
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while store.query(&telemetry::store::LogQuery { limit: 1000, ..Default::default() }).unwrap().len() < extra + 3 {
        assert!(std::time::Instant::now() < deadline, "the lines were never stored");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let db = Db::open_in_memory().unwrap();
    db.seed_test_admin();
    let state = AppState {
        db: db.into_shared(),
        engine_client: EngineClient::new("http://127.0.0.1:1"),
        encryption_key: [7u8; 32],
        status_cache: crate::http::status_page::new_status_cache(),
        exchange_rate: std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::xmr_only()),
        abuse: Default::default(),
        dns: std::sync::Arc::new(crate::embed_domains::UnavailableDns("DNS is not available in tests".to_string())),
        settings: crate::settings::MonokuloSettings::defaults(),
        log_store: Some(store),
    };
    let router = build_router(state.clone());
    let login = form("/dashboard/login", &[("email", TEST_ADMIN_EMAIL), ("password", TEST_ADMIN_PASSWORD)], None);
    let response = router.clone().oneshot(login).await.unwrap();
    let cookie = response.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
    Setup { router, state, cookie, trace_id, dispatch, _dir: dir }
}

fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn form(uri: &str, fields: &[(&str, &str)], cookie: Option<&str>) -> Request<Body> {
    let body = fields.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect::<Vec<_>>().join("&");
    let mut builder = Request::post(uri).header("content-type", "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    builder.body(Body::from(body)).unwrap()
}

impl Setup {
    async fn get(&self, uri: &str, fixi: bool) -> (StatusCode, axum::http::HeaderMap, String) {
        let mut builder = Request::get(uri).header("cookie", &self.cookie);
        if fixi {
            builder = builder.header("FX-Request", "true");
        }
        let response = self.router.clone().oneshot(builder.body(Body::empty()).unwrap()).await.unwrap();
        let (status, headers) = (response.status(), response.headers().clone());
        let body = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        (status, headers, body)
    }
}

fn logs_url(q: &str, extra: &str) -> String {
    format!("/dashboard/admin/logs?q={}{extra}", encode(q))
}

#[tokio::test]
async fn the_page_works_without_javascript_and_says_why_engine_lines_are_missing() {
    let s = setup(0).await;
    let (status, _, html) = s.get("/dashboard/admin/logs", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(r#"<form id="log-search" class="logs-search" method="get" action="/dashboard/admin/logs""#), "{html}");
    for message in ["payment seen for the page test", "the page test is slow", "the page test failed"] {
        assert!(html.contains(message), "{message}: {html}");
    }
    assert!(html.find("the page test failed").unwrap() < html.find("payment seen for the page test").unwrap(), "newest first");
    assert!(html.contains("set the engine admin token"), "{html}");
    assert!(html.contains(r#"class="log-histogram""#), "{html}");
    assert!(html.contains(">Refresh</a>"), "a Refresh link, never a refresh by itself");
    assert!(!html.contains("http-equiv=\"refresh\""));
    // Expanded properties, with links narrowing or widening the search.
    assert!(html.contains(&format!(r#"href="{}""#, logs_url("order.id = 'o_page'", "").replace('&', "&amp;"))), "{html}");
    assert!(html.contains(&format!(r#"href="{}""#, logs_url("not order.id = 'o_page'", ""))), "{html}");
    assert!(html.contains(&format!("/dashboard/admin/logs/trace/{}", s.trace_id)));
    assert!(html.contains("<code>order.id</code>"), "property names listed in the help: {html}");
}

#[tokio::test]
async fn a_search_filters_and_a_fixi_search_gets_only_the_results() {
    let s = setup(0).await;
    let (_, _, html) = s.get(&logs_url("order.id = 'o_page' or level >= error", ""), false).await;
    assert!(html.contains("payment seen for the page test") && html.contains("the page test failed"));
    assert!(!html.contains("the page test is slow"), "{html}");

    let (_, _, html) = s.get("/dashboard/admin/logs?level=warn", false).await;
    assert!(!html.contains("payment seen for the page test") && html.contains("the page test is slow"));

    let (status, _, fragment) = s.get(&logs_url("'slow'", ""), true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(fragment.starts_with(r#"<div id="log-results">"#), "{fragment}");
    assert!(fragment.contains("the page test is slow") && !fragment.contains("the page test failed"));
    assert!(!fragment.contains("<html"), "a fragment, not a page");
}

#[tokio::test]
async fn a_query_that_does_not_parse_is_shown_with_the_problem_marked() {
    let s = setup(0).await;
    let (status, _, html) = s.get(&logs_url("level = loud", ""), false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("a level is one of"), "{html}");
    assert!(html.contains("<code>level = <mark>loud</mark></code>"), "{html}");
    assert!(html.contains(r#"value="level = loud""#), "the query stays in the box to fix");
}

#[tokio::test]
async fn older_pages_follow_as_links_or_appended_by_fixi() {
    let s = setup(100).await;
    let (_, _, html) = s.get("/dashboard/admin/logs", false).await;
    assert_eq!(html.matches(r#"<details class="log-row""#).count(), 100);
    let older = html.split(r#"<a class="btn btn-secondary" href=""#).find(|part| part.contains("before=")).expect("an Older link");
    let older = older.split('"').next().unwrap().replace("&amp;", "&");
    let (_, _, page_two) = s.get(&older, false).await;
    assert_eq!(page_two.matches(r#"<details class="log-row""#).count(), 3);
    assert!(page_two.contains("filler line 0") && page_two.contains(">Newer</a>"));

    let (_, _, more) = s.get(&format!("{older}&part=more"), true).await;
    assert!(more.starts_with(r#"<details class="log-row""#), "{more}");
    assert!(more.ends_with(r#"<div id="log-more"></div>"#), "the last page has no Older: {more}");
}

#[tokio::test]
async fn a_trace_shows_its_spans_and_lines() {
    let s = setup(0).await;
    let (status, _, html) = s.get(&format!("/dashboard/admin/logs/trace/{}", s.trace_id), false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("logs page test request"), "the span: {html}");
    assert!(html.contains(r#"class="bar" style="left:0.00%;width:100.00%""#), "{html}");
    assert!(html.contains("payment seen for the page test") && html.contains("the page test is slow"));
    assert!(!html.contains("the page test failed"), "not in this trace");
    let (status, _, _) = s.get("/dashboard/admin/logs/trace/not-a-trace", false).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_search_downloads_as_ndjson_or_csv() {
    let s = setup(0).await;
    let (status, headers, ndjson) = s.get("/dashboard/admin/logs/export?format=ndjson&level=warn", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers["content-disposition"].to_str().unwrap().contains("monokulo-logs.ndjson"));
    let lines: Vec<serde_json::Value> = ndjson.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["message"], "the page test failed");
    assert_eq!(lines[0]["attributes"]["store.id"], "s_page");

    let (_, _, csv) = s.get(&format!("/dashboard/admin/logs/export?format=csv&q={}", encode("order.id = 'o_page'")), false).await;
    let rows: Vec<&str> = csv.lines().collect();
    assert_eq!(rows[0], "time,level,service,target,message,trace_id,attributes");
    assert_eq!(rows.len(), 2);
    assert!(rows[1].contains(r#""{""attempts"":2,""http.route"":""/pay"",""order.id"":""o_page""}""#), "{}", rows[1]);
}

#[tokio::test]
async fn searches_are_saved_and_removed_with_or_without_fixi() {
    let s = setup(0).await;
    let saved = form("/dashboard/admin/logs/saved", &[("name", "Slow"), ("query_string", "q=%27slow%27&level=warn&before=1.2.x")], Some(&s.cookie));
    let response = s.router.clone().oneshot(saved).await.unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(response.headers()["location"], "/dashboard/admin/logs?q=%27slow%27&level=warn", "paging isn't saved");
    let (_, _, html) = s.get("/dashboard/admin/logs", false).await;
    assert!(html.contains(r#"<a href="/dashboard/admin/logs?q=%27slow%27&amp;level=warn">Slow</a>"#), "{html}");

    let mut fixi = form("/dashboard/admin/logs/saved", &[("name", ""), ("query_string", "")], Some(&s.cookie));
    fixi.headers_mut().insert("FX-Request", "true".parse().unwrap());
    let response = s.router.clone().oneshot(fixi).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let fragment = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(fragment.starts_with(r#"<section id="saved-searches""#) && fragment.contains("Give the search a name"), "{fragment}");

    let admin_id = s.state.db.lock().get_user_by_email(TEST_ADMIN_EMAIL).unwrap().unwrap().id;
    let id = s.state.db.lock().list_saved_log_searches(&admin_id).unwrap()[0].id.clone();
    let mut remove = form(&format!("/dashboard/admin/logs/saved/{id}/delete"), &[], Some(&s.cookie));
    remove.headers_mut().insert("FX-Request", "true".parse().unwrap());
    let response = s.router.clone().oneshot(remove).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(s.state.db.lock().list_saved_log_searches(&admin_id).unwrap().is_empty());
}

#[tokio::test]
async fn only_admins_see_the_logs() {
    let s = setup(0).await;
    let response = s.router.clone().oneshot(Request::get("/dashboard/admin/logs").body(Body::empty()).unwrap()).await.unwrap();
    assert_ne!(response.status(), StatusCode::OK);
    let signup = form("/dashboard/signup", &[("email", "someone@example.com"), ("password", "correct horse battery staple")], None);
    s.router.clone().oneshot(signup).await.unwrap();
    let login = form("/dashboard/login", &[("email", "someone@example.com"), ("password", "correct horse battery staple")], None);
    let response = s.router.clone().oneshot(login).await.unwrap();
    let cookie = response.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
    for uri in ["/dashboard/admin/logs", "/dashboard/admin/logs/export", "/dashboard/admin/logs/tail"] {
        let response = s.router.clone().oneshot(Request::get(uri).header("cookie", &cookie).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
    }
}

#[tokio::test]
async fn live_streams_new_lines_to_the_top_of_the_list() {
    let s = setup(0).await;
    // After the error line: only lines newer than it come.
    let (_, _, html) = s.get("/dashboard/admin/logs", false).await;
    let tail = html.split(r#"fx-action=""#).find(|part| part.starts_with("/dashboard/admin/logs/tail")).unwrap();
    let tail = tail.split('"').next().unwrap().replace("&amp;", "&");
    assert!(tail.contains("after="), "{tail}");
    let response = s
        .router
        .clone()
        .oneshot(Request::get(&tail).header("cookie", &s.cookie).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut body = response.into_body();
    let first = tokio::time::timeout(std::time::Duration::from_millis(300), body.frame()).await;
    assert!(first.is_err(), "nothing newer than the newest line yet");

    tracing::dispatcher::with_default(&s.dispatch, || tracing::warn!(order.id = "o_live", "a live line for the page test"));
    let mut text = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !text.contains("\n\n") {
        assert!(std::time::Instant::now() < deadline, "no event came: {text}");
        if let Ok(Some(Ok(frame))) = tokio::time::timeout(std::time::Duration::from_secs(5), body.frame()).await {
            if let Some(data) = frame.data_ref() {
                text.push_str(std::str::from_utf8(data).unwrap());
            }
        }
    }
    assert!(text.contains(r##"event: {"target":"#log-rows","swap":"afterbegin"}"##), "{text}");
    assert!(text.contains("a live line for the page test"), "{text}");
    assert!(text.contains("\nid: "), "an id, so a reconnect resumes after it: {text}");
    assert!(!text.contains("the page test failed"), "older lines aren't sent again: {text}");
}
