//! In-person Point-of-Sale terminal - a Square-Terminal-like screen a
//! merchant runs on a device at the counter: enter an amount in the store's
//! own `base_currency`, show the shared checkout iframe for the customer to
//! pay, then react live
//! to the payment appearing.
//!
//! Deliberately behind [`AuthedUser`] and connection-ownership-checked
//! ([`load_owned_connection`]) exactly like every other `/dashboard/stores/{id}/*`
//! route (`http::orders`'s own module doc comment) - unlike `http::pay`'s
//! public, unauthenticated storefront endpoint, a POS terminal is operated
//! by the merchant themselves, logged in, standing at the register.
//!
//! Order creation reuses the exact same pricing/threshold-resolution path
//! `http::orders::create_order`/`http::pay::create_order` already use
//! (`crate::confirmation_thresholds::resolve_for_order`) - a POS sale is not
//! a different kind of order, just a different UI for creating one, always
//! denominated in this store's own `base_currency` rather than a
//! caller-chosen one (there's no currency picker on a terminal screen, see
//! this task's own spec point 2).
//!
//! Unlike the public checkout page (`http::checkout`, which must stay
//! usable with JavaScript disabled), this screen is a merchant-operated dashboard tool in the same
//! bucket as the rest of `/dashboard/*`, which already uses JS as
//! progressive enhancement elsewhere (`order_detail.html.hbs`'s share
//! button/local-time script). A live Square-Terminal-style keypad, stacked
//! backgrounded payments have no meaningful no-JS fallback, so this screen leans on
//! JS for its real interaction loop rather than a meta-refresh. Payment
//! status arrives over one Server-Sent Events stream per terminal
//! ([`order_events`]); [`order_status`] remains for one-off reads.
//!
//! **Error surfacing** (spec point 12): [`derive_payment_error`] flags
//! exactly the cases where the merchant, not just the customer's wallet,
//! needs to step in - a double-spend, an amount that doesn't match what was
//! asked for (under- or over-paid), or the order expiring before it ever
//! got there. The status stream itself dropping (the server unreachable) is
//! surfaced by the connection failing, not a field on a status event - the
//! client already has to handle "the connection failed" separately from
//! "the status says there's a problem".

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};

use crate::engine_client::{EngineClientError, OrderView};
use crate::views;
use crate::views::pos::PosViewModel;

use super::checkout::status_label;
use super::orders::{decrypt_sk, display_name_for, load_owned_connection};
use super::{ApiError, AppState, AuthedUser};

/// `GET /dashboard/stores/{id}/pos` - the terminal screen itself.
pub async fn pos_page(State(state): State<AppState>, AuthedUser(user, _): AuthedUser, Path(id): Path<String>) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // Fiat currencies here are always exactly 2 decimal places (the same
    // assumption `shared::exchange_rate::compute_xmr_amount` already
    // enforces server-side) - XMR itself carries its own native 12-decimal
    // precision (`shared::exchange_rate::parse_xmr_to_piconero`), which a
    // 2-decimal keypad would silently truncate. See this task's own "Decimal
    // entry" decision.
    let base_currency_decimals: u8 = if row.base_currency.eq_ignore_ascii_case("XMR") { 12 } else { 2 };

    let view = PosViewModel {
        connection_id: id,
        public_key: row.tenant_public_key,
        display_name: display_name_for(&row.site_url),
        base_currency: row.base_currency,
        base_currency_decimals,
    };
    let chrome = super::page_chrome(&state, Some(&user), format!("/dashboard/stores/{}/pos", view.connection_id));
    views::pos::page(&chrome, &view).into_response()
}

#[derive(Debug, Deserialize)]
pub struct PosCreateOrderRequest {
    /// A plain decimal string in the store's own `base_currency` - the
    /// keypad's own accumulated value, already formatted to the currency's
    /// real decimal precision (`PosViewModel::base_currency_decimals`)
    /// before it's ever sent here. Re-validated server-side the same way
    /// every other order-creation surface in this crate already is
    /// (`shared::exchange_rate::compute_order_amount`) - a client-side
    /// keypad bug or a hand-crafted request is not trusted to have gotten
    /// this right.
    pub amount: String,
    /// The terminal's own optional quick note (typically a customer's
    /// name) - stored as this order's real `merchant_order_id`, the same
    /// field `http::orders`'s dashboard form and `http::pay`'s public API
    /// already write, so it shows up wherever any other order's
    /// `merchant_order_id` does (the order detail page, `OrderView`).
    /// Trimmed and treated as absent if empty, same convention
    /// `http::orders::create_order`'s own form field already follows.
    #[serde(default)]
    pub merchant_order_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PosCreateOrderResponse {
    pub order_id: String,
    pub address: String,
    pub xmr_amount: String,
    pub amount: String,
    pub currency: String,
    pub confirmations_required: u64,
    pub expires_at: i64,
    pub merchant_order_id: Option<String>,
}

/// `POST /dashboard/stores/{id}/pos/orders` - creates a real order,
/// always priced in this store's own `base_currency` (spec point 2 - a POS
/// terminal has no currency picker). Mirrors
/// `http::orders::create_order`/`http::pay::create_order` field for field;
/// see either's own doc comment for why the pricing/threshold-resolution
/// steps look the way they do.
pub async fn create_order(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Json(req): Json<PosCreateOrderRequest>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(()) => return ApiError::Internal.into_response(),
    };

    let policy_lock = crate::confirmation_thresholds::policy_lock(&row.tenant_public_key);
    let _policy_guard = policy_lock.lock().await;
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(()) => return ApiError::Internal.into_response(),
    };

    let amount = req.amount.trim();
    if amount.is_empty() {
        return ApiError::BadRequest("Enter an amount.".to_string()).into_response();
    }
    let currency = row.base_currency.clone();
    let merchant_order_id = req.merchant_order_id.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);

    let (piconero_per_unit, provider) = match state.exchange_rate.piconero_per_unit_for(&row, &currency).await {
        Ok(Some(result)) => result,
        Ok(None) => return ApiError::BadRequest(format!("unsupported currency: {currency}")).into_response(),
        Err(crate::exchange_rate_config::ExchangeRateLookupError::ProviderNotConfigured(_)) => {
            return ApiError::BadRequest(format!("unsupported currency: {currency}")).into_response();
        }
        Err(e) => {
            eprintln!("exchange rate lookup failed for connection {} (currency {currency:?}): {e}", row.id);
            return ApiError::Internal.into_response();
        }
    };
    let xmr_amount_piconero = match shared::exchange_rate::compute_order_amount(&currency, amount, piconero_per_unit) {
        Ok(amount) => amount,
        Err(e) => return ApiError::BadRequest(e.to_string()).into_response(),
    };

    let sk = match decrypt_sk(&state, &row) {
        Ok(sk) => sk,
        Err(()) => return ApiError::Internal.into_response(),
    };
    let resolution =
        match crate::confirmation_thresholds::resolve_for_order(&state, &row, &sk, &currency, piconero_per_unit, xmr_amount_piconero).await {
            Ok(resolution) => resolution,
            Err(message) => return ApiError::BadRequest(message).into_response(),
        };

    match state
        .engine_client
        .create_order(&sk, xmr_amount_piconero, merchant_order_id.clone(), Some(resolution.confirmations_required))
        .await
    {
        Ok(order) => {
            if let Err(e) = state.db.lock().unwrap().create_order_currency_metadata(
                &row.id,
                &order.order_id,
                &currency,
                amount,
                piconero_per_unit,
                provider,
                crate::now_unix(),
                &resolution.base_currency,
                resolution.base_currency_piconero_per_unit,
                resolution.confirmations_required,
            ) {
                eprintln!(
                    "failed to record local fiat metadata for POS order {} on connection {}: {e} - the real \
                     order still exists on the engine and this response is still correct, but its fiat \
                     display on monokulo's own dashboard will be missing",
                    order.order_id, row.id
                );
            }

            let xmr_amount = shared::exchange_rate::format_piconero_as_xmr(order.xmr_amount_piconero);

            Json(PosCreateOrderResponse {
                order_id: order.order_id,
                address: order.address,
                xmr_amount,
                amount: amount.to_string(),
                currency,
                confirmations_required: resolution.confirmations_required,
                expires_at: order.expires_at,
                merchant_order_id,
            })
            .into_response()
        }
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            ApiError::BadRequest(message).into_response()
        }
        Err(_) => ApiError::Internal.into_response(),
    }
}

#[derive(Debug, Serialize)]
pub struct PosStatusResponse {
    pub status: String,
    pub confirmations: u64,
    pub confirmations_required: u64,
    pub is_terminal: bool,
    /// `Some(...)` exactly in the cases the merchant needs to step in and
    /// deal with the customer directly - see [`derive_payment_error`].
    /// `None` covers both "still pending/confirming normally" and "paid" -
    /// a real, error-free success is not an error just because it's also
    /// terminal.
    pub error: Option<String>,
}

/// The confirmations_required this *specific* order was actually created
/// with (`crate::confirmation_thresholds::Resolution::confirmations_required`,
/// snapshotted at creation time via `Db::create_order_currency_metadata` -
/// see [`create_order`] above) rather than the tenant's own *current*
/// default, which may have changed since. Falls back to the tenant's
/// present default only when no local snapshot exists at all (an order
/// created directly against the engine, or predating this field) - the same
/// "a reasonable, safe-side default" posture `http::checkout::render_checkout_page`
/// already applies to this exact fallback.
pub(super) async fn resolve_confirmations_required(state: &AppState, connection_id: &str, sk: &str, order_id: &str) -> u64 {
    let local = state.db.lock().unwrap().get_order_currency_metadata(connection_id, order_id).unwrap_or_default();
    if let Some(applied) = local.and_then(|m| m.confirmations_required_applied) {
        return applied;
    }
    state.engine_client.get_tenant(sk).await.map(|t| t.confirmations_required).unwrap_or(10)
}

/// Flags exactly the payment outcomes a merchant needs to personally
/// resolve with the customer standing in front of them - not every
/// non-`"paid"` status is an error (`"pending"`/`"unconfirmed"`/`"confirming"`
/// are all normal, expected, no-action-needed states on the way to a real
/// payment). See this module's own doc comment for the full reasoning
/// behind each case picked.
pub(super) fn derive_payment_error(order: &OrderView) -> Option<String> {
    if order.double_spend_detected_at.is_some() {
        return Some("Double-spend detected on this payment. Do not treat it as paid.".to_string());
    }
    match order.status.as_str() {
        "partial" => Some("Underpaid - the customer sent less than the requested amount.".to_string()),
        "overpaid" => Some("Overpaid - the customer sent more than the requested amount.".to_string()),
        "expired" => Some("This payment expired before it was completed.".to_string()),
        _ => None,
    }
}

/// `GET /dashboard/stores/{id}/pos/orders/{order_id}/status` - the
/// small JSON status of one POS order. The terminal screen itself follows
/// the same data live via [`order_events`].
pub async fn order_status(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, order_id)): Path<(String, String)>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(()) => return ApiError::Internal.into_response(),
    };
    let sk = match decrypt_sk(&state, &row) {
        Ok(sk) => sk,
        Err(()) => return ApiError::Internal.into_response(),
    };

    match load_pos_status(&state, &row.id, &sk, &order_id).await {
        Ok(status) => Json(status).into_response(),
        Err(EngineClientError::EngineError { status, .. }) if status == reqwest::StatusCode::NOT_FOUND => {
            ApiError::NotFound.into_response()
        }
        Err(_) => ApiError::Internal.into_response(),
    }
}

async fn load_pos_status(state: &AppState, connection_id: &str, sk: &str, order_id: &str) -> Result<PosStatusResponse, EngineClientError> {
    let detail = state.engine_client.get_order_detail(sk, order_id).await?;
    let confirmations_required = resolve_confirmations_required(state, connection_id, sk, order_id).await;
    // `status_label`'s own `is_terminal` already accounts for a
    // 0-conf-trusted order: the engine only ever reports `"paid"`
    // once *that order's own* `confirmations_required` (however it
    // was resolved at creation - possibly `0`) has actually been
    // met, so there's no separate threshold check to fold in here.
    let (_, _, is_terminal) = status_label(&detail.order.status);
    let error = derive_payment_error(&detail.order);
    Ok(PosStatusResponse {
        status: detail.order.status,
        confirmations: detail.order.confirmations,
        confirmations_required,
        is_terminal,
        error,
    })
}

/// More than any real counter has in flight at once; bounds how many
/// upstream reads one request can fan out to.
const MAX_WATCHED_ORDERS: usize = 32;

#[derive(Deserialize)]
pub struct PosEventsQuery {
    /// Comma-separated order ids - the one on screen plus every backgrounded one.
    orders: String,
}

/// `GET /dashboard/stores/{id}/pos/events?orders=a,b,...` - one
/// Server-Sent Events stream for every order the terminal is watching,
/// replacing a poll loop per order. Each `status` event is
/// [`PosStatusResponse`] plus its `order_id`, sent on connect and again
/// only when that order changes (`crate::live`). An order stops being
/// reported once terminal (or unknown to this store); the terminal reopens
/// the stream with a new id list whenever the set it watches changes.
pub async fn order_events(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Query(query): Query<PosEventsQuery>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(()) => return ApiError::Internal.into_response(),
    };
    let sk = match decrypt_sk(&state, &row) {
        Ok(sk) => sk,
        Err(()) => return ApiError::Internal.into_response(),
    };
    let mut order_ids: Vec<String> = Vec::new();
    for order_id in query.orders.split(',').map(str::trim).filter(|id| !id.is_empty()) {
        if !order_ids.iter().any(|seen| seen == order_id) {
            order_ids.push(order_id.to_string());
        }
    }
    if order_ids.is_empty() || order_ids.len() > MAX_WATCHED_ORDERS {
        return ApiError::BadRequest(format!("orders must list between 1 and {MAX_WATCHED_ORDERS} order ids")).into_response();
    }

    let streams = order_ids.into_iter().map(|order_id| {
        let subscription = state.engine_client.subscribe_order(&row.id, &sk, &order_id);
        let (state, connection_id, sk) = (state.clone(), row.id.clone(), sk.clone());
        Box::pin(crate::live::snapshot_stream(subscription, std::time::Duration::from_secs(60), move || {
            let (state, connection_id, sk, order_id) = (state.clone(), connection_id.clone(), sk.clone(), order_id.clone());
            async move {
                match load_pos_status(&state, &connection_id, &sk, &order_id).await {
                    Ok(status) => {
                        let mut json = serde_json::to_value(&status).ok()?;
                        json["order_id"] = serde_json::Value::String(order_id);
                        let data = json.to_string();
                        Some(crate::live::LiveSnapshot {
                            events: vec![axum::response::sse::Event::default().event("status").data(data.clone())],
                            fingerprint: data,
                            terminal: status.is_terminal,
                        })
                    }
                    // Not this store's order: nothing to watch.
                    Err(EngineClientError::EngineError { status, .. }) if status == reqwest::StatusCode::NOT_FOUND => {
                        Some(crate::live::LiveSnapshot { events: Vec::new(), fingerprint: String::new(), terminal: true })
                    }
                    Err(_) => None,
                }
            }
        }))
    });
    crate::live::sse(futures_util::stream::select_all(streams))
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

    const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
    const TEST_ENCRYPTION_KEY: [u8; 32] = [7u8; 32];

    fn test_exchange_rate_provider() -> std::sync::Arc<crate::exchange_rate_config::ExchangeRateProviders> {
        std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::xmr_only())
    }

    async fn test_state_with_real_engine() -> (AppState, scanner_test_support::TestEngineHandle) {
        let engine = scanner_test_support::TestEngineConfig::new().with_networks(&[monero::Network::Mainnet]).spawn().await;
        let engine_client = EngineClient::new(format!("http://{}", engine.addr));
        let state = AppState {
            db: { let db = Db::open_in_memory().unwrap(); db.seed_test_admin(); db.into_shared() },
            engine_client,
            encryption_key: TEST_ENCRYPTION_KEY,
            status_cache: crate::http::status_page::new_status_cache(),
            exchange_rate: test_exchange_rate_provider(),
            rate_limiter: std::sync::Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
            event_streams: Default::default(),
            dns: std::sync::Arc::new(crate::embed_domains::UnavailableDns("DNS is not available in tests".to_string())),
        };
        (state, engine)
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    async fn signed_up_and_logged_in_session_token(router: &Router, email: &str, password: &str) -> String {
        let signup = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/signup")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({ "email": email, "password": password }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(signup.status(), StatusCode::CREATED);

        let login = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({ "email": email, "password": password }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        body_json(login).await.as_object().unwrap().get("session_token").unwrap().as_str().unwrap().to_string()
    }

    async fn create_connection_with_base_currency(router: &Router, session_token: &str, base_currency: &str) -> String {
        let body = serde_json::json!({
            "platform": "custom",
            "site_url": "https://shop.example.com",
            "view_key_hex": TEST_VIEW_KEY_HEX,
            "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
            "network": "mainnet",
            "allowed_origins": [],
            "base_currency": base_currency,
        });
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connections")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        body_json(response).await.as_object().unwrap().get("connection_id").unwrap().as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn the_pos_page_renders_for_the_owning_user_with_the_stores_own_base_currency() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(&router, "pos-page@example.com", "correct horse battery staple").await;
        let id = create_connection_with_base_currency(&router, &session_token, "XMR").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{id}/pos"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("XMR"), "expected the store's own base currency shown, got: {html}");
    }

    #[tokio::test]
    async fn the_pos_page_is_a_real_404_not_a_500_for_an_unknown_connection() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(&router, "pos-page-404@example.com", "correct horse battery staple").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/dashboard/stores/nonexistent/pos")
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_pos_page_404s_for_a_connection_owned_by_a_different_user() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let owner_token = signed_up_and_logged_in_session_token(&router, "pos-owner@example.com", "correct horse battery staple").await;
        let id = create_connection_with_base_currency(&router, &owner_token, "XMR").await;

        let other_token = signed_up_and_logged_in_session_token(&router, "pos-other@example.com", "correct horse battery staple").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{id}/pos"))
                    .header("authorization", format!("Bearer {other_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "a connection owned by someone else must 404, not leak that it exists");
    }

    #[tokio::test]
    async fn the_pos_page_requires_authentication() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(&router, "pos-unauth@example.com", "correct horse battery staple").await;
        let id = create_connection_with_base_currency(&router, &session_token, "XMR").await;

        let response = router
            .oneshot(Request::builder().method("GET").uri(format!("/dashboard/stores/{id}/pos")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn creating_a_pos_order_uses_the_stores_own_base_currency_and_is_visible_on_the_dashboard() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "pos-create@example.com", "correct horse battery staple").await;
        let id = create_connection_with_base_currency(&router, &session_token, "XMR").await;

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/dashboard/stores/{id}/pos/orders"))
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::from(serde_json::json!({ "amount": "1.5" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["currency"], "XMR");
        assert_eq!(body["amount"], "1.5");
        assert_eq!(body["xmr_amount"], "1.500000000000");
        let address = body["address"].as_str().unwrap();
        assert!(!address.is_empty());
        assert!(body.get("monero_uri").is_none());
        assert!(body.get("qr_code_svg").is_none());
        let order_id = body["order_id"].as_str().unwrap().to_string();

        // Spec point 5: an order created through the POS screen is a real
        // order, visible on the normal orders list like any other.
        let orders_list = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{id}/orders"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(orders_list.status(), StatusCode::OK);
        let html = body_text(orders_list).await;
        assert!(html.contains(&order_id), "expected the POS-created order to show up in the dashboard's own orders list");
    }

    #[tokio::test]
    async fn creating_a_pos_order_with_a_note_records_it_as_the_real_merchant_order_id() {
        // Spec points 5-6: the terminal's own quick note (typically a
        // customer's name) is the same `merchant_order_id` every other
        // order-creation surface in this crate already writes - proven here
        // by checking it shows up on the real order detail page, the same
        // place `http::pay`'s own equivalent test checks.
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(&router, "pos-note@example.com", "correct horse battery staple").await;
        let id = create_connection_with_base_currency(&router, &session_token, "XMR").await;

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/dashboard/stores/{id}/pos/orders"))
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::from(serde_json::json!({ "amount": "1.0", "merchant_order_id": "Jane Doe" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["merchant_order_id"], "Jane Doe");
        let order_id = body["order_id"].as_str().unwrap().to_string();

        let detail_response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{id}/orders/{order_id}"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail_response.status(), StatusCode::OK);
        let html = body_text(detail_response).await;
        assert!(html.contains("Jane Doe"), "expected the real note shown as this order's merchant order id, got: {html}");
    }

    #[tokio::test]
    async fn creating_a_pos_order_with_a_blank_note_records_no_merchant_order_id() {
        // A note field left empty (or whitespace-only - a merchant tapping
        // it and tapping away) must not record a literal empty-string
        // `merchant_order_id` - same trim-to-`None` convention
        // `http::orders::create_order`'s own form field already follows.
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(&router, "pos-blank-note@example.com", "correct horse battery staple").await;
        let id = create_connection_with_base_currency(&router, &session_token, "XMR").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/dashboard/stores/{id}/pos/orders"))
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::from(serde_json::json!({ "amount": "1.0", "merchant_order_id": "   " }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert!(body["merchant_order_id"].is_null(), "a whitespace-only note must not become a stored merchant_order_id, got: {body}");
    }

    #[tokio::test]
    async fn creating_a_pos_order_rejects_an_empty_amount() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "pos-empty-amount@example.com", "correct horse battery staple").await;
        let id = create_connection_with_base_currency(&router, &session_token, "XMR").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/dashboard/stores/{id}/pos/orders"))
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::from(serde_json::json!({ "amount": "" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn creating_a_pos_order_rejects_a_malformed_amount() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "pos-bad-amount@example.com", "correct horse battery staple").await;
        let id = create_connection_with_base_currency(&router, &session_token, "XMR").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/dashboard/stores/{id}/pos/orders"))
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::from(serde_json::json!({ "amount": "not-a-number" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn creating_a_pos_order_404s_for_a_connection_owned_by_a_different_user() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let owner_token = signed_up_and_logged_in_session_token(&router, "pos-create-owner@example.com", "correct horse battery staple").await;
        let id = create_connection_with_base_currency(&router, &owner_token, "XMR").await;
        let other_token = signed_up_and_logged_in_session_token(&router, "pos-create-other@example.com", "correct horse battery staple").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/dashboard/stores/{id}/pos/orders"))
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {other_token}"))
                    .body(Body::from(serde_json::json!({ "amount": "1.0" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_status_endpoint_reports_pending_with_no_error_for_a_fresh_order() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(&router, "pos-status@example.com", "correct horse battery staple").await;
        let id = create_connection_with_base_currency(&router, &session_token, "XMR").await;

        let create = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/dashboard/stores/{id}/pos/orders"))
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::from(serde_json::json!({ "amount": "2.0" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let order_id = body_json(create).await["order_id"].as_str().unwrap().to_string();

        let status = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{id}/pos/orders/{order_id}/status"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let body = body_json(status).await;
        assert_eq!(body["status"], "pending");
        assert_eq!(body["confirmations"], 0);
        assert_eq!(body["is_terminal"], false);
        assert!(body["error"].is_null(), "a plain pending order must carry no error, got: {body}");
    }

    #[tokio::test]
    async fn the_events_stream_reports_every_watched_order_and_pushes_changes() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(&router, "pos-events@example.com", "correct horse battery staple").await;
        let id = create_connection_with_base_currency(&router, &session_token, "XMR").await;
        let mut order_ids = Vec::new();
        for _ in 0..2 {
            let create = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/dashboard/stores/{id}/pos/orders"))
                        .header("content-type", "application/json")
                        .header("authorization", format!("Bearer {session_token}"))
                        .body(Body::from(serde_json::json!({ "amount": "2.0" }).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            order_ids.push(body_json(create).await["order_id"].as_str().unwrap().to_string());
        }

        let events_request = |query: String, token: Option<&str>| {
            let mut builder = Request::builder().uri(format!("/dashboard/stores/{id}/pos/events?orders={query}"));
            if let Some(token) = token {
                builder = builder.header("authorization", format!("Bearer {token}"));
            }
            builder.body(Body::empty()).unwrap()
        };
        let unauthenticated = router.clone().oneshot(events_request(order_ids[0].clone(), None)).await.unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        let empty = router.clone().oneshot(events_request(String::new(), Some(&session_token))).await.unwrap();
        assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

        // An id that isn't this store's is simply never reported.
        let query = format!("{},{},pay_not_ours", order_ids[0], order_ids[1]);
        let response = router.clone().oneshot(events_request(query, Some(&session_token))).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        let (mut pending, mut parser) = (Vec::new(), crate::live::SseTestParser::default());

        let mut seen = Vec::new();
        for _ in 0..2 {
            let (event, data) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser).await.unwrap();
            assert_eq!(event, "status");
            let data: serde_json::Value = serde_json::from_str(&data).unwrap();
            assert_eq!(data["status"], "pending");
            assert!(data["error"].is_null());
            seen.push(data["order_id"].as_str().unwrap().to_string());
        }
        seen.sort();
        let mut expected = order_ids.clone();
        expected.sort();
        assert_eq!(seen, expected);

        assert!(engine.store().lock().unwrap().mark_double_spend_detected(&order_ids[1], crate::now_unix()).unwrap());
        let (event, data) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser).await.unwrap();
        assert_eq!(event, "status");
        let data: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(data["order_id"], order_ids[1].as_str());
        assert!(data["error"].as_str().unwrap().to_lowercase().contains("double-spend"), "got: {data}");
    }

    #[tokio::test]
    async fn the_status_endpoint_404s_for_an_unknown_order_id() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(&router, "pos-status-404@example.com", "correct horse battery staple").await;
        let id = create_connection_with_base_currency(&router, &session_token, "XMR").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{id}/pos/orders/nonexistent/status"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_status_endpoint_404s_for_a_connection_owned_by_a_different_user() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let owner_token = signed_up_and_logged_in_session_token(&router, "pos-status-owner@example.com", "correct horse battery staple").await;
        let id = create_connection_with_base_currency(&router, &owner_token, "XMR").await;
        let create = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/dashboard/stores/{id}/pos/orders"))
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {owner_token}"))
                    .body(Body::from(serde_json::json!({ "amount": "1.0" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let order_id = body_json(create).await["order_id"].as_str().unwrap().to_string();

        let other_token = signed_up_and_logged_in_session_token(&router, "pos-status-other@example.com", "correct horse battery staple").await;
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{id}/pos/orders/{order_id}/status"))
                    .header("authorization", format!("Bearer {other_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_pos_order_created_against_a_fiat_base_currency_is_rejected_with_no_priced_provider_configured() {
        // This test instance's `exchange_rate` is XMR-only
        // (`test_exchange_rate_provider`) - a store whose `base_currency` is
        // a fiat currency simply can't be priced on it, the same
        // "unsupported currency" outcome `http::pay::create_order`'s own
        // tests already exercise for the public endpoint. Proves the POS
        // endpoint surfaces that as a clear `400`, not a `500`.
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(&router, "pos-fiat@example.com", "correct horse battery staple").await;
        let id = create_connection_with_base_currency(&router, &session_token, "USD").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/dashboard/stores/{id}/pos/orders"))
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::from(serde_json::json!({ "amount": "10.00" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

/// [`derive_payment_error`] is pure - no engine, no database - so it's
/// unit-tested directly against hand-built [`OrderView`]s the same
/// "I/O-free logic gets its own plain unit tests" split
/// `confirmation_thresholds`'s own module doc comment already applies. A
/// real end-to-end double-spend/partial/overpaid/expired order would need
/// this crate's test engine to actually mine/fund/double-spend a real
/// transaction, which none of this crate's existing HTTP-level test
/// harnesses do (they only ever exercise the freshly-created `"pending"`
/// state) - that's the scanner crate's own job to prove, not this one's.
#[cfg(test)]
mod pure_logic_tests {
    use super::{derive_payment_error, OrderView};

    fn order_with_status(status: &str) -> OrderView {
        OrderView {
            order_id: "pay_test".to_string(),
            merchant_order_id: None,
            address: "addr".to_string(),
            xmr_amount_piconero: 1_000_000_000_000,
            amount_received_piconero: 0,
            status: status.to_string(),
            confirmations: 0,
            double_spend_detected_at: None,
            refund_address: None,
            created_at: 0,
            expires_at: 1000,
            updated_at: 0,
            first_scanned_height: None,
            last_scanned_height: None,
            currently_scanning: false,
        }
    }

    #[test]
    fn a_pending_unconfirmed_or_confirming_order_has_no_error() {
        for status in ["pending", "unconfirmed", "confirming"] {
            assert_eq!(derive_payment_error(&order_with_status(status)), None, "status {status:?} must not be an error");
        }
    }

    #[test]
    fn a_plain_paid_order_has_no_error() {
        assert_eq!(derive_payment_error(&order_with_status("paid")), None);
    }

    #[test]
    fn a_partial_payment_is_flagged_as_underpaid() {
        let error = derive_payment_error(&order_with_status("partial")).expect("partial must be flagged");
        assert!(error.to_lowercase().contains("underpaid"), "got: {error}");
    }

    #[test]
    fn an_overpaid_order_is_flagged() {
        let error = derive_payment_error(&order_with_status("overpaid")).expect("overpaid must be flagged");
        assert!(error.to_lowercase().contains("overpaid"), "got: {error}");
    }

    #[test]
    fn an_expired_order_is_flagged() {
        let error = derive_payment_error(&order_with_status("expired")).expect("expired must be flagged");
        assert!(error.to_lowercase().contains("expired"), "got: {error}");
    }

    #[test]
    fn a_double_spend_is_flagged_regardless_of_status() {
        // Double-spend detection takes priority over every other check here
        // (the `if` in `derive_payment_error` returns before the `match`
        // ever runs) - proven across several different statuses, not just
        // one, so a future reordering of that function can't silently drop
        // this precedence for some statuses but not others.
        for status in ["unconfirmed", "confirming", "paid", "partial"] {
            let mut order = order_with_status(status);
            order.double_spend_detected_at = Some(123);
            let error = derive_payment_error(&order).expect("a double-spend must always be flagged");
            assert!(error.to_lowercase().contains("double-spend"), "got: {error}");
        }
    }
}
