//! Order list/detail pages (read-only, WBS 1.3.3) plus webhook management
//! (create/delete - user-directed follow-up, since the engine's own admin
//! API already supported both and nothing in this crate exposed them): a
//! logged-in user views their tenant's real orders and manages its real
//! webhooks, proxied from the engine's own admin API using the connection's
//! decrypted `sk_...` token ([`crate::crypto::decrypt`]'s first real
//! consumer outside a test).
//!
//! Orders stay read-only, per WBS 1.3.3's own "what" bullet for that part of
//! this module (only `GET` engine routes): no order mutation here, ever.
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

use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use crate::crypto;
use crate::db::{StoreConnectionRow, UserRow};
use crate::engine_client::EngineClientError;
use crate::templates::{
    display_or_dash, display_timestamp, display_timestamp_or_dash, OrderDetailData, OrderDetailViewModel,
    OrderRowViewModel, OrdersViewModel, PaymentRowViewModel, WebhookRowViewModel, WebhooksViewModel,
};

use super::dashboard::redirect_302;
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
                    double_spend_detected_at_display: display_timestamp_or_dash(detail.order.double_spend_detected_at),
                    refund_address: detail.order.refund_address,
                    created_at_display: display_timestamp(detail.order.created_at),
                    expires_at_display: display_timestamp(detail.order.expires_at),
                    updated_at_display: display_timestamp(detail.order.updated_at),
                    payments: detail
                        .payments
                        .into_iter()
                        .map(|p| PaymentRowViewModel {
                            txid: p.txid,
                            output_index: p.output_index,
                            amount_piconero: p.amount_piconero,
                            first_seen_at_display: display_timestamp(p.first_seen_at),
                            block_height_display: display_or_dash(p.block_height.map(|h| h.to_string()).as_deref()),
                            voided_at_display: display_timestamp_or_dash(p.voided_at),
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

/// `GET /dashboard/connections/{id}/webhooks` - a table of the connection's
/// tenant's registered webhooks, plus (below) the create/delete actions
/// this same page's forms post back to.
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
    render_webhooks_page(&state, &id, &sk, None, None).await
}

/// Shared by `webhooks_list`/`webhooks_create`/`webhooks_delete` - every one
/// of them ends by showing the same page (a fresh webhook list, optionally
/// with an error or a just-created secret), so this is the one place that
/// actually fetches the list and renders it.
async fn render_webhooks_page(
    state: &AppState,
    connection_id: &str,
    sk: &str,
    error: Option<String>,
    created_webhook_signing_secret: Option<String>,
) -> Response {
    let webhooks = match state.engine_client.list_webhooks(sk).await {
        Ok(webhooks) => webhooks,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let view_model = WebhooksViewModel {
        connection_id: connection_id.to_string(),
        webhooks: webhooks
            .into_iter()
            .map(|w| WebhookRowViewModel {
                webhook_id: w.webhook_id,
                url: w.url,
                enabled: w.enabled,
                created_at: w.created_at,
            })
            .collect(),
        error,
        created_webhook_signing_secret,
    };
    let html =
        state.templates.render_webhooks(&view_model).expect("the built-in webhooks template must always render");
    Html(html).into_response()
}

#[derive(Deserialize)]
pub struct CreateWebhookForm {
    pub url: String,
}

/// `POST /dashboard/connections/{id}/webhooks` - registers a new webhook via
/// the engine's own `POST /api/v1/admin/tenant/webhooks` (already built,
/// nothing here proxies to previously; this is purely a UI gap closing).
/// Deliberately re-renders the page directly rather than redirecting on
/// success (unlike `webhooks_delete`'s POST-redirect-GET below) - the
/// engine's real, freshly-issued signing secret has to be shown somewhere,
/// exactly once, and a redirect would mean carrying it in a URL (browser
/// history, `Referer` headers) instead of a response body. See
/// `WebhooksViewModel::created_webhook_signing_secret`'s own doc comment for
/// why there's no second chance to show it later.
pub async fn webhooks_create(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(form): Form<CreateWebhookForm>,
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

    let url = form.url.trim();
    if url.is_empty() {
        return render_webhooks_page(&state, &id, &sk, Some("Enter a webhook URL.".to_string()), None).await;
    }

    match state.engine_client.create_webhook(&sk, url).await {
        Ok((_webhook_id, signing_secret)) => render_webhooks_page(&state, &id, &sk, None, Some(signing_secret)).await,
        // The engine's own validation (a malformed URL, a non-http(s) scheme -
        // `src/http/admin.rs::create_webhook` at the repo root) - the
        // caller's mistake, surfaced verbatim, same convention
        // `connections::create_connection_for_user` already applies to the
        // engine's tenant-creation `400`s.
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            render_webhooks_page(&state, &id, &sk, Some(message), None).await
        }
        Err(_) => render_webhooks_page(&state, &id, &sk, Some("Something went wrong. Please try again.".to_string()), None).await,
    }
}

/// `POST /dashboard/connections/{id}/webhooks/{webhook_id}/delete` - a POST
/// (not a real `DELETE`) because a plain HTML `<form>` can only submit
/// `GET`/`POST`. Redirects back to the plain webhook list on success
/// (POST-redirect-GET - refreshing the page after a delete must not risk
/// resubmitting it) or on the engine's own `404` for an unknown/not-this-
/// tenant's `webhook_id`; only a genuine internal error re-renders the page
/// with a visible error.
pub async fn webhooks_delete(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, webhook_id)): Path<(String, String)>,
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

    match state.engine_client.delete_webhook(&sk, &webhook_id).await {
        Ok(()) => redirect_302(&format!("/dashboard/connections/{id}/webhooks")),
        Err(EngineClientError::EngineError { status, .. }) if status == reqwest::StatusCode::NOT_FOUND => {
            redirect_302(&format!("/dashboard/connections/{id}/webhooks"))
        }
        Err(_) => render_webhooks_page(&state, &id, &sk, Some("Could not delete that webhook. Please try again.".to_string()), None).await,
    }
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
    render_store_detail_page(&state, row, None).await
}

/// Shared by `store_detail` and `create_order` - both end by showing the
/// same page (a fresh store overview, optionally with a create-order
/// error), same pattern as `orders.rs`'s own `render_webhooks_page`. Takes
/// an already ownership-checked row rather than re-checking it, since both
/// callers have already done that.
async fn render_store_detail_page(state: &AppState, row: StoreConnectionRow, order_creation_error: Option<String>) -> Response {
    let sk = match decrypt_sk(state, &row) {
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

    let is_woocommerce = row.platform == "woocommerce";
    let view_model = crate::templates::StoreDetailViewModel {
        store: Some(crate::templates::StoreDetailData {
            connection_id: row.id,
            display_name: display_name_for(&row.site_url),
            platform: row.platform,
            site_url: row.site_url,
            public_key: row.tenant_public_key,
            endpoint: row.moneropay_endpoint,
            health,
            health_label,
            created_at: row.created_at,
            recent_orders,
            is_woocommerce,
            order_creation_error,
        }),
    };
    let html =
        state.templates.render_store_detail(&view_model).expect("the built-in store detail template must always render");
    Html(html).into_response()
}

#[derive(Deserialize)]
pub struct CreateOrderForm {
    pub fiat_amount: String,
    pub fiat_currency: String,
}

/// `POST /dashboard/connections/{id}/orders/new` - creates a real order
/// directly from the dashboard, via the engine's own *public*
/// order-creation API (`EngineClient::create_order`, `pk_`-addressed, the
/// same endpoint a real storefront would call) - lets a merchant try the
/// payment flow without wiring up a storefront first. Redirects straight to
/// the new order's own detail page on success (POST-redirect-GET); a
/// validation error (unsupported currency, unparseable amount) re-renders
/// the store page with the engine's real message, same convention
/// `webhooks_create` already applies.
pub async fn create_order(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(form): Form<CreateOrderForm>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let fiat_amount = form.fiat_amount.trim();
    let fiat_currency = form.fiat_currency.trim();
    if fiat_amount.is_empty() || fiat_currency.is_empty() {
        return render_store_detail_page(&state, row, Some("Enter an amount and a currency.".to_string())).await;
    }

    match state.engine_client.create_order(&row.tenant_public_key, fiat_amount, fiat_currency).await {
        Ok(order) => redirect_302(&format!("/dashboard/connections/{id}/orders/{}", order.payment_id)),
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            render_store_detail_page(&state, row, Some(message)).await
        }
        Err(_) => render_store_detail_page(&state, row, Some("Something went wrong. Please try again.".to_string())).await,
    }
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
            status_cache: crate::http::status_page::new_status_cache(),
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
        // Real bug fixed: `created_at`/`expires_at`/`updated_at` used to be
        // shown as raw Unix seconds - now a human-readable UTC date/time.
        assert!(html.contains("UTC"), "expected human-readable timestamps, got: {html}");
        // `merchant_order_id` was never set on this seeded order - must show
        // a muted placeholder, not a blank cell.
        assert!(html.contains("muted"), "expected a muted placeholder for the unset merchant order id, got: {html}");
        assert!(html.contains(r#"<meta http-equiv="refresh""#), "expected an auto-refresh meta tag, got: {html}");
        assert!(html.contains("refreshes automatically"), "expected the refresh interval noted on the page, got: {html}");
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

    /// Minimal `application/x-www-form-urlencoded` percent-encoding for test
    /// fixtures - same approach `http/connect.rs`'s own test module already
    /// uses for its form-based tests.
    fn urlencoding_encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    fn form_post_request(uri: &str, bearer: &str, fields: &[(&str, &str)]) -> Request<Body> {
        let body =
            fields.iter().map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v))).collect::<Vec<_>>().join("&");
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", format!("Bearer {bearer}"))
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn creating_a_webhook_shows_its_signing_secret_once_and_lists_it_afterward() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "webhook-create@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/webhooks"),
                &session_token,
                &[("url", "https://merchant.example/moneropay-webhook")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("https://merchant.example/moneropay-webhook"), "expected the new webhook listed, got: {html}");
        assert!(html.contains("Webhook created"), "expected the one-time signing-secret banner, got: {html}");

        // The list itself (a separate GET, simulating a page reload) must
        // show the webhook but never the secret again - it's genuinely
        // gone, not just hidden by this response's own rendering choice.
        let list_response = router
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
        let list_html = body_text(list_response).await;
        assert!(list_html.contains("https://merchant.example/moneropay-webhook"));
        assert!(!list_html.contains("Webhook created"), "the signing secret must not reappear on a later page load, got: {list_html}");
    }

    #[tokio::test]
    async fn creating_a_webhook_with_an_empty_url_shows_a_clear_error() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "webhook-empty-url@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(form_post_request(&format!("/dashboard/connections/{connection_id}/webhooks"), &session_token, &[("url", "")]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("Enter a webhook URL."), "expected a clear validation error, got: {html}");
    }

    #[tokio::test]
    async fn creating_a_webhook_with_an_invalid_url_surfaces_the_engines_real_error() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "webhook-bad-url@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/webhooks"),
                &session_token,
                &[("url", "not a url at all")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("class=\"error\""), "expected the engine's real validation error surfaced, got: {html}");
    }

    #[tokio::test]
    async fn deleting_a_webhook_removes_it_from_the_list() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "webhook-delete@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let create_response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/webhooks"),
                &session_token,
                &[("url", "https://merchant.example/to-be-deleted")],
            ))
            .await
            .unwrap();
        let create_html = body_text(create_response).await;
        // The webhook_id isn't shown in the rendered page (only the URL is -
        // see webhooks.html.hbs), so read it back from the engine's own
        // list API directly via the same session, the same way a real
        // delete form's hidden webhook_id would have been rendered from.
        assert!(create_html.contains("https://merchant.example/to-be-deleted"));

        let list_before = router
            .clone()
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
        let list_before_html = body_text(list_before).await;
        let webhook_id_start = list_before_html.find("/webhooks/").expect("expected a delete form action containing the webhook id") + "/webhooks/".len();
        let webhook_id: String = list_before_html[webhook_id_start..].chars().take_while(|c| *c != '/').collect();
        assert!(!webhook_id.is_empty());

        let delete_response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/webhooks/{webhook_id}/delete"),
                &session_token,
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(delete_response.status(), StatusCode::FOUND, "expected a redirect back to the webhook list");

        let list_after = router
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
        let list_after_html = body_text(list_after).await;
        assert!(
            !list_after_html.contains("https://merchant.example/to-be-deleted"),
            "expected the deleted webhook gone, got: {list_after_html}"
        );
    }

    #[tokio::test]
    async fn creating_an_order_from_the_dashboard_redirects_to_its_real_detail_page() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "order-create@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/orders/new"),
                &session_token,
                &[("fiat_amount", "10.00"), ("fiat_currency", TEST_CURRENCY)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND, "expected a redirect to the new order's own detail page");
        let location = response.headers().get("location").unwrap().to_str().unwrap().to_string();
        assert!(
            location.starts_with(&format!("/dashboard/connections/{connection_id}/orders/")),
            "expected a redirect into this store's own orders, got: {location}"
        );

        let detail_response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(location)
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail_response.status(), StatusCode::OK);
        let html = body_text(detail_response).await;
        assert!(html.contains(TEST_CURRENCY), "expected the real, just-created order's detail page, got: {html}");
    }

    #[tokio::test]
    async fn creating_an_order_with_an_unsupported_currency_shows_the_engines_real_error() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "order-create-bad-currency@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/orders/new"),
                &session_token,
                &[("fiat_amount", "10.00"), ("fiat_currency", "NOTREAL")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a validation error re-renders the page, it doesn't redirect");
        let html = body_text(response).await;
        assert!(html.contains("unsupported currency"), "expected the engine's real validation error surfaced, got: {html}");
        assert!(html.contains("<form"), "the create-order form must still be present, got: {html}");
    }

    #[tokio::test]
    async fn a_different_user_cannot_create_an_order_on_someone_elses_connection() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let owner_token =
            signed_up_and_logged_in_session_token(&router, "order-create-owner@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &owner_token).await;

        let intruder_token =
            signed_up_and_logged_in_session_token(&router, "order-create-intruder@example.com", "correct horse battery staple").await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/orders/new"),
                &intruder_token,
                &[("fiat_amount", "10.00"), ("fiat_currency", TEST_CURRENCY)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_different_user_cannot_create_or_delete_webhooks_on_someone_elses_connection() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let owner_token =
            signed_up_and_logged_in_session_token(&router, "webhook-owner@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &owner_token).await;

        let intruder_token =
            signed_up_and_logged_in_session_token(&router, "webhook-intruder@example.com", "correct horse battery staple").await;

        let create_response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/webhooks"),
                &intruder_token,
                &[("url", "https://attacker.example/steal")],
            ))
            .await
            .unwrap();
        assert_eq!(create_response.status(), StatusCode::NOT_FOUND);

        let delete_response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/webhooks/some-webhook-id/delete"),
                &intruder_token,
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(delete_response.status(), StatusCode::NOT_FOUND);
    }
}
