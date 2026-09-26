//! Control-plane's own real, public checkout/payment page
//! (`docs/fx_refactor.md` Phase 2) - moved here from the engine. Every
//! engine call it makes goes through the engine's *admin* API with the
//! connection's own decrypted `sk_...`, entirely server-side (the browser
//! never sees it): `EngineClient::get_order_detail` (the same call the
//! dashboard's order-detail page uses) for the order and its payments, and
//! `EngineClient::set_refund_address` for the refund form. The engine is
//! private; monokulo never uses a public engine route.
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

use axum::extract::{Form, Path, Query, State};
use axum::response::{IntoResponse, Json, Response};
use axum::http::{HeaderMap, StatusCode};
use qrcode::render::svg;
use qrcode::QrCode;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::db::StoreConnectionRow;
use crate::engine_client::{EngineClientError, OrderDetailResponse, OrderView};
use crate::views;
use crate::views::checkout::{CheckoutPaymentViewModel, CheckoutShareViewModel, CheckoutViewModel};

use super::dashboard::redirect_302;
use super::{ApiError, AppState};

/// `pending`/`unconfirmed`/`confirming`/`partial` are still "in progress";
/// `paid`/`overpaid`/`expired` are terminal - used to decide whether the
/// page should keep updating. Mirrors the engine's own (soon-removed)
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
        "overpaid" => ("Overpaid", "status-overpaid", true),
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

/// Customer-facing amount guidance; POS keeps its own merchant-facing alerts.
fn checkout_payment_message(order: &OrderView) -> Option<String> {
    if order.double_spend_detected_at.is_some() {
        return super::pos::derive_payment_error(order);
    }
    let requested = shared::exchange_rate::format_piconero_as_xmr(order.xmr_amount_piconero);
    let received = shared::exchange_rate::format_piconero_as_xmr(order.amount_received_piconero);
    match order.status.as_str() {
        "partial" => {
            let remaining = shared::exchange_rate::format_piconero_as_xmr(
                order.xmr_amount_piconero.saturating_sub(order.amount_received_piconero),
            );
            Some(format!("{received} XMR received of {requested} XMR. Send the remaining {remaining} XMR to the address below."))
        }
        "overpaid" => {
            let extra = shared::exchange_rate::format_piconero_as_xmr(
                order.amount_received_piconero.saturating_sub(order.xmr_amount_piconero),
            );
            Some(format!("{received} XMR received for a {requested} XMR order ({extra} XMR extra). Do not send more. Contact the merchant about the extra amount."))
        }
        _ => super::pos::derive_payment_error(order),
    }
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
/// indistinguishable from an unknown `order_id` - the engine's own
/// `get_order_detail` already scopes lookups to the authenticated tenant,
/// so this can never leak another tenant's order by construction, not by
/// an extra check here.
async fn load_order(
    state: &AppState,
    pk: &str,
    order_id: &str,
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
    match state.engine_client.get_order_detail(&sk, order_id).await {
        Ok(detail) => Ok((row, sk, detail)),
        Err(EngineClientError::EngineError { status, .. }) if status == reqwest::StatusCode::NOT_FOUND => {
            Err(LoadError::NotFound)
        }
        Err(_) => Err(LoadError::Internal),
    }
}

/// `GET /pay/{pk}/orders/{order_id}` - the full checkout page.
#[derive(Clone, Default, Deserialize)]
pub struct CheckoutOptions {
    view: Option<String>,
    refund: Option<bool>,
    /// `/events` only: also stream re-rendered page fragments, for the
    /// checkout page's own script (`checkout.js`).
    fragments: Option<bool>,
    /// `refresh=false` turns off the no-JavaScript meta refresh, so a
    /// customer can type a refund address without the page reloading under
    /// them. Set by the page's own "Auto Refresh" toggle link.
    refresh: Option<bool>,
}

impl CheckoutOptions {
    fn is_compact(&self) -> bool { self.view.as_deref() == Some("compact") }
    fn refund_enabled(&self) -> bool { self.refund != Some(false) }
    fn auto_refresh(&self) -> bool { self.refresh != Some(false) }
    fn suffix(&self) -> String {
        let mut params = Vec::new();
        if self.is_compact() { params.push("view=compact"); }
        if !self.refund_enabled() { params.push("refund=false"); }
        if !self.auto_refresh() { params.push("refresh=false"); }
        if params.is_empty() { String::new() } else { format!("?{}", params.join("&")) }
    }
    /// The same page's query string with auto refresh flipped.
    fn toggled_refresh_suffix(&self) -> String {
        CheckoutOptions { refresh: if self.auto_refresh() { Some(false) } else { None }, ..self.clone() }.suffix()
    }
}

pub async fn checkout_page(State(state): State<AppState>, Path((pk, order_id)): Path<(String, String)>, Query(options): Query<CheckoutOptions>) -> Response {
    let (row, sk, detail) = match load_order(&state, &pk, &order_id).await {
        Ok(loaded) => loaded,
        Err(LoadError::NotFound) => return not_found_response(),
        Err(LoadError::Internal) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render_checkout_page(&state, pk, row, sk, detail, None, &options).await
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
    options: &CheckoutOptions,
) -> Response {
    let qr_code_svg = match qr_svg_for_html(&detail.order.address) {
        Ok(svg) => svg,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let order_id = detail.order.order_id.clone();
    let mut view = build_checkout_view(state, &pk, &row, &sk, detail, refund_address_error, options).await;
    view.qr_code_svg = qr_code_svg;
    let chrome = views::PageChrome::from_user(None, format!("/pay/{pk}/orders/{order_id}"));
    views::checkout::checkout_page(&chrome, &view).into_response()
}

/// Everything the checkout page shows except its QR code, which never
/// changes and is left empty here - the live-update stream re-renders only
/// the parts that do (`views::checkout::live_fragment`).
async fn build_checkout_view(
    state: &AppState,
    pk: &str,
    row: &StoreConnectionRow,
    sk: &str,
    detail: OrderDetailResponse,
    refund_address_error: Option<String>,
    options: &CheckoutOptions,
) -> CheckoutViewModel {
    // A reasonable, safe-side default if the engine is briefly unreachable
    // for this one extra call - the order data itself already loaded fine
    // above, so this page still shows something real rather than failing
    // outright over a field that only affects the confirmation-progress
    // display.
    let confirmations_required = super::pos::resolve_confirmations_required(state, &row.id, sk, &detail.order.order_id).await;

    let (amount, currency) =
        match state.db.lock().unwrap().get_order_currency_metadata(&row.id, &detail.order.order_id) {
            Ok(Some(metadata)) => (metadata.amount, metadata.currency),
            _ => ("—".to_string(), "".to_string()),
        };

    let (status_text, status_class, is_terminal) = status_label(&detail.order.status);
    // A subtle, progressive color shift as expiry nears (research on real
    // crypto-checkout UIs: a big alarming red countdown creates anxiety: a
    // quiet color change at 5 minutes, then 2, communicates urgency without
    // it) - computed here, server-side, same reasoning as `progress_percent`
    // above: values are correct in the initial HTML, including when
    // JavaScript is disabled.
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
    // Computed server-side so the progress bar is correct in the initial
    // HTML and after a status refresh. `confirmations_required ==
    // 0` (zero-conf trusted) means any receipt already counts as done.
    let progress_percent: u8 = if confirmations_required == 0 {
        if matches!(detail.order.status.as_str(), "paid" | "overpaid") { 100 } else { 0 }
    } else {
        ((detail.order.confirmations as f64 / confirmations_required as f64) * 100.0).round().min(100.0) as u8
    };
    CheckoutViewModel {
        order_id: detail.order.order_id.clone(),
        status_label: status_text.to_string(),
        status: detail.order.status.clone(),
        status_class: status_class.to_string(),
        address: detail.order.address.clone(),
        qr_code_svg: String::new(),
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
        is_compact: options.is_compact(),
        refund_enabled: options.refund_enabled(),
        query_suffix: options.suffix(),
        auto_refresh: options.auto_refresh(),
        toggled_refresh_suffix: options.toggled_refresh_suffix(),
        payment_error: checkout_payment_message(&detail.order),
        pk: pk.to_string(),
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
    }
}

#[derive(Deserialize)]
pub struct SetRefundAddressForm {
    pub refund_address: String,
}

/// `POST /pay/{pk}/orders/{order_id}/refund-address` - the checkout
/// page's own plain HTML form for a customer to record where a refund
/// should go, forwarding to the engine's admin API with the store's `sk_`
/// (`EngineClient::set_refund_address`) - monokulo stores nothing of
/// its own here, same "engine owns order state, monokulo owns
/// pricing/presentation" split every other order-mutating call in this
/// module already follows.
///
/// Browser-side auto-save requests JSON so it can show saving, saved, and
/// invalid states without leaving the page. A plain form POST still works
/// without JavaScript: it redirects on success and re-renders inline errors.
pub async fn set_refund_address(
    State(state): State<AppState>,
    Path((pk, order_id)): Path<(String, String)>,
    Query(options): Query<CheckoutOptions>,
    headers: HeaderMap,
    Form(form): Form<SetRefundAddressForm>,
) -> Response {
    let wants_json = headers.get(axum::http::header::ACCEPT).and_then(|value| value.to_str().ok()).is_some_and(|value| value.contains("application/json"));
    let (row, sk, detail) = match load_order(&state, &pk, &order_id).await {
        Ok(loaded) => loaded,
        Err(LoadError::NotFound) => return not_found_response(),
        Err(LoadError::Internal) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let refund_address = form.refund_address.trim();
    if refund_address.is_empty() {
        if wants_json { return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Enter a refund address."}))).into_response(); }
        return render_checkout_page(&state, pk, row, sk, detail, Some("Enter a refund address.".to_string()), &options).await;
    }

    let parsed = monero::Address::from_str(refund_address);
    let payment_address = monero::Address::from_str(&detail.order.address);
    if !matches!((&parsed, &payment_address), (Ok(refund), Ok(payment)) if refund.network == payment.network) {
        if wants_json { return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Enter a valid Monero address for this store's network."}))).into_response(); }
        return render_checkout_page(&state, pk, row, sk, detail, Some("Enter a valid Monero address for this store's network.".to_string()), &options).await;
    }

    match state.engine_client.set_refund_address(&sk, &order_id, refund_address).await {
        Ok(()) if wants_json => Json(serde_json::json!({"ok": true})).into_response(),
        Ok(()) => redirect_302(&format!("/pay/{pk}/orders/{order_id}{}", options.suffix())),
        Err(e) => {
            eprintln!("failed to set refund address for order {order_id} on connection {}: {e}", row.id);
            if wants_json { return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": "Something went wrong saving that. Please try again."}))).into_response(); }
            render_checkout_page(&state, pk, row, sk, detail, Some("Something went wrong saving that. Please try again.".to_string()), &options).await
        }
    }
}

#[derive(Serialize)]
pub struct CheckoutStatusResponse {
    pub status: String,
    pub confirmations: u64,
    pub confirmations_required: u64,
    pub is_terminal: bool,
    pub error: Option<String>,
}

/// `GET /pay/{pk}/orders/{order_id}/status` - the small JSON the checkout
/// page's own polling script reads (`status`/`confirmations` only - the
/// same two fields the engine's old polling JS ever read from its
/// equivalent response).
pub async fn checkout_status(State(state): State<AppState>, Path((pk, order_id)): Path<(String, String)>) -> Response {
    match load_order(&state, &pk, &order_id).await {
        Ok((row, sk, detail)) => {
            let confirmations_required = super::pos::resolve_confirmations_required(&state, &row.id, &sk, &detail.order.order_id).await;
            let error = checkout_payment_message(&detail.order);
            let (_, _, is_terminal) = status_label(&detail.order.status);
            Json(CheckoutStatusResponse {
                status: detail.order.status,
                confirmations: detail.order.confirmations,
                confirmations_required,
                is_terminal,
                error,
            })
            .into_response()
        }
        Err(LoadError::NotFound) => ApiError::NotFound.into_response(),
        Err(LoadError::Internal) => ApiError::Internal.into_response(),
    }
}

/// `GET /pay/{pk}/orders/{order_id}/events` - the same status as
/// [`checkout_status`], as a Server-Sent Events stream that sends it again
/// only when it changes (`crate::live`) and ends once the order is terminal.
///
/// Events: `status` - [`CheckoutStatusResponse`] JSON, always; `fragment` -
/// with `?fragments=true`, the checkout page's live regions re-rendered
/// (`views::checkout::live_fragment`) for `checkout.js` to swap in. Takes
/// the page's own `view`/`refund` options so fragments match what the page
/// rendered. Also re-checked every 30s, since the time left to pay moves
/// with the clock rather than with the order.
///
/// One source IP may hold only so many of these open per store at once
/// (`http::stream_limit`); past that the request gets `429`.
pub async fn checkout_events(
    State(state): State<AppState>,
    Path((pk, order_id)): Path<(String, String)>,
    Query(options): Query<CheckoutOptions>,
    extensions: axum::http::Extensions,
) -> Response {
    let (row, sk, _) = match load_order(&state, &pk, &order_id).await {
        Ok(loaded) => loaded,
        Err(LoadError::NotFound) => return ApiError::NotFound.into_response(),
        Err(LoadError::Internal) => return ApiError::Internal.into_response(),
    };
    // Same fail-open-without-a-peer-address rule as `rate_limit_middleware`
    // (only tests drive the router without one).
    let permit = match extensions.get::<axum::extract::ConnectInfo<std::net::SocketAddr>>() {
        Some(peer) => match state.event_streams.try_acquire(peer.0.ip(), &pk) {
            Some(permit) => Some(permit),
            None => return (StatusCode::TOO_MANY_REQUESTS, Json(serde_json::json!({ "error": "too many open update streams" }))).into_response(),
        },
        None => None,
    };
    let subscription = state.engine_client.subscribe_order(&row.id, &sk, &order_id);
    let fragments = options.fragments == Some(true);
    crate::live::live_events(subscription, std::time::Duration::from_secs(30), move || {
        // Held by the stream, so the slot frees when the stream ends.
        let _permit = &permit;
        let (state, pk, order_id, options) = (state.clone(), pk.clone(), order_id.clone(), options.clone());
        async move {
            let (row, sk, detail) = load_order(&state, &pk, &order_id).await.ok()?;
            let view = build_checkout_view(&state, &pk, &row, &sk, detail, None, &options).await;
            let status = CheckoutStatusResponse {
                status: view.status.clone(),
                confirmations: view.confirmations,
                confirmations_required: view.confirmations_required,
                is_terminal: view.is_terminal,
                error: view.payment_error.clone(),
            };
            let status_json = serde_json::to_string(&status).ok()?;
            let mut fingerprint = status_json.clone();
            let mut events = Vec::new();
            // Fragment first: a client may close the stream on seeing a
            // terminal `status`, and must have the final page state by then.
            if fragments {
                let html = views::checkout::live_fragment(&view).into_string();
                fingerprint.push_str(&html);
                events.push(axum::response::sse::Event::default().event("fragment").data(html));
            }
            events.push(axum::response::sse::Event::default().event("status").data(status_json));
            Some(crate::live::LiveSnapshot { events, fingerprint, terminal: view.is_terminal })
        }
    })
}

/// `GET /pay/{pk}/orders/{order_id}/share` - a real follow-up to
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
/// own wording, not a second copy of the payment logic (all of it - live
/// status updates, the QR code, the copy button - still lives in the one
/// iframed page).
///
/// Confirms the order actually exists first (the same `load_order` every
/// other route on this page uses) purely to render an honest not-found
/// state with the site's own nav around it, rather than a page whose only
/// content is a broken iframe.
pub async fn checkout_share_page(
    State(state): State<AppState>,
    Path((pk, order_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let found = match load_order(&state, &pk, &order_id).await {
        Ok(_) => true,
        Err(LoadError::NotFound) => false,
        Err(LoadError::Internal) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let status = if found { StatusCode::OK } else { StatusCode::NOT_FOUND };
    let authed = super::resolve_authed_user(&state, &headers);
    let current_path = format!("/pay/{pk}/orders/{order_id}/share");
    let chrome = super::page_chrome(&state, authed.as_ref().map(|(user, _)| user), current_path);
    let view = CheckoutShareViewModel { pk, order_id, found };
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
    use super::CheckoutOptions;

    #[test]
    fn the_auto_refresh_toggle_flips_only_the_refresh_parameter() {
        let on = CheckoutOptions { view: Some("compact".to_string()), refund: Some(false), ..Default::default() };
        assert_eq!(on.suffix(), "?view=compact&refund=false");
        assert_eq!(on.toggled_refresh_suffix(), "?view=compact&refund=false&refresh=false");

        let off = CheckoutOptions { refresh: Some(false), ..Default::default() };
        assert!(!off.auto_refresh());
        assert_eq!(off.suffix(), "?refresh=false");
        assert_eq!(off.toggled_refresh_suffix(), "");
    }
    use super::checkout_payment_message;

    #[test]
    fn checkout_amount_messages_use_exact_received_remaining_and_extra_xmr() {
        let mut order = crate::engine_client::OrderView {
            order_id: "pay_test".to_string(),
            merchant_order_id: None,
            address: "address".to_string(),
            xmr_amount_piconero: 500_000_000_000,
            amount_received_piconero: 200_000_000_000,
            status: "partial".to_string(),
            confirmations: 0,
            double_spend_detected_at: None,
            refund_address: None,
            created_at: 0,
            expires_at: 1800,
            updated_at: 0,
            first_scanned_height: None,
            last_scanned_height: None,
            currently_scanning: true,
        };
        assert_eq!(checkout_payment_message(&order).as_deref(), Some("0.200000000000 XMR received of 0.500000000000 XMR. Send the remaining 0.300000000000 XMR to the address below."));

        order.status = "overpaid".to_string();
        order.amount_received_piconero = 600_000_000_000;
        assert_eq!(checkout_payment_message(&order).as_deref(), Some("0.600000000000 XMR received for a 0.500000000000 XMR order (0.100000000000 XMR extra). Do not send more. Contact the merchant about the extra amount."));

        order.double_spend_detected_at = Some(123);
        assert!(checkout_payment_message(&order).unwrap().contains("Double-spend"));
    }

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
        let response = router.clone()
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
        body_json(response).await.as_object().unwrap().get("order_id").unwrap().as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn the_checkout_page_shows_the_real_address_amount_and_fiat_quote() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "checkout-page@example.com", "correct horse battery staple").await;
        let pk = create_connection(&router, &session_token).await;
        let order_id = create_order(&router, &pk, "25.00").await;

        let response = router.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/pay/{pk}/orders/{order_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains(&order_id), "expected the real order_id shown, got: {html}");
        assert!(html.contains("25.00"), "expected the real fiat amount shown, got: {html}");
        assert!(html.contains(TEST_CURRENCY), "expected the real fiat currency shown, got: {html}");
        assert!(html.contains("<svg"), "expected a real rendered QR code, got: {html}");
        let pos_response = router.clone().oneshot(
            Request::builder().uri(format!("/pay/{pk}/orders/{order_id}?view=compact&refund=false")).body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(pos_response.status(), StatusCode::OK);
        let pos_html = body_text(pos_response).await;
        assert!(pos_html.contains("pay-wrap checkout-compact"));
        assert!(!pos_html.contains("id=\"refund_address\""));
        // `_styles.html.hbs` (included here for base typography/color)
        // mentions both `.site-nav` and even the literal text `<nav>` in
        // its own CSS *comments* - a plain substring check for either would
        // false-positive on those comments even with no nav actually
        // rendered. `_nav.html.hbs`'s own real opening tag is the one
        // string that can only appear if `{{> nav}}` genuinely ran.
        assert!(!html.contains(r#"<nav class="site-nav">"#), "the checkout page must not carry the site nav, got: {html}");
        assert!(!html.contains("Monokulo"), "the checkout page must not carry the site brand/logo, got: {html}");
        // The payment deadline must be a real,
        // already-formatted relative duration baked into the server
        // response - this page must stay meaningful with JavaScript
        // disabled, so nothing on it may rely on `data-timestamp` +
        // client-side formatting any more.
        assert!(html.contains("Send payment within"), "expected a server-rendered payment deadline, got: {html}");
        assert!(!html.contains("data-timestamp"), "the checkout page must not depend on JS to format any timestamp, got: {html}");
        // The server-rendered page remains meaningful without JavaScript.
        assert!(html.contains("/static/checkout.js"));
        assert!(html.contains("Auto Refresh: ON"));
        assert!(html.contains(r#"<noscript><meta http-equiv="refresh" content="60""#));
        assert!(html.contains("style=\"width: 0%\""), "expected a real, already-computed progress-bar fill, got: {html}");
    }

    /// The checkout's refund-address form, a plain form POST with no JS,
    /// reaches the engine through its admin API
    /// (`POST /api/v1/admin/tenant/orders/{order_id}/refund-address`) and
    /// the address really is stored there.
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
        let order_id = create_order(&router, &pk, "25.00").await;

        // Before setting one, the checkout page must show the form, not a
        // refund address that was never set.
        let before = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/pay/{pk}/orders/{order_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let before_html = body_text(before).await;
        assert!(before_html.contains("id=\"refund_address\""), "expected the refund-address form present before one is set, got: {before_html}");

        let invalid = router.clone().oneshot(
            Request::builder().method("POST").uri(format!("/pay/{pk}/orders/{order_id}/refund-address?view=compact"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("refund_address=not-an-address")).unwrap()
        ).await.unwrap();
        assert_eq!(invalid.status(), StatusCode::OK);
        let invalid_html = body_text(invalid).await;
        assert!(invalid_html.contains("Enter a valid Monero address"));
        assert!(invalid_html.contains("refund-address?view=compact"));

        let invalid_json = router.clone().oneshot(
            Request::builder().method("POST").uri(format!("/pay/{pk}/orders/{order_id}/refund-address"))
                .header("accept", "application/json")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("refund_address=not-an-address")).unwrap()
        ).await.unwrap();
        assert_eq!(invalid_json.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(invalid_json).await.contains("valid Monero address"));

        let refund_address = "86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC";
        let valid_json = router.clone().oneshot(
            Request::builder().method("POST").uri(format!("/pay/{pk}/orders/{order_id}/refund-address"))
                .header("accept", "application/json")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("refund_address={refund_address}"))).unwrap()
        ).await.unwrap();
        assert_eq!(valid_json.status(), StatusCode::OK);
        assert!(body_text(valid_json).await.contains("\"ok\":true"));
        let submit = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/pay/{pk}/orders/{order_id}/refund-address"))
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
                    .uri(format!("/pay/{pk}/orders/{order_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let after_html = body_text(after).await;
        assert!(after_html.contains(refund_address), "expected the real, just-saved refund address shown, got: {after_html}");
        assert!(after_html.contains("id=\"refund_address\""), "expected the saved address to remain editable, got: {after_html}");
        assert!(after_html.contains("refund-field is-saved"));
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
        let order_id = create_order(&router, &pk, "25.00").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/pay/{pk}/orders/{order_id}/refund-address"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("refund_address="))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a rejected submission re-renders the page, it doesn't redirect");
        let html = body_text(response).await;
        assert!(html.contains("Enter a refund address."), "expected a clear inline error, got: {html}");
        assert!(html.contains(&order_id), "the real checkout page must still be shown, not a bare error, got: {html}");
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
        let order_id = create_order(&router, &pk, "10.00").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/pay/{pk}/orders/{order_id}/status"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["status"], "pending");
        assert_eq!(body["confirmations"], 0);
        assert!(body["confirmations_required"].is_number());
        assert!(body["error"].is_null());
    }

    #[tokio::test]
    async fn the_checkout_events_stream_pushes_a_change_made_on_the_engine() {
        let (state, engine) = test_state_with_real_engine().await;
        let engine_client = state.engine_client.clone();
        let router = build_router(state);
        let session_token =
            signed_up_and_logged_in_session_token(&router, "checkout-events@example.com", "correct horse battery staple").await;
        let pk = create_connection(&router, &session_token).await;
        let order_id = create_order(&router, &pk, "10.00").await;

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/pay/{pk}/orders/{order_id}/events?fragments=true"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let mut body = response.into_body();
        let (mut pending, mut parser) = (Vec::new(), crate::live::SseTestParser::default());

        let (event, fragment) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser).await.unwrap();
        assert_eq!(event, "fragment");
        assert!(fragment.contains(r#"id="live-status""#), "got: {fragment}");
        assert!(!fragment.contains("double-spend-banner"));
        let (event, status) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser).await.unwrap();
        assert_eq!(event, "status");
        let status: serde_json::Value = serde_json::from_str(&status).unwrap();
        assert_eq!(status["status"], "pending");
        assert_eq!(status["is_terminal"], false);
        assert_eq!(engine_client.live_upstream_count(), 1, "one engine stream for this store");

        // A change the engine makes on its own, not through monokulo.
        assert!(engine.store().lock().unwrap().mark_double_spend_detected(&order_id, crate::now_unix()).unwrap());

        let (event, fragment) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser).await.unwrap();
        assert_eq!(event, "fragment");
        assert!(fragment.contains("double-spend-banner"), "got: {fragment}");
        let (event, status) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser).await.unwrap();
        assert_eq!(event, "status");
        assert!(status.contains("Double-spend"), "got: {status}");

        drop(body);
        assert_eq!(engine_client.live_upstream_count(), 0, "the engine stream closes with its last watcher");
    }

    #[tokio::test]
    async fn one_source_may_hold_only_so_many_open_streams_per_store() {
        let (mut state, _engine) = test_state_with_real_engine().await;
        state.event_streams = std::sync::Arc::new(crate::http::stream_limit::StreamLimiter::new(1));
        let router = build_router(state);
        let session_token =
            signed_up_and_logged_in_session_token(&router, "checkout-events-cap@example.com", "correct horse battery staple").await;
        let pk = create_connection(&router, &session_token).await;
        let order_id = create_order(&router, &pk, "10.00").await;
        let open = |ip: [u8; 4]| {
            let mut request = Request::builder().uri(format!("/pay/{pk}/orders/{order_id}/events")).body(Body::empty()).unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((ip, 40000))));
            router.clone().oneshot(request)
        };

        let first = open([192, 0, 2, 1]).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(open([192, 0, 2, 1]).await.unwrap().status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(open([192, 0, 2, 2]).await.unwrap().status(), StatusCode::OK, "another source has its own allowance");

        // Closing the stream frees its slot.
        drop(first);
        assert_eq!(open([192, 0, 2, 1]).await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn the_checkout_events_stream_404s_for_an_unknown_order() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let session_token =
            signed_up_and_logged_in_session_token(&router, "checkout-events-404@example.com", "correct horse battery staple").await;
        let pk = create_connection(&router, &session_token).await;
        let response = router
            .oneshot(Request::builder().uri(format!("/pay/{pk}/orders/pay_missing/events")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn an_unknown_order_id_shows_a_real_not_found_page() {
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
        let order_id = create_order(&router, &pk, "25.00").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/pay/{pk}/orders/{order_id}/share"))
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
            html.contains(&format!(r#"src="/pay/{pk}/orders/{order_id}""#)),
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
