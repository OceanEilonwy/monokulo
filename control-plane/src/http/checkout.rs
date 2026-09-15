//! Control-plane's own real, public checkout/payment page
//! (`docs/fx_refactor.md` Phase 2) - moved here from the engine (whose own
//! equivalent, `src/http/public.rs::payment_page` at the repo root, is
//! scheduled for full removal in that same document's Phase 4). The
//! engine's public status API (`GET /api/v1/t/{pk}/orders/{payment_id}`)
//! deliberately carries no `payments` list at all (confirmed by reading
//! `OrderStatusResponse`), so this reuses the *admin* endpoint instead
//! (`EngineClient::get_order_detail`, already built and already used by
//! the dashboard's own order-detail page) via the connection's own
//! decrypted `sk_...`, entirely server-side - the browser never sees it,
//! and the engine needed zero changes to support this page.
//!
//! Deliberately **no per-tenant template customization** (`docs/fx_refactor.md`
//! decision 1, dropped outright) and **no site nav** (`{{> nav}}`) - a
//! merchant embeds this page in an iframe inside their *own* checkout flow;
//! MoneroPay's own site navigation has no business appearing inside it. It
//! still uses the shared `_styles` partial for consistent typography/color,
//! just not the nav.
//!
//! Fiat display prefers control-plane's own locally recorded quote
//! (`Db::get_order_fiat_metadata`) over the engine's own (still-present,
//! soon-removed) `OrderView.fiat_amount`/`fiat_currency` fields - forward-
//! compatible with Phase 3/4, when the engine's own copy disappears
//! entirely and this page needs no further change at all. Falls back to
//! the engine's copy only for an order that predates this feature, or was
//! created directly against the engine rather than through control-plane's
//! own `http::pay` endpoint.

use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::http::StatusCode;
use qrcode::render::svg;
use qrcode::QrCode;
use serde::Serialize;

use crate::db::StoreConnectionRow;
use crate::engine_client::{EngineClientError, OrderDetailResponse};
use crate::templates::{CheckoutPaymentViewModel, CheckoutViewModel};

use super::{ApiError, AppState};

/// `pending`/`unconfirmed`/`confirming`/`partial` are still "in progress";
/// `paid`/`overpaid`/`expired` are terminal - used to decide whether the
/// page should keep polling. Mirrors the engine's own (soon-removed)
/// `templates::status_label` - presentation logic, duplicated once rather
/// than shared, since it's a 10-line match statement used in exactly one
/// place per crate, not obviously worth a `shared` module of its own.
fn status_label(status: &str) -> (&'static str, &'static str, bool) {
    match status {
        "pending" => ("Waiting for payment", "status-pending", false),
        "unconfirmed" => ("Payment seen, unconfirmed", "status-unconfirmed", false),
        "confirming" => ("Confirming", "status-confirming", false),
        "partial" => ("Partial payment received", "status-partial", false),
        "paid" => ("Paid", "status-paid", true),
        "overpaid" => ("Overpaid", "status-paid", true),
        "expired" => ("Expired", "status-expired", true),
        _ => ("Unknown", "status-unknown", true),
    }
}

fn short_txid(txid: &str) -> String {
    if txid.len() <= 16 {
        return txid.to_string();
    }
    format!("{}…{}", &txid[..8], &txid[txid.len() - 6..])
}

/// Same SVG-trimming/accessibility treatment as the engine's own (soon-
/// removed) `qr_svg_for_html` - see that function's own doc comment
/// (`src/http/public.rs` at the repo root) for the full reasoning, ported
/// verbatim.
fn qr_svg_for_html(data: &str) -> Result<String, ApiError> {
    let full = QrCode::new(data.as_bytes())
        .map_err(|e| ApiError::BadRequest(format!("failed to encode QR code: {e}")))?
        .render::<svg::Color>()
        .build();
    let svg = match full.find("<svg") {
        Some(idx) => &full[idx..],
        None => &full[..],
    };
    Ok(svg.replacen("<svg", r#"<svg role="presentation" aria-hidden="true" focusable="false""#, 1))
}

enum LoadError {
    NotFound,
    Internal,
}

/// Shared by both routes below - looks up the connection by `pk`, decrypts
/// its `sk_...` once (the caller may need it again, e.g. for a follow-up
/// `get_tenant` call - handed back rather than re-decrypted), and fetches
/// the order's full detail from the engine's admin API. An order belonging
/// to a *different* tenant's `pk_` than the one in the URL is
/// indistinguishable from an unknown `payment_id` - the engine's own
/// `get_order_detail` already scopes lookups to the authenticated tenant,
/// so this can never leak another tenant's order by construction, not by
/// an extra check here.
async fn load_order(
    state: &AppState,
    pk: &str,
    payment_id: &str,
) -> Result<(StoreConnectionRow, String, OrderDetailResponse), LoadError> {
    let row = match state.db.lock().unwrap().get_store_connection_by_public_key(pk) {
        Ok(Some(row)) => row,
        Ok(None) => return Err(LoadError::NotFound),
        Err(_) => return Err(LoadError::Internal),
    };
    let sk = match crate::crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted) {
        Ok(sk) => sk,
        Err(_) => return Err(LoadError::Internal),
    };
    match state.engine_client.get_order_detail(&sk, payment_id).await {
        Ok(detail) => Ok((row, sk, detail)),
        Err(EngineClientError::EngineError { status, .. }) if status == reqwest::StatusCode::NOT_FOUND => {
            Err(LoadError::NotFound)
        }
        Err(_) => Err(LoadError::Internal),
    }
}

/// `GET /pay/{pk}/orders/{payment_id}` - the full checkout page.
pub async fn checkout_page(State(state): State<AppState>, Path((pk, payment_id)): Path<(String, String)>) -> Response {
    let (row, sk, detail) = match load_order(&state, &pk, &payment_id).await {
        Ok(loaded) => loaded,
        Err(LoadError::NotFound) => {
            let html = state
                .templates
                .render_checkout_not_found()
                .expect("the built-in checkout-not-found template must always render");
            return (StatusCode::NOT_FOUND, Html(html)).into_response();
        }
        Err(LoadError::Internal) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // A reasonable, safe-side default if the engine is briefly unreachable
    // for this one extra call - the order data itself already loaded fine
    // above, so this page still shows something real rather than failing
    // outright over a field that only affects the confirmation-progress
    // display.
    let confirmations_required = state.engine_client.get_tenant(&sk).await.map(|t| t.confirmations_required).unwrap_or(10);

    let (fiat_amount, fiat_currency) =
        match state.db.lock().unwrap().get_order_fiat_metadata(&row.id, &payment_id) {
            Ok(Some(metadata)) => (metadata.fiat_amount, metadata.fiat_currency),
            _ => (detail.order.fiat_amount.clone(), detail.order.fiat_currency.clone()),
        };

    let qr_code_svg = match qr_svg_for_html(&detail.order.address) {
        Ok(svg) => svg,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let (status_text, status_class, is_terminal) = status_label(&detail.order.status);
    let view = CheckoutViewModel {
        payment_id: detail.order.payment_id.clone(),
        status: detail.order.status.clone(),
        status_label: status_text.to_string(),
        status_class: status_class.to_string(),
        address: detail.order.address.clone(),
        qr_code_svg,
        xmr_amount: shared::exchange_rate::format_piconero_as_xmr(detail.order.xmr_amount_piconero),
        amount_received_xmr: shared::exchange_rate::format_piconero_as_xmr(detail.order.amount_received_piconero),
        fiat_amount,
        fiat_currency,
        confirmations: detail.order.confirmations,
        confirmations_required,
        is_terminal,
        double_spend_detected_at: detail.order.double_spend_detected_at,
        expires_at: detail.order.expires_at,
        merchant_order_id: detail.order.merchant_order_id.clone(),
        pk,
        payments: detail
            .payments
            .iter()
            .map(|p| CheckoutPaymentViewModel {
                txid_short: short_txid(&p.txid),
                amount_xmr: shared::exchange_rate::format_piconero_as_xmr(p.amount_piconero),
                confirmations: match p.block_height {
                    Some(_) => detail.order.confirmations,
                    None => 0,
                },
                is_zero_conf: p.block_height.is_none(),
            })
            .collect(),
    };

    let html = state.templates.render_checkout(&view).expect("the built-in checkout template must always render");
    Html(html).into_response()
}

#[derive(Serialize)]
pub struct CheckoutStatusResponse {
    pub status: String,
    pub confirmations: u64,
}

/// `GET /pay/{pk}/orders/{payment_id}/status` - the small JSON the checkout
/// page's own polling script reads (`status`/`confirmations` only - the
/// same two fields the engine's old polling JS ever read from its
/// equivalent response).
pub async fn checkout_status(State(state): State<AppState>, Path((pk, payment_id)): Path<(String, String)>) -> Response {
    match load_order(&state, &pk, &payment_id).await {
        Ok((_row, _sk, detail)) => {
            Json(CheckoutStatusResponse { status: detail.order.status, confirmations: detail.order.confirmations })
                .into_response()
        }
        Err(LoadError::NotFound) => ApiError::NotFound.into_response(),
        Err(LoadError::Internal) => ApiError::Internal.into_response(),
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

    const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
    const TEST_ENCRYPTION_KEY: [u8; 32] = [7u8; 32];
    const TEST_CURRENCY: &str = "USD";
    const TEST_RATE_PICONERO_PER_UNIT: u64 = 1_000_000_000_000;

    fn test_exchange_rate_provider() -> std::sync::Arc<dyn shared::exchange_rate::ExchangeRateProvider> {
        std::sync::Arc::new(shared::exchange_rate::FixedRateProvider::new(std::collections::HashMap::from([(
            TEST_CURRENCY.to_string(),
            TEST_RATE_PICONERO_PER_UNIT,
        )])))
    }

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
            exchange_rate: test_exchange_rate_provider(),
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

    async fn create_order(router: &Router, pk: &str, fiat_amount: &str) -> String {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/pay/{pk}/orders"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "fiat_amount": fiat_amount, "fiat_currency": TEST_CURRENCY }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        body_json(response).await.as_object().unwrap().get("payment_id").unwrap().as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn the_checkout_page_shows_the_real_address_amount_and_fiat_quote() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "checkout-page@example.com", "correct horse battery staple").await;
        let pk = create_connection(&router, &session_token).await;
        let payment_id = create_order(&router, &pk, "25.00").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/pay/{pk}/orders/{payment_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains(&payment_id), "expected the real payment_id shown, got: {html}");
        assert!(html.contains("25.00"), "expected the real fiat amount shown, got: {html}");
        assert!(html.contains(TEST_CURRENCY), "expected the real fiat currency shown, got: {html}");
        assert!(html.contains("<svg"), "expected a real rendered QR code, got: {html}");
        // `_styles.html.hbs` (included here for base typography/color)
        // mentions both `.site-nav` and even the literal text `<nav>` in
        // its own CSS *comments* - a plain substring check for either would
        // false-positive on those comments even with no nav actually
        // rendered. `_nav.html.hbs`'s own real opening tag is the one
        // string that can only appear if `{{> nav}}` genuinely ran.
        assert!(!html.contains(r#"<nav class="site-nav">"#), "the checkout page must not carry the site nav, got: {html}");
        assert!(!html.contains("MoneroPay Cloud"), "the checkout page must not carry the site brand/logo, got: {html}");
    }

    #[tokio::test]
    async fn the_checkout_status_endpoint_returns_real_status_and_confirmations() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "checkout-status@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let payment_id = create_order(&router, &pk, "10.00").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/pay/{pk}/orders/{payment_id}/status"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["status"], "pending");
        assert_eq!(body["confirmations"], 0);
    }

    #[tokio::test]
    async fn an_unknown_payment_id_shows_a_real_not_found_page() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "checkout-not-found@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/pay/{pk}/orders/nonexistent"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let html = body_text(response).await;
        assert!(html.to_lowercase().contains("not found"), "got: {html}");
    }

    #[tokio::test]
    async fn an_unknown_public_key_is_also_a_real_not_found_not_a_500() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/pay/pk_nonexistent/orders/pay_nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
