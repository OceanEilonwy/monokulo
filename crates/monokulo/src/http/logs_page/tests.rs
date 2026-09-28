use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::db::{TEST_ADMIN_EMAIL, TEST_ADMIN_PASSWORD};
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

    let state = AppState { log_store: Some(store), ..AppState::for_tests() };
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
    // Syntax help: a link to its own page, opened as a dialog by script.
    assert!(html.contains(r#"id="query-help-link" href="/dashboard/admin/logs/syntax""#), "{html}");
    assert!(html.contains(r#"<dialog id="query-help""#) && html.contains(r#"<button type="button" class="qh-chip">order.id</button>"#), "property names listed in the help: {html}");
    let (status, _, syntax) = s.get("/dashboard/admin/logs/syntax", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(syntax.contains("<h1>Search syntax</h1>") && syntax.contains(r#"<code class="qh-chip">order.id</code>"#), "{syntax}");
    // Without script each example is a search for it.
    assert!(syntax.contains(&format!(r#"href="/dashboard/admin/logs?q={}""#, encode("level >= warn"))), "{syntax}");
    // Lines come closed, without their properties: each holds a link to
    // its own page, which fixi fetches into place when it opens.
    assert!(!html.contains(r#"<details class="log-row" id="log-monokulo-1" open"#));
    assert!(!html.contains("Only lines where order.id is this"), "no properties in the list: {html}");
    let row_url = properties_url(&html, "payment seen for the page test");
    assert!(html.contains(r#"fx-trigger="toggle""#) && html.contains("Show this line"), "{html}");

    // Opened without JavaScript: a page with the line open, links
    // narrowing or widening the search, and a way back.
    let (status, _, page) = s.get(&row_url, false).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains(r#"<details class="log-row""#) && page.contains(" open>"), "{page}");
    assert!(page.contains(&format!(r#"href="{}""#, logs_url("order.id = 'o_page'", "").replace('&', "&amp;"))), "{page}");
    assert!(page.contains(&format!(r#"href="{}""#, logs_url("not order.id = 'o_page'", ""))), "{page}");
    assert!(page.contains(&format!("/dashboard/admin/logs/trace/{}", s.trace_id)));
    assert!(page.contains("Back to the search"));

    // Opened with fixi: only the properties, replacing the placeholder.
    let (status, _, fragment) = s.get(&row_url, true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(fragment.starts_with(r#"<div class="props" id="log-monokulo-"#), "{fragment}");
    assert!(fragment.contains("<th>order.id</th>") && fragment.contains("Only lines where order.id is this"), "{fragment}");

    // A line retention has deleted.
    let (status, _, page) = s.get("/dashboard/admin/logs/row/1.999999.monokulo", false).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(page.contains("no longer kept"), "{page}");
}

/// The properties URL of the line whose message is `message`.
fn properties_url(html: &str, message: &str) -> String {
    let at = html.find(message).unwrap_or_else(|| panic!("{message} missing: {html}"));
    let start = html[..at].rfind(r#"<details class="log-row""#).unwrap();
    let action = &html[start..at];
    let from = action.find(r#"fx-action=""#).unwrap() + r#"fx-action=""#.len();
    action[from..from + action[from..].find('"').unwrap()].replace("&amp;", "&")
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
async fn the_logs_pages_own_requests_are_hidden_unless_asked_for() {
    let s = setup(0).await;
    tracing::dispatcher::with_default(&s.dispatch, || {
        tracing::info_span!("HTTP request", http.route = "/dashboard/admin/logs/row/{cursor}").in_scope(|| tracing::info!("reading a log line"));
        tracing::info_span!("HTTP request", http.route = "/api/v1/admin/logs").in_scope(|| tracing::info!("the engine served its lines"));
    });
    let store = s.state.log_store.clone().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while store.query(&telemetry::store::LogQuery { limit: 1000, ..Default::default() }).unwrap().len() < 5 {
        assert!(std::time::Instant::now() < deadline, "the lines were never stored");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let (_, _, html) = s.get("/dashboard/admin/logs", false).await;
    assert!(html.contains("payment seen for the page test"), "other requests' lines stay: {html}");
    assert!(html.contains("the page test failed"), "lines outside any request stay: {html}");
    assert!(!html.contains("reading a log line") && !html.contains("the engine served its lines"), "{html}");
    assert!(html.contains(r#"<input type="checkbox" name="logs_requests" value="show">"#), "{html}");

    let (_, _, html) = s.get("/dashboard/admin/logs?logs_requests=show", false).await;
    assert!(html.contains("reading a log line") && html.contains("the engine served its lines"), "{html}");
    assert!(html.contains(r#"name="logs_requests" value="show" checked"#), "{html}");
    assert!(html.contains("logs_requests=show"), "the choice stays in the links: {html}");
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

/// A POS session sent through the real route, out of order and partly
/// late, shown as one timeline in the order the tablet recorded it.
#[tokio::test]
async fn a_pos_session_reads_as_one_timeline_in_the_tablets_order() {
    use tracing::instrument::WithSubscriber;
    let s = setup(0).await;
    let admin = s.state.db.lock().get_user_by_email(TEST_ADMIN_EMAIL).unwrap().unwrap();
    {
        let db = s.state.db.lock();
        db.create_store_connection("c_pos", &admin.id, "woocommerce", "https://pos-shop.example.com", "pk_pos", "enc", "http://engine", 1, "XMR").unwrap();
        db.set_client_logging("c_pos", true).unwrap();
    }
    let session = "5b0c7f0e-2a8d-4c61-9e3b-1f2d3c4b5a69";
    let t0: i64 = 1_790_000_000_000;
    let send = |events: serde_json::Value| {
        let body = serde_json::json!({ "session": session, "events": events }).to_string();
        let request = Request::post("/dashboard/stores/c_pos/pos/logs").header("cookie", &s.cookie).body(Body::from(body)).unwrap();
        s.router.clone().oneshot(request).with_subscriber(s.dispatch.clone())
    };
    // Sent after coming back online: the later batch arrives first.
    let later = serde_json::json!([
        { "seq": 4, "t": t0 + 43_000, "kind": "network.online", "detail": { "offline_ms": 42_000 } },
        { "seq": 5, "t": t0 + 200_000, "kind": "order.created", "order_id": "o_pos", "detail": { "has_note": true } },
        { "seq": 6, "t": t0 + 201_000, "level": "warn", "kind": "stream.error", "detail": { "orders": 1 } },
    ]);
    assert_eq!(send(later).await.unwrap().status(), StatusCode::NO_CONTENT);
    let earlier = serde_json::json!([
        { "seq": 1, "t": t0, "kind": "pos.opened", "detail": { "agent": "iPad Safari", "navigation": "navigate" } },
        { "seq": 2, "t": t0 + 500, "kind": "screen", "detail": { "screen": "keypad" } },
        { "seq": 3, "t": t0 + 1_000, "level": "warn", "kind": "network.offline" },
    ]);
    assert_eq!(send(earlier.clone()).await.unwrap().status(), StatusCode::NO_CONTENT);
    // Sent again, as when the answer to a batch is lost.
    assert_eq!(send(earlier).await.unwrap().status(), StatusCode::NO_CONTENT);
    let store = s.state.log_store.clone().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while store.query(&telemetry::store::LogQuery { limit: 1000, ..Default::default() }).unwrap().iter().filter(|r| r.attributes.contains_key("pos.seq")).count() < 9 {
        assert!(std::time::Instant::now() < deadline, "the POS lines were never stored");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let stored = store.query(&telemetry::store::LogQuery { limit: 1000, ..Default::default() }).unwrap();
    assert_eq!(stored.iter().filter(|r| r.attributes.contains_key("pos.seq")).count(), 9, "a resent batch is stored again");
    let (status, _, html) = s.get(&format!("/dashboard/admin/logs/pos/{session}"), false).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    let at = |needle: &str| html.find(needle).unwrap_or_else(|| panic!("{needle} missing: {html}"));
    assert!(at(">pos.opened<") < at(">screen<") && at(">screen<") < at(">network.offline<"));
    assert!(at(">network.offline<") < at(">network.online<") && at(">network.online<") < at(">order.created<"), "by seq, not arrival");
    assert!(html.contains("offline for 42 s"), "{html}");
    assert!(html.contains("Nothing recorded for 2 min 37 s"), "{html}");
    assert!(html.contains(r#"href="/dashboard/stores/c_pos/orders/o_pos""#), "{html}");
    assert!(html.contains("iPad Safari") && html.contains("pos-shop.example.com"), "{html}");
    assert_eq!(html.matches(">pos.opened<").count(), 1, "shown once: {html}");
    assert!(html.contains("<dt>Events</dt><dd>6</dd>"), "{html}");
    assert!(html.contains("<dt>Orders created</dt><dd>1</dd>") && html.contains("<dt>Warnings and errors</dt><dd>2</dd>"), "{html}");
    assert!(html.contains("sent "), "lines that arrived long after the tablet recorded them say so: {html}");

    // From the order, and from a line in Logs.
    let (status, headers, _) = s.get("/dashboard/admin/logs/pos?order=o_pos", false).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers["location"], format!("/dashboard/admin/logs/pos/{session}"));
    let (status, _, html) = s.get("/dashboard/admin/logs/pos?order=o_elsewhere", false).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(html.contains("No POS session recorded"));
    let (_, _, html) = s.get(&logs_url("pos.kind = 'order.created'", ""), false).await;
    let (_, _, fragment) = s.get(&properties_url(&html, "order.created has_note=true"), true).await;
    assert!(fragment.contains(&format!(r#"href="/dashboard/admin/logs/pos/{session}""#)) && fragment.contains("Show the POS session timeline"), "{fragment}");
    let (status, _, _) = s.get("/dashboard/admin/logs/pos/not-a-session!", false).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
