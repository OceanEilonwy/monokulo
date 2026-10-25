//! The API as its clients meet it: which keys it takes, what each is
//! offered, the links it gives (under a path prefix too), its event
//! streams and how its entry is cached.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::catalog;
use super::recording::{fixture, Fixture, Recorder, ONION_URI, PUBLIC_URI};
use crate::http::test_support::{api_request, assert_problem, body_json};

async fn engine() -> engine_test_support::TestEngineHandle {
    engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await
}

async fn get(router: &axum::Router, uri: &str, key: Option<&str>) -> axum::response::Response {
    router
        .clone()
        .oneshot(api_request("GET", uri, key, None))
        .await
        .unwrap()
}

/// Makes an order with `key`: its body.
async fn make_order(fixture: &Fixture, key: &str) -> Value {
    let made = fixture
        .router
        .clone()
        .oneshot(api_request(
            "POST",
            "/api/v1/orders",
            Some(key),
            Some(json!({"amount": "0.5", "currency": "XMR"})),
        ))
        .await
        .unwrap();
    assert_eq!(made.status(), StatusCode::CREATED);
    body_json(made).await
}

/// A request that came over Tor (the onion listener's connection info).
fn over_tor(mut request: Request<Body>) -> Request<Body> {
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        crate::abuse::proxy_protocol::OnionPeer {
            source: "fc00:dead:beef:4dad::1".parse().unwrap(),
        },
    ));
    request
}

/// Every href and form target in `body`.
fn hrefs(body: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![body];
    while let Some(value) = stack.pop() {
        match value {
            Value::Object(map) => {
                if let Some(Value::Object(links)) = map.get("_links") {
                    for (rel, link) in links {
                        if rel != "curies" {
                            out.push(link["href"].as_str().unwrap().to_owned());
                        }
                    }
                }
                if let Some(Value::Object(forms)) = map.get("_templates") {
                    for form in forms.values() {
                        out.push(form["target"].as_str().unwrap().to_owned());
                    }
                }
                stack.extend(map.values());
            }
            Value::Array(items) => stack.extend(items),
            _ => {}
        }
    }
    out
}

/// A shop's or an app's server can't solve a browser's proof of work, so
/// no machine route ever answers `challenge-required`: not the plugin's
/// or the app's handshake, not the API with the store's secret key or an
/// app's, not any log report, even under attack and past the soft limit
/// (past the hard limit each is `rate-limited` with `Retry-After`, tested
/// in `http::abuse`). Only an anonymous caller, a browser with the public
/// key or none, is asked to solve one.
#[tokio::test]
async fn no_machine_route_is_ever_challenged() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    fixture.state.abuse.reload(crate::abuse::AbuseConfig {
        under_attack: true,
        soft_per_min: 1,
        hard_per_min: 10_000,
        signed_in_per_min: 10_000,
        per_store_key_per_min: 10_000,
        client_logs_per_min: 10_000,
        ..fixture.state.abuse.config()
    });
    let from_one_peer = |mut request: Request<Body>| {
        request.extensions_mut().insert(axum::extract::ConnectInfo(
            "198.51.100.9:4000".parse::<std::net::SocketAddr>().unwrap(),
        ));
        request
    };
    let json_post = |uri: &str, body: Value| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let (sk, ak) = (fixture.sk.as_str(), fixture.ak.as_str());
    let logs = json!({"entries": [{"level": "error", "message": "unreachable"}]});
    let order = json!({"amount": "0.5", "currency": "XMR"});
    for _ in 0..3 {
        let requests = [
            json_post(
                &crate::http::connect::FINISH.replace("{slug}", "prestashop"),
                json!({"token": "conn_0"}),
            ),
            json_post(
                crate::http::apps::CONNECT,
                json!({"token": "ct_0", "slug": "monokular"}),
            ),
            api_request("GET", "/api/v1", Some(sk), None),
            api_request("GET", "/api/v1", Some(ak), None),
            api_request("POST", "/api/v1/orders", Some(sk), Some(order.clone())),
            api_request("POST", "/api/v1/orders", Some(ak), Some(order.clone())),
            api_request("POST", "/api/v1/logs", Some(sk), Some(logs.clone())),
            json_post("/telemetry/client", json!({"entries": []})),
        ];
        for request in requests {
            let (method, uri) = (request.method().clone(), request.uri().clone());
            let response = fixture
                .router
                .clone()
                .oneshot(from_one_peer(request))
                .await
                .unwrap();
            assert!(
                !response
                    .headers()
                    .contains_key(crate::http::abuse::CHALLENGE_HEADER),
                "{method} {uri} was challenged"
            );
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert!(
                !String::from_utf8_lossy(&body).contains("challenge-required"),
                "{method} {uri} ({status}) was challenged"
            );
        }
    }
    // The same peer with no key is a browser's, and is asked.
    let anonymous = fixture
        .router
        .clone()
        .oneshot(from_one_peer(api_request("GET", "/api/v1", None, None)))
        .await
        .unwrap();
    assert!(anonymous
        .headers()
        .contains_key(crate::http::abuse::CHALLENGE_HEADER));
}

/// A key this Monokulo doesn't know is refused with `401` on every route
/// of the API, the entry and the OpenAPI document included, never answered
/// as if no key were sent; with no key at all the entry stays open.
#[tokio::test]
async fn an_unknown_key_is_refused_on_every_route() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let order = make_order(&fixture, &fixture.sk).await;
    let order_id = order["order_id"].as_str().unwrap();
    // A disconnected app's key: a real key that no longer works.
    let gone = fixture
        .router
        .clone()
        .oneshot(api_request(
            "DELETE",
            "/api/v1/app",
            Some(&fixture.ak),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::NO_CONTENT);
    let unknown = [
        "Bearer sk_0123456789abcdef0123456789abcdef",
        "Bearer pk_0123456789abcdef0123456789abcdef",
        "Bearer ak_0123456789abcdef0123456789abcdef",
        &format!("Bearer {}", fixture.ak),
        "Bearer nonsense",
        "Bearer ",
        "Basic dXNlcjpwYXNz",
    ];
    let mut routes = 0;
    for handler in catalog::handlers() {
        let uri = handler.template.replace("{order_id}", order_id);
        for authorization in &unknown {
            let request = Request::builder()
                .method(handler.method)
                .uri(&uri)
                .header(header::AUTHORIZATION, *authorization)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap();
            let response = fixture.router.clone().oneshot(request).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{} {uri} with {authorization:?}",
                handler.method
            );
            assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
            assert_problem(response, "unauthorized").await;
        }
        routes += 1;
    }
    assert!(routes >= 9, "only {routes} routes");
    // No key: the entry is open, with nothing offered.
    let open = get(&fixture.router, "/api/v1", None).await;
    assert_eq!(open.status(), StatusCode::OK);
    let body = body_json(open).await;
    assert!(body.get("_templates").is_none(), "{body}");
}

/// The entry offers each key the forms it may use, and only those: a shop's
/// server sends its logs through the entry's form, never a path it builds.
#[tokio::test]
async fn the_entry_offers_each_key_its_own_forms() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    for (key, forms) in [
        (None, vec![]),
        (Some(fixture.pk.as_str()), vec!["create-order"]),
        (
            Some(fixture.sk.as_str()),
            vec!["create-order", "send-logs", "send-test-webhook"],
        ),
        (
            Some(fixture.ak.as_str()),
            vec!["create-order", "disconnect"],
        ),
    ] {
        let body = body_json(get(&fixture.router, "/api/v1", key).await).await;
        let mut offered: Vec<&str> = body["_templates"]
            .as_object()
            .map(|templates| templates.keys().map(String::as_str).collect())
            .unwrap_or_default();
        offered.sort_unstable();
        assert_eq!(offered, forms, "the forms offered to {key:?}");
    }
    let body = body_json(get(&fixture.router, "/api/v1", Some(&fixture.sk)).await).await;
    assert_eq!(body["_templates"]["send-logs"]["method"], "POST");
    assert_eq!(body["_templates"]["send-logs"]["target"], "/api/v1/logs");
}

/// An order links its page on the merchant's dashboard for the store's
/// secret key only, wherever the order is read (made, read, listed, on its
/// stream); the merchant, signed in, opens it.
#[tokio::test]
async fn an_order_links_its_dashboard_page_for_the_secret_key_only() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let made = make_order(&fixture, &fixture.sk).await;
    let order_id = made["order_id"].as_str().unwrap();
    let page = format!("/stores/{}/orders/{order_id}", fixture.store_id);
    assert_eq!(made["_links"]["mk:dashboard-order"]["href"], page.as_str());
    assert_eq!(made["_links"]["mk:dashboard-order"]["type"], "text/html");
    let read = body_json(
        get(
            &fixture.router,
            &format!("/api/v1/orders/{order_id}"),
            Some(&fixture.sk),
        )
        .await,
    )
    .await;
    assert_eq!(read["_links"]["mk:dashboard-order"]["href"], page.as_str());
    let listed = body_json(get(&fixture.router, "/api/v1/orders", Some(&fixture.sk)).await).await;
    assert!(listed["orders"]
        .as_array()
        .unwrap()
        .iter()
        .all(|o| o["_links"]["mk:dashboard-order"]["href"]
            .as_str()
            .is_some_and(|h| h.starts_with("/stores/"))));
    for key in [&fixture.pk, &fixture.ak] {
        let read = body_json(
            get(
                &fixture.router,
                &format!("/api/v1/orders/{order_id}"),
                Some(key),
            )
            .await,
        )
        .await;
        assert!(read["_links"]["mk:checkout"].is_object());
        assert!(read["_links"].get("mk:dashboard-order").is_none(), "{read}");
    }
    let app_order = make_order(&fixture, &fixture.ak).await;
    assert!(app_order["_links"].get("mk:dashboard-order").is_none());
    // The link is the merchant's page.
    let opened = fixture
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&page)
                .header(header::COOKIE, &fixture.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(opened.status(), StatusCode::OK);
}

/// Reads server-sent events from `response` until one named `name`: its
/// data.
async fn next_event(body: &mut Body, name: &str) -> Value {
    use http_body_util::BodyExt;
    let mut text = String::new();
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(10), body.frame())
            .await
            .expect("an event in time")
            .expect("the stream is open")
            .unwrap();
        let Ok(data) = frame.into_data() else {
            continue;
        };
        text.push_str(std::str::from_utf8(&data).unwrap());
        while let Some(end) = text.find("\n\n") {
            let block: String = text.drain(..end + 2).collect();
            if block.lines().any(|l| l == format!("event: {name}")) {
                let data = block
                    .lines()
                    .find_map(|l| l.strip_prefix("data: "))
                    .unwrap();
                return serde_json::from_str(data).unwrap();
            }
        }
    }
}

/// A sale event links its order, so an app follows the link rather than
/// building the order's URL; and the link works with the app's key.
#[tokio::test]
async fn a_sale_event_links_its_order() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let stream = get(&fixture.router, "/api/v1/events", Some(&fixture.ak)).await;
    assert_eq!(stream.status(), StatusCode::OK);
    let mut body = stream.into_body();
    next_event(&mut body, "ready").await;
    let made = make_order(&fixture, &fixture.ak).await;
    let sale = next_event(&mut body, "sale.waiting").await;
    let order_id = made["order_id"].as_str().unwrap();
    assert_eq!(sale["order_id"], order_id);
    let href = sale["_links"]["mk:order"]["href"].as_str().unwrap();
    assert_eq!(href, format!("/api/v1/orders/{order_id}"));
    assert_eq!(sale["_links"]["curies"][0]["name"], "mk");
    let order = get(&fixture.router, href, Some(&fixture.ak)).await;
    assert_eq!(order.status(), StatusCode::OK);
    assert_eq!(body_json(order).await["order_id"], order_id);
}

/// Behind a reverse proxy that serves Monokulo under a path
/// (`public_uri = "https://shop.example/monokulo"`), every link, form
/// target and `Location` the API gives, and the handshakes' `api`, start
/// with that path, so a client resolving them against the host it called
/// reaches Monokulo; onion links, and everything answered over Tor, don't.
#[tokio::test]
async fn links_carry_the_path_monokulo_is_served_under() {
    let engine = engine().await;
    let fixture = fixture(
        engine.router(),
        "https://shop.example/monokulo/",
        &Recorder::default(),
    )
    .await;
    assert_eq!(fixture.connected["api"], "/monokulo/api/v1");
    assert_eq!(
        fixture.connected["onion_api"],
        format!("{ONION_URI}/api/v1")
    );
    let made = fixture
        .router
        .clone()
        .oneshot(api_request(
            "POST",
            "/api/v1/orders",
            Some(&fixture.sk),
            Some(json!({"amount": "1", "currency": "XMR"})),
        ))
        .await
        .unwrap();
    assert_eq!(made.status(), StatusCode::CREATED);
    let location = made.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        location.starts_with("/monokulo/api/v1/orders/order_"),
        "{location}"
    );
    let order = body_json(made).await;
    let order_id = order["order_id"].as_str().unwrap();
    let entry = body_json(get(&fixture.router, "/api/v1", Some(&fixture.sk)).await).await;
    let read = body_json(
        get(
            &fixture.router,
            &format!("/api/v1/orders/{order_id}"),
            Some(&fixture.sk),
        )
        .await,
    )
    .await;
    let mut checked = 0;
    for body in [&entry, &order, &read] {
        for href in hrefs(body) {
            if href.starts_with("https://") {
                continue;
            }
            if let Some(path) = href.strip_prefix(ONION_URI) {
                assert!(!path.starts_with("/monokulo"), "{href}");
            } else {
                assert!(href.starts_with("/monokulo/"), "{href}");
            }
            checked += 1;
        }
    }
    assert!(checked > 15, "only {checked} links");
    assert_eq!(
        read["_links"]["mk:checkout"]["href"],
        format!("/monokulo/checkout/{order_id}")
    );
    assert_eq!(
        read["_links"]["mk:onion-checkout"]["href"],
        format!("{ONION_URI}/checkout/{order_id}")
    );
    assert_eq!(
        entry["_links"]["mk:onion-api"]["href"],
        format!("{ONION_URI}/api/v1")
    );

    // Over Tor: no prefix (tor reaches Monokulo directly).
    let request = over_tor(api_request("GET", "/api/v1", Some(&fixture.sk), None));
    let onion = body_json(fixture.router.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(onion["_links"]["self"]["href"], "/api/v1");
    assert!(
        hrefs(&onion).iter().all(|h| !h.contains("/monokulo/")),
        "{onion}"
    );

    // The plugin's handshake too.
    let code = credentials::auth::generate_connect_token();
    fixture
        .state
        .db
        .lock()
        .create_connect_token(
            &code.hash(),
            &crate::db::ConnectionId::new(fixture.store_id.clone()),
            "nonce",
            crate::now_unix(),
        )
        .unwrap();
    let finish = |code: &str| {
        Request::builder()
            .method("POST")
            .uri(crate::http::connect::FINISH.replace("{slug}", "prestashop"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"token": code}).to_string()))
            .unwrap()
    };
    let finished = fixture
        .router
        .clone()
        .oneshot(finish(code.expose()))
        .await
        .unwrap();
    assert_eq!(finished.status(), StatusCode::OK);
    let finished = body_json(finished).await;
    assert_eq!(finished["api"], "/monokulo/api/v1");
    assert_eq!(finished["onion_api"], format!("{ONION_URI}/api/v1"));
}

/// The entry can be kept: `Cache-Control: private` for an hour, `Vary:
/// Authorization`, and an `ETag` that answers `304` while it holds; each
/// key's entry is its own.
#[tokio::test]
async fn the_entry_can_be_cached() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let mut tags = std::collections::HashSet::new();
    for key in [
        None,
        Some(&fixture.pk),
        Some(&fixture.sk),
        Some(&fixture.ak),
    ] {
        let key = key.map(String::as_str);
        let first = get(&fixture.router, "/api/v1", key).await;
        assert_eq!(first.status(), StatusCode::OK);
        let headers = first.headers().clone();
        assert_eq!(headers[header::CACHE_CONTROL], "private, max-age=3600");
        assert!(
            headers
                .get_all(header::VARY)
                .iter()
                .any(|v| v.to_str().unwrap().contains("Authorization")),
            "{headers:?}"
        );
        let etag = headers[header::ETAG].to_str().unwrap().to_owned();
        assert!(tags.insert(etag.clone()), "{key:?} shares an ETag");
        let mut again = Request::builder()
            .uri("/api/v1")
            .header(header::IF_NONE_MATCH, &etag);
        if let Some(key) = key {
            again = again.header(header::AUTHORIZATION, format!("Bearer {key}"));
        }
        let again = fixture
            .router
            .clone()
            .oneshot(again.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED, "{key:?}");
        assert_eq!(again.headers()[header::ETAG], etag.as_str());
        assert_eq!(
            again.headers()[header::CACHE_CONTROL],
            "private, max-age=3600"
        );
        use http_body_util::BodyExt;
        assert!(again
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty());
    }
    // A tag it no longer has: the entry, afresh.
    let stale = fixture
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1")
                .header(header::IF_NONE_MATCH, "\"0123\"")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::OK);
    // Nothing else in the API is kept.
    let store = get(&fixture.router, "/api/v1/store", Some(&fixture.sk)).await;
    assert_eq!(store.headers()[header::CACHE_CONTROL], "no-store");
    assert!(store.headers().get(header::ETAG).is_none());
}

mod orders;
