//! In-person Point-of-Sale terminal - a Square-Terminal-like screen a
//! merchant runs on a device at the counter: enter an amount in the store's
//! own `base_currency`, show the shared checkout iframe for the customer to
//! pay, then react live
//! to the payment appearing.
//!
//! Deliberately behind [`AuthedUser`] and connection-ownership-checked
//! ([`load_owned_connection`]) exactly like every other `/stores/{id}/*`
//! route (`http::orders`'s own module doc comment) - unlike the machine
//! API's order creation (`POST /api/v1/orders`), a POS terminal is operated
//! by the merchant themselves, logged in, standing at the register.
//!
//! Order creation reuses the exact same pricing/threshold-resolution path
//! `http::orders::create_order` and `api::v1::orders::create` already use
//! (`crate::confirmation_thresholds::resolve_for_order`) - a POS sale is not
//! a different kind of order, just a different UI for creating one, always
//! denominated in this store's own `base_currency` rather than a
//! caller-chosen one (there's no currency picker on a terminal screen).
//!
//! Unlike the public checkout page (`http::checkout`, which must stay
//! usable with JavaScript disabled), this screen is a merchant-operated dashboard tool in the same
//! bucket as the rest of the signed-in pages, which already use JS as
//! progressive enhancement elsewhere (the order page's share
//! button/local-time script). A live Square-Terminal-style keypad, stacked
//! backgrounded payments have no meaningful no-JS fallback, so this screen leans on
//! JS for its real interaction loop rather than a meta-refresh. Payment
//! status arrives over one Server-Sent Events stream per terminal
//! ([`order_events`]); [`order_status`] remains for one-off reads.
//!
//! **Error surfacing**: [`derive_payment_error`] flags
//! exactly the cases where the merchant, not just the customer's wallet,
//! needs to step in - a double-spend, an amount that doesn't match what was
//! asked for (under- or over-paid), or the order expiring before it ever
//! got there. The status stream itself dropping (the server unreachable) is
//! surfaced by the connection failing, not a field on a status event - the
//! client already has to handle "the connection failed" separately from
//! "the status says there's a problem".

use engine_protocol::order_status::OrderStatus;
use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};

use crate::api::runtime::extract::{JsonBody, QueryParams};
use crate::engine_client::{EngineClientError, OrderView};
use crate::views;
use crate::views::pos::PosViewModel;

use super::orders::{decrypt_sk, load_owned_connection};
use super::{ApiError, AppState, AuthedUser};
use crate::crypto::AtRestKey;
use crate::db::Database;
use crate::http::Engine;

/// `GET /stores/{id}/pos` - the terminal screen itself.
pub async fn pos_page(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<crate::db::ConnectionId>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // The keypad types in the currency's usual minor units (cents for USD,
    // whole yen, all 12 places for XMR). The server takes up to 12 in any
    // currency, so this only shapes what's typed, never what's accepted.
    let base_currency_decimals = crate::currencies::minor_units(&row.base_currency);

    let store_id = row.id.clone();
    let client_logging = state
        .db
        .read(move |db| db.client_logging(&store_id))
        .await
        .unwrap_or(false);
    let row = row.into_row();
    let view = PosViewModel {
        connection_id: id,
        display_name: row.name.clone(),
        base_currency: row.base_currency,
        base_currency_decimals,
        client_logging,
    };
    let chrome = super::page_chrome(
        &state,
        Some(&user),
        format!("/stores/{}/pos", view.connection_id),
    )
    .await;
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
    /// field `http::orders`'s dashboard form and the machine API
    /// already write, so it shows up wherever any other order's
    /// `merchant_order_id` does (the order detail page, `OrderView`).
    /// Trimmed and treated as absent if empty, same convention
    /// `http::orders::create_order`'s own form field already follows.
    #[serde(default)]
    pub merchant_order_id: Option<String>,
    #[serde(default)]
    pub request_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PosCreateOrderResponse {
    pub order_id: crate::db::OrderId,
    pub address: String,
    pub xmr_amount: String,
    pub amount: String,
    pub currency: String,
    pub confirmations_required: u64,
    pub expires_at: i64,
    pub merchant_order_id: Option<String>,
}

/// `POST /stores/{id}/pos/orders` - creates a real order,
/// always priced in this store's own `base_currency` (a POS terminal has no
/// currency picker). Mirrors
/// `http::orders::create_order`/`api::v1::orders::create` field for field;
/// see either's own doc comment for why the pricing/threshold-resolution
/// steps look the way they do.
pub async fn create_order(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<crate::db::ConnectionId>,
    JsonBody(req): JsonBody<PosCreateOrderRequest>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(()) => return ApiError::Internal.into_response(),
    };

    if let Some(key) = req.request_key.as_deref() {
        if key.len() > 100 || key.is_empty() {
            return ApiError::BadRequest("Invalid request key.".to_string()).into_response();
        }
    }

    let policy = crate::confirmation_thresholds::lock_policy(&row.tenant_public_key).await;
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(()) => return ApiError::Internal.into_response(),
    };

    if let Some(key) = req.request_key.as_deref() {
        let (store_id, key) = (row.id.clone(), key.to_string());
        let existing = state
            .db
            .read(move |db| db.pos_order_by_request_key(&store_id, &key))
            .await;
        match existing {
            Ok(Some(existing)) => {
                let sk = match decrypt_sk(&state.encryption_key, &row) {
                    Ok(sk) => sk,
                    Err(()) => return ApiError::Internal.into_response(),
                };
                let detail = match state.engine.client.get_order_detail(&sk, &existing).await {
                    Ok(detail) => detail,
                    Err(error) => return engine_failure(&error),
                };
                let (store_id, order_id) = (row.id.clone(), existing.clone());
                let metadata_result = state
                    .db
                    .read(move |db| db.get_order_currency_metadata(&store_id, &order_id))
                    .await;
                let metadata = match metadata_result {
                    Ok(Some(metadata)) => metadata,
                    _ => return ApiError::Internal.into_response(),
                };
                if metadata.amount != req.amount.trim()
                    || detail.order.merchant_order_id.as_deref().unwrap_or("")
                        != req.merchant_order_id.as_deref().unwrap_or("").trim()
                {
                    return ApiError::BadRequest(
                        "Request key was already used for a different order.".to_string(),
                    )
                    .into_response();
                }
                return Json(PosCreateOrderResponse {
                    order_id: existing,
                    address: detail.order.address,
                    xmr_amount: shared::exchange_rate::format_piconero_as_xmr(
                        detail.order.xmr_amount_piconero,
                    ),
                    amount: metadata.amount,
                    currency: metadata.currency,
                    confirmations_required: metadata
                        .confirmations_required_applied
                        .unwrap_or(crate::confirmation_thresholds::FALLBACK_CONFIRMATIONS),
                    expires_at: detail.order.expires_at,
                    merchant_order_id: detail.order.merchant_order_id,
                })
                .into_response();
            }
            Ok(None) => {}
            Err(_) => return ApiError::Internal.into_response(),
        }
    }

    let amount = req.amount.trim();
    if amount.is_empty() {
        return ApiError::BadRequest("Enter an amount.".to_string()).into_response();
    }
    let currency = row.base_currency.clone();
    let merchant_order_id = req
        .merchant_order_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if merchant_order_id
        .as_ref()
        .is_some_and(|reference| reference.chars().count() > 120)
    {
        return ApiError::BadRequest("Reference must be 120 characters or fewer.".to_string())
            .into_response();
    }

    let (piconero_per_unit, provider) = match state
        .exchange_rate
        .piconero_per_unit_for(&row, &currency)
        .await
    {
        Ok(Some(result)) => result,
        Ok(None) => {
            return ApiError::BadRequest(format!("unsupported currency: {currency}"))
                .into_response()
        }
        Err(crate::exchange_rate_config::ExchangeRateLookupError::ProviderNotConfigured(_)) => {
            return ApiError::BadRequest(format!("unsupported currency: {currency}"))
                .into_response();
        }
        Err(e) => {
            tracing::warn!(store.id = %row.id, currency = ?currency, error = %e, "no usable exchange rate right now");
            return match e.unavailable(&currency) {
                Some(unavailable) => unavailable.problem().into_response(),
                None => ApiError::Internal.into_response(),
            };
        }
    };
    let xmr_amount_piconero =
        match shared::exchange_rate::compute_order_amount(&currency, amount, piconero_per_unit) {
            Ok(amount) => amount,
            Err(e) => return ApiError::BadRequest(e.to_string()).into_response(),
        };

    let sk = match decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return ApiError::Internal.into_response(),
    };
    let resolution = match crate::confirmation_thresholds::resolve_for_order(
        &state,
        &row,
        &policy,
        &sk,
        &currency,
        piconero_per_unit,
        xmr_amount_piconero,
    )
    .await
    {
        Ok(resolution) => resolution,
        Err(crate::confirmation_thresholds::ResolveError::NoRate(message)) => {
            return ApiError::BadRequest(message).into_response()
        }
        Err(e) => return e.problem().into_response(),
    };

    match state
        .engine
        .client
        .create_order(
            &sk,
            shared::xmr_amount::Piconero(xmr_amount_piconero),
            merchant_order_id.clone(),
            // The till takes an amount, not a list of items: nothing to
            // describe, so the checkout shows the store's payment
            // description, if it has one.
            None,
            Some(resolution.confirmations_required),
            // The terminal's own key for this sale: a retry after a lost
            // answer (or a failed local write below) gets the same order.
            req.request_key.as_ref().map(|key| format!("pos:{key}")),
        )
        .await
    {
        Ok(order) => {
            let (store_id, order_id) = (row.id.clone(), order.order_id.clone());
            let (request_key, reference) = (req.request_key.clone(), merchant_order_id.clone());
            let recorded = state
                .db
                .write(move |db| {
                    db.insert_pos_order(
                        &store_id,
                        &order_id,
                        request_key.as_deref(),
                        reference.as_deref(),
                        crate::now_unix(),
                    )
                })
                .await;
            if let Err(e) = recorded {
                tracing::error!(order.id = %order.order_id, store.id = %row.id, error = %e, "failed to record a POS order");
                return ApiError::Internal.into_response();
            }
            let (store_id, order_id, order_currency, order_amount, provider) = (
                row.id.clone(),
                order.order_id.clone(),
                currency.clone(),
                amount.to_string(),
                provider.to_string(),
            );
            let base_currency = resolution.base_currency.clone();
            let (base_rate, confirmations) = (
                resolution.base_currency_piconero_per_unit,
                resolution.confirmations_required,
            );
            let recorded = state
                .db
                .write(move |db| {
                    db.create_order_currency_metadata(
                        &store_id,
                        &order_id,
                        &order_currency,
                        &order_amount,
                        shared::xmr_amount::Piconero(piconero_per_unit),
                        &provider,
                        crate::now_unix(),
                        &base_currency,
                        base_rate,
                        confirmations,
                        // The merchant's own signed-in session: as trusted as
                        // the key.
                        true,
                        Some("pos"),
                        // The till's reference (often the customer's name)
                        // is for the payer to see.
                        true,
                    )
                })
                .await;
            if let Err(e) = recorded {
                tracing::error!(
                    order.id = %order.order_id,
                    store.id = %row.id,
                    error = %e,
                    "failed to record local fiat metadata - the real order still exists on the engine and this \
                     response is still correct, but its fiat display on monokulo's own pages will be missing"
                );
            }
            let xmr_amount =
                shared::exchange_rate::format_piconero_as_xmr(order.xmr_amount_piconero);

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
        Err(EngineClientError::EngineError { status, message })
            if status == reqwest::StatusCode::BAD_REQUEST =>
        {
            ApiError::BadRequest(message).into_response()
        }
        Err(error) if error.is_unavailable() => ApiError::EngineUnavailable.into_response(),
        Err(_) => ApiError::Internal.into_response(),
    }
}

#[derive(Debug, Serialize)]
pub struct PosStatusResponse {
    pub status: engine_protocol::order_status::OrderStatus,
    pub updated_at: i64,
    pub confirmations: u64,
    pub confirmations_required: u64,
    pub is_terminal: bool,
    /// `Some(...)` exactly in the cases the merchant needs to step in and
    /// deal with the customer directly - see [`derive_payment_error`].
    /// `None` covers both "still pending/confirming normally" and "paid" -
    /// a real, error-free success is not an error just because it's also
    /// terminal.
    pub error: Option<String>,
    /// What has arrived so far and what is still owed, so a live update to
    /// a partly paid order shows the real amounts.
    pub received_xmr: String,
    pub remaining_xmr: String,
    /// After a partial payment, the QR code redrawn for what is still owed
    /// ([`super::checkout::payment_uri`]); absent otherwise, when the code
    /// the terminal already has is still right.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qr_svg: Option<crate::qr::QrSvg>,
    /// What the POS itself holds about the order, so every terminal
    /// watching it hears of a change at once (the engine knows none of it).
    pub backgrounded: bool,
    pub cancelled_at: Option<i64>,
    pub refund_address: Option<String>,
}

#[derive(Serialize)]
pub struct PosOrderData {
    order_id: crate::db::OrderId,
    /// The order id and the address as the rest of the site shows them
    /// shortened (`views::short_id_text`, `views::short_address_text`), so
    /// the POS app never cuts an identifier itself.
    order_id_short: String,
    merchant_order_id: Option<String>,
    address: String,
    address_short: String,
    xmr_amount: String,
    amount: String,
    currency: String,
    status: engine_protocol::order_status::OrderStatus,
    updated_at: i64,
    confirmations: u64,
    confirmations_required: u64,
    error: Option<String>,
    backgrounded: bool,
    cancelled_at: Option<i64>,
    created_at: i64,
    expires_at: i64,
    /// XMR received so far, and what is still owed (zero once covered),
    /// both formatted here like `xmr_amount`.
    received_xmr: String,
    remaining_xmr: String,
    /// The customer's refund address, once recorded.
    refund_address: Option<String>,
    /// The payment QR code (an SVG, rendered here like the checkout's own),
    /// only on a single order's detail - never in lists, where it would
    /// only add weight.
    #[serde(skip_serializing_if = "Option::is_none")]
    qr_svg: Option<crate::qr::QrSvg>,
}

async fn pos_order_data(
    state: &AppState,
    connection_id: &crate::db::ConnectionId,
    sk: &credentials::auth::RawToken,
    row: crate::db::PosOrderRow,
) -> Result<PosOrderData, EngineClientError> {
    let detail = state
        .engine
        .client
        .get_order_detail(sk, &row.order_id)
        .await?;
    let metadata = order_metadata(state, connection_id, vec![row.order_id.clone()])
        .await
        .remove(&row.order_id);
    let mut data = pos_order_view(metadata, row, &detail.order);
    data.qr_svg = super::checkout::payment_qr_svg(&detail.order).ok();
    Ok(data)
}

/// The fiat metadata of these orders, in one read; an order without any is
/// left out.
async fn order_metadata(
    state: &AppState,
    connection_id: &crate::db::ConnectionId,
    order_ids: Vec<crate::db::OrderId>,
) -> HashMap<crate::db::OrderId, crate::db::OrderCurrencyMetadataRow> {
    let store_id = connection_id.clone();
    state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>(
                order_ids
                    .into_iter()
                    .filter_map(|id| {
                        let metadata = db.get_order_currency_metadata(&store_id, &id).ok()??;
                        Some((id, metadata))
                    })
                    .collect(),
            )
        })
        .await
        .unwrap_or_default()
}

/// One POS order as the terminal shows it, from its local row and the
/// engine's view of it (read singly or in a batch).
fn pos_order_view(
    metadata: Option<crate::db::OrderCurrencyMetadataRow>,
    row: crate::db::PosOrderRow,
    order: &OrderView,
) -> PosOrderData {
    let confirmations_required = metadata
        .as_ref()
        .and_then(|m| m.confirmations_required_applied)
        .unwrap_or(crate::confirmation_thresholds::FALLBACK_CONFIRMATIONS);
    PosOrderData {
        order_id_short: crate::views::short_id_text(row.order_id.as_str()),
        order_id: row.order_id,
        merchant_order_id: order.merchant_order_id.clone(),
        address_short: crate::views::short_address_text(&order.address),
        address: order.address.clone(),
        xmr_amount: shared::exchange_rate::format_piconero_as_xmr(order.xmr_amount_piconero),
        amount: metadata
            .as_ref()
            .map(|m| m.amount.clone())
            .unwrap_or_else(|| {
                shared::exchange_rate::format_piconero_as_xmr(order.xmr_amount_piconero)
            }),
        currency: metadata
            .as_ref()
            .map(|m| m.currency.clone())
            .unwrap_or_else(|| "XMR".to_string()),
        status: order.status,
        updated_at: order.updated_at,
        confirmations: order.confirmations,
        confirmations_required,
        error: derive_payment_error(order),
        backgrounded: row.backgrounded,
        cancelled_at: order.cancelled_at,
        created_at: row.created_at,
        expires_at: order.expires_at,
        received_xmr: shared::exchange_rate::format_piconero_as_xmr(order.amount_received_piconero),
        remaining_xmr: shared::exchange_rate::format_piconero_as_xmr(
            order
                .xmr_amount_piconero
                .saturating_sub(order.amount_received_piconero),
        ),
        refund_address: order.refund_address.clone(),
        qr_svg: None,
    }
}

/// The engine's views of `order_ids`, keyed by id, in as few requests as its
/// per-request cap allows. The engine rate-limits each store, so a terminal
/// watching many orders must not spend one request per order.
async fn engine_orders(
    state: &AppState,
    sk: &credentials::auth::RawToken,
    order_ids: &[crate::db::OrderId],
) -> Result<HashMap<crate::db::OrderId, OrderView>, EngineClientError> {
    let mut orders = HashMap::with_capacity(order_ids.len());
    for chunk in order_ids.chunks(crate::engine_client::MAX_ORDER_IDS_PER_REQUEST) {
        for order in state.engine.client.list_orders_by_ids(sk, chunk).await? {
            orders.insert(order.order_id.clone(), order);
        }
    }
    Ok(orders)
}

/// The response for an engine call that failed: an order the engine does
/// not know is `404`; anything else (its rate limit, or no answer at all)
/// is a retryable `503`, not an opaque internal error.
fn engine_failure(error: &EngineClientError) -> Response {
    match error {
        EngineClientError::EngineError { status, .. }
            if *status == reqwest::StatusCode::NOT_FOUND =>
        {
            ApiError::NotFound.into_response()
        }
        _ => ApiError::EngineUnavailable.into_response(),
    }
}

#[derive(Deserialize)]
pub struct PosListQuery {
    offset: Option<i64>,
    limit: Option<i64>,
    search: Option<String>,
    /// `active`: every POS order still open, however old, in one list (no
    /// paging) - what the terminal's Active tab and background stack show.
    state: Option<String>,
}

/// Most open orders one engine page holds; the terminal reads pages until
/// a short one.
const OPEN_ORDERS_PAGE: u32 = 200;

/// Every POS order of this store that is still open: the engine's open
/// orders (web orders included; a cancelled one is closed) narrowed to the
/// POS's own.
async fn active_pos_orders(
    state: &AppState,
    connection_id: &crate::db::ConnectionId,
    sk: &credentials::auth::RawToken,
) -> Result<Vec<PosOrderData>, EngineClientError> {
    let mut open = Vec::new();
    loop {
        let page = state
            .engine
            .client
            .list_orders_page(sk, true, None, OPEN_ORDERS_PAGE, open.len() as u32)
            .await?;
        let short = (page.len() as u32) < OPEN_ORDERS_PAGE;
        open.extend(page);
        if short {
            break;
        }
    }
    let ids: Vec<crate::db::OrderId> = open.iter().map(|order| order.order_id.clone()).collect();
    let store_id = connection_id.clone();
    let rows = state
        .db
        .read(move |db| db.get_pos_orders(&store_id, &ids))
        .await
        .unwrap_or_default();
    let mut metadata = order_metadata(
        state,
        connection_id,
        rows.iter().map(|row| row.order_id.clone()).collect(),
    )
    .await;
    let mut rows: HashMap<crate::db::OrderId, crate::db::PosOrderRow> = rows
        .into_iter()
        .map(|row| (row.order_id.clone(), row))
        .collect();
    Ok(open
        .into_iter()
        .filter_map(|order| {
            rows.remove(&order.order_id)
                .map(|row| pos_order_view(metadata.remove(&row.order_id), row, &order))
        })
        .collect())
}

pub async fn list_orders(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<crate::db::ConnectionId>,
    QueryParams(query): QueryParams<PosListQuery>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(()) => return ApiError::Internal.into_response(),
    };
    let sk = match decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return ApiError::Internal.into_response(),
    };
    if query.state.as_deref() == Some("active") {
        return match active_pos_orders(&state, &id, &sk).await {
            Ok(orders) => {
                Json(serde_json::json!({"orders": orders, "total": orders.len()})).into_response()
            }
            Err(error) => engine_failure(&error),
        };
    }
    let offset = query.offset.unwrap_or(0).max(0);
    let limit = query.limit.unwrap_or(40).clamp(1, 100);
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|term| !term.is_empty());
    if search.is_some_and(|term| term.chars().count() > 120) {
        return ApiError::BadRequest("Search is too long.".to_string()).into_response();
    }
    let (store_id, search) = (id.clone(), search.map(str::to_string));
    let (rows, total) = match state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                db.list_pos_orders(&store_id, limit, offset, search.as_deref())?,
                db.count_pos_orders(&store_id, search.as_deref())?,
            ))
        })
        .await
    {
        Ok(found) => found,
        Err(_) => return ApiError::Internal.into_response(),
    };
    let ids: Vec<crate::db::OrderId> = rows.iter().map(|row| row.order_id.clone()).collect();
    let mut metadata = order_metadata(&state, &id, ids.clone()).await;
    let mut views = match engine_orders(&state, &sk, &ids).await {
        Ok(views) => views,
        Err(error) => return engine_failure(&error),
    };
    // An order the engine no longer knows is left out, as before.
    let orders: Vec<PosOrderData> = rows
        .into_iter()
        .filter_map(|row| {
            views
                .remove(&row.order_id)
                .map(|view| pos_order_view(metadata.remove(&row.order_id), row, &view))
        })
        .collect();
    Json(serde_json::json!({"orders": orders, "total": total, "offset": offset, "limit": limit}))
        .into_response()
}

pub async fn order_detail(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, order_id)): Path<(crate::db::ConnectionId, crate::db::OrderId)>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(()) => return ApiError::Internal.into_response(),
    };
    let (store_id, order) = (id.clone(), order_id.clone());
    let pos_row = match state
        .db
        .read(move |db| db.get_pos_order(&store_id, &order))
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(_) => return ApiError::Internal.into_response(),
    };
    let sk = match decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return ApiError::Internal.into_response(),
    };
    match pos_order_data(&state, &id, &sk, pos_row).await {
        Ok(data) => Json(data).into_response(),
        Err(error) => engine_failure(&error),
    }
}

pub async fn background_order(
    State(db): State<Database>,
    AuthedUser(user, _): AuthedUser,
    Path((id, order_id)): Path<(crate::db::ConnectionId, crate::db::OrderId)>,
) -> Response {
    if !matches!(load_owned_connection(&db, &user, &id).await, Ok(Some(_))) {
        return ApiError::NotFound.into_response();
    }
    match db
        .write(move |db| db.background_pos_order(&id, &order_id))
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => ApiError::NotFound.into_response(),
        Err(_) => ApiError::Internal.into_response(),
    }
}

pub async fn cancel_order(
    State(db): State<Database>,
    State(encryption_key): State<AtRestKey>,
    State(engine): State<Engine>,
    AuthedUser(user, _): AuthedUser,
    Path((id, order_id)): Path<(crate::db::ConnectionId, crate::db::OrderId)>,
) -> Response {
    let row = match load_owned_connection(&db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(()) => return ApiError::Internal.into_response(),
    };
    // Only the POS's own orders are its to cancel.
    let (store_id, order) = (id.clone(), order_id.clone());
    match db.read(move |db| db.get_pos_order(&store_id, &order)).await {
        Ok(Some(_)) => {}
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(_) => return ApiError::Internal.into_response(),
    }
    let sk = match decrypt_sk(&encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return ApiError::Internal.into_response(),
    };
    // The engine checks nothing has reached the order and cancels it in one
    // transaction, so a payment recorded first refuses the cancel and one
    // recorded after it still counts; everything that shows the order
    // (the checkout, the dashboard, the API, webhooks, apps) hears of it
    // from the engine.
    match engine.client.cancel_order(&sk, &order_id).await {
        Ok(()) => {}
        Err(EngineClientError::EngineError { status, .. })
            if status == reqwest::StatusCode::CONFLICT =>
        {
            return ApiError::BadRequest("This order has payment activity and cannot be cancelled. Background it for review instead.".to_string()).into_response();
        }
        Err(error) => return engine_failure(&error),
    }
    // Off the terminal's foreground, as before: a payment sent anyway
    // shows in the order for review.
    let (store_id, order) = (id.clone(), order_id.clone());
    match db
        .write(move |db| db.background_pos_order(&store_id, &order))
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => ApiError::Internal.into_response(),
    }
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
pub(crate) async fn resolve_confirmations_required(
    state: &AppState,
    connection_id: &crate::db::ConnectionId,
    sk: &credentials::auth::RawToken,
    order_id: &crate::db::OrderId,
) -> u64 {
    let (store_id, order) = (connection_id.clone(), order_id.clone());
    let local = state
        .db
        .read(move |db| db.get_order_currency_metadata(&store_id, &order))
        .await
        .unwrap_or_default();
    if let Some(applied) = local.and_then(|m| m.confirmations_required_applied) {
        return applied;
    }
    state
        .engine
        .client
        .get_tenant(sk)
        .await
        .map(|t| t.confirmations_required)
        .unwrap_or(crate::confirmation_thresholds::FALLBACK_CONFIRMATIONS)
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
    match order.status {
        OrderStatus::Partial => {
            Some("Underpaid - the customer sent less than the requested amount.".to_string())
        }
        OrderStatus::Overpaid => {
            Some("Overpaid - the customer sent more than the requested amount.".to_string())
        }
        OrderStatus::Expired => Some("This payment expired before it was completed.".to_string()),
        OrderStatus::Cancelled => None,
        OrderStatus::Pending
        | OrderStatus::Unconfirmed
        | OrderStatus::Confirming
        | OrderStatus::Paid => None,
    }
}

/// `GET /stores/{id}/pos/orders/{order_id}/status` - the
/// small JSON status of one POS order. The terminal screen itself follows
/// the same data live via [`order_events`].
pub async fn order_status(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, order_id)): Path<(crate::db::ConnectionId, crate::db::OrderId)>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(()) => return ApiError::Internal.into_response(),
    };
    let sk = match decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return ApiError::Internal.into_response(),
    };

    let (store_id, order) = (id.clone(), order_id.clone());
    let pos_row = match state
        .db
        .read(move |db| db.get_pos_order(&store_id, &order))
        .await
    {
        Ok(pos_row) => pos_row,
        Err(_) => return ApiError::Internal.into_response(),
    };
    match state.engine.client.get_order_detail(&sk, &order_id).await {
        Ok(detail) => Json(pos_status(&state, &row.id, &sk, &detail.order, pos_row.as_ref()).await)
            .into_response(),
        Err(error) => engine_failure(&error),
    }
}

/// `pos_row` is the order's POS row, `None` for an order the POS didn't
/// create.
async fn pos_status(
    state: &AppState,
    connection_id: &crate::db::ConnectionId,
    sk: &credentials::auth::RawToken,
    order: &OrderView,
    pos_row: Option<&crate::db::PosOrderRow>,
) -> PosStatusResponse {
    let confirmations_required =
        resolve_confirmations_required(state, connection_id, sk, &order.order_id).await;
    // `order_state`'s own `is_terminal` already accounts for a
    // 0-conf-trusted order: the engine only ever reports `"paid"`
    // once *that order's own* `confirmations_required` (however it
    // was resolved at creation - possibly `0`) has actually been
    // met, so there's no separate threshold check to fold in here.
    let (_, _, is_terminal) = crate::views::order_state(order.status);
    PosStatusResponse {
        status: order.status,
        updated_at: order.updated_at,
        confirmations: order.confirmations,
        confirmations_required,
        is_terminal,
        error: derive_payment_error(order),
        received_xmr: shared::exchange_rate::format_piconero_as_xmr(order.amount_received_piconero),
        remaining_xmr: shared::exchange_rate::format_piconero_as_xmr(
            order
                .xmr_amount_piconero
                .saturating_sub(order.amount_received_piconero),
        ),
        qr_svg: if order.status == OrderStatus::Partial {
            super::checkout::payment_qr_svg(order).ok()
        } else {
            None
        },
        backgrounded: pos_row.is_some_and(|row| row.backgrounded),
        cancelled_at: order.cancelled_at,
        refund_address: order.refund_address.clone(),
    }
}

/// More than any real counter has in flight at once; bounds how many
/// upstream reads one request can fan out to.
const MAX_WATCHED_ORDERS: usize = 32;

#[derive(Deserialize)]
pub struct PosEventsQuery {
    /// Comma-separated order ids - the one on screen plus every backgrounded one.
    orders: String,
}

/// `GET /stores/{id}/pos/events?orders=a,b,...` - one
/// Server-Sent Events stream for every order the terminal is watching,
/// replacing a poll loop per order. Each `status` event is
/// [`PosStatusResponse`] plus its `order_id`, sent on connect and again
/// only when that order changes (`crate::live`). An order stops being
/// reported once terminal (or unknown to this store); the terminal reopens
/// the stream with a new id list whenever the set it watches changes.
pub async fn order_events(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<crate::db::ConnectionId>,
    QueryParams(query): QueryParams<PosEventsQuery>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(()) => return ApiError::Internal.into_response(),
    };
    let sk = match decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return ApiError::Internal.into_response(),
    };
    let mut order_ids: Vec<crate::db::OrderId> = Vec::new();
    for order_id in query
        .orders
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        if !order_ids.iter().any(|seen| seen.as_str() == order_id) {
            order_ids.push(crate::db::OrderId::new(order_id));
        }
        // Past the cap the request is refused anyway: no point walking a
        // longer list (the de-duplication is quadratic).
        if order_ids.len() > MAX_WATCHED_ORDERS {
            break;
        }
    }
    if order_ids.is_empty() || order_ids.len() > MAX_WATCHED_ORDERS {
        return ApiError::BadRequest(format!(
            "orders must list between 1 and {MAX_WATCHED_ORDERS} order ids"
        ))
        .into_response();
    }

    let subscriptions = order_ids
        .iter()
        .map(|order_id| {
            (
                state.engine.client.subscribe_order(&row.id, &sk, order_id),
                state.db.order_changes().subscribe(&row.id, order_id),
            )
        })
        .collect();
    let connection_id = row.id.clone();
    // One engine read per change for every watched order together: the
    // engine rate-limits each store, and a busy counter watches many orders.
    crate::live::sse(crate::live::batch_snapshot_stream(
        subscriptions,
        std::time::Duration::from_secs(60),
        move |ids| {
            let (state, connection_id, sk) = (state.clone(), connection_id.clone(), sk.clone());
            async move {
                let mut views = engine_orders(&state, &sk, &ids).await.ok()?;
                // Without its POS row a backgrounded order would be sent as
                // in the foreground: a failed read waits for the next change.
                let (store_id, wanted) = (connection_id.clone(), ids.clone());
                let pos_rows: HashMap<_, _> = state
                    .db
                    .read(move |db| db.get_pos_orders(&store_id, &wanted))
                    .await
                    .ok()?
                    .into_iter()
                    .map(|row| (row.order_id.clone(), row))
                    .collect();
                let mut snapshots = Vec::with_capacity(ids.len());
                for order_id in ids {
                    let Some(order) = views.remove(&order_id) else {
                        // Not this store's order: nothing to watch.
                        snapshots.push((
                            order_id,
                            crate::live::LiveSnapshot {
                                events: Vec::new(),
                                fingerprint: String::new(),
                                terminal: true,
                            },
                        ));
                        continue;
                    };
                    let status =
                        pos_status(&state, &connection_id, &sk, &order, pos_rows.get(&order_id))
                            .await;
                    let mut json = serde_json::to_value(&status).ok()?;
                    json["order_id"] = serde_json::Value::String(order_id.to_string());
                    let data = json.to_string();
                    snapshots.push((
                        order_id,
                        crate::live::LiveSnapshot {
                            events: vec![axum::response::sse::Event::default()
                                .event("status")
                                .data(data.clone())],
                            fingerprint: data,
                            terminal: status.is_terminal,
                        },
                    ));
                }
                Some(snapshots)
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use tower::ServiceExt;

    use crate::engine_client::EngineClient;

    use super::super::{build_router, AppState};

    async fn test_state_with_real_engine() -> (AppState, engine_test_support::TestEngineHandle) {
        test_state_with_engine(engine_test_support::TestEngineConfig::new()).await
    }

    async fn test_state_with_engine(
        config: engine_test_support::TestEngineConfig,
    ) -> (AppState, engine_test_support::TestEngineHandle) {
        let engine = config
            .with_networks(&[monero::Network::Mainnet])
            .spawn()
            .await;
        let engine_client = EngineClient::embedded_for_tests(engine.router());
        let state = AppState {
            engine: crate::http::Engine::new(engine_client),
            ..AppState::for_tests()
        };
        (state, engine)
    }

    use crate::http::test_support::body_json;

    use crate::http::test_support::body_text;

    use crate::http::test_support::signed_up_and_logged_in_session_token;

    /// A store for `session_token`'s account priced in `base_currency`,
    /// made through store setup: its id.
    async fn create_connection_with_base_currency(
        router: &Router,
        state: &AppState,
        session_token: &str,
        base_currency: &str,
    ) -> String {
        crate::http::test_support::store_with_keys(
            router,
            state,
            session_token,
            "https://shop.example.com",
            base_currency,
        )
        .await
        .id
    }

    #[tokio::test]
    async fn pending_order_can_be_backgrounded_listed_reopened_and_cancelled() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let owner = signed_up_and_logged_in_session_token(
            &router,
            "pos-flow@example.com",
            "correct horse battery staple",
        )
        .await;
        let other = signed_up_and_logged_in_session_token(
            &router,
            "pos-flow-other@example.com",
            "correct horse battery staple",
        )
        .await;
        let id = create_connection_with_base_currency(&router, &state, &owner, "XMR").await;
        let base = format!("/stores/{id}/pos/orders");
        let create_body = serde_json::json!({"amount":"0.010000000000","merchant_order_id":"Mia coffee","request_key":"retry-key"}).to_string();
        let create = |token: &str, body: String| {
            Request::builder()
                .method("POST")
                .uri(&base)
                .header("content-type", "application/json")
                .header("cookie", format!("session={token}"))
                .body(Body::from(body))
                .unwrap()
        };
        let first = router
            .clone()
            .oneshot(create(&owner, create_body.clone()))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first_id = body_json(first).await["order_id"]
            .as_str()
            .unwrap()
            .to_string();
        let retry = router
            .clone()
            .oneshot(create(&owner, create_body))
            .await
            .unwrap();
        assert_eq!(body_json(retry).await["order_id"], first_id);
        let changed = router.clone().oneshot(create(&owner, serde_json::json!({"amount":"0.020000000000","merchant_order_id":"Mia coffee","request_key":"retry-key"}).to_string())).await.unwrap();
        assert_eq!(changed.status(), StatusCode::BAD_REQUEST);
        let order_url = format!("{base}/{first_id}");
        let other_list = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&base)
                    .header("cookie", format!("session={other}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(other_list.status(), StatusCode::NOT_FOUND);
        let background = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("{order_url}/background"))
                    .header("cookie", format!("session={owner}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(background.status(), StatusCode::NO_CONTENT);
        let list = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{base}?limit=1&offset=0"))
                    .header("cookie", format!("session={owner}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let list = body_json(list).await;
        assert_eq!(list["total"], 1);
        assert_eq!(list["orders"][0]["order_id"], first_id);
        assert_eq!(list["orders"][0]["backgrounded"], true);
        assert_eq!(list["orders"][0]["status"], "pending");
        // The id and address come shortened by the server too, so the POS
        // app shows them as the rest of the site does.
        let listed = &list["orders"][0];
        assert_eq!(
            listed["order_id_short"],
            crate::views::short_id_text(first_id.as_str())
        );
        assert!(listed["order_id_short"].as_str().unwrap().contains('…'));
        assert_eq!(
            listed["address_short"],
            crate::views::short_address_text(listed["address"].as_str().unwrap())
        );
        assert_eq!(
            listed["address_short"]
                .as_str()
                .unwrap()
                .matches('…')
                .count(),
            2
        );
        let search_list = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{base}?search=mia"))
                    .header("cookie", format!("session={owner}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(search_list).await["total"], 1);
        let other_detail = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&order_url)
                    .header("cookie", format!("session={other}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(other_detail.status(), StatusCode::NOT_FOUND);
        let cancel = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("{order_url}/cancel"))
                    .header("cookie", format!("session={owner}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancel.status(), StatusCode::NO_CONTENT);
        let detail = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&order_url)
                    .header("cookie", format!("session={owner}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let detail = body_json(detail).await;
        assert!(detail["cancelled_at"].as_i64().is_some());
        assert_eq!(detail["merchant_order_id"], "Mia coffee");
        let repeated = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("{order_url}/cancel"))
                    .header("cookie", format!("session={owner}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(repeated.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn production_pos_assets_are_served_with_browser_mime_types() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        for (path, mime, marker) in [
            (
                crate::assets::url("pos-app.js"),
                "text/javascript; charset=utf-8",
                "pos-root",
            ),
            (
                crate::assets::url("pos-app.css"),
                "text/css; charset=utf-8",
                ".pos-stack-card",
            ),
        ] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(&path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[axum::http::header::CONTENT_TYPE], mime);
            assert!(body_text(response).await.contains(marker));
        }
    }

    /// Each POS route is the owner's alone: another merchant gets a bare
    /// 404 (never a 403 that would confirm the store exists), as does an
    /// unknown store or order, and no session gets a 401.
    #[tokio::test]
    async fn every_pos_route_is_the_owners_alone() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let owner_token = signed_up_and_logged_in_session_token(
            &router,
            "pos-owner@example.com",
            "correct horse battery staple",
        )
        .await;
        let id = create_connection_with_base_currency(&router, &state, &owner_token, "XMR").await;
        let create = |id: &str, token: &str| {
            Request::builder()
                .method("POST")
                .uri(format!("/stores/{id}/pos/orders"))
                .header("content-type", "application/json")
                .header("cookie", format!("session={token}"))
                .body(Body::from(
                    serde_json::json!({ "amount": "1.0" }).to_string(),
                ))
                .unwrap()
        };
        let created = router
            .clone()
            .oneshot(create(&id, &owner_token))
            .await
            .unwrap();
        let order_id = body_json(created).await["order_id"]
            .as_str()
            .unwrap()
            .to_string();
        let other_token = signed_up_and_logged_in_session_token(
            &router,
            "pos-other@example.com",
            "correct horse battery staple",
        )
        .await;
        let get = |uri: String, token: Option<&str>| {
            let mut request = Request::builder().method("GET").uri(uri);
            if let Some(token) = token {
                request = request.header("cookie", format!("session={token}"));
            }
            request.body(Body::empty()).unwrap()
        };

        for (row, request, status) in [
            (
                "another's page",
                get(format!("/stores/{id}/pos"), Some(&other_token)),
                StatusCode::NOT_FOUND,
            ),
            (
                "another's new order",
                create(&id, &other_token),
                StatusCode::NOT_FOUND,
            ),
            (
                "another's order status",
                get(
                    format!("/stores/{id}/pos/orders/{order_id}/status"),
                    Some(&other_token),
                ),
                StatusCode::NOT_FOUND,
            ),
            (
                "an unknown store",
                get("/stores/nonexistent/pos".to_string(), Some(&owner_token)),
                StatusCode::NOT_FOUND,
            ),
            (
                "an unknown order",
                get(
                    format!("/stores/{id}/pos/orders/nonexistent/status"),
                    Some(&owner_token),
                ),
                StatusCode::NOT_FOUND,
            ),
            (
                "no session",
                get(format!("/stores/{id}/pos"), None),
                StatusCode::UNAUTHORIZED,
            ),
        ] {
            let response = router.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), status, "{row}");
        }
    }

    #[tokio::test]
    async fn creating_a_pos_order_uses_the_stores_own_base_currency_and_is_visible_on_the_dashboard(
    ) {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pos-create@example.com",
            "correct horse battery staple",
        )
        .await;
        let id = create_connection_with_base_currency(&router, &state, &session_token, "XMR").await;

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/stores/{id}/pos/orders"))
                    .header("content-type", "application/json")
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::from(
                        serde_json::json!({ "amount": "1.5" }).to_string(),
                    ))
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

        // An order created through the POS screen is a real
        // order, visible on the normal orders list like any other.
        let orders_list = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{id}/orders"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(orders_list.status(), StatusCode::OK);
        let html = body_text(orders_list).await;
        assert!(
            html.contains(&order_id),
            "expected the POS-created order to show up in the dashboard's own orders list"
        );

        // Created by the merchant's own session: recorded as trusted as the key.
        let metadata = state
            .db
            .lock()
            .get_order_currency_metadata(
                &engine_protocol::ids::ConnectionId::new(id.to_string()),
                &engine_protocol::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert!(metadata.created_with_key);
    }

    #[tokio::test]
    async fn creating_a_pos_order_with_a_note_records_it_as_the_real_merchant_order_id() {
        // The terminal's own quick note (typically a
        // customer's name) is the same `merchant_order_id` every other
        // order-creation surface in this crate already writes - proven here
        // by checking it shows up on the real order detail page, the same
        // place the machine API's own equivalent test checks.
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pos-note@example.com",
            "correct horse battery staple",
        )
        .await;
        let id = create_connection_with_base_currency(&router, &state, &session_token, "XMR").await;

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/stores/{id}/pos/orders"))
                    .header("content-type", "application/json")
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::from(
                        serde_json::json!({ "amount": "1.0", "merchant_order_id": "Jane Doe" })
                            .to_string(),
                    ))
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
                    .uri(format!("/stores/{id}/orders/{order_id}"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail_response.status(), StatusCode::OK);
        let html = body_text(detail_response).await;
        assert!(
            html.contains("Jane Doe"),
            "expected the real note shown as this order's merchant order id, got: {html}"
        );

        // Creating a pos order with a blank note records no merchant order id.
        {
            // A note field left empty (or whitespace-only - a merchant tapping
            // it and tapping away) must not record a literal empty-string
            // `merchant_order_id` - same trim-to-`None` convention
            // `http::orders::create_order`'s own form field already follows.
            let (state, _engine) = test_state_with_real_engine().await;
            let router = build_router(state.clone());

            let session_token = signed_up_and_logged_in_session_token(
                &router,
                "pos-blank-note@example.com",
                "correct horse battery staple",
            )
            .await;
            let id =
                create_connection_with_base_currency(&router, &state, &session_token, "XMR").await;

            let response = router
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/stores/{id}/pos/orders"))
                        .header("content-type", "application/json")
                        .header("cookie", format!("session={session_token}"))
                        .body(Body::from(
                            serde_json::json!({ "amount": "1.0", "merchant_order_id": "   " })
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = body_json(response).await;
            assert!(
                body["merchant_order_id"].is_null(),
                "a whitespace-only note must not become a stored merchant_order_id, got: {body}"
            );
        }
    }

    #[tokio::test]
    async fn creating_a_pos_order_rejects_a_malformed_amount() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pos-bad-amount@example.com",
            "correct horse battery staple",
        )
        .await;
        let id = create_connection_with_base_currency(&router, &state, &session_token, "XMR").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/stores/{id}/pos/orders"))
                    .header("content-type", "application/json")
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::from(
                        serde_json::json!({ "amount": "not-a-number" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // Creating a pos order rejects an empty amount.
        {
            let (state, _engine) = test_state_with_real_engine().await;
            let router = build_router(state.clone());

            let session_token = signed_up_and_logged_in_session_token(
                &router,
                "pos-empty-amount@example.com",
                "correct horse battery staple",
            )
            .await;
            let id =
                create_connection_with_base_currency(&router, &state, &session_token, "XMR").await;

            let response = router
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/stores/{id}/pos/orders"))
                        .header("content-type", "application/json")
                        .header("cookie", format!("session={session_token}"))
                        .body(Body::from(serde_json::json!({ "amount": "" }).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]
    async fn the_status_endpoint_reports_pending_with_no_error_for_a_fresh_order() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pos-status@example.com",
            "correct horse battery staple",
        )
        .await;
        let id = create_connection_with_base_currency(&router, &state, &session_token, "XMR").await;

        let create = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/stores/{id}/pos/orders"))
                    .header("content-type", "application/json")
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::from(
                        serde_json::json!({ "amount": "2.0" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let order_id = body_json(create).await["order_id"]
            .as_str()
            .unwrap()
            .to_string();

        let status = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{id}/pos/orders/{order_id}/status"))
                    .header("cookie", format!("session={session_token}"))
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
        assert!(
            body["error"].is_null(),
            "a plain pending order must carry no error, got: {body}"
        );
    }

    #[tokio::test]
    async fn the_events_stream_reports_every_watched_order_and_pushes_changes() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pos-events@example.com",
            "correct horse battery staple",
        )
        .await;
        let id = create_connection_with_base_currency(&router, &state, &session_token, "XMR").await;
        let mut order_ids = Vec::new();
        for _ in 0..2 {
            let create = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/stores/{id}/pos/orders"))
                        .header("content-type", "application/json")
                        .header("cookie", format!("session={session_token}"))
                        .body(Body::from(
                            serde_json::json!({ "amount": "2.0" }).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            order_ids.push(
                body_json(create).await["order_id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }

        let events_request = |query: String, token: Option<&str>| {
            let mut builder =
                Request::builder().uri(format!("/stores/{id}/pos/events?orders={query}"));
            if let Some(token) = token {
                builder = builder.header("cookie", format!("session={token}"));
            }
            builder.body(Body::empty()).unwrap()
        };
        let unauthenticated = router
            .clone()
            .oneshot(events_request(order_ids[0].clone(), None))
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        let empty = router
            .clone()
            .oneshot(events_request(String::new(), Some(&session_token)))
            .await
            .unwrap();
        assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

        // An id that isn't this store's is simply never reported.
        let query = format!("{},{},pay_not_ours", order_ids[0], order_ids[1]);
        let response = router
            .clone()
            .oneshot(events_request(query, Some(&session_token)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        let (mut pending, mut parser) = (Vec::new(), crate::live::SseTestParser::default());

        let mut seen = Vec::new();
        for _ in 0..2 {
            let (event, data) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser)
                .await
                .unwrap();
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

        assert!(engine
            .store()
            .lock()
            .mark_double_spend_detected(
                &engine_protocol::ids::OrderId::new(order_ids[1].to_string()),
                crate::now_unix()
            )
            .unwrap());
        let (event, data) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser)
            .await
            .unwrap();
        assert_eq!(event, "status");
        let data: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(data["order_id"], order_ids[1].as_str());
        assert!(
            data["error"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("double-spend"),
            "got: {data}"
        );
    }

    async fn create_pos_orders(
        router: &Router,
        session_token: &str,
        id: &str,
        count: usize,
    ) -> Vec<String> {
        let mut order_ids = Vec::with_capacity(count);
        for _ in 0..count {
            let create = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/stores/{id}/pos/orders"))
                        .header("content-type", "application/json")
                        .header("cookie", format!("session={session_token}"))
                        .body(Body::from(
                            serde_json::json!({ "amount": "2.0" }).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(create.status(), StatusCode::OK);
            order_ids.push(
                body_json(create).await["order_id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
        order_ids
    }

    /// Regression: the engine rate-limits each store (per token, as
    /// configured by `server.rate_limit_per_token_per_min`), and the
    /// terminal used to spend one engine request per
    /// order on its list and again per order on its live stream, so a store
    /// with a few dozen open orders ran out after a reload or two and every
    /// screen showed "internal error". Both now cost one read per batch.
    #[tokio::test]
    async fn the_order_list_and_events_stream_read_many_orders_in_one_engine_request() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pos-batch@example.com",
            "correct horse battery staple",
        )
        .await;
        let id = create_connection_with_base_currency(&router, &state, &session_token, "XMR").await;
        let order_ids = create_pos_orders(&router, &session_token, &id, 30).await;

        let before = engine.tenant_request_count();
        let list = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/stores/{id}/pos/orders?limit=40"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        assert_eq!(
            body_json(list).await["orders"].as_array().unwrap().len(),
            30
        );
        assert_eq!(
            engine.tenant_request_count() - before,
            1,
            "listing 30 orders reads them in one engine request"
        );

        let before = engine.tenant_request_count();
        let events = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/stores/{id}/pos/events?orders={}",
                        order_ids.join(",")
                    ))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(events.status(), StatusCode::OK);
        let mut body = events.into_body();
        let (mut pending, mut parser) = (Vec::new(), crate::live::SseTestParser::default());
        let mut seen = std::collections::HashSet::new();
        while seen.len() < order_ids.len() {
            let (event, data) = crate::live::next_sse_event(&mut body, &mut pending, &mut parser)
                .await
                .unwrap();
            assert_eq!(event, "status");
            let data: serde_json::Value = serde_json::from_str(&data).unwrap();
            seen.insert(data["order_id"].as_str().unwrap().to_string());
        }
        // One read for all 30 snapshots, and at most the store's one shared
        // upstream event stream plus its first resync read beside it.
        let spent = engine.tenant_request_count() - before;
        assert!(
            spent <= 3,
            "watching 30 orders cost {spent} engine requests"
        );
    }

    /// A merchant signed in to a store on a real engine, for the tests of
    /// what wakes a POS order's live stream.
    struct LiveStore {
        state: AppState,
        engine: std::sync::Arc<engine_test_support::TestEngineHandle>,
        router: Router,
        session_token: String,
        id: String,
    }

    /// One open `pos/events` stream.
    struct LiveEvents {
        body: Body,
        pending: Vec<(String, String)>,
        parser: crate::live::SseTestParser,
    }

    impl LiveEvents {
        /// The next status event; one that never comes (a change that
        /// didn't wake the stream) fails on `next_sse_event`'s timeout.
        async fn next(&mut self) -> serde_json::Value {
            let (event, data) =
                crate::live::next_sse_event(&mut self.body, &mut self.pending, &mut self.parser)
                    .await
                    .unwrap();
            assert_eq!(event, "status");
            serde_json::from_str(&data).unwrap()
        }
    }

    impl LiveStore {
        async fn new(email: &str) -> Self {
            let (state, engine) = test_state_with_real_engine().await;
            let router = build_router(state.clone());
            let session_token = signed_up_and_logged_in_session_token(
                &router,
                email,
                "correct horse battery staple",
            )
            .await;
            let id =
                create_connection_with_base_currency(&router, &state, &session_token, "XMR").await;
            LiveStore {
                state,
                engine: std::sync::Arc::new(engine),
                router,
                session_token,
                id,
            }
        }

        async fn orders(&self, count: usize) -> Vec<String> {
            create_pos_orders(&self.router, &self.session_token, &self.id, count).await
        }

        async fn send(&self, method: &str, path: String) -> axum::response::Response {
            self.router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(format!("/stores/{}/{path}", self.id))
                        .header("cookie", format!("session={}", self.session_token))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        }

        async fn open(&self, order: &str) -> LiveEvents {
            let response = self.send("GET", format!("pos/events?orders={order}")).await;
            assert_eq!(response.status(), StatusCode::OK);
            LiveEvents {
                body: response.into_body(),
                pending: Vec::new(),
                parser: crate::live::SseTestParser::default(),
            }
        }

        /// The terminal's `background` or `cancel` button.
        async fn act(&self, order: &str, action: &str) -> axum::response::Response {
            self.send("POST", format!("pos/orders/{order}/{action}"))
                .await
        }

        fn pos_row(&self, order: &str) -> crate::db::PosOrderRow {
            self.state
                .db
                .lock()
                .get_pos_order(
                    &crate::db::ConnectionId::new(self.id.as_str()),
                    &crate::db::OrderId::new(order),
                )
                .unwrap()
                .unwrap()
        }

        /// A stream on `order`, opened once the store's engine stream is
        /// up. That stream wakes every watched order when it connects,
        /// which would also carry a change a test's action failed to
        /// announce; an engine change to `order` (its refund address, set
        /// from the checkout page) shows on the stream only once it is up,
        /// so its connect has passed before the order under test is
        /// watched. Keep the stream returned open throughout: the store's
        /// engine stream closes with its last watcher.
        async fn connect_engine_stream(&self, order: &str) -> LiveEvents {
            let mut stream = self.open(order).await;
            assert!(stream.next().await["refund_address"].is_null());
            let refund_address = "86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC";
            let set = self
                .router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/checkout/{order}/refund-address"))
                        .header("accept", "application/json")
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from(format!("refund_address={refund_address}")))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(set.status(), StatusCode::OK);
            assert_eq!(stream.next().await["refund_address"], refund_address);
            stream
        }

        async fn status(&self, order: &str) -> serde_json::Value {
            let response = self.send("GET", format!("pos/orders/{order}/status")).await;
            assert_eq!(response.status(), StatusCode::OK);
            body_json(response).await
        }
    }

    /// Backgrounding (the POS's own record, which the engine knows nothing
    /// of) and cancelling (the engine's) both reach the order's live
    /// stream at once, so a watching terminal never has to reload.
    #[tokio::test]
    async fn backgrounding_and_cancelling_an_order_are_sent_on_its_live_stream() {
        let store = LiveStore::new("pos-stream-state@example.com").await;
        let mut orders = store.orders(2).await;
        let (order, other) = (orders.remove(0), orders.remove(0));
        let _engine_stream = store.connect_engine_stream(&other).await;

        let mut events = store.open(&order).await;
        let first = events.next().await;
        assert_eq!(first["backgrounded"], false);
        assert!(first["cancelled_at"].is_null());

        for (action, check) in [("background", "backgrounded"), ("cancel", "cancelled_at")] {
            assert_eq!(
                store.act(&order, action).await.status(),
                StatusCode::NO_CONTENT
            );
            let update = events.next().await;
            assert_eq!(update["backgrounded"], true, "after {action}");
            assert_eq!(
                update["cancelled_at"].is_i64(),
                check == "cancelled_at",
                "after {action}"
            );
        }
        let status = store.status(&order).await;
        assert_eq!(status["status"], "cancelled");
        assert_eq!(status["is_terminal"], true);
    }

    /// A POS cancellation is the engine's, so every surface that shows the
    /// order says so: the checkout live (its stream) and on a reload with
    /// no JavaScript (no address to copy, no more refreshing), the order's
    /// dashboard page and the orders list, the API's order and its event
    /// stream.
    #[tokio::test]
    async fn a_pos_cancellation_shows_on_the_checkout_dashboard_and_api() {
        let store = LiveStore::new("pos-cancel-everywhere@example.com").await;
        let order = store.orders(1).await.remove(0);
        let get = |uri: String, cookie: bool| {
            let mut request = Request::builder().uri(uri);
            if cookie {
                request = request.header("cookie", format!("session={}", store.session_token));
            }
            store
                .router
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
        };
        let text = |response: axum::response::Response| async move {
            assert_eq!(response.status(), StatusCode::OK);
            body_text(response).await
        };
        let pk = store
            .state
            .db
            .lock()
            .get_store_connection_by_id(&crate::db::ConnectionId::new(store.id.as_str()))
            .unwrap()
            .unwrap()
            .tenant_public_key;
        let api = |uri: String| {
            store
                .router
                .clone()
                .oneshot(crate::http::test_support::api_request(
                    "GET",
                    &uri,
                    Some(&pk),
                    None,
                ))
        };

        let page = text(get(format!("/checkout/{order}"), false).await.unwrap()).await;
        assert!(
            page.contains(r#"id="checkout-refresh""#),
            "refreshes while open"
        );
        assert!(!page.contains("data-void"), "the address can be copied");
        let mut checkout = get(format!("/checkout/{order}/events?fragments=true"), false)
            .await
            .unwrap()
            .into_body();
        let mut api_events = api(format!("/api/v1/orders/{order}/events"))
            .await
            .unwrap()
            .into_body();
        let (mut pending, mut parser) = (Vec::new(), crate::live::SseTestParser::default());
        let (mut api_pending, mut api_parser) = (Vec::new(), crate::live::SseTestParser::default());
        let first = next_named(&mut checkout, &mut pending, &mut parser, "status").await;
        assert_eq!(first["status"], "pending");
        let first = next_named(&mut api_events, &mut api_pending, &mut api_parser, "order").await;
        assert_eq!(first["status"], "pending");

        assert_eq!(
            store.act(&order, "cancel").await.status(),
            StatusCode::NO_CONTENT
        );

        // Live, without a reload.
        let mut fragments = String::new();
        let live = loop {
            let (event, data) =
                crate::live::next_sse_event(&mut checkout, &mut pending, &mut parser)
                    .await
                    .unwrap();
            if event == "fragment" {
                fragments.push_str(&data);
            } else if event == "status" {
                break serde_json::from_str::<serde_json::Value>(&data).unwrap();
            }
        };
        assert_eq!(live["status"], "cancelled");
        assert_eq!(live["is_terminal"], true);
        assert!(fragments.contains("This order was cancelled"), "told live");
        assert!(
            fragments.contains("data-void"),
            "the address struck through live"
        );
        let api_live =
            next_named(&mut api_events, &mut api_pending, &mut api_parser, "order").await;
        assert_eq!(api_live["status"], "cancelled");
        assert_eq!(api_live["is_final"], true);

        // Reloaded, with no JavaScript: told, nothing to pay, no refresh.
        let page = text(get(format!("/checkout/{order}"), false).await.unwrap()).await;
        assert!(
            page.contains("This order was cancelled"),
            "told on a reload"
        );
        assert!(page.contains("qr-wrap is-spent"), "the code faded");
        assert!(page.contains("data-void"), "the address struck through");
        assert!(
            !page.contains(r#"id="checkout-refresh""#),
            "no more refreshing"
        );

        let detail = text(
            get(format!("/stores/{}/orders/{order}", store.id), true)
                .await
                .unwrap(),
        )
        .await;
        assert!(detail.contains("state-cancelled"), "the badge");
        assert!(detail.contains("Cancelled at"), "the fact");
        assert!(
            detail.contains("This order was cancelled at"),
            "the activity"
        );
        let list = text(
            get(format!("/stores/{}/orders", store.id), true)
                .await
                .unwrap(),
        )
        .await;
        assert!(list.contains("state-cancelled"), "the orders list");

        let body = body_json(api(format!("/api/v1/orders/{order}")).await.unwrap()).await;
        assert_eq!(body["status"], "cancelled");
        assert_eq!(body["is_final"], true);
        assert_eq!(
            body["cancelled_at"],
            live_cancelled_at(&store, &order).await
        );
        assert_eq!(body["expires_at"], body["cancelled_at"]);
    }

    /// The data of the next `name` event on a stream, skipping others.
    async fn next_named(
        body: &mut Body,
        pending: &mut Vec<(String, String)>,
        parser: &mut crate::live::SseTestParser,
        name: &str,
    ) -> serde_json::Value {
        loop {
            let (event, data) = crate::live::next_sse_event(body, pending, parser)
                .await
                .unwrap();
            if event == name {
                return serde_json::from_str(&data).unwrap();
            }
        }
    }

    async fn live_cancelled_at(store: &LiveStore, order: &str) -> serde_json::Value {
        store.status(order).await["cancelled_at"].clone()
    }

    /// A payment the engine recorded before the cancel refuses it: the
    /// terminal is told why, and the order stays as it is, open and not
    /// backgrounded.
    #[tokio::test]
    async fn a_payment_recorded_before_the_cancel_refuses_it() {
        let store = LiveStore::new("pos-cancel-refused@example.com").await;
        let order = store.orders(1).await.remove(0);
        store
            .engine
            .record_order_payment(&order, shared::xmr_amount::Piconero(1), Some(1))
            .unwrap();

        let response = store.act(&order, "cancel").await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(response)
            .await
            .contains("This order has payment activity and cannot be cancelled"));
        assert!(!store.pos_row(&order).backgrounded);
        let status = store.status(&order).await;
        assert_eq!(status["status"], "partial");
        assert!(status["cancelled_at"].is_null());
    }

    /// Money sent to a cancelled order anyway is never lost: a little
    /// leaves it cancelled, showing what arrived; the rest pays it, and it
    /// shows paid, still saying when it was cancelled, for the merchant to
    /// review.
    #[tokio::test]
    async fn money_sent_after_a_cancel_shows_on_the_cancelled_order() {
        let store = LiveStore::new("pos-paid-after-cancel@example.com").await;
        let order = store.orders(1).await.remove(0);
        assert_eq!(
            store.act(&order, "cancel").await.status(),
            StatusCode::NO_CONTENT
        );
        // Cancelling again changes nothing.
        assert_eq!(
            store.act(&order, "cancel").await.status(),
            StatusCode::NO_CONTENT
        );
        store
            .engine
            .record_order_payment(&order, shared::xmr_amount::Piconero(1), Some(1))
            .unwrap();
        let status = store.status(&order).await;
        assert_eq!(status["status"], "cancelled");
        assert_eq!(status["received_xmr"], "0.000000000001");
        let cancelled_at = status["cancelled_at"].as_i64().expect("cancelled");

        store.engine.mark_order_paid(&order).unwrap();
        let status = store.status(&order).await;
        assert_eq!(status["status"], "overpaid");
        assert_eq!(status["cancelled_at"], cancelled_at);
    }

    /// Without its POS row a backgrounded order would be shown in the
    /// foreground: when the rows can't be read, the stream sends nothing
    /// until the next change and the status endpoint fails.
    #[tokio::test]
    async fn a_failed_read_of_the_pos_rows_sends_nothing_and_fails_the_status() {
        let store = LiveStore::new("pos-read-fails@example.com").await;
        let mut orders = store.orders(2).await;
        let (order, other) = (orders.remove(0), orders.remove(0));
        let _engine_stream = store.connect_engine_stream(&other).await;
        // Every read of the POS rows fails, and says when it happened.
        let (refused_sender, mut refused) = tokio::sync::mpsc::unbounded_channel();
        {
            let db = store.state.db.lock();
            let conn = db.conn_for_test();
            conn.create_scalar_function(
                "refuse_pos_order_read",
                0,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                move |_| -> rusqlite::Result<rusqlite::types::Null> {
                    let _ = refused_sender.send(());
                    Err(rusqlite::Error::UserFunctionError(
                        "refused for the test".into(),
                    ))
                },
            )
            .unwrap();
            conn.execute_batch(
                "CREATE TEMP VIEW pos_orders AS
                 SELECT * FROM main.pos_orders WHERE refuse_pos_order_read()",
            )
            .unwrap();
        }

        assert_eq!(
            store
                .send("GET", format!("pos/orders/{order}/status"))
                .await
                .status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        refused.recv().await.unwrap();

        let mut events = store.open(&order).await;
        let next = events.next();
        tokio::pin!(next);
        tokio::select! {
            sent = &mut next => panic!("sent without the order's POS row: {sent}"),
            _ = refused.recv() => {}
        }
        store
            .state
            .db
            .lock()
            .conn_for_test()
            .execute_batch("DROP VIEW temp.pos_orders")
            .unwrap();
        assert_eq!(
            store.act(&order, "background").await.status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(next.await["backgrounded"], true);
    }

    /// The terminal's Active tab and background stack come from
    /// `?state=active`: every POS order still open, however many newer
    /// orders have finished since; never a paid one, one the POS cancelled,
    /// or an order that did not come from the POS.
    #[tokio::test]
    async fn the_active_list_has_every_open_pos_order_however_many_newer_ones_finished() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pos-active@example.com",
            "correct horse battery staple",
        )
        .await;
        let id = create_connection_with_base_currency(&router, &state, &session_token, "XMR").await;
        let waiting = create_pos_orders(&router, &session_token, &id, 1)
            .await
            .remove(0);
        for paid in create_pos_orders(&router, &session_token, &id, 45).await {
            engine.mark_order_paid(&paid).unwrap();
        }
        let cancelled = create_pos_orders(&router, &session_token, &id, 1)
            .await
            .remove(0);
        let cancel = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/stores/{id}/pos/orders/{cancelled}/cancel"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancel.status(), StatusCode::NO_CONTENT);
        // An order from somewhere else (the store's website), still open.
        let pk = state
            .db
            .lock()
            .get_store_connection_by_id(&engine_protocol::ids::ConnectionId::new(id.to_string()))
            .unwrap()
            .unwrap()
            .tenant_public_key;
        let web = router
            .clone()
            .oneshot(crate::http::test_support::api_request(
                "POST",
                "/api/v1/orders",
                Some(&pk),
                Some(serde_json::json!({ "amount": "1.0", "currency": "XMR" })),
            ))
            .await
            .unwrap();
        assert_eq!(web.status(), StatusCode::CREATED);

        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/stores/{id}/pos/orders?state=active"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let ids: Vec<String> = body_json(response).await["orders"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["order_id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, vec![waiting]);
    }

    #[tokio::test]
    async fn a_rate_limited_engine_is_a_retryable_503_not_an_internal_error() {
        let (state, _engine) = test_state_with_engine(
            engine_test_support::TestEngineConfig::new().with_rate_limit(20),
        )
        .await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pos-limited@example.com",
            "correct horse battery staple",
        )
        .await;
        let id = create_connection_with_base_currency(&router, &state, &session_token, "XMR").await;
        let order_id = create_pos_orders(&router, &session_token, &id, 1)
            .await
            .remove(0);

        for _ in 0..40 {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/stores/{id}/pos/orders/{order_id}"))
                        .header("cookie", format!("session={session_token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            if response.status() == StatusCode::OK {
                continue;
            }
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            let error = body_json(response).await["detail"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(error.contains("busy"), "got: {error}");
            return;
        }
        panic!("the engine's rate limit was never reached");
    }

    #[tokio::test]
    async fn a_pos_order_created_against_a_fiat_base_currency_is_rejected_with_no_priced_provider_configured(
    ) {
        // This test instance's `exchange_rate` is XMR-only
        // (`test_exchange_rate_provider`) - a store whose `base_currency` is
        // a fiat currency simply can't be priced on it, the same
        // "unsupported currency" outcome the machine API's own
        // tests already exercise for `POST /api/v1/orders`. Proves the POS
        // endpoint surfaces that as a clear `400`, not a `500`.
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "pos-fiat@example.com",
            "correct horse battery staple",
        )
        .await;
        let id = create_connection_with_base_currency(&router, &state, &session_token, "USD").await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/stores/{id}/pos/orders"))
                    .header("content-type", "application/json")
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::from(
                        serde_json::json!({ "amount": "10.00" }).to_string(),
                    ))
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
            order_id: engine_protocol::ids::OrderId::new("pay_test".to_string()),
            merchant_order_id: None,
            description: None,
            address: "addr".to_string(),
            xmr_amount_piconero: 1_000_000_000_000,
            amount_received_piconero: 0,
            status: status.parse().expect("a known status"),
            confirmations: 0,
            double_spend_detected_at: None,
            cancelled_at: None,
            refund_address: None,
            created_at: 0,
            expires_at: 1000,
            updated_at: 0,
            first_scanned_height: None,
            last_scanned_height: None,
            currently_scanning: false,
        }
    }

    /// What the till says about each state: nothing while the payment is
    /// on its way or paid; underpaid, overpaid and expired are flagged; and
    /// a double-spend first, whatever the status (the `if` in
    /// `derive_payment_error` returns before the `match`).
    #[test]
    fn each_state_is_flagged_or_not_and_a_double_spend_always_is() {
        for (status, flag) in [
            ("pending", None),
            ("unconfirmed", None),
            ("confirming", None),
            ("paid", None),
            ("partial", Some("underpaid")),
            ("overpaid", Some("overpaid")),
            ("expired", Some("expired")),
        ] {
            let error = derive_payment_error(&order_with_status(status));
            match flag {
                None => assert_eq!(error, None, "{status}"),
                Some(word) => assert!(
                    error
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(word),
                    "{status}: {error:?}"
                ),
            }
        }
        for status in ["unconfirmed", "confirming", "paid", "partial"] {
            let mut order = order_with_status(status);
            order.double_spend_detected_at = Some(123);
            let error =
                derive_payment_error(&order).expect("a double-spend must always be flagged");
            assert!(
                error.to_lowercase().contains("double-spend"),
                "got: {error}"
            );
        }
    }
}
