//! HTTP-layer integration tests, driven through the real `Router` via
//! `tower::ServiceExt::oneshot` - no bound socket needed. These exercise the same
//! IDOR/auth properties `store.rs` already tests at the repository level, but end to
//! end through real request parsing, auth extraction, and JSON (de)serialization,
//! per `docs/TESTING.md` §5.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Arc, RwLock};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use monero::consensus::encode::deserialize;
use monero::{Network, PrivateKey, PublicKey, Transaction};
use tower::ServiceExt;

use crate::daemon::fake::FakeDaemonClient;
use crate::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use crate::key_custody::{KeyCustody, PlainKeyCustody};
use crate::scanner_status::new_scanner_status_map;
use crate::store::Store;

use super::rate_limit::RateLimiter;
use super::{AppState, build_router};

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

fn test_app_state() -> AppState {
    let store = Store::open_in_memory().unwrap().into_shared();
    let key_custody: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
    // A real (fake-backed, but genuinely `MoneroDaemonClient`-implementing)
    // node for mainnet - matches `configured_networks` below, and gives
    // `status_page`'s own tests something real to query rather than an
    // empty map that would make every test tenant's own network
    // inconsistent with what `daemons` actually has.
    let mainnet_daemon = Arc::new(FallbackDaemonClient::new(vec![FallbackNode {
        label: "fake-node:18081".to_string(),
        client: Arc::new(FakeDaemonClient::new()),
    }]));
    AppState {
        store,
        key_custody,
        key_custody_backend: "plain".to_string(),
        wallet_handles: Arc::new(RwLock::new(HashMap::new())),
        // Every test tenant is created without an explicit `network`, which
        // defaults to mainnet (see admin::create_tenant) - so mainnet must be
        // "configured" for tenant creation to succeed in these tests.
        configured_networks: Arc::new(HashSet::from([Network::Mainnet])),
        // Generous by default so the auth/IDOR/order-flow tests below aren't
        // incidentally affected by rate limiting - the middleware's own behavior is
        // tested separately, end to end, in `rate_limit_middleware_rejects_after_the_limit_with_a_real_connect_info`.
        admin_rate_limiter: Arc::new(RateLimiter::new(10_000)),
        daemons: Arc::new(HashMap::from([(Network::Mainnet, mainnet_daemon)])),
        scanner_status: new_scanner_status_map(),
        scan_poll_interval_secs: 2,
        expired_order_grace_period_seconds: 21_600,
    }
}

fn test_router() -> Router {
    build_router(test_app_state(), 1_000_000)
}

fn json_request(method: &str, uri: &str, bearer: Option<&str>, origin: Option<&str>, body: serde_json::Value) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri).header("content-type", "application/json");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
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
        None,
        serde_json::json!({
            "view_key_hex": valid_view_key_hex(seed),
            "spend_pubkey_hex": valid_spend_pubkey_hex(seed.wrapping_add(1)),
        }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    TestTenant {
        public_key: body["public_key"].as_str().unwrap().to_string(),
        secret_token: body["secret_token"].as_str().unwrap().to_string(),
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
        None,
        serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64 }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["xmr_amount_piconero"], 167_500_000_000u64);
    let order_id = body["order_id"].as_str().unwrap().to_string();
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
    let state = test_app_state();
    let store = state.store.clone();
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;

    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        None,
        serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64, "confirmations_required": 3 }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let order_id = body_json(response).await["order_id"].as_str().unwrap().to_string();

    let guard = store.lock().unwrap();
    let tenant_id = guard.find_tenant_by_public_key(&tenant.public_key).unwrap().unwrap().id;
    let order = guard.get_order(&tenant_id, &order_id).unwrap().unwrap();
    assert_eq!(order.confirmations_required_override, Some(3));
}

#[tokio::test]
async fn creating_an_order_with_no_confirmations_required_override_leaves_it_unset() {
    let state = test_app_state();
    let store = state.store.clone();
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;

    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        None,
        serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64 }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let order_id = body_json(response).await["order_id"].as_str().unwrap().to_string();

    let guard = store.lock().unwrap();
    let tenant_id = guard.find_tenant_by_public_key(&tenant.public_key).unwrap().unwrap().id;
    let order = guard.get_order(&tenant_id, &order_id).unwrap().unwrap();
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
        None,
        serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64, "confirmations_required": 721u64 }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST, "confirmations_required=721 must be rejected");
}

#[tokio::test]
async fn creating_an_order_with_confirmations_required_zero_is_accepted() {
    let state = test_app_state();
    let store = state.store.clone();
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;

    let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        None,
        serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64, "confirmations_required": 0u64 }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let order_id = body_json(response).await["order_id"].as_str().unwrap().to_string();

    let guard = store.lock().unwrap();
    let tenant_id = guard.find_tenant_by_public_key(&tenant.public_key).unwrap().unwrap().id;
    let order = guard.get_order(&tenant_id, &order_id).unwrap().unwrap();
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
async fn create_tenant_rejects_a_syntactically_valid_but_off_curve_spend_pubkey_as_bad_request_not_internal_error() {
    let router = test_router();
    let req = json_request(
        "POST",
        "/api/v1/admin/tenants",
        None,
        None,
        serde_json::json!({
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
    assert!(message.contains("spend public key"), "expected the real validation message, got: {message}");
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
        None,
        serde_json::json!({
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
    assert!(message.contains("view key"), "expected the real validation message, got: {message}");
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
    let state = test_app_state();
    let store = state.store.clone();
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;

    let mut addresses = Vec::new();
    for _ in 0..3 {
        let req = json_request(
        "POST",
        "/api/v1/admin/tenant/orders",
        Some(&tenant.secret_token),
        None,
        serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64 }),
        );
        let response = router.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        addresses.push(body_json(response).await["address"].as_str().unwrap().to_string());
    }

    let mut deduped = addresses.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(deduped.len(), 3, "every order must be issued its own subaddress");

    let s = store.lock().unwrap();
    let tenant_row = s.find_tenant_by_secret_token(&tenant.secret_token).unwrap().unwrap();
    assert_eq!(
        tenant_row.next_minor_index, 4,
        "indices 1..3 were issued, so the counter must sit at exactly 4 - no gaps for addresses nobody holds"
    );
    // The scanner scans `0..next_minor_index`, so every index the counter covers
    // must resolve to a real order - that is precisely the invariant whose violation
    // let a matched payment be dropped.
    for minor in 1..tenant_row.next_minor_index {
        assert!(
            s.find_order_by_minor_index(&tenant_row.id, minor).unwrap().is_some(),
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
    let req = Request::builder().method("GET").uri("/api/v1/admin/tenant").body(Body::empty()).unwrap();
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
        None,
        serde_json::json!({ "xmr_amount_piconero": 67_000_000_000u64 }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    let body = body_json(response).await;
    let order_b_order_id = body["order_id"].as_str().unwrap().to_string();

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
    let new_secret = body["secret_token"].as_str().unwrap().to_string();

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/admin/tenant")
        .header("authorization", format!("Bearer {}", tenant.secret_token))
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "the pre-rotation token must stop working immediately");

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
        ("POST", format!("/api/v1/t/{pk}/orders/{order_id}/refund-address")),
    ] {
        let req = json_request(method, &uri, None, None, serde_json::json!({ "xmr_amount_piconero": 1u64, "refund_address": "x" }));
        let response = router.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {uri} must not exist");
    }

    for uri in [
        format!("/api/v1/t/{pk}/orders"),
        "/api/v1/admin/tenant/orders".to_string(),
        "/api/v1/admin/tenants".to_string(),
        "/status".to_string(),
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
        assert!(response.headers().get("access-control-allow-origin").is_none(), "{uri} must not grant CORS");
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
        None,
        serde_json::json!({ "url": "https://b.example/hook" }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let webhook_id = body["webhook_id"].as_str().unwrap().to_string();
    assert!(body["signing_secret"].as_str().unwrap().starts_with("whsec_"));

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
        None,
        serde_json::json!({ "xmr_amount_piconero": 0u64 }),
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
        None,
        serde_json::json!({ "xmr_amount_piconero": "not_a_number" }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn tenant_creation_is_rejected_for_a_network_with_no_configured_node() {
    // The failure mode this check exists to prevent: a tenant whose address gets
    // derived for a chain nothing on this instance is actually scanning, so a
    // real payment to it would simply never be detected - a silent, much worse
    // failure than refusing to create the tenant at all.
    let router = test_router(); // only mainnet is configured, see test_app_state()

    let req = json_request(
        "POST",
        "/api/v1/admin/tenants",
        None,
        None,
        serde_json::json!({
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
        None,
        serde_json::json!({
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
        None,
        serde_json::json!({ "xmr_amount_piconero": 67_000_000_000u64 }),
    );
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unauthenticated_routes_are_limited_per_address_by_the_admin_limiter() {
    // Tenant creation and `/status` carry no token, so the admin limiter keys
    // them on the caller's address (with a fabricated `ConnectInfo`, the way
    // production's `into_make_service_with_connect_info` provides it).
    let mut state = test_app_state();
    state.admin_rate_limiter = Arc::new(RateLimiter::new(2));
    let router = build_router(state, 1_000_000);

    let peer: std::net::SocketAddr = "10.0.0.1:12345".parse().unwrap();
    let make_request = || {
        let mut req = Request::builder().method("GET").uri("/status").body(Body::empty()).unwrap();
        req.extensions_mut().insert(axum::extract::ConnectInfo(peer));
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
    let mut state = test_app_state();
    state.admin_rate_limiter = Arc::new(RateLimiter::new(2));
    let router = build_router(state, 1_000_000);

    let peer: std::net::SocketAddr = "10.0.0.2:12345".parse().unwrap();
    let make_request = || {
        let mut req = Request::builder().method("GET").uri("/api/v1/admin/tenant").body(Body::empty()).unwrap();
        req.extensions_mut().insert(axum::extract::ConnectInfo(peer));
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
    let mut state = test_app_state();
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
        req.extensions_mut().insert(axum::extract::ConnectInfo(peer));
        req
    };

    // Same IP, two different (unknown, so 401 not 200) tokens - both get through
    // the rate limiter itself, each consuming its own budget of 1.
    let r1 = router.clone().oneshot(make_request("sk_tenant_one")).await.unwrap();
    let r2 = router.clone().oneshot(make_request("sk_tenant_two")).await.unwrap();
    assert_eq!(r1.status(), StatusCode::UNAUTHORIZED, "tenant one's first request must not be rate-limited");
    assert_eq!(r2.status(), StatusCode::UNAUTHORIZED, "tenant two's own budget must be independent of tenant one's");

    // Tenant one's *second* request, same IP, is over its own budget of 1.
    let r3 = router.oneshot(make_request("sk_tenant_one")).await.unwrap();
    assert_eq!(r3.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn oversized_request_body_is_rejected_before_reaching_the_handler() {
    let router = build_router(test_app_state(), 16); // absurdly small cap for the test

    let oversized_body = serde_json::json!({ "xmr_amount_piconero": 67_000_000_000u64 }).to_string();
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
    assert!(address.starts_with('4'), "primary_address {address} is not a standard mainnet address");
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
            .oneshot(json_request("POST", "/api/v1/admin/tenants", None, None, create_body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "creation accepted {bad}");
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
            .oneshot(json_request("PATCH", "/api/v1/admin/tenant", Some(&tenant.secret_token), None, bad.clone()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "patch accepted {bad}");
    }

    // Sane values on both paths still go through, so the checks aren't just
    // rejecting everything.
    let response = router
        .clone()
        .oneshot(json_request(
            "PATCH",
            "/api/v1/admin/tenant",
            Some(&tenant.secret_token),
            None,
            serde_json::json!({ "confirmations_required": 3, "order_expiry_seconds": 900 }),
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
            None,
            serde_json::json!({ "confirmations_required": 0 }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "confirmations_required=0 must be accepted on patch");
    assert_eq!(body_json(response).await["confirmations_required"], 0);

    let create_body = serde_json::json!({
        "view_key_hex": valid_view_key_hex(43),
        "spend_pubkey_hex": valid_spend_pubkey_hex(44),
        "confirmations_required": 0,
    });
    let response =
        router.clone().oneshot(json_request("POST", "/api/v1/admin/tenants", None, None, create_body)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "confirmations_required=0 must be accepted on creation");
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
                None,
                serde_json::json!({ "xmr_amount_piconero": 1_000_000u64 }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let order_id = order["order_id"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/admin/tenant/orders/{order_id}/refund-address");
    let body = serde_json::json!({ "refund_address": "refund-here" });

    let no_key = router.clone().oneshot(json_request("POST", &uri, None, None, body.clone())).await.unwrap();
    assert_eq!(no_key.status(), StatusCode::UNAUTHORIZED);

    let other_tenant =
        router.clone().oneshot(json_request("POST", &uri, Some(&b.secret_token), None, body.clone())).await.unwrap();
    assert_eq!(other_tenant.status(), StatusCode::NOT_FOUND, "tenant B must not reach tenant A's order");

    let owner = router.clone().oneshot(json_request("POST", &uri, Some(&a.secret_token), None, body)).await.unwrap();
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
        None,
        serde_json::json!({ "xmr_amount_piconero": 167_500_000_000u64 }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let a_order_id = order["order_id"].as_str().unwrap().to_string();
    let a_webhook = body_json(
        router
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/admin/tenant/webhooks",
                Some(&a.secret_token),
                None,
                serde_json::json!({ "url": "https://a.example/hook" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let a_webhook_id = a_webhook["webhook_id"].as_str().unwrap().to_string();

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

    // The rescan family (`docs/order_rescan_wbs.md` Phase 2) - both the trigger and
    // the status route resolve their order the same tenant-scoped way `get_order`
    // does, so they get the same IDOR check as every other order-scoped route above.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/admin/tenant/orders/{a_order_id}/rescan"))
                .header("authorization", format!("Bearer {}", b.secret_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = router
        .clone()
        .oneshot(json_request(
            "POST",
            &format!("/api/v1/admin/tenant/orders/{a_order_id}/rescan"),
            Some(&b.secret_token),
            None,
            serde_json::json!({ "mode": "simple" }),
        ))
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
            None,
            serde_json::json!({ "tenant_id": "whatever" }),
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
                None,
                serde_json::json!({ "url": bad_url }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "accepted {bad_url:?}");
    }

    // The tenant-patch route must not offer a back door for any webhook field.
    let response = router
        .clone()
        .oneshot(json_request(
            "PATCH",
            "/api/v1/admin/tenant",
            Some(&tenant.secret_token),
            None,
            serde_json::json!({ "webhook_url": "file:///etc/passwd", "url": "file:///etc/passwd" }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "unknown fields are ignored, not applied");
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
    assert_eq!(webhooks.as_array().unwrap().len(), 0, "no webhook may have been created by a patch");
}

async fn get_status_json(router: Router) -> serde_json::Value {
    let response = router.oneshot(Request::builder().method("GET").uri("/status").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

#[tokio::test]
async fn status_endpoint_is_reachable_with_no_authentication_at_all() {
    // Deliberately no `authorization` header, no session cookie - see
    // `build_router`'s own doc comment on why this route is unauthenticated.
    let router = build_router(test_app_state(), 1_000_000);
    let body = get_status_json(router).await;
    assert!(body["networks"].is_array());
}

#[tokio::test]
async fn status_endpoint_shows_the_real_configured_network_and_node_with_its_live_height() {
    let router = build_router(test_app_state(), 1_000_000);
    let body = get_status_json(router).await;
    let network = &body["networks"][0];
    assert_eq!(network["network"], "mainnet", "expected the configured network shown, got: {body}");
    let node = &network["nodes"][0];
    assert_eq!(node["label"], "fake-node:18081", "expected the real node label shown, got: {body}");
    // FakeDaemonClient::new() starts at height 0 - a real, live query result,
    // not a placeholder, and must not be confused with "unknown" (null).
    assert_eq!(node["height"], 0, "expected the node's real live height shown, got: {body}");
    assert!(node["error"].is_null(), "a reachable node must have no error, got: {body}");
    assert!(!network["scanner"]["ever_ticked"].as_bool().unwrap(), "no scan tick has happened in this test, got: {body}");
}

#[tokio::test]
async fn status_endpoint_shows_an_offline_node_as_an_error_not_a_silent_gap() {
    let mut state = test_app_state();
    let offline_daemon = Arc::new(FallbackDaemonClient::new(vec![FallbackNode {
        label: "dead-node:18081".to_string(),
        client: Arc::new(FakeDaemonClient::default()), // starts offline (see FakeDaemonClient::new vs. Default)
    }]));
    state.daemons = Arc::new(HashMap::from([(Network::Mainnet, offline_daemon)]));
    let router = build_router(state, 1_000_000);
    let body = get_status_json(router).await;
    let node = &body["networks"][0]["nodes"][0];
    assert_eq!(node["label"], "dead-node:18081");
    assert!(node["height"].is_null(), "expected no height for an offline node, got: {body}");
    let error = node["error"].as_str().expect("expected a real error message");
    assert!(error.contains("fake daemon is offline"), "expected the real error message surfaced, got: {error}");
}

#[tokio::test]
async fn status_endpoint_reflects_a_healthy_recent_scan_tick() {
    let state = test_app_state();
    crate::scanner_status::record_tick(&state.scanner_status, Network::Mainnet, crate::now_unix(), crate::now_unix(), 3, &Ok::<(), String>(()));
    let router = build_router(state, 1_000_000);
    let body = get_status_json(router).await;
    let scanner = &body["networks"][0]["scanner"];
    assert!(scanner["ever_ticked"].as_bool().unwrap());
    assert!(scanner["last_tick_ok"].as_bool().unwrap(), "expected a healthy fresh successful tick, got: {body}");
    assert!(!scanner["is_stale"].as_bool().unwrap());
    assert_eq!(scanner["tenants_scanned"], 3, "expected the real tenants-scanned count shown, got: {body}");
}

#[tokio::test]
async fn status_endpoint_reflects_a_failing_scan_tick_with_its_real_error() {
    let state = test_app_state();
    crate::scanner_status::record_tick(
        &state.scanner_status,
        Network::Mainnet,
        crate::now_unix(),
        crate::now_unix(),
        1,
        &Err::<(), String>("node returned garbage".to_string()),
    );
    let router = build_router(state, 1_000_000);
    let body = get_status_json(router).await;
    let scanner = &body["networks"][0]["scanner"];
    assert!(!scanner["last_tick_ok"].as_bool().unwrap(), "expected the real failing-tick state, got: {body}");
    assert_eq!(scanner["last_error"], "node returned garbage", "expected the real error message surfaced, got: {body}");
}

#[tokio::test]
async fn status_endpoint_reflects_a_stale_scanner_that_has_stopped_ticking() {
    let state = test_app_state();
    // A tick that "succeeded" a very long time ago - the scanner itself is
    // the thing that's actually broken here (stopped ticking at all), which
    // must read differently from a merely-failing-but-alive tick.
    crate::scanner_status::record_tick(&state.scanner_status, Network::Mainnet, 1, 1, 2, &Ok::<(), String>(()));
    let router = build_router(state, 1_000_000);
    let body = get_status_json(router).await;
    let scanner = &body["networks"][0]["scanner"];
    assert!(scanner["is_stale"].as_bool().unwrap(), "expected stale once a tick is far older than the poll interval, got: {body}");
}

#[tokio::test]
async fn status_endpoint_with_no_configured_networks_says_so_plainly() {
    let mut state = test_app_state();
    state.daemons = Arc::new(HashMap::new());
    let router = build_router(state, 1_000_000);
    let body = get_status_json(router).await;
    assert_eq!(body["networks"].as_array().unwrap().len(), 0);
}

/// Like `test_app_state`, but hands back the mainnet `FakeDaemonClient` directly so
/// a test can script real, findable block heights/transactions - `test_app_state`'s
/// own daemon starts with no blocks at all, which is fine for most tests here but
/// not for these.
fn test_app_state_with_real_daemon() -> (AppState, Arc<FakeDaemonClient>) {
    let store = Store::open_in_memory().unwrap().into_shared();
    let key_custody: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
    let fake_daemon = Arc::new(FakeDaemonClient::new());
    let mainnet_daemon = Arc::new(FallbackDaemonClient::new(vec![FallbackNode {
        label: "fake-node:18081".to_string(),
        client: fake_daemon.clone(),
    }]));
    let state = AppState {
        store,
        key_custody,
        key_custody_backend: "plain".to_string(),
        wallet_handles: Arc::new(RwLock::new(HashMap::new())),
        configured_networks: Arc::new(HashSet::from([Network::Mainnet])),
        admin_rate_limiter: Arc::new(RateLimiter::new(10_000)),
        daemons: Arc::new(HashMap::from([(Network::Mainnet, mainnet_daemon)])),
        scanner_status: new_scanner_status_map(),
        scan_poll_interval_secs: 2,
        expired_order_grace_period_seconds: 21_600,
    };
    (state, fake_daemon)
}

// -- Instance admin settings API -------------------------------------------

fn settings_request(method: &str, bearer: Option<&str>, body: Option<serde_json::Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri("/api/v1/admin/settings").header("content-type", "application/json");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(body.map(|b| Body::from(b.to_string())).unwrap_or(Body::empty())).unwrap()
}

#[tokio::test]
async fn instance_admin_settings_requires_a_bearer_token_at_all() {
    let (state, _daemon) = test_app_state_with_real_daemon();
    crate::http::instance_admin::seed_admin_token_for_tests(&state.store.lock().unwrap(), "admin_test_token");
    let router = build_router(state, 1_000_000);
    let response = router.oneshot(settings_request("GET", None, None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_tenants_own_secret_token_cannot_authenticate_as_the_instance_admin() {
    let (state, _daemon) = test_app_state_with_real_daemon();
    crate::http::instance_admin::seed_admin_token_for_tests(&state.store.lock().unwrap(), "admin_test_token");
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;
    let response = router.oneshot(settings_request("GET", Some(&tenant.secret_token), None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "a tenant's own sk_ must never satisfy the instance-wide admin API");
}

#[tokio::test]
async fn get_settings_reports_code_defaults_when_nothing_is_configured() {
    let (state, _daemon) = test_app_state_with_real_daemon();
    crate::http::instance_admin::seed_admin_token_for_tests(&state.store.lock().unwrap(), "admin_test_token");
    let router = build_router(state, 1_000_000);
    let response = router.oneshot(settings_request("GET", Some("admin_test_token"), None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["scalars"]["payment.confirmations_required"]["value"], "10");
    assert_eq!(body["scalars"]["payment.confirmations_required"]["source"], "default");
    assert_eq!(body["monero_node"]["mainnet"], serde_json::Value::Null, "an unconfigured network reports null, not a fabricated node");
}

#[tokio::test]
async fn updating_a_scalar_setting_persists_and_a_later_get_reflects_it() {
    let (state, _daemon) = test_app_state_with_real_daemon();
    crate::http::instance_admin::seed_admin_token_for_tests(&state.store.lock().unwrap(), "admin_test_token");
    let router = build_router(state, 1_000_000);

    let post = router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some("admin_test_token"),
            Some(serde_json::json!({ "scalars": { "payment.confirmations_required": "3" } })),
        ))
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::OK, "expected the save to succeed, got: {:?}", body_json(post).await);

    let get = router.oneshot(settings_request("GET", Some("admin_test_token"), None)).await.unwrap();
    let body = body_json(get).await;
    assert_eq!(body["scalars"]["payment.confirmations_required"]["value"], "3");
    assert_eq!(body["scalars"]["payment.confirmations_required"]["source"], "database");
}

#[tokio::test]
async fn an_env_var_override_is_reported_as_effective_even_after_a_database_save() {
    let (state, _daemon) = test_app_state_with_real_daemon();
    crate::http::instance_admin::seed_admin_token_for_tests(&state.store.lock().unwrap(), "admin_test_token");
    let router = build_router(state, 1_000_000);

    router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some("admin_test_token"),
            Some(serde_json::json!({ "scalars": { "payment.confirmations_required": "3" } })),
        ))
        .await
        .unwrap();

    let env = shared::settings::test_env::set("SCANNER_PAYMENT_CONFIRMATIONS_REQUIRED", Some("99"));
    let get = router.oneshot(settings_request("GET", Some("admin_test_token"), None)).await.unwrap();
    drop(env);
    let body = body_json(get).await;
    assert_eq!(body["scalars"]["payment.confirmations_required"]["value"], "99");
    assert_eq!(body["scalars"]["payment.confirmations_required"]["source"], "env");
}

#[tokio::test]
async fn saving_an_out_of_range_scalar_is_rejected_and_nothing_changes() {
    let (state, _daemon) = test_app_state_with_real_daemon();
    crate::http::instance_admin::seed_admin_token_for_tests(&state.store.lock().unwrap(), "admin_test_token");
    let router = build_router(state, 1_000_000);

    // `0` is a legal value now (native 0-conf) - `1000` (over the 720 cap) is the
    // out-of-range example instead.
    let post = router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some("admin_test_token"),
            Some(serde_json::json!({ "scalars": { "payment.confirmations_required": "1000" } })),
        ))
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::BAD_REQUEST);

    let get = router.oneshot(settings_request("GET", Some("admin_test_token"), None)).await.unwrap();
    let body = body_json(get).await;
    assert_eq!(body["scalars"]["payment.confirmations_required"]["value"], "10", "the rejected save must not have taken effect");
}

#[tokio::test]
async fn a_partially_invalid_save_changes_nothing_not_just_the_valid_half() {
    let (state, _daemon) = test_app_state_with_real_daemon();
    crate::http::instance_admin::seed_admin_token_for_tests(&state.store.lock().unwrap(), "admin_test_token");
    let router = build_router(state, 1_000_000);

    let post = router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some("admin_test_token"),
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

    let get = router.oneshot(settings_request("GET", Some("admin_test_token"), None)).await.unwrap();
    let body = body_json(get).await;
    assert_eq!(body["scalars"]["payment.reorg_check_depth"]["value"], "20", "the valid field in the same request must not have been saved either");
}

#[tokio::test]
async fn socket_key_custody_backend_without_a_socket_path_is_rejected() {
    let (state, _daemon) = test_app_state_with_real_daemon();
    crate::http::instance_admin::seed_admin_token_for_tests(&state.store.lock().unwrap(), "admin_test_token");
    let router = build_router(state, 1_000_000);

    let post = router
        .oneshot(settings_request(
            "POST",
            Some("admin_test_token"),
            Some(serde_json::json!({ "scalars": { "key_custody.backend": "socket" } })),
        ))
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn socket_key_custody_backend_with_a_socket_path_in_the_same_request_succeeds() {
    let (state, _daemon) = test_app_state_with_real_daemon();
    crate::http::instance_admin::seed_admin_token_for_tests(&state.store.lock().unwrap(), "admin_test_token");
    let router = build_router(state, 1_000_000);

    let post = router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some("admin_test_token"),
            Some(serde_json::json!({
                "scalars": { "key_custody.backend": "socket", "key_custody.socket_path": "/run/moneropay/key-custody.sock" }
            })),
        ))
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::OK, "expected success, got: {:?}", body_json(post).await);

    let get = router.oneshot(settings_request("GET", Some("admin_test_token"), None)).await.unwrap();
    let body = body_json(get).await;
    assert_eq!(body["scalars"]["key_custody.backend"]["value"], "socket");
}

#[tokio::test]
async fn socket_key_custody_backend_using_an_already_saved_socket_path_succeeds() {
    // The cross-field check must consider the *merged* state, not just this one
    // request's own body - a caller flipping `backend` to "socket" in a request
    // that doesn't also repeat an already-saved `socket_path` must still succeed.
    let (state, _daemon) = test_app_state_with_real_daemon();
    crate::http::instance_admin::seed_admin_token_for_tests(&state.store.lock().unwrap(), "admin_test_token");
    let router = build_router(state, 1_000_000);

    router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some("admin_test_token"),
            Some(serde_json::json!({ "scalars": { "key_custody.socket_path": "/run/moneropay/key-custody.sock" } })),
        ))
        .await
        .unwrap();

    let post = router
        .oneshot(settings_request(
            "POST",
            Some("admin_test_token"),
            Some(serde_json::json!({ "scalars": { "key_custody.backend": "socket" } })),
        ))
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::OK, "expected success, got: {:?}", body_json(post).await);
}

#[tokio::test]
async fn setting_a_monero_node_round_trips_including_its_fallback_list() {
    let (state, _daemon) = test_app_state_with_real_daemon();
    crate::http::instance_admin::seed_admin_token_for_tests(&state.store.lock().unwrap(), "admin_test_token");
    let router = build_router(state, 1_000_000);

    let node = serde_json::json!({
        "host": "primary.example",
        "port": 18081,
        "ssl": false,
        "accept_self_signed_certs": true,
        "fallbacks": [
            { "host": "backup.example", "port": 18081, "ssl": true, "accept_self_signed_certs": false, "fallbacks": [] }
        ]
    });
    let post = router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some("admin_test_token"),
            Some(serde_json::json!({ "monero_node": { "mainnet": node.clone() } })),
        ))
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::OK, "expected success, got: {:?}", body_json(post).await);

    let get = router.oneshot(settings_request("GET", Some("admin_test_token"), None)).await.unwrap();
    let body = body_json(get).await;
    assert_eq!(body["monero_node"]["mainnet"], node);
}

#[tokio::test]
async fn clearing_a_monero_node_with_a_null_value_removes_its_configuration() {
    let (state, _daemon) = test_app_state_with_real_daemon();
    crate::http::instance_admin::seed_admin_token_for_tests(&state.store.lock().unwrap(), "admin_test_token");
    let router = build_router(state, 1_000_000);

    let node = serde_json::json!({ "host": "primary.example", "port": 18081, "ssl": false, "accept_self_signed_certs": true, "fallbacks": [] });
    router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some("admin_test_token"),
            Some(serde_json::json!({ "monero_node": { "mainnet": node } })),
        ))
        .await
        .unwrap();

    router
        .clone()
        .oneshot(settings_request(
            "POST",
            Some("admin_test_token"),
            Some(serde_json::json!({ "monero_node": { "mainnet": null } })),
        ))
        .await
        .unwrap();

    let get = router.oneshot(settings_request("GET", Some("admin_test_token"), None)).await.unwrap();
    let body = body_json(get).await;
    assert_eq!(body["monero_node"]["mainnet"], serde_json::Value::Null);
}

#[tokio::test]
async fn ensure_admin_token_seeded_generates_exactly_once_and_the_generated_token_authenticates() {
    let store = Store::open_in_memory().unwrap();
    let generated = crate::http::instance_admin::ensure_admin_token_seeded(&store).expect("a fresh database has no token yet");

    let state = AppState {
        store: std::sync::Arc::new(std::sync::Mutex::new(store)),
        key_custody: std::sync::Arc::new(PlainKeyCustody::default()),
        key_custody_backend: "plain".to_string(),
        wallet_handles: Arc::new(RwLock::new(HashMap::new())),
        configured_networks: Arc::new(HashSet::new()),
        admin_rate_limiter: Arc::new(RateLimiter::new(10_000)),
        daemons: Arc::new(HashMap::new()),
        scanner_status: new_scanner_status_map(),
        scan_poll_interval_secs: 2,
        expired_order_grace_period_seconds: 21_600,
    };
    let second_call = crate::http::instance_admin::ensure_admin_token_seeded(&state.store.lock().unwrap());
    assert_eq!(second_call, None, "a token that already exists must never be silently regenerated (that would invalidate the first one)");

    let router = build_router(state, 1_000_000);
    let response = router.oneshot(settings_request("GET", Some(&generated), None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "the freshly generated token must actually authenticate");
}

// -- Payment lookup by txid (`docs/txid_lookup_and_scan_chunking_wbs.md` Part B) --

fn lookup_request(token: &str, txid: &str) -> Request<Body> {
    json_request("POST", "/api/v1/admin/tenant/payments/lookup", Some(token), None, serde_json::json!({ "txid": txid }))
}

fn fixture_tx_for_lookup_tests() -> Transaction {
    let raw_tx = hex::decode(include_str!("../../tests/fixtures/subaddress_tx.hex")).unwrap();
    deserialize(&raw_tx).unwrap()
}

/// The real view/spend key pair `subaddress_tx.hex` actually pays (subaddress
/// 0/1) - duplicated from `scanner.rs`'s own private `fixture_view_key`/
/// `fixture_spend_pubkey` test helpers, which aren't reachable from this
/// module (`scanner::tests` is a private module). A `create_tenant`-issued
/// random per-seed key pair could never match this fixed fixture transaction,
/// and this crate's own convention keeps real crypto-matching correctness
/// tested at the scanner-level (`scanner.rs`'s own exhaustive suite) rather
/// than re-proven through the full HTTP stack - these two helpers exist only
/// so the one thing that's genuinely new here (this handler's own wiring of
/// already-proven primitives) gets one real, opt-in-if-you-want-it, true
/// end-to-end check too.
fn fixture_view_key_hex() -> String {
    hex::encode(
        PrivateKey::from_slice(&hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07").unwrap())
            .unwrap()
            .to_bytes(),
    )
}

fn fixture_spend_pubkey_hex() -> String {
    let secret_spend =
        PrivateKey::from_slice(&hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907").unwrap()).unwrap();
    hex::encode(PublicKey::from_private_key(&secret_spend).to_bytes())
}

async fn create_fixture_tenant(router: &Router) -> TestTenant {
    let req = json_request(
        "POST",
        "/api/v1/admin/tenants",
        None,
        None,
        serde_json::json!({
            "view_key_hex": fixture_view_key_hex(),
            "spend_pubkey_hex": fixture_spend_pubkey_hex(),
        }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    TestTenant { public_key: body["public_key"].as_str().unwrap().to_string(), secret_token: body["secret_token"].as_str().unwrap().to_string() }
}

#[tokio::test]
async fn lookup_payment_rejects_a_malformed_txid() {
    let (state, _daemon) = test_app_state_with_real_daemon();
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;

    let response = router.clone().oneshot(lookup_request(&tenant.secret_token, "not-a-real-txid")).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn lookup_payment_requires_authentication() {
    let (state, _daemon) = test_app_state_with_real_daemon();
    let router = build_router(state, 1_000_000);
    let req = json_request("POST", "/api/v1/admin/tenant/payments/lookup", None, None, serde_json::json!({ "txid": "0".repeat(64) }));
    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn lookup_payment_reports_not_found_on_chain_for_an_unknown_txid() {
    let (state, _daemon) = test_app_state_with_real_daemon();
    let router = build_router(state, 1_000_000);
    let tenant = create_tenant(&router, 1).await;

    let bogus = "0".repeat(64);
    let response = router.clone().oneshot(lookup_request(&tenant.secret_token, &bogus)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["outcome"], "not_found_on_chain");
}

#[tokio::test]
async fn lookup_payment_reports_no_matching_order_for_a_real_but_unrelated_tx() {
    let (state, daemon) = test_app_state_with_real_daemon();
    let router = build_router(state, 1_000_000);
    // Random, non-fixture keys - this tenant genuinely has no claim on the
    // fixture transaction's outputs.
    let tenant = create_tenant(&router, 1).await;

    let tx = fixture_tx_for_lookup_tests();
    daemon.set_mempool(vec![tx.clone()]);
    use monero::cryptonote::hash::Hashable;
    let txid = hex::encode(tx.hash().to_bytes());

    let response = router.clone().oneshot(lookup_request(&tenant.secret_token, &txid)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["outcome"], "no_matching_order");
}

#[tokio::test]
async fn lookup_payment_matches_and_records_a_real_mempool_payment() {
    let (state, daemon) = test_app_state_with_real_daemon();
    let store = state.store.clone();
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
        None,
        serde_json::json!({ "xmr_amount_piconero": 1u64 }),
    );
    let response = router.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let order_id = body_json(response).await["order_id"].as_str().unwrap().to_string();

    let tx = fixture_tx_for_lookup_tests();
    daemon.set_mempool(vec![tx.clone()]);
    use monero::cryptonote::hash::Hashable;
    let txid = hex::encode(tx.hash().to_bytes());

    let response = router.clone().oneshot(lookup_request(&tenant.secret_token, &txid)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["outcome"], "matched");
    assert_eq!(body["order_ids"].as_array().unwrap(), &[serde_json::Value::String(order_id.clone())]);

    let payments = store.lock().unwrap().get_all_payments(&order_id).unwrap();
    assert_eq!(payments.len(), 1, "the match must actually be recorded, not just reported");

    // A second lookup of the same, already-applied txid must be a safe no-op
    // that still reports the same match - `record_scan_match`'s own existing
    // idempotency, exercised through this new endpoint specifically.
    let response = router.clone().oneshot(lookup_request(&tenant.secret_token, &txid)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["outcome"], "matched");
    let payments = store.lock().unwrap().get_all_payments(&order_id).unwrap();
    assert_eq!(payments.len(), 1, "looking the same txid up twice must not duplicate the recorded payment");
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
                    event = value.to_string();
                } else if let Some(value) = line.strip_prefix("data: ") {
                    data = value.to_string();
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
            None,
            serde_json::json!({ "xmr_amount_piconero": 1_000_000_000_000u64 }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await["order_id"].as_str().unwrap().to_string()
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
        .oneshot(Request::builder().uri("/api/v1/admin/tenant/events").body(Body::empty()).unwrap())
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
                None,
                serde_json::json!({ "refund_address": "refund" }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let (event, data) = next_sse_event(&mut body, &mut buffer).await;
    assert_eq!(event, "order");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&data).unwrap()["order_id"], order_id.as_str());
}
