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
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};
use shared::order_status::OrderStatus;
use std::str::FromStr;

use crate::db::StoreConnectionRow;
use crate::engine_client::{EngineClientError, OrderDetailResponse, OrderView};
use crate::views;
use crate::views::checkout::{CheckoutPaymentViewModel, CheckoutShareViewModel, CheckoutViewModel};

use super::dashboard::redirect_302;
use super::{ApiError, AppState};

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
    // Amounts without trailing zeros: this is read by the customer.
    let xmr = |piconero: u64| {
        crate::views::trim_xmr(&shared::exchange_rate::format_piconero_as_xmr(piconero)).to_string()
    };
    let requested = xmr(order.xmr_amount_piconero);
    let received = xmr(order.amount_received_piconero);
    match order.status {
        OrderStatus::Partial => {
            let remaining = xmr(order
                .xmr_amount_piconero
                .saturating_sub(order.amount_received_piconero));
            Some(format!("{received} XMR received of {requested} XMR. Send the remaining {remaining} XMR to the address below."))
        }
        OrderStatus::Overpaid => {
            let extra = xmr(order
                .amount_received_piconero
                .saturating_sub(order.xmr_amount_piconero));
            Some(format!("{received} XMR received for a {requested} XMR order ({extra} XMR extra). Do not send more. Contact the merchant about the extra amount."))
        }
        _ => super::pos::derive_payment_error(order),
    }
}

/// What an order's QR code holds: while the customer still owes something
/// (`pending`, or the rest after a `partial` payment), a Monero payment URI
/// with the amount due (`monero:<address>?tx_amount=0.0006`), so a wallet
/// that reads it fills the amount in; otherwise the bare address. The
/// address never changes, so a code scanned before a partial payment still
/// pays the right order - only the amount it asks for differs.
pub(super) fn payment_uri(order: &crate::engine_client::OrderView) -> String {
    let due = order
        .xmr_amount_piconero
        .saturating_sub(order.amount_received_piconero);
    if matches!(order.status, OrderStatus::Pending | OrderStatus::Partial) && due > 0 {
        let amount = shared::exchange_rate::format_piconero_as_xmr(due);
        format!(
            "monero:{}?tx_amount={}",
            order.address,
            crate::views::trim_xmr(&amount)
        )
    } else {
        order.address.clone()
    }
}

/// The order's QR code ([`payment_uri`]), as page-ready SVG.
pub(super) fn payment_qr_svg(
    order: &crate::engine_client::OrderView,
) -> Result<crate::qr::QrSvg, ApiError> {
    crate::qr::encode(&payment_uri(order))
        .map_err(|e| ApiError::BadRequest(format!("failed to encode QR code: {e}")))
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
    order_id: &crate::db::OrderId,
) -> Result<
    (
        StoreConnectionRow,
        shared::auth::RawToken,
        OrderDetailResponse,
    ),
    LoadError,
> {
    load_order_with(state, pk, order_id, None).await
}

/// [`load_order`], reading the order through `shared` when given: a live
/// stream's re-read, which every other stream on the same order reuses.
async fn load_order_with(
    state: &AppState,
    pk: &str,
    order_id: &crate::db::OrderId,
    shared: Option<&crate::live::SharedDetail>,
) -> Result<
    (
        StoreConnectionRow,
        shared::auth::RawToken,
        OrderDetailResponse,
    ),
    LoadError,
> {
    let key = pk.to_string();
    let row = match state
        .db
        .read(move |db| db.get_store_connection_by_public_key(&key))
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return Err(LoadError::NotFound),
        Err(_) => return Err(LoadError::Internal),
    };
    let sk = match super::orders::decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(_) => return Err(LoadError::Internal),
    };
    let read = state.engine.client.get_order_detail(&sk, order_id);
    let detail = match shared {
        Some(shared) => shared.get(read).await,
        None => read.await,
    };
    match detail {
        Ok(detail) => Ok((row, sk, detail)),
        Err(EngineClientError::EngineError { status, .. })
            if status == reqwest::StatusCode::NOT_FOUND =>
        {
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
    /// `/events` only: stream each changed page fragment as its own
    /// message, routed by ssexi to the element it replaces
    /// (structured_logging.md part 8), then `status`, then `done` once the
    /// order is final. What the checkout page itself uses now; `fragments`
    /// stays for pages already open with the earlier script.
    routed: Option<bool>,
    /// `refresh=false` turns off the no-JavaScript meta refresh, so a
    /// customer can type a refund address without the page reloading under
    /// them. Set by the page's own "Auto Refresh" toggle link.
    refresh: Option<bool>,
    /// `light` or `dark` pins the page's theme; anything else (or none)
    /// follows the customer's device. For an integrator matching their own
    /// site, and for monokulo's share page passing a signed-in viewer's
    /// choice.
    theme: Option<String>,
    /// An IANA zone (`Australia/Perth`) to show times in. Without one (or
    /// with one this build doesn't know), times are UTC, and the page's
    /// script shows them in the customer's own zone.
    timezone: Option<String>,
}

impl CheckoutOptions {
    /// The zone named by `timezone`, if it names a real one.
    fn zone(&self) -> Option<&str> {
        self.timezone
            .as_deref()
            .filter(|name| jiff::tz::TimeZone::get(name).is_ok())
    }
    fn clock(&self) -> views::time::Clock {
        views::time::Clock::new(self.zone(), None, crate::now_unix())
    }
    fn theme(&self) -> crate::db::Theme {
        crate::db::Theme::from_db_str(self.theme.as_deref().unwrap_or(""))
    }
    fn is_compact(&self) -> bool {
        self.view.as_deref() == Some("compact")
    }
    fn refund_enabled(&self) -> bool {
        self.refund != Some(false)
    }
    fn auto_refresh(&self) -> bool {
        self.refresh != Some(false)
    }
    fn suffix(&self) -> String {
        let mut params = Vec::new();
        if self.is_compact() {
            params.push("view=compact");
        }
        if !self.refund_enabled() {
            params.push("refund=false");
        }
        if !self.auto_refresh() {
            params.push("refresh=false");
        }
        match self.theme() {
            crate::db::Theme::Light => params.push("theme=light"),
            crate::db::Theme::Dark => params.push("theme=dark"),
            crate::db::Theme::System => {}
        }
        let timezone = self.zone().map(|zone| {
            format!(
                "timezone={}",
                url::form_urlencoded::byte_serialize(zone.as_bytes()).collect::<String>()
            )
        });
        let params: Vec<&str> = params.into_iter().chain(timezone.as_deref()).collect();
        if params.is_empty() {
            String::new()
        } else {
            format!("?{}", params.join("&"))
        }
    }
    /// The same page's query string with auto refresh flipped.
    fn toggled_refresh_suffix(&self) -> String {
        CheckoutOptions {
            refresh: if self.auto_refresh() {
                Some(false)
            } else {
                None
            },
            ..self.clone()
        }
        .suffix()
    }
}

pub async fn checkout_page(
    State(state): State<AppState>,
    Path((pk, order_id)): Path<(String, crate::db::OrderId)>,
    Query(options): Query<CheckoutOptions>,
    headers: HeaderMap,
) -> Response {
    let (row, sk, detail) = match load_order(&state, &pk, &order_id).await {
        Ok(loaded) => loaded,
        Err(LoadError::NotFound) => return not_found_response(),
        Err(LoadError::Internal) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if must_open_from_shop(&state, &row, &order_id, &headers).await {
        return open_from_shop_response(&pk, &order_id);
    }
    with_vary_on_fetch_dest(render_checkout_page(&state, pk, row, sk, detail, None, &options).await)
}

/// Whether this request for a checkout (or share) page must be turned away
/// with "Open this payment from the shop's website": the store restricts
/// its checkout to its verified domains, the order was created by a browser
/// page (not with the store's secret key - `created_with_key`), and the
/// browser says the page is being loaded as something other than a frame.
///
/// That closes the last way to show such an order outside the shop: its
/// creation already needed a verified page's `Origin` (which a script
/// outside a browser can forge), and `frame-ancestors` already keeps it out
/// of other sites' frames, but the checkout URL itself could still be sent
/// to a customer to open directly.
///
/// "Framed" is judged by `Sec-Fetch-Dest`, which browsers set and pages
/// can't: `iframe` or `frame` pass. A request without the header (an older
/// browser, or anything that isn't a browser) is let through, as are
/// orders created with the key (WooCommerce, the dashboard, the POS and
/// payment links shared from it), orders monokulo has no record of, and
/// every order of an unrestricted store.
async fn must_open_from_shop(
    state: &AppState,
    row: &StoreConnectionRow,
    order_id: &crate::db::OrderId,
    headers: &HeaderMap,
) -> bool {
    let Some(dest) = headers
        .get("sec-fetch-dest")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    if dest.eq_ignore_ascii_case("iframe") || dest.eq_ignore_ascii_case("frame") {
        return false;
    }
    let (id, order_id) = (row.id.clone(), order_id.clone());
    state
        .db
        .read(move |db| {
            if !db.embed_restricted(&id).unwrap_or(false) {
                return Ok(false);
            }
            Ok::<_, crate::db::DbError>(matches!(
                db.get_order_currency_metadata(&id, &order_id),
                Ok(Some(metadata)) if !metadata.created_with_key
            ))
        })
        .await
        .unwrap_or(false)
}

fn open_from_shop_response(pk: &str, order_id: &crate::db::OrderId) -> Response {
    let chrome = views::PageChrome::from_user(None, format!("/pay/{pk}/orders/{order_id}"));
    with_vary_on_fetch_dest(
        (
            StatusCode::FORBIDDEN,
            views::checkout::open_from_shop_page(&chrome),
        )
            .into_response(),
    )
}

/// The checkout and share pages answer differently depending on
/// `Sec-Fetch-Dest` ([`must_open_from_shop`]), so any cache must key on it.
fn with_vary_on_fetch_dest(mut response: Response) -> Response {
    response.headers_mut().append(
        axum::http::header::VARY,
        axum::http::HeaderValue::from_static("Sec-Fetch-Dest"),
    );
    response
}

fn not_found_response() -> Response {
    let chrome = views::PageChrome::from_user(None, "/");
    (
        StatusCode::NOT_FOUND,
        views::checkout::not_found_page(&chrome),
    )
        .into_response()
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
    sk: shared::auth::RawToken,
    detail: OrderDetailResponse,
    refund_address_error: Option<String>,
    options: &CheckoutOptions,
) -> Response {
    if payment_qr_svg(&detail.order).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let order_id = detail.order.order_id.clone();
    let view =
        build_checkout_view(state, &pk, &row, &sk, detail, refund_address_error, options).await;
    let mut chrome = views::PageChrome::from_user(None, format!("/pay/{pk}/orders/{order_id}"));
    chrome.theme = options.theme();
    // Browser problem reports only from a store that opted in (D8).
    let id = row.id.clone();
    chrome.browser_reports = state
        .db
        .read(move |db| db.client_logging(&id))
        .await
        .unwrap_or(false);
    views::checkout::checkout_page(&chrome, &view).into_response()
}

/// Everything the checkout page shows, the QR code included: it asks for
/// the amount still due, so a partial payment redraws it. The live-update
/// stream re-renders only the parts that change
/// (`views::checkout::live_parts`).
async fn build_checkout_view(
    state: &AppState,
    pk: &str,
    row: &StoreConnectionRow,
    sk: &shared::auth::RawToken,
    detail: OrderDetailResponse,
    refund_address_error: Option<String>,
    options: &CheckoutOptions,
) -> CheckoutViewModel {
    // A reasonable, safe-side default if the engine is briefly unreachable
    // for this one extra call - the order data itself already loaded fine
    // above, so this page still shows something real rather than failing
    // outright over a field that only affects the confirmation-progress
    // display.
    let confirmations_required =
        super::pos::resolve_confirmations_required(state, &row.id, sk, &detail.order.order_id)
            .await;

    let (id, order_id) = (row.id.clone(), detail.order.order_id.clone());
    let (amount, currency) = match state
        .db
        .read(move |db| db.get_order_currency_metadata(&id, &order_id))
        .await
    {
        Ok(Some(metadata)) => (metadata.amount, metadata.currency),
        _ => ("—".to_string(), "".to_string()),
    };

    let (status_text, status_class, is_terminal) = crate::views::order_state(detail.order.status);
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
        if matches!(
            detail.order.status,
            OrderStatus::Paid | OrderStatus::Overpaid
        ) {
            100
        } else {
            0
        }
    } else {
        ((detail.order.confirmations as f64 / confirmations_required as f64) * 100.0)
            .round()
            .min(100.0) as u8
    };
    CheckoutViewModel {
        order_id: detail.order.order_id.clone(),
        status_label: status_text.to_string(),
        status: detail.order.status.into(),
        status_class: status_class.to_string(),
        address: detail.order.address.clone(),
        qr_code_svg: payment_qr_svg(&detail.order).unwrap_or_default(),
        amount_due_xmr: shared::exchange_rate::format_piconero_as_xmr(
            detail
                .order
                .xmr_amount_piconero
                .saturating_sub(detail.order.amount_received_piconero),
        ),
        xmr_amount: shared::exchange_rate::format_piconero_as_xmr(detail.order.xmr_amount_piconero),
        amount_received_xmr: shared::exchange_rate::format_piconero_as_xmr(
            detail.order.amount_received_piconero,
        ),
        amount,
        currency,
        confirmations: detail.order.confirmations,
        confirmations_required,
        progress_percent,
        is_terminal,
        double_spend_detected_at: detail.order.double_spend_detected_at,
        clock: options.clock(),
        local_times: options.zone().is_none(),
        expires_in_display: crate::templates::format_duration_until(
            detail.order.expires_at,
            crate::now_unix(),
        ),
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
    Path((pk, order_id)): Path<(String, crate::db::OrderId)>,
    Query(options): Query<CheckoutOptions>,
    headers: HeaderMap,
    Form(form): Form<SetRefundAddressForm>,
) -> Response {
    let wants_json = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("application/json"));
    let (row, sk, detail) = match load_order(&state, &pk, &order_id).await {
        Ok(loaded) => loaded,
        Err(LoadError::NotFound) => return not_found_response(),
        Err(LoadError::Internal) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // Once the order is settled (paid, overpaid or expired) a refund
    // address already on record is locked: a refund may be on its way to
    // it, and anyone who comes by the order URL later (a shared link, a
    // browser history) must not be able to redirect it. An order with
    // nothing received and no address yet can still have one set.
    let settled = matches!(
        detail.order.status,
        shared::order_status::OrderStatus::Paid
            | shared::order_status::OrderStatus::Overpaid
            | shared::order_status::OrderStatus::Expired
    );
    if settled && detail.order.refund_address.is_some() {
        if wants_json {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "The refund address for this order can no longer be changed."})),
            )
                .into_response();
        }
        return render_checkout_page(
            &state,
            pk,
            row,
            sk,
            detail,
            Some("The refund address for this order can no longer be changed.".to_string()),
            &options,
        )
        .await;
    }

    let refund_address = form.refund_address.trim();
    if refund_address.is_empty() {
        if wants_json {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Enter a refund address."})),
            )
                .into_response();
        }
        return render_checkout_page(
            &state,
            pk,
            row,
            sk,
            detail,
            Some("Enter a refund address.".to_string()),
            &options,
        )
        .await;
    }

    let parsed = monero::Address::from_str(refund_address);
    let payment_address = monero::Address::from_str(&detail.order.address);
    if !matches!((&parsed, &payment_address), (Ok(refund), Ok(payment)) if refund.network == payment.network)
    {
        if wants_json {
            return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Enter a valid Monero address for this store's network."}))).into_response();
        }
        return render_checkout_page(
            &state,
            pk,
            row,
            sk,
            detail,
            Some("Enter a valid Monero address for this store's network.".to_string()),
            &options,
        )
        .await;
    }

    match state
        .engine
        .client
        .set_refund_address(&sk, &order_id, refund_address)
        .await
    {
        Ok(()) if wants_json => Json(serde_json::json!({"ok": true})).into_response(),
        Ok(()) => redirect_302(&format!("/pay/{pk}/orders/{order_id}{}", options.suffix())),
        Err(e) => {
            tracing::error!(order.id = %order_id, store.id = %row.id, error = %e, "failed to set a refund address");
            if wants_json {
                return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": "Something went wrong saving that. Please try again."}))).into_response();
            }
            render_checkout_page(
                &state,
                pk,
                row,
                sk,
                detail,
                Some("Something went wrong saving that. Please try again.".to_string()),
                &options,
            )
            .await
        }
    }
}

#[derive(Serialize)]
pub struct CheckoutStatusResponse {
    pub status: crate::views::DisplayStatus,
    pub confirmations: u64,
    pub confirmations_required: u64,
    pub is_terminal: bool,
    pub error: Option<String>,
}

/// `GET /pay/{pk}/orders/{order_id}/status` - the small JSON the checkout
/// page's own polling script reads (`status`/`confirmations` only - the
/// same two fields the engine's old polling JS ever read from its
/// equivalent response).
pub async fn checkout_status(
    State(state): State<AppState>,
    Path((pk, order_id)): Path<(String, crate::db::OrderId)>,
) -> Response {
    match load_order(&state, &pk, &order_id).await {
        Ok((row, sk, detail)) => {
            let confirmations_required = super::pos::resolve_confirmations_required(
                &state,
                &row.id,
                &sk,
                &detail.order.order_id,
            )
            .await;
            let error = checkout_payment_message(&detail.order);
            let (_, _, is_terminal) = crate::views::order_state(detail.order.status);
            Json(CheckoutStatusResponse {
                status: detail.order.status.into(),
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
/// One client may hold only so many of these open per store at once
/// (`crate::abuse::streams`); past that the request gets `429`.
pub async fn checkout_events(
    State(state): State<AppState>,
    Path((pk, order_id)): Path<(String, crate::db::OrderId)>,
    Query(options): Query<CheckoutOptions>,
    extensions: axum::http::Extensions,
) -> Response {
    let (row, sk, _) = match load_order(&state, &pk, &order_id).await {
        Ok(loaded) => loaded,
        Err(LoadError::NotFound) => return ApiError::NotFound.into_response(),
        Err(LoadError::Internal) => return ApiError::Internal.into_response(),
    };
    // The client `http::abuse` identified; absent only when a test drives
    // the router without a connection, which fails open.
    let permit = match extensions.get::<crate::abuse::ClientIdentity>() {
        Some(client) => match state.abuse.streams.try_acquire(client, &pk) {
            Some(permit) => Some(permit),
            None => {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(serde_json::json!({ "error": "too many open update streams" })),
                )
                    .into_response()
            }
        },
        None => None,
    };
    let subscription = state.engine.client.subscribe_order(&row.id, &sk, &order_id);
    // One engine read per change for this order, however many streams
    // watch it.
    let shared = subscription.shared_detail();
    let fragments = options.fragments == Some(true);
    let routed = options.routed == Some(true);
    // What each routed part last looked like on this stream, so only
    // changed ones are sent.
    let sent: std::sync::Arc<parking_lot::Mutex<std::collections::HashMap<&'static str, String>>> =
        Default::default();
    crate::live::live_events(
        subscription,
        std::time::Duration::from_secs(30),
        move || {
            let sent = sent.clone();
            // Held by the stream, so the slot frees when the stream ends.
            let _permit = &permit;
            let (state, pk, order_id, options, shared) = (
                state.clone(),
                pk.clone(),
                order_id.clone(),
                options.clone(),
                shared.clone(),
            );
            async move {
                let (row, sk, detail) =
                    match load_order_with(&state, &pk, &order_id, Some(&shared)).await {
                        Ok(loaded) => loaded,
                        // The order is gone (or never was this store's): one
                        // last event and the stream ends, rather than staying
                        // open for good. An unreachable engine waits instead.
                        Err(LoadError::NotFound) => {
                            return Some(crate::live::LiveSnapshot {
                                events: vec![axum::response::sse::Event::default()
                                    .event("done")
                                    .data("not_found")],
                                fingerprint: "not_found".to_string(),
                                terminal: true,
                            })
                        }
                        Err(LoadError::Internal) => return None,
                    };
                let view =
                    build_checkout_view(&state, &pk, &row, &sk, detail, None, &options).await;
                let status = CheckoutStatusResponse {
                    status: view.status,
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
                    events.push(
                        axum::response::sse::Event::default()
                            .event("fragment")
                            .data(html),
                    );
                }
                if routed {
                    let mut sent = sent.lock();
                    for (id, part) in views::checkout::live_parts(&view) {
                        let html = part.into_string();
                        fingerprint.push_str(&html);
                        if sent.get(id) != Some(&html) {
                            let route = format!(r##"{{"target":"#{id}","swap":"outerHTML"}}"##);
                            events.push(
                                axum::response::sse::Event::default()
                                    .event(route)
                                    .data(html.clone()),
                            );
                            sent.insert(id, html);
                        }
                    }
                }
                events.push(
                    axum::response::sse::Event::default()
                        .event("status")
                        .data(status_json),
                );
                if routed && view.is_terminal {
                    events.push(
                        axum::response::sse::Event::default()
                            .event("done")
                            .data("final"),
                    );
                }
                Some(crate::live::LiveSnapshot {
                    events,
                    fingerprint,
                    terminal: view.is_terminal,
                })
            }
        },
    )
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
    Path((pk, order_id)): Path<(String, crate::db::OrderId)>,
    headers: HeaderMap,
) -> Response {
    let found = match load_order(&state, &pk, &order_id).await {
        Ok((row, _, _)) => {
            // The share page frames the checkout from monokulo itself, so it
            // would otherwise show a browser-created order of a restricted
            // store as a full page - the same rule applies to it.
            if must_open_from_shop(&state, &row, &order_id, &headers).await {
                return open_from_shop_response(&pk, &order_id);
            }
            true
        }
        Err(LoadError::NotFound) => false,
        Err(LoadError::Internal) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let status = if found {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    };
    let authed = super::resolve_authed_user(&state, &headers).await;
    let current_path = format!("/pay/{pk}/orders/{order_id}/share");
    let chrome =
        super::page_chrome(&state, authed.as_ref().map(|(user, _)| user), current_path).await;
    let view = CheckoutShareViewModel {
        pk,
        order_id,
        found,
    };
    with_vary_on_fetch_dest((status, views::checkout::share_page(&chrome, &view)).into_response())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_known_timezone_is_kept_in_the_pages_own_links_and_an_unknown_one_is_dropped() {
        let options = |zone: &str| super::CheckoutOptions {
            timezone: Some(zone.to_string()),
            ..Default::default()
        };
        assert_eq!(options("Asia/Tokyo").suffix(), "?timezone=Asia%2FTokyo");
        assert_eq!(options("Asia/Tokyo").clock().name(), "Asia/Tokyo");
        assert_eq!(options("Not/AZone").suffix(), "");
        assert_eq!(options("Not/AZone").clock().name(), "UTC");
        let compact = super::CheckoutOptions {
            view: Some("compact".into()),
            ..options("Europe/London")
        };
        assert_eq!(compact.suffix(), "?view=compact&timezone=Europe%2FLondon");
    }

    #[test]
    fn the_qr_code_asks_for_the_amount_due_and_only_while_one_is_due() {
        let order = |status: &str, received: u64| -> super::OrderView {
            serde_json::from_value(serde_json::json!({
                "order_id": "o_1", "merchant_order_id": null, "address": "4Addr", "xmr_amount_piconero": 1_000_000_000u64,
                "amount_received_piconero": received, "status": status, "confirmations": 0, "double_spend_detected_at": null,
                "refund_address": null, "created_at": 0, "expires_at": 0, "updated_at": 0, "first_scanned_height": null,
                "last_scanned_height": null, "currently_scanning": false,
            }))
            .unwrap()
        };
        assert_eq!(
            super::payment_uri(&order("pending", 0)),
            "monero:4Addr?tx_amount=0.001"
        );
        // After a partial payment, the code asks for the rest.
        assert_eq!(
            super::payment_uri(&order("partial", 400_000_000)),
            "monero:4Addr?tx_amount=0.0006"
        );
        // Nothing is due once paid, or while a payment confirms.
        assert_eq!(super::payment_uri(&order("paid", 1_000_000_000)), "4Addr");
        assert_eq!(
            super::payment_uri(&order("confirming", 1_000_000_000)),
            "4Addr"
        );
    }

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::engine_client::EngineClient;

    use super::super::{build_router, AppState};
    use super::CheckoutOptions;

    #[test]
    fn the_auto_refresh_toggle_flips_only_the_refresh_parameter() {
        let on = CheckoutOptions {
            view: Some("compact".to_string()),
            refund: Some(false),
            ..Default::default()
        };
        assert_eq!(on.suffix(), "?view=compact&refund=false");
        assert_eq!(
            on.toggled_refresh_suffix(),
            "?view=compact&refund=false&refresh=false"
        );

        let off = CheckoutOptions {
            refresh: Some(false),
            ..Default::default()
        };
        assert!(!off.auto_refresh());
        assert_eq!(off.suffix(), "?refresh=false");
        assert_eq!(off.toggled_refresh_suffix(), "");
    }
    use super::checkout_payment_message;

    #[test]
    fn checkout_amount_messages_use_exact_received_remaining_and_extra_xmr() {
        let mut order = crate::engine_client::OrderView {
            order_id: shared::ids::OrderId::new("pay_test".to_string()),
            merchant_order_id: None,
            address: "address".to_string(),
            xmr_amount_piconero: 500_000_000_000,
            amount_received_piconero: 200_000_000_000,
            status: shared::order_status::OrderStatus::Partial,
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
        assert_eq!(
            checkout_payment_message(&order).as_deref(),
            Some("0.2 XMR received of 0.5 XMR. Send the remaining 0.3 XMR to the address below.")
        );

        order.status = shared::order_status::OrderStatus::Overpaid;
        order.amount_received_piconero = 600_000_000_000;
        assert_eq!(checkout_payment_message(&order).as_deref(), Some("0.6 XMR received for a 0.5 XMR order (0.1 XMR extra). Do not send more. Contact the merchant about the extra amount."));

        order.double_spend_detected_at = Some(123);
        assert!(checkout_payment_message(&order)
            .unwrap()
            .contains("Double-spend"));
    }

    const TEST_VIEW_KEY_HEX: &str =
        "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str =
        "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
    // `"XMR"`, not a fiat currency - this module's tests are about the
    // checkout page's own rendering, not about exercising a real (mocked)
    // fiat provider (`pay.rs`'s own tests do that), and an XMR-denominated
    // order needs no provider configured at all.
    const TEST_CURRENCY: &str = "XMR";

    async fn test_state_with_real_engine() -> (AppState, scanner_test_support::TestEngineHandle) {
        let engine = scanner_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .spawn()
            .await;
        let engine_client = EngineClient::new(format!("http://{}", engine.addr));
        let state = AppState {
            engine: crate::http::Engine::new(engine_client),
            ..AppState::for_tests()
        };
        (state, engine)
    }

    use crate::http::test_support::body_json;

    use crate::http::test_support::body_text;

    use crate::http::test_support::signed_up_and_logged_in_session_token;

    async fn create_connection(router: &Router, session_token: &str) -> String {
        let body = serde_json::json!({
            "platform": "custom",
            "site_url": "https://shop.example.com",
            "view_key_hex": TEST_VIEW_KEY_HEX,
            "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
            "network": "mainnet",
            "domains": [],
            "base_currency": "XMR",
        });
        let response = router
            .clone()
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
        body_json(response)
            .await
            .as_object()
            .unwrap()
            .get("public_key")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
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
                        serde_json::json!({ "amount": amount, "currency": TEST_CURRENCY })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        body_json(response)
            .await
            .as_object()
            .unwrap()
            .get("order_id")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn the_checkout_page_shows_the_real_address_amount_and_fiat_quote() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "checkout-page@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let order_id = create_order(&router, &pk, "25.00").await;

        let response = router
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
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains(&order_id),
            "expected the real order_id shown, got: {html}"
        );
        assert!(
            html.contains("25.00"),
            "expected the real fiat amount shown, got: {html}"
        );
        assert!(
            html.contains(TEST_CURRENCY),
            "expected the real fiat currency shown, got: {html}"
        );
        assert!(
            html.contains("<svg"),
            "expected a real rendered QR code, got: {html}"
        );
        let pos_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/pay/{pk}/orders/{order_id}?view=compact&refund=false"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
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
        assert!(
            !html.contains(r#"<nav class="site-nav">"#),
            "the checkout page must not carry the site nav, got: {html}"
        );
        assert!(
            !html.contains("Monokulo"),
            "the checkout page must not carry the site brand/logo, got: {html}"
        );
        // The payment deadline must be a real,
        // already-formatted relative duration baked into the server
        // response - this page must stay meaningful with JavaScript
        // disabled, so nothing on it may rely on `data-timestamp` +
        // client-side formatting any more.
        assert!(
            html.contains(" left</span>"),
            "expected a server-rendered payment deadline, got: {html}"
        );
        assert!(
            !html.contains("data-timestamp"),
            "the checkout page must not depend on JS to format any timestamp, got: {html}"
        );
        // The server-rendered page remains meaningful without JavaScript.
        assert!(html.contains("/static/checkout.js"));
        assert!(html.contains("Auto Refresh: ON"));
        assert!(html.contains(r#"<noscript><meta http-equiv="refresh" content="60""#));
        assert!(
            html.contains("style=\"width: 0%\""),
            "expected a real, already-computed progress-bar fill, got: {html}"
        );
    }

    /// The checkout's refund-address form, a plain form POST with no JS,
    /// reaches the engine through its admin API
    /// (`POST /api/v1/admin/tenant/orders/{order_id}/refund-address`) and
    /// the address really is stored there.
    #[tokio::test]
    async fn setting_a_refund_address_through_the_checkout_pages_own_form_persists_it_on_the_engine(
    ) {
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
        assert!(
            before_html.contains("id=\"refund_address\""),
            "expected the refund-address form present before one is set, got: {before_html}"
        );

        let invalid = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/pay/{pk}/orders/{order_id}/refund-address?view=compact"
                    ))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("refund_address=not-an-address"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::OK);
        let invalid_html = body_text(invalid).await;
        assert!(invalid_html.contains("Enter a valid Monero address"));
        assert!(invalid_html.contains("refund-address?view=compact"));

        let invalid_json = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/pay/{pk}/orders/{order_id}/refund-address"))
                    .header("accept", "application/json")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("refund_address=not-an-address"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid_json.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(invalid_json)
            .await
            .contains("valid Monero address"));

        let refund_address = "86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC";
        let valid_json = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/pay/{pk}/orders/{order_id}/refund-address"))
                    .header("accept", "application/json")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!("refund_address={refund_address}")))
                    .unwrap(),
            )
            .await
            .unwrap();
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
        assert_eq!(
            submit.status(),
            StatusCode::FOUND,
            "expected a redirect back to the plain checkout page"
        );

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
        assert!(
            after_html.contains(refund_address),
            "expected the real, just-saved refund address shown, got: {after_html}"
        );
        assert!(
            after_html.contains("id=\"refund_address\""),
            "expected the saved address to remain editable, got: {after_html}"
        );
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
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a rejected submission re-renders the page, it doesn't redirect"
        );
        let html = body_text(response).await;
        assert!(
            html.contains("Enter a refund address."),
            "expected a clear inline error, got: {html}"
        );
        assert!(
            html.contains(&order_id),
            "the real checkout page must still be shown, not a bare error, got: {html}"
        );
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
        let engine_client = state.engine.client.clone();
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "checkout-events@example.com",
            "correct horse battery staple",
        )
        .await;
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

        let (event, fragment) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser)
            .await
            .unwrap();
        assert_eq!(event, "fragment");
        assert!(fragment.contains(r#"id="live-status""#), "got: {fragment}");
        assert!(!fragment.contains("state-double-spend"));
        let (event, status) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser)
            .await
            .unwrap();
        assert_eq!(event, "status");
        let status: serde_json::Value = serde_json::from_str(&status).unwrap();
        assert_eq!(status["status"], "pending");
        assert_eq!(status["is_terminal"], false);
        assert_eq!(
            engine_client.live_upstream_count(),
            1,
            "one engine stream for this store"
        );

        // A change the engine makes on its own, not through monokulo.
        assert!(engine
            .store()
            .lock()
            .mark_double_spend_detected(
                &shared::ids::OrderId::new(order_id.to_string()),
                crate::now_unix()
            )
            .unwrap());

        let (event, fragment) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser)
            .await
            .unwrap();
        assert_eq!(event, "fragment");
        assert!(fragment.contains("state-double-spend"), "got: {fragment}");
        let (event, status) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser)
            .await
            .unwrap();
        assert_eq!(event, "status");
        assert!(status.contains("Double-spend"), "got: {status}");

        drop(body);
        assert_eq!(
            engine_client.live_upstream_count(),
            0,
            "the engine stream closes with its last watcher"
        );
    }

    /// The checkout page's own stream (`routed=true`, structured_logging.md
    /// part 8): every live part at first, each routed to the element it
    /// replaces, then only the parts that change.
    #[tokio::test]
    async fn the_routed_checkout_stream_sends_only_changed_parts_to_their_elements() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "checkout-routed@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let order_id = create_order(&router, &pk, "10.00").await;

        let page = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/pay/{pk}/orders/{order_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = String::from_utf8(
            page.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        let events_url = format!("/pay/{pk}/orders/{order_id}/events?routed=true");
        assert!(
            html.contains(&format!(
                r#"id="checkout-stream" hidden fx-action="{events_url}" fx-trigger="fx:inited""#
            )),
            "{html}"
        );
        assert!(
            html.contains(r#"fx-trigger="refund:save""#) && html.contains("/static/ssexi.js"),
            "{html}"
        );
        assert!(
            !html.contains("/static/telemetry.js"),
            "no browser reports from the checkout (D8)"
        );

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&events_url)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let mut body = response.into_body();
        let (mut pending, mut parser) = (Vec::new(), crate::live::SseTestParser::default());
        let mut first = Vec::new();
        loop {
            let (event, data) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser)
                .await
                .unwrap();
            if event == "status" {
                break;
            }
            first.push((event, data));
        }
        let targets: Vec<&str> = first.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(
            targets,
            [
                r##"{"target":"#live-status","swap":"outerHTML"}"##,
                r##"{"target":"#live-pay","swap":"outerHTML"}"##,
                r##"{"target":"#address-label","swap":"outerHTML"}"##,
                r##"{"target":"#live-progress","swap":"outerHTML"}"##,
                r##"{"target":"#live-payments","swap":"outerHTML"}"##,
            ]
        );
        assert!(
            first[0]
                .1
                .starts_with(r#"<div id="live-status" class="stage-slot" data-live"#),
            "{}",
            first[0].1
        );

        assert!(engine
            .store()
            .lock()
            .mark_double_spend_detected(
                &shared::ids::OrderId::new(order_id.to_string()),
                crate::now_unix()
            )
            .unwrap());
        let (event, data) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser)
            .await
            .unwrap();
        assert_eq!(
            event, r##"{"target":"#live-status","swap":"outerHTML"}"##,
            "only what changed"
        );
        assert!(data.contains("state-double-spend"), "{data}");
        let (event, _) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser)
            .await
            .unwrap();
        assert_eq!(event, "status");
    }

    #[tokio::test]
    async fn one_source_may_hold_only_so_many_open_streams_per_store() {
        let (mut state, _engine) = test_state_with_real_engine().await;
        state.abuse = std::sync::Arc::new(crate::abuse::AbuseProtection::new(
            crate::abuse::AbuseConfig {
                stream_cap: 1,
                ..Default::default()
            },
        ));
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "checkout-events-cap@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let order_id = create_order(&router, &pk, "10.00").await;
        let open = |ip: [u8; 4]| {
            let mut request = Request::builder()
                .uri(format!("/pay/{pk}/orders/{order_id}/events"))
                .body(Body::empty())
                .unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                std::net::SocketAddr::from((ip, 40000)),
            ));
            router.clone().oneshot(request)
        };

        let first = open([192, 0, 2, 1]).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(
            open([192, 0, 2, 1]).await.unwrap().status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            open([192, 0, 2, 2]).await.unwrap().status(),
            StatusCode::OK,
            "another source has its own allowance"
        );

        // Closing the stream frees its slot.
        drop(first);
        assert_eq!(open([192, 0, 2, 1]).await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn the_checkout_events_stream_404s_for_an_unknown_order() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "checkout-events-404@example.com",
            "correct horse battery staple",
        )
        .await;
        let pk = create_connection(&router, &session_token).await;
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/pay/{pk}/orders/pay_missing/events"))
                    .body(Body::empty())
                    .unwrap(),
            )
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

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "checkout-share@example.com",
            "correct horse battery staple",
        )
        .await;
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
        assert!(
            html.contains(r#"<nav class="site-nav">"#),
            "expected the real site nav, got: {html}"
        );
        assert!(
            html.contains("Monokulo"),
            "expected the real site brand, got: {html}"
        );
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
        assert!(
            html.contains(r#"<nav class="site-nav">"#),
            "expected the real site nav even on the not-found state, got: {html}"
        );
        assert!(
            !html.contains("<iframe"),
            "must not render a broken iframe pointing at a nonexistent order"
        );
    }
}
