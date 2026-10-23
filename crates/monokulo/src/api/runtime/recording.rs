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
            &content_type,
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
pub struct Recorder(Arc<Mutex<Vec<Exchange>>>);

impl Recorder {
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
        if let Some(exchange) = exchange {
            recorder.push(exchange);
        }
        response
    }

    fn push(&self, exchange: Exchange) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(exchange);
    }

    /// The exchanges so far, taken.
    pub fn take(&self) -> Vec<Exchange> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// A state like a real instance's, on `engine`: a web address and an
/// onion address (`None` for one with no web address yet).
async fn state(engine: axum::Router, public_uri: Option<&str>) -> AppState {
    let public = public_uri
        .map(|uri| format!("public_uri = \"{uri}\"\n"))
        .unwrap_or_default();
    AppState {
        engine: crate::http::Engine::new(crate::engine_client::EngineClient::embedded_for_tests(
            engine,
        )),
        ..AppState::for_tests()
    }
    .with_options(&format!(
        "{public}onion_uri = \"{ONION_URI}\"\n\n[tor]\nmode = \"external\"\n\n[abuse]\nonion_listener = \"127.0.0.1:8082\"\n"
    ))
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
}

/// A [`Fixture`] on `engine` (a test engine's router) at `public_uri`,
/// whose router records into `recorder`.
pub async fn fixture(engine: axum::Router, public_uri: &str, recorder: &Recorder) -> Fixture {
    let state = state(engine, Some(public_uri)).await;
    let router = recorder.around(build_router(state.clone()));
    // The seeded admin: this instance keeps sign-up to invites.
    let admin = admin_session_cookie(&router).await;
    let session = admin.strip_prefix("session=").unwrap().to_owned();
    let store = store_with_keys(&router, &state, &session, "bakery.example", "XMR").await;
    let cookie = format!("session={session}");
    let connected = connect_app(&router, &store.id, &cookie).await;
    Fixture {
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

/// One of each kind of exchange, against `engine` (a test engine's
/// router): the API with each key, its streams, its problems, the
/// handshakes and theirs.
pub async fn record(engine: axum::Router) -> Vec<Exchange> {
    let recorder = Recorder::default();
    let fixture = fixture(engine.clone(), PUBLIC_URI, &recorder).await;
    assert_eq!(
        fixture.connected,
        json!({"app_key": fixture.ak, "api": "/api/v1", "onion_api": format!("{ONION_URI}/api/v1")})
    );
    let Fixture {
        state,
        router,
        store_id,
        sk,
        pk,
        ak,
        ..
    } = fixture;
    let (sk, pk) = (sk.as_str(), pk.as_str());
    let call = |method: &'static str, uri: String, key: Option<&str>, body: Option<Value>| {
        let request = api_request(method, &uri, key, body);
        let router = router.clone();
        async move { router.oneshot(request).await.unwrap() }
    };

    // The plugin's handshake, and each problem it can be made to answer.
    let code = plugin_connect_code(&state, &store_id);
    let finish = crate::http::connect::FINISH.replace("{slug}", "prestashop");
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
    router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(&finish)
                .body(Body::from("token=conn_0"))
                .unwrap(),
        )
        .await
        .unwrap();
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
    let unset = recorder.around(build_router(state_without_address(engine).await));
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

    // The entry and the store as each key sees them.
    let entry = call("GET", "/api/v1".into(), None, None).await;
    let etag = entry.headers()[header::ETAG].to_str().unwrap().to_owned();
    for key in [Some(sk), Some(pk), Some(ak.as_str())] {
        call("GET", "/api/v1".into(), key, None).await;
        call("GET", "/api/v1/store".into(), key, None).await;
    }
    call("GET", "/api/v1/store".into(), None, None).await;
    call("GET", "/api/v1".into(), Some("sk_0000"), None).await;
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

    // The store's sale events, from before an app's order is made.
    let stream = call("GET", "/api/v1/events".into(), Some(&ak), None).await;
    assert_eq!(stream.status(), StatusCode::OK);
    let sale = call(
        "POST",
        "/api/v1/orders".into(),
        Some(&ak),
        Some(json!({"amount": "24.50", "currency": "XMR", "merchant_order_id": "#1042", "idempotency_key": "till:payment-9f2c"})),
    )
    .await;
    assert_eq!(sale.status(), StatusCode::CREATED);
    let events = read_events(stream, 2).await;
    recorder.push(Exchange {
        method: "GET".into(),
        route: super::super::v1::events::Path::TEMPLATE.into(),
        path: "/api/v1/events".into(),
        key: Some("ak".into()),
        request: None,
        status: 200,
        response: Some(events),
    });

    let made = call(
        "POST",
        "/api/v1/orders".into(),
        Some(sk),
        Some(json!({
            "amount": "0.25",
            "currency": "XMR",
            "merchant_order_id": "1042",
            "description": "Order #1042: Blue mug + 2 more",
            "show_merchant_order_id": true,
            "idempotency_key": "order-1042-total-025",
        })),
    )
    .await;
    assert_eq!(made.status(), StatusCode::CREATED);
    let order = body_json(made).await;
    let order_id = order["order_id"].as_str().unwrap().to_owned();
    call("GET", "/api/v1/orders?limit=1".into(), Some(sk), None).await;
    call(
        "POST",
        "/api/v1/orders".into(),
        Some(pk),
        Some(json!({"amount": "0.1", "currency": "XMR"})),
    )
    .await;
    call(
        "POST",
        "/api/v1/orders".into(),
        Some(sk),
        Some(json!({"amount": "twelve", "currency": "XMR"})),
    )
    .await;
    call(
        "POST",
        "/api/v1/orders".into(),
        Some(sk),
        Some(json!({"amount": "1", "currency": "NOPE"})),
    )
    .await;
    call("GET", format!("/api/v1/orders/{order_id}"), Some(sk), None).await;
    call("GET", format!("/api/v1/orders/{order_id}"), Some(pk), None).await;
    call(
        "GET",
        "/api/v1/orders/order_00000000000000000000000000000000".into(),
        Some(sk),
        None,
    )
    .await;
    // One order's changes: the first event is the order as it is.
    let order_stream = call(
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
        request: None,
        status: 200,
        response: Some(events),
    });
    call("GET", "/api/v1/app".into(), Some(&ak), None).await;
    call("GET", "/api/v1/app".into(), Some(sk), None).await;
    let logs = json!({"entries": [{"level": "error", "message": "Monokulo could not be reached"}]});
    call("POST", "/api/v1/logs".into(), Some(sk), Some(logs.clone())).await;
    state
        .db
        .lock()
        .set_client_logging(&crate::db::ConnectionId::new(store_id.clone()), true)
        .unwrap();
    call("POST", "/api/v1/logs".into(), Some(sk), Some(logs)).await;
    call("GET", "/api/v1/openapi.json".into(), None, None).await;
    call("DELETE", "/api/v1/app".into(), Some(&ak), None).await;
    call("GET", "/api/v1".into(), Some(&ak), None).await;

    let recorded = recorder.take();
    // The first of each method, route, key and status, for a problem each
    // problem, and for a stream the one its events were read from.
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
    state(engine, None).await
}

/// `exchanges` with what changes from run to run (ids, keys, addresses,
/// times) replaced by stable stand-ins of the same shape, the same one
/// each time the same value appears.
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
                if let Some(name) = rename(s) {
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
                            let base = 1_767_225_600;
                            *v = match (key.as_str(), created) {
                                ("expires_at", Some(created)) => {
                                    Value::from(base + ((at - created) + 30) / 60 * 60)
                                }
                                _ => Value::from(base),
                            };
                            continue;
                        }
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
            assert!(
                stream["example"]
                    .as_str()
                    .is_some_and(|e| e.starts_with("event: ")),
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
                        let Some(text) = entry["example"].as_str() else {
                            continue;
                        };
                        for block in text.split("\n\n").filter(|b| !b.is_empty()) {
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

/// Every link and form target in the recorded answers is a route this
/// router has, or a page outside the API.
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
