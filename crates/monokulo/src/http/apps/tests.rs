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
use crate::http::test_support::{
    admin_session_cookie, api_request, body_json, body_text, form_post, store_with_keys,
};
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

/// A store of the seeded admin's: its id, and the admin's cookie.
async fn store(router: &Router) -> (String, String) {
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
    (crate::http::test_support::store_made(&made), cookie)
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
            &format!("/stores/{id}/settings/apps/connect"),
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
        .split(&format!("/stores/{id}/settings/apps/connect/"))
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

/// An order made with `key`, as a till makes one.
fn order(key: &str, idempotency_key: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/orders")
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

/// `GET uri` on the API with `key`: its status and body.
async fn read(router: &Router, uri: &str, key: &str) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(api_request("GET", uri, Some(key), None))
        .await
        .unwrap();
    (response.status(), body_json(response).await)
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
/// key reads its store and its own connection, makes orders that are the
/// app's and announced on the store's stream; the dashboard lists the
/// device; disconnecting it stops the key.
#[tokio::test]
async fn an_app_connects_with_a_code_makes_orders_and_hears_of_them() {
    let (state, _engine) = state_with_engine(&format!("public_uri = \"{PUBLIC_URI}\"\n")).await;
    let router = build_router(state.clone());
    let (id, cookie) = store(&router).await;

    let (token, status) = code(&router, &id, &cookie, "Front counter").await;
    assert!(token.starts_with("ct_"), "{token}");
    let unused = body_text(
        get(
            &router,
            &format!("/stores/{id}/settings/apps/connect/{status}"),
            &cookie,
        )
        .await,
    )
    .await;
    assert!(unused.contains("No app has used the code yet"), "{unused}");

    let response = redeem(&router, connect_body(&token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let creds = body_json(response).await;
    // Only the key and where the API is: the rest the app reads with it.
    // No onion address here, so no `onion_api`.
    assert_eq!(creds["api"], "/api/v1");
    assert_eq!(creds.as_object().unwrap().len(), 2, "{creds}");
    let key = creds["app_key"].as_str().unwrap().to_owned();
    assert!(key.starts_with("ak_"));

    let (status_code, app) = read(&router, "/api/v1/app", &key).await;
    assert_eq!(status_code, StatusCode::OK);
    assert_eq!(
        app["device_name"], "Front counter",
        "the merchant's name wins"
    );
    assert_eq!(app["platform"], "android");
    let (status_code, store) = read(&router, "/api/v1/store", &key).await;
    assert_eq!(status_code, StatusCode::OK);
    assert_eq!(store["name"], "Bakery");
    assert_eq!(store["wallet"]["network"], "mainnet");

    // Once only.
    crate::http::test_support::assert_problem(
        redeem(&router, connect_body(&token)).await,
        "invalid-connect-code",
    )
    .await;
    let used = body_text(
        get(
            &router,
            &format!("/stores/{id}/settings/apps/connect/{status}"),
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
        .oneshot(api_request("GET", "/api/v1/events", None, None))
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let stream = router
        .clone()
        .oneshot(api_request("GET", "/api/v1/events", Some(&key), None))
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    let mut body = stream.into_body();
    let mut buffer = String::new();
    assert_eq!(next_event(&mut body, &mut buffer).await.0, "ready");

    let made = router.clone().oneshot(order(&key, "sq:abc")).await.unwrap();
    assert_eq!(made.status(), StatusCode::CREATED);
    let made = body_json(made).await;
    let order_id = made["order_id"].as_str().unwrap().to_owned();
    // A second till finding the same sale gets the same order.
    let again = body_json(router.clone().oneshot(order(&key, "sq:abc")).await.unwrap()).await;
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
            &format!("/stores/{id}/settings?tab=connections"),
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
    let app_id = app["app_id"].as_str().unwrap();
    let gone = router
        .clone()
        .oneshot(form_post(
            &format!("/stores/{id}/settings/apps/{app_id}/disconnect"),
            Some(&cookie),
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::SEE_OTHER);
    let refused = router.clone().oneshot(order(&key, "sq:def")).await.unwrap();
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
/// for an app, and an app's key is no good for another store's orders.
#[tokio::test]
async fn an_app_disconnects_itself_and_its_key_is_only_its_stores() {
    let (state, _engine) = state_with_engine(&format!("public_uri = \"{PUBLIC_URI}\"\n")).await;
    let router = build_router(state.clone());
    let (id, cookie) = store(&router).await;
    let (token, _) = code(&router, &id, &cookie, "").await;
    let creds = body_json(redeem(&router, connect_body(&token)).await).await;
    let key = creds["app_key"].as_str().unwrap().to_owned();
    let (_, app) = read(&router, "/api/v1/app", &key).await;
    assert_eq!(
        app["device_name"], "Pixel 8",
        "the app names the device when the merchant didn't"
    );

    // Another store's order, made with that store's secret key: not this
    // app's to read.
    let session = cookie.strip_prefix("session=").unwrap();
    let other = store_with_keys(&router, &state, session, "", "XMR").await;
    let theirs = router
        .clone()
        .oneshot(order(&other.secret_key, "sq:1"))
        .await
        .unwrap();
    assert_eq!(theirs.status(), StatusCode::CREATED);
    let theirs = body_json(theirs).await["order_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let uri = format!("/api/v1/orders/{theirs}");
    assert_eq!(
        read(&router, &uri, &other.secret_key).await.0,
        StatusCode::OK
    );
    assert_eq!(read(&router, &uri, &key).await.0, StatusCode::NOT_FOUND);

    // The other store can't disconnect this store's app: nothing is
    // deleted, and the app's key still works.
    let app_id = app["app_id"].as_str().unwrap();
    let elsewhere = router
        .clone()
        .oneshot(form_post(
            &format!("/stores/{}/settings/apps/{app_id}/disconnect", other.id),
            Some(&cookie),
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(elsewhere.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        state
            .db
            .lock()
            .list_apps(&shared::ids::ConnectionId::new(id.clone()))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(read(&router, "/api/v1/app", &key).await.0, StatusCode::OK);

    let delete = |key: String| {
        let router = router.clone();
        async move {
            router
                .oneshot(api_request("DELETE", "/api/v1/app", Some(&key), None))
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(
        delete(other.secret_key.clone()).await,
        StatusCode::FORBIDDEN,
        "a store's key is not an app"
    );
    assert_eq!(delete(key.clone()).await, StatusCode::NO_CONTENT);
    assert_eq!(delete(key.clone()).await, StatusCode::UNAUTHORIZED);
    assert!(state
        .db
        .lock()
        .list_apps(&shared::ids::ConnectionId::new(id))
        .unwrap()
        .is_empty());
}

/// An app running in a browser on another site disconnects itself, the way
/// a browser sends it: a preflight asking for `DELETE` with an
/// `Authorization` header, which must allow `DELETE` or the browser stops
/// there, then the `DELETE /api/v1/app` itself with the app's key, carried
/// out and answered with `access-control-allow-origin` so the page can read
/// that it worked.
#[tokio::test]
async fn an_app_on_another_site_disconnects_itself_cross_origin() {
    let (state, _engine) = state_with_engine(&format!("public_uri = \"{PUBLIC_URI}\"\n")).await;
    let router = build_router(state.clone());
    let (id, cookie) = store(&router).await;
    let (token, _) = code(&router, &id, &cookie, "").await;
    let creds = body_json(redeem(&router, connect_body(&token)).await).await;
    let key = creds["app_key"].as_str().unwrap().to_owned();

    let origin = "https://docs.monokulo.com";
    let preflight = router
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/app")
                .header("origin", origin)
                .header("access-control-request-method", "DELETE")
                .header("access-control-request-headers", "authorization")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        preflight.status().is_success(),
        "got {}",
        preflight.status()
    );
    assert_eq!(preflight.headers()["access-control-allow-origin"], origin);
    let methods = preflight.headers()["access-control-allow-methods"]
        .to_str()
        .unwrap();
    assert!(methods.contains("DELETE"), "DELETE in {methods}");

    let mut request = api_request("DELETE", "/api/v1/app", Some(&key), None);
    request
        .headers_mut()
        .insert("origin", origin.parse().unwrap());
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()["access-control-allow-origin"], origin);
    assert!(!response
        .headers()
        .contains_key("access-control-allow-credentials"));
    assert!(state
        .db
        .lock()
        .list_apps(&shared::ids::ConnectionId::new(id))
        .unwrap()
        .is_empty());
}

/// Without a public address the code can't be made, and redeeming says so
/// with a `no-public-address` problem before spending a code; a bad slug
/// or body is `invalid-request`, an unknown code `invalid-connect-code`:
/// every error a documented problem.
#[tokio::test]
async fn redeeming_refuses_clearly_and_spends_nothing_it_cant_use() {
    use crate::http::test_support::assert_problem;
    let (state, _engine) = state_with_engine("").await;
    let router = build_router(state.clone());
    let (id, cookie) = store(&router).await;
    let page = router
        .clone()
        .oneshot(form_post(
            &format!("/stores/{id}/settings/apps/connect"),
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
    assert_problem(
        redeem(&router, connect_body("ct_test")).await,
        "no-public-address",
    )
    .await;
    let state = state
        .with_options(&format!("public_uri = \"{PUBLIC_URI}\"\n"))
        .await;
    let router = build_router(state.clone());
    let mut bad = connect_body("ct_test");
    bad["slug"] = "Monokular!".into();
    assert_problem(redeem(&router, bad).await, "invalid-request").await;
    assert_problem(
        redeem(&router, serde_json::json!({ "slug": "monokular" })).await,
        "invalid-request",
    )
    .await;
    let not_json = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/apps/connect")
                .body(Body::from("token=ct_test"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(not_json, "unsupported-media-type").await;
    assert_problem(
        redeem(&router, connect_body("ct_nope")).await,
        "invalid-connect-code",
    )
    .await;
    assert_eq!(
        redeem(&router, connect_body("ct_test")).await.status(),
        StatusCode::OK
    );
}

/// With an onion address, an app is given the API's entry on it too
/// (`onion_api`), absolute, so an app that can use Tor reaches Monokulo
/// over it from the start.
#[tokio::test]
async fn an_app_is_given_the_onion_entry_while_there_is_one() {
    const ONION: &str = "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
    let (state, _engine) = state_with_engine(&format!(
        "public_uri = \"{PUBLIC_URI}\"\nonion_uri = \"{ONION}\"\n[tor]\nmode = \"external\"\n[abuse]\nonion_listener = \"127.0.0.1:8082\"\n"
    ))
    .await;
    let router = build_router(state);
    let (id, cookie) = store(&router).await;
    let (token, _) = code(&router, &id, &cookie, "Till").await;
    let creds = body_json(redeem(&router, connect_body(&token)).await).await;
    assert_eq!(creds["api"], "/api/v1");
    assert_eq!(creds["onion_api"], format!("{ONION}/api/v1"));
}
