//! Control-plane's own real, public order-creation endpoint
//! (`docs/fx_refactor.md` Phase 1.4) - the production counterpart to
//! `orders::create_order`'s "create a test order" dashboard button, now the
//! actual path a real storefront/plugin calls to create a fiat-priced
//! order. Unauthenticated and addressed by the tenant's own `pk_...` - the
//! same already-public identifier the engine's own equivalent endpoint and
//! checkout page use, not control-plane's internal `connection_id` (which
//! nothing outside this service has ever had a reason to know). Rate-limited
//! per source IP (`http::rate_limit`) - see that module's own doc comment
//! for why control-plane needed a rate limiter at all as of this endpoint.
//!
//! Computes the XMR amount from control-plane's own exchange rate
//! (`AppState.exchange_rate`) and passes that raw `xmr_amount_piconero` to
//! the engine's own (now XMR-only, `docs/fx_refactor.md` Phase 3) public
//! order-creation endpoint - control-plane's computation is the only rate
//! computation in the whole system now. The fiat amount/currency the
//! caller asked for is recorded locally (`Db::create_order_fiat_metadata`)
//! for display purposes only; the engine never sees or stores it.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};

use crate::engine_client::EngineClientError;
use crate::now_unix;

use super::{ApiError, AppState};

#[derive(Deserialize)]
pub struct CreateOrderRequest {
    pub fiat_amount: String,
    pub fiat_currency: String,
}

/// Mirrors the engine's own `public::CreateOrderResponse` field-for-field -
/// deliberately the same shape a caller integrating against the engine
/// directly today already expects, so migrating a storefront from calling
/// the engine to calling this endpoint instead is a base-URL change, not a
/// response-parsing rewrite.
/// `fiat_amount`/`fiat_currency` echo back what the caller asked for
/// (`req.fiat_amount`/`req.fiat_currency`), not anything the engine
/// returned - the engine has no concept of fiat at all any more.
#[derive(Debug, Serialize)]
pub struct CreateOrderResponse {
    pub payment_id: String,
    pub address: String,
    pub xmr_amount_piconero: u64,
    pub fiat_amount: String,
    pub fiat_currency: String,
    pub expires_at: i64,
}

/// `POST /pay/{pk}/orders`.
pub async fn create_order(
    State(state): State<AppState>,
    Path(pk): Path<String>,
    Json(req): Json<CreateOrderRequest>,
) -> Response {
    let row = match state.db.lock().unwrap().get_store_connection_by_public_key(&pk) {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(_) => return ApiError::Internal.into_response(),
    };

    // Control-plane's own exchange rate is the only rate computation left in
    // the whole system (`docs/fx_refactor.md` Phase 3) - a real, fast `400`
    // for an unsupported currency or a malformed amount, before the engine
    // (which has no concept of fiat at all) is ever called. Dispatched by
    // *this store's own* chosen provider (`row.fx_provider`), a per-merchant
    // setting, not one shared instance-wide choice.
    let piconero_per_unit = match state.exchange_rate.piconero_per_unit(&row.fx_provider, &req.fiat_currency).await {
        Ok(Some(rate)) => rate,
        Ok(None) => return ApiError::BadRequest(format!("unsupported currency: {}", req.fiat_currency)).into_response(),
        Err(e) => {
            eprintln!("exchange rate lookup failed for connection {} (provider {:?}): {e}", row.id, row.fx_provider);
            return ApiError::Internal.into_response();
        }
    };
    let xmr_amount_piconero = match shared::exchange_rate::compute_xmr_amount(&req.fiat_amount, piconero_per_unit) {
        Ok(amount) => amount,
        Err(e) => return ApiError::BadRequest(e.to_string()).into_response(),
    };

    match state.engine_client.create_order(&pk, xmr_amount_piconero).await {
        Ok(order) => {
            // Best-effort: a failure to record the local metadata row must
            // never fail an order that the engine has *already* genuinely
            // created - the order is real either way, and the customer is
            // already looking at (or about to be redirected to) a real
            // payment address. Losing this one local record is a strictly
            // smaller problem than telling a customer their real order
            // failed when it didn't.
            if let Err(e) = state.db.lock().unwrap().create_order_fiat_metadata(
                &row.id,
                &order.payment_id,
                &req.fiat_currency,
                &req.fiat_amount,
                piconero_per_unit,
                &row.fx_provider,
                now_unix(),
            ) {
                eprintln!(
                    "failed to record local fiat metadata for order {} on connection {}: {e} - the real order \
                     still exists on the engine and this response is still correct, but its fiat display on \
                     control-plane's own dashboard/checkout page will be missing",
                    order.payment_id, row.id
                );
            }

            Json(CreateOrderResponse {
                payment_id: order.payment_id,
                address: order.address,
                xmr_amount_piconero: order.xmr_amount_piconero,
                fiat_amount: req.fiat_amount,
                fiat_currency: req.fiat_currency,
                expires_at: order.expires_at,
            })
            .into_response()
        }
        // The engine's own validation (an unconfigured network, its own
        // unsupported-currency check) - surfaced verbatim, same convention
        // every other caller of `EngineClient` in this crate already
        // applies to the engine's real `400`s.
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            ApiError::BadRequest(message).into_response()
        }
        Err(_) => ApiError::Internal.into_response(),
    }
}

const CLIENT_LIBRARY_JS: &str = include_str!("../../static/moneropay-client.js");

/// `GET /static/moneropay-client.js` - the thin embed library a merchant's
/// static site `<script src>`s (`docs/fx_refactor.md` decision 3 / Phase
/// 4.3). Moved here from the engine, which no longer has any checkout UI or
/// fiat concept for it to talk to - this version's `createOrder`/`mount`
/// call control-plane's own `/pay/{pk}/orders` and
/// `/pay/{pk}/orders/{payment_id}` instead. Served from this binary rather
/// than a CDN so a self-hoster's static site has no third-party dependency
/// in its payment path, same reasoning the engine's original had.
pub async fn client_library() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "text/javascript; charset=utf-8")], CLIENT_LIBRARY_JS)
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

    /// Same fixed-scalar construction every other module's own tests use -
    /// see `connections.rs` for why these particular values pass the
    /// engine's real wallet-material validation.
    const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
    const TEST_ENCRYPTION_KEY: [u8; 32] = [7u8; 32];
    const TEST_CURRENCY: &str = "USD";
    const TEST_RATE_PICONERO_PER_UNIT: u64 = 1_000_000_000_000;

    fn test_exchange_rate_provider() -> std::sync::Arc<crate::exchange_rate_config::ExchangeRateProviders> {
        std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::fixed_only(std::collections::HashMap::from([(
            TEST_CURRENCY.to_string(),
            TEST_RATE_PICONERO_PER_UNIT,
        )])))
    }

    async fn test_state_with_real_engine() -> (AppState, engine_test_support::TestEngineHandle) {
        let engine = engine_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .spawn()
            .await;
        let engine_client = EngineClient::new(format!("http://{}", engine.addr));
        let state = AppState {
            db: Db::open_in_memory().unwrap().into_shared(),
            engine_client,
            encryption_key: TEST_ENCRYPTION_KEY,
            templates: std::sync::Arc::new(crate::templates::TemplateEngine::new().unwrap()),
            status_cache: crate::http::status_page::new_status_cache(),
            exchange_rate: test_exchange_rate_provider(),
            rate_limiter: std::sync::Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
        };
        (state, engine)
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
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

    /// Creates a real `store_connections` row (and a real tenant on the
    /// real spawned engine) for the given session, returning its public
    /// key - the identifier this module's own endpoint is addressed by.
    async fn create_connection(router: &Router, session_token: &str) -> String {
        let body = serde_json::json!({
            "platform": "custom",
            "site_url": "https://shop.example.com",
            "view_key_hex": TEST_VIEW_KEY_HEX,
            "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
            "network": "mainnet",
            "allowed_origins": [],
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
        body_json(response).await.as_object().unwrap().get("public_key").unwrap().as_str().unwrap().to_string()
    }

    fn create_order_request(pk: &str, fiat_amount: &str, fiat_currency: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(format!("/pay/{pk}/orders"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({ "fiat_amount": fiat_amount, "fiat_currency": fiat_currency }).to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn creating_a_real_order_through_the_public_endpoint_returns_a_real_address_and_payment_id() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "pay-endpoint@example.com", "correct horse battery staple").await;
        let pk = create_connection(&router, &session_token).await;

        let response = router.oneshot(create_order_request(&pk, "25.00", TEST_CURRENCY)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "expected the real engine to accept and create the order");
        let body = body_json(response).await;
        let obj = body.as_object().unwrap();
        assert!(obj.get("payment_id").unwrap().as_str().unwrap().starts_with("pay_"));
        assert!(!obj.get("address").unwrap().as_str().unwrap().is_empty());
        assert_eq!(obj.get("fiat_currency").unwrap().as_str().unwrap(), TEST_CURRENCY);
        assert_eq!(obj.get("fiat_amount").unwrap().as_str().unwrap(), "25.00");
    }

    /// The other half of the test above: proves the local fiat-metadata
    /// row was actually recorded (not just the engine's own response
    /// echoed back) by reading it straight out of control-plane's own `Db`.
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
        let pk = create_connection(&router, &session_token).await;

        let response = router.oneshot(create_order_request(&pk, "10.00", TEST_CURRENCY)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let payment_id = body.as_object().unwrap().get("payment_id").unwrap().as_str().unwrap().to_string();

        let connection_id =
            state.db.lock().unwrap().get_store_connection_by_public_key(&pk).unwrap().unwrap().id;
        let metadata = state.db.lock().unwrap().get_order_fiat_metadata(&connection_id, &payment_id).unwrap();
        let metadata = metadata.expect("expected a real local fiat-metadata row for the order just created");
        assert_eq!(metadata.fiat_currency, TEST_CURRENCY);
        assert_eq!(metadata.fiat_amount, "10.00");
        assert_eq!(metadata.piconero_per_unit, TEST_RATE_PICONERO_PER_UNIT);
    }

    #[tokio::test]
    async fn creating_an_order_for_an_unknown_public_key_returns_404() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let response = router.oneshot(create_order_request("pk_nonexistent", "25.00", TEST_CURRENCY)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn creating_an_order_with_an_unsupported_currency_is_rejected_before_ever_reaching_the_engine() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint-bad-currency@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;

        let response = router.oneshot(create_order_request(&pk, "25.00", "NOTREAL")).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert!(body["error"].as_str().unwrap().contains("unsupported currency"), "got: {body}");
    }

    #[tokio::test]
    async fn creating_an_order_with_a_malformed_amount_is_a_clear_400_not_a_500() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint-bad-amount@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;

        let response = router.oneshot(create_order_request(&pk, "not-a-number", TEST_CURRENCY)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn client_library_is_served_as_javascript_and_calls_control_planes_own_endpoints() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let req = Request::builder().method("GET").uri("/static/moneropay-client.js").body(Body::empty()).unwrap();
        let response = router.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("content-type").unwrap(), "text/javascript; charset=utf-8");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let js = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(js.contains("MoneroPay"));
        assert!(js.contains("createOrder"));
        assert!(js.contains("mount"));
        // The real point of this rewrite (`docs/fx_refactor.md` decision 3):
        // it must call control-plane's own `/pay/{pk}/orders` endpoint, not
        // the engine's old `/api/v1/t/{pk}/orders` - and the mounted iframe
        // must point at control-plane's own checkout page, not the engine's
        // now-deleted `/pay/v1/{pk}/{payment_id}`.
        assert!(js.contains("/pay/\" + encodeURIComponent(publicKey) + \"/orders"), "should call control-plane's own order-creation endpoint, got: {js}");
        assert!(js.contains("/orders/\" + encodeURIComponent(paymentId)"), "should iframe control-plane's own checkout page, got: {js}");
        assert!(!js.contains("/api/v1/t/"), "must not reference the engine's own API directly: {js}");
        assert!(!js.contains("/pay/v1/"), "must not reference the engine's own (deleted) checkout route: {js}");
    }
}
