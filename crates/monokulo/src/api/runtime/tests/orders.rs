//! Making orders through the API as a shop's website does (the public
//! key, a real engine, real fiat pricing), and what surrounds it: the
//! stores' pricing settings, the plugin's logs, the embed library and the
//! static files the pages around a checkout load.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::engine_client::EngineClient;
use crate::http::{build_router, AppState};

// This module's own tests deliberately exercise a real fiat quote (via a
// local mock Coingecko server below), not just XMR - `api::v1::orders::create`
// is the real production storefront-facing endpoint, so its own tests are
// the ones that should prove a real fiat currency actually works end to
// end, unlike most other modules' tests (see `orders.rs`'s own doc
// comment on why those use `"XMR"` instead).
const TEST_CURRENCY: &str = "USD";
// A mock price of exactly $1.00 makes the resulting piconero-per-unit
// exactly 1e12 (`1_000_000_000_000.0 / 1.0`), a clean round number to
// assert against without any floating-point rounding to account for.
const TEST_RATE_PICONERO_PER_UNIT: u64 = 1_000_000_000_000;

/// Spins up a real local HTTP server standing in for Coingecko - same
/// no-mocking-library pattern `shared::exchange_rate`'s own tests use.
/// Returns the base URL a `CoingeckoRateProvider` can be pointed at.
async fn spawn_mock_coingecko() -> String {
    async fn price() -> axum::response::Response {
        use axum::response::IntoResponse;
        (
            [("content-type", "application/json")],
            r#"{"monero":{"usd":1.0}}"#,
        )
            .into_response()
    }
    let app = Router::new().route("/api/v3/simple/price", axum::routing::get(price));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn test_exchange_rate_provider(
) -> std::sync::Arc<crate::exchange_rate_config::ExchangeRateProviders> {
    let base_url = spawn_mock_coingecko().await;
    std::sync::Arc::new(
        crate::exchange_rate_config::ExchangeRateProviders::coingecko_only(base_url),
    )
}

async fn test_state_with_real_engine() -> (AppState, engine_test_support::TestEngineHandle) {
    let engine = engine_test_support::TestEngineConfig::new()
        .with_networks(&[monero::Network::Mainnet])
        .spawn()
        .await;
    let engine_client = EngineClient::embedded_for_tests(engine.router());
    let state = AppState {
        exchange_rate: test_exchange_rate_provider().await,
        engine: crate::http::Engine::new(engine_client),
        ..AppState::for_tests()
    };
    (state, engine)
}

use crate::http::test_support::body_json;

use crate::http::test_support::body_text;

use crate::http::test_support::signed_up_and_logged_in_session_token;

/// Makes a real store (and a real tenant on the real spawned engine)
/// for the given session through store setup, returning its public
/// key - the key a website's orders are made with.
async fn create_connection(router: &Router, state: &AppState, session_token: &str) -> String {
    create_connection_with_base_currency(router, state, session_token, "XMR").await
}

/// The same, with a caller-chosen base currency.
async fn create_connection_with_base_currency(
    router: &Router,
    state: &AppState,
    session_token: &str,
    base_currency: &str,
) -> String {
    // No two stores share a site.
    static STORES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = STORES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    crate::http::test_support::store_with_keys(
        router,
        state,
        session_token,
        &format!("shop-{n}.example.com"),
        base_currency,
    )
    .await
    .public_key
}

/// A website's order: `POST /api/v1/orders` with the store's public key.
fn create_order_request(pk: &str, amount: &str, currency: &str) -> Request<Body> {
    crate::http::test_support::api_request(
        "POST",
        "/api/v1/orders",
        Some(pk),
        Some(serde_json::json!({ "amount": amount, "currency": currency })),
    )
}

/// A storefront retrying `POST /api/v1/orders` with the same
/// idempotency key (its first answer was lost) gets the same order and
/// address; another key, another order; the key reused for a different
/// total is a 409; a malformed key is a 400.
#[tokio::test]
async fn a_retried_order_creation_with_one_key_makes_one_order() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "idempotent@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;
    let post = |body: serde_json::Value| {
        let (router, pk) = (router.clone(), pk.clone());
        async move {
            let response = router
                .oneshot(crate::http::test_support::api_request(
                    "POST",
                    "/api/v1/orders",
                    Some(&pk),
                    Some(body),
                ))
                .await
                .unwrap();
            (response.status(), body_json(response).await)
        }
    };
    let order = |key: &str, amount: &str| {
        serde_json::json!({
            "amount": amount,
            "currency": TEST_CURRENCY,
            "merchant_order_id": "wc-9",
            "idempotency_key": key,
        })
    };
    let (status, first) = post(order("wc:9:25.00", "25.00")).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, retry) = post(order("wc:9:25.00", "25.00")).await;
    assert_eq!(status, StatusCode::CREATED, "{retry}");
    assert_eq!(retry["order_id"], first["order_id"]);
    assert_eq!(retry["address"], first["address"]);

    let (status, other) = post(order("wc:9:30.00", "30.00")).await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
    assert_ne!(other["order_id"], first["order_id"]);

    let (status, clash) = post(order("wc:9:25.00", "26.00")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{clash}");
    assert_eq!(
        clash["type"],
        "https://docs.monokulo.com/problems/idempotency-conflict"
    );

    let (status, bad) = post(order("has space", "25.00")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    assert_eq!(
        bad["type"],
        "https://docs.monokulo.com/problems/invalid-request"
    );
}

/// An order's description is checked, trimmed, echoed and passed to
/// the engine, which keeps it; the reference shows to the payer unless
/// the caller says otherwise. A description over 140 characters, with
/// a line break, or blank, is a 400 naming it.
#[tokio::test]
async fn an_orders_description_reaches_the_engine_and_bad_ones_are_refused() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "described@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;
    let post = |body: serde_json::Value| {
        let (router, pk) = (router.clone(), pk.clone());
        async move {
            let response = router
                .oneshot(crate::http::test_support::api_request(
                    "POST",
                    "/api/v1/orders",
                    Some(&pk),
                    Some(body),
                ))
                .await
                .unwrap();
            (response.status(), body_text(response).await)
        }
    };
    let order = |extra: serde_json::Value| {
        let mut body = serde_json::json!({
            "amount": "10.00",
            "currency": TEST_CURRENCY,
            "merchant_order_id": "1042",
        });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };

    let (status, body) = post(order(serde_json::json!({ "description": "é".repeat(141) }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.contains("Description must be at most 140 characters"),
        "{body}"
    );
    assert!(body.contains("invalid-request"), "{body}");
    let (status, body) = post(order(
        serde_json::json!({ "description": "Blue mug\nRed mug" }),
    ))
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("Description must match"), "{body}");
    // Blank is no description at all: leave the field out instead.
    let (status, body) = post(order(serde_json::json!({ "description": "   " }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("Description must be at least 1"), "{body}");

    let (status, body) = post(order(
        serde_json::json!({ "description": format!("  {}  ", "é".repeat(140)) }),
    ))
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(created["description"], "é".repeat(140));
    assert_eq!(created["show_merchant_order_id"], true);

    let row = state
        .db
        .lock()
        .get_store_connection_by_public_key(&pk)
        .unwrap()
        .unwrap();
    let sk = crate::http::decrypt_sk(&state.encryption_key, &row).unwrap();
    let order_id =
        engine_protocol::ids::OrderId::new(created["order_id"].as_str().unwrap().to_string());
    let detail = state
        .engine
        .client
        .get_order_detail(&sk, &order_id)
        .await
        .unwrap();
    assert_eq!(detail.order.description, Some("é".repeat(140)));
    let metadata = state
        .db
        .lock()
        .get_order_currency_metadata(&row.id, &order_id)
        .unwrap()
        .unwrap();
    assert!(metadata.show_merchant_order_id);

    // Without one there is none; the reference can be kept off the
    // checkout.
    let (status, body) = post(order(
        serde_json::json!({ "show_merchant_order_id": false }),
    ))
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(created["description"], serde_json::Value::Null);
    assert_eq!(created["show_merchant_order_id"], false);
    let order_id =
        engine_protocol::ids::OrderId::new(created["order_id"].as_str().unwrap().to_string());
    let metadata = state
        .db
        .lock()
        .get_order_currency_metadata(&row.id, &order_id)
        .unwrap()
        .unwrap();
    assert!(!metadata.show_merchant_order_id);
}

/// Every static file a real page asks for is served, with the type its
/// extension needs (a font or script with the wrong type is refused by
/// the browser): the sign-up and login pages, a customer's checkout, and
/// the static files those scripts load in turn.
#[tokio::test]
async fn every_static_file_the_pages_reference_is_served_with_its_type() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "assets@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;
    let order = body_json(
        router
            .clone()
            .oneshot(create_order_request(&pk, "25.00", TEST_CURRENCY))
            .await
            .unwrap(),
    )
    .await;
    let checkout = format!("/checkout/{}", order["order_id"].as_str().unwrap());

    let mut referenced = std::collections::BTreeSet::new();
    for page in ["/signup", "/login", checkout.as_str()] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(page).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{page}");
        let html = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        for part in html.split("/static/").skip(1) {
            let name: String = part
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '.' || *c == '_')
                .collect();
            referenced.insert(name.trim_end_matches('.').to_string());
        }
    }
    // checkout.js loads the QR decoder; the POS loads its app and styles.
    referenced.extend(
        ["jsQR.js", "pos-app.js", "pos-app.css", "monokulo-client.js"].map(|name| {
            crate::assets::url(name)
                .trim_start_matches("/static/")
                .to_string()
        }),
    );
    assert!(
        referenced.iter().any(|name| name.ends_with(".woff2")),
        "{referenced:?}"
    );
    for name in &referenced {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/static/{name}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "/static/{name}");
        let content_type = response.headers()["content-type"]
            .to_str()
            .unwrap()
            .to_string();
        let expected = match name.rsplit('.').next().unwrap() {
            "js" => "javascript",
            "css" => "text/css",
            "svg" => "image/svg+xml",
            "woff2" => "font/woff2",
            other => panic!("unexpected static file type {other} ({name})"),
        };
        assert!(
            content_type.contains(expected),
            "/static/{name} served as {content_type}"
        );
        // Cacheable, and a browser that has it gets no body back.
        let etag = response.headers()["etag"].clone();
        assert!(
            response.headers().contains_key("cache-control"),
            "/static/{name}"
        );
        let again = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/static/{name}"))
                    .header("if-none-match", etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED, "/static/{name}");
        assert!(again
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty());
        assert!(
            !response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty(),
            "/static/{name} is empty"
        );
    }
}

/// Without JavaScript only the checkout embed refreshes by itself
/// (structured_logging.md D4); every other page is a snapshot with a
/// Reload button. (The challenge page's continue refresh is covered
/// by `views::challenge`.)
/// No answer repeats a header (but `Set-Cookie`): the API, its problems
/// and CORS preflights from another site, the checkout and its share
/// page, the merchant's pages, the point of sale and the static files each
/// send one `Vary` and one `Cache-Control`, whatever several layers added
/// to them. (The recorded API exchanges are checked the same way, in
/// `recording::exchange`.)
#[tokio::test]
async fn no_answer_repeats_a_header() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "one-vary@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;
    let store_id = state
        .db
        .lock()
        .get_store_connection_by_public_key(&pk)
        .unwrap()
        .unwrap()
        .id;
    let order = body_json(
        router
            .clone()
            .oneshot(create_order_request(&pk, "0.5", "XMR"))
            .await
            .unwrap(),
    )
    .await;
    let order_id = order["order_id"].as_str().unwrap().to_owned();
    let script = crate::assets::url("checkout.js");

    let origin = "https://elsewhere.example";
    let cookie = format!("session={session_token}");
    let requests: Vec<(&str, String, Option<&str>, bool)> = vec![
        ("GET", "/api/v1".into(), None, true),
        ("GET", "/api/v1".into(), Some(pk.as_str()), true),
        ("GET", "/api/v1/store".into(), Some(pk.as_str()), true),
        (
            "GET",
            format!("/api/v1/orders/{order_id}"),
            Some(pk.as_str()),
            true,
        ),
        ("GET", "/api/v1".into(), Some("sk_0000"), true),
        ("OPTIONS", "/api/v1/orders".into(), None, true),
        ("GET", "/api/v1/openapi.json".into(), None, true),
        ("GET", format!("/checkout/{order_id}"), None, false),
        ("GET", format!("/checkout/{order_id}"), None, true),
        ("GET", format!("/checkout/{order_id}/share"), None, false),
        ("GET", "/static/monokulo-client.js".into(), None, true),
        ("GET", script, None, false),
        ("GET", "/".into(), None, false),
        ("GET", "/login".into(), None, false),
        ("GET", "/status".into(), None, false),
        ("GET", format!("/stores/{store_id}"), Some("cookie"), false),
        (
            "GET",
            format!("/stores/{store_id}/pos"),
            Some("cookie"),
            false,
        ),
        (
            "GET",
            format!("/stores/{store_id}/pos/orders"),
            Some("cookie"),
            false,
        ),
        (
            "GET",
            format!("/stores/{store_id}/orders/{order_id}"),
            Some("cookie"),
            false,
        ),
    ];
    for (method, path, key, cross_origin) in requests {
        let mut request = Request::builder().method(method).uri(&path);
        match key {
            Some("cookie") => request = request.header("cookie", &cookie),
            Some(key) => request = request.header("authorization", format!("Bearer {key}")),
            None => {}
        }
        if cross_origin {
            request = request.header("origin", origin);
        }
        if method == "OPTIONS" {
            request = request
                .header("access-control-request-method", "POST")
                .header(
                    "access-control-request-headers",
                    "authorization, content-type",
                );
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let repeated = crate::http::repeated_headers(response.headers());
        assert!(
            repeated.is_empty(),
            "{method} {path} ({}) repeats {repeated:?}: {:?}",
            response.status(),
            response.headers()
        );
    }
}

#[tokio::test]
async fn only_the_checkout_refreshes_by_itself() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "no-refresh@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;
    let store_id = state
        .db
        .lock()
        .get_store_connection_by_public_key(&pk)
        .unwrap()
        .unwrap()
        .id;
    let order = body_json(
        router
            .clone()
            .oneshot(create_order_request(&pk, "25.00", TEST_CURRENCY))
            .await
            .unwrap(),
    )
    .await;
    let order_id = order["order_id"].as_str().unwrap().to_string();

    let get = |path: String| {
        Request::builder()
            .uri(path)
            .header("cookie", format!("session={session_token}"))
            .body(Body::empty())
            .unwrap()
    };
    for path in [
        "/".to_string(),
        "/login".into(),
        "/signup".into(),
        "/account".into(),
        "/account?tab=wallets".into(),
        "/setup".into(),
        "/account/wallets/add".into(),
        format!("/stores/{store_id}"),
        format!("/stores/{store_id}/settings"),
        format!("/stores/{store_id}/orders"),
        format!("/stores/{store_id}/orders/{order_id}"),
        format!("/stores/{store_id}/orders/new"),
    ] {
        let response = router.clone().oneshot(get(path.clone())).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let html = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(
            !html.contains("http-equiv=\"refresh\""),
            "{path} must not refresh by itself: {html}"
        );
    }
    let customer = Request::builder()
        .uri(format!("/checkout/{order_id}"))
        .body(Body::empty())
        .unwrap();
    let checkout = router.clone().oneshot(customer).await.unwrap();
    let html = String::from_utf8(
        checkout
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(
        html.contains("<noscript><meta http-equiv=\"refresh\""),
        "the checkout keeps its no-JS refresh: {html}"
    );
}

#[tokio::test]
async fn creating_a_real_order_through_the_public_endpoint_returns_a_real_address_and_order_id() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());

    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "pay-endpoint@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;

    let response = router
        .oneshot(create_order_request(&pk, "25.00", TEST_CURRENCY))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "expected the real engine to accept and create the order"
    );
    let location = response.headers()["location"].to_str().unwrap().to_string();
    let body = body_json(response).await;
    let obj = body.as_object().unwrap();
    assert!(obj
        .get("order_id")
        .unwrap()
        .as_str()
        .unwrap()
        .starts_with("order_"));
    assert!(!obj.get("address").unwrap().as_str().unwrap().is_empty());
    assert_eq!(
        obj.get("currency").unwrap().as_str().unwrap(),
        TEST_CURRENCY
    );
    assert_eq!(obj.get("amount").unwrap().as_str().unwrap(), "25.00");
    assert_eq!(
        location,
        format!("/api/v1/orders/{}", body["order_id"].as_str().unwrap())
    );
}

#[tokio::test]
async fn threshold_database_failure_rejects_order_even_with_zero_conf_default() {
    let (state, engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "threshold-db-failure@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;
    let row = state
        .db
        .lock()
        .get_store_connection_by_public_key(&pk)
        .unwrap()
        .unwrap();
    let sk = crate::http::decrypt_sk(&state.encryption_key, &row).unwrap();
    state
        .engine
        .client
        .set_confirmations_required(&sk, 0)
        .await
        .unwrap();
    state.db.lock().break_confirmation_thresholds_for_test();

    let response = router
        .oneshot(create_order_request(&pk, "25.00", TEST_CURRENCY))
        .await
        .unwrap();
    assert!(!response.status().is_success());
    let tenant_id = engine
        .store()
        .lock()
        .list_active_tenants()
        .unwrap()
        .into_iter()
        .find(|t| t.public_key == pk)
        .unwrap()
        .id;
    assert!(engine
        .store()
        .lock()
        .list_orders(&tenant_id, None, 10, None)
        .unwrap()
        .is_empty());
}

/// The plugin's forwarded errors (structured_logging.md 2.4), sent to
/// `POST /api/v1/logs`, need the store's own secret key (not its
/// public one) and its Diagnostics on, and are capped.
#[tokio::test]
async fn only_the_store_itself_can_forward_its_plugin_errors() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "plugin-logs@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;
    let row = state
        .db
        .lock()
        .get_store_connection_by_public_key(&pk)
        .unwrap()
        .unwrap();
    let sk = crate::http::decrypt_sk(&state.encryption_key, &row).unwrap();
    let forward = |key: Option<&str>, entries: usize| {
        let entries: Vec<_> = (0..entries)
                .map(|n| serde_json::json!({ "level": "error", "message": format!("failure {n}"), "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736" }))
                .collect();
        crate::http::test_support::api_request(
            "POST",
            "/api/v1/logs",
            key,
            Some(serde_json::json!({ "entries": entries })),
        )
    };
    assert_eq!(
        router
            .clone()
            .oneshot(forward(Some(sk.expose()), 2))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN,
        "Diagnostics is off"
    );
    state.db.lock().set_client_logging(&row.id, true).unwrap();
    assert_eq!(
        router
            .clone()
            .oneshot(forward(Some(sk.expose()), 2))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        router
            .clone()
            .oneshot(forward(None, 1))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        router
            .clone()
            .oneshot(forward(Some("sk_wrong"), 1))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        router
            .clone()
            .oneshot(forward(Some(&pk), 1))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN,
        "the public key may not"
    );
    assert_eq!(
        router
            .clone()
            .oneshot(forward(Some(sk.expose()), 21))
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn an_order_waits_for_its_stores_policy_edit() {
    let (state, engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "policy-edit-race@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;
    let guard = crate::confirmation_thresholds::lock_policy(&pk).await;
    let request = create_order_request(&pk, "25.00", TEST_CURRENCY);
    let mut task = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut task)
            .await
            .is_err()
    );
    let tenant_id = engine
        .store()
        .lock()
        .list_active_tenants()
        .unwrap()
        .into_iter()
        .find(|t| t.public_key == pk)
        .unwrap()
        .id;
    assert!(engine
        .store()
        .lock()
        .list_orders(&tenant_id, None, 10, None)
        .unwrap()
        .is_empty());
    drop(guard);
    assert_eq!(task.await.unwrap().status(), StatusCode::CREATED);
}

#[tokio::test]
async fn malformed_stored_threshold_cannot_fall_back_to_zero_conf() {
    let (state, engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "malformed-threshold@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;
    let row = state
        .db
        .lock()
        .get_store_connection_by_public_key(&pk)
        .unwrap()
        .unwrap();
    let sk = crate::http::decrypt_sk(&state.encryption_key, &row).unwrap();
    state
        .engine
        .client
        .set_confirmations_required(&sk, 0)
        .await
        .unwrap();
    state
        .db
        .lock()
        .replace_confirmation_thresholds(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            &row.id,
            &[],
            Some(("corrupt", "not-an-amount", 20, crate::now_unix())),
        )
        .unwrap();
    let response = router
        .oneshot(create_order_request(&pk, "25.00", TEST_CURRENCY))
        .await
        .unwrap();
    assert!(!response.status().is_success());
    let tenant_id = engine
        .store()
        .lock()
        .list_active_tenants()
        .unwrap()
        .into_iter()
        .find(|t| t.public_key == pk)
        .unwrap()
        .id;
    assert!(engine
        .store()
        .lock()
        .list_orders(&tenant_id, None, 10, None)
        .unwrap()
        .is_empty());
}

/// A real, previously-missing capability: `EngineClient::create_order`
/// used to silently drop `merchant_order_id` no matter what a caller
/// asked for - every order's own merchant order id always showed as
/// unset on the dashboard regardless. Proves it's genuinely recorded on
/// the engine now (not just echoed by monokulo), by reading it
/// back through the real dashboard order-detail page, not just this
/// endpoint's own response.
#[tokio::test]
async fn creating_a_real_order_with_a_merchant_order_id_records_it_on_the_engine() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());

    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "pay-endpoint-merchant-order-id@example.com",
        "correct horse battery staple",
    )
    .await;

    let store = crate::http::test_support::store_with_keys(
        &router,
        &state,
        &session_token,
        "shop.example.com",
        "XMR",
    )
    .await;
    let connection_id = store.id;

    let response = router
        .clone()
        .oneshot(crate::http::test_support::api_request(
            "POST",
            "/api/v1/orders",
            Some(&store.public_key),
            Some(serde_json::json!({
                "amount": "25.00",
                "currency": TEST_CURRENCY,
                "merchant_order_id": "order-1234",
            })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    let order_id = body["order_id"].as_str().unwrap().to_string();
    assert_eq!(
        body["merchant_order_id"], "order-1234",
        "expected the real merchant_order_id echoed back, got: {body}"
    );

    let detail_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/stores/{connection_id}/orders/{order_id}"))
                .header("cookie", format!("session={session_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(detail_response.status(), StatusCode::OK);
    let html = body_text(detail_response).await;
    assert!(
        html.contains("order-1234"),
        "expected the real merchant_order_id shown on the dashboard, got: {html}"
    );
}

/// The other half of the test above: proves the local fiat-metadata
/// row was actually recorded (not just the engine's own response
/// echoed back) by reading it straight out of monokulo's own `Db`.
#[tokio::test]
async fn creating_a_real_order_records_local_fiat_metadata() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());

    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "pay-endpoint-metadata@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;

    let response = router
        .oneshot(create_order_request(&pk, "10.00", TEST_CURRENCY))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    let order_id = body
        .as_object()
        .unwrap()
        .get("order_id")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();

    let connection_id = state
        .db
        .lock()
        .get_store_connection_by_public_key(&pk)
        .unwrap()
        .unwrap()
        .id;
    let metadata = state
        .db
        .lock()
        .get_order_currency_metadata(
            &connection_id,
            &engine_protocol::ids::OrderId::new(order_id.to_string()),
        )
        .unwrap();
    let metadata =
        metadata.expect("expected a real local fiat-metadata row for the order just created");
    assert_eq!(metadata.currency, TEST_CURRENCY);
    assert_eq!(metadata.amount, "10.00");
    assert_eq!(
        metadata.piconero_per_unit,
        shared::xmr_amount::Piconero(TEST_RATE_PICONERO_PER_UNIT)
    );

    // This store's base currency ("XMR", `create_connection`'s own
    // default) differs from the order's own currency ("USD"), so a
    // real second rate lookup (for "XMR" itself - always the identity
    // rate, regardless of provider, same as
    // `an_xmr_order_is_always_priced_at_the_identity_rate_regardless_of_the_stores_provider`
    // in `exchange_rate_config`) must have run and been snapshotted
    // separately from the order's own USD rate above.
    assert_eq!(metadata.store_base_currency, Some("XMR".to_string()));
    assert_eq!(
        metadata.base_currency_piconero_per_unit,
        Some(shared::xmr_amount::Piconero(TEST_RATE_PICONERO_PER_UNIT))
    );
    assert_eq!(
        metadata.confirmations_required_applied,
        Some(10),
        "no custom threshold exists, so the tenant's own default (10) applies"
    );
}

/// An order needs a key that names a store: an unknown one, or none,
/// is a 401.
#[tokio::test]
async fn creating_an_order_with_an_unknown_public_key_or_none_is_a_401() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());

    let response = router
        .clone()
        .oneshot(create_order_request(
            "pk_nonexistent",
            "25.00",
            TEST_CURRENCY,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = router
        .oneshot(crate::http::test_support::api_request(
            "POST",
            "/api/v1/orders",
            None,
            Some(serde_json::json!({ "amount": "25.00", "currency": TEST_CURRENCY })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// A local server standing in for CoinMarketCap's keyless price
/// conversion. `usd_price` is what one XMR costs in USD; `None` answers
/// with a body-level error, as the real API does when it is unavailable.
async fn spawn_mock_coinmarketcap(usd_price: Option<f64>) -> String {
    let body = match usd_price {
            Some(price) => format!(
                r#"{{"data":{{"id":328,"quote":{{"USD":{{"price":{price}}}}}}},"status":{{"error_code":0}}}}"#
            ),
            None => r#"{"status":{"error_code":"500","error_message":"The system is busy, please try again later!"}}"#.to_string(),
        };
    let app = Router::new().route(
        "/v2/tools/price-conversion",
        axum::routing::get(move || {
            let body = body.clone();
            async move {
                use axum::response::IntoResponse;
                ([("content-type", "application/json")], body).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// Submits the store settings page's provider form (`use_<provider>` and
/// `position_<provider>` fields) as the store's owner.
async fn save_provider_settings(
    router: &Router,
    session_token: &str,
    connection_id: &str,
    fields: &[(&str, &str)],
) {
    let body = fields
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/stores/{connection_id}/settings"))
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", format!("session={session_token}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        response.status().is_redirection() || response.status() == StatusCode::OK,
        "saving provider settings failed: {}",
        response.status()
    );
}

/// Creates an order and returns (provider recorded, piconero-per-unit
/// recorded) from monokulo's own database.
async fn order_pricing(state: &AppState, router: &Router, pk: &str) -> (String, u64) {
    let response = router
        .clone()
        .oneshot(create_order_request(pk, "10.00", TEST_CURRENCY))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "order creation failed"
    );
    let order_id = body_json(response).await["order_id"]
        .as_str()
        .unwrap()
        .to_string();
    let connection_id = state
        .db
        .lock()
        .get_store_connection_by_public_key(pk)
        .unwrap()
        .unwrap()
        .id;
    let metadata = state
        .db
        .lock()
        .get_order_currency_metadata(
            &connection_id,
            &engine_protocol::ids::OrderId::new(order_id.to_string()),
        )
        .unwrap()
        .unwrap();
    (metadata.provider, metadata.piconero_per_unit.get())
}

/// Changing a store's providers, or their order, prices the very next
/// order - nothing between the settings page and order creation caches
/// the store's choice - and each order records the provider that priced it.
#[tokio::test]
async fn a_change_to_the_stores_provider_settings_prices_the_very_next_order() {
    let (mut state, _engine) = test_state_with_real_engine().await;
    let coingecko = spawn_mock_coingecko().await; // $1.00 per XMR
    let coinmarketcap = spawn_mock_coinmarketcap(Some(2.0)).await; // $2.00 per XMR
    state.exchange_rate = std::sync::Arc::new(
        crate::exchange_rate_config::ExchangeRateProviders::coingecko_and_coinmarketcap(
            coingecko,
            coinmarketcap,
        ),
    );
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "pay-provider-change@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;
    let connection_id = state
        .db
        .lock()
        .get_store_connection_by_public_key(&pk)
        .unwrap()
        .unwrap()
        .id;

    // A new store starts on Coingecko alone.
    assert_eq!(
        order_pricing(&state, &router, &pk).await,
        ("coingecko".to_string(), 1_000_000_000_000)
    );

    // CoinMarketCap first, Coingecko second.
    save_provider_settings(
        &router,
        &session_token,
        connection_id.as_str(),
        &[
            ("use_coingecko", "on"),
            ("position_coingecko", "2"),
            ("use_coinmarketcap", "on"),
            ("position_coinmarketcap", "1"),
        ],
    )
    .await;
    assert_eq!(
        order_pricing(&state, &router, &pk).await,
        ("coinmarketcap".to_string(), 500_000_000_000)
    );

    // Swapped back.
    save_provider_settings(
        &router,
        &session_token,
        connection_id.as_str(),
        &[
            ("use_coingecko", "on"),
            ("position_coingecko", "1"),
            ("use_coinmarketcap", "on"),
            ("position_coinmarketcap", "2"),
        ],
    )
    .await;
    assert_eq!(
        order_pricing(&state, &router, &pk).await,
        ("coingecko".to_string(), 1_000_000_000_000)
    );

    // Coingecko switched off entirely.
    save_provider_settings(
        &router,
        &session_token,
        connection_id.as_str(),
        &[("use_coinmarketcap", "on"), ("position_coinmarketcap", "1")],
    )
    .await;
    assert_eq!(
        order_pricing(&state, &router, &pk).await,
        ("coinmarketcap".to_string(), 500_000_000_000)
    );
}

/// The first preferred provider being unavailable, or having no rate for
/// the currency, hands over to the next; and the order records the
/// provider that actually answered.
#[tokio::test]
async fn an_unavailable_or_rateless_provider_falls_through_to_the_next_preferred() {
    let (mut state, _engine) = test_state_with_real_engine().await;
    // Coingecko answers, but has no rate for USD; CoinMarketCap answers.
    async fn no_usd() -> axum::response::Response {
        use axum::response::IntoResponse;
        (
            [("content-type", "application/json")],
            r#"{"monero":{"eur":1.0}}"#,
        )
            .into_response()
    }
    let app = Router::new().route("/api/v3/simple/price", axum::routing::get(no_usd));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let coingecko = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let coinmarketcap = spawn_mock_coinmarketcap(Some(2.0)).await;
    state.exchange_rate = std::sync::Arc::new(
        crate::exchange_rate_config::ExchangeRateProviders::coingecko_and_coinmarketcap(
            coingecko,
            coinmarketcap,
        ),
    );
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "pay-provider-fallback@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;
    let connection_id = state
        .db
        .lock()
        .get_store_connection_by_public_key(&pk)
        .unwrap()
        .unwrap()
        .id;
    save_provider_settings(
        &router,
        &session_token,
        connection_id.as_str(),
        &[
            ("use_coingecko", "on"),
            ("position_coingecko", "1"),
            ("use_coinmarketcap", "on"),
            ("position_coinmarketcap", "2"),
        ],
    )
    .await;

    assert_eq!(
        order_pricing(&state, &router, &pk).await,
        ("coinmarketcap".to_string(), 500_000_000_000)
    );

    // Now the preferred provider is down altogether (it hangs up on
    // every request).
    state.exchange_rate = std::sync::Arc::new(
        crate::exchange_rate_config::ExchangeRateProviders::coingecko_and_coinmarketcap(
            format!("http://{}", service_runtime::unreachable::address()),
            spawn_mock_coinmarketcap(Some(4.0)).await,
        ),
    );
    let router = build_router(state.clone());
    assert_eq!(
        order_pricing(&state, &router, &pk).await,
        ("coinmarketcap".to_string(), 250_000_000_000)
    );
}

/// A local server standing in for haveno.markets: a two-sided USD book
/// whose midpoint is `usd_price`, or (`None`) a one-sided one.
async fn spawn_mock_haveno(usd_price: Option<f64>) -> String {
    let body = match usd_price {
        Some(p) => format!(
            r#"{{"USD":{{"pair":"XMR_USD","highest_bid":{},"lowest_ask":{}}}}}"#,
            p - 0.05,
            p + 0.05
        ),
        None => r#"{"USD":{"pair":"XMR_USD","highest_bid":1.0,"lowest_ask":null}}"#.to_string(),
    };
    let app = Router::new().route(
        "/api/v1/tickers",
        axum::routing::get(move || {
            let body = body.clone();
            async move {
                use axum::response::IntoResponse;
                ([("content-type", "application/json")], body).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// Haveno slots into the same per-store order: preferred first it prices
/// the next order and is recorded on it; when its book is one-sided the
/// next provider takes over.
#[tokio::test]
async fn haveno_can_be_ordered_first_and_falls_through_when_its_book_is_one_sided() {
    let (mut state, _engine) = test_state_with_real_engine().await;
    let coingecko = spawn_mock_coingecko().await; // $1.00 per XMR
    let coinmarketcap = spawn_mock_coinmarketcap(Some(2.0)).await;
    let haveno = spawn_mock_haveno(Some(4.0)).await; // midpoint $4.00 per XMR
    state.exchange_rate = std::sync::Arc::new(
        crate::exchange_rate_config::ExchangeRateProviders::all(coingecko, coinmarketcap, haveno),
    );
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "pay-provider-haveno@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;
    let connection_id = state
        .db
        .lock()
        .get_store_connection_by_public_key(&pk)
        .unwrap()
        .unwrap()
        .id;

    save_provider_settings(
        &router,
        &session_token,
        connection_id.as_str(),
        &[
            ("use_haveno", "on"),
            ("position_haveno", "1"),
            ("use_coinmarketcap", "on"),
            ("position_coinmarketcap", "2"),
            ("use_coingecko", "on"),
            ("position_coingecko", "3"),
        ],
    )
    .await;
    assert_eq!(
        order_pricing(&state, &router, &pk).await,
        ("haveno".to_string(), 250_000_000_000)
    );

    // Same settings, but haveno's book has lost its ask: the next order
    // falls to coinmarketcap without touching the settings.
    state.exchange_rate =
        std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::all(
            spawn_mock_coingecko().await,
            spawn_mock_coinmarketcap(Some(2.0)).await,
            spawn_mock_haveno(None).await,
        ));
    let router = build_router(state.clone());
    assert_eq!(
        order_pricing(&state, &router, &pk).await,
        ("coinmarketcap".to_string(), 500_000_000_000)
    );
}

/// Haveno's per-store limits apply to the very next order: tightening
/// the spread limit, or taking the currency off the store's list, sends
/// the next order to the next provider; loosening restores it.
#[tokio::test]
async fn a_change_to_the_stores_haveno_limits_prices_the_very_next_order() {
    let (mut state, _engine) = test_state_with_real_engine().await;
    let haveno = spawn_mock_haveno(Some(4.0)).await; // bid 3.95 / ask 4.05: a 2.5% spread
    state.exchange_rate =
        std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::all(
            spawn_mock_coingecko().await, // $1.00 per XMR
            "http://127.0.0.1:1",
            haveno,
        ));
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "pay-haveno-limits@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;
    let connection_id = state
        .db
        .lock()
        .get_store_connection_by_public_key(&pk)
        .unwrap()
        .unwrap()
        .id;
    let save = |spread: &'static str, currencies: &'static str| {
        let router = router.clone();
        let session_token = session_token.clone();
        let connection_id = connection_id.clone();
        async move {
            save_provider_settings(
                &router,
                &session_token,
                connection_id.as_str(),
                &[
                    ("use_haveno", "on"),
                    ("position_haveno", "1"),
                    ("use_coingecko", "on"),
                    ("position_coingecko", "2"),
                    ("haveno_currencies", currencies),
                    ("haveno_max_spread_pct", spread),
                    ("haveno_min_offers_per_side", "1"),
                    ("haveno_min_depth_xmr_per_side", "0"),
                ],
            )
            .await
        }
    };

    save("5", "").await;
    assert_eq!(
        order_pricing(&state, &router, &pk).await,
        ("haveno".to_string(), 250_000_000_000)
    );

    save("1", "").await; // 2.5% is now too wide
    assert_eq!(
        order_pricing(&state, &router, &pk).await,
        ("coingecko".to_string(), 1_000_000_000_000)
    );

    save("5", "").await;
    assert_eq!(
        order_pricing(&state, &router, &pk).await,
        ("haveno".to_string(), 250_000_000_000)
    );

    save("5", "EUR").await; // USD is no longer on the store's list
    assert_eq!(
        order_pricing(&state, &router, &pk).await,
        ("coingecko".to_string(), 1_000_000_000_000)
    );

    save("5", "EUR,USD").await;
    assert_eq!(
        order_pricing(&state, &router, &pk).await,
        ("haveno".to_string(), 250_000_000_000)
    );
}

/// A local haveno.markets whose tickers request is counted.
async fn spawn_counting_haveno(
    usd_price: f64,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = calls.clone();
    let body = format!(
        r#"{{"USD":{{"pair":"XMR_USD","highest_bid":{},"lowest_ask":{}}}}}"#,
        usd_price - 0.05,
        usd_price + 0.05
    );
    let app = Router::new().route(
        "/api/v1/tickers",
        axum::routing::get(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = body.clone();
            async move {
                use axum::response::IntoResponse;
                ([("content-type", "application/json")], body).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), calls)
}

/// Two merchants on one instance, sharing its one provider cache, with
/// different Haveno limits: each store's orders are priced by its own
/// limits, however their orders interleave, from a single upstream
/// fetch; and neither merchant can read or change the other's settings.
#[tokio::test]
async fn two_merchants_with_different_haveno_limits_are_priced_by_their_own_limits_from_one_shared_cache(
) {
    let (mut state, _engine) = test_state_with_real_engine().await;
    let (haveno, tickers_calls) = spawn_counting_haveno(4.0).await; // a 2.5% spread
    state.exchange_rate =
        std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::all(
            spawn_mock_coingecko().await, // $1.00 per XMR
            "http://127.0.0.1:1",
            haveno,
        ));
    let router = build_router(state.clone());

    let token_a = signed_up_and_logged_in_session_token(
        &router,
        "pay-user-a@example.com",
        "correct horse battery staple",
    )
    .await;
    let token_b = signed_up_and_logged_in_session_token(
        &router,
        "pay-user-b@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk_a = create_connection(&router, &state, &token_a).await;
    let pk_b = create_connection(&router, &state, &token_b).await;
    let id_a = state
        .db
        .lock()
        .get_store_connection_by_public_key(&pk_a)
        .unwrap()
        .unwrap()
        .id;
    let id_b = state
        .db
        .lock()
        .get_store_connection_by_public_key(&pk_b)
        .unwrap()
        .unwrap()
        .id;

    let haveno_first = |spread: &'static str| {
        vec![
            ("use_haveno", "on"),
            ("position_haveno", "1"),
            ("use_coingecko", "on"),
            ("position_coingecko", "2"),
            ("haveno_currencies", ""),
            ("haveno_max_spread_pct", spread),
            ("haveno_min_offers_per_side", "1"),
            ("haveno_min_depth_xmr_per_side", "0"),
        ]
    };
    save_provider_settings(&router, &token_a, id_a.as_str(), &haveno_first("5")).await; // accepts 2.5%
    save_provider_settings(&router, &token_b, id_b.as_str(), &haveno_first("1")).await; // does not

    let haveno_price = ("haveno".to_string(), 250_000_000_000);
    let coingecko_price = ("coingecko".to_string(), 1_000_000_000_000);
    for round in 0..3 {
        assert_eq!(
            order_pricing(&state, &router, &pk_a).await,
            haveno_price,
            "A, round {round}"
        );
        assert_eq!(
            order_pricing(&state, &router, &pk_b).await,
            coingecko_price,
            "B, round {round}"
        );
        assert_eq!(
            order_pricing(&state, &router, &pk_b).await,
            coingecko_price,
            "B again, round {round}"
        );
        assert_eq!(
            order_pricing(&state, &router, &pk_a).await,
            haveno_price,
            "A again, round {round}"
        );
    }
    assert_eq!(
        tickers_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "both merchants were served from one upstream fetch"
    );

    // B loosening its own limit changes B's next order and nothing of A's.
    save_provider_settings(&router, &token_b, id_b.as_str(), &haveno_first("3")).await;
    assert_eq!(order_pricing(&state, &router, &pk_b).await, haveno_price);
    assert_eq!(order_pricing(&state, &router, &pk_a).await, haveno_price);
    assert_eq!(
        state
            .db
            .lock()
            .get_store_connection_by_id(&id_a)
            .unwrap()
            .unwrap()
            .fx_provider_settings
            .haveno
            .max_spread_pct,
        5.0
    );

    // A merchant cannot change, or see, another merchant's store settings.
    let before_a = state
        .db
        .lock()
        .get_store_connection_by_id(&id_a)
        .unwrap()
        .unwrap();
    let body = "use_coingecko=on&position_coingecko=1&haveno_currencies=EUR&haveno_max_spread_pct=0.5&haveno_min_offers_per_side=9&haveno_min_depth_xmr_per_side=9";
    let forged = |token: &str, uri: String, method: &str, body: &str| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("cookie", format!("session={token}"))
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let response = router
        .clone()
        .oneshot(forged(
            &token_b,
            format!("/stores/{id_a}/settings"),
            "POST",
            body,
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "B may not change A's store"
    );
    let response = router
        .clone()
        .oneshot(forged(
            &token_b,
            format!("/stores/{id_a}/settings"),
            "GET",
            "",
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "B may not view A's store settings"
    );
    let after_a = state
        .db
        .lock()
        .get_store_connection_by_id(&id_a)
        .unwrap()
        .unwrap();
    assert_eq!(
        (after_a.fx_providers, after_a.fx_provider_settings),
        (before_a.fx_providers, before_a.fx_provider_settings)
    );
    assert_eq!(
        order_pricing(&state, &router, &pk_a).await,
        haveno_price,
        "A's pricing is unchanged by B's attempt"
    );
}

#[tokio::test]
async fn creating_an_order_with_an_unknown_currency_is_rejected_before_ever_reaching_the_engine() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());

    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "pay-endpoint-bad-currency@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection(&router, &state, &session_token).await;

    let response = router
        .oneshot(create_order_request(&pk, "25.00", "NOTREAL"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .contains("unknown currency"),
        "got: {body}"
    );

    // The store's own base currency ("EUR", a perfectly real known currency -
    // `crate::currencies`'s selection-time check happily accepted it when this
    // store was created) turns out to have no available rate provider at
    // resolution time (this test module's mock Coingecko only ever prices
    // `"usd"`) - resolving the confirmation threshold needs a real EUR rate to
    // convert the order's own amount into base-currency terms, and there isn't
    // one. The order-currency itself ("USD") is perfectly priceable; it's
    // specifically the base currency conversion that fails - proving the
    // "usable" check really is a separate, later concern from "known"
    // (`crate::currencies`'s own doc comment, and
    // `confirmation_thresholds::resolve_for_order`'s).
    {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint-unpriceable-base-currency@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection_with_base_currency(&router, &state, &session_token, "EUR").await;

        let response = router
            .clone()
            .oneshot(create_order_request(&pk, "25.00", "USD"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert!(
            body["detail"].as_str().unwrap().contains("EUR"),
            "expected a clear error naming the unpriceable base currency, got: {body}"
        );

        // The engine must never have created a real order at all - threshold
        // resolution runs *before* `EngineClient::create_order`, so a
        // failure here must leave no ghost order behind on the engine.
        let store = engine.store().lock();
        let tenant_id = store
            .list_active_tenants()
            .unwrap()
            .into_iter()
            .find(|t| t.public_key == pk)
            .unwrap()
            .id;
        let orders = store.list_orders(&tenant_id, None, 100, None).unwrap();
        assert!(
            orders.is_empty(),
            "expected no order to have been created on the real engine, got: {orders:?}"
        );
    }

    // The other half of the same two-stage split (`crate::currencies`'s own doc
    // comment): `EUR` is a perfectly real, known currency - this test's own
    // `spawn_mock_coingecko` only ever prices `"usd"` (a fixed stub response),
    // so a request for `EUR` genuinely has no rate available. That must surface
    // as a distinct "unsupported", not "unknown", currency error.
    {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint-unsupported-currency@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &state, &session_token).await;

        let response = router
            .oneshot(create_order_request(&pk, "25.00", "EUR"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert!(
            body["detail"]
                .as_str()
                .unwrap()
                .contains("unsupported currency"),
            "got: {body}"
        );
    }

    // Creating an order with a malformed amount is a clear 400 not a 500.
    {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint-bad-amount@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &state, &session_token).await;

        let response = router
            .oneshot(create_order_request(&pk, "not-a-number", TEST_CURRENCY))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn the_embed_library_never_calls_the_engine_and_never_talks_to_its_checkout() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());

    let req = Request::builder()
        .method("GET")
        .uri("/static/monokulo-client.js")
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/javascript; charset=utf-8"
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let js = String::from_utf8(bytes.to_vec()).unwrap();
    // The embed library talks to monokulo only: never the engine, which
    // is private.
    assert!(
        !js.contains("/api/v1/t/"),
        "must not reference the engine's own API directly: {js}"
    );
    assert!(
        !js.contains("/pay/v1/"),
        "must not reference an engine checkout route: {js}"
    );
    // The iframe posts no messages back: mount() follows the order itself.
    assert!(
        !js.contains("postMessage"),
        "the embed library must not depend on the iframe posting a message any more, got: {js}"
    );
    for asset in [
        crate::assets::url("checkout.js"),
        crate::assets::url("jsQR.js"),
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(&asset).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{asset}");
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/javascript; charset=utf-8"
        );
    }
    // The checkout page and whatever embeds it (the POS terminal, this
    // library, a merchant's own page) stay independent: each follows the
    // order itself, and neither talks to the other.
    let checkout_js = include_str!("../../../../static/checkout.js");
    assert!(
        !checkout_js.contains("postMessage")
            && !checkout_js.contains("window.parent")
            && !checkout_js.contains("window.top"),
        "checkout.js must not know about its embedder"
    );
}

/// A local Coingecko that rate limits every request, asking for `secs`.
async fn spawn_rate_limited_coingecko(secs: u64) -> String {
    let app = Router::new().fallback(move || async move {
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

/// With its only rate provider rate limiting this Monokulo (CoinGecko's
/// `429`) and no rate cached, every route that prices an order answers
/// that there's no rate right now, with the provider's wait: the API and
/// the point of sale the `rates-unavailable` problem (`503`, `retry_after`,
/// `Retry-After`), the dashboard's test order its form again with `503`.
/// The same for a store whose base currency it is, ordering in XMR (its
/// confirmations are decided in the base currency). An XMR order of a
/// store priced in XMR needs no rate and is made; with the engine then
/// unreachable, it is `engine-unavailable` (503) everywhere, the checkout
/// page too.
#[tokio::test]
async fn a_rate_limited_provider_or_an_unreachable_engine_is_a_503_on_every_route_that_makes_an_order(
) {
    let (mut state, _engine) = test_state_with_real_engine().await;
    state.exchange_rate = std::sync::Arc::new(
        crate::exchange_rate_config::ExchangeRateProviders::coingecko_only(
            spawn_rate_limited_coingecko(42).await,
        ),
    );
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "rates-unavailable@example.com",
        "correct horse battery staple",
    )
    .await;
    let pk = create_connection_with_base_currency(&router, &state, &session_token, "USD").await;
    let store_id = state
        .db
        .lock()
        .get_store_connection_by_public_key(&pk)
        .unwrap()
        .unwrap()
        .id;

    let check_problem = |response: axum::response::Response, route: &'static str| async move {
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{route}"
        );
        assert_eq!(response.headers()["retry-after"], "42", "{route}");
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json",
            "{route}"
        );
        let problem = body_json(response).await;
        assert_eq!(
            problem["type"], "https://docs.monokulo.com/problems/rates-unavailable",
            "{route}"
        );
        assert_eq!(problem["status"], 503, "{route}");
        assert_eq!(problem["retry_after"], 42, "{route}");
        assert!(
            problem["detail"].as_str().unwrap().contains("USD"),
            "{route}: {problem}"
        );
    };

    // The API, in the rate-limited currency and (the store's base
    // currency deciding its confirmations) in XMR.
    for currency in ["USD", "XMR"] {
        let response = router
            .clone()
            .oneshot(create_order_request(&pk, "10.00", currency))
            .await
            .unwrap();
        check_problem(response, "POST /api/v1/orders").await;
    }

    // The point of sale, in the store's base currency.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/stores/{store_id}/pos/orders"))
                .header("content-type", "application/json")
                .header("cookie", format!("session={session_token}"))
                .body(Body::from(
                    serde_json::json!({ "amount": "1.50" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    check_problem(response, "POST /stores/{id}/pos/orders").await;

    // The dashboard's test order: its form again, saying when to retry.
    for currency in ["USD", "XMR"] {
        let response = router
            .clone()
            .oneshot(crate::http::test_support::form_post(
                &format!("/stores/{store_id}/orders/new"),
                Some(&format!("session={session_token}")),
                &[("amount", "10.00"), ("currency", currency)],
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{currency}"
        );
        assert_eq!(response.headers()["retry-after"], "42");
        let html = body_text(response).await;
        assert!(
            html.contains("no exchange rate for USD right now") && html.contains("in 42 s"),
            "{currency}: {html}"
        );
    }

    // A store priced in XMR, ordering in XMR: no rate needed.
    let xmr_pk = create_connection(&router, &state, &session_token).await;
    let response = router
        .clone()
        .oneshot(create_order_request(&xmr_pk, "0.5", "XMR"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let order_id = body_json(response).await["order_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // And with the engine unreachable: every route that makes or reads an
    // order says the engine is unavailable (503), not that something broke
    // here (500).
    let mut down = state.clone();
    down.engine = crate::http::Engine::new(EngineClient::for_tests(format!(
        "http://{}",
        service_runtime::unreachable::address()
    )));
    let router = build_router(down);
    let response = router
        .clone()
        .oneshot(create_order_request(&xmr_pk, "0.5", "XMR"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body_json(response).await["type"],
        "https://docs.monokulo.com/problems/engine-unavailable"
    );
    let xmr_store = state
        .db
        .lock()
        .get_store_connection_by_public_key(&xmr_pk)
        .unwrap()
        .unwrap()
        .id;
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/stores/{xmr_store}/pos/orders"))
                .header("content-type", "application/json")
                .header("cookie", format!("session={session_token}"))
                .body(Body::from(
                    serde_json::json!({ "amount": "1.50" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let response = router
        .clone()
        .oneshot(crate::http::test_support::form_post(
            &format!("/stores/{xmr_store}/orders/new"),
            Some(&format!("session={session_token}")),
            &[("amount", "1.50"), ("currency", "XMR")],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(body_text(response).await.contains("busy or unreachable"));
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/checkout/{order_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
