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
    let settings =
        body_text(get(&router, &format!("/stores/{id}/settings?tab=apps"), &cookie).await).await;
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

/// The connect pages walk the strip under the title: the name, with how
/// the app will read under Apps; the code, whose status asks for itself
/// (fixi) until an app uses it, then moves the strip on and stops; and a
/// code it can't vouch for reads as run out.
#[tokio::test]
async fn the_connect_pages_follow_the_code_until_an_app_uses_it() {
    let (state, _engine) = state_with_engine(&format!("public_uri = \"{PUBLIC_URI}\"\n")).await;
    let router = build_router(state.clone());
    let (id, cookie) = store(&router).await;

    let form = body_text(
        get(
            &router,
            &format!("/stores/{id}/settings/apps/connect"),
            &cookie,
        )
        .await,
    )
    .await;
    assert!(
        form.contains("aria-current=\"step\"><span>Name it"),
        "{form}"
    );
    assert!(form.contains("Under Apps it will read"), "{form}");
    assert!(
        form.contains("data-preview=\"connect-app-preview\""),
        "{form}"
    );
    assert!(form.contains("/static/connect-app."), "{form}");

    let (token, status) = code(&router, &id, &cookie, "Front counter").await;
    let status_uri = format!("/stores/{id}/settings/apps/connect/{status}");
    let fx_get = |uri: String| {
        let router = router.clone();
        let cookie = cookie.clone();
        async move {
            let response = router
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header("cookie", cookie)
                        .header(crate::http::fx::FX_REQUEST, "true")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            body_text(response).await
        }
    };

    // Waiting: only the status, asking again; no page around it.
    let waiting = fx_get(status_uri.clone()).await;
    assert!(waiting.contains("fx-poll=\"2000\""), "{waiting}");
    assert!(waiting.contains("Waiting for the app"), "{waiting}");
    assert!(!waiting.contains("mk-page-header"), "{waiting}");
    // Without JavaScript, the page: the card named for the device, the time left.
    let page = body_text(get(&router, &status_uri, &cookie).await).await;
    assert!(page.contains("aria-current=\"step\"><span>Scan"), "{page}");
    assert!(page.contains("Front counter"), "{page}");
    assert!(page.contains("left to scan"), "{page}");

    let response = redeem(&router, connect_body(&token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let connected = fx_get(status_uri.clone()).await;
    assert!(
        connected.contains("Monokular · Front counter is connected."),
        "{connected}"
    );
    assert!(
        !connected.contains("fx-poll"),
        "it stops asking: {connected}"
    );
    assert!(
        connected.contains("id=\"connect-app-steps\" data-fx-oob")
            && !connected.contains("aria-current=\"step\""),
        "every step done: {connected}"
    );
    assert!(
        connected.contains("id=\"connect-app-time\" data-fx-oob"),
        "{connected}"
    );

    // Not a code of this store's: nothing to wait for.
    let unknown = body_text(
        get(
            &router,
            &format!("/stores/{id}/settings/apps/connect/{}", "ab".repeat(32)),
            &cookie,
        )
        .await,
    )
    .await;
    assert!(unknown.contains("it has run out"), "{unknown}");
    assert!(!unknown.contains("fx-poll"), "{unknown}");
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
            .list_apps(&engine_protocol::ids::ConnectionId::new(id.clone()))
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
        .list_apps(&engine_protocol::ids::ConnectionId::new(id))
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
        .list_apps(&engine_protocol::ids::ConnectionId::new(id))
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
    let hash = credentials::auth::RawToken::presented("ct_test").hash();
    let store_id = engine_protocol::ids::ConnectionId::new(id);
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

// -- An app's page: its push targets, Remove, Send a test push ------------------

/// An app connected to the store `store_id`, made as a redeemed code makes
/// one: its id.
fn connected_app(state: &AppState, store_id: &str, slug: &str, device: &str) -> crate::db::AppId {
    let db = state.db.lock();
    let raw = credentials::auth::generate_app_connect_token();
    let store = crate::db::ConnectionId::new(store_id);
    let now = crate::now_unix();
    db.create_app_connect_token(&raw.hash(), &store, device, now)
        .unwrap();
    let key = credentials::auth::generate_app_key();
    db.redeem_app_connect_token(
        &raw.hash(),
        now,
        600,
        &crate::db::NewApp {
            slug,
            name: "Monokular",
            device_name: "",
            platform: "ios",
            version: "1.4.0",
            key_hash: &key.hash(),
        },
    )
    .unwrap()
    .unwrap()
    .id
}

/// The two kinds of target, as an app registers them.
enum Kind {
    Encrypted,
    LiveActivity,
}

/// What a page must never show of the targets registered here.
const SECRETS: [&str; 3] = ["/v1/push/ph_", "secret-path", "/la/"];

/// Registers a push target of `kind` at `endpoint` for `app`.
fn target(
    state: &AppState,
    app: &crate::db::AppId,
    client_id: &str,
    kind: Kind,
    endpoint: &str,
) -> crate::db::PushTargetId {
    let keys = ::push::encrypt::ReceiverKeys::generate().subscription_keys();
    let kind = match kind {
        Kind::Encrypted => crate::db::TargetKind::Encrypted {
            p256dh: keys.p256dh,
            auth: keys.auth,
        },
        Kind::LiveActivity => crate::db::TargetKind::LiveActivity {
            include_payment_uri: true,
        },
    };
    let registered = state
        .db
        .lock()
        .register_push_target(
            &crate::db::NewPushTarget {
                app_id: app,
                client_id: &crate::api::runtime::scalars::ClientId::parse(client_id).unwrap(),
                kind: &kind,
                endpoint: &::push::Endpoint::parse(endpoint).unwrap(),
                events: &crate::apps::SaleKind::ALL,
            },
            crate::now_unix(),
        )
        .unwrap();
    match registered {
        crate::db::Registration::Registered(target) => target.id,
        other => panic!("{other:?}"),
    }
}

/// Records an attempt at a test push to `target`: answered `status`
/// (delivered when 2xx), counting against it otherwise.
fn attempted(
    state: &AppState,
    store: &str,
    app: &crate::db::AppId,
    target: &crate::db::PushTargetId,
    status: u16,
) {
    let db = state.db.lock();
    let now = crate::now_unix();
    let id = db
        .queue_test_push(&crate::db::TestPush {
            store_id: &crate::db::ConnectionId::new(store),
            app_id: app,
            target_id: target,
            event: crate::apps::SaleKind::Paid,
            at: now,
            ttl: ::push::Ttl::seconds(1800),
            due_at: now + 60,
        })
        .unwrap()
        .unwrap();
    let row = db.get_push_target(app, target).unwrap().unwrap();
    let result = if (200..300).contains(&status) {
        crate::db::PushResult::Delivered
    } else {
        crate::db::PushResult::Failed {
            error: format!("the push service answered {status}"),
            retry_at: Some(now + 5),
            failure_limit: Some(20),
        }
    };
    db.record_push_attempt(
        id,
        &row,
        &crate::db::PushAttempt {
            at: now,
            status: Some(status),
            duration_ms: 240,
            result,
        },
    )
    .unwrap();
}

fn app_page(store: &str, app: &crate::db::AppId) -> String {
    format!("/stores/{store}/settings/apps/{app}")
}

fn location(response: &axum::response::Response) -> String {
    response.headers()["location"].to_str().unwrap().to_owned()
}

/// The page shows the app's details, each target by its host alone (with
/// "(the push relay)" for the relay's), its health and its pushes, and
/// nothing of an endpoint or its keys; only the store's members see it,
/// and only for the store's own apps.
#[tokio::test]
async fn an_apps_page_shows_its_targets_by_host_only_to_its_store_alone() {
    let (state, _engine) = state_with_engine("[signup]\nmode = \"public\"\n").await;
    let router = build_router(state.clone());
    let (id, cookie) = store(&router).await;
    let app = connected_app(&state, &id, "monokular", "Front counter");
    let alerts = target(
        &state,
        &app,
        "alerts",
        Kind::Encrypted,
        "https://relay.monokulo.com/v1/push/ph_6f1c2d3e4f5a69788796a5b4c3d2e1f0",
    );
    target(
        &state,
        &app,
        "island",
        Kind::LiveActivity,
        "https://push.example.test/la/secret-path",
    );
    attempted(&state, &id, &app, &alerts, 201);
    let keys: Vec<String> = {
        let db = state.db.lock();
        db.list_push_targets(&app)
            .unwrap()
            .into_iter()
            .filter_map(|t| match t.kind {
                crate::db::TargetKind::Encrypted { p256dh, auth } => {
                    Some([p256dh.to_base64url(), auth.to_base64url()])
                }
                crate::db::TargetKind::LiveActivity { .. } => None,
            })
            .flatten()
            .collect()
    };

    let response = get(&router, &app_page(&id, &app), &cookie).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    for shown in [
        "Monokular · Front counter",
        "card-app-details",
        "2 of 8 · both healthy",
        "relay.monokulo.com</strong> (the push relay)",
        "push.example.test</strong> · since",
        "Encrypted push",
        "Live Activity",
        "Shown in the island",
        "201 in 240 ms",
        "Send a test push",
        "data-test-push",
        "dropped at 20",
        "(<code>push.keep_days</code>)",
    ] {
        assert!(html.contains(shown), "{shown} missing");
    }
    for secret in SECRETS.iter().map(|s| s.to_string()).chain(keys) {
        assert!(!html.contains(&secret), "{secret} on the page");
    }
    // The key's prefix is named, never a key.
    assert!(
        html.match_indices("ak_")
            .all(|(at, _)| html[at + 3..].starts_with("</code>")),
        "no app key"
    );

    // Another store's app: not on this store's pages.
    let token = cookie
        .split(';')
        .next()
        .and_then(|pair| pair.strip_prefix("session="))
        .unwrap();
    let other = store_with_keys(&router, &state, token, "", "USD").await.id;
    assert_eq!(
        get(&router, &app_page(&other, &app), &cookie)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let gone = crate::db::AppId::new("app_00000000000000000000000000000000");
    assert_eq!(
        get(&router, &app_page(&id, &gone), &cookie).await.status(),
        StatusCode::NOT_FOUND
    );
    // Someone else's store: as if it weren't there.
    let stranger = crate::http::test_support::signed_up_and_logged_in_session_token(
        &router,
        "stranger@example.com",
        "correct horse battery staple",
    )
    .await;
    let stranger = format!("session={stranger}");
    assert_eq!(
        get(&router, &app_page(&id, &app), &stranger).await.status(),
        StatusCode::NOT_FOUND
    );
    // Signed out: refused, as every store page is.
    let signed_out = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(app_page(&id, &app))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        signed_out.status() == StatusCode::UNAUTHORIZED || signed_out.status().is_redirection(),
        "{}",
        signed_out.status()
    );
}

/// Each app on the Apps card has its line: healthy, failing with its run
/// and its last answer, or no targets, each linking to the app's page;
/// and Connect an app is the card's head.
#[tokio::test]
async fn the_apps_card_says_how_each_apps_pushes_are_doing() {
    let (state, _engine) = state_with_engine("").await;
    let router = build_router(state.clone());
    let (id, cookie) = store(&router).await;
    let healthy = connected_app(&state, &id, "monokular", "Front counter");
    let fine = target(
        &state,
        &healthy,
        "alerts",
        Kind::Encrypted,
        "https://relay.monokulo.com/v1/push/ph_1",
    );
    target(
        &state,
        &healthy,
        "island",
        Kind::LiveActivity,
        "https://relay.monokulo.com/v1/push/ph_2",
    );
    attempted(&state, &id, &healthy, &fine, 201);
    let failing = connected_app(&state, &id, "tillwatch", "Kitchen");
    let bad = target(
        &state,
        &failing,
        "alerts",
        Kind::Encrypted,
        "https://push.example.test/secret-path",
    );
    for _ in 0..3 {
        attempted(&state, &id, &failing, &bad, 502);
    }
    let none = connected_app(&state, &id, "monokular", "Back office iPad");

    let response = get(&router, &format!("/stores/{id}/settings?tab=apps"), &cookie).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("Connected apps"), "the card's title");
    assert!(
        html.contains(r#"aria-label="Connect an app""#),
        "its head's action"
    );
    let line = |app: &crate::db::AppId| {
        let link = format!(r#"href="{}""#, app_page(&id, app));
        let at = html
            .find(&link)
            .unwrap_or_else(|| panic!("a link to {app}"));
        let start = html[..at].rfind(r#"<span class="app-pushes""#).unwrap();
        html[start..at].to_owned()
    };
    let healthy = line(&healthy);
    assert!(healthy.contains(r#"data-pushes="healthy""#), "{healthy}");
    assert!(
        healthy.contains("2 push targets · last push just now ·"),
        "{healthy}"
    );
    let failing = line(&failing);
    assert!(failing.contains(r#"data-pushes="failing""#), "{failing}");
    assert!(
        failing.contains("1 push target · none delivered yet ·"),
        "{failing}"
    );
    assert!(
        !failing.contains("no push yet"),
        "pushes were tried, so not 'no push yet': {failing}"
    );
    assert!(
        failing.contains("1 failing (3 in a row, last 502 just now)"),
        "{failing}"
    );
    let none = line(&none);
    assert!(none.contains(r#"data-pushes="none""#), "{none}");
    assert!(
        none.contains("No push targets: the app registers one once it"),
        "{none}"
    );
    for secret in SECRETS {
        assert!(!html.contains(secret), "{secret} on the card");
    }
}

/// Remove deletes the one target named, of that store's app, with its
/// pushes, and comes back to the page saying so; a target that isn't the
/// app's (another app's, gone already) is 404 and stays.
#[tokio::test]
async fn remove_deletes_only_the_named_target_of_that_app() {
    let (state, _engine) = state_with_engine("").await;
    let router = build_router(state.clone());
    let (id, cookie) = store(&router).await;
    let app = connected_app(&state, &id, "monokular", "Front counter");
    let other = connected_app(&state, &id, "monokular", "Back office");
    let gone = target(
        &state,
        &app,
        "alerts",
        Kind::Encrypted,
        "https://relay.monokulo.com/v1/push/ph_1",
    );
    attempted(&state, &id, &app, &gone, 201);
    let kept = target(
        &state,
        &app,
        "island",
        Kind::LiveActivity,
        "https://relay.monokulo.com/v1/push/ph_2",
    );
    let theirs = target(
        &state,
        &other,
        "alerts",
        Kind::Encrypted,
        "https://relay.monokulo.com/v1/push/ph_3",
    );
    let remove = |app: &crate::db::AppId, target: &crate::db::PushTargetId| {
        form_post(
            &format!("{}/push-targets/{target}/remove", app_page(&id, app)),
            Some(&cookie),
            &[],
        )
    };

    let removed = router.clone().oneshot(remove(&app, &gone)).await.unwrap();
    assert_eq!(removed.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        location(&removed),
        format!("{}?removed=1#card-push-targets", app_page(&id, &app))
    );
    {
        let db = state.db.lock();
        assert!(db.get_push_target(&app, &gone).unwrap().is_none());
        assert!(db.get_push_target(&app, &kept).unwrap().is_some());
        assert!(db.get_push_target(&other, &theirs).unwrap().is_some());
        assert_eq!(
            db.push_delivery_count_of_app(&app).unwrap(),
            0,
            "its pushes went with it"
        );
    }
    let html = body_text(get(&router, &location(&removed), &cookie).await).await;
    assert!(html.contains("Push target removed."), "the toast");
    assert!(html.contains("1 of 8"), "one left");

    // Again: it's gone. Another app's target through this app's page: not this app's.
    for (app, target) in [(&app, &gone), (&app, &theirs)] {
        let refused = router.clone().oneshot(remove(app, target)).await.unwrap();
        assert_eq!(refused.status(), StatusCode::NOT_FOUND);
    }
    assert!(state
        .db
        .lock()
        .get_push_target(&other, &theirs)
        .unwrap()
        .is_some());
}

/// Send a test push from the page: queued and sent at once by the worker's
/// own code, refused on a private address as any push is (no request is
/// made), shown under Recent pushes as a test with no order, its toast
/// saying how it went; the store's 7th in a minute is refused, nothing
/// queued.
#[tokio::test]
async fn a_test_push_from_the_page_is_sent_recorded_and_limited() {
    let (state, _engine) = state_with_engine("").await;
    let router = build_router(state.clone());
    let (id, cookie) = store(&router).await;
    let app = connected_app(&state, &id, "monokular", "Front counter");
    let alerts = target(
        &state,
        &app,
        "alerts",
        Kind::Encrypted,
        "https://10.1.2.3/secret-path",
    );
    let send = |event: &str| {
        form_post(
            &format!("{}/test-push", app_page(&id, &app)),
            Some(&cookie),
            &[("target", alerts.as_str()), ("event", event)],
        )
    };

    let sent = router.clone().oneshot(send("sale.expired")).await.unwrap();
    assert_eq!(sent.status(), StatusCode::SEE_OTHER);
    let to = location(&sent);
    assert!(
        to.starts_with(&format!("{}?tested=", app_page(&id, &app))),
        "{to}"
    );
    let html = body_text(get(&router, &to, &cookie).await).await;
    assert!(html.contains("Test push failed."), "the toast");
    assert!(
        html.contains("10.1.2.3: refused: a private address; it is tried again"),
        "why"
    );
    assert!(html.contains("data-test-push"), "the test tag");
    assert!(html.contains("<code class=\"event-name\">sale.expired</code>"));
    assert!(!html.contains("secret-path"), "no endpoint");
    {
        let db = state.db.lock();
        let pushes = db.push_deliveries_of_app(&app, 10).unwrap();
        assert_eq!(pushes.len(), 1);
        assert!(pushes[0].is_test());
        assert_eq!(pushes[0].attempt_count, 1);
        assert_eq!(
            db.get_push_target(&app, &alerts).unwrap().unwrap().failures,
            1,
            "it counts"
        );
    }

    for _ in 1..crate::webhooks::test_delivery::TESTS_PER_MINUTE {
        let sent = router.clone().oneshot(send("sale.paid")).await.unwrap();
        assert_eq!(sent.status(), StatusCode::SEE_OTHER);
    }
    let refused = router.clone().oneshot(send("sale.paid")).await.unwrap();
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    let html = body_text(refused).await;
    assert!(
        html.contains("This store has sent 6 test pushes in the last minute. Try again in"),
        "{html}"
    );
    assert_eq!(
        state.db.lock().push_delivery_count_of_app(&app).unwrap(),
        6,
        "nothing more queued"
    );

    // An event that isn't one, a target that isn't the app's: refused.
    let wrong = router.clone().oneshot(send("order.paid")).await.unwrap();
    assert_eq!(wrong.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

/// A test push that is its target's last allowed failure in a row deletes
/// the target, and the push with it, as any push does: the redirect carries
/// what the toast says, since nothing is left to read back; a link carrying
/// anything but those words shows no toast.
#[tokio::test]
async fn a_test_push_that_drops_its_target_still_says_how_it_went() {
    let (state, _engine) = state_with_engine("").await;
    let router = build_router(state.clone());
    let (id, cookie) = store(&router).await;
    let app = connected_app(&state, &id, "monokular", "Front counter");
    let alerts = target(
        &state,
        &app,
        "alerts",
        Kind::Encrypted,
        "https://10.1.2.3/secret-path",
    );
    let limit = state.settings.push.load().target_failure_limit;
    for _ in 1..limit {
        attempted(&state, &id, &app, &alerts, 502);
    }

    let sent = router
        .clone()
        .oneshot(form_post(
            &format!("{}/test-push", app_page(&id, &app)),
            Some(&cookie),
            &[("target", alerts.as_str()), ("event", "sale.paid")],
        ))
        .await
        .unwrap();
    assert_eq!(sent.status(), StatusCode::SEE_OTHER);
    let to = location(&sent);
    assert_eq!(
        to,
        format!(
            "{}?dropped=limit&host=10.1.2.3&trouble=private-address#card-push-targets",
            app_page(&id, &app)
        )
    );
    assert!(
        state
            .db
            .lock()
            .get_push_target(&app, &alerts)
            .unwrap()
            .is_none(),
        "dropped"
    );
    let html = body_text(get(&router, &to, &cookie).await).await;
    assert!(html.contains("Test push failed."), "the toast");
    assert!(
        html.contains(&format!(
            "10.1.2.3: refused: a private address (its {limit}th failure in a row), so the target was dropped; the app registers again when it next opens."
        )),
        "why"
    );
    assert!(!html.contains("secret-path"), "no endpoint");

    for forged in [
        "?dropped=limit&host=%3Cscript%3E&status=502",
        "?dropped=anything&host=10.1.2.3",
        "?dropped=limit",
    ] {
        let html = body_text(
            get(
                &router,
                &format!("{}{forged}", app_page(&id, &app)),
                &cookie,
            )
            .await,
        )
        .await;
        assert!(!html.contains("Test push failed."), "{forged}");
    }
}

/// The test push lands on a push service (a stub on this machine, which
/// `webhooks.allow_private_urls` lets it reach) and the page's toast says
/// it was delivered, by whom and how fast.
#[tokio::test]
async fn a_test_push_from_the_page_is_delivered_over_http() {
    let (state, _engine) = state_with_engine("[webhooks]\nallow_private_urls = true\n").await;
    let router = build_router(state.clone());
    let (id, cookie) = store(&router).await;
    let received =
        std::sync::Arc::new(parking_lot::Mutex::new(Vec::<axum::http::HeaderMap>::new()));
    let stub = {
        let received = received.clone();
        axum::Router::new().fallback(move |headers: axum::http::HeaderMap| {
            let received = received.clone();
            async move {
                received.lock().push(headers);
                StatusCode::CREATED
            }
        })
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });
    let app = connected_app(&state, &id, "monokular", "Front counter");
    let island = target(
        &state,
        &app,
        "island",
        Kind::LiveActivity,
        &format!("http://{addr}/la/secret-path"),
    );

    let sent = router
        .clone()
        .oneshot(form_post(
            &format!("{}/test-push", app_page(&id, &app)),
            Some(&cookie),
            &[("target", island.as_str()), ("event", "sale.paid")],
        ))
        .await
        .unwrap();
    assert_eq!(sent.status(), StatusCode::SEE_OTHER);
    let html = body_text(get(&router, &location(&sent), &cookie).await).await;
    assert!(html.contains("Test push delivered."), "the toast");
    assert!(
        html.contains("127.0.0.1 answered 201 in "),
        "who and how fast"
    );
    assert!(!html.contains("secret-path"));
    let received = received.lock();
    assert_eq!(received.len(), 1, "sent once");
    assert!(received[0]["authorization"]
        .to_str()
        .unwrap()
        .starts_with("vapid "));
    assert_eq!(received[0]["urgency"], "high");
    let row = state
        .db
        .lock()
        .push_deliveries_of_app(&app, 1)
        .unwrap()
        .remove(0);
    assert!(row.last_duration_ms.is_some());
    assert!(row.delivered_at.is_some());
    let payload: serde_json::Value = serde_json::from_str(&row.payload).unwrap();
    assert_eq!(payload["test"], true);
    assert_eq!(payload["phase"], "paid");
}

/// The Apps card's line says how long ago, at the page's moment: the last
/// delivered push and the worst target's last failure, in minutes.
#[test]
fn an_apps_push_line_says_how_long_ago_its_last_push_and_failure_were() {
    use crate::db::{AppId, AppPushSummary, LastAttempt};
    use crate::views::connected_apps::PushLine;
    const NOW: i64 = 1_790_576_527;
    let clock = crate::views::time::Clock::utc(NOW);
    let summary = |failing: u32, worst_last: Option<LastAttempt>| AppPushSummary {
        app_id: AppId::new("app_front"),
        targets: 2,
        failing,
        worst_failures: if failing > 0 { 12 } else { 0 },
        worst_last,
        last_delivered_at: Some(NOW - 120),
    };
    assert_eq!(
        super::push_line(Some(&summary(0, None)), &clock),
        PushLine::Healthy {
            targets: 2,
            last_push: Some("2 min ago".into()),
        }
    );
    let failed = LastAttempt {
        at: NOW - 14 * 60,
        status: Some(502),
        error: Some("the push service answered 502".into()),
        duration_ms: Some(240),
        delivered: false,
    };
    assert_eq!(
        super::push_line(Some(&summary(1, Some(failed))), &clock),
        PushLine::Failing {
            targets: 2,
            last_push: Some("2 min ago".into()),
            failing: 1,
            in_a_row: 12,
            last_failure: Some(("502".into(), "14 min ago".into())),
        }
    );
    assert_eq!(super::push_line(None, &clock), PushLine::NoTargets);
}
