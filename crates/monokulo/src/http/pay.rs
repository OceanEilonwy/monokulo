//! Control-plane's own real, public order-creation endpoint
//! (`docs/fx_refactor.md` Phase 1.4) - the production counterpart to
//! `orders::create_order`'s "create a test order" dashboard button, now the
//! actual path a real storefront/plugin calls to create a fiat-priced
//! order. Unauthenticated and addressed by the tenant's own `pk_...` - the
//! same already-public identifier the engine's own equivalent endpoint and
//! checkout page use, not monokulo's internal `connection_id` (which
//! nothing outside this service has ever had a reason to know). Rate-limited
//! per source IP (`http::rate_limit`) - see that module's own doc comment
//! for why monokulo needed a rate limiter at all as of this endpoint.
//!
//! Computes the XMR amount from monokulo's own exchange rate
//! (`AppState.exchange_rate`) and passes that raw `xmr_amount_piconero` to
//! the engine's own (now XMR-only, `docs/fx_refactor.md` Phase 3) public
//! order-creation endpoint - monokulo's computation is the only rate
//! computation in the whole system now. The fiat amount/currency the
//! caller asked for is recorded locally (`Db::create_order_currency_metadata`)
//! for display purposes only; the engine never sees or stores it.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};

use crate::engine_client::EngineClientError;
use crate::now_unix;

use super::{ApiError, AppState};

#[derive(Deserialize)]
pub struct CreateOrderRequest {
    pub amount: String,
    pub currency: String,
    /// Optional caller-supplied identifier (a storefront's own order/cart
    /// id) - passed straight through to the engine's own `create_order`
    /// (`EngineClient::create_order`'s own `merchant_order_id` parameter)
    /// and shown on the dashboard's order detail page, so a merchant can
    /// match a Monokulo order back to their own records. `None`/omitted
    /// when a caller doesn't have one, same `Option` default-to-`None`
    /// convention this whole codebase already uses for an optional field.
    #[serde(default)]
    pub merchant_order_id: Option<String>,
}

/// Mirrors the engine's own `public::CreateOrderResponse` field-for-field -
/// deliberately the same shape a caller integrating against the engine
/// directly today already expects, so migrating a storefront from calling
/// the engine to calling this endpoint instead is a base-URL change, not a
/// response-parsing rewrite.
/// `amount`/`currency` echo back what the caller asked for
/// (`req.amount`/`req.currency`), not anything the engine
/// returned - the engine has no concept of fiat at all any more.
#[derive(Debug, Serialize)]
pub struct CreateOrderResponse {
    pub order_id: String,
    pub address: String,
    pub xmr_amount_piconero: u64,
    pub amount: String,
    pub currency: String,
    pub merchant_order_id: Option<String>,
    pub expires_at: i64,
}

/// `POST /pay/{pk}/orders`.
pub async fn create_order(
    State(state): State<AppState>,
    Path(pk): Path<String>,
    Json(req): Json<CreateOrderRequest>,
) -> Response {
    let policy_lock = crate::confirmation_thresholds::policy_lock(&pk);
    let _policy_guard = policy_lock.lock().await;
    let row = match state.db.lock().unwrap().get_store_connection_by_public_key(&pk) {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(_) => return ApiError::Internal.into_response(),
    };

    // Selection-time validation first, entirely independent of whether any
    // provider can actually price it (`crate::currencies`'s own doc comment)
    // - "unknown currency" (this doesn't exist at all) is a genuinely
    // different, clearer error than "unsupported currency" (a real currency
    // this instance just can't get a live rate for right now), so the two
    // get distinct messages rather than being collapsed into one.
    let currency_known = crate::currencies::is_known_currency(&state.db.lock().unwrap(), &req.currency);
    match currency_known {
        Ok(true) => {}
        Ok(false) => return ApiError::BadRequest(format!("unknown currency: {}", req.currency)).into_response(),
        Err(_) => return ApiError::Internal.into_response(),
    }

    // Control-plane's own exchange rate is the only rate computation left in
    // the whole system (`docs/fx_refactor.md` Phase 3) - a real, fast `400`
    // for an unsupported currency or a malformed amount, before the engine
    // (which has no concept of currency at all) is ever called.
    // `"XMR"` always uses the trivial identity rate regardless of this
    // store's chosen `fx_provider`; every other currency is dispatched by
    // *this store's own* chosen provider, a per-merchant setting, not one
    // shared instance-wide choice.
    let (piconero_per_unit, provider) = match state.exchange_rate.piconero_per_unit_for(&row, &req.currency).await {
        Ok(Some(result)) => result,
        Ok(None) => return ApiError::BadRequest(format!("unsupported currency: {}", req.currency)).into_response(),
        Err(crate::exchange_rate_config::ExchangeRateLookupError::ProviderNotConfigured(_)) => {
            // Not a real failure - this store's provider (or no provider at
            // all) simply can't price this currency on this instance, same
            // user-facing meaning as `Ok(None)` above.
            return ApiError::BadRequest(format!("unsupported currency: {}", req.currency)).into_response();
        }
        Err(e) => {
            eprintln!("exchange rate lookup failed for connection {} (currency {:?}): {e}", row.id, req.currency);
            return ApiError::Internal.into_response();
        }
    };
    let xmr_amount_piconero = match shared::exchange_rate::compute_order_amount(&req.currency, &req.amount, piconero_per_unit) {
        Ok(amount) => amount,
        Err(e) => return ApiError::BadRequest(e.to_string()).into_response(),
    };

    let sk = match crate::crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted) {
        Ok(sk) => sk,
        Err(_) => return ApiError::Internal.into_response(),
    };
    let resolution =
        match crate::confirmation_thresholds::resolve_for_order(&state, &row, &sk, &req.currency, piconero_per_unit, xmr_amount_piconero).await {
            Ok(resolution) => resolution,
            Err(message) => return ApiError::BadRequest(message).into_response(),
        };

    match state
        .engine_client
        .create_order(&sk, xmr_amount_piconero, req.merchant_order_id.clone(), Some(resolution.confirmations_required))
        .await
    {
        Ok(order) => {
            // Best-effort: a failure to record the local metadata row must
            // never fail an order that the engine has *already* genuinely
            // created - the order is real either way, and the customer is
            // already looking at (or about to be redirected to) a real
            // payment address. Losing this one local record is a strictly
            // smaller problem than telling a customer their real order
            // failed when it didn't.
            if let Err(e) = state.db.lock().unwrap().create_order_currency_metadata(
                &row.id,
                &order.order_id,
                &req.currency,
                &req.amount,
                piconero_per_unit,
                provider,
                now_unix(),
                &resolution.base_currency,
                resolution.base_currency_piconero_per_unit,
                resolution.confirmations_required,
            ) {
                eprintln!(
                    "failed to record local fiat metadata for order {} on connection {}: {e} - the real order \
                     still exists on the engine and this response is still correct, but its fiat display on \
                     monokulo's own dashboard/checkout page will be missing",
                    order.order_id, row.id
                );
            }

            Json(CreateOrderResponse {
                order_id: order.order_id,
                address: order.address,
                xmr_amount_piconero: order.xmr_amount_piconero,
                amount: req.amount,
                currency: req.currency,
                merchant_order_id: req.merchant_order_id,
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

const CLIENT_LIBRARY_JS: &str = include_str!("../../static/monokulo-client.js");

/// `GET /static/monokulo-client.js` - the thin embed library a merchant's
/// static site `<script src>`s (`docs/fx_refactor.md` decision 3 / Phase
/// 4.3). Moved here from the engine, which no longer has any checkout UI or
/// fiat concept for it to talk to - this version's `createOrder`/`mount`
/// call monokulo's own `/pay/{pk}/orders` and
/// `/pay/{pk}/orders/{order_id}` instead. Served from this binary rather
/// than a CDN so a self-hoster's static site has no third-party dependency
/// in its payment path, same reasoning the engine's original had.
pub async fn client_library() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "text/javascript; charset=utf-8")], CLIENT_LIBRARY_JS)
}

pub async fn checkout_script() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../../static/checkout.js"))
}

pub async fn qr_decoder_script() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../../static/jsQR.js"))
}

const LOGO_SVG: &str = include_str!("../../static/logo.svg");
const LOGO_INVERTED_SVG: &str = include_str!("../../static/logo-inverted.svg");
const FAVICON_SVG: &str = include_str!("../../static/favicon.svg");

/// `GET /static/logo.svg` - the full Monokulo mark, ink-on-paper, for use
/// over the page's own light background (`landing.html.hbs`'s hero).
/// Served the same way as [`client_library`] (a plain, unauthenticated
/// static asset baked into the binary) for the same reason: no third-party
/// CDN dependency in a page real customers may end up on.
pub async fn logo_svg() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "image/svg+xml")], LOGO_SVG)
}

/// `GET /static/logo-inverted.svg` - the same mark recolored for
/// `_nav.html.hbs`'s dark (`--ink`) bar - see that file's own doc comment.
pub async fn logo_inverted_svg() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "image/svg+xml")], LOGO_INVERTED_SVG)
}

/// `GET /static/favicon.svg` - the simplified, small-size version of the
/// same mark, linked from `_styles.html.hbs` (`<link rel="icon">`) so every
/// page that includes the `styles` partial gets a browser-tab icon for
/// free.
pub async fn favicon_svg() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "image/svg+xml")], FAVICON_SVG)
}

const MANROPE_500_WOFF2: &[u8] = include_bytes!("../../static/manrope-500.woff2");
const MANROPE_700_WOFF2: &[u8] = include_bytes!("../../static/manrope-700.woff2");
const MANROPE_800_WOFF2: &[u8] = include_bytes!("../../static/manrope-800.woff2");

/// `GET /static/manrope-{500,700,800}.woff2` - the UI typeface
/// (`_styles.html.hbs`'s `@font-face`), self-hosted for the same reason as
/// the logo/favicon/client-library assets above: no third-party CDN
/// dependency on any page. This one matters more than most - a Google
/// Fonts `<link>` would leak every visitor's IP to Google on every page
/// load, checkout included, which is the wrong tradeoff for a
/// privacy-focused payment tool. Latin subset only (this UI has no other
/// script), matching what a Google Fonts request for this weight range
/// would itself have served.
pub async fn manrope_500_woff2() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "font/woff2")], MANROPE_500_WOFF2)
}
pub async fn manrope_700_woff2() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "font/woff2")], MANROPE_700_WOFF2)
}
pub async fn manrope_800_woff2() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "font/woff2")], MANROPE_800_WOFF2)
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
    // This module's own tests deliberately exercise a real fiat quote (via a
    // local mock Coingecko server below), not just XMR - `http::pay::create_order`
    // is the real production storefront-facing endpoint, so its own tests are
    // the ones that should prove a real fiat currency actually works end to
    // end, unlike most other modules' tests (see `orders.rs`'s own doc
    // comment on why those use `"XMR"` instead).
    const TEST_CURRENCY: &str = "USD";
    // A mock price of exactly $1.00 makes the resulting piconero-per-unit
    // exactly 1e12 (`1_000_000_000_000.0 / 1.0`), a clean round number to
    // assert against without any floating-point rounding to account for.
    const TEST_RATE_PICONERO_PER_UNIT: u64 = 1_000_000_000_000;

    /// Spins up a real local HTTP server standing in for Coingecko - same
    /// no-mocking-library pattern `shared::exchange_rate`'s own tests use.
    /// Returns the base URL a `CoingeckoRateProvider` can be pointed at.
    async fn spawn_mock_coingecko() -> String {
        async fn price() -> axum::response::Response {
            use axum::response::IntoResponse;
            ([("content-type", "application/json")], r#"{"monero":{"usd":1.0}}"#).into_response()
        }
        let app = Router::new().route("/api/v3/simple/price", axum::routing::get(price));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    async fn test_exchange_rate_provider() -> std::sync::Arc<crate::exchange_rate_config::ExchangeRateProviders> {
        let base_url = spawn_mock_coingecko().await;
        std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::coingecko_only(base_url))
    }

    async fn test_state_with_real_engine() -> (AppState, scanner_test_support::TestEngineHandle) {
        let engine = scanner_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .spawn()
            .await;
        let engine_client = EngineClient::new(format!("http://{}", engine.addr));
        let state = AppState {
            db: { let db = Db::open_in_memory().unwrap(); db.seed_test_admin(); db.into_shared() },
            engine_client,
            encryption_key: TEST_ENCRYPTION_KEY,
            status_cache: crate::http::status_page::new_status_cache(),
            exchange_rate: test_exchange_rate_provider().await,
            rate_limiter: std::sync::Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
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
            "base_currency": "XMR",
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

    fn create_order_request(pk: &str, amount: &str, currency: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(format!("/pay/{pk}/orders"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({ "amount": amount, "currency": currency }).to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn creating_a_real_order_through_the_public_endpoint_returns_a_real_address_and_order_id() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "pay-endpoint@example.com", "correct horse battery staple").await;
        let pk = create_connection(&router, &session_token).await;

        let response = router.oneshot(create_order_request(&pk, "25.00", TEST_CURRENCY)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "expected the real engine to accept and create the order");
        let body = body_json(response).await;
        let obj = body.as_object().unwrap();
        assert!(obj.get("order_id").unwrap().as_str().unwrap().starts_with("order_"));
        assert!(!obj.get("address").unwrap().as_str().unwrap().is_empty());
        assert_eq!(obj.get("currency").unwrap().as_str().unwrap(), TEST_CURRENCY);
        assert_eq!(obj.get("amount").unwrap().as_str().unwrap(), "25.00");
    }

    #[tokio::test]
    async fn threshold_database_failure_rejects_order_even_with_zero_conf_default() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(&router, "threshold-db-failure@example.com", "correct horse battery staple").await;
        let pk = create_connection(&router, &session_token).await;
        let row = state.db.lock().unwrap().get_store_connection_by_public_key(&pk).unwrap().unwrap();
        let sk = crate::crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted).unwrap();
        state.engine_client.set_confirmations_required(&sk, 0).await.unwrap();
        state.db.lock().unwrap().break_confirmation_thresholds_for_test();

        let response = router.oneshot(create_order_request(&pk, "25.00", TEST_CURRENCY)).await.unwrap();
        assert_ne!(response.status(), StatusCode::OK);
        let tenant_id = engine.store().lock().unwrap().find_tenant_by_public_key(&pk).unwrap().unwrap().id;
        assert!(engine.store().lock().unwrap().list_orders(&tenant_id, None, 10, None).unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_order_waits_for_its_stores_policy_edit() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(&router, "policy-edit-race@example.com", "correct horse battery staple").await;
        let pk = create_connection(&router, &session_token).await;
        let policy_lock = crate::confirmation_thresholds::policy_lock(&pk);
        let guard = policy_lock.lock().await;
        let request = create_order_request(&pk, "25.00", TEST_CURRENCY);
        let mut task = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
        assert!(tokio::time::timeout(std::time::Duration::from_millis(100), &mut task).await.is_err());
        let tenant_id = engine.store().lock().unwrap().find_tenant_by_public_key(&pk).unwrap().unwrap().id;
        assert!(engine.store().lock().unwrap().list_orders(&tenant_id, None, 10, None).unwrap().is_empty());
        drop(guard);
        assert_eq!(task.await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn malformed_stored_threshold_cannot_fall_back_to_zero_conf() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(&router, "malformed-threshold@example.com", "correct horse battery staple").await;
        let pk = create_connection(&router, &session_token).await;
        let row = state.db.lock().unwrap().get_store_connection_by_public_key(&pk).unwrap().unwrap();
        let sk = crate::crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted).unwrap();
        state.engine_client.set_confirmations_required(&sk, 0).await.unwrap();
        state.db.lock().unwrap().create_confirmation_threshold("corrupt", &row.id, "not-an-amount", 20, crate::now_unix()).unwrap();
        let response = router.oneshot(create_order_request(&pk, "25.00", TEST_CURRENCY)).await.unwrap();
        assert_ne!(response.status(), StatusCode::OK);
        let tenant_id = engine.store().lock().unwrap().find_tenant_by_public_key(&pk).unwrap().unwrap().id;
        assert!(engine.store().lock().unwrap().list_orders(&tenant_id, None, 10, None).unwrap().is_empty());
    }

    /// A real, previously-missing capability: `EngineClient::create_order`
    /// used to silently drop `merchant_order_id` no matter what a caller
    /// asked for - every order's own merchant order id always showed as
    /// unset on the dashboard regardless. Proves it's genuinely recorded on
    /// the engine now (not just echoed by monokulo), by reading it
    /// back through the real dashboard order-detail page, not just this
    /// endpoint's own response.
    #[tokio::test]
    async fn creating_a_real_order_with_a_merchant_order_id_records_it_on_the_engine() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint-merchant-order-id@example.com",
            "correct horse battery staple",
        )
        .await;

        let connect_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connections")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::from(
                        serde_json::json!({
                            "platform": "custom",
                            "site_url": "https://shop.example.com",
                            "view_key_hex": TEST_VIEW_KEY_HEX,
                            "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
                            "network": "mainnet",
                            "allowed_origins": [],
                            "base_currency": "XMR",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(connect_response.status(), StatusCode::CREATED);
        let connect_body = body_json(connect_response).await;
        let pk = connect_body["public_key"].as_str().unwrap().to_string();
        let connection_id = connect_body["connection_id"].as_str().unwrap().to_string();

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/pay/{pk}/orders"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "amount": "25.00", "currency": TEST_CURRENCY, "merchant_order_id": "order-1234" })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let order_id = body["order_id"].as_str().unwrap().to_string();
        assert_eq!(body["merchant_order_id"], "order-1234", "expected the real merchant_order_id echoed back, got: {body}");

        let detail_response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{connection_id}/orders/{order_id}"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail_response.status(), StatusCode::OK);
        let html = body_text(detail_response).await;
        assert!(html.contains("order-1234"), "expected the real merchant_order_id shown on the dashboard, got: {html}");
    }

    /// The other half of the test above: proves the local fiat-metadata
    /// row was actually recorded (not just the engine's own response
    /// echoed back) by reading it straight out of monokulo's own `Db`.
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
        let order_id = body.as_object().unwrap().get("order_id").unwrap().as_str().unwrap().to_string();

        let connection_id =
            state.db.lock().unwrap().get_store_connection_by_public_key(&pk).unwrap().unwrap().id;
        let metadata = state.db.lock().unwrap().get_order_currency_metadata(&connection_id, &order_id).unwrap();
        let metadata = metadata.expect("expected a real local fiat-metadata row for the order just created");
        assert_eq!(metadata.currency, TEST_CURRENCY);
        assert_eq!(metadata.amount, "10.00");
        assert_eq!(metadata.piconero_per_unit, TEST_RATE_PICONERO_PER_UNIT);

        // This store's base currency ("XMR", `create_connection`'s own
        // default) differs from the order's own currency ("USD"), so a
        // real second rate lookup (for "XMR" itself - always the identity
        // rate, regardless of provider, same as
        // `an_xmr_order_is_always_priced_at_the_identity_rate_regardless_of_the_stores_provider`
        // in `exchange_rate_config`) must have run and been snapshotted
        // separately from the order's own USD rate above.
        assert_eq!(metadata.store_base_currency, Some("XMR".to_string()));
        assert_eq!(metadata.base_currency_piconero_per_unit, Some(TEST_RATE_PICONERO_PER_UNIT));
        assert_eq!(metadata.confirmations_required_applied, Some(10), "no custom threshold exists, so the tenant's own default (10) applies");
    }

    #[tokio::test]
    async fn creating_an_order_for_an_unknown_public_key_returns_404() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let response = router.oneshot(create_order_request("pk_nonexistent", "25.00", TEST_CURRENCY)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn creating_an_order_with_an_unknown_currency_is_rejected_before_ever_reaching_the_engine() {
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
        assert!(body["error"].as_str().unwrap().contains("unknown currency"), "got: {body}");
    }

    /// The other half of the same two-stage split (`crate::currencies`'s own
    /// doc comment): `EUR` is a perfectly real, known currency - this test's
    /// own `spawn_mock_coingecko` only ever prices `"usd"` (a fixed stub
    /// response), so a request for `EUR` genuinely has no rate available.
    /// That must surface as a distinct "unsupported", not "unknown",
    /// currency error.
    #[tokio::test]
    async fn creating_an_order_with_a_known_but_provider_unsupported_currency_gets_a_distinct_error() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint-unsupported-currency@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;

        let response = router.oneshot(create_order_request(&pk, "25.00", "EUR")).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert!(body["error"].as_str().unwrap().contains("unsupported currency"), "got: {body}");
    }

    /// Same `create_connection` shape, but with a caller-chosen
    /// `base_currency` rather than the hardcoded `"XMR"` - needed by the
    /// threshold-resolution tests below, which specifically want a base
    /// currency this test module's own mock Coingecko *can't* price (it
    /// only ever stubs `"usd"`).
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
        body_json(response).await.as_object().unwrap().get("public_key").unwrap().as_str().unwrap().to_string()
    }

    /// The store's own base currency ("EUR", a perfectly real known
    /// currency - `crate::currencies`'s selection-time check happily
    /// accepted it when this store was created) turns out to have no
    /// available rate provider at resolution time (this test module's mock
    /// Coingecko only ever prices `"usd"`) - resolving the confirmation
    /// threshold needs a real EUR rate to convert the order's own amount
    /// into base-currency terms, and there isn't one. The order-currency
    /// itself ("USD") is perfectly priceable; it's specifically the base
    /// currency conversion that fails - proving the "usable" check really
    /// is a separate, later concern from "known" (`crate::currencies`'s own
    /// doc comment, and `confirmation_thresholds::resolve_for_order`'s).
    #[tokio::test]
    async fn creating_an_order_is_rejected_when_the_stores_own_base_currency_has_no_available_rate_provider() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pay-endpoint-unpriceable-base-currency@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection_with_base_currency(&router, &session_token, "EUR").await;

        let response = router.clone().oneshot(create_order_request(&pk, "25.00", "USD")).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert!(
            body["error"].as_str().unwrap().contains("EUR"),
            "expected a clear error naming the unpriceable base currency, got: {body}"
        );

        // The engine must never have created a real order at all - threshold
        // resolution runs *before* `EngineClient::create_order`, so a
        // failure here must leave no ghost order behind on the engine.
        let store = engine.store().lock().unwrap();
        let tenant_id = store.find_tenant_by_public_key(&pk).unwrap().unwrap().id;
        let orders = store.list_orders(&tenant_id, None, 100, None).unwrap();
        assert!(orders.is_empty(), "expected no order to have been created on the real engine, got: {orders:?}");
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
    async fn client_library_is_served_as_javascript_and_calls_monokulos_own_endpoints() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let req = Request::builder().method("GET").uri("/static/monokulo-client.js").body(Body::empty()).unwrap();
        let response = router.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("content-type").unwrap(), "text/javascript; charset=utf-8");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let js = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(js.contains("Monokulo"));
        assert!(js.contains("createOrder"));
        assert!(js.contains("mount"));
        // The real point of this rewrite (`docs/fx_refactor.md` decision 3):
        // it must call monokulo's own `/pay/{pk}/orders` endpoint, not
        // the engine's old `/api/v1/t/{pk}/orders` - and the mounted iframe
        // must point at monokulo's own checkout page, not the engine's
        // now-deleted `/pay/v1/{pk}/{order_id}`.
        assert!(js.contains("/pay/\" + encodeURIComponent(publicKey) + \"/orders"), "should call monokulo's own order-creation endpoint, got: {js}");
        assert!(js.contains("/orders/\" + encodeURIComponent(orderId)"), "should iframe monokulo's own checkout page, got: {js}");
        assert!(!js.contains("/api/v1/t/"), "must not reference the engine's own API directly: {js}");
        assert!(!js.contains("/pay/v1/"), "must not reference the engine's own (deleted) checkout route: {js}");
        // The iframe does not post messages back, so mount() drives
        // callbacks through its own status request even when presentation
        // query parameters are present on the iframe URL.
        assert!(!js.contains("postMessage"), "the embed library must not depend on the iframe posting a message any more, got: {js}");
        assert!(js.contains(r#"var statusUrl = checkoutUrl + "/status";"#), "expected mount() to poll the status endpoint directly, got: {js}");
        assert!(js.contains("options.refund === false"));
        for asset in ["/static/checkout.js", "/static/jsQR.js"] {
            let response = router.clone().oneshot(
                Request::builder().uri(asset).body(Body::empty()).unwrap()
            ).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{asset}");
            assert_eq!(response.headers().get("content-type").unwrap(), "text/javascript; charset=utf-8");
        }
        // The checkout page and whatever embeds it (the POS terminal, this
        // library, a merchant's own page) stay independent: each follows the
        // order itself, and neither talks to the other.
        let checkout_js = include_str!("../../static/checkout.js");
        assert!(!checkout_js.contains("postMessage") && !checkout_js.contains("window.parent") && !checkout_js.contains("window.top"), "checkout.js must not know about its embedder");
    }
}
