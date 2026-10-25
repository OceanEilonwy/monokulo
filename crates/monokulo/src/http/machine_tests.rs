//! What machine clients get from the whole router: every JSON route's
//! errors are documented problems, and the onion links are there exactly
//! while the instance has an onion address, while a visitor on Tor is
//! never given the web address.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use tower::ServiceExt;

use super::test_support::{
    api_request, assert_problem, body_json, body_text, signed_up_and_logged_in_session_token,
    store_with_keys, TestStore,
};
use super::{build_router, AppState};
use crate::engine_client::EngineClient;

const WEB: &str = "https://pay.example.test";
const WEB_HOST: &str = "pay.example.test";
/// The Tor Project's own v3 address, as this instance's onion address.
const ONION: &str = "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
const PASSWORD: &str = "correct horse battery staple";
/// A mainnet address of another wallet, for a refund address.
const REFUND_ADDRESS: &str = "44AFFq5kSiGBoZ4NMDwYtN18obc8AemS33DBLWs3H7otXft3XjrpDtQGv7SqSsaBYBb98uNbr2VBBEt7f2wfn3RVGQBEP3A";

/// An instance on a real engine with `public_uri` set, with an onion
/// address (an external tor's) when `onion`.
async fn instance(onion: bool) -> (AppState, engine_test_support::TestEngineHandle) {
    let engine =
        engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
    let mut options = format!("public_uri = \"{WEB}\"\n");
    if onion {
        options.push_str(&format!(
            "onion_uri = \"{ONION}\"\n[tor]\nmode = \"external\"\n[abuse]\nonion_listener = \"127.0.0.1:8082\"\n"
        ));
    }
    options.push_str("[signup]\nmode = \"public\"\n");
    let state = AppState {
        engine: crate::http::Engine::new(EngineClient::embedded_for_tests(engine.router())),
        ..AppState::for_tests()
    }
    .with_options(&options)
    .await;
    (state, engine)
}

/// A merchant with a store: their session token and the store.
async fn merchant(router: &Router, state: &AppState, email: &str) -> (String, TestStore) {
    let session = signed_up_and_logged_in_session_token(router, email, PASSWORD).await;
    let store = store_with_keys(router, state, &session, "shop.example.com", "XMR").await;
    (session, store)
}

async fn send(router: &Router, request: Request<Body>) -> Response {
    router.clone().oneshot(request).await.unwrap()
}

/// A new order of the store's, made with `key`: its body.
async fn new_order(router: &Router, key: &str) -> serde_json::Value {
    let made = send(
        router,
        api_request(
            "POST",
            "/api/v1/orders",
            Some(key),
            Some(serde_json::json!({ "amount": "1.00", "currency": "XMR" })),
        ),
    )
    .await;
    assert_eq!(made.status(), StatusCode::CREATED);
    body_json(made).await
}

fn request(method: &str, uri: &str) -> axum::http::request::Builder {
    Request::builder().method(method).uri(uri)
}

/// Every route a machine calls outside `/api/v1` (the point of sale, the
/// checkout's refund address and live updates, client logs, key bundles,
/// the admin's own scripts), on every way its request can be wrong, answers
/// with a documented problem. (`/api/v1` is checked by its contract test,
/// the handshakes by `http::connect`'s and `http::apps`'s, rate limits by
/// `http::abuse`'s.)
#[tokio::test]
async fn every_json_route_answers_its_errors_with_documented_problems() {
    let (state, _engine) = instance(false).await;
    let router = build_router(state.clone());
    let (session, store) = merchant(&router, &state, "problems@example.com").await;
    let cookie = format!("session={session}");
    let id = &store.id;
    let order = new_order(&router, &store.public_key).await;
    let order_id = order["order_id"].as_str().unwrap();
    let json = |method: &str, uri: &str, cookie: Option<&str>, body: &str| {
        let mut builder = request(method, uri)
            .header("accept", "application/json")
            .header("content-type", "application/json");
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        builder.body(Body::from(body.to_owned())).unwrap()
    };
    let signed_in = Some(cookie.as_str());

    // The point of sale: signed out, someone else's store, a body or query
    // it can't read, an order that isn't there.
    let cases: Vec<(Request<Body>, &str)> = vec![
        (
            json("GET", &format!("/stores/{id}/pos/orders"), None, ""),
            "signed-out",
        ),
        (
            json("POST", &format!("/stores/{id}/pos/orders"), None, "{}"),
            "signed-out",
        ),
        (
            json("GET", "/stores/st_nobody/pos/orders", signed_in, ""),
            "not-found",
        ),
        (
            request("POST", &format!("/stores/{id}/pos/orders"))
                .header("cookie", &cookie)
                .body(Body::from("amount=1"))
                .unwrap(),
            "unsupported-media-type",
        ),
        (
            json(
                "POST",
                &format!("/stores/{id}/pos/orders"),
                signed_in,
                "{oops",
            ),
            "invalid-request",
        ),
        (
            json("POST", &format!("/stores/{id}/pos/orders"), signed_in, "{}"),
            "invalid-request",
        ),
        (
            json(
                "GET",
                &format!("/stores/{id}/pos/orders?limit=many"),
                signed_in,
                "",
            ),
            "invalid-request",
        ),
        (
            json("GET", &format!("/stores/{id}/pos/events"), signed_in, ""),
            "invalid-request",
        ),
        (
            json(
                "GET",
                &format!("/stores/{id}/pos/orders/order_nope"),
                signed_in,
                "",
            ),
            "not-found",
        ),
        (
            json(
                "GET",
                &format!("/stores/{id}/pos/orders/order_nope/status"),
                signed_in,
                "",
            ),
            "not-found",
        ),
        (
            json(
                "POST",
                &format!("/stores/{id}/pos/orders/order_nope/cancel"),
                signed_in,
                "",
            ),
            "not-found",
        ),
        (
            json(
                "POST",
                &format!("/stores/{id}/pos/orders/order_nope/background"),
                signed_in,
                "",
            ),
            "not-found",
        ),
        // The point of sale's log reports: off for this store, or not one.
        (
            json("POST", &format!("/stores/{id}/pos/logs"), signed_in, "{}"),
            "logs-off",
        ),
        (
            json("POST", "/stores/st_nobody/pos/logs", signed_in, "{}"),
            "not-found",
        ),
        // A browser's error report that isn't one.
        (
            json("POST", "/telemetry/client", None, "not json"),
            "invalid-request",
        ),
        // A key bundle that expired.
        (
            json("GET", "/key-custody/bundles/kb_gone", None, ""),
            "not-found",
        ),
        // The checkout's live updates and refund address, for its script.
        (
            json("GET", "/checkout/order_nope/events", None, ""),
            "not-found",
        ),
        (
            request("POST", "/checkout/order_nope/refund-address")
                .header("accept", "application/json")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("refund_address=x"))
                .unwrap(),
            "not-found",
        ),
        (
            request("POST", &format!("/checkout/{order_id}/refund-address"))
                .header("accept", "application/json")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("refund_address="))
                .unwrap(),
            "invalid-refund-address",
        ),
        (
            request("POST", &format!("/checkout/{order_id}/refund-address"))
                .header("accept", "application/json")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("refund_address=4notanaddress"))
                .unwrap(),
            "invalid-refund-address",
        ),
        (
            request("POST", &format!("/checkout/{order_id}/refund-address"))
                .header("accept", "application/json")
                .body(Body::from("refund_address=x"))
                .unwrap(),
            "invalid-request",
        ),
        // A merchant who isn't the admin, on the admin's routes.
        (
            json("GET", "/admin/query/value", signed_in, ""),
            "admin-only",
        ),
    ];
    for (request, slug) in cases {
        let what = format!("{} {}", request.method(), request.uri());
        let response = send(&router, request).await;
        let status = response.status();
        let body = assert_problem(response, slug).await;
        assert!(
            status.is_client_error() || status.is_server_error(),
            "{what}: {body}"
        );
    }

    // The same refund address form without JavaScript still gets the page.
    let page = send(
        &router,
        request("POST", &format!("/checkout/{order_id}/refund-address"))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("refund_address="))
            .unwrap(),
    )
    .await;
    assert!(page.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    // And with JavaScript a good address is saved.
    let saved = send(
        &router,
        request("POST", &format!("/checkout/{order_id}/refund-address"))
            .header("accept", "application/json")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(format!("refund_address={REFUND_ADDRESS}")))
            .unwrap(),
    )
    .await;
    assert_eq!(saved.status(), StatusCode::OK);
    assert_eq!(body_json(saved).await["ok"], true);
}

/// Without an onion address there are no onion links anywhere: not on an
/// order (made, read or listed), not on the entry.
#[tokio::test]
async fn without_an_onion_address_there_are_no_onion_links() {
    let (state, _engine) = instance(false).await;
    let router = build_router(state.clone());
    let (_, store) = merchant(&router, &state, "no-onion@example.com").await;
    let created = new_order(&router, &store.secret_key).await;
    let order_id = created["order_id"].as_str().unwrap();
    let read = body_json(
        send(
            &router,
            api_request(
                "GET",
                &format!("/api/v1/orders/{order_id}"),
                Some(&store.secret_key),
                None,
            ),
        )
        .await,
    )
    .await;
    let list = body_json(
        send(
            &router,
            api_request("GET", "/api/v1/orders", Some(&store.secret_key), None),
        )
        .await,
    )
    .await;
    let entry = body_json(send(&router, api_request("GET", "/api/v1", None, None)).await).await;
    for body in [&created, &read, &list, &entry] {
        let text = body.to_string();
        assert!(!text.contains("onion"), "{text}");
    }
    assert_eq!(
        created["_links"]["mk:checkout"]["href"],
        format!("/checkout/{order_id}")
    );
}

/// A request that came over Tor: the circuit it came on.
fn over_tor(mut request: Request<Body>) -> Request<Body> {
    request
        .extensions_mut()
        .insert(ConnectInfo(crate::tor::circuit_peer(7)));
    request
}

/// What a response says, headers and body, for looking for an address in.
async fn everything_in(response: Response) -> (StatusCode, String) {
    let status = response.status();
    let headers = format!("{:?}", response.headers());
    (status, format!("{headers}\n{}", body_text(response).await))
}

/// A visitor on Tor, or a client that called over the onion address, is
/// never given the web address: not in the API's entry or orders (whose
/// links are relative, or absolute on the onion address), not on the
/// checkout page, its share page, its live updates, nor in a redirect or
/// `Location`; and a merchant on Tor is given no web address on their
/// store's pages, nor in the code an app connects with.
#[tokio::test]
async fn nothing_served_over_tor_names_the_web_address() {
    let (state, _engine) = instance(true).await;
    let router = build_router(state.clone());
    let (session, store) = merchant(&router, &state, "onion-visitor@example.com").await;
    let cookie = format!("session={session}");
    let signed_in = |method: &str, uri: &str, form: &str| {
        request(method, uri)
            .header("cookie", &cookie)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(form.to_owned()))
            .unwrap()
    };
    let (pk, sk) = (store.public_key.as_str(), store.secret_key.as_str());

    let made = send(
        &router,
        over_tor(api_request(
            "POST",
            "/api/v1/orders",
            Some(pk),
            Some(serde_json::json!({ "amount": "1.00", "currency": "XMR" })),
        )),
    )
    .await;
    assert_eq!(made.status(), StatusCode::CREATED);
    let location = made.headers()["location"].to_str().unwrap().to_owned();
    assert!(location.starts_with("/api/v1/orders/"), "{location}");
    let (_, made) = everything_in(made).await;
    assert!(!made.contains(WEB_HOST), "{made}");
    let order_id = location.rsplit('/').next().unwrap().to_owned();
    assert!(
        made.contains(&format!("{ONION}/checkout/{order_id}")),
        "{made}"
    );

    let checkout = format!("/checkout/{order_id}");
    let reads: Vec<Request<Body>> = vec![
        api_request("GET", "/api/v1", Some(pk), None),
        api_request("GET", "/api/v1", None, None),
        api_request("GET", &location, Some(pk), None),
        api_request("GET", "/api/v1/orders", Some(sk), None),
        api_request("GET", "/api/v1/store", Some(sk), None),
        request("GET", &checkout).body(Body::empty()).unwrap(),
        request("GET", &format!("{checkout}?view=compact&refund=false"))
            .body(Body::empty())
            .unwrap(),
        request("GET", &format!("{checkout}/share"))
            .body(Body::empty())
            .unwrap(),
        // Without JavaScript: a refused address re-renders the page, a good
        // one redirects back to it.
        request("POST", &format!("{checkout}/refund-address"))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("refund_address=nope"))
            .unwrap(),
        request("POST", &format!("{checkout}/refund-address?view=compact"))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(format!("refund_address={REFUND_ADDRESS}")))
            .unwrap(),
        request("GET", "/checkout/order_nope")
            .body(Body::empty())
            .unwrap(),
        signed_in("GET", &format!("/stores/{}/settings", store.id), ""),
        signed_in(
            "GET",
            &format!("/stores/{}/settings?tab=wallet", store.id),
            "",
        ),
        signed_in(
            "POST",
            &format!("/stores/{}/settings/apps/connect", store.id),
            "device_name=Till",
        ),
    ];
    for read in reads {
        let what = format!("{} {}", read.method(), read.uri());
        let (status, text) = everything_in(send(&router, over_tor(read)).await).await;
        assert!(
            status.is_success() || status.is_redirection() || status == StatusCode::NOT_FOUND,
            "{what}: {status} {text}"
        );
        assert!(
            !text.contains(WEB_HOST),
            "{what} names the web address: {text}"
        );
        assert!(!text.contains("onion-location"), "{what}: {text}");
    }

    // The live updates: their headers and first events.
    let events = send(
        &router,
        over_tor(
            request("GET", &format!("{checkout}/events"))
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await;
    assert_eq!(events.status(), StatusCode::OK);
    assert!(!format!("{:?}", events.headers()).contains(WEB_HOST));
    let mut body = events.into_body();
    let mut seen = String::new();
    while !seen.contains("event: status") {
        use http_body_util::BodyExt;
        let frame = tokio::time::timeout(std::time::Duration::from_secs(10), body.frame())
            .await
            .expect("the first events")
            .expect("a frame")
            .expect("no error");
        if let Ok(data) = frame.into_data() {
            seen.push_str(&String::from_utf8_lossy(&data));
        }
    }
    assert!(!seen.contains(WEB_HOST), "{seen}");
}

/// No handler writes an error as `{"error": ...}` JSON again: a machine's
/// errors are problems (`crate::api::runtime::problem`). Reads the crate's
/// own sources, leaving out tests (whose fake engines speak the engine's
/// own format).
#[test]
fn no_handler_answers_with_an_error_object() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let pattern = regex::Regex::new(r#"json!\(\s*\{\s*"error"\s*:"#).unwrap();
    let mut found = Vec::new();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        if name == "tests.rs" || name.ends_with("_tests.rs") {
            continue;
        }
        // A Windows checkout may have CRLF line endings.
        let text = std::fs::read_to_string(&file)
            .unwrap()
            .replace("\r\n", "\n");
        // Everything before the file's own test module.
        let code = text.split("#[cfg(test)]\nmod tests").next().unwrap();
        for m in pattern.find_iter(code) {
            let line = code[..m.start()].lines().count();
            found.push(format!("{}:{line}", file.display()));
        }
    }
    assert!(
        found.is_empty(),
        "answer with a Problem (crate::api::runtime::problem), not {{\"error\": ...}}: {found:?}"
    );
}
