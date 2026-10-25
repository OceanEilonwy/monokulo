//! The reference, recorded: one exchange of each kind with the API and the
//! handshakes, against a real engine, each answer checked against the
//! OpenAPI document as it's made ([`record`]), then the examples, the
//! OpenAPI document and the reference written from them and checked
//! ([`Reference`]). In a test build, and with the `test-support` feature
//! for the example that writes them (`examples/api_reference.rs`, run by
//! `cargo xtask api openapi` and `cargo xtask pages api`), which needs no
//! test build.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{MatchedPath, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::catalog::{self, Kind};
use super::examples::Exchange;
use super::openapi::{self, OPENAPI_JSON};
use super::problem::Problem;
use super::{reference, DOCS};
use crate::http::test_support::{
    admin_session_cookie, api_request, body_json, body_text, form_post, store_with_keys,
};
use crate::http::{build_router, AppState};

/// The moment every recorded time is: 2026-01-01T00:00:00Z, the start of
/// a minute. The abuse protection's clock is stopped there ([`abuse`]),
/// and [`stable`] puts every `*_at` field there.
pub const RECORDED_AT: i64 = 1_767_225_600;

/// How long every recorded duration (`*_ms`) took, as [`stable`] writes it.
const DURATION_MS: u64 = 12;

/// The address the recorded instance has.
pub const PUBLIC_URI: &str = "https://www.monokulo.com";

/// The onion address the recorded instance has, so the examples show the
/// onion links (`mk:onion-checkout`, `mk:onion-api`). The Tor Project's own.
pub const ONION_URI: &str = "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";

/// The key kind a request's bearer token says it is.
fn key_of(request: &Request) -> Option<String> {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    ["sk", "ak", "pk"]
        .into_iter()
        .find(|kind| token.starts_with(&format!("{kind}_")))
        .map(str::to_owned)
}

/// Runs `request`, checking a JSON answer against the OpenAPI document
/// (`openapi::check_response`): the answer, and the exchange it was.
pub(super) async fn exchange(request: Request, next: Next) -> (Response, Option<Exchange>) {
    let Some(route) = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned())
    else {
        return (next.run(request).await, None);
    };
    let method = request.method().as_str().to_owned();
    let path = request.uri().to_string();
    let key = key_of(&request);
    let (parts, body) = request.into_parts();
    let request_bytes = body
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let request_json: Option<Value> = serde_json::from_slice(&request_bytes).ok();
    let response = next
        .run(Request::from_parts(parts, Body::from(request_bytes)))
        .await;
    // One of each header (one `Vary`, one `Cache-Control`): a client that
    // keeps the last of each name would lose the others.
    let repeated = crate::http::repeated_headers(response.headers());
    assert!(
        repeated.is_empty(),
        "{method} {path} repeats {repeated:?}: {:?}",
        response.headers()
    );
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let mut exchange = Exchange {
        method,
        route,
        path,
        key,
        onion: false,
        request: request_json,
        status: response.status().as_u16(),
        response: None,
    };
    // A stream stays a stream, and a page a page: only JSON is read here.
    if !content_type.contains("json") {
        return (response, Some(exchange));
    }
    let (parts, body) = response.into_parts();
    let bytes: Bytes = body
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if !content_type.starts_with(OPENAPI_JSON) {
        openapi::check_response(
            &exchange.method,
            &exchange.route,
            parts.status,
            &parts.headers,
            &json,
        );
        exchange.response = Some(json);
    }
    (
        Response::from_parts(parts, Body::from(bytes)),
        Some(exchange),
    )
}

/// Keeps every exchange its router makes, each checked as it's made.
#[derive(Clone, Default)]
pub struct Recorder {
    exchanges: Arc<Mutex<Vec<Exchange>>>,
    /// Whether its router's Monokulo has an onion address.
    onion: bool,
}

impl Recorder {
    /// This recorder, for a router whose Monokulo has an onion address:
    /// what it records is kept with the rest, marked as such.
    pub fn with_onion(&self) -> Recorder {
        Recorder {
            exchanges: self.exchanges.clone(),
            onion: true,
        }
    }

    /// `router`, recording.
    pub fn around(&self, router: axum::Router) -> axum::Router {
        router.layer(axum::middleware::from_fn_with_state(
            self.clone(),
            Self::middleware,
        ))
    }

    async fn middleware(
        State(recorder): State<Recorder>,
        request: Request,
        next: Next,
    ) -> Response {
        let (response, exchange) = exchange(request, next).await;
        if let Some(mut exchange) = exchange {
            exchange.onion = recorder.onion;
            recorder.push(exchange);
        }
        response
    }

    fn push(&self, exchange: Exchange) {
        self.exchanges
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(exchange);
    }

    /// The exchanges so far, taken.
    pub fn take(&self) -> Vec<Exchange> {
        std::mem::take(&mut *self.exchanges.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// The options file of a recorded instance: its web address (`None` for
/// one with none yet) and, with `onion`, an onion address served by an
/// external tor. Webhooks may go to private addresses: the store's go to
/// a receiver on this machine ([`receiver`]).
fn options(public_uri: Option<&str>, onion: bool) -> String {
    let public = public_uri
        .map(|uri| format!("public_uri = \"{uri}\"\n"))
        .unwrap_or_default();
    let webhooks = "\n[webhooks]\nallow_private_urls = true\n";
    if onion {
        format!(
            "{public}onion_uri = \"{ONION_URI}\"\n\n[tor]\nmode = \"external\"\n\n[abuse]\nonion_listener = \"127.0.0.1:8082\"\n{webhooks}"
        )
    } else {
        format!("{public}{webhooks}")
    }
}

/// A webhook receiver on this machine that takes every delivery: its URL.
/// It answers the same each time, so the examples of a test delivery
/// (`send-test-webhook`) are the same each time too.
async fn receiver() -> String {
    let app = axum::Router::new().fallback(|| async { "ok" });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/monokulo/webhook")
}

/// Points every webhook of `state`'s stores at `url`: the plugin's,
/// registered at `bakery.example`, at the fixture's [`receiver`].
fn webhooks_to(state: &AppState, url: &str) {
    state
        .db
        .lock()
        .conn_for_test()
        .execute("UPDATE webhooks SET url = ?1", [url])
        .unwrap();
}

/// A state like a real instance's, on `engine`, with [`options`].
async fn state(engine: axum::Router, public_uri: Option<&str>, onion: bool) -> AppState {
    AppState {
        engine: crate::http::Engine::new(crate::engine_client::EngineClient::embedded_for_tests(
            engine,
        )),
        abuse: abuse(crate::abuse::AbuseConfig::default()),
        ..AppState::for_tests()
    }
    .with_options(&options(public_uri, onion))
    .await
}

fn json_post(uri: &str, body: &Value) -> Request {
    axum::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("monokulo-client", "monokular/1.0.0")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Connects an app to store `id` as the dashboard and the app do: what the
/// handshake answered.
async fn connect_app(router: &axum::Router, id: &str, cookie: &str) -> Value {
    let page = router
        .clone()
        .oneshot(form_post(
            &format!("/stores/{id}/settings/apps/connect"),
            Some(cookie),
            &[("device_name", "Front till")],
        ))
        .await
        .unwrap();
    let html = body_text(page).await;
    let start = html.find("/apps/connect#token=").expect("a code") + 20;
    let token: String = html[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let redeemed = router
        .clone()
        .oneshot(json_post(
            crate::http::apps::CONNECT,
            &json!({"token": token, "slug": "monokular", "name": "Monokular", "platform": "android"}),
        ))
        .await
        .unwrap();
    assert_eq!(redeemed.status(), StatusCode::OK);
    body_json(redeemed).await
}

/// A connect code for store `id`, as the dashboard mints one for a plugin.
fn plugin_connect_code(state: &AppState, id: &str) -> String {
    let raw = shared::auth::generate_connect_token();
    state
        .db
        .lock()
        .create_connect_token(
            &raw.hash(),
            &crate::db::ConnectionId::new(id.to_owned()),
            "nonce-1",
            crate::now_unix(),
        )
        .unwrap();
    raw.expose().to_owned()
}

/// Reads the server-sent events of `response` until `until` of them have
/// come: each `{"event", "data"}`, its data JSON where it is.
async fn read_events(response: Response, until: usize) -> Value {
    let mut body = response.into_body();
    let mut text = String::new();
    let mut events = Vec::new();
    while events.len() < until {
        let frame = tokio::time::timeout(Duration::from_secs(10), body.frame())
            .await
            .expect("the stream sends its events in time")
            .expect("the stream is still open")
            .expect("the stream can be read");
        let Ok(data) = frame.into_data() else {
            continue;
        };
        text.push_str(std::str::from_utf8(&data).unwrap());
        while let Some(end) = text.find("\n\n") {
            let block: String = text.drain(..end + 2).collect();
            let (mut name, mut data) = (None, String::new());
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("event:") {
                    name = Some(value.trim().to_owned());
                } else if let Some(value) = line.strip_prefix("data:") {
                    data.push_str(value.trim_start());
                }
            }
            // A comment (the keep-alive) has neither.
            if let Some(name) = name {
                let data = serde_json::from_str(&data).unwrap_or(Value::String(data));
                events.push(json!({"event": name, "data": data}));
            }
        }
    }
    Value::Array(events)
}

/// A Monokulo on a real engine with a store, its keys and a connected app,
/// as the recording and the API's own tests use one.
pub struct Fixture {
    pub state: AppState,
    /// Its router, recording into the recorder it was made with.
    pub router: axum::Router,
    /// The merchant's session cookie (`session=…`).
    pub cookie: String,
    pub store_id: String,
    pub sk: String,
    pub pk: String,
    pub ak: String,
    /// What the app's handshake answered.
    pub connected: Value,
    /// Where the store's webhook goes: a receiver on this machine that
    /// takes every delivery ([`receiver`]).
    pub receiver: String,
}

/// A [`Fixture`] on `engine` (a test engine's router) at `public_uri`,
/// with an onion address, whose router records into `recorder`.
pub async fn fixture(engine: axum::Router, public_uri: &str, recorder: &Recorder) -> Fixture {
    fixture_on(engine, public_uri, true, recorder).await
}

/// A [`Fixture`] on `engine` at `public_uri`, with an onion address when
/// `onion` says, whose router records into `recorder`.
pub async fn fixture_on(
    engine: axum::Router,
    public_uri: &str,
    onion: bool,
    recorder: &Recorder,
) -> Fixture {
    let state = state(engine, Some(public_uri), onion).await;
    let router = recorder.around(build_router(state.clone()));
    // The seeded admin: this instance keeps sign-up to invites.
    let admin = admin_session_cookie(&router).await;
    let session = admin.strip_prefix("session=").unwrap().to_owned();
    let store = store_with_keys(&router, &state, &session, "bakery.example", "XMR").await;
    let cookie = format!("session={session}");
    let connected = connect_app(&router, &store.id, &cookie).await;
    // A webhook, as the store's server registers one, so a test delivery
    // (`send-test-webhook`) has somewhere to go.
    let receiver = receiver().await;
    crate::webhooks::create(
        &state.db,
        &state.encryption_key,
        &crate::db::ConnectionId::new(store.id.clone()),
        &receiver,
        &std::collections::BTreeMap::new(),
    )
    .await
    .unwrap();
    Fixture {
        receiver,
        ak: connected["app_key"].as_str().unwrap().to_owned(),
        state,
        router,
        cookie,
        store_id: store.id,
        sk: store.secret_key,
        pk: store.public_key,
        connected,
    }
}

/// The plugin's handshake for store `id` with a fresh connect code, as
/// its server finishes connecting: what it answered.
async fn finish_plugin(router: &axum::Router, state: &AppState, id: &str) -> Value {
    let code = plugin_connect_code(state, id);
    let finished = router
        .clone()
        .oneshot(json_post(
            &crate::http::connect::FINISH.replace("{slug}", "prestashop"),
            &json!({"token": code, "webhook_url": "https://bakery.example/monokulo/webhook", "name": "PrestaShop"}),
        ))
        .await
        .unwrap();
    assert_eq!(finished.status(), StatusCode::OK);
    body_json(finished).await
}

/// `request` from the clearnet client `peer`, as a real connection
/// brings it: the abuse layer counts it.
fn from_peer(mut request: Request, peer: &str) -> Request {
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        peer.parse::<std::net::SocketAddr>().unwrap(),
    ));
    request
}

/// The handshakes' problems that need an instance of their own: too
/// large a body, too many requests (`rate-limited`, never a challenge: a
/// server can't solve one) and a failure here (`internal`): the plugin's
/// when this Monokulo's encryption key doesn't open the store's keys, the
/// app's when its database fails.
async fn record_handshake_failures(
    engine: axum::Router,
    state: &AppState,
    store_id: &str,
    recorder: &Recorder,
) {
    let finish = crate::http::connect::FINISH.replace("{slug}", "prestashop");
    let router = recorder.around(build_router(state.clone()));
    let too_large = json!({"token": "x".repeat(3 * 1024 * 1024)});
    for uri in [finish.as_str(), crate::http::apps::CONNECT] {
        let answer = router
            .clone()
            .oneshot(json_post(uri, &too_large))
            .await
            .unwrap();
        assert_eq!(answer.status(), StatusCode::PAYLOAD_TOO_LARGE, "{uri}");
    }

    let limited = with_abuse(
        state,
        crate::abuse::AbuseConfig {
            soft_per_min: 1,
            hard_per_min: 1,
            ..Default::default()
        },
        recorder,
    );
    for (uri, body, peer) in [
        (
            finish.as_str(),
            json!({"token": "conn_0"}),
            "203.0.113.10:1000",
        ),
        (
            crate::http::apps::CONNECT,
            json!({"token": "ct_0", "slug": "monokular"}),
            "203.0.113.11:1000",
        ),
    ] {
        let mut last = StatusCode::OK;
        for _ in 0..2 {
            last = limited
                .clone()
                .oneshot(from_peer(json_post(uri, &body), peer))
                .await
                .unwrap()
                .status();
        }
        assert_eq!(last, StatusCode::TOO_MANY_REQUESTS, "{uri}");
    }

    let wrong_key = AppState {
        encryption_key: crate::crypto::AtRestKey::new([9; 32]),
        ..state.clone()
    };
    let code = plugin_connect_code(&wrong_key, store_id);
    let answer = recorder
        .around(build_router(wrong_key))
        .oneshot(json_post(&finish, &json!({"token": code})))
        .await
        .unwrap();
    assert_eq!(answer.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let broken = state_on_its_own(engine).await;
    broken
        .db
        .lock()
        .conn_for_test()
        .execute_batch("DROP TABLE app_connect_tokens")
        .unwrap();
    let answer = recorder
        .around(build_router(broken))
        .oneshot(json_post(
            crate::http::apps::CONNECT,
            &json!({"token": "ct_0", "slug": "monokular"}),
        ))
        .await
        .unwrap();
    assert_eq!(answer.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// A store's three keys.
struct Keys<'a> {
    sk: &'a str,
    pk: &'a str,
    ak: &'a str,
}

impl<'a> Keys<'a> {
    /// Those `handler` takes.
    fn admitted(&self, handler: &catalog::HandlerInfo) -> Vec<&'a str> {
        use super::auth::Key;
        [(Key::Pk, self.pk), (Key::Sk, self.sk), (Key::Ak, self.ak)]
            .into_iter()
            .filter(|(key, _)| handler.who.admits(Some(*key)))
            .map(|(_, token)| token)
            .collect()
    }

    /// Those `handler` refuses.
    fn refused(&self, handler: &catalog::HandlerInfo) -> Vec<&'a str> {
        use super::auth::Key;
        [(Key::Pk, self.pk), (Key::Sk, self.sk), (Key::Ak, self.ak)]
            .into_iter()
            .filter(|(key, _)| !handler.who.admits(Some(*key)))
            .map(|(_, token)| token)
            .collect()
    }

    /// The key a server would call `handler` with: the secret key, or
    /// the app's where only an app's is taken.
    fn server(&self, handler: &catalog::HandlerInfo) -> &'a str {
        if handler.who.admits(Some(super::auth::Key::Sk)) {
            self.sk
        } else {
            self.ak
        }
    }
}

/// `method uri` on `router` with `key` and `body`: its answer.
async fn call_api(
    router: &axum::Router,
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: Option<Value>,
) -> Response {
    router
        .clone()
        .oneshot(api_request(method, uri, key, body))
        .await
        .unwrap()
}

/// The path of the route `template`, for the order `order_id`.
fn path_of(template: &str, order_id: &str) -> String {
    template.replace("{order_id}", order_id)
}

/// What a client sends `handler`: `order` to make an order, a log report
/// to the logs, an empty object to test the store's webhook (the plugin's,
/// as a plugin's "Test connection" sends it), nothing elsewhere.
fn body_for(handler: &catalog::HandlerInfo, order: &Value) -> Option<Value> {
    match (handler.method, handler.template) {
        ("POST", t) if t == super::super::v1::orders::Path::TEMPLATE => Some(order.clone()),
        ("POST", t) if t == super::super::v1::logs::Path::TEMPLATE => Some(
            json!({"entries": [{"level": "error", "message": "Monokulo could not be reached"}]}),
        ),
        ("POST", t) if t == super::super::v1::webhook_tests::Path::TEMPLATE => Some(json!({})),
        _ => None,
    }
}

/// The `send-test-webhook` form's answers: a test delivered to the
/// plugin's webhook, one to a webhook the store doesn't have, then the
/// store past its test limit (`rate-limited`; the limit's clock is the
/// abuse protection's, stopped, so its wait is the same every time).
async fn record_test_webhooks(router: &axum::Router, sk: &str) {
    let uri = super::super::v1::webhook_tests::Path::TEMPLATE;
    let sent = call_api(router, "POST", uri, Some(sk), Some(json!({}))).await;
    assert_eq!(sent.status(), StatusCode::OK);
    assert_eq!(body_json(sent).await["delivered"], true);
    let unknown = json!({"webhook_id": "wh_00000000000000000000000000000000"});
    let answer = call_api(router, "POST", uri, Some(sk), Some(unknown)).await;
    assert_eq!(answer.status(), StatusCode::NOT_FOUND);
    let answer = call_api(
        router,
        "POST",
        uri,
        Some(sk),
        Some(json!({"webhook_id": 7})),
    )
    .await;
    assert_eq!(answer.status(), StatusCode::BAD_REQUEST);
    let mut last = StatusCode::OK;
    for _ in 0..=crate::webhooks::test_delivery::TESTS_PER_MINUTE {
        last = call_api(router, "POST", uri, Some(sk), Some(json!({})))
            .await
            .status();
        if last == StatusCode::TOO_MANY_REQUESTS {
            break;
        }
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
}

/// The order's changes, followed with `key` (of kind `kind`) on `router`,
/// which records into `recorder`: its first event, the order as it is,
/// kept as the stream's answer.
async fn record_order_events(
    router: &axum::Router,
    recorder: &Recorder,
    order_id: &str,
    key: &str,
    kind: &str,
) {
    let uri = format!("/api/v1/orders/{order_id}/events");
    let stream = call_api(router, "GET", &uri, Some(key), None).await;
    assert_eq!(stream.status(), StatusCode::OK);
    let events = read_events(stream, 1).await;
    recorder.push(Exchange {
        method: "GET".into(),
        route: super::super::v1::order_events::Path::TEMPLATE.into(),
        path: uri,
        key: Some(kind.into()),
        onion: recorder.onion,
        request: None,
        status: 200,
        response: Some(events),
    });
}

/// A local CoinGecko that rate limits every request, asking for `secs`.
async fn rate_limited_coingecko(secs: u64) -> String {
    let app = axum::Router::new().fallback(move || async move {
        use axum::response::IntoResponse;
        (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", secs.to_string())],
            r#"{"status":{"error_code":429,"error_message":"You've exceeded the Rate Limit."}}"#,
        )
            .into_response()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// `router` on a Monokulo like `state` but with abuse protection `config`,
/// on the recording's clock ([`abuse`]).
fn with_abuse(
    state: &AppState,
    config: crate::abuse::AbuseConfig,
    recorder: &Recorder,
) -> axum::Router {
    recorder.around(build_router(AppState {
        abuse: abuse(config),
        ..state.clone()
    }))
}

/// Abuse protection with `config`, its clock stopped at [`RECORDED_AT`]:
/// how long a client is told to wait (`retry_after`, `Retry-After`) and
/// when a challenge expires depend on where in the minute its requests
/// fall, so on the system's clock they'd change from run to run.
fn abuse(config: crate::abuse::AbuseConfig) -> Arc<crate::abuse::AbuseProtection> {
    Arc::new(
        crate::abuse::AbuseProtection::new(config)
            .with_clock(crate::abuse::Clock::Fixed(RECORDED_AT)),
    )
}

/// Runs a statement on `state`'s database, to make it fail for a moment.
fn sql(state: &AppState, statement: &str) {
    state
        .db
        .lock()
        .conn_for_test()
        .execute_batch(statement)
        .unwrap();
}

/// Every problem each route of the API may answer (`HandlerInfo::problems`)
/// that the main sequence doesn't meet, each from the state that makes it:
/// a key it doesn't know or take; a body or query it can't read; too many
/// requests, a challenge, too many open streams; this Monokulo's own
/// failures (its database, a store key it can't open), the engine
/// unreachable, the rate providers rate limiting it; and, making an order,
/// a key used for another order, a site the store hasn't verified and its
/// policy unreadable.
async fn record_route_problems(
    state: &AppState,
    router: &axum::Router,
    order_id: &str,
    keys: &Keys<'_>,
    recorder: &Recorder,
) {
    use super::problem::Problem;
    let store_id = state
        .db
        .lock()
        .get_store_connection_by_public_key(keys.pk)
        .unwrap()
        .unwrap()
        .id;
    let xmr = json!({"amount": "0.25", "currency": "XMR"});
    let handlers = catalog::handlers();

    // A key this Monokulo doesn't know, and each key a route doesn't take.
    for handler in &handlers {
        let uri = path_of(handler.template, order_id);
        let body = body_for(handler, &xmr);
        call_api(router, handler.method, &uri, Some("sk_0000"), body.clone()).await;
        for key in keys.refused(handler) {
            call_api(router, handler.method, &uri, Some(key), body.clone()).await;
        }
    }

    // A body or query a route can't read.
    let orders = super::super::v1::orders::Path::TEMPLATE;
    let logs = super::super::v1::logs::Path::TEMPLATE;
    let tests = super::super::v1::webhook_tests::Path::TEMPLATE;
    for uri in [orders, logs, tests] {
        // An empty object is all a test webhook takes: it would send one
        // (`record_test_webhooks` does).
        if uri != tests {
            call_api(router, "POST", uri, Some(keys.sk), Some(json!({}))).await;
        }
        let unlabelled = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {}", keys.sk))
                    .body(Body::from("amount=1"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unlabelled.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let too_large =
            json!({"amount": "1", "currency": "XMR", "description": "x".repeat(64 * 1024)});
        let answer = call_api(router, "POST", uri, Some(keys.sk), Some(too_large)).await;
        assert_eq!(answer.status(), StatusCode::PAYLOAD_TOO_LARGE, "{uri}");
    }
    let many = json!({"entries": vec![json!({"level": "error", "message": "again"}); super::super::v1::logs::MAX_ENTRIES + 1]});
    let answer = call_api(router, "POST", logs, Some(keys.sk), Some(many)).await;
    assert_eq!(answer.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let answer = call_api(
        router,
        "GET",
        "/api/v1/orders?limit=many",
        Some(keys.sk),
        None,
    )
    .await;
    assert_eq!(answer.status(), StatusCode::BAD_REQUEST);

    // The order's idempotency key, sent again for another amount.
    let other =
        json!({"amount": "0.30", "currency": "XMR", "idempotency_key": "order-1042-total-025"});
    let answer = call_api(router, "POST", orders, Some(keys.sk), Some(other)).await;
    assert_eq!(answer.status(), StatusCode::CONFLICT);

    // A store that takes orders only from its verified websites, asked
    // with its public key from nowhere; then its policy unreadable.
    state
        .db
        .lock()
        .set_embed_restricted(&store_id, true)
        .unwrap();
    let answer = call_api(router, "POST", orders, Some(keys.pk), Some(xmr.clone())).await;
    assert_eq!(answer.status(), StatusCode::FORBIDDEN);
    state
        .db
        .lock()
        .set_embed_restricted(&store_id, false)
        .unwrap();
    sql(
        state,
        "ALTER TABLE store_domains RENAME TO store_domains_away",
    );
    let answer = call_api(router, "POST", orders, Some(keys.pk), Some(xmr.clone())).await;
    sql(
        state,
        "ALTER TABLE store_domains_away RENAME TO store_domains",
    );
    assert_eq!(answer.status(), StatusCode::SERVICE_UNAVAILABLE);

    // Too many requests: a server's key past its budget, then a browser
    // asked to solve a challenge, on every route that asks one.
    let limited = with_abuse(
        state,
        crate::abuse::AbuseConfig {
            soft_per_min: 1,
            hard_per_min: 1,
            per_store_key_per_min: 1,
            client_logs_per_min: 1,
            ..Default::default()
        },
        recorder,
    );
    for (n, handler) in handlers.iter().enumerate() {
        let uri = path_of(handler.template, order_id);
        let peer = format!("203.0.113.20:{}", 2000 + n);
        let mut last = StatusCode::OK;
        for _ in 0..2 {
            let request = api_request(
                handler.method,
                &uri,
                Some(keys.server(handler)),
                body_for(handler, &xmr),
            );
            last = limited
                .clone()
                .oneshot(from_peer(request, &peer))
                .await
                .unwrap()
                .status();
        }
        assert_eq!(last, StatusCode::TOO_MANY_REQUESTS, "{uri}");
    }
    let under_attack = with_abuse(
        state,
        crate::abuse::AbuseConfig {
            under_attack: true,
            ..Default::default()
        },
        recorder,
    );
    for (n, handler) in handlers.iter().enumerate() {
        if !handler.problems().contains(&Problem::ChallengeRequired) {
            continue;
        }
        let uri = path_of(handler.template, order_id);
        let request = api_request(handler.method, &uri, None, body_for(handler, &xmr));
        let answer = under_attack
            .clone()
            .oneshot(from_peer(request, &format!("203.0.113.30:{}", 2000 + n)))
            .await
            .unwrap();
        assert_eq!(answer.status(), StatusCode::TOO_MANY_REQUESTS, "{uri}");
    }

    // A client with as many streams open on the store as it may.
    let one_stream = with_abuse(
        state,
        crate::abuse::AbuseConfig {
            stream_cap: 1,
            ..Default::default()
        },
        recorder,
    );
    let stream = |uri: String| {
        from_peer(
            api_request("GET", &uri, Some(keys.sk), None),
            "203.0.113.40:1000",
        )
    };
    let open = one_stream
        .clone()
        .oneshot(stream("/api/v1/events".into()))
        .await
        .unwrap();
    assert_eq!(open.status(), StatusCode::OK);
    for uri in [
        "/api/v1/events".to_owned(),
        format!("/api/v1/orders/{order_id}/events"),
    ] {
        let answer = one_stream
            .clone()
            .oneshot(stream(uri.clone()))
            .await
            .unwrap();
        assert_eq!(answer.status(), StatusCode::TOO_MANY_REQUESTS, "{uri}");
    }
    drop(open);

    // This Monokulo's own failures: a store key it can't open (its
    // encryption key isn't the one the store's was sealed with), its
    // database failing to read a store's wallet or to forget an app.
    let wrong_key = recorder.around(build_router(AppState {
        encryption_key: crate::crypto::AtRestKey::new([9; 32]),
        ..state.clone()
    }));
    for handler in &handlers {
        if !handler.problems().contains(&Problem::Internal) || handler.method == "DELETE" {
            continue;
        }
        // Their secret key alone, which a wrong encryption key leaves
        // unknown (`401`): each fails here another way, below.
        if handler.template == super::super::v1::store::Path::TEMPLATE
            || handler.template == super::super::v1::webhook_tests::Path::TEMPLATE
        {
            continue;
        }
        // The public key and the app's are checked without opening the
        // store's secret key.
        let key = if handler.who.admits(Some(super::auth::Key::Pk)) {
            keys.pk
        } else if handler.who.admits(Some(super::auth::Key::Ak)) {
            keys.ak
        } else {
            keys.sk
        };
        let answer = call_api(
            &wrong_key,
            handler.method,
            &path_of(handler.template, order_id),
            Some(key),
            body_for(handler, &xmr),
        )
        .await;
        assert_eq!(
            answer.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "{} {}",
            handler.method,
            handler.template
        );
    }
    sql(state, "ALTER TABLE wallets RENAME TO wallets_away");
    let answer = call_api(router, "GET", "/api/v1/store", Some(keys.sk), None).await;
    sql(state, "ALTER TABLE wallets_away RENAME TO wallets");
    assert_eq!(answer.status(), StatusCode::INTERNAL_SERVER_ERROR);
    sql(state, "ALTER TABLE webhooks RENAME TO webhooks_away");
    let tests = super::super::v1::webhook_tests::Path::TEMPLATE;
    let answer = call_api(router, "POST", tests, Some(keys.sk), Some(json!({}))).await;
    sql(state, "ALTER TABLE webhooks_away RENAME TO webhooks");
    assert_eq!(answer.status(), StatusCode::INTERNAL_SERVER_ERROR);
    sql(
        state,
        "CREATE TRIGGER keep_apps BEFORE DELETE ON app_connections BEGIN SELECT RAISE(FAIL, 'kept'); END",
    );
    let answer = call_api(router, "DELETE", "/api/v1/app", Some(keys.ak), None).await;
    sql(state, "DROP TRIGGER keep_apps");
    assert_eq!(answer.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // The engine unreachable: every route that asks it.
    let down = recorder.around(build_router(AppState {
        engine: crate::http::Engine::new(crate::engine_client::EngineClient::for_tests(format!(
            "http://{}",
            shared::unreachable::address()
        ))),
        ..state.clone()
    }));
    for handler in &handlers {
        if !handler.problems().contains(&Problem::EngineUnavailable) {
            continue;
        }
        let answer = call_api(
            &down,
            handler.method,
            &path_of(handler.template, order_id),
            Some(keys.sk),
            body_for(handler, &xmr),
        )
        .await;
        assert_eq!(
            answer.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{} {}",
            handler.method,
            handler.template
        );
    }

    // The rate providers rate limiting this Monokulo, with no rate kept.
    let rateless = recorder.around(build_router(AppState {
        exchange_rate: Arc::new(
            crate::exchange_rate_config::ExchangeRateProviders::coingecko_only(
                rate_limited_coingecko(42).await,
            ),
        ),
        ..state.clone()
    }));
    let answer = call_api(
        &rateless,
        "POST",
        orders,
        Some(keys.sk),
        Some(json!({"amount": "10.00", "currency": "USD"})),
    )
    .await;
    assert_eq!(answer.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(answer.headers()[header::RETRY_AFTER], "42");
}

/// A state of its own (its own database), at the recorded address.
async fn state_on_its_own(engine: axum::Router) -> AppState {
    state(engine, Some(PUBLIC_URI), false).await
}

/// One coherent sequence of exchanges, against `engine` (a test engine's
/// router), on a Monokulo without an onion address (the common case) and
/// then the same one with one: the handshakes and each problem they
/// answer; the API with each key, its streams and its problems. One order
/// is made (by an app, then again by the shop's server with the same
/// `idempotency_key`, which gets that same order; the website's same key
/// is its own, and makes another), and it is the one read, listed,
/// streamed and linked, so a client can follow the examples' links from
/// one to the next.
pub async fn record(engine: axum::Router) -> Vec<Exchange> {
    let recorder = Recorder::default();
    let fixture = fixture_on(engine.clone(), PUBLIC_URI, false, &recorder).await;
    assert_eq!(
        fixture.connected,
        json!({"app_key": fixture.ak, "api": "/api/v1"})
    );
    let Fixture {
        state,
        router,
        store_id,
        sk,
        pk,
        ak,
        cookie,
        receiver,
        ..
    } = fixture;
    let (sk, pk) = (sk.as_str(), pk.as_str());
    let call = |router: &axum::Router,
                method: &'static str,
                uri: String,
                key: Option<&str>,
                body: Option<Value>| {
        let request = api_request(method, &uri, key, body);
        let router = router.clone();
        async move { router.oneshot(request).await.unwrap() }
    };

    // The plugin's handshake, and each problem it can be made to answer.
    let finish = crate::http::connect::FINISH.replace("{slug}", "prestashop");
    let code = plugin_connect_code(&state, &store_id);
    let finished = router
        .clone()
        .oneshot(json_post(
            &finish,
            &json!({"token": code, "webhook_url": "https://bakery.example/monokulo/webhook", "name": "PrestaShop"}),
        ))
        .await
        .unwrap();
    assert_eq!(finished.status(), StatusCode::OK);
    assert_eq!(body_json(finished).await["secret_token"], sk);
    // The plugin registered its webhook at its shop: point it here.
    webhooks_to(&state, &receiver);
    for (uri, body) in [
        (finish.clone(), json!({"token": code})),
        (
            finish.clone(),
            json!({"token": "conn_0", "webhook_url": "ftp://bakery.example"}),
        ),
        (
            crate::http::connect::FINISH.replace("{slug}", "Presta!"),
            json!({"token": "conn_0"}),
        ),
    ] {
        router
            .clone()
            .oneshot(json_post(&uri, &body))
            .await
            .unwrap();
    }
    for uri in [finish.as_str(), crate::http::apps::CONNECT] {
        router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(uri)
                    .body(Body::from("token=conn_0"))
                    .unwrap(),
            )
            .await
            .unwrap();
    }
    // The app's handshake's problems (`connect_app` connected one).
    for body in [
        json!({"token": "ct_0", "slug": "monokular"}),
        json!({"token": "ct_0", "slug": "Monokular!"}),
    ] {
        router
            .clone()
            .oneshot(json_post(crate::http::apps::CONNECT, &body))
            .await
            .unwrap();
    }
    // Both handshakes on a Monokulo whose admin hasn't set its address.
    let unset = recorder.around(build_router(state_without_address(engine.clone()).await));
    unset
        .clone()
        .oneshot(json_post(&finish, &json!({"token": "conn_0"})))
        .await
        .unwrap();
    unset
        .clone()
        .oneshot(json_post(
            crate::http::apps::CONNECT,
            &json!({"token": "ct_0", "slug": "monokular"}),
        ))
        .await
        .unwrap();
    record_handshake_failures(engine.clone(), &state, &store_id, &recorder).await;

    // The entry and the store as each key sees them.
    let entry = call(&router, "GET", "/api/v1".into(), None, None).await;
    let etag = entry.headers()[header::ETAG].to_str().unwrap().to_owned();
    for key in [Some(sk), Some(pk), Some(ak.as_str())] {
        call(&router, "GET", "/api/v1".into(), key, None).await;
        call(&router, "GET", "/api/v1/store".into(), key, None).await;
    }
    call(&router, "GET", "/api/v1/store".into(), None, None).await;
    call(&router, "GET", "/api/v1".into(), Some("sk_0000"), None).await;
    let unchanged = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/v1")
                .header(header::IF_NONE_MATCH, &etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);

    // The order: made by the app (a till) while the store's sale events
    // are followed, then again, with the same idempotency key, by the
    // shop's server, which gets that same order, and by its website, whose
    // keys are its own (anyone can send the public key), which gets
    // another.
    let order_input = json!({
        "amount": "0.25",
        "currency": "XMR",
        "merchant_order_id": "1042",
        "description": "Order #1042: Blue mug + 2 more",
        "show_merchant_order_id": true,
        "idempotency_key": "order-1042-total-025",
    });
    let stream = call(&router, "GET", "/api/v1/events".into(), Some(&ak), None).await;
    assert_eq!(stream.status(), StatusCode::OK);
    let mut made = Vec::new();
    for key in [ak.as_str(), sk] {
        let answer = call(
            &router,
            "POST",
            "/api/v1/orders".into(),
            Some(key),
            Some(order_input.clone()),
        )
        .await;
        assert_eq!(answer.status(), StatusCode::CREATED);
        made.push(body_json(answer).await);
    }
    let [by_app, by_server] = &made[..] else {
        unreachable!("two orders were asked for")
    };
    assert_eq!(
        by_server["order_id"], by_app["order_id"],
        "the server's key gets the app's order"
    );
    let order_id = by_app["order_id"].as_str().unwrap().to_owned();
    let events = read_events(stream, 2).await;
    recorder.push(Exchange {
        method: "GET".into(),
        route: super::super::v1::events::Path::TEMPLATE.into(),
        path: "/api/v1/events".into(),
        key: Some("ak".into()),
        onion: false,
        request: None,
        status: 200,
        response: Some(events),
    });
    call(&router, "GET", "/api/v1/orders".into(), Some(sk), None).await;
    for amount in ["twelve", "1"] {
        let currency = if amount == "1" { "NOPE" } else { "XMR" };
        call(
            &router,
            "POST",
            "/api/v1/orders".into(),
            Some(sk),
            Some(json!({"amount": amount, "currency": currency})),
        )
        .await;
    }
    for key in [sk, pk, ak.as_str()] {
        call(
            &router,
            "GET",
            format!("/api/v1/orders/{order_id}"),
            Some(key),
            None,
        )
        .await;
    }
    call(
        &router,
        "GET",
        "/api/v1/orders/order_00000000000000000000000000000000".into(),
        Some(sk),
        None,
    )
    .await;
    // The order's changes: the first event is the order as it is.
    let order_stream = call(
        &router,
        "GET",
        format!("/api/v1/orders/{order_id}/events"),
        Some(sk),
        None,
    )
    .await;
    assert_eq!(order_stream.status(), StatusCode::OK);
    let events = read_events(order_stream, 1).await;
    recorder.push(Exchange {
        method: "GET".into(),
        route: super::super::v1::order_events::Path::TEMPLATE.into(),
        path: format!("/api/v1/orders/{order_id}/events"),
        key: Some("sk".into()),
        onion: false,
        request: None,
        status: 200,
        response: Some(events),
    });
    call(&router, "GET", "/api/v1/app".into(), Some(&ak), None).await;
    call(&router, "GET", "/api/v1/app".into(), Some(sk), None).await;
    let logs = json!({"entries": [{"level": "error", "message": "Monokulo could not be reached"}]});
    call(
        &router,
        "POST",
        "/api/v1/logs".into(),
        Some(sk),
        Some(logs.clone()),
    )
    .await;
    state
        .db
        .lock()
        .set_client_logging(&crate::db::ConnectionId::new(store_id.clone()), true)
        .unwrap();
    call(&router, "POST", "/api/v1/logs".into(), Some(sk), Some(logs)).await;
    call(&router, "GET", "/api/v1/openapi.json".into(), None, None).await;
    // The rest of each key's answers: the store's sales with its secret
    // key, its orders with the app's, the order's changes with each key.
    drop(call(&router, "GET", "/api/v1/events".into(), Some(sk), None).await);
    call(&router, "GET", "/api/v1/orders".into(), Some(&ak), None).await;
    for (key, kind) in [(pk, "pk"), (ak.as_str(), "ak")] {
        record_order_events(&router, &recorder, &order_id, key, kind).await;
    }
    // The problems the other routes answer: a key that may not, an
    // unknown key, an order the store doesn't have; then every other
    // problem each route may answer.
    call(&router, "DELETE", "/api/v1/app".into(), Some(sk), None).await;
    call(&router, "GET", "/api/v1/events".into(), Some(pk), None).await;
    call(&router, "GET", "/api/v1/orders".into(), Some(pk), None).await;
    call(
        &router,
        "GET",
        "/api/v1/openapi.json".into(),
        Some("sk_0000"),
        None,
    )
    .await;
    call(
        &router,
        "GET",
        "/api/v1/orders/order_00000000000000000000000000000000/events".into(),
        Some(sk),
        None,
    )
    .await;
    let keys = Keys {
        sk,
        pk,
        ak: ak.as_str(),
    };
    webhooks_to(&state, &receiver);
    record_route_problems(&state, &router, &order_id, &keys, &recorder).await;

    // The same Monokulo once its admin has given it an onion address: the
    // handshakes, every route with each key it takes, and the order, made
    // again and read, now with their onion links.
    let onion_state = state
        .clone()
        .with_options(&options(Some(PUBLIC_URI), true))
        .await;
    let onion = recorder.with_onion();
    let onion_router = onion.around(build_router(onion_state.clone()));
    let finished = finish_plugin(&onion_router, &onion_state, &store_id).await;
    webhooks_to(&onion_state, &receiver);
    assert_eq!(finished["onion_api"], format!("{ONION_URI}/api/v1"));
    let connected = connect_app(&onion_router, &store_id, &cookie).await;
    assert_eq!(connected["onion_api"], format!("{ONION_URI}/api/v1"));
    let onion_ak = connected["app_key"].as_str().unwrap().to_owned();
    for key in [None, Some(sk), Some(pk), Some(onion_ak.as_str())] {
        call(&onion_router, "GET", "/api/v1".into(), key, None).await;
    }
    let made = call(
        &onion_router,
        "POST",
        "/api/v1/orders".into(),
        Some(sk),
        Some(order_input.clone()),
    )
    .await;
    assert_eq!(body_json(made).await["order_id"], order_id.as_str());
    let onion_keys = Keys {
        sk,
        pk,
        ak: onion_ak.as_str(),
    };
    for handler in catalog::handlers() {
        // The app disconnects itself last; the document is the same.
        if handler.method == "DELETE"
            || handler.template == super::super::v1::openapi::Path::TEMPLATE
        {
            continue;
        }
        for key in onion_keys.admitted(handler) {
            // The website's same key is its own and makes another order:
            // that is made last (see the end).
            if key == pk && handler.method == "POST" && handler.template == "/api/v1/orders" {
                continue;
            }
            let answer = call(
                &onion_router,
                handler.method,
                path_of(handler.template, &order_id),
                Some(key),
                body_for(handler, &order_input),
            )
            .await;
            assert!(
                answer.status().is_success(),
                "{} {}: {}",
                handler.method,
                handler.template,
                answer.status()
            );
        }
    }
    for (key, kind) in [(sk, "sk"), (pk, "pk"), (onion_ak.as_str(), "ak")] {
        record_order_events(&onion_router, &onion, &order_id, key, kind).await;
    }
    call(
        &onion_router,
        "DELETE",
        "/api/v1/app".into(),
        Some(&onion_ak),
        None,
    )
    .await;

    record_test_webhooks(&router, sk).await;

    // Last, the app disconnects itself; its key no longer works.
    call(&router, "DELETE", "/api/v1/app".into(), Some(&ak), None).await;
    call(&router, "GET", "/api/v1".into(), Some(&ak), None).await;

    // Last of all (so no list shows it), the website, with the same
    // idempotency key, on each network: its keys are its own (anyone can
    // send the public key), so it gets an order of its own, never the
    // server's, and the same one again on the onion.
    let mut by_website = Vec::new();
    for router in [&router, &onion_router] {
        let made = call(
            router,
            "POST",
            "/api/v1/orders".into(),
            Some(pk),
            Some(order_input.clone()),
        )
        .await;
        assert_eq!(made.status(), StatusCode::CREATED);
        by_website.push(body_json(made).await);
    }
    assert_ne!(by_website[0]["order_id"], order_id.as_str());
    assert_ne!(by_website[0]["address"], by_app["address"]);
    assert_eq!(by_website[1]["order_id"], by_website[0]["order_id"]);

    let recorded = recorder.take();
    // The first of each method, route, key, network and status, for a
    // problem each problem, and for a stream the one its events were read
    // from.
    let mut seen = std::collections::HashSet::new();
    let kept: Vec<_> = recorded
        .into_iter()
        .filter(|e| {
            let problem = e
                .response
                .as_ref()
                .and_then(|r| r.get("type"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            seen.insert((
                e.method.clone(),
                e.route.clone(),
                e.key.clone(),
                e.onion,
                e.status,
                problem,
                e.response.is_some(),
            ))
        })
        .collect();
    stable(kept)
}

/// A state with no web address (`public_uri`) set yet.
async fn state_without_address(engine: axum::Router) -> AppState {
    state(engine, None, false).await
}

/// A challenge (`crate::abuse::challenge`: the hex of
/// `pow|<difficulty>|<expires>|<client>|<random>`, a dot, the hex of its
/// HMAC) with its two random parts, the nonce and the HMAC under this
/// process's random key, made zeros of the same length. Those are random
/// so no one can forge or guess a challenge, and stay so: everything else
/// in it (its expiry on the recording's stopped clock among them) is as
/// recorded. `None` for anything else.
fn challenge_stand_in(value: &str) -> Option<String> {
    let (payload, mac) = value.split_once('.')?;
    let text = String::from_utf8(hex::decode(payload).ok()?).ok()?;
    if !text.starts_with("pow|") || hex::decode(mac).ok()?.len() != 32 {
        return None;
    }
    let (fields, random) = text.rsplit_once('|')?;
    let payload = format!("{fields}|{}", "0".repeat(random.len()));
    Some(format!(
        "{}.{}",
        hex::encode(payload),
        "0".repeat(mac.len())
    ))
}

/// `exchanges` with what changes from run to run replaced by stable
/// stand-ins of the same shape, the same one each time the same value
/// appears: ids, keys and addresses (random, and the engine's), times
/// (the engine's clock isn't the recording's to stop) and a challenge's
/// random parts ([`challenge_stand_in`]). The rest is made the same each
/// run where it's recorded: the abuse protection's clock is stopped
/// ([`abuse`]), and the exchanges are made one after another, in order.
pub fn stable(exchanges: Vec<Exchange>) -> Vec<Exchange> {
    let mut names: HashMap<String, String> = HashMap::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut rename = |value: &str| -> Option<String> {
        let is_hex = |s: &str| s.len() >= 16 && s.chars().all(|c| c.is_ascii_hexdigit());
        // `order_…`, `sk_…`, `whsec_…`: a prefix and random hex.
        let kind = match value.split_once('_') {
            Some((prefix, rest))
                if !prefix.is_empty()
                    && prefix.chars().all(|c| c.is_ascii_lowercase())
                    && is_hex(rest) =>
            {
                Some(prefix.to_owned())
            }
            _ if value.len() == 95
                && value.starts_with(['4', '5', '7', '8'])
                && value.chars().all(|c| c.is_ascii_alphanumeric()) =>
            {
                Some("address".to_owned())
            }
            _ if value.len() == 36
                && value.chars().filter(|c| *c == '-').count() == 4
                && is_hex(&value.replace('-', "")) =>
            {
                Some("uuid".to_owned())
            }
            _ => None,
        }?;
        if let Some(name) = names.get(value) {
            return Some(name.clone());
        }
        let n = counts.entry(kind.clone()).or_insert(0);
        *n += 1;
        let name = match kind.as_str() {
            "address" => format!("8{}{:04}", "A".repeat(90), n),
            "uuid" => format!("00000000-0000-4000-8000-{n:012}"),
            prefix => {
                let width = value.len() - prefix.len() - 1;
                format!("{prefix}_{n:0width$}")
            }
        };
        names.insert(value.to_owned(), name.clone());
        Some(name)
    };
    fn walk(value: &mut Value, rename: &mut dyn FnMut(&str) -> Option<String>) {
        match value {
            Value::String(s) => {
                // Whole values, and ids inside paths.
                if let Some(stand_in) = challenge_stand_in(s) {
                    *s = stand_in;
                } else if let Some(name) = rename(s) {
                    *s = name;
                } else {
                    let parts: Vec<String> = s
                        .split('/')
                        .map(|part| rename(part).unwrap_or_else(|| part.to_owned()))
                        .collect();
                    *s = parts.join("/");
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|v| walk(v, rename)),
            Value::Object(map) => {
                let created = map.get("created_at").and_then(Value::as_i64);
                for (key, v) in map.iter_mut() {
                    if key.ends_with("_at") || key == "at" {
                        if let Some(at) = v.as_i64() {
                            // Times: a fixed moment, keeping how long an
                            // order runs (to the minute).
                            let base = RECORDED_AT;
                            *v = match (key.as_str(), created) {
                                ("expires_at", Some(created)) => {
                                    Value::from(base + ((at - created) + 30) / 60 * 60)
                                }
                                _ => Value::from(base),
                            };
                            continue;
                        }
                    }
                    // How long something took (a test delivery's
                    // `duration_ms`): a few milliseconds, the same each
                    // run.
                    if key.ends_with("_ms") && v.is_u64() {
                        *v = Value::from(DURATION_MS);
                        continue;
                    }
                    walk(v, rename);
                }
            }
            _ => {}
        }
    }
    exchanges
        .into_iter()
        .map(|mut exchange| {
            let mut path = Value::String(exchange.path.clone());
            walk(&mut path, &mut rename);
            exchange.path = path.as_str().unwrap_or_default().to_owned();
            if let Some(request) = &mut exchange.request {
                walk(request, &mut rename);
            }
            if let Some(response) = &mut exchange.response {
                walk(response, &mut rename);
            }
            exchange
        })
        .collect()
}

/// The examples, the OpenAPI document and the reference, written from
/// recorded exchanges.
pub struct Reference {
    pub examples: Vec<Exchange>,
    pub openapi: Value,
    pub reference: Value,
}

impl Reference {
    /// Written from `examples`, and checked: the document is OpenAPI 3.1
    /// and every example in it fits its schema, every link names a route,
    /// and every resource, relation, problem and handshake is there with
    /// its page. Panics when it isn't so.
    pub fn of(examples: Vec<Exchange>) -> Self {
        every_link_names_a_route(&examples);
        every_link_to_an_example_leads_to_one(&examples);
        every_key_answers_on_both_networks(&examples);
        let openapi = openapi::build(&examples);
        let reference = reference::catalog(&examples);
        let written = Reference {
            examples,
            openapi,
            reference,
        };
        written.check();
        written
    }

    /// Its files, by name: `examples.json`, `openapi.json` and
    /// `reference.json`.
    pub fn files(&self) -> [(&'static str, String); 3] {
        [
            (
                "examples.json",
                serde_json::to_string_pretty(&self.examples).expect("JSON") + "\n",
            ),
            ("openapi.json", openapi::to_text(&self.openapi)),
            ("reference.json", reference::to_text(&self.reference)),
        ]
    }

    /// Writes its files to `dir`.
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        for (name, text) in self.files() {
            std::fs::write(dir.join(name), text)?;
        }
        Ok(())
    }

    fn check(&self) {
        let document = &self.openapi;
        assert_eq!(document["openapi"], "3.1.0");
        let examples = examples_fit_their_schemas(document);
        assert!(examples > 20, "only {examples} examples in the document");
        every_answer_has_an_example(document);
        // No field is ever `null`, in a schema or in anything recorded:
        // one with no value is left out.
        let nulls = nulls_in(document, "#");
        assert!(nulls.is_empty(), "the document has null at {nulls:#?}");
        assert!(
            !openapi::to_text(document).contains("`null`"),
            "a description still says a field may be `null`"
        );
        for exchange in &self.examples {
            let what = format!("{} {}", exchange.method, exchange.path);
            for body in [&exchange.request, &exchange.response]
                .into_iter()
                .flatten()
            {
                let nulls = nulls_in(body, &what);
                assert!(nulls.is_empty(), "an exchange has null at {nulls:#?}");
            }
        }
        // Every page the API links, and both handshakes, are in it.
        for resource in catalog::resources() {
            assert!(
                !document["paths"][resource.template]["get"].is_null()
                    || !document["paths"][resource.template].is_null(),
                "{} isn't in the document",
                resource.template
            );
        }
        for handshake in catalog::handshakes() {
            let operation =
                &document["paths"][handshake.template][handshake.method.to_ascii_lowercase()];
            assert!(
                !operation.is_null(),
                "{} isn't in the document",
                handshake.template
            );
            for problem in handshake.problems {
                assert!(
                    !operation["responses"][problem.status().as_u16().to_string()].is_null(),
                    "{} doesn't list {}",
                    handshake.template,
                    problem.slug()
                );
            }
        }
        // Each stream says what it sends, and shows it.
        for resource in catalog::resources()
            .into_iter()
            .filter(|r| r.kind == Kind::Stream)
        {
            let stream = &document["paths"][resource.template]["get"]["responses"]["200"]
                ["content"]["text/event-stream"];
            assert!(
                stream["x-events"]
                    .as_object()
                    .is_some_and(|e| !e.is_empty()),
                "{} lists no events",
                resource.template
            );
            let mut examples: Vec<&Value> = vec![&stream["example"]];
            if let Some(named) = stream["examples"].as_object() {
                examples = named.values().map(|e| &e["value"]).collect();
            }
            assert!(
                examples
                    .iter()
                    .all(|e| e.as_str().is_some_and(|e| e.starts_with("event: "))),
                "{} has no recorded example",
                resource.template
            );
        }
        let reference = &self.reference;
        let resources = reference["resources"].as_array().unwrap();
        assert_eq!(resources.len(), catalog::resources().len());
        for resource in catalog::resources() {
            let page = format!("{DOCS}/rels/{}", resource.rel);
            assert!(
                resources
                    .iter()
                    .any(|r| r["rel"] == resource.rel && r["page"] == page),
                "{} has no page at {page}",
                resource.rel
            );
        }
        // Every onion link's relation has a page too, named apart from
        // every resource's.
        let rels = reference["rels"].as_array().unwrap();
        let onion_rels = catalog::rels();
        assert_eq!(rels.len(), onion_rels.len());
        for rel in &onion_rels {
            assert!(rel.rel.starts_with("onion-"), "{}", rel.rel);
            assert!(
                catalog::resources().iter().all(|r| r.rel != rel.rel),
                "{} is a resource's relation too",
                rel.rel
            );
            assert!(
                rels.iter().any(|r| r["rel"] == rel.rel
                    && r["page"] == format!("{DOCS}/rels/{}", rel.rel)
                    && !r["description"].as_str().unwrap_or_default().is_empty()),
                "{} has no page",
                rel.rel
            );
        }
        let problems = reference["problems"].as_array().unwrap();
        for problem in Problem::ALL {
            assert!(
                problems
                    .iter()
                    .any(|p| p["slug"] == problem.slug() && p["type"] == problem.type_uri()),
                "{} isn't in the reference",
                problem.slug()
            );
        }
    }
}

/// Where `value` has `null` (as a value, or as a schema's `type`), each
/// as a path under `at`.
pub fn nulls_in(value: &Value, at: &str) -> Vec<String> {
    let mut found = Vec::new();
    match value {
        Value::Null => found.push(at.to_owned()),
        Value::String(s)
            if s == "null"
                && (at.ends_with("/type")
                    || at
                        .rsplit_once('/')
                        .is_some_and(|(up, _)| up.ends_with("/type"))) =>
        {
            found.push(at.to_owned())
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                found.extend(nulls_in(item, &format!("{at}/{i}")));
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                found.extend(nulls_in(item, &format!("{at}/{key}")));
            }
        }
        _ => {}
    }
    found
}

/// Records the exchanges against `engine` and writes the reference to
/// `dir`.
pub async fn write(engine: axum::Router, dir: &Path) -> std::io::Result<()> {
    Reference::of(record(engine).await).write(dir)
}

/// Every example in `document` fits the schema beside it (a stream's
/// events, each event's data); returns how many there are.
pub fn examples_fit_their_schemas(document: &Value) -> usize {
    let step = openapi::pointer_step;
    let validate = |pointer: String, example: &Value, what: &str| {
        let mut schema = document.clone();
        schema["$ref"] = Value::String(pointer);
        let validator =
            jsonschema::validator_for(&schema).unwrap_or_else(|e| panic!("{what}: {e}"));
        let errors: Vec<String> = validator
            .iter_errors(example)
            .map(|e| e.to_string())
            .collect();
        assert!(
            errors.is_empty(),
            "{what}: the example doesn't fit:\n{}\n{example:#}",
            errors.join("\n")
        );
    };
    let mut checked = 0;
    for (route, operations) in document["paths"].as_object().expect("paths") {
        for (verb, operation) in operations.as_object().expect("operations") {
            let responses = operation["responses"].as_object().expect("responses");
            for (status, response) in responses {
                let Some(content) = response["content"].as_object() else {
                    continue;
                };
                for (media_type, entry) in content {
                    let what = format!("{verb} {route} {status} {media_type}");
                    let at = format!(
                        "#/paths/{}/{verb}/responses/{status}/content/{}",
                        step(route),
                        step(media_type)
                    );
                    if media_type == "text/event-stream" {
                        // Each event's data fits its event's schema.
                        let mut texts: Vec<&str> = entry["example"].as_str().into_iter().collect();
                        if let Some(named) = entry["examples"].as_object() {
                            texts.extend(named.values().filter_map(|e| e["value"].as_str()));
                        }
                        for block in texts
                            .into_iter()
                            .flat_map(|text| text.split("\n\n"))
                            .filter(|b| !b.is_empty())
                        {
                            let name = block
                                .lines()
                                .find_map(|l| l.strip_prefix("event: "))
                                .expect("an event");
                            let data = block
                                .lines()
                                .find_map(|l| l.strip_prefix("data: "))
                                .expect("data");
                            let data = serde_json::from_str(data)
                                .unwrap_or(Value::String(data.to_owned()));
                            assert!(
                                !entry["x-events"][name].is_null(),
                                "{what}: {name} isn't listed"
                            );
                            validate(
                                format!("{at}/x-events/{}/data", step(name)),
                                &data,
                                &format!("{what} {name}"),
                            );
                            checked += 1;
                        }
                        continue;
                    }
                    let mut examples: Vec<&Value> = entry.get("example").into_iter().collect();
                    if let Some(named) = entry["examples"].as_object() {
                        examples.extend(named.values().map(|e| &e["value"]));
                    }
                    for example in examples {
                        validate(format!("{at}/schema"), example, &what);
                        checked += 1;
                    }
                }
            }
        }
    }
    checked
}

/// Every answer the document gives an operation (each status, each
/// media type but a page's HTML) has a recorded example, and each problem
/// it lists has one of its own, so no gap in the recording goes unnoticed:
/// a handshake's success with and without an onion address and each
/// problem it answers, each route's answers and each of its problems.
fn every_answer_has_an_example(document: &Value) {
    let mut missing = Vec::new();
    for (template, operations) in document["paths"].as_object().into_iter().flatten() {
        for (method, operation) in operations.as_object().into_iter().flatten() {
            let Some(responses) = operation["responses"].as_object() else {
                continue;
            };
            for (status, response) in responses {
                for (media, content) in response["content"].as_object().into_iter().flatten() {
                    // A page's HTML, and the document itself, which
                    // can't be its own example.
                    if media == "text/html" || media == OPENAPI_JSON {
                        continue;
                    }
                    let has = !content["example"].is_null()
                        || content["examples"]
                            .as_object()
                            .is_some_and(|examples| !examples.is_empty());
                    if !has {
                        missing.push(format!("{method} {template} {status} {media}"));
                    }
                    // Each problem listed there, one by one.
                    let types = &content["schema"]["allOf"][1]["properties"]["type"]["enum"];
                    for named in types.as_array().into_iter().flatten() {
                        let slug = named
                            .as_str()
                            .and_then(|t| t.rsplit('/').next())
                            .unwrap_or_default();
                        if content["examples"][slug].is_null() {
                            missing.push(format!("{method} {template} {status} {slug}"));
                        }
                    }
                }
            }
        }
    }
    assert!(missing.is_empty(), "no recorded example for: {missing:#?}");
}

/// Every link in the examples to a resource with recorded examples leads
/// to one of them (the same order, not another): the examples are one
/// sequence, which a client can follow link by link. The website's own
/// order (its idempotency keys are its own, so it never gets the
/// server's) starts a sequence of its own, not followed further.
fn every_link_to_an_example_leads_to_one(recorded: &[Exchange]) {
    let path_of = |href: &str| href.split('?').next().unwrap_or(href).to_owned();
    let mut checked = 0;
    for exchange in recorded {
        let Some(response) = &exchange.response else {
            continue;
        };
        let websites = (exchange.method == "POST" && exchange.key.as_deref() == Some("pk"))
            .then(|| response["order_id"].as_str())
            .flatten();
        let mut stack = vec![response];
        while let Some(value) = stack.pop() {
            match value {
                Value::Object(map) => {
                    for (rel, link) in map
                        .get("_links")
                        .and_then(Value::as_object)
                        .into_iter()
                        .flatten()
                    {
                        let Some(href) = link["href"].as_str() else {
                            continue;
                        };
                        let path = path_of(href.strip_prefix(ONION_URI).unwrap_or(href));
                        let Some(route) = route_of(&path).filter(|r| r.contains('{')) else {
                            continue;
                        };
                        let examples: Vec<String> = recorded
                            .iter()
                            .filter(|e| e.route == route && e.method == "GET")
                            .map(|e| path_of(&e.path))
                            .collect();
                        if examples.is_empty() || websites.is_some_and(|id| path.contains(id)) {
                            continue;
                        }
                        assert!(
                            examples.contains(&path),
                            "{} {} links {rel} to {path}, but the examples of {route} are {examples:?}",
                            exchange.method,
                            exchange.path
                        );
                        checked += 1;
                    }
                    stack.extend(map.values());
                }
                Value::Array(items) => stack.extend(items),
                _ => {}
            }
        }
    }
    assert!(checked > 5, "only {checked} links to examples checked");
}

/// The API route template `path` is on.
fn route_of(path: &str) -> Option<&'static str> {
    catalog::resources()
        .iter()
        .map(|r| r.template)
        .find(|template| {
            let (a, b): (Vec<&str>, Vec<&str>) =
                (template.split('/').collect(), path.split('/').collect());
            a.len() == b.len()
                && a.iter()
                    .zip(&b)
                    .all(|(t, p)| t == p || (t.starts_with('{') && !p.is_empty()))
        })
}

/// Every route of the API was answered with success for every key it
/// takes (`pk`, `sk`, `ak`, or none), both on a Monokulo without an onion
/// address and on one with: each answer the onion address may change is
/// recorded both ways (the document shows the onion one where it differs).
/// The OpenAPI document itself, which no key changes, is the exception.
fn every_key_answers_on_both_networks(recorded: &[Exchange]) {
    use super::auth::Key;
    let mut missing = Vec::new();
    for handler in catalog::handlers() {
        let document = catalog::resources()
            .into_iter()
            .any(|r| r.template == handler.template && r.kind == Kind::Document);
        if document {
            continue;
        }
        let keys = [
            (None, None),
            (Some(Key::Pk), Some("pk")),
            (Some(Key::Sk), Some("sk")),
            (Some(Key::Ak), Some("ak")),
        ];
        for (key, name) in keys {
            if !handler.who.admits(key) {
                continue;
            }
            for onion in [false, true] {
                let found = recorded.iter().any(|e| {
                    e.method == handler.method
                        && e.route == handler.template
                        && e.key.as_deref() == name
                        && e.onion == onion
                        && e.status < 300
                });
                if !found {
                    missing.push(format!(
                        "{} {} with {}{}",
                        handler.method,
                        handler.template,
                        name.unwrap_or("no key"),
                        if onion { ", with an onion address" } else { "" }
                    ));
                }
            }
        }
    }
    assert!(missing.is_empty(), "no recorded answer for: {missing:#?}");
}

fn every_link_names_a_route(recorded: &[Exchange]) {
    let templates: Vec<&str> = catalog::resources().iter().map(|r| r.template).collect();
    let matches = |href: &str| {
        let path = href.split('?').next().unwrap_or(href);
        templates.iter().any(|template| {
            let (a, b): (Vec<&str>, Vec<&str>) =
                (template.split('/').collect(), path.split('/').collect());
            a.len() == b.len()
                && a.iter()
                    .zip(&b)
                    .all(|(t, p)| t == p || (t.starts_with('{') && !p.is_empty()))
        })
    };
    let mut checked = 0;
    for exchange in recorded {
        let Some(response) = &exchange.response else {
            continue;
        };
        let mut stack = vec![response];
        while let Some(value) = stack.pop() {
            match value {
                Value::Object(map) => {
                    if let Some(Value::Object(links)) = map.get("_links") {
                        for (rel, link) in links {
                            if rel == "curies" {
                                continue;
                            }
                            let href = link["href"].as_str().unwrap();
                            if href.starts_with("https://") {
                                continue;
                            }
                            // An onion link: a route, on the onion address.
                            let href = match href.strip_prefix(ONION_URI) {
                                Some(path) => {
                                    assert!(rel.starts_with("mk:onion-"), "{rel}: {href}");
                                    path
                                }
                                None => href,
                            };
                            assert!(matches(href), "{rel}: {href} is no route");
                            checked += 1;
                        }
                    }
                    if let Some(Value::Object(templates)) = map.get("_templates") {
                        for (name, template) in templates {
                            let target = template["target"].as_str().unwrap();
                            assert!(matches(target), "{name}: {target} is no route");
                            checked += 1;
                        }
                    }
                    stack.extend(map.values());
                }
                Value::Array(items) => stack.extend(items),
                _ => {}
            }
        }
    }
    assert!(checked > 10, "only {checked} links checked");
}
