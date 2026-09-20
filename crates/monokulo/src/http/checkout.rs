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
//! Monokulo's own site navigation has no business appearing inside it. It
//! still uses the shared `_styles` partial for consistent typography/color,
//! just not the nav.
//!
//! Fiat display comes entirely from monokulo's own locally recorded
//! quote (`Db::get_order_currency_metadata`) - the engine has no concept of
//! fiat at all any more (`docs/fx_refactor.md` Phase 3), so an order with no
//! local record (predates this feature, or was created directly against the
//! engine rather than through monokulo's own `http::pay` endpoint)
//! simply shows a dash rather than a fabricated amount.

use axum::extract::{Form, Path, State};
use axum::response::{IntoResponse, Json, Response};
use axum::http::{HeaderMap, StatusCode};
use qrcode::render::svg;
use qrcode::QrCode;
use serde::{Deserialize, Serialize};

use crate::db::StoreConnectionRow;
use crate::engine_client::{EngineClientError, OrderDetailResponse};
use crate::views;
use crate::views::checkout::{CheckoutPaymentViewModel, CheckoutShareViewModel, CheckoutViewModel};

use super::dashboard::redirect_302;
use super::{ApiError, AppState};

/// `pending`/`unconfirmed`/`confirming`/`partial` are still "in progress";
/// `paid`/`overpaid`/`expired` are terminal - used to decide whether the
/// page should keep polling. Mirrors the engine's own (soon-removed)
/// `templates::status_label` - presentation logic, duplicated once rather
/// than shared, since it's a 10-line match statement used in exactly one
/// place per crate, not obviously worth a `shared` module of its own.
pub(super) fn status_label(status: &str) -> (&'static str, &'static str, bool) {
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
pub(super) fn qr_svg_for_html(data: &str) -> Result<String, ApiError> {
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
        Err(LoadError::NotFound) => return not_found_response(),
        Err(LoadError::Internal) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render_checkout_page(&state, pk, row, sk, detail, None).await
}

fn not_found_response() -> Response {
    let chrome = views::PageChrome::from_user(None, "/");
    (StatusCode::NOT_FOUND, views::checkout::not_found_page(&chrome)).into_response()
}

/// The real body of the checkout page, shared by the plain `GET` above and
/// `set_refund_address`'s own error paths below (re-rendering the exact
/// same page with an inline error, rather than a bare error response, on a
/// rejected refund-address submission) - both already hold a freshly
/// loaded `(row, sk, detail)` from `load_order`, so this never re-fetches.
async fn render_checkout_page(
    state: &AppState,
    pk: String,
    row: StoreConnectionRow,
    sk: String,
    detail: OrderDetailResponse,
    refund_address_error: Option<String>,
) -> Response {
    // A reasonable, safe-side default if the engine is briefly unreachable
    // for this one extra call - the order data itself already loaded fine
    // above, so this page still shows something real rather than failing
    // outright over a field that only affects the confirmation-progress
    // display.
    let confirmations_required = state.engine_client.get_tenant(&sk).await.map(|t| t.confirmations_required).unwrap_or(10);

    let (amount, currency) =
        match state.db.lock().unwrap().get_order_currency_metadata(&row.id, &detail.order.payment_id) {
            Ok(Some(metadata)) => (metadata.amount, metadata.currency),
            _ => ("—".to_string(), "".to_string()),
        };

    let qr_code_svg = match qr_svg_for_html(&detail.order.address) {
        Ok(svg) => svg,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let (status_text, status_class, is_terminal) = status_label(&detail.order.status);
    // A subtle, progressive color shift as expiry nears (research on real
    // crypto-checkout UIs: a big alarming red countdown creates anxiety: a
    // quiet color change at 5 minutes, then 2, communicates urgency without
    // it) - computed here, server-side, same reasoning as `progress_percent`
    // above: this page has no `<script>` at all, so anything time-sensitive
    // has to already be correct in the HTML this handler returns, refreshed
    // by the meta-refresh tag rather than a client-side timer.
    let seconds_until_expiry = detail.order.expires_at - crate::now_unix();
    let expiry_urgency_class = if is_terminal {
        String::new()
    } else if seconds_until_expiry <= 120 {
        "expiry-urgent".to_string()
    } else if seconds_until_expiry <= 300 {
        "expiry-soon".to_string()
    } else {
        String::new()
    };
    // Computed here, server-side, not by client JS from a live poll - this
    // page has no `<script>` at all any more (a meta-refresh re-fetches the
    // whole page instead), so the progress bar's fill has to already be
    // correct in the HTML this handler returns. `confirmations_required ==
    // 0` (zero-conf trusted) means any receipt already counts as done.
    let progress_percent: u8 = if confirmations_required == 0 {
        100
    } else {
        ((detail.order.confirmations as f64 / confirmations_required as f64) * 100.0).round().min(100.0) as u8
    };
    let view = CheckoutViewModel {
        payment_id: detail.order.payment_id.clone(),
        status_label: status_text.to_string(),
        status_class: status_class.to_string(),
        address: detail.order.address.clone(),
        qr_code_svg,
        xmr_amount: shared::exchange_rate::format_piconero_as_xmr(detail.order.xmr_amount_piconero),
        amount_received_xmr: shared::exchange_rate::format_piconero_as_xmr(detail.order.amount_received_piconero),
        amount,
        currency,
        confirmations: detail.order.confirmations,
        confirmations_required,
        progress_percent,
        is_terminal,
        double_spend_detected_at: detail.order.double_spend_detected_at,
        double_spend_detected_at_display: crate::templates::display_timestamp_or_dash(detail.order.double_spend_detected_at),
        expires_in_display: crate::templates::format_duration_until(detail.order.expires_at, crate::now_unix()),
        expiry_urgency_class,
        refund_address: detail.order.refund_address.clone(),
        refund_address_error,
        pk: pk.clone(),
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

    let chrome = views::PageChrome::from_user(None, format!("/pay/{pk}/orders/{}", detail.order.payment_id));
    views::checkout::checkout_page(&chrome, &view).into_response()
}

#[derive(Deserialize)]
pub struct SetRefundAddressForm {
    pub refund_address: String,
}

/// `POST /pay/{pk}/orders/{payment_id}/refund-address` - the checkout
/// page's own plain HTML form for a customer to record where a refund
/// should go, forwarding to the engine's own real endpoint
/// (`EngineClient::set_refund_address`) - monokulo stores nothing of
/// its own here, same "engine owns order state, monokulo owns
/// pricing/presentation" split every other order-mutating call in this
/// module already follows. A real, previously-missing capability: the
/// engine has supported this since `src/http/public.rs::set_refund_address`
/// existed, but nothing before this handler ever exposed a way to call it -
/// a customer paying through the checkout widget had no path to set one at
/// all.
///
/// Plain form POST, not `fetch`/JSON - this page carries no JavaScript at
/// all (see its own doc comment). Redirects back to the plain checkout page
/// on success (POST-redirect-GET, same convention the dashboard's own forms
/// use); an empty submission or a real engine failure re-renders the same
/// page with an inline error instead of a bare error response - a customer
/// typing the wrong thing here shouldn't lose their place mid-payment.
pub async fn set_refund_address(
    State(state): State<AppState>,
    Path((pk, payment_id)): Path<(String, String)>,
    Form(form): Form<SetRefundAddressForm>,
) -> Response {
    let (row, sk, detail) = match load_order(&state, &pk, &payment_id).await {
        Ok(loaded) => loaded,
        Err(LoadError::NotFound) => return not_found_response(),
        Err(LoadError::Internal) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let refund_address = form.refund_address.trim();
    if refund_address.is_empty() {
        return render_checkout_page(&state, pk, row, sk, detail, Some("Enter a refund address.".to_string())).await;
    }

    match state.engine_client.set_refund_address(&pk, &payment_id, refund_address).await {
        Ok(()) => redirect_302(&format!("/pay/{pk}/orders/{payment_id}")),
        Err(e) => {
            eprintln!("failed to set refund address for order {payment_id} on connection {}: {e}", row.id);
            render_checkout_page(&state, pk, row, sk, detail, Some("Something went wrong saving that. Please try again.".to_string())).await
        }
    }
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

/// `GET /pay/{pk}/orders/{payment_id}/share` - a real follow-up to
/// `docs/fx_refactor.md`: "on the order details page you should be able to
/// obtain a payment link which can be shared to someone who needs to pay
/// for the order". `checkout_page` above is deliberately bare (no nav, no
/// site branding at all - see this module's own doc comment) since it's
/// built to be iframed inside a *merchant's* own page; handed directly to a
/// customer with no page of their own around it, that bareness reads as a
/// broken or unbranded link, not a real invoice. This route wraps the exact
/// same checkout page in an iframe, inside a real, nav-bearing
/// monokulo page, so a link shared over chat/email lands somewhere
/// that visibly is Monokulo - "cohesive" per the same follow-up's
/// own wording, not a second copy of the payment logic (all of it - status
/// polling, the QR code, the copy button - still lives in the one iframed
/// page).
///
/// Confirms the order actually exists first (the same `load_order` every
/// other route on this page uses) purely to render an honest not-found
/// state with the site's own nav around it, rather than a page whose only
/// content is a broken iframe.
pub async fn checkout_share_page(
    State(state): State<AppState>,
    Path((pk, payment_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let found = match load_order(&state, &pk, &payment_id).await {
        Ok(_) => true,
        Err(LoadError::NotFound) => false,
        Err(LoadError::Internal) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let status = if found { StatusCode::OK } else { StatusCode::NOT_FOUND };
    let authed = super::resolve_authed_user(&state, &headers);
    let current_path = format!("/pay/{pk}/orders/{payment_id}/share");
    let chrome = views::PageChrome::from_user(authed.as_ref().map(|(user, _)| user), current_path);
    let view = CheckoutShareViewModel { pk, payment_id, found };
    (status, views::checkout::share_page(&chrome, &view)).into_response()
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
    // `"XMR"`, not a fiat currency - this module's tests are about the
    // checkout page's own rendering, not about exercising a real (mocked)
    // fiat provider (`pay.rs`'s own tests do that), and an XMR-denominated
    // order needs no provider configured at all.
    const TEST_CURRENCY: &str = "XMR";

    fn test_exchange_rate_provider() -> std::sync::Arc<crate::exchange_rate_config::ExchangeRateProviders> {
        std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::xmr_only())
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

    async fn create_order(router: &Router, pk: &str, amount: &str) -> String {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/pay/{pk}/orders"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "amount": amount, "currency": TEST_CURRENCY }).to_string(),
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
        assert!(!html.contains("Monokulo"), "the checkout page must not carry the site brand/logo, got: {html}");
        // The real point of this follow-up: "Expires in" must be a real,
        // already-formatted relative duration baked into the server
        // response - this page must stay meaningful with JavaScript
        // disabled, so nothing on it may rely on `data-timestamp` +
        // client-side formatting any more.
        assert!(html.contains("Expires in"), "expected a server-rendered expiry duration, got: {html}");
        assert!(!html.contains("data-timestamp"), "the checkout page must not depend on JS to format any timestamp, got: {html}");
        // The real point of this follow-up: no JavaScript at all on this
        // page - a meta-refresh re-fetches it instead of a poll loop, and
        // the progress bar's fill is a real inline style already baked in.
        assert!(!html.contains("<script"), "the checkout page must carry no JavaScript at all, got: {html}");
        assert!(
            html.contains(r#"<meta http-equiv="refresh" content="10">"#),
            "expected a meta-refresh directive on a still-in-progress order, got: {html}"
        );
        assert!(html.contains("style=\"width: 0%\""), "expected a real, already-computed progress-bar fill, got: {html}");
    }

    /// A real, previously-missing capability: the engine has supported a
    /// customer-settable refund address since `src/http/public.rs::
    /// set_refund_address` existed, but nothing in monokulo's own
    /// checkout page ever exposed a way to call it. Plain form POST, no
    /// JS - this page carries none.
    #[tokio::test]
    async fn setting_a_refund_address_through_the_checkout_pages_own_form_persists_it_on_the_engine() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "checkout-refund-address@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let payment_id = create_order(&router, &pk, "25.00").await;

        // Before setting one, the checkout page must show the form, not a
        // refund address that was never set.
        let before = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/pay/{pk}/orders/{payment_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let before_html = body_text(before).await;
        assert!(before_html.contains("id=\"refund_address\""), "expected the refund-address form present before one is set, got: {before_html}");

        let refund_address = "86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC";
        let submit = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/pay/{pk}/orders/{payment_id}/refund-address"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!("refund_address={refund_address}")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(submit.status(), StatusCode::FOUND, "expected a redirect back to the plain checkout page");

        let after = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/pay/{pk}/orders/{payment_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let after_html = body_text(after).await;
        assert!(after_html.contains(refund_address), "expected the real, just-saved refund address shown, got: {after_html}");
        assert!(!after_html.contains("id=\"refund_address\""), "expected the form gone once a refund address is set, got: {after_html}");
    }

    #[tokio::test]
    async fn submitting_an_empty_refund_address_shows_a_clear_error_not_a_bare_error_page() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "checkout-refund-address-empty@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let payment_id = create_order(&router, &pk, "25.00").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/pay/{pk}/orders/{payment_id}/refund-address"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("refund_address="))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a rejected submission re-renders the page, it doesn't redirect");
        let html = body_text(response).await;
        assert!(html.contains("Enter a refund address."), "expected a clear inline error, got: {html}");
        assert!(html.contains(&payment_id), "the real checkout page must still be shown, not a bare error, got: {html}");
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

    #[tokio::test]
    async fn the_share_page_wraps_the_real_checkout_page_with_the_site_nav() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "checkout-share@example.com", "correct horse battery staple").await;
        let pk = create_connection(&router, &session_token).await;
        let payment_id = create_order(&router, &pk, "25.00").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/pay/{pk}/orders/{payment_id}/share"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        // Unlike the bare checkout page itself, this one *does* carry the
        // real site nav - the whole point of this page's existence.
        assert!(html.contains(r#"<nav class="site-nav">"#), "expected the real site nav, got: {html}");
        assert!(html.contains("Monokulo"), "expected the real site brand, got: {html}");
        // The iframe must point at the real, unwrapped checkout page for
        // this exact order - not a second copy of the payment UI.
        assert!(
            html.contains(&format!(r#"src="/pay/{pk}/orders/{payment_id}""#)),
            "expected an iframe pointing at the real checkout page, got: {html}"
        );
    }

    #[tokio::test]
    async fn the_share_page_shows_a_real_not_found_state_with_the_site_nav_for_an_unknown_order() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "checkout-share-not-found@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/pay/{pk}/orders/nonexistent/share"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let html = body_text(response).await;
        assert!(html.to_lowercase().contains("not found"), "got: {html}");
        // Unlike the bare checkout page's own not-found state, this one
        // still carries the site nav - it's never meant to be iframed.
        assert!(html.contains(r#"<nav class="site-nav">"#), "expected the real site nav even on the not-found state, got: {html}");
        assert!(!html.contains("<iframe"), "must not render a broken iframe pointing at a nonexistent order");
    }
}
