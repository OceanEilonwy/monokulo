//! Read-only order list/detail + webhook list pages (WBS 1.3.3): a
//! logged-in user views their tenant's real orders and webhooks, proxied
//! from the engine's own admin API using the connection's decrypted
//! `sk_...` token ([`crate::crypto::decrypt`]'s first real consumer outside
//! a test).
//!
//! Scope is deliberately read-only, per the WBS's own "what" bullet for this
//! task (only `GET` engine routes): no webhook create/delete, no order
//! mutation here.
//!
//! A user can have more than one `store_connections` row, so every route
//! here is scoped by `{id}` in the path - and ownership-checked:
//! [`load_owned_connection`] requires the row to both exist *and* belong to
//! the authenticated user, or the route returns a bare `404`. That `404` is
//! deliberately not distinguished from "no such connection at all" - the
//! same enumeration-defense principle `AuthedUser`/`login` already apply to
//! accounts (see `http/mod.rs`'s `ApiError` doc comment), applied here to
//! object-level access instead. A caller guessing another user's connection
//! id must learn nothing beyond what they'd learn guessing a nonexistent
//! one.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

use crate::crypto;
use crate::db::{StoreConnectionRow, UserRow};
use crate::engine_client::EngineClientError;
use crate::templates::{
    OrderDetailData, OrderDetailViewModel, OrderRowViewModel, OrdersViewModel, PaymentRowViewModel,
    WebhookRowViewModel, WebhooksViewModel,
};

use super::{AppState, AuthedUser};

/// Looks up `store_connections` row `id` and confirms it belongs to `user`.
/// `Ok(None)` covers *both* "no such row" and "exists but belongs to someone
/// else" - callers must map that uniformly to `404` (see this module's own
/// doc comment), never distinguishing the two. `Err(())` is a real database
/// failure - the caller's problem, not the requester's.
fn load_owned_connection(state: &AppState, user: &UserRow, id: &str) -> Result<Option<StoreConnectionRow>, ()> {
    let row = state.db.lock().unwrap().get_store_connection_by_id(id).map_err(|_| ())?;
    Ok(row.filter(|row| row.user_id == user.id))
}

/// Decrypts the connection's stored `sk_...` token under `state`'s
/// encryption key. A failure here means a row this service itself wrote and
/// encrypted can't be decrypted with its own key - shouldn't happen, but
/// handled as a plain internal error rather than unwrapped/panicked on (see
/// the task's own note on this).
fn decrypt_sk(state: &AppState, row: &StoreConnectionRow) -> Result<String, ()> {
    crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted).map_err(|_| ())
}

/// `GET /dashboard/connections/{id}/orders` - a simple table of the
/// connection's tenant's orders on the engine.
pub async fn orders_list(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let sk = match decrypt_sk(&state, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let orders = match state.engine_client.list_orders(&sk).await {
        Ok(orders) => orders,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let view_model = OrdersViewModel {
        connection_id: id,
        orders: orders
            .into_iter()
            .map(|o| OrderRowViewModel {
                payment_id: o.payment_id,
                status: o.status,
                fiat_amount: o.fiat_amount,
                fiat_currency: o.fiat_currency,
                created_at: o.created_at,
            })
            .collect(),
    };
    let html = state.templates.render_orders(&view_model).expect("the built-in orders template must always render");
    Html(html).into_response()
}

/// `GET /dashboard/connections/{id}/orders/{payment_id}` - the order's full
/// detail (every `OrderView` field plus its `payments` list). A
/// `payment_id` the engine doesn't recognize for this tenant (unknown, or
/// belonging to a different one) renders a clear "not found" state with a
/// real `404`, not a raw `500` - the engine's own `404` is distinguished
/// from every other non-success status the same way
/// `connections::create_connection_for_user` already distinguishes the
/// engine's `400` from everything else.
pub async fn order_detail(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, payment_id)): Path<(String, String)>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let sk = match decrypt_sk(&state, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    match state.engine_client.get_order_detail(&sk, &payment_id).await {
        Ok(detail) => {
            let view_model = OrderDetailViewModel {
                connection_id: id,
                order: Some(OrderDetailData {
                    payment_id: detail.order.payment_id,
                    merchant_order_id: detail.order.merchant_order_id,
                    address: detail.order.address,
                    fiat_currency: detail.order.fiat_currency,
                    fiat_amount: detail.order.fiat_amount,
                    xmr_amount_piconero: detail.order.xmr_amount_piconero,
                    amount_received_piconero: detail.order.amount_received_piconero,
                    status: detail.order.status,
                    confirmations: detail.order.confirmations,
                    double_spend_detected_at: detail.order.double_spend_detected_at,
                    refund_address: detail.order.refund_address,
                    created_at: detail.order.created_at,
                    expires_at: detail.order.expires_at,
                    updated_at: detail.order.updated_at,
                    payments: detail
                        .payments
                        .into_iter()
                        .map(|p| PaymentRowViewModel {
                            txid: p.txid,
                            output_index: p.output_index,
                            amount_piconero: p.amount_piconero,
                            first_seen_at: p.first_seen_at,
                            block_height: p.block_height,
                            voided_at: p.voided_at,
                        })
                        .collect(),
                }),
            };
            let html = state
                .templates
                .render_order_detail(&view_model)
                .expect("the built-in order detail template must always render");
            Html(html).into_response()
        }
        Err(EngineClientError::EngineError { status, .. }) if status == reqwest::StatusCode::NOT_FOUND => {
            let view_model = OrderDetailViewModel { connection_id: id, order: None };
            let html = state
                .templates
                .render_order_detail(&view_model)
                .expect("the built-in order detail template must always render");
            (StatusCode::NOT_FOUND, Html(html)).into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `GET /dashboard/connections/{id}/webhooks` - a simple table of the
/// connection's tenant's registered webhooks. Read-only, per this task's
/// scope: no create/delete route lives here.
pub async fn webhooks_list(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let sk = match decrypt_sk(&state, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let webhooks = match state.engine_client.list_webhooks(&sk).await {
        Ok(webhooks) => webhooks,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let view_model = WebhooksViewModel {
        connection_id: id,
        webhooks: webhooks
            .into_iter()
            .map(|w| WebhookRowViewModel {
                webhook_id: w.webhook_id,
                url: w.url,
                enabled: w.enabled,
                created_at: w.created_at,
            })
            .collect(),
    };
    let html =
        state.templates.render_webhooks(&view_model).expect("the built-in webhooks template must always render");
    Html(html).into_response()
}

/// Derives a human-readable store name from `site_url`, since
/// `store_connections` has no dedicated display-name column (a real,
/// deliberate scope decision - see `Db::list_store_connections_for_user`'s
/// doc comment: adding a migration for a column nothing else needs wasn't
/// worth it when the URL's own host is already a perfectly good name).
/// Falls back to the raw `site_url` string if it doesn't parse as a URL at
/// all, so this never panics or produces an empty name.
pub(super) fn display_name_for(site_url: &str) -> String {
    url::Url::parse(site_url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| site_url.to_string())
}

/// The only store-health signal available without this service also probing
/// the merchant's own `site_url` (nothing here does that): whether the
/// engine actually answers `GET /api/v1/admin/tenant` for this connection's
/// `sk_...`. `("ok", "healthy")`/`("error", "unreachable")` are used
/// directly as a CSS class suffix (`tag-{{health}}`, see `_styles.html.hbs`)
/// and a human label respectively - kept as two separate strings rather than
/// deriving one from the other so the template never has to.
pub(super) fn health_of_tenant_lookup<T>(result: &Result<T, EngineClientError>) -> (String, String) {
    match result {
        Ok(_) => ("ok".to_string(), "healthy".to_string()),
        Err(_) => ("error".to_string(), "unreachable".to_string()),
    }
}

/// `GET /dashboard/connections/{id}` - the store overview page: identity,
/// health, recent orders, and the same integration-help content
/// (`_integration_help.html.hbs`) shown right after a successful connect, so
/// a merchant can always find it again later without re-connecting.
pub async fn store_detail(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => {
            let html = state
                .templates
                .render_store_detail(&crate::templates::StoreDetailViewModel { store: None })
                .expect("the built-in store detail template must always render");
            return (StatusCode::NOT_FOUND, Html(html)).into_response();
        }
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let sk = match decrypt_sk(&state, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let tenant_result = state.engine_client.get_tenant(&sk).await;
    let (health, health_label) = health_of_tenant_lookup(&tenant_result);

    // A store whose engine is currently unreachable still gets a real page -
    // just with no order data available, rather than a hard error. The
    // health tag above is what actually communicates the problem.
    let recent_orders = match state.engine_client.list_orders(&sk).await {
        Ok(mut orders) => {
            orders.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            orders
                .into_iter()
                .take(10)
                .map(|o| OrderRowViewModel {
                    payment_id: o.payment_id,
                    status: o.status,
                    fiat_amount: o.fiat_amount,
                    fiat_currency: o.fiat_currency,
                    created_at: o.created_at,
                })
                .collect()
        }
        Err(_) => Vec::new(),
    };

    let view_model = crate::templates::StoreDetailViewModel {
        store: Some(crate::templates::StoreDetailData {
            connection_id: id,
            display_name: display_name_for(&row.site_url),
            platform: row.platform,
            site_url: row.site_url,
            public_key: row.tenant_public_key,
            endpoint: row.moneropay_endpoint,
            health,
            health_label,
            created_at: row.created_at,
            recent_orders,
        }),
    };
    let html =
        state.templates.render_store_detail(&view_model).expect("the built-in store detail template must always render");
    Html(html).into_response()
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::db::Db;
    use crate::engine_client::EngineClient;

    use super::super::{AppState, build_router};

    /// Same fixed-scalar construction `engine_client.rs`'s and
    /// `connections.rs`'s own tests use.
    const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
    const TEST_ENCRYPTION_KEY: [u8; 32] = [7u8; 32];

    /// A fixed, arbitrary exchange rate for a test-only currency - only its
    /// non-zero-ness matters, since `compute_xmr_amount` (engine crate) is
    /// exact integer arithmetic regardless of the rate's real-world
    /// plausibility.
    const TEST_CURRENCY: &str = "USD";
    const TEST_RATE_PICONERO_PER_UNIT: u64 = 1_000_000_000_000;

    async fn test_state_with_real_engine() -> (AppState, engine_test_support::TestEngineHandle) {
        let engine = engine_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .with_rate(TEST_CURRENCY, TEST_RATE_PICONERO_PER_UNIT)
            .spawn()
            .await;
        let engine_client = EngineClient::new(format!("http://{}", engine.addr));
        let state = AppState {
            db: Db::open_in_memory().unwrap().into_shared(),
            engine_client,
            encryption_key: TEST_ENCRYPTION_KEY,
            templates: std::sync::Arc::new(crate::templates::TemplateEngine::new().unwrap()),
        };
        (state, engine)
    }

    fn signup_request(email: &str, password: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/signup")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({ "email": email, "password": password }).to_string()))
            .unwrap()
    }

    fn login_request(email: &str, password: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/login")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({ "email": email, "password": password }).to_string()))
            .unwrap()
    }

    fn create_connection_request(bearer: &str) -> Request<Body> {
        let body = serde_json::json!({
            "platform": "woocommerce",
            "site_url": "https://shop.example.com",
            "view_key_hex": TEST_VIEW_KEY_HEX,
            "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
            "network": "mainnet",
            "allowed_origins": [],
        });
        Request::builder()
            .method("POST")
            .uri("/connections")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {bearer}"))
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// Signs up and logs in a fresh user against `router`, returning their
    /// session (bearer) token.
    async fn signed_up_and_logged_in_session_token(router: &Router, email: &str, password: &str) -> String {
        let signup = router.clone().oneshot(signup_request(email, password)).await.unwrap();
        assert_eq!(signup.status(), StatusCode::CREATED);

        let login = router.clone().oneshot(login_request(email, password)).await.unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        body_json(login).await.as_object().unwrap().get("session_token").unwrap().as_str().unwrap().to_string()
    }

    /// Creates a real `store_connections` row for the given session token via
    /// the JSON `/connections` API, returning `(connection_id, public_key)`.
    async fn create_connection(router: &Router, session_token: &str) -> (String, String) {
        let response = router.clone().oneshot(create_connection_request(session_token)).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response).await;
        let obj = body.as_object().unwrap();
        (
            obj.get("connection_id").unwrap().as_str().unwrap().to_string(),
            obj.get("public_key").unwrap().as_str().unwrap().to_string(),
        )
    }

    /// Seeds a real order against the engine's *public* order-creation API
    /// (`POST /api/v1/t/{pk}/orders`), using a raw `reqwest` call directly
    /// against the spawned engine's address - not the control plane's own
    /// router, since this is the engine's own public surface a real
    /// storefront (or its plugin) would call, not anything the control
    /// plane proxies. No `Origin` header is sent, so the tenant's
    /// (empty) `allowed_origins` never comes into play - see
    /// `src/http/public.rs::resolve_public_tenant` at the repo root: an
    /// absent `Origin` skips that check entirely, exactly like a
    /// server-to-server call would.
    async fn seed_real_order(engine_addr: std::net::SocketAddr, public_key: &str) -> String {
        let response = reqwest::Client::new()
            .post(format!("http://{engine_addr}/api/v1/t/{public_key}/orders"))
            .json(&serde_json::json!({
                "fiat_amount": "10.00",
                "fiat_currency": TEST_CURRENCY,
            }))
            .send()
            .await
            .expect("seeding a real order against the engine's public API failed");
        assert_eq!(response.status(), reqwest::StatusCode::OK, "expected the engine to accept the seeded order");
        let body: serde_json::Value = response.json().await.unwrap();
        body.as_object().unwrap().get("payment_id").unwrap().as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn orders_list_shows_a_real_order_seeded_against_the_engines_public_api() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "orders-owner@example.com", "correct horse battery staple")
                .await;
        let (connection_id, public_key) = create_connection(&router, &session_token).await;

        let payment_id = seed_real_order(engine.addr, &public_key).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/connections/{connection_id}/orders"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains(&payment_id), "expected the seeded order's payment_id in the response, got: {html}");
    }

    #[tokio::test]
    async fn order_detail_shows_the_seeded_orders_full_detail() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "order-detail-owner@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) = create_connection(&router, &session_token).await;

        let payment_id = seed_real_order(engine.addr, &public_key).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/connections/{connection_id}/orders/{payment_id}"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains(&payment_id), "expected the order's payment_id in its detail page, got: {html}");
        assert!(html.contains(TEST_CURRENCY), "expected the order's fiat currency in its detail page, got: {html}");
    }

    #[tokio::test]
    async fn order_detail_for_an_unknown_payment_id_renders_a_clear_not_found_state() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "order-detail-missing@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/connections/{connection_id}/orders/no-such-payment-id"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "expected a real 404, not a raw 500");
        let html = body_text(response).await;
        assert!(html.to_lowercase().contains("not found"), "expected a clear not-found state, got: {html}");
    }

    #[tokio::test]
    async fn webhooks_list_on_a_connection_with_none_registered_renders_an_empty_but_valid_page() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "webhooks-empty@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/connections/{connection_id}/webhooks"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("<table"), "expected a real, valid page even with no webhooks, got: {html}");
    }

    #[tokio::test]
    async fn a_different_user_hitting_the_first_users_connection_gets_404_not_their_data() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let owner_token =
            signed_up_and_logged_in_session_token(&router, "cross-user-owner@example.com", "correct horse battery staple")
                .await;
        let (connection_id, public_key) = create_connection(&router, &owner_token).await;
        seed_real_order(engine.addr, &public_key).await;

        let other_token = signed_up_and_logged_in_session_token(
            &router,
            "cross-user-intruder@example.com",
            "a different password entirely",
        )
        .await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/connections/{connection_id}/orders"))
                    .header("authorization", format!("Bearer {other_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Not 403 (which would confirm the id exists) and not 200 with the
        // owner's data - a bare 404, indistinguishable from a nonexistent id.
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unauthenticated_requests_to_all_three_new_routes_get_401_before_any_ownership_check() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        // A connection id doesn't even need to exist for this - `AuthedUser`
        // must reject before the handler ever looks it up.
        let fake_id = "nonexistent-connection-id";

        for uri in [
            format!("/dashboard/connections/{fake_id}/orders"),
            format!("/dashboard/connections/{fake_id}/orders/some-payment-id"),
            format!("/dashboard/connections/{fake_id}/webhooks"),
        ] {
            let response = router
                .clone()
                .oneshot(Request::builder().method("GET").uri(uri.clone()).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "expected 401 for {uri} with no session");
        }
    }

    #[tokio::test]
    async fn store_detail_shows_the_real_connected_stores_overview_and_integration_help() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(&router, "store-detail@example.com", "correct horse battery staple").await;
        let (connection_id, public_key) = create_connection(&router, &session_token).await;
        let payment_id = seed_real_order(engine.addr, &public_key).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/connections/{connection_id}"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;

        assert!(html.contains(&public_key), "expected the store's public key, got: {html}");
        assert!(html.contains("shop.example.com"), "expected a display name derived from site_url, got: {html}");
        assert!(html.contains("tag-ok"), "the engine is genuinely reachable, so health must render as ok, got: {html}");
        assert!(html.contains(&payment_id), "expected the seeded order in the recent-orders list, got: {html}");
        // The integration-help partial - the same content the WBS asked to
        // be "accessible from the store page for each connected store".
        assert!(html.contains("Integrate this store"), "expected the integration help section, got: {html}");
    }

    #[tokio::test]
    async fn store_detail_for_an_unowned_or_unknown_connection_returns_404() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let owner_token = signed_up_and_logged_in_session_token(&router, "store-detail-owner@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &owner_token).await;

        let other_token = signed_up_and_logged_in_session_token(&router, "store-detail-intruder@example.com", "correct horse battery staple").await;

        for uri in [format!("/dashboard/connections/{connection_id}"), "/dashboard/connections/nonexistent".to_string()] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(uri.clone())
                        .header("authorization", format!("Bearer {other_token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "expected 404 for {uri}");
        }
    }
}
