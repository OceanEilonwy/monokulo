//! Renewing an order as a till meets it (docs/decisions/order-renewal.md):
//! the `renew-order` form on an order, offered to the store's key and its
//! apps' keys only; an expired order nothing reached renewed once, priced
//! again from its own record, at the same address; the keys that may ask;
//! and its local records, which its sale events read.

use axum::http::StatusCode;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::super::recording::{connect_app, fixture, Fixture, Recorder, PUBLIC_URI};
use crate::db::{ConnectionId, OrderId};
use crate::http::test_support::{api_request, assert_problem, body_json};

async fn engine() -> engine_test_support::TestEngineHandle {
    engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await
}

async fn call(
    fixture: &Fixture,
    method: &str,
    uri: &str,
    key: &str,
    body: Option<Value>,
) -> axum::response::Response {
    fixture
        .router
        .clone()
        .oneshot(api_request(method, uri, Some(key), body))
        .await
        .unwrap()
}

/// An order of 0.25 XMR made with `key`: its body.
async fn order(fixture: &Fixture, key: &str) -> Value {
    let made = call(
        fixture,
        "POST",
        "/api/v1/orders",
        key,
        Some(json!({"amount": "0.25", "currency": "XMR", "merchant_order_id": "#1042"})),
    )
    .await;
    assert_eq!(made.status(), StatusCode::CREATED);
    body_json(made).await
}

/// The order's own path.
fn path(order: &Value) -> String {
    format!("/api/v1/orders/{}", order["order_id"].as_str().unwrap())
}

/// An order carries its `renew-order` form, on itself, for the store's
/// secret key and its apps' keys; the public key isn't offered it, and may
/// not send it.
#[tokio::test]
async fn the_renew_order_form_is_on_the_order_for_the_stores_keys_only() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let made = order(&fixture, &fixture.ak).await;
    let form = &made["_templates"]["renew-order"];
    assert_eq!(form["method"], "POST", "{made}");
    assert_eq!(form["target"], path(&made));
    assert_eq!(form["properties"][0]["name"], "expires_at");
    for key in [fixture.sk.as_str(), fixture.ak.as_str()] {
        let read = body_json(call(&fixture, "GET", &path(&made), key, None).await).await;
        assert_eq!(read["_templates"]["renew-order"]["target"], path(&made));
    }
    let public = body_json(call(&fixture, "GET", &path(&made), &fixture.pk, None).await).await;
    assert!(public["_templates"]["renew-order"].is_null(), "{public}");
    let refused = call(
        &fixture,
        "POST",
        &path(&made),
        &fixture.pk,
        Some(json!({"expires_at": made["expires_at"]})),
    )
    .await;
    assert_problem(refused, "forbidden").await;
}

/// An app's expired order, nothing paid to it, renewed by another of the
/// store's apps: the same order and address, priced again from its own
/// record at the rate now, with a fresh expiry, pending; its local records
/// follow. Asked again for the same quote (the first till, at once), it is
/// answered as it is, renewed once.
#[tokio::test]
async fn an_expired_unpaid_order_is_renewed_once_keeping_its_id_and_address() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let other = connect_app(&fixture.router, &fixture.store_id, &fixture.cookie).await;
    let other = other["app_key"].as_str().unwrap().to_owned();
    let made = order(&fixture, &fixture.ak).await;
    let id = made["order_id"].as_str().unwrap();
    engine.mark_order_expired(id).unwrap();
    let expired = body_json(call(&fixture, "GET", &path(&made), &fixture.ak, None).await).await;
    assert_eq!(expired["status"], "expired");
    let ended = expired["expires_at"].as_i64().unwrap();
    // The price in its record moved, as a fiat price does with the rate:
    // the renewal quotes it again.
    let store = ConnectionId::new(fixture.store_id.clone());
    fixture
        .state
        .db
        .lock()
        .conn_for_test()
        .execute(
            "UPDATE order_currency_metadata SET amount = '0.30' WHERE order_id = ?1",
            [id],
        )
        .unwrap();

    let renewed = call(
        &fixture,
        "POST",
        &path(&made),
        &other,
        Some(json!({"expires_at": ended})),
    )
    .await;
    assert_eq!(renewed.status(), StatusCode::OK);
    let renewed = body_json(renewed).await;
    assert_eq!(renewed["order_id"], id);
    assert_eq!(renewed["address"], made["address"]);
    assert_eq!(renewed["status"], "pending");
    assert_eq!(renewed["xmr_amount_piconero"], 300_000_000_000u64);
    let expires_at = renewed["expires_at"].as_i64().unwrap();
    assert!(
        expires_at > clock::Clock::System.now().as_secs(),
        "a fresh expiry: {renewed}"
    );
    assert_eq!(renewed["_templates"]["renew-order"]["target"], path(&made));

    let app_order = fixture
        .state
        .db
        .lock()
        .app_order(&OrderId::new(id.to_owned()))
        .unwrap()
        .unwrap();
    assert_eq!(
        (app_order.xmr_amount_piconero.get(), app_order.expires_at),
        (300_000_000_000, expires_at)
    );
    let metadata = fixture
        .state
        .db
        .lock()
        .get_order_currency_metadata(&store, &OrderId::new(id.to_owned()))
        .unwrap()
        .unwrap();
    assert_eq!(metadata.provider, "xmr");

    // The first till, asking about the same quote: one renewal.
    let again = call(
        &fixture,
        "POST",
        &path(&made),
        &fixture.ak,
        Some(json!({"expires_at": ended})),
    )
    .await;
    assert_eq!(again.status(), StatusCode::OK);
    let again = body_json(again).await;
    assert_eq!(
        (again["expires_at"].as_i64(), &again["xmr_amount_piconero"]),
        (Some(expires_at), &json!(300_000_000_000u64))
    );

    // Pending, it isn't renewed: there's nothing to renew.
    let refused = call(
        &fixture,
        "POST",
        &path(&made),
        &fixture.sk,
        Some(json!({"expires_at": expires_at})),
    )
    .await;
    let problem = assert_problem(refused, "order-not-renewable").await;
    assert!(
        problem["detail"].as_str().unwrap().contains("pending"),
        "{problem}"
    );
}

/// An order something reached is never renewed, even expired; an app's
/// key renews only orders the store's apps made; and an order the store
/// doesn't have isn't found.
#[tokio::test]
async fn a_paid_to_order_a_shops_order_and_another_stores_are_refused() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let paid_to = order(&fixture, &fixture.ak).await;
    let id = paid_to["order_id"].as_str().unwrap();
    // A part, still in the pool.
    engine
        .record_order_payment(id, pricing::xmr_amount::Piconero(1_000), None)
        .unwrap();
    engine.mark_order_expired(id).unwrap();
    let read = body_json(call(&fixture, "GET", &path(&paid_to), &fixture.ak, None).await).await;
    assert_eq!(read["status"], "expired");
    let refused = call(
        &fixture,
        "POST",
        &path(&paid_to),
        &fixture.ak,
        Some(json!({"expires_at": read["expires_at"]})),
    )
    .await;
    let problem = assert_problem(refused, "order-not-renewable").await;
    assert!(
        problem["detail"].as_str().unwrap().contains("payment"),
        "{problem}"
    );

    let shops = order(&fixture, &fixture.sk).await;
    let shop_id = shops["order_id"].as_str().unwrap();
    engine.mark_order_expired(shop_id).unwrap();
    let read = body_json(call(&fixture, "GET", &path(&shops), &fixture.sk, None).await).await;
    let body = json!({"expires_at": read["expires_at"]});
    let refused = call(
        &fixture,
        "POST",
        &path(&shops),
        &fixture.ak,
        Some(body.clone()),
    )
    .await;
    assert_problem(refused, "forbidden").await;
    let renewed = call(&fixture, "POST", &path(&shops), &fixture.sk, Some(body)).await;
    assert_eq!(renewed.status(), StatusCode::OK, "the store's own key may");

    let missing = call(
        &fixture,
        "POST",
        "/api/v1/orders/order_00000000000000000000000000000000",
        &fixture.sk,
        Some(json!({"expires_at": 1})),
    )
    .await;
    assert_problem(missing, "not-found").await;
}
