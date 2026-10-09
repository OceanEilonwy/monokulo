//! HTTP-layer integration tests, driven through the real `Router` via
//! `tower::ServiceExt::oneshot` - no bound snp needed. These exercise the same
//! IDOR/auth properties `store.rs` already tests at the repository level, but end to
//! end through real request parsing, auth extraction, and JSON (de)serialization,
//! per `docs/TESTING.md` §5.

use std::collections::HashMap;
use std::str::FromStr as _;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt as _;
use monero::consensus::encode::deserialize;
use monero::{Network, PrivateKey, PublicKey, Transaction};
use tower::ServiceExt as _;

use crate::daemon::fake::FakeDaemonClient;
use crate::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use crate::key_custody::{KeyCustody, PlainKeyCustody};
use crate::store::Store;

use super::rate_limit::RateLimiter;
use super::router_as_monokulo as build_router;
use super::{
    request_limit_middleware, stream_limit_middleware, ApiError, AppState, RequestLimits,
    TEST_ENGINE_TOKEN,
};

fn valid_scalar_bytes(seed: u8) -> [u8; 32] {
    let mut b = [seed; 32];
    b[31] &= 0x0f;
    b
}

fn valid_view_key_hex(seed: u8) -> String {
    hex::encode(valid_scalar_bytes(seed))
}

fn valid_spend_pubkey_hex(seed: u8) -> String {
    let secret = PrivateKey::from_slice(&valid_scalar_bytes(seed)).unwrap();
    hex::encode(PublicKey::from_private_key(&secret).to_bytes())
}

fn test_router() -> Router {
    build_router(AppState::for_tests(), 1_000_000)
}

fn json_request(
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    body: &serde_json::Value,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

struct TestTenant {
    public_key: String,
    secret_token: String,
}

async fn create_tenant(router: &Router, seed: u8) -> TestTenant {
    let req = json_request(
        "POST",
        "/api/v1/admin/tenants",
        None,
        &serde_json::json!({
            "view_key_hex": valid_view_key_hex(seed),
            "spend_pubkey_hex": valid_spend_pubkey_hex(seed.wrapping_add(1)),
        }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    TestTenant {
        public_key: body["public_key"].as_str().unwrap().to_owned(),
        secret_token: body["secret_token"].as_str().unwrap().to_owned(),
    }
}

#[tokio::test]
async fn create_tenant_then_create_order_happy_path() {
    let router = test_router();
    let tenant = create_tenant(&router, 1).await;

    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        &serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64 }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["xmr_amount_piconero"], 167_500_000_000u64);
    let order_id = body["order_id"].as_str().unwrap().to_owned();
    assert!(!body["address"].as_str().unwrap().is_empty());

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/admin/tenant/orders/{order_id}"))
        .header("authorization", format!("Bearer {}", tenant.secret_token))
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["status"], "pending");
    assert_eq!(body["order_id"], order_id);
}

#[tokio::test]
async fn creating_an_order_with_a_confirmations_required_override_persists_it() {
    let state = AppState::for_tests();
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;

    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        &serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64, "confirmations_required": 3 }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let order_id = body_json(response).await["order_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let guard = store.lock();
    let tenant_id = guard
        .list_active_tenants()
        .unwrap()
        .into_iter()
        .find(|t| t.public_key == tenant.public_key)
        .unwrap()
        .id;
    let order = guard
        .get_order(&tenant_id, &shared::ids::OrderId::new(order_id))
        .unwrap()
        .unwrap();
    assert_eq!(order.confirmations_required_override, Some(3));
}

#[tokio::test]
async fn creating_an_order_with_no_confirmations_required_override_leaves_it_unset() {
    let state = AppState::for_tests();
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;

    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        &serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64 }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let order_id = body_json(response).await["order_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let guard = store.lock();
    let tenant_id = guard
        .list_active_tenants()
        .unwrap()
        .into_iter()
        .find(|t| t.public_key == tenant.public_key)
        .unwrap()
        .id;
    let order = guard
        .get_order(&tenant_id, &shared::ids::OrderId::new(order_id))
        .unwrap()
        .unwrap();
    assert_eq!(order.confirmations_required_override, None);
}

#[tokio::test]
async fn creating_an_order_with_an_out_of_range_confirmations_required_is_rejected() {
    // `0` is no longer out of range - native 0-conf, see `status::derive_status`'s
    // own doc comment - so the only remaining bad value is over the 720 cap.
    let router = test_router();
    let tenant = create_tenant(&router, 1).await;

    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        &serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64, "confirmations_required": 721u64 }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "confirmations_required=721 must be rejected"
    );
}

#[tokio::test]
async fn creating_an_order_with_confirmations_required_zero_is_accepted() {
    let state = AppState::for_tests();
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;

    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        &serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64, "confirmations_required": 0u64 }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let order_id = body_json(response).await["order_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let guard = store.lock();
    let tenant_id = guard
        .list_active_tenants()
        .unwrap()
        .into_iter()
        .find(|t| t.public_key == tenant.public_key)
        .unwrap()
        .id;
    let order = guard
        .get_order(&tenant_id, &shared::ids::OrderId::new(order_id))
        .unwrap()
        .unwrap();
    assert_eq!(order.confirmations_required_override, Some(0));
}

/// Regression test: a `spend_pubkey_hex` that's the right length and valid hex
/// (so `WalletMaterial::from_hex` accepts it) but isn't actually a point on the
/// curve used to fail all the way through as a bare `500 Internal Server Error`
/// with no indication the *caller* sent something wrong - a `KeyCustodyError::
/// InvalidKeyMaterial` from `register_wallet`'s `to_view_pair()` call fell
/// through `http/mod.rs`'s generic `From<KeyCustodyError> for ApiError` (`Internal`
/// by design for most callers - see that mapping's own doc comment) uncaught.
/// `admin::create_tenant` now maps it explicitly via
/// `key_custody_error_for_new_tenant`, since here it genuinely is the caller's
/// mistake, exactly like a badly-formed hex string a few lines earlier already
/// is. Confirmed against a real curve-point check, not a mocked error - see
/// `daemon_fallback`/`key_custody` for why this codebase treats "trust real
/// crypto libraries over hand-rolled validation" as a hard rule.
#[tokio::test]
async fn create_tenant_rejects_a_syntactically_valid_but_off_curve_spend_pubkey_as_bad_request_not_internal_error(
) {
    let router = test_router();
    let req = json_request(
        "POST",
        "/api/v1/admin/tenants",
        None,
        &serde_json::json!({
            "view_key_hex": valid_view_key_hex(1),
            // 32 well-formed hex bytes, all 0xff - not a valid Ed25519/Monero
            // curve point (real, verified: this genuinely fails
            // `PublicKey::from_slice`, not assumed).
            "spend_pubkey_hex": "ff".repeat(32),
        }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "an off-curve spend key is the caller's mistake, not a server failure"
    );
    let body = body_json(response).await;
    let message = body["error"].as_str().unwrap();
    assert!(
        message.contains("spend public key"),
        "expected the real validation message, got: {message}"
    );
}

/// Same regression, for the view key half of the pair (`PrivateKey::from_slice`
/// rejects a non-canonical scalar - one that hasn't been reduced mod the curve
/// order - the same way `PublicKey::from_slice` rejects an off-curve point
/// above).
#[tokio::test]
async fn create_tenant_rejects_a_non_canonical_view_key_scalar_as_bad_request_not_internal_error() {
    let router = test_router();
    let req = json_request(
        "POST",
        "/api/v1/admin/tenants",
        None,
        &serde_json::json!({
            // 0xff * 32 as a little-endian scalar is far larger than the curve
            // order l - a real non-canonical scalar, not merely hypothetical.
            "view_key_hex": "ff".repeat(32),
            "spend_pubkey_hex": valid_spend_pubkey_hex(1),
        }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    let message = body["error"].as_str().unwrap();
    assert!(
        message.contains("view key"),
        "expected the real validation message, got: {message}"
    );
}

#[tokio::test]
async fn successive_orders_get_distinct_addresses_and_never_leave_an_unclaimed_index_behind() {
    // Order creation no longer allocates a minor index up front and inserts the
    // order row later, after awaiting a subaddress derivation: `next_minor_index` is
    // what the scanner reads to decide which subaddresses it scans, so the gap
    // between the two was a window in which a scanner tick could match a real output
    // against an index no order existed for and drop it - permanently, if the
    // sighting was inside a mined block. The peek/derive/claim-and-insert sequence
    // that replaces it must still hand out one distinct address per order and must
    // not skip indices along the way (a skipped index means an address was derived,
    // counted, and never issued to anyone).
    let state = AppState::for_tests();
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;

    let mut addresses = Vec::new();
    for _ in 0..3 {
        let req = json_request(
            "POST",
            "/api/v1/admin/tenant/orders",
            Some(&tenant.secret_token),
            &serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64 }),
        );
        let response = router.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        addresses.push(
            body_json(response).await["address"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }

    let mut deduped = addresses.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(
        deduped.len(),
        3,
        "every order must be issued its own subaddress"
    );

    let s = store.lock();
    let tenant_row = s
        .find_tenant_by_secret_token(&shared::auth::RawToken::presented(&tenant.secret_token))
        .unwrap()
        .unwrap();
    assert_eq!(
        tenant_row.next_minor_index, 4,
        "indices 1..3 were issued, so the counter must sit at exactly 4 - no gaps for addresses nobody holds"
    );
    // The scanner scans `0..next_minor_index`, so every index the counter covers
    // must resolve to a real order - that is precisely the invariant whose violation
    // let a matched payment be dropped.
    for minor in 1..tenant_row.next_minor_index {
        assert!(
            s.find_order_by_minor_index(&tenant_row.id, minor)
                .unwrap()
                .is_some(),
            "minor index {minor} is scannable but has no order to attribute a payment to"
        );
    }
}

#[tokio::test]
async fn admin_route_rejects_the_tenants_own_public_key_as_a_bearer_token() {
    let router = test_router();
    let tenant = create_tenant(&router, 2).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/admin/tenant")
        .header("authorization", format!("Bearer {}", tenant.public_key))
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_route_with_no_authorization_header_is_rejected() {
    let router = test_router();
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/admin/tenant")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tenant_a_cannot_read_tenant_bs_order_via_admin_api() {
    // The exact IDOR scenario from docs/DESIGN.md §10.1 and docs/TESTING.md §5,
    // exercised end to end through real HTTP requests: tenant A's valid sk_ plus
    // tenant B's real order_id must come back as 404, not tenant B's order.
    let router = test_router();
    let tenant_a = create_tenant(&router, 3).await;
    let tenant_b = create_tenant(&router, 4).await;

    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant_b.secret_token),
        &serde_json::json!({ "xmr_amount_piconero": 67_000_000_000u64 }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    let body = body_json(response).await;
    let order_b_order_id = body["order_id"].as_str().unwrap().to_owned();

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/admin/tenant/orders/{order_b_order_id}"))
        .header("authorization", format!("Bearer {}", tenant_a.secret_token))
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Sanity check: tenant B's own token DOES see it, proving the 404 above is
    // specifically about cross-tenant scoping and not a broken route.
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/admin/tenant/orders/{order_b_order_id}"))
        .header("authorization", format!("Bearer {}", tenant_b.secret_token))
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn rotated_secret_invalidates_the_old_token_end_to_end() {
    let router = test_router();
    let tenant = create_tenant(&router, 5).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/admin/tenant/rotate-secret")
        .header("authorization", format!("Bearer {}", tenant.secret_token))
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let new_secret = body["secret_token"].as_str().unwrap().to_owned();

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/admin/tenant")
        .header("authorization", format!("Bearer {}", tenant.secret_token))
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "the pre-rotation token must stop working immediately"
    );

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/admin/tenant")
        .header("authorization", format!("Bearer {new_secret}"))
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// The engine is private: its old public, `pk_`-addressed order routes are
/// gone (orders are created, read and updated only through the `sk_` admin
/// API, by monokulo), and no route grants CORS to any browser origin.
#[tokio::test]
async fn the_engine_serves_no_public_order_routes_and_no_cors() {
    let router = test_router();
    let tenant = create_tenant(&router, 6).await;
    let order_id = create_admin_order(&router, &tenant).await;

    let pk = &tenant.public_key;
    for (method, uri) in [
        ("POST", format!("/api/v1/t/{pk}/orders")),
        ("GET", format!("/api/v1/t/{pk}/orders/{order_id}")),
        (
            "POST",
            format!("/api/v1/t/{pk}/orders/{order_id}/refund-address"),
        ),
    ] {
        let req = json_request(
            method,
            &uri,
            None,
            &serde_json::json!({ "xmr_amount_piconero": 1u64, "refund_address": "x" }),
        );
        let response = router.clone().oneshot(req).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{method} {uri} must not exist"
        );
    }

    for uri in [
        format!("/api/v1/t/{pk}/orders"),
        "/api/v1/admin/tenant/orders".to_owned(),
        "/api/v1/admin/tenants".to_owned(),
        "/status".to_owned(),
    ] {
        let preflight = Request::builder()
            .method("OPTIONS")
            .uri(&uri)
            .header("origin", "https://merchant.example")
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "content-type")
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(preflight).await.unwrap();
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none(),
            "{uri} must not grant CORS"
        );
    }
}

#[tokio::test]
async fn webhook_lifecycle_is_scoped_to_the_owning_tenant() {
    let router = test_router();
    let tenant_a = create_tenant(&router, 7).await;
    let tenant_b = create_tenant(&router, 8).await;

    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/webhooks",
        Some(&tenant_b.secret_token),
        &serde_json::json!({ "url": "https://b.example/hook" }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let webhook_id = body["webhook_id"].as_str().unwrap().to_owned();
    assert!(body["signing_secret"]
        .as_str()
        .unwrap()
        .starts_with("whsec_"));

    // Tenant A cannot delete tenant B's webhook.
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/admin/tenant/webhooks/{webhook_id}"))
        .header("authorization", format!("Bearer {}", tenant_a.secret_token))
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Tenant B can.
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/admin/tenant/webhooks/{webhook_id}"))
        .header("authorization", format!("Bearer {}", tenant_b.secret_token))
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_zero_or_malformed_xmr_amount_is_rejected_with_bad_request() {
    // This engine no longer has any concept of fiat/exchange rates
    // (`docs/fx_refactor.md` decision 2) - a caller supplies the exact
    // `xmr_amount_piconero` an order is worth directly, so the only amount-shaped
    // validation left here is "an order can't be worth exactly nothing" and "the
    // field must actually be present and correctly typed."
    let router = test_router();
    let tenant = create_tenant(&router, 9).await;

    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        &serde_json::json!({ "xmr_amount_piconero": 0u64 }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // A string where the extractor expects a `u64` never reaches the handler
    // body at all - axum's own `Json<T>` rejection fires first, with its
    // standard `422 Unprocessable Entity` (not this handler's own `400`s,
    // which only cover validation the handler itself performs).
    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        &serde_json::json!({ "xmr_amount_piconero": "not_a_number" }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

/// A creation repeating an idempotency key gets the order that key made
/// (same id, same address, no second subaddress claimed); the key reused
/// for a different purchase is refused; a bad key is refused up front.
#[tokio::test]
async fn an_idempotency_key_makes_a_retried_order_creation_return_the_first_order() {
    let router = test_router();
    let tenant = create_tenant(&router, 11).await;
    let create = |body: serde_json::Value| {
        let router = router.clone();
        let token = tenant.secret_token.clone();
        async move {
            let response = router
                .oneshot(json_request(
                    "POST",
                    "/api/v1/admin/tenant/orders",
                    Some(&token),
                    &body,
                ))
                .await
                .unwrap();
            (response.status(), body_json(response).await)
        }
    };
    let body = serde_json::json!({
        "xmr_amount_piconero": 5_000_000_000u64,
        "merchant_order_id": "wc-77",
        "idempotency_key": "pay:wc-77:1",
    });
    let (status, first) = create(body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, again) = create(body).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["order_id"], first["order_id"]);
    assert_eq!(again["address"], first["address"]);

    // A different purchase under the same key: refused, nothing made.
    let (status, clash) = create(serde_json::json!({
        "xmr_amount_piconero": 6_000_000_000u64,
        "merchant_order_id": "wc-77",
        "idempotency_key": "pay:wc-77:1",
    }))
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{clash}");

    // Without a key, or with another, a new order (and the next address).
    let (status, other) = create(serde_json::json!({
        "xmr_amount_piconero": 5_000_000_000u64,
        "merchant_order_id": "wc-77",
        "idempotency_key": "pay:wc-77:2",
    }))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(other["order_id"], first["order_id"]);
    assert_ne!(other["address"], first["address"]);

    for bad in ["", "has space", &"k".repeat(129)] {
        let (status, _) = create(serde_json::json!({
            "xmr_amount_piconero": 5_000_000_000u64,
            "idempotency_key": bad,
        }))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
    }
}

/// An order above what SQLite's signed integer holds would be stored
/// negative; it is refused with the limit named.
#[tokio::test]
async fn an_xmr_amount_above_the_cap_is_rejected_with_bad_request() {
    let router = test_router();
    let tenant = create_tenant(&router, 9).await;
    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        &serde_json::json!({ "xmr_amount_piconero": u64::MAX }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(body_json(response).await["error"]
        .as_str()
        .unwrap()
        .contains(&crate::http::orders::MAX_ORDER_PICONERO.to_string()));
    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        &serde_json::json!({ "xmr_amount_piconero": crate::http::orders::MAX_ORDER_PICONERO }),
    );
    assert_eq!(
        router.oneshot(req).await.unwrap().status(),
        StatusCode::OK,
        "the cap itself is allowed"
    );
}

/// Extra webhook headers are checked when the webhook is saved: a name or
/// value no HTTP client would send, or one the engine sets itself, would
/// otherwise fail every delivery of that webhook until it was deleted.
#[tokio::test]
async fn webhook_extra_headers_are_validated_when_saved() {
    let router = test_router();
    let tenant = create_tenant(&router, 7).await;
    let create = |extra_headers: serde_json::Value| {
        let router = router.clone();
        let token = tenant.secret_token.clone();
        async move {
            let response = router
                .oneshot(json_request(
                    "POST",
                    "/api/v1/admin/tenant/webhooks",
                    Some(&token),
                    &serde_json::json!({ "url": "https://b.example/hook", "extra_headers": extra_headers }),
                ))
                .await
                .unwrap();
            (response.status(), body_json(response).await)
        }
    };
    for (refused, why) in [
        (serde_json::json!(["x"]), "not an object"),
        (serde_json::json!({ "x-count": 3 }), "not a string"),
        (
            serde_json::json!({ "bad header": "v" }),
            "a space in the name",
        ),
        (
            serde_json::json!({ "x-note": "line\nbreak" }),
            "a newline in the value",
        ),
        (serde_json::json!({ "Host": "evil.example" }), "the host"),
        (serde_json::json!({ "Content-Length": "0" }), "the length"),
        (
            serde_json::json!({ "X-Monokulo-Signature": "forged" }),
            "the engine's own",
        ),
        (
            serde_json::json!({ "x-long": "v".repeat(5 * 1024) }),
            "too many bytes",
        ),
    ] {
        let (status, body) = create(refused).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
    }
    let too_many: serde_json::Map<String, serde_json::Value> = (0..21)
        .map(|i| (format!("x-h{i}"), serde_json::Value::String("v".into())))
        .collect();
    let (status, _) = create(serde_json::Value::Object(too_many)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "too many headers");

    // Valid ones are kept, with their names in lowercase.
    let (status, body) = create(serde_json::json!({ "X-Api-Key": "k", "x-shop": "main" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = router
        .clone()
        .oneshot(json_request(
            "GET",
            "/api/v1/admin/tenant/webhooks",
            Some(&tenant.secret_token),
            &serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(stored.status(), StatusCode::OK);
    let (status, _) = create(serde_json::Value::Null).await;
    assert_eq!(status, StatusCode::OK, "no extra headers at all");
}

#[tokio::test]
async fn tenant_creation_is_rejected_for_a_network_with_no_configured_node() {
    // The failure mode this check exists to prevent: a tenant whose address gets
    // derived for a chain nothing on this instance is actually scanning, so a
    // real payment to it would simply never be detected - a silent, much worse
    // failure than refusing to create the tenant at all.
    let router = test_router(); // only mainnet is configured, see AppState::for_tests()

    let req = json_request(
        "POST",
        "/api/v1/admin/tenants",
        None,
        &serde_json::json!({
            "view_key_hex": valid_view_key_hex(20),
            "spend_pubkey_hex": valid_spend_pubkey_hex(21),
            "network": "stagenet",
        }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // The same request without a network (defaulting to mainnet, which *is*
    // configured) must succeed - proving the rejection above is really about
    // network availability, not something else in the request.
    let req = json_request(
        "POST",
        "/api/v1/admin/tenants",
        None,
        &serde_json::json!({
            "view_key_hex": valid_view_key_hex(20),
            "spend_pubkey_hex": valid_spend_pubkey_hex(21),
        }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn tenant_deletion_disables_it_and_admin_routes_stop_working() {
    let router = test_router();
    let tenant = create_tenant(&router, 10).await;

    let req = Request::builder()
        .method("DELETE")
        .uri("/api/v1/admin/tenant")
        .header("authorization", format!("Bearer {}", tenant.secret_token))
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/admin/tenant")
        .header("authorization", format!("Bearer {}", tenant.secret_token))
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // A disabled tenant's key must also stop working for order creation.
    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        &serde_json::json!({ "xmr_amount_piconero": 67_000_000_000u64 }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// A request that authenticated just before its store was deleted finds
/// no live handle afterwards; it must not register the store's keys again
/// (nothing would ever remove them), it is simply refused.
#[tokio::test]
async fn a_deleted_stores_keys_are_not_registered_again_by_a_request_in_flight() {
    let state = AppState::for_tests();
    let router = build_router(state.clone(), 1_000_000);
    let created = create_tenant(&router, 10).await;
    let tenant = state
        .db
        .lock()
        .find_tenant_by_secret_token(&shared::auth::RawToken::presented(&created.secret_token))
        .unwrap()
        .unwrap();
    assert!(state.custody.wallet_handles.read().contains_key(&tenant.id));

    let delete = Request::builder()
        .method("DELETE")
        .uri("/api/v1/admin/tenant")
        .header("authorization", format!("Bearer {}", created.secret_token))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.oneshot(delete).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    assert!(!state.custody.wallet_handles.read().contains_key(&tenant.id));

    // The stale `Tenant` an in-flight request still holds.
    let resolved = crate::http::resolve_wallet_handle(&state, &tenant).await;
    assert!(
        matches!(resolved, Err(ApiError::Unauthorized)),
        "{resolved:?}"
    );
    assert!(
        !state.custody.wallet_handles.read().contains_key(&tenant.id),
        "no handle came back"
    );
}

#[tokio::test]
async fn unauthenticated_routes_are_limited_per_address_by_the_admin_limiter() {
    // Tenant creation and `/status` carry no token, so the admin limiter keys
    // them on the caller's address (with a fabricated `ConnectInfo`, the way
    // production's `into_make_service_with_connect_info` provides it).
    let mut state = AppState::for_tests();
    state.admin_rate_limiter = Arc::new(RateLimiter::new(2));
    let router = build_router(state, 1_000_000);

    let peer: std::net::SocketAddr = "10.0.0.1:12345".parse().unwrap();
    let make_request = || {
        let mut req = Request::builder()
            .method("GET")
            .uri("/status")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(peer));
        req
    };

    let r1 = router.clone().oneshot(make_request()).await.unwrap();
    let r2 = router.clone().oneshot(make_request()).await.unwrap();
    let r3 = router.oneshot(make_request()).await.unwrap();
    assert_eq!(r1.status(), StatusCode::OK);
    assert_eq!(r2.status(), StatusCode::OK);
    assert_eq!(r3.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn admin_rate_limit_middleware_rejects_after_the_limit_with_a_real_connect_info() {
    // Same shape as the public-route test above, but against the admin API's own
    // separate limiter - with no `Authorization` header at all, so this exercises
    // `admin_rate_limit_middleware`'s IP-fallback path (see its own doc comment).
    let mut state = AppState::for_tests();
    state.admin_rate_limiter = Arc::new(RateLimiter::new(2));
    let router = build_router(state, 1_000_000);

    let peer: std::net::SocketAddr = "10.0.0.2:12345".parse().unwrap();
    let make_request = || {
        let mut req = Request::builder()
            .method("GET")
            .uri("/api/v1/admin/tenant")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(peer));
        req
    };

    let r1 = router.clone().oneshot(make_request()).await.unwrap();
    let r2 = router.clone().oneshot(make_request()).await.unwrap();
    let r3 = router.oneshot(make_request()).await.unwrap();

    // All three get 401 (no auth header) or 429 - what matters is the third is
    // specifically rate-limited, not merely unauthorized like the first two.
    assert_eq!(r1.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(r2.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(r3.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn admin_rate_limit_middleware_keys_on_the_presented_token_not_the_source_ip() {
    // The real bug this whole change fixes: two different tenants' traffic,
    // proxied through the *same* source IP (exactly what happens when a hosted
    // control plane calls this API on every real user's behalf), must not share
    // one budget - each `sk_...` gets its own.
    let mut state = AppState::for_tests();
    state.admin_rate_limiter = Arc::new(RateLimiter::new(1));
    let router = build_router(state, 1_000_000);

    let peer: std::net::SocketAddr = "10.0.0.3:12345".parse().unwrap();
    let make_request = |token: &str| {
        let mut req = Request::builder()
            .method("GET")
            .uri("/api/v1/admin/tenant")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(peer));
        req
    };

    // Same IP, two different (unknown, so 401 not 200) tokens - both get through
    // the rate limiter itself, each consuming its own budget of 1.
    let r1 = router
        .clone()
        .oneshot(make_request("sk_tenant_one"))
        .await
        .unwrap();
    let r2 = router
        .clone()
        .oneshot(make_request("sk_tenant_two"))
        .await
        .unwrap();
    assert_eq!(
        r1.status(),
        StatusCode::UNAUTHORIZED,
        "tenant one's first request must not be rate-limited"
    );
    assert_eq!(
        r2.status(),
        StatusCode::UNAUTHORIZED,
        "tenant two's own budget must be independent of tenant one's"
    );

    // Tenant one's *second* request, same IP, is over its own budget of 1.
    let r3 = router.oneshot(make_request("sk_tenant_one")).await.unwrap();
    assert_eq!(r3.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn oversized_request_body_is_rejected_before_reaching_the_handler() {
    let router = build_router(AppState::for_tests(), 16); // absurdly small cap for the test

    let oversized_body =
        serde_json::json!({ "xmr_amount_piconero": 67_000_000_000u64 }).to_string();
    assert!(oversized_body.len() > 16);

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/admin/tenants")
        .header("content-type", "application/json")
        .body(Body::from(oversized_body))
        .unwrap();
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn a_tenants_reported_primary_address_is_a_standard_address_a_payment_could_reach() {
    // `primary_address` is derived at subaddress index 0/0 and handed straight back
    // to the merchant. If it were encoded as a *subaddress* (the underlying
    // primitive's default at 0/0), a sender would derive the shared secret from
    // `8*r*C` against the root pair's `C = v*G` while this wallet looks for
    // `8*v*r*S` - so anything paid to that string would be undetectable by the very
    // wallet it names. Mainnet standard addresses start with '4', subaddresses with
    // '8'.
    let router = test_router();
    let tenant = create_tenant(&router, 30).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/admin/tenant")
        .header("authorization", format!("Bearer {}", tenant.secret_token))
        .body(Body::empty())
        .unwrap();
    let body = body_json(router.oneshot(req).await.unwrap()).await;
    let address = body["primary_address"].as_str().unwrap();
    assert!(
        address.starts_with('4'),
        "primary_address {address} is not a standard mainnet address"
    );
    let parsed = monero::Address::from_str(address).unwrap();
    assert_eq!(parsed.addr_type, monero::AddressType::Standard);
    assert_eq!(parsed.network, Network::Mainnet);
}

#[tokio::test]
async fn tenant_settings_with_a_silent_failure_mode_are_rejected_on_creation_and_on_patch() {
    // Both write the same two columns, so a bound enforced on only one of them is
    // no bound at all. `confirmations_required` over 720 is indistinguishable from
    // "never settles" (`0` is fine now - native 0-conf, see `status::derive_status`'s
    // own doc comment); a non-positive `order_expiry_seconds` expires every order at
    // the moment it is created; and an `order_expiry_seconds` near `i64::MAX`
    // overflows the `created_at + expiry` addition, wrapping the deadline into the
    // past.
    let router = test_router();

    for bad in [
        serde_json::json!({ "confirmations_required": 100_000 }),
        serde_json::json!({ "order_expiry_seconds": 0 }),
        serde_json::json!({ "order_expiry_seconds": -60 }),
        serde_json::json!({ "order_expiry_seconds": i64::MAX }),
    ] {
        let mut create_body = serde_json::json!({
            "view_key_hex": valid_view_key_hex(40),
            "spend_pubkey_hex": valid_spend_pubkey_hex(41),
        });
        for (k, v) in bad.as_object().unwrap() {
            create_body[k] = v.clone();
        }
        let response = router
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/admin/tenants",
                None,
                &create_body,
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "creation accepted {bad}"
        );
    }

    let tenant = create_tenant(&router, 42).await;
    for bad in [
        serde_json::json!({ "confirmations_required": 100_000 }),
        serde_json::json!({ "order_expiry_seconds": 0 }),
        serde_json::json!({ "order_expiry_seconds": -1 }),
        serde_json::json!({ "order_expiry_seconds": i64::MAX }),
    ] {
        let response = router
            .clone()
            .oneshot(json_request(
                "PATCH",
                "/api/v1/admin/tenant",
                Some(&tenant.secret_token),
                &bad.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "patch accepted {bad}"
        );
    }

    // Sane values on both paths still go through, so the checks aren't just
    // rejecting everything.
    let response = router
        .clone()
        .oneshot(json_request(
            "PATCH",
            "/api/v1/admin/tenant",
            Some(&tenant.secret_token),
            &serde_json::json!({ "confirmations_required": 3, "order_expiry_seconds": 900 }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["confirmations_required"], 3);
    assert_eq!(body["order_expiry_seconds"], 900);

    // `0` is a real, deliberate, accepted value on both paths - native 0-conf as
    // the tenant's own default, not just as a per-order override.
    let response = router
        .clone()
        .oneshot(json_request(
            "PATCH",
            "/api/v1/admin/tenant",
            Some(&tenant.secret_token),
            &serde_json::json!({ "confirmations_required": 0 }),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "confirmations_required=0 must be accepted on patch"
    );
    assert_eq!(body_json(response).await["confirmations_required"], 0);

    let create_body = serde_json::json!({
        "view_key_hex": valid_view_key_hex(43),
        "spend_pubkey_hex": valid_spend_pubkey_hex(44),
        "confirmations_required": 0,
    });
    let response = router
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/admin/tenants",
            None,
            &create_body,
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "confirmations_required=0 must be accepted on creation"
    );
}

#[tokio::test]
async fn the_admin_refund_address_route_is_scoped_to_the_tenant_behind_the_secret_key() {
    // Monokulo's checkout records a customer's refund address through this
    // route with the store's `sk_`; the engine needs no public route for it.
    let router = test_router();
    let a = create_tenant(&router, 60).await;
    let b = create_tenant(&router, 62).await;
    let order = body_json(
        router
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/admin/tenant/orders",
                Some(&a.secret_token),
                &serde_json::json!({ "xmr_amount_piconero": 1_000_000u64 }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let order_id = order["order_id"].as_str().unwrap().to_owned();
    let uri = format!("/api/v1/admin/tenant/orders/{order_id}/refund-address");
    let body = serde_json::json!({ "refund_address": "refund-here" });

    let no_key = router
        .clone()
        .oneshot(json_request("POST", &uri, None, &body.clone()))
        .await
        .unwrap();
    assert_eq!(no_key.status(), StatusCode::UNAUTHORIZED);

    let other_tenant = router
        .clone()
        .oneshot(json_request(
            "POST",
            &uri,
            Some(&b.secret_token),
            &body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(
        other_tenant.status(),
        StatusCode::NOT_FOUND,
        "tenant B must not reach tenant A's order"
    );

    let owner = router
        .clone()
        .oneshot(json_request("POST", &uri, Some(&a.secret_token), &body))
        .await
        .unwrap();
    assert_eq!(owner.status(), StatusCode::OK);

    let detail = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/admin/tenant/orders/{order_id}"))
                .header("authorization", format!("Bearer {}", a.secret_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(detail).await["refund_address"], "refund-here");
}

#[tokio::test]
async fn every_admin_route_resolves_its_tenant_from_the_bearer_token_alone() {
    // The structural IDOR fix (§DESIGN.md §10.1) is only a guarantee if it holds for
    // *every* route in the family, not the handful it was designed around. This
    // walks all of them with tenant B's token and asserts none of them can be
    // steered at tenant A's data by any identifier in the path or the body.
    let router = test_router();
    let a = create_tenant(&router, 50).await;
    let b = create_tenant(&router, 52).await;

    // Give A an order and a webhook to try to reach.
    let order = body_json(
        router
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/admin/tenant/orders",
                Some(&a.secret_token),
                &serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64 }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let a_order_id = order["order_id"].as_str().unwrap().to_owned();
    let a_webhook = body_json(
        router
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/admin/tenant/webhooks",
                Some(&a.secret_token),
                &serde_json::json!({ "url": "https://a.example/hook" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let a_webhook_id = a_webhook["webhook_id"].as_str().unwrap().to_owned();

    // B's token against A's identifiers: every one must miss.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/admin/tenant/orders/{a_order_id}"))
                .header("authorization", format!("Bearer {}", b.secret_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/admin/tenant/webhooks/{a_webhook_id}"))
                .header("authorization", format!("Bearer {}", b.secret_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // B's listings show only B's own (empty) data, never A's.
    let orders = body_json(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/admin/tenant/orders")
                    .header("authorization", format!("Bearer {}", b.secret_token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(orders.as_array().unwrap().len(), 0);

    let webhooks = body_json(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/admin/tenant/webhooks")
                    .header("authorization", format!("Bearer {}", b.secret_token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(webhooks.as_array().unwrap().len(), 0);

    // A's own view is untouched by any of the above.
    let webhooks = body_json(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/admin/tenant/webhooks")
                    .header("authorization", format!("Bearer {}", a.secret_token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(webhooks.as_array().unwrap().len(), 1);

    // And rotating B's secret can't be aimed at A either - A's token keeps working.
    let response = router
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/admin/tenant/rotate-secret",
            Some(&b.secret_token),
            &serde_json::json!({ "tenant_id": "whatever" }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/admin/tenant")
                .header("authorization", format!("Bearer {}", a.secret_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn every_path_that_can_set_a_webhook_url_validates_the_scheme() {
    // Webhook creation is currently the only route that writes a URL, and this is
    // what proves it - if a second one is ever added without its own check, the
    // sweep below over every route in the router stops matching reality.
    let router = test_router();
    let tenant = create_tenant(&router, 60).await;

    for bad_url in [
        "file:///etc/passwd",
        "gopher://example.com/",
        "ftp://example.com/hook",
        "javascript:alert(1)",
        "not a url at all",
        "",
    ] {
        let response = router
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/admin/tenant/webhooks",
                Some(&tenant.secret_token),
                &serde_json::json!({ "url": bad_url }),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "accepted {bad_url:?}"
        );
    }

    // The tenant-patch route must not offer a back door for any webhook field.
    let response = router
        .clone()
        .oneshot(json_request(
            "PATCH",
            "/api/v1/admin/tenant",
            Some(&tenant.secret_token),
            &serde_json::json!({ "webhook_url": "file:///etc/passwd", "url": "file:///etc/passwd" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "unknown fields are ignored, not applied"
    );
    let webhooks = body_json(
        router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/admin/tenant/webhooks")
                    .header("authorization", format!("Bearer {}", tenant.secret_token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        webhooks.as_array().unwrap().len(),
        0,
        "no webhook may have been created by a patch"
    );
}

/// /status reports the engine's CPU and memory, and each network's scaling
/// figures (`docs/engine_scaling.md` section 6). One block that has taken over
/// two minutes, while its node answers, is reported as slow, with what is
/// known about it.
#[tokio::test]
async fn status_reports_resources_scaling_and_a_slow_block() {
    let state = AppState::for_tests();
    let progress =
        crate::scanner_status::progress_of(&state.networks.scanner_status, Network::Mainnet);
    let router = build_router(state, 1_000_000);

    let status = get_status_json(router.clone()).await;
    assert!(status["resources"]["cpu_count"].as_u64().unwrap() >= 1);
    assert!(!status["resources"]["host_id"].as_str().unwrap().is_empty());
    let scaling = &status["networks"][0]["scaling"];
    assert_eq!(scaling["slow"], serde_json::Value::Null);
    assert_eq!(scaling["budget_mb"], 8);
    assert!(scaling["pace"].is_string(), "{scaling}");
    assert!(
        status["networks"][0]["scanner"]["ever_ticked"] == false,
        "progress alone isn't a tick"
    );

    // A minute in: not slow yet.
    progress.lock().start_block(42, crate::now_unix() - 60);
    let status = get_status_json(router.clone()).await;
    assert_eq!(
        status["networks"][0]["scaling"]["slow"],
        serde_json::Value::Null
    );

    // Past two minutes on the same block: slow.
    progress.lock().in_progress = None;
    progress.lock().start_block(42, crate::now_unix() - 130);
    progress.lock().fetched_block(42, 400_000_000);
    let status = get_status_json(router).await;
    let slow = &status["networks"][0]["scaling"]["slow"];
    assert_eq!(slow["height"], 42, "{status}");
    assert_eq!(slow["wire_bytes"], 400_000_000);
    assert!(slow["elapsed_secs"].as_i64().unwrap() >= 130);
    assert_eq!(slow["node"], "fake-node:18081");
}

async fn get_status_json(router: Router) -> serde_json::Value {
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

#[tokio::test]
async fn status_endpoint_is_reachable_with_no_authentication_at_all() {
    // Deliberately no `authorization` header, no session cookie - see
    // `build_router`'s own doc comment on why this route is unauthenticated.
    let router = build_router(AppState::for_tests(), 1_000_000);
    let body = get_status_json(router).await;
    assert!(body["networks"].is_array());
    assert!(
        body["loop_restarts"].is_array(),
        "restart counts are reported (task 7.9), got: {body}"
    );
    assert_eq!(body["webhook_backlog"]["due"], 0, "got: {body}");
}

#[tokio::test]
async fn status_endpoint_shows_the_real_configured_network_and_node_with_its_live_height() {
    let router = build_router(AppState::for_tests(), 1_000_000);
    let body = get_status_json(router).await;
    let network = &body["networks"][0];
    assert_eq!(
        network["network"], "mainnet",
        "expected the configured network shown, got: {body}"
    );
    let node = &network["nodes"][0];
    assert_eq!(
        node["label"], "fake-node:18081",
        "expected the real node label shown, got: {body}"
    );
    // FakeDaemonClient::new() starts at height 0 - a real, live query result,
    // not a placeholder, and must not be confused with "unknown" (null).
    assert_eq!(
        node["height"], 0,
        "expected the node's real live height shown, got: {body}"
    );
    assert!(
        node["error"].is_null(),
        "a reachable node must have no error, got: {body}"
    );
    assert_eq!(node["in_cooldown"], false, "got: {body}");
    assert!(
        !network["scanner"]["ever_ticked"].as_bool().unwrap(),
        "no scan tick has happened in this test, got: {body}"
    );
    assert_eq!(network["lagging_tenants"], 0, "got: {body}");
    assert_eq!(network["max_blocks_behind"], 0, "got: {body}");
}

#[tokio::test]
async fn status_endpoint_shows_an_offline_node_as_an_error_not_a_silent_gap() {
    let mut state = AppState::for_tests();
    let offline_daemon = Arc::new(FallbackDaemonClient::new(vec![FallbackNode {
        label: "dead-node:18081".to_owned(),
        client: Arc::new(FakeDaemonClient::default()), // starts offline (see FakeDaemonClient::new vs. Default)
    }]));
    state.networks.daemons =
        crate::engine_settings::Daemons::fixed(HashMap::from([(Network::Mainnet, offline_daemon)]));
    let router = build_router(state, 1_000_000);
    let body = get_status_json(router).await;
    let node = &body["networks"][0]["nodes"][0];
    assert_eq!(node["label"], "dead-node:18081");
    assert!(
        node["height"].is_null(),
        "expected no height for an offline node, got: {body}"
    );
    let error = node["error"]
        .as_str()
        .expect("expected a real error message");
    assert!(
        error.contains("fake daemon is offline"),
        "expected the real error message surfaced, got: {error}"
    );
}

#[tokio::test]
async fn status_endpoint_reflects_a_healthy_recent_scan_tick() {
    let state = AppState::for_tests();
    crate::scanner_status::record_tick(
        &state.networks.scanner_status,
        Network::Mainnet,
        crate::now_unix(),
        crate::now_unix(),
        3,
        &Ok::<(), String>(()),
    );
    let router = build_router(state, 1_000_000);
    let body = get_status_json(router).await;
    let scanner = &body["networks"][0]["scanner"];
    assert!(scanner["ever_ticked"].as_bool().unwrap());
    assert!(
        scanner["last_tick_ok"].as_bool().unwrap(),
        "expected a healthy fresh successful tick, got: {body}"
    );
    assert!(!scanner["is_stale"].as_bool().unwrap());
    assert_eq!(
        scanner["tenants_scanned"], 3,
        "expected the real tenants-scanned count shown, got: {body}"
    );
}

#[tokio::test]
async fn status_endpoint_reflects_a_failing_scan_tick_with_its_real_error() {
    let state = AppState::for_tests();
    crate::scanner_status::record_tick(
        &state.networks.scanner_status,
        Network::Mainnet,
        crate::now_unix(),
        crate::now_unix(),
        1,
        &Err::<(), String>("node returned garbage".to_owned()),
    );
    let router = build_router(state, 1_000_000);
    let body = get_status_json(router).await;
    let scanner = &body["networks"][0]["scanner"];
    assert!(
        !scanner["last_tick_ok"].as_bool().unwrap(),
        "expected the real failing-tick state, got: {body}"
    );
    assert_eq!(
        scanner["last_error"], "node returned garbage",
        "expected the real error message surfaced, got: {body}"
    );
}

#[tokio::test]
async fn status_endpoint_reflects_a_stale_scanner_that_has_stopped_ticking() {
    let state = AppState::for_tests();
    // A tick that "succeeded" a very long time ago - the scanner itself is
    // the thing that's actually broken here (stopped ticking at all), which
    // must read differently from a merely-failing-but-alive tick.
    crate::scanner_status::record_tick(
        &state.networks.scanner_status,
        Network::Mainnet,
        1,
        1,
        2,
        &Ok::<(), String>(()),
    );
    let router = build_router(state, 1_000_000);
    let body = get_status_json(router).await;
    let scanner = &body["networks"][0]["scanner"];
    assert!(
        scanner["is_stale"].as_bool().unwrap(),
        "expected stale once a tick is far older than the poll interval, got: {body}"
    );
}

#[tokio::test]
async fn status_endpoint_with_no_configured_networks_says_so_plainly() {
    let mut state = AppState::for_tests();
    state.networks.daemons = crate::engine_settings::Daemons::fixed(HashMap::new());
    let router = build_router(state, 1_000_000);
    let body = get_status_json(router).await;
    assert_eq!(body["networks"].as_array().unwrap().len(), 0);
}

/// Like `AppState::for_tests`, but hands back the mainnet `FakeDaemonClient` directly so
/// a test can script real, findable block heights/transactions - `for_tests`'
/// own daemon starts with no blocks at all, which is fine for most tests here but
/// not for these.
async fn test_app_state_with_real_daemon() -> (AppState, Arc<FakeDaemonClient>) {
    test_app_state_with_real_daemon_and_env(live_settings::Env::fixed(
        Vec::<(String, String)>::new(),
    ))
    .await
}

/// With a real settings registry over the test's store (so the settings
/// API works), and `env` as the environment it sees.
async fn test_app_state_with_real_daemon_and_env(
    env: live_settings::Env,
) -> (AppState, Arc<FakeDaemonClient>) {
    let store = Store::open_in_memory().unwrap().into_shared();
    let fake_daemon = Arc::new(FakeDaemonClient::new());
    let mainnet_daemon = Arc::new(FallbackDaemonClient::new(vec![FallbackNode {
        label: "fake-node:18081".to_owned(),
        client: Arc::<FakeDaemonClient>::clone(&fake_daemon),
    }]));
    let state = AppState {
        settings: crate::engine_settings::EngineSettings::load_with(
            Arc::clone(&store),
            None,
            Arc::new(RateLimiter::new(10_000)),
            env,
        )
        .await
        .unwrap(),
        networks: crate::http::Networks {
            daemons: crate::engine_settings::Daemons::fixed(HashMap::from([(
                Network::Mainnet,
                mainnet_daemon,
            )])),
            scanner_status: crate::scanner_status::new_scanner_status_map(),
        },
        ..AppState::for_tests_with_store(store)
    };
    (state, fake_daemon)
}

/// The settings API over an options file on disk: GET says where it is,
/// a save writes it, a hand edit is applied by a reload, and a bad edit is
/// refused by line, changing nothing.
#[tokio::test]
async fn the_options_file_is_saved_to_and_reloaded_through_the_api() {
    let dir = std::env::temp_dir().join(format!("engine-options-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("engine.toml");
    std::fs::write(&path, "# Mine.\n[payment]\nconfirmations_required = 4\n").unwrap();
    let store = Store::open_in_memory().unwrap().into_shared();
    let settings = crate::engine_settings::EngineSettings::load_full(
        Arc::clone(&store),
        None,
        None,
        Arc::new(RateLimiter::new(10_000)),
        live_settings::Env::fixed([("ENGINE_TOKEN", TEST_ENGINE_TOKEN)]),
        live_settings::OptionsFile::at(&path),
        false,
    )
    .await
    .unwrap();
    let router = build_router(
        AppState {
            settings,
            ..AppState::for_tests_with_store(store)
        },
        1_000_000,
    );
    let get = async |router: Router| {
        body_json(router.oneshot(settings_request("GET", None)).await.unwrap()).await
    };
    let body = get(router.clone()).await;
    assert_eq!(body["options_file"]["path"], path.display().to_string());
    assert_eq!(body["options_file"]["writable"], true);
    assert_eq!(
        body["scalars"]["payment.confirmations_required"]["value"],
        "4"
    );
    assert_eq!(
        body["scalars"]["payment.confirmations_required"]["source"],
        "toml"
    );

    let saved = router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some(serde_json::json!({ "scalars": { "payment.order_expiry_minutes": "45" } })),
        ))
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# Mine.\n[payment]\nconfirmations_required = 4\norder_expiry_minutes = 45\n"
    );

    let reload = || {
        router.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/admin/settings/reload")
                .header(shared::auth::ENGINE_TOKEN_HEADER, TEST_ENGINE_TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
    };
    std::fs::write(&path, "[payment]\nconfirmations_required = 7\n").unwrap();
    let reloaded = reload().await.unwrap();
    assert_eq!(reloaded.status(), StatusCode::OK);
    assert_eq!(
        body_json(reloaded).await["changed"],
        serde_json::json!([
            "payment.confirmations_required",
            "payment.order_expiry_minutes"
        ])
    );
    assert_eq!(
        get(router.clone()).await["scalars"]["payment.confirmations_required"]["value"],
        "7"
    );

    std::fs::write(&path, "[payment]\nconfirmations_required = 7000\n").unwrap();
    let refused = reload().await.unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let body = body_json(refused).await.to_string();
    assert!(
        body.contains("line 2: payment.confirmations_required"),
        "{body}"
    );
    assert_eq!(
        get(router.clone()).await["scalars"]["payment.confirmations_required"]["value"],
        "7"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// An options file the engine can't write: GET says so and locks what it
/// holds, with why; a save sent anyway is refused (409) and the file stays
/// as it was. A runtime switch, in the database, still saves.
#[cfg(unix)]
#[tokio::test]
async fn a_read_only_options_file_is_locked_and_a_save_to_it_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!("engine-read-only-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("engine.toml");
    let text = "[payment]\nconfirmations_required = 4\n";
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    if std::fs::OpenOptions::new().append(true).open(&path).is_ok() {
        return; // Root: permission bits don't bind it.
    }
    let store = Store::open_in_memory().unwrap().into_shared();
    let settings = crate::engine_settings::EngineSettings::load_full(
        Arc::clone(&store),
        None,
        None,
        Arc::new(RateLimiter::new(10_000)),
        live_settings::Env::fixed([("ENGINE_TOKEN", TEST_ENGINE_TOKEN)]),
        live_settings::OptionsFile::at(&path),
        false,
    )
    .await
    .unwrap();
    let router = build_router(
        AppState {
            settings,
            ..AppState::for_tests_with_store(store)
        },
        1_000_000,
    );
    let body = body_json(
        router
            .clone()
            .oneshot(settings_request("GET", None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["options_file"]["writable"], false);
    let locked = body["scalars"]["payment.confirmations_required"]["locked"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(locked.contains("can't be written by the engine"), "{body}");
    assert!(
        body["scalars"]["logging.dev_mode_until"]["locked"].is_null(),
        "a runtime switch stays editable: {body}"
    );

    let refused = router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some(serde_json::json!({ "scalars": { "payment.confirmations_required": "6" } })),
        ))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let message = body_json(refused).await.to_string();
    assert!(
        message.contains("can't be written by this process"),
        "{message}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

    let saved = router
        .oneshot(settings_request(
            "POST",
            Some(serde_json::json!({ "scalars": { "logging.dev_mode_until": "0" } })),
        ))
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

// -- The engine token ---------------------------------------------------

/// Every route, whatever credential of its own it takes, refuses a request
/// without the engine token, or with any other value in its header
/// (a store's own `sk_` included); with it, a route answers as usual.
#[tokio::test]
async fn every_route_refuses_a_request_without_the_engine_token() {
    let state = AppState::for_tests();
    let tenant = create_tenant(&build_router(state.clone(), 1_000_000), 1).await;
    let router = super::build_router(state, 1_000_000);
    let endpoints = [
        ("POST", "/api/v1/admin/tenants"),
        ("GET", "/status"),
        ("GET", "/api/v1/admin/settings"),
        ("POST", "/api/v1/admin/settings"),
        ("GET", "/api/v1/admin/logs"),
        ("GET", "/api/v1/admin/engine/activity?network=mainnet"),
        ("GET", "/api/v1/admin/tenant"),
        ("GET", "/api/v1/admin/tenant/orders"),
        ("GET", "/api/v1/admin/tenant/events"),
        ("GET", "/api/v1/admin/order-events"),
        ("GET", "/no/such/route"),
    ];
    let wrong_values = [None, Some("wrong"), Some(tenant.secret_token.as_str())];
    for (method, uri) in endpoints {
        for wrong in wrong_values {
            let mut request = Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", format!("Bearer {}", tenant.secret_token))
                .header("content-type", "application/json");
            if let Some(value) = wrong {
                request = request.header(shared::auth::ENGINE_TOKEN_HEADER, value);
            }
            let response = router
                .clone()
                .oneshot(request.body(Body::from("{}")).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri} with {wrong:?}"
            );
        }
    }

    let status = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/status")
                .header(shared::auth::ENGINE_TOKEN_HEADER, TEST_ENGINE_TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    let own = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/tenant")
                .header(shared::auth::ENGINE_TOKEN_HEADER, TEST_ENGINE_TOKEN)
                .header("authorization", format!("Bearer {}", tenant.secret_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(own.status(), StatusCode::OK);
}

/// The engine token doesn't stand in for a store's own `sk_`: a store's
/// routes still need it.
#[tokio::test]
async fn a_stores_routes_still_need_its_own_secret() {
    let router = test_router();
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/tenant")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// -- Instance admin settings API -------------------------------------------

fn settings_request(method: &str, body: Option<serde_json::Value>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri("/api/v1/admin/settings")
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap()
}

#[tokio::test]
async fn get_settings_reports_code_defaults_when_nothing_is_configured() {
    let (state, _daemon) = test_app_state_with_real_daemon().await;
    let router = build_router(state, 1_000_000);
    let response = router.oneshot(settings_request("GET", None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(
        body["scalars"]["payment.confirmations_required"]["value"],
        "10"
    );
    assert_eq!(
        body["scalars"]["payment.confirmations_required"]["source"],
        "default"
    );
    assert_eq!(
        body["monero_node"]["mainnet"],
        serde_json::Value::Null,
        "an unconfigured network reports null, not a fabricated node"
    );
}

#[tokio::test]
async fn updating_a_scalar_setting_persists_and_a_later_get_reflects_it() {
    let (state, _daemon) = test_app_state_with_real_daemon().await;
    let router = build_router(state, 1_000_000);

    let post = router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some(serde_json::json!({ "scalars": { "payment.confirmations_required": "3" } })),
        ))
        .await
        .unwrap();
    assert_eq!(
        post.status(),
        StatusCode::OK,
        "expected the save to succeed, got: {:?}",
        body_json(post).await
    );

    let get = router.oneshot(settings_request("GET", None)).await.unwrap();
    let body = body_json(get).await;
    assert_eq!(
        body["scalars"]["payment.confirmations_required"]["value"],
        "3"
    );
    assert_eq!(
        body["scalars"]["payment.confirmations_required"]["source"],
        "toml"
    );
}

/// A setting given on the command line wins and is locked: the API says so,
/// and refuses a save of it.
#[tokio::test]
async fn a_setting_given_on_the_command_line_is_locked_and_a_save_of_it_refused() {
    let env = live_settings::Env::fixed(Vec::<(String, String)>::new())
        .with_cli([("payment.confirmations_required".to_owned(), "99".to_owned())].into());
    let (state, _daemon) = test_app_state_with_real_daemon_and_env(env).await;
    let router = build_router(state, 1_000_000);

    let post = router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some(serde_json::json!({ "scalars": { "payment.confirmations_required": "3" } })),
        ))
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::BAD_REQUEST);
    let refused = body_json(post).await.to_string();
    assert!(
        refused.contains("--payment-confirmations-required"),
        "{refused}"
    );

    let get = router.oneshot(settings_request("GET", None)).await.unwrap();
    let body = body_json(get).await;
    let setting = &body["scalars"]["payment.confirmations_required"];
    assert_eq!(setting["value"], "99");
    assert_eq!(setting["source"], "cli");
    assert!(
        setting["locked"]
            .as_str()
            .is_some_and(|why| why.contains("--payment-confirmations-required")),
        "{setting}"
    );
}

#[tokio::test]
async fn saving_an_out_of_range_scalar_is_rejected_and_nothing_changes() {
    let (state, _daemon) = test_app_state_with_real_daemon().await;
    let router = build_router(state, 1_000_000);

    // `0` is a legal value now (native 0-conf) - `1000` (over the 720 cap) is the
    // out-of-range example instead.
    let post = router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some(serde_json::json!({ "scalars": { "payment.confirmations_required": "1000" } })),
        ))
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::BAD_REQUEST);

    let get = router.oneshot(settings_request("GET", None)).await.unwrap();
    let body = body_json(get).await;
    assert_eq!(
        body["scalars"]["payment.confirmations_required"]["value"], "10",
        "the rejected save must not have taken effect"
    );
}

/// The collector's headers are a secret: they come from the environment,
/// are shown masked, and can't be saved through the API.
#[tokio::test]
async fn otlp_headers_come_from_the_environment_and_are_never_saved_or_shown() {
    let env = live_settings::Env::fixed([(
        "ENGINE_LOGGING_OTLP_HEADERS",
        "authorization=Bearer sk-live-abc123",
    )]);
    let (state, _daemon) = test_app_state_with_real_daemon_and_env(env).await;
    let router = build_router(state, 1_000_000);

    let get = router
        .clone()
        .oneshot(settings_request("GET", None))
        .await
        .unwrap();
    let body = body_json(get).await;
    assert_eq!(body["scalars"]["logging.otlp_headers"]["source"], "env");
    assert!(!body.to_string().contains("sk-live-abc123"), "{body}");

    let refused = router
        .oneshot(settings_request(
            "POST",
            Some(serde_json::json!({ "scalars": { "logging.otlp_headers": "x-team=ops,sk-live-other" } })),
        ))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = body_json(refused).await.to_string();
    assert!(body.contains("ENGINE_LOGGING_OTLP_HEADERS"), "{body}");
    assert!(!body.contains("sk-live"), "{body}");
}

#[tokio::test]
async fn a_partially_invalid_save_changes_nothing_not_just_the_valid_half() {
    let (state, _daemon) = test_app_state_with_real_daemon().await;
    let router = build_router(state, 1_000_000);

    let post = router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some(serde_json::json!({
                "scalars": {
                    "payment.reorg_check_depth": "50",
                    "payment.confirmations_required": "1000"
                }
            })),
        ))
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::BAD_REQUEST);

    let get = router.oneshot(settings_request("GET", None)).await.unwrap();
    let body = body_json(get).await;
    assert_eq!(
        body["scalars"]["payment.reorg_check_depth"]["value"], "20",
        "the valid field in the same request must not have been saved either"
    );
}

#[cfg(feature = "snp")]
#[tokio::test]
async fn enabling_the_snp_key_custody_backend_is_saved_and_reported() {
    let (state, _daemon) = test_app_state_with_real_daemon().await;
    let router = build_router(state, 1_000_000);

    let post = router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some(serde_json::json!({
                "scalars": {
                    "key_custody.enabled_backends": "plain,snp",
                    "key_custody.default_backend": "snp"
                }
            })),
        ))
        .await
        .unwrap();
    assert_eq!(
        post.status(),
        StatusCode::OK,
        "expected success, got: {:?}",
        body_json(post).await
    );

    let get = router.oneshot(settings_request("GET", None)).await.unwrap();
    let body = body_json(get).await;
    assert_eq!(
        body["scalars"]["key_custody.default_backend"]["value"],
        "snp"
    );
    assert_eq!(
        body["scalars"]["key_custody.enabled_backends"]["value"],
        "plain, snp"
    );
}

/// Which engine images are trusted with keys is set where the engine is
/// deployed (options file, environment), never through the settings API.
#[tokio::test]
async fn the_snp_trust_settings_cannot_be_changed_through_the_settings_api() {
    let (state, _daemon) = test_app_state_with_real_daemon().await;
    let router = build_router(state, 1_000_000);

    for (key, value) in [
        ("key_custody.snp_trusted_id_key", "ab".repeat(48)),
        ("key_custody.snp_min_guest_svn", "0".to_owned()),
    ] {
        let post = router
            .clone()
            .oneshot(settings_request(
                "POST",
                Some(serde_json::json!({ "scalars": { key: value } })),
            ))
            .await
            .unwrap();
        assert_eq!(post.status(), StatusCode::BAD_REQUEST, "{key}");
    }
}

/// Ports on 127.0.0.1 that hang up on every connection
/// ([`shared::unreachable`]), so a node saved there fails at once, with no
/// DNS. Not closed ports: Windows takes two seconds to refuse each try.
fn unreachable_ports<const N: usize>() -> [u16; N] {
    std::array::from_fn(|_| shared::unreachable::address().port())
}

fn unreachable_port() -> u16 {
    let [port] = unreachable_ports();
    port
}

#[tokio::test]
async fn setting_a_monero_node_round_trips_including_its_fallback_list() {
    let (state, _daemon) = test_app_state_with_real_daemon().await;
    let router = build_router(state, 1_000_000);

    let [primary, backup] = unreachable_ports();
    let node = serde_json::json!({
        "host": "127.0.0.1",
        "port": primary,
        "ssl": false,
        "accept_self_signed_certs": true,
        "fallbacks": [
            { "host": "127.0.0.1", "port": backup, "ssl": true, "accept_self_signed_certs": false, "fallbacks": [] }
        ]
    });
    let post = router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some(serde_json::json!({ "monero_node": { "mainnet": node.clone() } })),
        ))
        .await
        .unwrap();
    assert_eq!(
        post.status(),
        StatusCode::OK,
        "expected success, got: {:?}",
        body_json(post).await
    );

    let get = router.oneshot(settings_request("GET", None)).await.unwrap();
    let body = body_json(get).await;
    assert_eq!(body["monero_node"]["mainnet"], node);
}

#[tokio::test]
async fn clearing_a_monero_node_with_a_null_value_removes_its_configuration() {
    let (state, _daemon) = test_app_state_with_real_daemon().await;
    let router = build_router(state, 1_000_000);

    let node = serde_json::json!({ "host": "127.0.0.1", "port": unreachable_port(), "ssl": false, "accept_self_signed_certs": true, "fallbacks": [] });
    router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some(serde_json::json!({ "monero_node": { "mainnet": node } })),
        ))
        .await
        .unwrap();

    router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some(serde_json::json!({ "monero_node": { "mainnet": null } })),
        ))
        .await
        .unwrap();

    let get = router.oneshot(settings_request("GET", None)).await.unwrap();
    let body = body_json(get).await;
    assert_eq!(body["monero_node"]["mainnet"], serde_json::Value::Null);
}

// -- Payment lookup by txid (`docs/txid_lookup_and_scan_chunking_wbs.md` Part B) --

fn lookup_request(token: &str, txid: &str) -> Request<Body> {
    json_request(
        "POST",
        "/api/v1/admin/tenant/payments/lookup",
        Some(token),
        &serde_json::json!({ "txid": txid }),
    )
}

fn fixture_tx_for_lookup_tests() -> Transaction {
    let raw_tx = hex::decode(include_str!("../../fixtures/subaddress_tx.hex")).unwrap();
    deserialize(&raw_tx).unwrap()
}

/// The real view/spend key pair `subaddress_tx.hex` actually pays (subaddress
/// 0/1) - duplicated from `scanner.rs`'s own private `fixture_view_key`/
/// `fixture_spend_pubkey` test helpers, which aren't reachable from this
/// module (`engine::tests` is a private module). A `create_tenant`-issued
/// random per-seed key pair could never match this fixed fixture transaction,
/// and this crate's own convention keeps real crypto-matching correctness
/// tested at the scanner-level (`scanner.rs`'s own exhaustive suite) rather
/// than re-proven through the full HTTP stack - these two helpers exist only
/// so the one thing that's genuinely new here (this handler's own wiring of
/// already-proven primitives) gets one real, opt-in-if-you-want-it, true
/// end-to-end check too.
fn fixture_view_key_hex() -> String {
    hex::encode(
        PrivateKey::from_slice(
            &hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07")
                .unwrap(),
        )
        .unwrap()
        .to_bytes(),
    )
}

fn fixture_spend_pubkey_hex() -> String {
    let secret_spend = PrivateKey::from_slice(
        &hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907").unwrap(),
    )
    .unwrap();
    hex::encode(PublicKey::from_private_key(&secret_spend).to_bytes())
}

async fn create_fixture_tenant(router: &Router) -> TestTenant {
    let req = json_request(
        "POST",
        "/api/v1/admin/tenants",
        None,
        &serde_json::json!({
            "view_key_hex": fixture_view_key_hex(),
            "spend_pubkey_hex": fixture_spend_pubkey_hex(),
        }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    TestTenant {
        public_key: body["public_key"].as_str().unwrap().to_owned(),
        secret_token: body["secret_token"].as_str().unwrap().to_owned(),
    }
}

#[tokio::test]
async fn lookup_payment_rejects_a_malformed_txid() {
    let (state, _daemon) = test_app_state_with_real_daemon().await;
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;

    let response = router
        .clone()
        .oneshot(lookup_request(&tenant.secret_token, "not-a-real-txid"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn lookup_payment_requires_authentication() {
    let (state, _daemon) = test_app_state_with_real_daemon().await;
    let router = build_router(state, 1_000_000);
    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/payments/lookup",
        None,
        &serde_json::json!({ "txid": "0".repeat(64) }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn lookup_payment_reports_not_found_on_chain_for_an_unknown_txid() {
    let (state, _daemon) = test_app_state_with_real_daemon().await;
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;

    let bogus = "0".repeat(64);
    let response = router
        .clone()
        .oneshot(lookup_request(&tenant.secret_token, &bogus))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["outcome"], "not_found_on_chain");
}

#[tokio::test]
async fn lookup_payment_reports_no_matching_order_for_a_real_but_unrelated_tx() {
    use monero::cryptonote::hash::Hashable as _;
    let (state, daemon) = test_app_state_with_real_daemon().await;
    let router = build_router(state, 1_000_000);
    // Random, non-fixture keys - this tenant genuinely has no claim on the
    // fixture transaction's outputs.
    let tenant = create_tenant(&router, 1).await;

    let tx = fixture_tx_for_lookup_tests();
    daemon.set_mempool(vec![tx.clone()]);
    let txid = hex::encode(tx.hash().to_bytes());

    let response = router
        .clone()
        .oneshot(lookup_request(&tenant.secret_token, &txid))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["outcome"], "no_matching_order");
}

#[tokio::test]
async fn lookup_payment_matches_and_records_a_real_mempool_payment() {
    use monero::cryptonote::hash::Hashable as _;
    let (state, daemon) = test_app_state_with_real_daemon().await;
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let tenant = create_fixture_tenant(&router).await;

    // A real order against this tenant's own minor_index 1 (`subaddress_tx.hex`
    // pays subaddress 0/1, the same fixture `scanner.rs`'s own tests already
    // rely on) - the first order any fresh tenant creates gets minor_index 1
    // (`store::tests::minor_index_allocation_starts_at_one_and_increments`).
    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        &serde_json::json!({ "xmr_amount_piconero": 1u64 }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let order_id = body_json(response).await["order_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let tx = fixture_tx_for_lookup_tests();
    daemon.set_mempool(vec![tx.clone()]);
    let txid = hex::encode(tx.hash().to_bytes());

    let response = router
        .clone()
        .oneshot(lookup_request(&tenant.secret_token, &txid))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["outcome"], "matched");
    assert_eq!(
        body["order_ids"].as_array().unwrap(),
        &[serde_json::Value::String(order_id.clone())]
    );

    let payments = store
        .lock()
        .get_all_payments(&shared::ids::OrderId::new(order_id.clone()))
        .unwrap();
    assert_eq!(
        payments.len(),
        1,
        "the match must actually be recorded, not just reported"
    );

    // A second lookup of the same, already-applied txid must be a safe no-op
    // that still reports the same match - `record_scan_match`'s own existing
    // idempotency, exercised through this new endpoint specifically.
    let response = router
        .clone()
        .oneshot(lookup_request(&tenant.secret_token, &txid))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["outcome"], "matched");
    let payments = store
        .lock()
        .get_all_payments(&shared::ids::OrderId::new(order_id.clone()))
        .unwrap();
    assert_eq!(
        payments.len(),
        1,
        "looking the same txid up twice must not duplicate the recorded payment"
    );
}

/// Reads SSE frames from `body` until one full event has arrived, returning
/// its `(event, data)`.
async fn next_sse_event(body: &mut Body, buffer: &mut String) -> (String, String) {
    loop {
        if let Some(end) = buffer.find("\n\n") {
            let block: String = buffer.drain(..end + 2).collect();
            let mut event = String::new();
            let mut data = String::new();
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("event: ") {
                    event = value.to_owned();
                } else if let Some(value) = line.strip_prefix("data: ") {
                    data = value.to_owned();
                } else {
                    // Comments and other fields: not asserted on.
                }
            }
            if !event.is_empty() {
                return (event, data);
            }
            continue;
        }
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), body.frame())
            .await
            .expect("timed out waiting for an SSE event")
            .expect("stream ended")
            .unwrap();
        if let Ok(bytes) = frame.into_data() {
            buffer.push_str(std::str::from_utf8(&bytes).unwrap());
        }
    }
}

/// Creates an order the only way there is now: the tenant's `sk_` against
/// the admin API (what monokulo does for every order).
async fn create_admin_order(router: &Router, tenant: &TestTenant) -> String {
    let response = router
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/admin/tenant/orders",
            Some(&tenant.secret_token),
            &serde_json::json!({ "xmr_amount_piconero": 1_000_000_000_000u64 }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await["order_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn order_events_stream_reports_only_the_authenticated_tenants_changes() {
    let router = test_router();
    let tenant = create_tenant(&router, 11).await;
    let other = create_tenant(&router, 21).await;
    let order_id = create_admin_order(&router, &tenant).await;
    let other_order_id = create_admin_order(&router, &other).await;

    let unauthenticated = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/tenant/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/tenant/events")
                .header("authorization", format!("Bearer {}", tenant.secret_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut body = response.into_body();
    let mut buffer = String::new();
    assert_eq!(next_sse_event(&mut body, &mut buffer).await.0, "ready");

    for (owner, id) in [(&other, &other_order_id), (&tenant, &order_id)] {
        let response = router
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/api/v1/admin/tenant/orders/{id}/refund-address"),
                Some(&owner.secret_token),
                &serde_json::json!({ "refund_address": "refund" }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let (event, data) = next_sse_event(&mut body, &mut buffer).await;
    assert_eq!(event, "order");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&data).unwrap()["order_id"],
        order_id.as_str()
    );
}

// -- The order-event log stream ----------------------------------------

/// Reads SSE frames from `body` until one full event has arrived, returning
/// its `(event, id, data)`.
async fn next_logged_event(body: &mut Body, buffer: &mut String) -> (String, String, String) {
    loop {
        if let Some(end) = buffer.find("\n\n") {
            let block: String = buffer.drain(..end + 2).collect();
            let (mut event, mut id, mut data) = (String::new(), String::new(), String::new());
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("event: ") {
                    event = value.to_owned();
                } else if let Some(value) = line.strip_prefix("id: ") {
                    id = value.to_owned();
                } else if let Some(value) = line.strip_prefix("data: ") {
                    data = value.to_owned();
                } else {
                    // Comments: keep-alives.
                }
            }
            if !event.is_empty() {
                return (event, id, data);
            }
            continue;
        }
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), body.frame())
            .await
            .expect("timed out waiting for an SSE event")
            .expect("stream ended")
            .unwrap();
        if let Ok(bytes) = frame.into_data() {
            buffer.push_str(std::str::from_utf8(&bytes).unwrap());
        }
    }
}

/// Opens the order-event stream with `request` built on top of a GET of
/// `uri`.
async fn open_order_event_log(
    router: &Router,
    uri: &str,
    last_event_id: Option<&str>,
) -> axum::response::Response {
    let mut request = Request::builder().uri(uri);
    if let Some(id) = last_event_id {
        request = request.header("last-event-id", id);
    }
    router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// The stream replays every kept event after the reader's position, oldest
/// first, each with its sequence as its id and the order's fields in its
/// data, then sends new ones as they commit.
#[tokio::test]
async fn the_order_event_stream_replays_from_a_position_then_goes_live() {
    let state = AppState::for_tests();
    let router = build_router(state.clone(), 1_000_000);
    let tenant = create_tenant(&router, 12).await;
    let order_id = shared::ids::OrderId::new(create_admin_order(&router, &tenant).await);
    let (first, second) = {
        let store = state.db.lock();
        (
            store
                .append_order_event(
                    &order_id,
                    "order.unconfirmed",
                    &[("status", "unconfirmed")],
                    100,
                )
                .unwrap(),
            store
                .append_order_event(&order_id, "order.paid", &[("status", "paid")], 200)
                .unwrap(),
        )
    };

    let response = open_order_event_log(
        &router,
        &format!("/api/v1/admin/order-events?after={first}"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut body = response.into_body();
    let mut buffer = String::new();
    let (event, id, data) = next_logged_event(&mut body, &mut buffer).await;
    assert_eq!((event.as_str(), id), ("order_event", second.to_string()));
    let data: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(data["event"], "order.paid");
    assert_eq!(data["status"], "paid");
    assert_eq!(data["order_id"], order_id.as_str());
    assert_eq!(data["tenant"], tenant.public_key.as_str());
    assert_eq!(data["xmr_amount_piconero"], 1_000_000_000_000u64);
    assert_eq!(data["merchant_order_id"], serde_json::Value::Null);
    assert_eq!(data["created_at"], 200);
    assert!(data["event_id"].as_str().unwrap().starts_with("evt_"));

    // Live: a status change committed now arrives next.
    let third = state
        .db
        .lock()
        .in_transaction(|s| {
            s.append_order_event(&order_id, "order.double_spend_detected", &[], 300)
        })
        .unwrap();
    let (event, id, data) = next_logged_event(&mut body, &mut buffer).await;
    assert_eq!((event.as_str(), id), ("order_event", third.to_string()));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&data).unwrap()["event"],
        "order.double_spend_detected"
    );

    // `Last-Event-ID` wins over `?after=`, as on an EventSource reconnect.
    let response = open_order_event_log(
        &router,
        "/api/v1/admin/order-events?after=0",
        Some(&second.to_string()),
    )
    .await;
    let mut body = response.into_body();
    let mut buffer = String::new();
    let (_, id, _) = next_logged_event(&mut body, &mut buffer).await;
    assert_eq!(id, third.to_string());
}

/// A reader asking from before the oldest kept event is told, with a
/// distinct `events_lost` event, which events it missed, then gets the
/// rest; so is one whose position this log never handed out.
#[tokio::test]
async fn the_order_event_stream_says_when_a_reader_missed_events() {
    let state = AppState::for_tests();
    let router = build_router(state.clone(), 1_000_000);
    let tenant = create_tenant(&router, 13).await;
    let order_id = shared::ids::OrderId::new(create_admin_order(&router, &tenant).await);
    let kept = {
        let store = state.db.lock();
        for at in [10, 20] {
            store
                .append_order_event(&order_id, "order.unconfirmed", &[], at)
                .unwrap();
        }
        let kept = store
            .append_order_event(&order_id, "order.paid", &[], 5000)
            .unwrap();
        assert_eq!(store.prune_order_events_before(1000).unwrap(), 2);
        kept
    };

    let mut body = open_order_event_log(&router, "/api/v1/admin/order-events?after=0", None)
        .await
        .into_body();
    let mut buffer = String::new();
    let (event, id, data) = next_logged_event(&mut body, &mut buffer).await;
    assert_eq!(event, "events_lost");
    assert_eq!(id, (kept - 1).to_string());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&data).unwrap(),
        serde_json::json!({ "requested_after": 0, "resume_after": kept - 1 })
    );
    let (event, id, _) = next_logged_event(&mut body, &mut buffer).await;
    assert_eq!((event.as_str(), id), ("order_event", kept.to_string()));

    // A position past the newest event: another engine's, or one whose
    // database was replaced.
    let mut body = open_order_event_log(&router, "/api/v1/admin/order-events?after=999", None)
        .await
        .into_body();
    let mut buffer = String::new();
    let (event, _, data) = next_logged_event(&mut body, &mut buffer).await;
    assert_eq!(event, "events_lost");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&data).unwrap()["resume_after"],
        kept - 1
    );

    // Nothing lost: no `events_lost`, straight to the event.
    let mut body = open_order_event_log(
        &router,
        &format!("/api/v1/admin/order-events?after={}", kept - 1),
        None,
    )
    .await
    .into_body();
    let mut buffer = String::new();
    assert_eq!(
        next_logged_event(&mut body, &mut buffer).await.0,
        "order_event"
    );

    // Not a position at all.
    let response = open_order_event_log(&router, "/api/v1/admin/order-events", Some("abc")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn listing_orders_by_ids_returns_only_this_tenants_named_orders_in_order() {
    async fn create(router: &Router, token: &str) -> String {
        let req = json_request(
            "POST",
            "/api/v1/admin/tenant/orders",
            Some(token),
            &serde_json::json!({ "xmr_amount_piconero": 1_000u64 }),
        );
        body_json(router.clone().oneshot(req).await.unwrap()).await["order_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    let router = test_router();
    let tenant = create_tenant(&router, 40).await;
    let other = create_tenant(&router, 42).await;
    let (a, b, _c) = (
        create(&router, &tenant.secret_token).await,
        create(&router, &tenant.secret_token).await,
        create(&router, &tenant.secret_token).await,
    );
    let foreign = create(&router, &other.secret_token).await;

    let uri = format!("/api/v1/admin/tenant/orders?ids={b},order_unknown,{foreign},{a}");
    let response = router
        .clone()
        .oneshot(json_request(
            "GET",
            &uri,
            Some(&tenant.secret_token),
            &serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let ids: Vec<String> = body_json(response)
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["order_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        ids,
        vec![b, a],
        "only this tenant's named orders, in the order asked"
    );

    let too_many = (0..=super::admin::MAX_LIST_ORDER_IDS)
        .map(|i| format!("order_{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let response = router
        .oneshot(json_request(
            "GET",
            &format!("/api/v1/admin/tenant/orders?ids={too_many}"),
            Some(&tenant.secret_token),
            &serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn listing_orders_can_page_search_and_keep_to_open_orders() {
    let state = AppState::for_tests();
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 44).await;
    let mut ids = Vec::new();
    for reference in ["Table 1", "Table 2", "wc-1042", "Table 3"] {
        let req = json_request(
            "POST",
            "/api/v1/admin/tenant/orders",
            Some(&tenant.secret_token),
            &serde_json::json!({ "xmr_amount_piconero": 1_000u64, "merchant_order_id": reference }),
        );
        ids.push(
            body_json(router.clone().oneshot(req).await.unwrap()).await["order_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    // "Table 2" is paid, so no longer open.
    {
        let store = store.lock();
        store
            .record_payment_match(
                &shared::ids::OrderId::new(ids[1].clone()),
                "tx_paid",
                0,
                1_000,
                "[]",
                crate::now_unix(),
                Some(10),
                None,
            )
            .unwrap();
        crate::scanner::recompute_and_notify(
            &store,
            &shared::ids::OrderId::new(ids[1].clone()),
            100,
            crate::now_unix(),
        )
        .unwrap();
    }
    let list = |query: &str| {
        let router = router.clone();
        let uri = format!("/api/v1/admin/tenant/orders?{query}");
        let token = tenant.secret_token.clone();
        async move {
            let response = router
                .oneshot(json_request(
                    "GET",
                    &uri,
                    Some(&token),
                    &serde_json::Value::Null,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            body_json(response)
                .await
                .as_array()
                .unwrap()
                .iter()
                .map(|o| o["merchant_order_id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        }
    };
    // Made within the same second, so compare as sets; pages follow the
    // full list's own order.
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    assert_eq!(
        sorted(list("open=true").await),
        vec!["Table 1", "Table 3", "wc-1042"]
    );
    let tables = list("search=table").await;
    assert_eq!(
        sorted(tables.clone()),
        vec!["Table 1", "Table 2", "Table 3"]
    );
    assert_eq!(
        list(&format!("search={}", &ids[2][6..14])).await,
        vec!["wc-1042"],
        "by order id"
    );
    assert_eq!(list("search=table&limit=2").await, tables[..2].to_vec());
    assert_eq!(
        list("search=table&limit=2&offset=2").await,
        tables[2..].to_vec()
    );
    assert_eq!(list("search=nothing").await, Vec::<String>::new());
    // A blank search is no search: the plain list, status filter honoured.
    assert_eq!(
        sorted(list("search=%20%20&status=paid").await),
        vec!["Table 2"]
    );
    // The two shapes of listing don't mix: a parameter of the other shape
    // is refused, never quietly ignored.
    for query in [
        "open=true&status=paid",
        "search=table&cursor=1",
        "offset=1&cursor_id=x",
    ] {
        let response = router
            .clone()
            .oneshot(json_request(
                "GET",
                &format!("/api/v1/admin/tenant/orders?{query}"),
                Some(&tenant.secret_token),
                &serde_json::Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
    }
}

// -- Request and stream limits (admin_settings_v2.md task 7.10) ---------------

/// `/wait` holds its request permit until `release` is notified, and says
/// so on `holding` once it has it: a test that needs the limit reached
/// waits for that, never for a clock.
fn limited_router(
    limits: RequestLimits,
    holding: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
) -> Router {
    use axum::routing::get;
    let slow = get(async || {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        "slow"
    });
    let wait = get(move || {
        let (holding, release) = (Arc::clone(&holding), Arc::clone(&release));
        async move {
            // Registered before `holding` is signalled, so a `notify_one`
            // sent before this task is back here is still seen.
            let released = release.notified();
            holding.notify_one();
            released.await;
            "released"
        }
    });
    let stream = get(async || {
        let body =
            futures_util::stream::pending::<Result<axum::body::Bytes, std::convert::Infallible>>();
        Body::from_stream(body)
    });
    Router::new()
        .route("/slow", slow)
        .route("/wait", wait)
        .route("/fast", get(async || "fast"))
        .layer(axum::middleware::from_fn_with_state(
            limits.clone(),
            request_limit_middleware,
        ))
        .merge(
            Router::new()
                .route("/stream", stream)
                .layer(axum::middleware::from_fn_with_state(
                    limits,
                    stream_limit_middleware,
                )),
        )
}

async fn status_of(router: &Router, path: &str) -> StatusCode {
    router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test(start_paused = true)]
async fn a_request_that_takes_too_long_gets_503() {
    let limits = RequestLimits::new(10, 10, std::time::Duration::from_millis(100));
    let router = limited_router(limits, Arc::default(), Arc::default());
    assert_eq!(
        status_of(&router, "/slow").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn requests_beyond_the_concurrency_limit_get_503_at_once_instead_of_queueing() {
    let holding = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let limits = RequestLimits::new(1, 10, std::time::Duration::from_secs(60));
    let router = limited_router(limits, Arc::clone(&holding), Arc::clone(&release));
    let held = tokio::spawn({
        let router = router.clone();
        async move { status_of(&router, "/wait").await }
    });
    holding.notified().await;
    assert_eq!(
        status_of(&router, "/wait").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    release.notify_one();
    assert_eq!(held.await.unwrap(), StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn event_streams_outlive_the_request_timeout_and_have_their_own_cap() {
    let limits = RequestLimits::new(1, 1, std::time::Duration::from_millis(100));
    let router = limited_router(limits, Arc::default(), Arc::default());
    let first = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    // Still open well past the request timeout, and not using up the request
    // limit: an ordinary request still gets through.
    assert_eq!(status_of(&router, "/fast").await, StatusCode::OK);
    assert_eq!(
        status_of(&router, "/stream").await,
        StatusCode::SERVICE_UNAVAILABLE,
        "stream cap reached"
    );
    drop(first);
    assert_eq!(
        status_of(&router, "/stream").await,
        StatusCode::OK,
        "a closed stream frees its place"
    );
}

// -- Database and backend failures map to 503 (admin_settings_v2.md task 7.7) --

#[test]
fn a_full_or_locked_database_is_503_but_a_constraint_violation_is_500() {
    use crate::store::StoreError;
    let sqlite = |code| {
        StoreError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(code),
            None,
        ))
    };
    for code in [
        rusqlite::ffi::SQLITE_FULL,
        rusqlite::ffi::SQLITE_BUSY,
        rusqlite::ffi::SQLITE_LOCKED,
        rusqlite::ffi::SQLITE_IOERR,
    ] {
        assert!(
            matches!(ApiError::from(sqlite(code)), ApiError::Unavailable(_)),
            "code {code}"
        );
    }
    assert!(matches!(
        ApiError::from(sqlite(rusqlite::ffi::SQLITE_CONSTRAINT)),
        ApiError::Internal(_)
    ));
}

#[test]
fn a_key_custody_backend_that_is_down_is_503() {
    use super::ApiError;
    use crate::key_custody::KeyCustodyError;
    assert!(matches!(
        ApiError::from(KeyCustodyError::BackendUnavailable("down".into())),
        ApiError::Unavailable(_)
    ));
    assert!(matches!(
        ApiError::from(KeyCustodyError::UnknownWallet),
        ApiError::Unavailable(_)
    ));
}

// -- Settings apply to the running engine (admin_settings_v2.md part 2) --------

/// An engine whose node settings really build daemon clients, as in
/// production, starting with no node configured.
async fn engine_that_applies_node_settings() -> (
    Router,
    crate::engine_settings::Daemons,
    Arc<RateLimiter<String>>,
) {
    let store = Store::open_in_memory().unwrap().into_shared();
    let daemons = crate::engine_settings::Daemons::default();
    let rate_limiter = Arc::new(RateLimiter::new(10_000));
    let settings = crate::engine_settings::EngineSettings::load_with(
        Arc::clone(&store),
        Some(crate::engine_settings::NodesReloadable {
            daemons: daemons.clone(),
        }),
        Arc::clone(&rate_limiter),
        live_settings::Env::fixed(Vec::<(String, String)>::new()),
    )
    .await
    .unwrap();
    let state = AppState {
        admin_rate_limiter: Arc::clone(&rate_limiter),
        settings,
        networks: crate::http::Networks {
            daemons: daemons.clone(),
            scanner_status: crate::scanner_status::new_scanner_status_map(),
        },
        ..AppState::for_tests_with_store(store)
    };
    (build_router(state, 16 * 1024 * 1024), daemons, rate_limiter)
}

async fn save_settings(router: &Router, body: serde_json::Value) -> serde_json::Value {
    let response = router
        .clone()
        .oneshot(settings_request("POST", Some(body)))
        .await
        .unwrap();
    let status = response.status();
    let json = body_json(response).await;
    assert_eq!(status, StatusCode::OK, "save refused: {json}");
    json
}

fn stagenet_tenant_request(seed: u8) -> Request<Body> {
    json_request(
        "POST",
        "/api/v1/admin/tenants",
        None,
        &serde_json::json!({
            "view_key_hex": valid_view_key_hex(seed),
            "spend_pubkey_hex": valid_spend_pubkey_hex(seed.wrapping_add(1)),
            "network": "stagenet",
        }),
    )
}

#[tokio::test]
async fn a_saved_node_is_used_straight_away_and_clearing_it_stops_it_the_reported_bug() {
    let (router, daemons, _) = engine_that_applies_node_settings().await;

    // Before: stagenet isn't configured, so a stagenet store can't be created.
    let refused = router
        .clone()
        .oneshot(stagenet_tenant_request(1))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert!(get_status_json(router.clone()).await["networks"]
        .as_array()
        .unwrap()
        .is_empty());

    // Save a stagenet node, as dev-run.sh and the admin page do.
    let [primary, fallback] = unreachable_ports();
    save_settings(
        &router,
        serde_json::json!({ "monero_node": { "stagenet": {
            "host": "127.0.0.1", "port": primary, "ssl": false, "accept_self_signed_certs": true,
            "fallbacks": [{ "host": "127.0.0.1", "port": fallback, "ssl": false, "accept_self_signed_certs": true, "fallbacks": [] }]
        } } }),
    )
    .await;

    // Straight away, with no restart: a client for it exists, a stagenet
    // store can be created, and /status lists it with both nodes.
    assert!(daemons.is_configured(Network::Stagenet));
    let created = router
        .clone()
        .oneshot(stagenet_tenant_request(2))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let status = get_status_json(router.clone()).await;
    assert_eq!(
        status["networks"][0]["network"], "stagenet",
        "got: {status}"
    );
    assert_eq!(status["networks"][0]["nodes"].as_array().unwrap().len(), 2);

    // Clearing it stops it, and the save says a store still uses it (D2).
    let cleared = save_settings(
        &router,
        serde_json::json!({ "monero_node": { "stagenet": null } }),
    )
    .await;
    assert_eq!(
        cleared["warnings"]["unserved_networks"],
        serde_json::json!([{ "network": "stagenet", "tenants": 1 }])
    );
    assert!(!daemons.is_configured(Network::Stagenet));
    let refused = router
        .clone()
        .oneshot(stagenet_tenant_request(3))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_unchanged_network_keeps_its_client_when_another_network_is_saved() {
    let (router, daemons, _) = engine_that_applies_node_settings().await;
    let node = serde_json::json!({ "host": "127.0.0.1", "port": unreachable_port(), "ssl": false, "accept_self_signed_certs": true, "fallbacks": [] });
    save_settings(
        &router,
        serde_json::json!({ "monero_node": { "stagenet": node.clone() } }),
    )
    .await;
    let before = daemons.get(Network::Stagenet).unwrap();
    save_settings(
        &router,
        serde_json::json!({ "monero_node": { "testnet": node } }),
    )
    .await;
    assert!(
        Arc::ptr_eq(&before, &daemons.get(Network::Stagenet).unwrap()),
        "stagenet's client (and its node health) was kept"
    );
    assert!(daemons.is_configured(Network::Testnet));
}

#[tokio::test]
async fn a_saved_rate_limit_body_limit_and_tenant_default_apply_to_the_next_request() {
    let (router, _, rate_limiter) = engine_that_applies_node_settings().await;
    save_settings(
        &router,
        serde_json::json!({ "scalars": {
            "server.rate_limit_per_token_per_min": "7",
            "server.max_body_bytes": "300",
            "payment.confirmations_required": "3"
        } }),
    )
    .await;
    assert_eq!(rate_limiter.limit(), 7);

    let big = json_request(
        "POST",
        "/api/v1/admin/tenants",
        None,
        &serde_json::json!({ "view_key_hex": "a".repeat(400), "spend_pubkey_hex": "b" }),
    );
    assert_eq!(
        router.clone().oneshot(big).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );

    // The same body sent in chunks, with no length declared up front: cut
    // off at the limit as it's read.
    let text =
        serde_json::json!({ "view_key_hex": "a".repeat(400), "spend_pubkey_hex": "b" }).to_string();
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> = text
        .into_bytes()
        .chunks(50)
        .map(|c| Ok(c.to_vec()))
        .collect();
    let streamed = Request::builder()
        .method("POST")
        .uri("/api/v1/admin/tenants")
        .header("content-type", "application/json")
        .body(Body::from_stream(futures_util::stream::iter(chunks)))
        .unwrap();
    assert_eq!(
        router.clone().oneshot(streamed).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    // A chunked body that breaks off (the client went away) is not an
    // oversized one.
    let broken: Vec<Result<Vec<u8>, std::io::Error>> = vec![
        Ok(b"{\"view_key".to_vec()),
        Err(std::io::Error::other("connection reset")),
    ];
    let broken = Request::builder()
        .method("POST")
        .uri("/api/v1/admin/tenants")
        .header("content-type", "application/json")
        .body(Body::from_stream(futures_util::stream::iter(broken)))
        .unwrap();
    assert_eq!(
        router.clone().oneshot(broken).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );

    save_settings(
        &router,
        serde_json::json!({ "scalars": { "server.max_body_bytes": "8192" } }),
    )
    .await;
    let save_node = serde_json::json!({ "monero_node": { "mainnet": { "host": "127.0.0.1", "port": unreachable_port(), "ssl": false, "accept_self_signed_certs": true, "fallbacks": [] } } });
    save_settings(&router, save_node).await;
    let tenant = create_tenant(&router, 5).await;
    let me = router
        .clone()
        .oneshot(json_request(
            "GET",
            "/api/v1/admin/tenant",
            Some(&tenant.secret_token),
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
    let me = body_json(me).await;
    assert_eq!(
        me["confirmations_required"], 3,
        "the saved default, not the old hardcoded 10: {me}"
    );
}

#[tokio::test]
async fn saving_a_restart_only_setting_says_so() {
    let (router, _, _) = engine_that_applies_node_settings().await;
    let saved = save_settings(
        &router,
        serde_json::json!({ "scalars": { "server.worker_threads": "4" } }),
    )
    .await;
    assert_eq!(
        saved["warnings"]["restart_required"],
        serde_json::json!(["server.worker_threads"])
    );
    let get = router
        .clone()
        .oneshot(settings_request("GET", None))
        .await
        .unwrap();
    let body = body_json(get).await;
    assert_eq!(
        body["scalars"]["server.worker_threads"]["pending_restart"],
        true
    );
    assert_eq!(
        body["scalars"]["server.worker_threads"]["applies"],
        "restart"
    );
    assert!(body["networks"]["stagenet"]["description"]
        .as_str()
        .unwrap()
        .contains("stagenet"));
}

#[tokio::test]
async fn an_unknown_setting_is_refused_and_nothing_is_saved() {
    let (router, _, _) = engine_that_applies_node_settings().await;
    let response = router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some(serde_json::json!({ "scalars": { "payment.default_rescan_lookback_days": "3", "payment.confirmations_required": "4" } })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let get = router
        .clone()
        .oneshot(settings_request("GET", None))
        .await
        .unwrap();
    assert_eq!(
        body_json(get).await["scalars"]["payment.confirmations_required"]["value"],
        "10"
    );
}

#[tokio::test]
async fn status_lists_a_store_whose_network_has_no_answering_node() {
    let (router, _, _) = engine_that_applies_node_settings().await;
    // A node that answers nothing: configured, but unreachable.
    save_settings(
        &router,
        serde_json::json!({ "monero_node": { "stagenet": { "host": "127.0.0.1", "port": unreachable_port(), "ssl": false, "accept_self_signed_certs": true, "fallbacks": [] } } }),
    )
    .await;
    let created = router
        .clone()
        .oneshot(stagenet_tenant_request(7))
        .await
        .unwrap();
    let created = body_json(created).await;
    let public_key = created["public_key"].as_str().unwrap().to_owned();

    let status = get_status_json(router.clone()).await;
    assert_eq!(
        status["unserved_tenants"],
        serde_json::json!([{ "public_key": public_key, "network": "stagenet", "reason": "no_reachable_node", "blocks_behind": null }]),
        "got: {status}"
    );
}

#[tokio::test]
async fn saving_nodes_that_dont_answer_for_a_network_stores_use_is_reported() {
    let (router, _, _) = engine_that_applies_node_settings().await;
    let node = |port: u16| serde_json::json!({ "monero_node": { "stagenet": { "host": "127.0.0.1", "port": port, "ssl": false, "accept_self_signed_certs": true, "fallbacks": [] } } });
    let [first, second] = unreachable_ports();
    save_settings(&router, node(first)).await;
    assert_eq!(
        router
            .clone()
            .oneshot(stagenet_tenant_request(8))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    let saved = save_settings(&router, node(second)).await;
    assert_eq!(
        saved["warnings"]["unserved_networks"],
        serde_json::json!([{ "network": "stagenet", "tenants": 1 }]),
        "{saved}"
    );
}

// -- Per-store key custody (admin_settings_v2.md part 5) --------------------

/// Two in-process backends, named as the real ones (plain stands in for
/// snp), so a store can be moved between them without SEV-SNP hardware.
fn test_app_state_with_two_custody_backends() -> (AppState, Arc<dyn KeyCustody>, Arc<dyn KeyCustody>)
{
    let plain: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
    let snp: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
    let router = crate::key_custody::CustodyRouter::new(
        HashMap::from([
            ("plain".to_owned(), Arc::clone(&plain)),
            ("snp".to_owned(), Arc::clone(&snp)),
        ]),
        "plain",
    );
    let mut state = AppState::for_tests();
    state.custody.backends = Arc::new(router);
    (state, plain, snp)
}

async fn own_tenant_view(router: &Router, token: &str) -> serde_json::Value {
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/admin/tenant")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

async fn create_order_for(router: &Router, token: &str) -> axum::response::Response {
    router
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/admin/tenant/orders",
            Some(token),
            &serde_json::json!({ "xmr_amount_piconero": 1_000_000_000u64 }),
        ))
        .await
        .unwrap()
}

fn switch_request(token: &str, backend: &str, seed: u8) -> Request<Body> {
    json_request(
        "PUT",
        "/api/v1/admin/tenant/key-custody",
        Some(token),
        &serde_json::json!({
            "backend": backend,
            "view_key_hex": valid_view_key_hex(seed),
            "spend_pubkey_hex": valid_spend_pubkey_hex(seed.wrapping_add(1)),
        }),
    )
}

#[tokio::test]
async fn a_new_store_goes_to_the_default_backend_or_the_one_it_asks_for() {
    let (state, _, _) = test_app_state_with_two_custody_backends();
    let router = build_router(state, 1_000_000);

    let by_default = create_tenant(&router, 1).await;
    assert_eq!(
        own_tenant_view(&router, &by_default.secret_token).await["key_custody_backend"],
        "plain"
    );

    let response = router
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/admin/tenants",
            None,
            &serde_json::json!({
                "view_key_hex": valid_view_key_hex(3),
                "spend_pubkey_hex": valid_spend_pubkey_hex(4),
                "key_custody_backend": "snp",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let token = body_json(response).await["secret_token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        own_tenant_view(&router, &token).await["key_custody_backend"],
        "snp"
    );
    assert_eq!(
        create_order_for(&router, &token).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_new_store_can_not_use_a_backend_that_is_not_enabled() {
    let (state, _, _) = test_app_state_with_two_custody_backends();
    let router = build_router(state, 1_000_000);
    let response = router
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/admin/tenants",
            None,
            &serde_json::json!({
                "view_key_hex": valid_view_key_hex(3),
                "spend_pubkey_hex": valid_spend_pubkey_hex(4),
                "key_custody_backend": "hsm",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn moving_a_store_to_another_backend_keeps_it_taking_orders_and_frees_the_old_registration() {
    let (state, plain, snp) = test_app_state_with_two_custody_backends();
    let wallet_handles = Arc::clone(&state.custody.wallet_handles);
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;
    assert_eq!(
        create_order_for(&router, &tenant.secret_token)
            .await
            .status(),
        StatusCode::OK
    );
    let old_handle = wallet_handles.read().values().copied().next().unwrap();
    let before = own_tenant_view(&router, &tenant.secret_token).await;

    let response = router
        .clone()
        .oneshot(switch_request(&tenant.secret_token, "snp", 1))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let view = body_json(response).await;
    assert_eq!(view["key_custody_backend"], "snp");
    assert!(
        view.get("view_key_hex").is_none(),
        "keys are never echoed back"
    );

    let new_handle = wallet_handles.read().values().copied().next().unwrap();
    assert!(
        snp.derive_subaddress(
            new_handle,
            crate::key_custody::SubaddressIndex::default(),
            Network::Mainnet
        )
        .await
        .is_ok(),
        "the store's keys are in the new backend"
    );
    assert!(
        plain
            .derive_subaddress(
                old_handle,
                crate::key_custody::SubaddressIndex::default(),
                Network::Mainnet
            )
            .await
            .is_err(),
        "and no longer in the old one"
    );

    // Same wallet, so the same addresses: an order made now gets the next one.
    let response = create_order_for(&router, &tenant.secret_token).await;
    assert_eq!(response.status(), StatusCode::OK);
    let tenant_row = store
        .lock()
        .list_active_tenants()
        .unwrap()
        .into_iter()
        .find(|t| t.public_key == tenant.public_key)
        .unwrap();
    assert_eq!(tenant_row.key_custody_backend, "snp");
    assert_eq!(
        tenant_row.primary_address,
        before["primary_address"].as_str().unwrap()
    );

    // After a restart the store's keys come back from the new backend.
    wallet_handles.write().clear();
    assert_eq!(
        create_order_for(&router, &tenant.secret_token)
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn moving_a_store_needs_the_keys_of_its_own_wallet() {
    let (state, _, _) = test_app_state_with_two_custody_backends();
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;

    let response = router
        .clone()
        .oneshot(switch_request(&tenant.secret_token, "snp", 7))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert!(body.to_string().contains("different wallet"), "{body}");
    let tenant_row = store
        .lock()
        .list_active_tenants()
        .unwrap()
        .into_iter()
        .find(|t| t.public_key == tenant.public_key)
        .unwrap();
    assert_eq!(tenant_row.key_custody_backend, "plain", "nothing changed");
    assert_eq!(
        create_order_for(&router, &tenant.secret_token)
            .await
            .status(),
        StatusCode::OK
    );

    let response = router
        .clone()
        .oneshot(switch_request(&tenant.secret_token, "hsm", 1))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "only an enabled backend"
    );
}

#[tokio::test]
async fn an_order_made_while_the_backend_has_just_lost_the_store_still_succeeds() {
    // The backend restarted and forgot every wallet, but the engine's map
    // still has the old handle: the order re-registers and goes through.
    let (state, plain, _) = test_app_state_with_two_custody_backends();
    let wallet_handles = Arc::clone(&state.custody.wallet_handles);
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;
    let handle = wallet_handles.read().values().copied().next().unwrap();
    plain.remove_wallet(handle).await.unwrap();

    assert_eq!(
        create_order_for(&router, &tenant.secret_token)
            .await
            .status(),
        StatusCode::OK
    );
    assert_ne!(
        wallet_handles.read().values().copied().next().unwrap(),
        handle
    );
}

/// A backend that holds wallets but whose health check fails, as a backend
/// in another process does once it stops answering.
#[derive(Default)]
struct UnansweringKeyCustody {
    inner: PlainKeyCustody,
}

#[async_trait::async_trait]
impl KeyCustody for UnansweringKeyCustody {
    async fn register_wallet(
        &self,
        material: crate::key_custody::WalletMaterial,
    ) -> Result<crate::key_custody::WalletHandle, crate::key_custody::KeyCustodyError> {
        self.inner.register_wallet(material).await
    }
    async fn remove_wallet(
        &self,
        handle: crate::key_custody::WalletHandle,
    ) -> Result<(), crate::key_custody::KeyCustodyError> {
        self.inner.remove_wallet(handle).await
    }
    async fn seal(
        &self,
        material: &crate::key_custody::WalletMaterial,
    ) -> Result<Vec<u8>, crate::key_custody::KeyCustodyError> {
        self.inner.seal(material).await
    }
    async fn unseal_and_register(
        &self,
        sealed: &[u8],
    ) -> Result<crate::key_custody::WalletHandle, crate::key_custody::KeyCustodyError> {
        self.inner.unseal_and_register(sealed).await
    }
    async fn derive_subaddress(
        &self,
        handle: crate::key_custody::WalletHandle,
        index: crate::key_custody::SubaddressIndex,
        network: Network,
    ) -> Result<monero::Address, crate::key_custody::KeyCustodyError> {
        self.inner.derive_subaddress(handle, index, network).await
    }
    async fn scan_tx_outputs(
        &self,
        handle: crate::key_custody::WalletHandle,
        tx: &crate::key_custody::ScanInput,
        major_range: std::ops::Range<u32>,
        minor_range: std::ops::Range<u32>,
    ) -> Result<Vec<crate::key_custody::MatchedOutput>, crate::key_custody::KeyCustodyError> {
        self.inner
            .scan_tx_outputs(handle, tx, major_range, minor_range)
            .await
    }
    async fn check_state(&self) -> Result<u64, crate::key_custody::KeyCustodyError> {
        Err(crate::key_custody::KeyCustodyError::BackendUnavailable(
            "connection refused".to_owned(),
        ))
    }
}

#[tokio::test]
async fn status_lists_stores_whose_key_storage_is_turned_off_or_not_answering() {
    let plain: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
    let snp: Arc<dyn KeyCustody> = Arc::new(UnansweringKeyCustody::default());
    let router_custody = Arc::new(crate::key_custody::CustodyRouter::new(
        HashMap::from([
            ("plain".to_owned(), Arc::clone(&plain)),
            ("snp".to_owned(), snp),
        ]),
        "plain",
    ));
    let mut state = AppState::for_tests();
    state.custody.backends =
        Arc::<crate::key_custody::router::CustodyRouter>::clone(&router_custody);
    let router = build_router(state, 1_000_000);

    let on_plain = create_tenant(&router, 1).await;
    let response = router
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/admin/tenants",
            None,
            &serde_json::json!({
                "view_key_hex": valid_view_key_hex(3),
                "spend_pubkey_hex": valid_spend_pubkey_hex(4),
                "key_custody_backend": "snp",
            }),
        ))
        .await
        .unwrap();
    let on_socket = body_json(response).await["public_key"]
        .as_str()
        .unwrap()
        .to_owned();

    let status = get_status_json(router.clone()).await;
    assert_eq!(
        status["key_custody"],
        serde_json::json!([{ "backend": "plain", "error": null }, { "backend": "snp", "error": "key custody backend unavailable: connection refused" }]),
        "{status}"
    );
    let reasons: Vec<(String, String)> = status["unserved_tenants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            (
                u["public_key"].as_str().unwrap().to_owned(),
                u["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        reasons,
        vec![(on_socket.clone(), "custody_unavailable".to_owned())],
        "{status}"
    );
    assert!(!reasons.iter().any(|(key, _)| key == &on_plain.public_key));

    router_custody.replace(HashMap::from([("plain".to_owned(), plain)]), "plain");
    let status = get_status_json(router.clone()).await;
    assert_eq!(
        status["unserved_tenants"][0]["reason"], "custody_disabled",
        "{status}"
    );
    assert_eq!(status["unserved_tenants"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_store_a_block_or_two_behind_is_not_reported_but_one_further_behind_is() {
    let state = AppState::for_tests();
    let store = Arc::clone(state.db.shared_store_for_test());
    store
        .lock()
        .set_scanned_block(Network::Mainnet, 100, "h100")
        .unwrap();
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;
    let row = store
        .lock()
        .list_active_tenants()
        .unwrap()
        .into_iter()
        .find(|t| t.public_key == tenant.public_key)
        .unwrap();
    // Anchored at 100 (the last scanned block), then the network moves on
    // while this store's cursor stays behind.
    assert_eq!(
        row.scanned_through_height,
        Some(100),
        "a new store starts at the network's last scanned block"
    );
    store
        .lock()
        .set_scanned_block(Network::Mainnet, 102, "h102")
        .unwrap();
    let status = get_status_json(router.clone()).await;
    assert_eq!(
        status["unserved_tenants"],
        serde_json::json!([]),
        "2 blocks behind is normal mid-tick: {status}"
    );

    store
        .lock()
        .set_scanned_block(Network::Mainnet, 103, "h103")
        .unwrap();
    let status = get_status_json(router.clone()).await;
    assert_eq!(
        status["unserved_tenants"][0]["reason"], "catching_up",
        "{status}"
    );
    assert_eq!(status["unserved_tenants"][0]["blocks_behind"], 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_overlapping_moves_of_one_store_leave_its_row_and_its_live_keys_in_the_same_backend() {
    let plain: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
    let snp: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
    let custody = Arc::new(crate::key_custody::CustodyRouter::new(
        HashMap::from([("plain".to_owned(), plain), ("snp".to_owned(), snp)]),
        "plain",
    ));
    let mut state = AppState::for_tests();
    state.custody.backends = Arc::<crate::key_custody::router::CustodyRouter>::clone(&custody);
    let wallet_handles = Arc::clone(&state.custody.wallet_handles);
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;
    for _ in 0..10 {
        let a = tokio::spawn(router.clone().oneshot(switch_request(
            &tenant.secret_token,
            "snp",
            1,
        )));
        let b = tokio::spawn(router.clone().oneshot(switch_request(
            &tenant.secret_token,
            "plain",
            1,
        )));
        assert_eq!(a.await.unwrap().unwrap().status(), StatusCode::OK);
        assert_eq!(b.await.unwrap().unwrap().status(), StatusCode::OK);
        let row = store
            .lock()
            .list_active_tenants()
            .unwrap()
            .into_iter()
            .find(|t| t.public_key == tenant.public_key)
            .unwrap();
        let handle = wallet_handles.read()[&row.id];
        assert_eq!(
            custody.backend_of(handle).as_deref(),
            Some(row.key_custody_backend.as_str())
        );
    }
}

#[test]
fn a_disabled_store_can_not_be_moved() {
    let store = Store::open_in_memory().unwrap();
    let tenant = store
        .create_tenant(
            &crate::store::NewTenant {
                key_custody_backend: "plain".into(),
                sealed_key_material: vec![1],
                primary_address: "4x".into(),
                network: "mainnet".into(),
                confirmations_required: None,
                order_expiry_seconds: None,
            },
            1,
        )
        .unwrap()
        .tenant;
    store.disable_tenant(&tenant.id, 2).unwrap();
    assert!(matches!(
        store.update_tenant_key_custody(&tenant.id, "snp", &[2]),
        Err(crate::store::StoreError::NotFound)
    ));
}

// -- The log API (structured_logging.md 3.3) ---------------------------------

/// A directory removed when the test ends, passed or failed.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn the_log_api_answers_the_admin_with_filtered_lines_traces_and_query_errors() {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "engine-log-api-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    )));
    let dir = &dir.0;
    std::fs::create_dir_all(dir).unwrap();
    let (telemetry, subscriber) = telemetry::build(
        "engine",
        telemetry::Format::Json,
        false,
        "info",
        std::io::sink,
    );
    let log_store = telemetry.open_store(&dir.join("engine.logs.db")).unwrap();
    let trace_id = {
        let _guard = tracing::subscriber::set_default(subscriber);
        let span = tracing::info_span!("log api test span");
        span.in_scope(|| tracing::info!(store.id = "s_log_api", "log api test first"));
        tracing::warn!(store.id = "s_other", "log api test second");
        telemetry::trace::span_context(&span)
            .unwrap()
            .trace_id()
            .to_string()
    };
    // The store says when it has stored a batch; that is waited for, not
    // a clock.
    let mut latest = log_store.subscribe();
    let stored = |store: &telemetry::store::LogStore| {
        store
            .query(&telemetry::store::LogQuery {
                limit: 10,
                ..Default::default()
            })
            .unwrap()
            .len()
    };
    while stored(&log_store) < 2 {
        latest.changed().await.unwrap();
    }

    let mut state = AppState::for_tests();
    state.log_store = Some(log_store);
    let router = build_router(state, 1_000_000);
    let get = |uri: String| Request::builder().uri(uri).body(Body::empty()).unwrap();

    let response = router
        .clone()
        .oneshot(get(
            "/api/v1/admin/logs?q=store.id%20%3D%20%27s_log_api%27".into()
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: telemetry::store::api::LogsResponse =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body.rows.len(), 1);
    assert_eq!(body.rows[0].message, "log api test first");
    assert_eq!(body.rows[0].trace_id.as_deref(), Some(trace_id.as_str()));

    let response = router
        .clone()
        .oneshot(get("/api/v1/admin/logs?q=level%20%3D%20loud".into()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: telemetry::store::api::QueryErrorResponse =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(error.error.contains("a level is one of"), "{error:?}");
    assert_eq!(error.start, 8);

    let response = router
        .clone()
        .oneshot(get(format!("/api/v1/admin/logs/trace/{trace_id}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let trace: telemetry::store::Trace =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(trace.logs.len(), 1);

    let response = router
        .clone()
        .oneshot(get("/api/v1/admin/logs/trace/not-a-trace".into()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = router
        .clone()
        .oneshot(get("/api/v1/admin/logs/attributes".into()))
        .await
        .unwrap();
    let names: telemetry::store::api::AttributesResponse =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(names.names.contains(&"store.id".to_owned()), "{names:?}");
}

#[tokio::test]
async fn without_a_log_store_the_log_api_says_so() {
    let state = AppState::for_tests();
    let router = build_router(state, 1_000_000);
    let request = Request::builder()
        .uri("/api/v1/admin/logs")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.oneshot(request).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

// -- A node saved for the wrong network (nicer_admin_screen.md step 4) --

/// A stand-in node that answers `get_height` and, when `nettype` is set,
/// `get_info` with that network; with `None` it doesn't know `get_info`
/// (an old or odd node).
async fn spawn_node_on(nettype: Option<&'static str>) -> std::net::SocketAddr {
    use axum::routing::post;
    let app = Router::new()
        .route("/get_height", post(async || { axum::Json(serde_json::json!({ "height": 10, "status": "OK" })) }))
        .route(
            "/json_rpc",
            post(async move |axum::Json(request): axum::Json<serde_json::Value>| {
                let id = request["id"].clone();
                match (request["method"].as_str(), nettype) {
                    (Some("get_info"), Some(nettype)) => {
                        axum::Json(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "nettype": nettype, "status": "OK" } }))
                    }
                    _ => axum::Json(serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "Method not found" } })),
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn node_json(addr: std::net::SocketAddr, fallbacks: &[std::net::SocketAddr]) -> serde_json::Value {
    serde_json::json!({
        "host": addr.ip().to_string(), "port": addr.port(), "ssl": false, "accept_self_signed_certs": true,
        "fallbacks": fallbacks.iter().map(|f| serde_json::json!({ "host": f.ip().to_string(), "port": f.port(), "ssl": false, "accept_self_signed_certs": true, "fallbacks": [] })).collect::<Vec<_>>(),
    })
}

async fn settings_router() -> Router {
    let (state, _daemon) = test_app_state_with_real_daemon().await;
    build_router(state, 1_000_000)
}

async fn saved_node(router: &Router, network: &str) -> serde_json::Value {
    let get = router
        .clone()
        .oneshot(settings_request("GET", None))
        .await
        .unwrap();
    body_json(get).await["monero_node"][network].clone()
}

async fn try_save(router: &Router, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(settings_request("POST", Some(body)))
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

#[tokio::test]
async fn a_stagenet_node_that_says_it_is_on_mainnet_is_refused_and_nothing_changes() {
    let router = settings_router().await;
    let mainnet = spawn_node_on(Some("mainnet")).await;
    let stagenet = spawn_node_on(Some("stagenet")).await;

    let (status, body) = try_save(
        &router,
        serde_json::json!({ "scalars": { "payment.confirmations_required": "4" }, "monero_node": { "stagenet": node_json(mainnet, &[]) } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let message = format!("127.0.0.1:{} is on mainnet, not stagenet.", mainnet.port());
    assert_eq!(
        body["fields"],
        serde_json::json!([{ "key": "monero_node.stagenet", "message": message }]),
        "{body}"
    );
    assert_eq!(
        saved_node(&router, "stagenet").await,
        serde_json::Value::Null,
        "nothing saved"
    );
    let get = router
        .clone()
        .oneshot(settings_request("GET", None))
        .await
        .unwrap();
    assert_eq!(
        body_json(get).await["scalars"]["payment.confirmations_required"]["value"],
        "10",
        "not even the rest of the request"
    );

    // A fallback on the wrong network is refused the same way.
    let (status, body) = try_save(
        &router,
        serde_json::json!({ "monero_node": { "stagenet": node_json(stagenet, &[mainnet]) } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["fields"][0]["message"], message, "{body}");

    // On the right network, it's saved.
    let (status, body) = try_save(
        &router,
        serde_json::json!({ "monero_node": { "mainnet": node_json(mainnet, &[]) } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn try_check(router: &Router, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/admin/settings/check")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

/// A check refuses what a save would refuse, with the same answer, and
/// otherwise says what a save would do; either way it saves nothing.
#[tokio::test]
async fn a_check_answers_as_a_save_would_and_saves_nothing() {
    let router = settings_router().await;
    let mainnet = spawn_node_on(Some("mainnet")).await;
    let stagenet = spawn_node_on(Some("stagenet")).await;

    for refused in [
        serde_json::json!({ "monero_node": { "stagenet": node_json(mainnet, &[]) } }),
        serde_json::json!({ "scalars": { "payment.confirmations_required": "-1" } }),
        serde_json::json!({ "scalars": { "payment.nope": "1" } }),
        serde_json::json!({ "monero_node": { "moonnet": null } }),
    ] {
        let checked = try_check(&router, refused.clone()).await;
        assert_eq!(
            checked.0,
            StatusCode::BAD_REQUEST,
            "{refused}: {}",
            checked.1
        );
        assert_eq!(
            checked,
            try_save(&router, refused.clone()).await,
            "{refused}"
        );
    }

    let request = serde_json::json!({
        "scalars": { "payment.confirmations_required": "4" },
        "monero_node": { "stagenet": node_json(stagenet, &[]) },
    });
    let (status, body) = try_check(&router, request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["has_changes"], true);
    assert_eq!(
        body["changed"],
        serde_json::json!(["payment.confirmations_required", "monero_node.stagenet"]),
        "{body}"
    );
    // Nothing was saved.
    assert_eq!(
        saved_node(&router, "stagenet").await,
        serde_json::Value::Null
    );
    let get = router
        .clone()
        .oneshot(settings_request("GET", None))
        .await
        .unwrap();
    assert_eq!(
        body_json(get).await["scalars"]["payment.confirmations_required"]["value"],
        "10"
    );

    // The save says the same; checked again, nothing would change.
    let (status, saved) = try_save(&router, request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["changed"], body["changed"]);
    let (status, again) = try_check(&router, request).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["has_changes"], false, "{again}");
    assert_eq!(again["changed"], serde_json::json!([]));
}

/// A key custody backend that's turned on but can't run (it failed to
/// start) says so on the setting that turned it on.
#[tokio::test]
async fn a_backend_that_cannot_run_is_a_problem_on_the_setting_that_enables_it() {
    use crate::key_custody::{CustodyRouter, KeyCustody, PlainKeyCustody, Unstarted};
    let plain: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
    let snp: Arc<dyn KeyCustody> = Arc::new(Unstarted(
        "the snp backend can't start: no security processor".into(),
    ));
    let backends = HashMap::from([("plain".to_owned(), plain), ("snp".to_owned(), snp)]);
    // An engine with settings, as `settings_router`'s.
    let (base, _daemon) = test_app_state_with_real_daemon().await;
    let state = AppState {
        custody: crate::http::Custody {
            backends: Arc::new(CustodyRouter::new(backends, "plain")),
            ..base.custody.clone()
        },
        ..base
    };
    let router = build_router(state, 1_000_000);
    let response = router.oneshot(settings_request("GET", None)).await.unwrap();
    let body = body_json(response).await;
    let problem = body["scalars"]["key_custody.enabled_backends"]["problem"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        problem.contains(
            "The snp backend can't run: the snp backend can't start: no security processor."
        ),
        "{body}"
    );
}

/// Only a node that answers with a network it isn't being saved for is
/// refused: one that doesn't answer may just be down (D2), and one that
/// doesn't say, or is a regtest node, might be right.
#[tokio::test]
async fn nodes_that_do_not_answer_or_do_not_say_are_saved() {
    let router = settings_router().await;
    let fakechain = spawn_node_on(Some("fakechain")).await;
    let old = spawn_node_on(None).await;
    let nothing = std::net::SocketAddr::from(([127, 0, 0, 1], unreachable_port()));

    for (network, node) in [
        ("stagenet", nothing),
        ("testnet", fakechain),
        ("mainnet", old),
    ] {
        let (status, body) = try_save(
            &router,
            serde_json::json!({ "monero_node": { network: node_json(node, &[]) } }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{network}: {body}");
        assert_eq!(
            saved_node(&router, network).await["port"],
            node.port(),
            "{network}"
        );
    }
}

#[tokio::test]
async fn a_node_listed_twice_in_one_network_is_refused() {
    let router = settings_router().await;
    let node = spawn_node_on(Some("stagenet")).await;
    let (status, body) = try_save(
        &router,
        serde_json::json!({ "monero_node": { "stagenet": node_json(node, &[node]) } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["fields"],
        serde_json::json!([{ "key": "monero_node.stagenet", "message": format!("127.0.0.1:{} is listed twice.", node.port()) }])
    );
    assert_eq!(
        saved_node(&router, "stagenet").await,
        serde_json::Value::Null
    );
}

#[tokio::test]
async fn status_says_which_network_each_node_is_on() {
    let mainnet = spawn_node_on(Some("mainnet")).await;
    let mut state = AppState::for_tests();
    let daemon = Arc::new(FallbackDaemonClient::new(vec![
        FallbackNode {
            label: format!("127.0.0.1:{}", mainnet.port()),
            client: Arc::new(
                crate::daemon_rpc::RpcDaemonClient::new("127.0.0.1", mainnet.port(), false, true)
                    .unwrap(),
            ),
        },
        FallbackNode {
            label: "fake-node:18081".to_owned(),
            client: Arc::new(FakeDaemonClient::new()),
        },
    ]));
    state.networks.daemons =
        crate::engine_settings::Daemons::fixed(HashMap::from([(Network::Stagenet, daemon)]));
    let body = get_status_json(build_router(state, 1_000_000)).await;
    let nodes = &body["networks"][0]["nodes"];
    assert_eq!(nodes[0]["network"], "mainnet", "{body}");
    assert_eq!(nodes[0]["height"], 9, "{body}");
    assert!(
        nodes[1]["network"].is_null(),
        "a node that doesn't say: {body}"
    );
}

/// An operator's "take a new anchor" (`docs/proof_of_work.md)`: the anchor
/// and proven chain go, checking stays on (nothing settles until the next
/// anchor); refused where checking is off, or for no network.
#[tokio::test]
async fn forgetting_a_proof_anchor_keeps_checking_on() {
    let store = Store::open_in_memory().unwrap().into_shared();
    let router = build_router(
        AppState::for_tests_with_store(Arc::clone(&store)),
        1_000_000,
    );
    let forget = |network: &str| {
        router.clone().oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/admin/proof/{network}/anchor"))
                .header(shared::auth::ENGINE_TOKEN_HEADER, TEST_ENGINE_TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
    };
    assert_eq!(
        forget("mainnet").await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        forget("moonnet").await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );

    {
        let store = store.lock();
        store.enable_proof(Network::Mainnet, 1).unwrap();
        let block = |height: u64| crate::pow::ProvenBlock {
            height,
            id: [height as u8; 32],
            timestamp: height,
            cumulative_difficulty: u128::from(height),
        };
        store
            .write_anchor(
                Network::Mainnet,
                &crate::store::proof::NewAnchor {
                    agreed: 1,
                    nodes: 1,
                    window: (1..=3).map(block).collect(),
                    seeds: vec![],
                },
                2,
            )
            .unwrap();
    }
    assert_eq!(
        forget("mainnet").await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    let store = store.lock();
    let state = store.proof_network(Network::Mainnet).unwrap().unwrap();
    assert_eq!(state.anchor, None);
    assert_eq!(store.proven_tip(Network::Mainnet).unwrap(), None);
    assert_eq!(store.proof_ceiling(Network::Mainnet).unwrap(), Some(0));
}

#[path = "order_properties.rs"]
mod properties;

// -- Wallets shared by several stores (migration 0028) ----------------------

async fn create_wallet(router: &Router, seed: u8) -> serde_json::Value {
    let req = json_request(
        "POST",
        "/api/v1/admin/wallets",
        None,
        &serde_json::json!({
            "view_key_hex": valid_view_key_hex(seed),
            "spend_pubkey_hex": valid_spend_pubkey_hex(seed.wrapping_add(1)),
        }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

async fn create_tenant_on(router: &Router, wallet_id: &str) -> TestTenant {
    let req = json_request(
        "POST",
        "/api/v1/admin/tenants",
        None,
        &serde_json::json!({ "wallet_id": wallet_id }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    TestTenant {
        public_key: body["public_key"].as_str().unwrap().to_owned(),
        secret_token: body["secret_token"].as_str().unwrap().to_owned(),
    }
}

async fn order_address(router: &Router, tenant: &TestTenant) -> String {
    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        &serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64 }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await["address"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Two shops on one wallet claim order addresses from the wallet's one
/// counter, in turn: no address is ever handed to both, so a payment can
/// only ever match the order it was meant for.
#[tokio::test]
async fn two_stores_on_one_wallet_never_hand_out_the_same_address() {
    let state = AppState::for_tests();
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let wallet = create_wallet(&router, 40).await;
    let wallet_id = wallet["wallet_id"].as_str().unwrap();
    let shop = create_tenant_on(&router, wallet_id).await;
    let market = create_tenant_on(&router, wallet_id).await;

    let mut addresses = Vec::new();
    for tenant in [&shop, &market, &shop, &market, &market] {
        addresses.push(order_address(&router, tenant).await);
    }
    let mut deduped = addresses.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(
        deduped.len(),
        addresses.len(),
        "an address was handed out twice"
    );

    let s = store.lock();
    let wallet_row = s.get_wallet(wallet_id).unwrap().unwrap();
    assert_eq!(
        wallet_row.next_minor_index, 6,
        "five orders claimed indices 1..=5"
    );
    for tenant in [&shop, &market] {
        let row = s
            .find_tenant_by_secret_token(&shared::auth::RawToken::presented(&tenant.secret_token))
            .unwrap()
            .unwrap();
        assert_eq!(row.wallet_id, wallet_id);
        assert_eq!(
            row.next_minor_index, 6,
            "each store's scan range covers every index handed out on the wallet"
        );
        assert_eq!(
            row.primary_address,
            wallet["primary_address"].as_str().unwrap()
        );
    }
}

/// A store made with its own keys gets a wallet of its own, which a second
/// store can then join.
#[tokio::test]
async fn a_store_made_with_keys_gets_its_own_wallet_another_store_can_join() {
    let router = test_router();
    let first = create_tenant(&router, 41).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/admin/tenant")
        .header("authorization", format!("Bearer {}", first.secret_token))
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let view = body_json(response).await;
    let wallet_id = view["wallet_id"].as_str().unwrap().to_owned();
    assert!(wallet_id.starts_with("wl_"));

    let second = create_tenant_on(&router, &wallet_id).await;
    assert_ne!(
        order_address(&router, &first).await,
        order_address(&router, &second).await
    );
}

fn retire_request(wallet_id: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/api/v1/admin/wallets/{wallet_id}/retire"))
        .body(Body::empty())
        .unwrap()
}

async fn wallet_status(router: &Router, wallet_id: &str) -> serde_json::Value {
    let req = Request::builder()
        .uri(format!("/api/v1/admin/wallets/{wallet_id}"))
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

/// Retiring: refused while a store uses the wallet; then its keys are
/// deleted from every row that held them, no store can be made on it, and
/// it comes back only with its own keys.
#[tokio::test]
async fn a_retired_wallet_has_its_keys_deleted_everywhere_and_comes_back_only_with_them() {
    let state = AppState::for_tests();
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let wallet = create_wallet(&router, 42).await;
    let wallet_id = wallet["wallet_id"].as_str().unwrap();
    let tenant = create_tenant_on(&router, wallet_id).await;
    let status = wallet_status(&router, wallet_id).await;
    assert_eq!(status["stores"], 1);
    assert_eq!(status["retired_at"], serde_json::Value::Null);

    let response = router
        .clone()
        .oneshot(retire_request(wallet_id))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(body_json(response).await.to_string().contains("store"));

    let req = Request::builder()
        .method("DELETE")
        .uri("/api/v1/admin/tenant")
        .header("authorization", format!("Bearer {}", tenant.secret_token))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.clone().oneshot(req).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    let response = router
        .clone()
        .oneshot(retire_request(wallet_id))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let retired_at = body_json(response).await["retired_at"].as_i64().unwrap();
    assert_eq!(
        wallet_status(&router, wallet_id).await["retired_at"],
        retired_at
    );
    assert_eq!(
        router
            .clone()
            .oneshot(retire_request(wallet_id))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );

    // No copy of the keys is left: not the wallet's, not the deleted store's.
    {
        let s = store.lock();
        assert!(s
            .get_wallet(wallet_id)
            .unwrap()
            .unwrap()
            .sealed_key_material
            .is_empty());
        let holders: Vec<Vec<u8>> = s
            .conn_for_test()
            .prepare("SELECT sealed_key_material FROM tenants WHERE wallet_id = ?1")
            .unwrap()
            .query_map([wallet_id], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(holders.len(), 1);
        assert!(holders.iter().all(Vec::is_empty), "{holders:?}");
    }
    let req = json_request(
        "POST",
        "/api/v1/admin/tenants",
        None,
        &serde_json::json!({ "wallet_id": wallet_id }),
    );
    assert_eq!(
        router.clone().oneshot(req).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );

    let restore = |seed: u8| {
        json_request(
            "POST",
            &format!("/api/v1/admin/wallets/{wallet_id}/restore"),
            None,
            &serde_json::json!({
                "view_key_hex": valid_view_key_hex(seed),
                "spend_pubkey_hex": valid_spend_pubkey_hex(seed.wrapping_add(1)),
            }),
        )
    };
    let response = router.clone().oneshot(restore(7)).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "another wallet's keys"
    );
    let response = router.clone().oneshot(restore(42)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await["primary_address"],
        wallet["primary_address"]
    );
    assert_eq!(
        wallet_status(&router, wallet_id).await["retired_at"],
        serde_json::Value::Null
    );
    let again = create_tenant_on(&router, wallet_id).await;
    assert!(!order_address(&router, &again).await.is_empty());
    assert_eq!(
        router.clone().oneshot(restore(42)).await.unwrap().status(),
        StatusCode::NOT_FOUND,
        "only a retired wallet is brought back"
    );
}

#[tokio::test]
async fn a_store_on_a_wallet_refuses_keys_too_an_unknown_wallet_and_another_network() {
    let router = test_router();
    let wallet = create_wallet(&router, 43).await;
    let wallet_id = wallet["wallet_id"].as_str().unwrap();
    for body in [
        serde_json::json!({
            "wallet_id": wallet_id,
            "view_key_hex": valid_view_key_hex(44),
            "spend_pubkey_hex": valid_spend_pubkey_hex(45),
        }),
        serde_json::json!({ "wallet_id": "wl_nope" }),
        serde_json::json!({ "wallet_id": wallet_id, "network": "stagenet" }),
    ] {
        let req = json_request("POST", "/api/v1/admin/tenants", None, &body);
        let response = router.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
    }
}

#[tokio::test]
async fn adding_a_wallet_with_bad_keys_is_refused_and_registers_nothing() {
    let state = AppState::for_tests();
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let req = json_request(
        "POST",
        "/api/v1/admin/wallets",
        None,
        &serde_json::json!({ "view_key_hex": "zz", "spend_pubkey_hex": "00" }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(store.lock().count_tenants().unwrap(), 0);
}

// -- Changing a store's wallet (migration 0029) -----------------------------

async fn change_wallet(
    router: &Router,
    tenant: &TestTenant,
    wallet_id: &str,
) -> axum::response::Response {
    let req = json_request(
        "PUT",
        "/api/v1/admin/tenant/wallet",
        Some(&tenant.secret_token),
        &serde_json::json!({ "wallet_id": wallet_id }),
    );
    router.clone().oneshot(req).await.unwrap()
}

async fn new_order(router: &Router, tenant: &TestTenant) -> serde_json::Value {
    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        &serde_json::json!({ "xmr_amount_piconero": 1u64 }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

fn tenant_row(store: &crate::store::SharedStore, tenant: &TestTenant) -> crate::store::Tenant {
    store
        .lock()
        .find_tenant_by_secret_token(&shared::auth::RawToken::presented(&tenant.secret_token))
        .unwrap()
        .unwrap()
}

/// A store changes wallet with an order still open. A payment to that order
/// arrives after the change: it is still found, with the old wallet's keys,
/// and credited to that order, not to the store's first order on the new
/// wallet, which has the same subaddress index there.
#[tokio::test]
async fn an_order_from_before_a_wallet_change_is_still_paid_and_a_new_order_with_its_index_is_not()
{
    use monero::cryptonote::hash::Hashable as _;
    let (state, daemon) = test_app_state_with_real_daemon().await;
    let store = Arc::clone(state.db.shared_store_for_test());
    let handles = Arc::clone(&state.custody.wallet_handles);
    let router = build_router(state, 1_000_000);
    // `subaddress_tx.hex` pays this wallet's subaddress 0/1: the first order.
    let tenant = create_fixture_tenant(&router).await;
    let before = new_order(&router, &tenant).await;
    let old_wallet = tenant_row(&store, &tenant).wallet_id;
    let new_wallet = create_wallet(&router, 50).await;
    let new_wallet_id = new_wallet["wallet_id"].as_str().unwrap();

    let response = change_wallet(&router, &tenant, new_wallet_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["orders_on_previous_wallet"], 1);
    assert_eq!(body["tenant"]["wallet_id"], new_wallet_id);
    assert_eq!(
        body["tenant"]["primary_address"],
        new_wallet["primary_address"]
    );

    // The new wallet's counter: index 1 again, so a new address.
    let after = new_order(&router, &tenant).await;
    assert_ne!(after["address"], before["address"]);
    let row = tenant_row(&store, &tenant);
    let watchers = store.lock().watchers_of(&row.id).unwrap();
    assert_eq!(watchers.len(), 1);
    let watcher = &watchers[0];
    assert_eq!(watcher.wallet_id, old_wallet);
    {
        let s = store.lock();
        let before_id = shared::ids::OrderId::new(before["order_id"].as_str().unwrap().to_owned());
        let after_id = shared::ids::OrderId::new(after["order_id"].as_str().unwrap().to_owned());
        assert_eq!(
            s.find_order_by_minor_index(&watcher.id, 1)
                .unwrap()
                .unwrap()
                .id,
            before_id
        );
        assert_eq!(
            s.find_order_by_minor_index(&row.id, 1).unwrap().unwrap().id,
            after_id
        );
        // Each row's scan window: its own orders.
        let windows = s
            .scan_windows(&[row.id.clone(), watcher.id.clone()], crate::now_unix(), 0)
            .unwrap();
        assert_eq!(windows[&row.id], vec![1]);
        assert_eq!(windows[&watcher.id], vec![1]);
    }
    // The store has the new keys; the scan row the old ones.
    assert!(handles.read().contains_key(&row.id));
    assert!(handles.read().contains_key(&watcher.id));
    // A scan row is no store: not counted, never called as.
    assert_eq!(store.lock().count_tenants().unwrap(), 1);

    let tx = fixture_tx_for_lookup_tests();
    daemon.set_mempool(vec![tx.clone()]);
    let txid = hex::encode(tx.hash().to_bytes());
    let response = router
        .clone()
        .oneshot(lookup_request(&tenant.secret_token, &txid))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["outcome"], "matched");
    assert_eq!(body["order_ids"], serde_json::json!([before["order_id"]]));
    let s = store.lock();
    let paid = |order: &serde_json::Value| {
        s.get_all_payments(&shared::ids::OrderId::new(
            order["order_id"].as_str().unwrap().to_owned(),
        ))
        .unwrap()
        .len()
    };
    assert_eq!(paid(&before), 1);
    assert_eq!(
        paid(&after),
        0,
        "the new wallet's order at the same index was not paid"
    );
}

/// A scan that began before the change may have used the old keys for the
/// store: what it found isn't recorded against the store, where an index
/// can now name an order on the new wallet.
#[tokio::test]
async fn a_scan_from_before_a_wallet_change_is_not_recorded_against_the_store() {
    let (state, _daemon) = test_app_state_with_real_daemon().await;
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let tenant = create_fixture_tenant(&router).await;
    new_order(&router, &tenant).await;
    let began = crate::now_unix();
    let wallet = create_wallet(&router, 51).await;
    assert_eq!(
        change_wallet(&router, &tenant, wallet["wallet_id"].as_str().unwrap())
            .await
            .status(),
        StatusCode::OK
    );
    let after = new_order(&router, &tenant).await;
    let row = tenant_row(&store, &tenant);
    let found = crate::scanner::ScanResult {
        matches: vec![crate::key_custody::MatchedOutput {
            output_index: 0,
            subaddress_index: crate::key_custody::SubaddressIndex { major: 0, minor: 1 },
            amount_piconero: Some(1),
        }],
        txid: "aa".repeat(32),
        key_images_json: "[]".into(),
        output_keys: HashMap::new(),
    };
    let locked = store.lock();
    let before_change =
        crate::scanner::record_scan_match(&locked, &row.id, &found, began, None).unwrap();
    assert!(before_change.is_empty());
    let order_id = shared::ids::OrderId::new(after["order_id"].as_str().unwrap().to_owned());
    assert!(locked.get_all_payments(&order_id).unwrap().is_empty());
    // A scan begun after the change is the store's own.
    let current =
        crate::scanner::record_scan_match(&locked, &row.id, &found, crate::now_unix() + 1, None)
            .unwrap();
    assert_eq!(current.into_iter().collect::<Vec<_>>(), vec![order_id]);
}

#[tokio::test]
async fn a_wallet_change_is_refused_to_the_same_wallet_an_unknown_one_or_another_network() {
    let state = AppState::for_tests();
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 52).await;
    let own = tenant_row(&store, &tenant).wallet_id;

    let response = change_wallet(&router, &tenant, &own).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        change_wallet(&router, &tenant, "wl_nope").await.status(),
        StatusCode::NOT_FOUND
    );

    let stagenet = store
        .lock()
        .create_wallet(
            &crate::store::NewWallet {
                key_custody_backend: "plain".into(),
                sealed_key_material: vec![1],
                primary_address: "5stagenet".into(),
                network: "stagenet".into(),
            },
            crate::now_unix(),
        )
        .unwrap();
    let response = change_wallet(&router, &tenant, &stagenet.id).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = body_json(response).await;
    assert!(body.to_string().contains("network"), "{body}");
    assert_eq!(
        tenant_row(&store, &tenant).wallet_id,
        own,
        "nothing changed"
    );
    assert!({
        let id = tenant_row(&store, &tenant).id;
        store.lock().watchers_of(&id)
    }
    .unwrap()
    .is_empty());

    // Without a store's token, nothing.
    let req = json_request(
        "PUT",
        "/api/v1/admin/tenant/wallet",
        None,
        &serde_json::json!({ "wallet_id": own }),
    );
    assert_eq!(
        router.oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

/// The wallet a store left can't be deleted while an order on it can still
/// be paid; once none can, it can, and the keys watching it go.
#[tokio::test]
async fn a_wallet_left_with_open_orders_is_only_retired_once_they_closed() {
    let state = AppState::for_tests();
    let store = Arc::clone(state.db.shared_store_for_test());
    let handles = Arc::clone(&state.custody.wallet_handles);
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 53).await;
    let order = new_order(&router, &tenant).await;
    let old_wallet = tenant_row(&store, &tenant).wallet_id;
    let new_wallet = create_wallet(&router, 54).await;
    assert_eq!(
        change_wallet(&router, &tenant, new_wallet["wallet_id"].as_str().unwrap())
            .await
            .status(),
        StatusCode::OK
    );
    let watcher = {
        let id = tenant_row(&store, &tenant).id;
        store.lock().watchers_of(&id)
    }
    .unwrap()
    .remove(0);

    let delete = || retire_request(&old_wallet);
    let response = router.clone().oneshot(delete()).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(body_json(response).await.to_string().contains("order"));
    let status = wallet_status(&router, &old_wallet).await;
    assert_eq!(
        (status["stores"].as_u64(), status["payable_orders"].as_u64()),
        (Some(0), Some(1))
    );
    assert!(status["payable_until"].as_i64().is_some());

    // Closed long ago: past the grace period, nothing can pay it now.
    store
        .lock()
        .execute_raw_for_test(&format!(
            "UPDATE orders SET status = 'expired', closed_at_utc = 1 WHERE id = '{}'",
            order["order_id"].as_str().unwrap()
        ))
        .unwrap();
    assert_eq!(
        router.clone().oneshot(delete()).await.unwrap().status(),
        StatusCode::OK
    );
    assert!({
        let id = tenant_row(&store, &tenant).id;
        store.lock().watchers_of(&id)
    }
    .unwrap()
    .is_empty());
    assert!(
        !handles.read().contains_key(&watcher.id),
        "the old keys left key custody"
    );
}

/// Back and forth: a store returning to a wallet it left, and leaving it
/// again, uses the one scan row for its orders there, whose indices stay
/// unique since they come from that wallet's one counter.
#[tokio::test]
async fn a_store_changing_back_and_forth_keeps_one_scan_row_per_wallet_it_left() {
    let state = AppState::for_tests();
    let store = Arc::clone(state.db.shared_store_for_test());
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 55).await;
    let first = tenant_row(&store, &tenant).wallet_id;
    let second = create_wallet(&router, 56).await["wallet_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut addresses = vec![new_order(&router, &tenant).await["address"].clone()];
    for wallet in [&second, &first, &second, &first] {
        assert_eq!(
            change_wallet(&router, &tenant, wallet).await.status(),
            StatusCode::OK
        );
        addresses.push(new_order(&router, &tenant).await["address"].clone());
    }
    let mut unique = addresses.clone();
    unique.sort_by_key(ToString::to_string);
    unique.dedup();
    assert_eq!(
        unique.len(),
        addresses.len(),
        "an address was handed out twice"
    );
    let row = tenant_row(&store, &tenant);
    let watchers = store.lock().watchers_of(&row.id).unwrap();
    let mut wallets: Vec<_> = watchers.iter().map(|w| w.wallet_id.clone()).collect();
    wallets.sort();
    let mut expected = vec![first.clone(), second.clone()];
    expected.sort();
    assert_eq!(wallets, expected, "one scan row per wallet left");
    assert_eq!(row.wallet_id, first);
    // Every order the store took is watched by the row holding its wallet.
    let s = store.lock();
    let windows = s
        .scan_windows(
            &std::iter::once(row.id.clone())
                .chain(watchers.iter().map(|w| w.id.clone()))
                .collect::<Vec<_>>(),
            crate::now_unix(),
            0,
        )
        .unwrap();
    let watched: usize = windows.values().map(Vec::len).sum();
    assert_eq!(watched, addresses.len());
    assert_eq!(
        windows[&row.id].len(),
        1,
        "the store watches only what it took since the last change"
    );
}
