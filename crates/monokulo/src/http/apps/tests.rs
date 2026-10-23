//! Connected apps end to end, in-process, against a real engine: the
//! dashboard's code, an app redeeming it, making orders with its key,
//! hearing of them on the store's event stream, and being disconnected.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::engine_client::EngineClient;
use crate::http::test_support::{admin_session_cookie, body_json, body_text, form_post};
use crate::http::{build_router, AppState};

const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
const TEST_SPEND_PUBKEY_HEX: &str =
    "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
const PUBLIC_URI: &str = "https://pay.example.test";

async fn state_with_engine(options: &str) -> (AppState, engine_test_support::TestEngineHandle) {
    let engine =
        engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
    let state = AppState {
        engine: crate::http::Engine::new(EngineClient::embedded_for_tests(engine.router())),
        ..AppState::for_tests()
    }
    .with_options(options)
    .await;
    (state, engine)
}

/// A store of the seeded admin's: its id and public key, and the admin's
/// cookie.
async fn store(router: &Router, state: &AppState) -> (String, String, String) {
    let cookie = admin_session_cookie(router).await;
    let made = crate::http::test_support::set_up_store_with_keys(
        router,
        &cookie,
        &[
            ("store_name", "Bakery"),
            ("store_site", ""),
            ("view_key_hex", TEST_VIEW_KEY_HEX),
            ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
        ],
    )
    .await;
    let id = crate::http::test_support::store_made(&made);
    let pk = state
        .db
        .lock()
        .get_store_connection_by_id(&shared::ids::ConnectionId::new(id.clone()))
        .unwrap()
        .unwrap()
        .tenant_public_key;
    (id, pk, cookie)
}

async fn get(router: &Router, uri: &str, cookie: &str) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Shows a code on the dashboard and returns its token, from the link.
async fn code(router: &Router, id: &str, cookie: &str, device_name: &str) -> (String, String) {
    let response = router
        .clone()
        .oneshot(form_post(
            &format!("/dashboard/stores/{id}/apps/connect"),
            Some(cookie),
            &[("device_name", device_name)],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    let start = html
        .find("/apps/connect#token=")
        .expect("the page shows the link")
        + 20;
    let token: String = html[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let status = html
        .split(&format!("/dashboard/stores/{id}/apps/connect/"))
        .nth(1)
        .map(|rest| {
            rest.chars()
                .take_while(char::is_ascii_hexdigit)
                .collect::<String>()
        })
        .expect("the page links to its status");
    (token, status)
}

async fn redeem(router: &Router, body: serde_json::Value) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/apps/connect")
                .header("content-type", "application/json")
                .header("monokulo-client", "monokular/0.1.0")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

fn connect_body(token: &str) -> serde_json::Value {
    serde_json::json!({
        "token": token,
        "slug": "monokular",
        "name": "Monokular",
        "device_name": "Pixel 8",
        "platform": "android",
    })
}

fn order(pk: &str, key: &str, idempotency_key: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/pay/{pk}/orders"))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"))
        .header("monokulo-client", "monokular/0.1.1")
        .body(Body::from(
            serde_json::json!({
                "amount": "0.5",
                "currency": "XMR",
                "merchant_order_id": "#1042",
                "idempotency_key": idempotency_key,
            })
            .to_string(),
        ))
        .unwrap()
}

/// The next server-sent event on `body`: its name and data.
async fn next_event(body: &mut Body, buffer: &mut String) -> (String, serde_json::Value) {
    loop {
        if let Some(end) = buffer.find("\n\n") {
            let block: String = buffer.drain(..end + 2).collect();
            let mut name = String::new();
            let mut data = String::new();
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    name = v.trim().to_owned();
                } else if let Some(v) = line.strip_prefix("data:") {
                    data.push_str(v.trim());
                }
            }
            if name.is_empty() {
                continue; // a keep-alive comment
            }
            return (name, serde_json::from_str(&data).unwrap());
        }
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .expect("an event within 5 s")
            .expect("the stream stays open")
            .unwrap();
        if let Ok(data) = frame.into_data() {
            buffer.push_str(std::str::from_utf8(&data).unwrap());
        }
    }
}

/// The whole flow: a code from the dashboard, redeemed once for a key; the
/// key makes orders that are the app's and announced on the store's stream;
/// the dashboard lists the device; disconnecting it stops the key.
#[tokio::test]
async fn an_app_connects_with_a_code_makes_orders_and_hears_of_them() {
    let (state, _engine) = state_with_engine(&format!("public_uri = \"{PUBLIC_URI}\"\n")).await;
    let router = build_router(state.clone());
    let (id, pk, cookie) = store(&router, &state).await;

    let (token, status) = code(&router, &id, &cookie, "Front counter").await;
    assert!(token.starts_with("ct_"), "{token}");
    let unused = body_text(
        get(
            &router,
            &format!("/dashboard/stores/{id}/apps/connect/{status}"),
            &cookie,
        )
        .await,
    )
    .await;
    assert!(unused.contains("No app has used the code yet"), "{unused}");

    let response = redeem(&router, connect_body(&token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let creds = body_json(response).await;
    assert_eq!(creds["public_key"], pk.as_str());
    assert_eq!(creds["endpoint"], PUBLIC_URI);
    assert_eq!(
        creds["device_name"], "Front counter",
        "the merchant's name wins"
    );
    assert_eq!(creds["store"]["name"], "Bakery");
    assert_eq!(creds["wallet"]["network"], "mainnet");
    let primary_address = {
        let db = state.db.lock();
        let store = db
            .get_store_connection_by_id(&shared::ids::ConnectionId::new(id.clone()))
            .unwrap()
            .unwrap();
        db.get_wallet(&store.user_id, store.wallet_id.as_ref().unwrap())
            .unwrap()
            .unwrap()
            .primary_address
    };
    assert_eq!(
        creds["wallet"]["address"], primary_address,
        "the whole address, for the app to copy"
    );
    let short = creds["wallet"]["address_short"].as_str().unwrap();
    let (head, tail) = short.split_once('…').unwrap();
    assert!(
        primary_address.starts_with(head) && primary_address.ends_with(tail),
        "{short} is {primary_address} shortened"
    );
    let key = creds["app_key"].as_str().unwrap().to_owned();
    assert!(key.starts_with("ak_"));

    // Once only.
    assert_eq!(
        redeem(&router, connect_body(&token)).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let used = body_text(
        get(
            &router,
            &format!("/dashboard/stores/{id}/apps/connect/{status}"),
            &cookie,
        )
        .await,
    )
    .await;
    assert!(
        used.contains("Monokular · Front counter is connected"),
        "{used}"
    );

    // The stream is for keys only.
    let anonymous = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/pay/{pk}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let stream = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/pay/{pk}/events"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    let mut body = stream.into_body();
    let mut buffer = String::new();
    assert_eq!(next_event(&mut body, &mut buffer).await.0, "ready");

    let made = router
        .clone()
        .oneshot(order(&pk, &key, "sq:abc"))
        .await
        .unwrap();
    assert_eq!(made.status(), StatusCode::OK);
    let made = body_json(made).await;
    let order_id = made["order_id"].as_str().unwrap().to_owned();
    // A second till finding the same sale gets the same order.
    let again = body_json(
        router
            .clone()
            .oneshot(order(&pk, &key, "sq:abc"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(again["order_id"], order_id.as_str());

    let (name, event) = next_event(&mut body, &mut buffer).await;
    assert_eq!(name, "sale.waiting");
    assert_eq!(event["order_id"], order_id.as_str());
    assert_eq!(event["address"], made["address"]);
    assert_eq!(event["merchant_order_id"], "#1042");
    assert_eq!(event["device_name"], "Front counter");

    let app_order = state
        .db
        .lock()
        .app_order(&crate::db::OrderId::new(order_id.clone()))
        .unwrap()
        .expect("the order is recorded as the app's");
    assert_eq!(app_order.amount, "0.5");
    let settings = body_text(
        get(
            &router,
            &format!("/dashboard/stores/{id}/settings?tab=connections"),
            &cookie,
        )
        .await,
    )
    .await;
    assert!(settings.contains("Monokular · Front counter"), "{settings}");
    assert!(
        settings.contains("version 0.1.1"),
        "the order's client version is kept: {settings}"
    );

    // Disconnected from the dashboard: the key stops working.
    let app_id = creds["app_id"].as_str().unwrap();
    let gone = router
        .clone()
        .oneshot(form_post(
            &format!("/dashboard/stores/{id}/apps/{app_id}/disconnect"),
            Some(&cookie),
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::SEE_OTHER);
    let refused = router
        .clone()
        .oneshot(order(&pk, &key, "sq:def"))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    // Its open stream ends, so the app learns at once.
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = body.frame().await {
            frame.unwrap();
        }
    })
    .await;
    assert!(ended.is_ok(), "the disconnected app's stream should end");
}

/// An app disconnects itself with its own key; the store's key can't do it
/// for an app, and an app's key is no good for another store.
#[tokio::test]
async fn an_app_disconnects_itself_and_its_key_is_only_its_stores() {
    let (state, _engine) = state_with_engine(&format!("public_uri = \"{PUBLIC_URI}\"\n")).await;
    let router = build_router(state.clone());
    let (id, pk, cookie) = store(&router, &state).await;
    let (token, _) = code(&router, &id, &cookie, "").await;
    let creds = body_json(redeem(&router, connect_body(&token)).await).await;
    assert_eq!(
        creds["device_name"], "Pixel 8",
        "the app names the device when the merchant didn't"
    );
    let key = creds["app_key"].as_str().unwrap().to_owned();

    // Another store's public key with this app's key.
    let elsewhere = router
        .clone()
        .oneshot(order("pk_someone_else", &key, "sq:1"))
        .await
        .unwrap();
    assert_eq!(elsewhere.status(), StatusCode::UNAUTHORIZED);

    let delete = |key: String| {
        let (router, pk) = (router.clone(), pk.clone());
        async move {
            router
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri(format!("/pay/{pk}/app"))
                        .header("authorization", format!("Bearer {key}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(delete(key.clone()).await, StatusCode::NO_CONTENT);
    assert_eq!(delete(key.clone()).await, StatusCode::UNAUTHORIZED);
    assert!(state
        .db
        .lock()
        .list_apps(&shared::ids::ConnectionId::new(id))
        .unwrap()
        .is_empty());
}

/// Without a public address the code can't be made, and redeeming says so
/// with a 503 before spending a code; a bad slug is a 400; an unknown code
/// a bare 401.
#[tokio::test]
async fn redeeming_refuses_clearly_and_spends_nothing_it_cant_use() {
    let (state, _engine) = state_with_engine("").await;
    let router = build_router(state.clone());
    let (id, _, cookie) = store(&router, &state).await;
    let page = router
        .clone()
        .oneshot(form_post(
            &format!("/dashboard/stores/{id}/apps/connect"),
            Some(&cookie),
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body_text(page).await.contains("no public address yet"));

    // A code made while there was one, redeemed after it was removed.
    let hash = shared::auth::RawToken::presented("ct_test").hash();
    let store_id = shared::ids::ConnectionId::new(id);
    state
        .db
        .lock()
        .create_app_connect_token(&hash, &store_id, "", crate::now_unix())
        .unwrap();
    assert_eq!(
        redeem(&router, connect_body("ct_test")).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let state = state
        .with_options(&format!("public_uri = \"{PUBLIC_URI}\"\n"))
        .await;
    let router = build_router(state.clone());
    let mut bad = connect_body("ct_test");
    bad["slug"] = "Monokular!".into();
    assert_eq!(redeem(&router, bad).await.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        redeem(&router, connect_body("ct_nope")).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        redeem(&router, connect_body("ct_test")).await.status(),
        StatusCode::OK
    );
}

/// A connect link opened in a browser explains itself, and names no app.
#[tokio::test]
async fn the_connect_link_opened_in_a_browser_says_to_use_the_app() {
    let router = build_router(AppState::for_tests());
    let response = router
        .oneshot(
            Request::builder()
                .uri("/apps/connect")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("paste it into the app"), "{html}");
    assert!(!html.contains("Monokular"), "{html}");
}
