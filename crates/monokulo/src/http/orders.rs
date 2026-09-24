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

use std::collections::HashMap;

use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::crypto;
use crate::db::{StoreConnectionRow, UserRow};
use crate::engine_client::{EngineClientError, PaymentLookupView};
use crate::templates::{display_or_dash, display_scan_range, display_timestamp, display_timestamp_or_dash};
use crate::views;
use crate::views::orders::{OrderDetailData, OrderDetailViewModel, OrderRowViewModel, OrdersViewModel, PaymentRowViewModel};

use super::dashboard::redirect_302;
use super::{AppState, AuthedUser};

/// Looks up `store_connections` row `id` and confirms it belongs to `user`.
/// `Ok(None)` covers *both* "no such row" and "exists but belongs to someone
/// else" - callers must map that uniformly to `404` (see this module's own
/// doc comment), never distinguishing the two. `Err(())` is a real database
/// failure - the caller's problem, not the requester's.
pub(super) fn load_owned_connection(state: &AppState, user: &UserRow, id: &str) -> Result<Option<StoreConnectionRow>, ()> {
    let row = state.db.lock().unwrap().get_store_connection_by_id(id).map_err(|_| ())?;
    Ok(row.filter(|row| row.user_id == user.id))
}

/// Decrypts the connection's stored `sk_...` token under `state`'s
/// encryption key. A failure here means a row this service itself wrote and
/// encrypted can't be decrypted with its own key - shouldn't happen, but
/// handled as a plain internal error rather than unwrapped/panicked on (see
/// the task's own note on this).
pub(super) fn decrypt_sk(state: &AppState, row: &StoreConnectionRow) -> Result<String, ()> {
    crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted).map_err(|_| ())
}

/// Shared by `orders_list` and `lookup_payment` below - both need the same
/// "real orders, real fiat metadata" view model, just with a different
/// `lookup_*` overlay (nothing, for a plain page view; a real result, after a
/// lookup form submission).
async fn build_orders_view_model(
    state: &AppState,
    row: &StoreConnectionRow,
    sk: &str,
) -> Result<OrdersViewModel, ()> {
    let orders = state.engine_client.list_orders(sk).await.map_err(|_| ())?;
    // The engine has no concept of fiat any more (`docs/fx_refactor.md` Phase
    // 3) - fiat display comes entirely from monokulo's own local
    // `order_currency_metadata`, keyed by payment_id, fetched once for the whole
    // list rather than per-row.
    let fiat_metadata = state.db.lock().unwrap().list_order_currency_metadata_for_connection(&row.id).unwrap_or_default();

    Ok(OrdersViewModel {
        connection_id: row.id.clone(),
        orders: orders
            .into_iter()
            .map(|o| {
                let (amount, currency) = match fiat_metadata.get(&o.payment_id) {
                    Some(m) => (m.amount.clone(), m.currency.clone()),
                    None => ("—".to_string(), "".to_string()),
                };
                OrderRowViewModel { payment_id: o.payment_id, status: o.status, amount, currency, created_at: o.created_at }
            })
            .collect(),
        lookup_txid_value: String::new(),
        lookup_message: None,
        lookup_found_payment_id: None,
    })
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
    let view_model = match build_orders_view_model(&state, &row, &sk).await {
        Ok(vm) => vm,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let chrome = views::PageChrome::from_user(Some(&user), format!("/dashboard/connections/{id}/orders"));
    views::orders::list_page(&chrome, &view_model).into_response()
}

#[derive(Deserialize)]
pub struct LookupPaymentForm {
    txid: String,
}

/// `POST /dashboard/connections/{id}/orders/lookup` - `docs/txid_lookup_and_
/// scan_chunking_wbs.md` Part B.3, the direct replacement for the old
/// manual rescan feature. Re-renders the same orders list with the result
/// shown inline (a plain message, plus a link to the matched order if any) -
/// recompute and redisplay, never a redirect that would lose the result.
pub async fn lookup_payment(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(form): Form<LookupPaymentForm>,
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

    let txid = form.txid.trim().to_string();
    let (message, found_payment_id) = match state.engine_client.lookup_payment(&sk, &txid).await {
        Ok(PaymentLookupView::NotFoundOnChain) => {
            ("No transaction with that ID was found on the network.".to_string(), None)
        }
        Ok(PaymentLookupView::NoMatchingOrder) => {
            ("That transaction exists, but doesn't pay any of this store's orders.".to_string(), None)
        }
        Ok(PaymentLookupView::Matched { order_ids }) => {
            ("Match found and recorded.".to_string(), order_ids.into_iter().next())
        }
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            (format!("Couldn't look that up: {message}"), None)
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let mut view_model = match build_orders_view_model(&state, &row, &sk).await {
        Ok(vm) => vm,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    view_model.lookup_txid_value = txid;
    view_model.lookup_message = Some(message);
    view_model.lookup_found_payment_id = found_payment_id;

    let chrome = views::PageChrome::from_user(Some(&user), format!("/dashboard/connections/{id}/orders"));
    views::orders::list_page(&chrome, &view_model).into_response()
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
    headers: HeaderMap,
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
    let chrome = views::PageChrome::from_user(Some(&user), format!("/dashboard/connections/{id}/orders/{payment_id}"));
    // A real, absolute, copy-pasteable URL - not just the path - since the
    // whole point is something a merchant can paste into an email or chat
    // to someone who isn't already looking at this dashboard. This
    // instance has no configured "external base URL" of its own yet, so
    // this is built from the *incoming* request's own `Host` header (what
    // the merchant's own browser just used to reach this page - reliably
    // the right host for a link they're about to copy from it) plus
    // `X-Forwarded-Proto` if a reverse proxy set it (the common way a
    // self-hosted instance behind real TLS termination communicates that
    // inward), falling back to plain `http` for local/dev use.
    let host = headers.get(axum::http::header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let scheme = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()).unwrap_or("http");
    let payment_link = format!("{scheme}://{host}/pay/{}/orders/{}/share", row.tenant_public_key, payment_id);

    match state.engine_client.get_order_detail(&sk, &payment_id).await {
        Ok(detail) => {
            // The engine has no concept of fiat any more (`docs/fx_refactor.md`
            // Phase 3) - fiat display comes entirely from monokulo's own
            // local `order_currency_metadata`, absent for any order that predates
            // this record (falls back to a dash rather than failing the page).
            let metadata = state.db.lock().unwrap().get_order_currency_metadata(&row.id, &payment_id).ok().flatten();
            let (amount, currency) = match &metadata {
                Some(m) => (m.amount.clone(), m.currency.clone()),
                None => ("—".to_string(), "".to_string()),
            };
            let (rate_display, rate_provider) = match &metadata {
                Some(m) => (
                    format!("{} XMR per 1 {}", shared::exchange_rate::format_piconero_as_xmr(m.piconero_per_unit), m.currency),
                    m.provider.clone(),
                ),
                None => ("—".to_string(), "—".to_string()),
            };
            // Same "no snapshot, show a dash" fallback as `rate_display`
            // above - `metadata`'s own 3 threshold-snapshot fields are all
            // `Option` for the exact same reasons (migration 0016's own doc
            // comment).
            let confirmations_required_display = metadata
                .as_ref()
                .and_then(|m| m.confirmations_required_applied)
                .map(|c| c.to_string())
                .unwrap_or_else(|| "—".to_string());
            let base_currency_display =
                metadata.as_ref().and_then(|m| m.store_base_currency.clone()).unwrap_or_else(|| "—".to_string());
            let base_currency_rate_display = match metadata.as_ref().and_then(|m| m.store_base_currency.clone()) {
                Some(base_currency) => match metadata.as_ref().and_then(|m| m.base_currency_piconero_per_unit) {
                    Some(rate) => {
                        format!("{} XMR per 1 {base_currency}", shared::exchange_rate::format_piconero_as_xmr(rate))
                    }
                    None => "same as order currency".to_string(),
                },
                None => "—".to_string(),
            };
            let view_model = OrderDetailViewModel {
                connection_id: id.to_string(),
                order: Some(OrderDetailData {
                    payment_id: detail.order.payment_id,
                    merchant_order_id: detail.order.merchant_order_id,
                    address: detail.order.address,
                    currency,
                    amount,
                    rate_display,
                    rate_provider,
                    xmr_amount_piconero: detail.order.xmr_amount_piconero,
                    amount_received_piconero: detail.order.amount_received_piconero,
                    status: detail.order.status,
                    confirmations: detail.order.confirmations,
                    confirmations_required_display,
                    base_currency_display,
                    base_currency_rate_display,
                    double_spend_detected_at: detail.order.double_spend_detected_at,
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
                    payment_link,
                    scan_range_display: display_scan_range(
                        detail.order.first_scanned_height,
                        detail.order.last_scanned_height,
                        detail.order.currently_scanning,
                    ),
                }),
                meta_refresh_secs: 15,
            };
            views::orders::detail_page(&chrome, &view_model).into_response()
        }
        Err(EngineClientError::EngineError { status, .. }) if status == reqwest::StatusCode::NOT_FOUND => {
            let view_model = OrderDetailViewModel { connection_id: id.to_string(), order: None, meta_refresh_secs: 15 };
            (StatusCode::NOT_FOUND, views::orders::detail_page(&chrome, &view_model)).into_response()
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
    render_webhooks_page(&state, &id, &sk, &user, None, None).await
}

/// Shared by `webhooks_list`/`webhooks_create`/`webhooks_delete` - every one
/// of them ends by showing the same page (a fresh webhook list, optionally
/// with an error or a just-created secret), so this is the one place that
/// actually fetches the list and renders it.
async fn render_webhooks_page(
    state: &AppState,
    connection_id: &str,
    sk: &str,
    user: &UserRow,
    error: Option<String>,
    created_webhook_signing_secret: Option<String>,
) -> Response {
    let webhooks = match state.engine_client.list_webhooks(sk).await {
        Ok(webhooks) => webhooks,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let chrome = views::PageChrome::from_user(Some(user), format!("/dashboard/connections/{connection_id}/webhooks"));
    let view_model = views::webhooks::WebhooksViewModel {
        connection_id: connection_id.to_string(),
        webhooks: webhooks
            .into_iter()
            .map(|w| views::webhooks::WebhookRowViewModel {
                webhook_id: w.webhook_id,
                url: w.url,
                enabled: w.enabled,
                created_at: w.created_at,
            })
            .collect(),
        error,
        created_webhook_signing_secret,
    };
    views::webhooks::page(&chrome, &view_model).into_response()
}

#[derive(Deserialize)]
pub struct CreateWebhookForm {
    pub url: String,
    /// One `Header-Name: value` pair per line - the engine's own
    /// `extra_headers` (`CreateWebhookRequest`, `src/http/admin.rs` at the
    /// repo root) wants a flat JSON object of string values, and this is
    /// the plainest way to collect an arbitrary number of them from an
    /// HTML form without JS-driven "add another row" UI. Blank lines are
    /// ignored; every other line must contain `:` or the whole submission
    /// is rejected with a clear error (see [`parse_extra_headers`]) -
    /// rejecting outright rather than silently dropping a malformed line,
    /// since a header the merchant *thinks* they configured but didn't
    /// would be a much worse failure mode than an upfront error.
    #[serde(default)]
    pub extra_headers: String,
}

/// Parses [`CreateWebhookForm::extra_headers`]'s `Header-Name: value` lines
/// into the flat string map `EngineClient::create_webhook` wants. `Err`
/// names the exact offending line so the re-rendered form's error is
/// actionable, not just "invalid input somewhere."
fn parse_extra_headers(text: &str) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut headers = std::collections::BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(format!("Custom headers must be one \"Header-Name: value\" pair per line - could not parse: {line:?}"));
        };
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() {
            return Err(format!("Custom headers must be one \"Header-Name: value\" pair per line - could not parse: {line:?}"));
        }
        headers.insert(name.to_string(), value.to_string());
    }
    Ok(headers)
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
        return render_webhooks_page(&state, &id, &sk, &user, Some("Enter a webhook URL.".to_string()), None).await;
    }
    let extra_headers = match parse_extra_headers(&form.extra_headers) {
        Ok(headers) => headers,
        Err(message) => return render_webhooks_page(&state, &id, &sk, &user, Some(message), None).await,
    };

    match state.engine_client.create_webhook(&sk, url, &extra_headers).await {
        Ok((_webhook_id, signing_secret)) => render_webhooks_page(&state, &id, &sk, &user, None, Some(signing_secret)).await,
        // The engine's own validation (a malformed URL, a non-http(s) scheme -
        // `src/http/admin.rs::create_webhook` at the repo root) - the
        // caller's mistake, surfaced verbatim, same convention
        // `connections::create_connection_for_user` already applies to the
        // engine's tenant-creation `400`s.
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            render_webhooks_page(&state, &id, &sk, &user, Some(message), None).await
        }
        Err(_) => render_webhooks_page(&state, &id, &sk, &user, Some("Something went wrong. Please try again.".to_string()), None).await,
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
        Err(_) => render_webhooks_page(&state, &id, &sk, &user, Some("Could not delete that webhook. Please try again.".to_string()), None).await,
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
            let chrome = views::PageChrome::from_user(Some(&user), format!("/dashboard/connections/{id}"));
            let data = views::store_detail::StoreDetailViewModel { store: None };
            return (StatusCode::NOT_FOUND, views::store_detail::page(&chrome, &data)).into_response();
        }
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render_store_detail_page(&state, row, &user, None, None).await
}

/// Shared by `store_detail`, `create_order`, and `update_confirmations_required`
/// - all three end by showing the same page (a fresh store overview,
/// optionally with a create-order or settings error), same pattern as
/// `orders.rs`'s own `render_webhooks_page`. Takes an already
/// ownership-checked row rather than re-checking it, since every caller has
/// already done that.
async fn render_store_detail_page(
    state: &AppState,
    row: StoreConnectionRow,
    user: &UserRow,
    order_creation_error: Option<String>,
    settings_error: Option<String>,
) -> Response {
    let chrome = views::PageChrome::from_user(Some(user), format!("/dashboard/connections/{}", row.id));
    let sk = match decrypt_sk(state, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let tenant_result = state.engine_client.get_tenant(&sk).await;
    let (health, health_label) = health_of_tenant_lookup(&tenant_result);
    let confirmations_required = tenant_result.as_ref().map(|t| t.confirmations_required).unwrap_or(0);
    let zero_conf_max_xmr = tenant_result
        .as_ref()
        .ok()
        .and_then(|t| t.zero_conf_max_piconero)
        .filter(|&piconero| piconero > 0)
        .map(shared::xmr_amount::format_piconero_as_xmr)
        .unwrap_or_default();

    // A store whose engine is currently unreachable still gets a real page -
    // just with no order data available, rather than a hard error. The
    // health tag above is what actually communicates the problem.
    let recent_orders = match state.engine_client.list_orders(&sk).await {
        Ok(mut orders) => {
            orders.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            let fiat_metadata =
                state.db.lock().unwrap().list_order_currency_metadata_for_connection(&row.id).unwrap_or_default();
            orders
                .into_iter()
                .take(10)
                .map(|o| {
                    let (amount, currency) = match fiat_metadata.get(&o.payment_id) {
                        Some(m) => (m.amount.clone(), m.currency.clone()),
                        None => ("—".to_string(), "".to_string()),
                    };
                    views::orders::OrderRowViewModel { payment_id: o.payment_id, status: o.status, amount, currency, created_at: o.created_at }
                })
                .collect()
        }
        Err(_) => Vec::new(),
    };

    let is_woocommerce = row.platform == "woocommerce";
    let fx_provider_options = state
        .exchange_rate
        .available_providers()
        .into_iter()
        .map(|name| views::store_detail::FxProviderOption { selected: name == row.fx_provider, name: name.to_string() })
        .collect();
    // A live Coingecko failure here degrades to "XMR only" rather than
    // failing this whole page - same "show something real-ish rather than
    // fail outright" approach `health`/`recent_orders` above already take
    // for their own engine-reachability failures.
    let order_currency_options =
        state.exchange_rate.supported_currencies_for(&row).await.unwrap_or_else(|_| vec!["XMR".to_string()]);
    let order_currency_is_locked_to_xmr = order_currency_options.len() == 1 && order_currency_options[0] == "XMR";
    let (base_currency_options, confirmation_thresholds) = {
        let db = state.db.lock().unwrap();
        let options = crate::currencies::currency_options(&db, &row.base_currency).unwrap_or_default();
        let thresholds = db
            .list_confirmation_thresholds(&row.id)
            .unwrap_or_default()
            .into_iter()
            .map(|t| views::store_detail::ConfirmationThresholdView { id: t.id, unit_amount: t.unit_amount, confirmations_required: t.confirmations_required })
            .collect::<Vec<_>>();
        (options, thresholds)
    };
    let confirmation_thresholds_at_max = confirmation_thresholds.len() >= 5;
    let view_model = views::store_detail::StoreDetailViewModel {
        store: Some(views::store_detail::StoreDetailData {
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
            order_currency_options,
            order_currency_is_locked_to_xmr,
            confirmations_required,
            fx_provider: row.fx_provider,
            fx_provider_options,
            base_currency: row.base_currency,
            base_currency_options,
            confirmation_thresholds,
            confirmation_thresholds_at_max,
            zero_conf_max_xmr,
            settings_error,
        }),
    };
    views::store_detail::page(&chrome, &view_model).into_response()
}

#[derive(Deserialize)]
pub struct CreateOrderForm {
    pub amount: String,
    pub currency: String,
    /// A blank field submits as `Some("")` (a plain HTML form always sends
    /// the input's value, even empty) - trimmed and turned into a real
    /// `None` before reaching `EngineClient::create_order`, same "empty
    /// means unset" handling `amount`/`currency` above already get.
    #[serde(default)]
    pub merchant_order_id: String,
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

    let amount = form.amount.trim();
    let currency = form.currency.trim();
    if amount.is_empty() || currency.is_empty() {
        return render_store_detail_page(&state, row, &user, Some("Enter an amount and a currency.".to_string()), None).await;
    }

    // Selection-time validation first, entirely independent of whether any
    // provider can actually price it - see `crate::currencies`'s own doc
    // comment and `http::pay::create_order`'s matching check for why
    // "unknown currency" and "unsupported currency" are kept as distinct
    // messages rather than collapsed into one.
    let currency_known = crate::currencies::is_known_currency(&state.db.lock().unwrap(), currency);
    match currency_known {
        Ok(true) => {}
        Ok(false) => return render_store_detail_page(&state, row, &user, Some(format!("unknown currency: {currency}")), None).await,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }

    // The engine has no concept of currency any more (`docs/fx_refactor.md`
    // Phase 3) - monokulo's own exchange rate does the same computation
    // `http::pay::create_order` does for a real storefront call. `"XMR"`
    // always uses the trivial identity rate regardless of this store's
    // chosen `fx_provider`; every other currency goes through it.
    let (piconero_per_unit, provider) = match state.exchange_rate.piconero_per_unit_for(&row, currency).await {
        Ok(Some(result)) => result,
        Ok(None) => {
            return render_store_detail_page(&state, row, &user, Some(format!("unsupported currency: {currency}")), None).await
        }
        Err(crate::exchange_rate_config::ExchangeRateLookupError::ProviderNotConfigured(_)) => {
            // Not a real failure - this store's provider (or no provider at
            // all) simply can't price this currency on this instance, same
            // user-facing meaning as `Ok(None)` above.
            return render_store_detail_page(&state, row, &user, Some(format!("unsupported currency: {currency}")), None).await
        }
        Err(e) => {
            eprintln!("exchange rate lookup failed for connection {} (currency {currency:?}): {e}", row.id);
            return render_store_detail_page(&state, row, &user, Some("Something went wrong looking up the exchange rate. Please try again.".to_string()), None)
                .await;
        }
    };
    let xmr_amount_piconero = match shared::exchange_rate::compute_order_amount(currency, amount, piconero_per_unit) {
        Ok(amount) => amount,
        Err(e) => return render_store_detail_page(&state, row, &user, Some(e.to_string()), None).await,
    };
    let merchant_order_id = {
        let trimmed = form.merchant_order_id.trim();
        if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
    };

    let sk = match decrypt_sk(&state, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let resolution = match crate::confirmation_thresholds::resolve_for_order(&state, &row, &sk, currency, piconero_per_unit, xmr_amount_piconero).await {
        Ok(resolution) => resolution,
        Err(message) => return render_store_detail_page(&state, row, &user, Some(message), None).await,
    };

    match state
        .engine_client
        .create_order(&row.tenant_public_key, xmr_amount_piconero, merchant_order_id, Some(resolution.confirmations_required))
        .await
    {
        Ok(order) => {
            if let Err(e) = state.db.lock().unwrap().create_order_currency_metadata(
                &row.id,
                &order.payment_id,
                currency,
                amount,
                piconero_per_unit,
                provider,
                crate::now_unix(),
                &resolution.base_currency,
                resolution.base_currency_piconero_per_unit,
                resolution.confirmations_required,
            ) {
                eprintln!(
                    "failed to record local fiat metadata for order {} on connection {}: {e} - the real order \
                     still exists on the engine and this response is still correct, but its fiat display on \
                     monokulo's own dashboard will be missing",
                    order.payment_id, row.id
                );
            }
            redirect_302(&format!("/dashboard/connections/{id}/orders/{}", order.payment_id))
        }
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            render_store_detail_page(&state, row, &user, Some(message), None).await
        }
        Err(_) => {
            render_store_detail_page(&state, row, &user, Some("Something went wrong. Please try again.".to_string()), None).await
        }
    }
}

#[derive(Deserialize)]
pub struct UpdateConfirmationsForm {
    pub confirmations_required: String,
}

/// `POST /dashboard/connections/{id}/settings/confirmations` - updates the
/// tenant's `confirmations_required` via the engine's own `PATCH
/// /api/v1/admin/tenant` (`EngineClient::set_confirmations_required`).
/// `String`, not `u64`, on the form field: an unparseable value (empty,
/// non-numeric) is a validation error surfaced the same way as every other
/// one here, not a `400` from axum's own form extractor before this
/// handler ever runs - a merchant who fat-fingers this field should see the
/// same page with the same clear message, not a generic framework error
/// page.
pub async fn update_confirmations_required(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(form): Form<UpdateConfirmationsForm>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let confirmations_required: u64 = match form.confirmations_required.trim().parse() {
        Ok(n) => n,
        Err(_) => {
            return render_store_detail_page(&state, row, &user, None, Some("Enter a whole number of confirmations.".to_string()))
                .await;
        }
    };

    let sk = match decrypt_sk(&state, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    match state.engine_client.set_confirmations_required(&sk, confirmations_required).await {
        Ok(_) => redirect_302(&format!("/dashboard/connections/{id}")),
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            render_store_detail_page(&state, row, &user, None, Some(message)).await
        }
        Err(_) => {
            render_store_detail_page(&state, row, &user, None, Some("Something went wrong. Please try again.".to_string())).await
        }
    }
}

#[derive(Deserialize)]
pub struct UpdateFxProviderForm {
    pub fx_provider: String,
}

/// `POST /dashboard/connections/{id}/settings/fx-provider` - a per-store
/// choice of exchange-rate provider (a real follow-up to
/// `docs/fx_refactor.md`: "the FX provider should be configurable on a
/// per-store basis"). Unlike `update_confirmations_required`, this never
/// calls the engine at all - `fx_provider` is entirely monokulo's own
/// concept (`db::StoreConnectionRow::fx_provider`), so a plain local update
/// plus a redirect is the whole handler. Validated against
/// `ExchangeRateProviders::available_providers` (this instance's own real
/// configuration) rather than accepted verbatim - a merchant selecting
/// `"coingecko"` on an instance with no Coingecko currencies configured
/// would otherwise silently create orders that fail at order-creation time
/// instead of being told clearly, right here, that the choice doesn't work.
pub async fn update_fx_provider(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(form): Form<UpdateFxProviderForm>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    if !state.exchange_rate.is_available(&form.fx_provider) {
        return render_store_detail_page(
            &state,
            row,
            &user,
            None,
            Some(format!("{:?} is not an available exchange rate provider on this instance.", form.fx_provider)),
        )
        .await;
    }

    // Bound to a local first, not matched on directly: a `MutexGuard`
    // temporary created in a `match` scrutinee is kept alive for every arm
    // of that match (a real Rust footgun, not an oversight) - held across
    // the `Err` arm's own `.await` below, it would make this handler's
    // future `!Send` and fail to compile as an axum route at all.
    let update_result = state.db.lock().unwrap().update_store_connection_fx_provider(&row.id, &form.fx_provider);
    match update_result {
        Ok(()) => redirect_302(&format!("/dashboard/connections/{id}")),
        Err(_) => {
            render_store_detail_page(&state, row, &user, None, Some("Something went wrong. Please try again.".to_string())).await
        }
    }
}

#[derive(Deserialize)]
pub struct UpdateBaseCurrencyForm {
    pub base_currency: String,
}

/// `POST /dashboard/connections/{id}/settings/base-currency` - selection-time
/// validation only (`crate::currencies::resolve_currency`), the same
/// two-stage split every currency selection in this crate follows - see that
/// module's own doc comment. Never checks whether any exchange-rate provider
/// actually supports the chosen currency; that's a separate, later concern
/// (order creation, threshold resolution), not this handler's job.
///
/// Changing the currency deletes every custom confirmation threshold for
/// this store (`Db::update_store_connection_base_currency`'s own doc
/// comment) - an old amount in a since-abandoned currency means nothing any
/// more.
pub async fn update_base_currency(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(form): Form<UpdateBaseCurrencyForm>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // Bound to a local first, not matched on directly - see
    // `update_fx_provider`'s own doc comment on exactly this footgun
    // (a `MutexGuard` temporary in a `match` scrutinee stays alive across
    // every arm, including one that `.await`s, which would make this
    // handler's future `!Send`).
    let resolved = crate::currencies::resolve_currency(&state.db.lock().unwrap(), &form.base_currency);
    let base_currency = match resolved {
        Ok(Some(code)) => code,
        Ok(None) => {
            return render_store_detail_page(
                &state,
                row,
                &user,
                None,
                Some(format!("{:?} is not a known currency.", form.base_currency)),
            )
            .await;
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let update_result = state.db.lock().unwrap().update_store_connection_base_currency(&row.id, &base_currency);
    match update_result {
        Ok(()) => redirect_302(&format!("/dashboard/connections/{id}")),
        Err(_) => {
            render_store_detail_page(&state, row, &user, None, Some("Something went wrong. Please try again.".to_string())).await
        }
    }
}

#[derive(Deserialize)]
pub struct UpdateZeroConfForm {
    /// An XMR decimal amount (`shared::xmr_amount::parse_xmr_to_piconero`),
    /// not a piconero integer - the same "human enters a real-world unit"
    /// convention every other amount field on this page already follows.
    /// Blank means "accept no 0-conf payments at all", not a validation
    /// error - the field has no `required` attribute in the template, since
    /// "off" is this setting's own valid, common value.
    #[serde(default)]
    pub zero_conf_max_xmr: String,
}

/// `POST /dashboard/connections/{id}/settings/zero-conf` - sets (or clears,
/// via an empty submission) this store's `zero_conf_max_piconero`: orders at
/// or under this XMR amount can read as `paid` off a mempool-only,
/// zero-confirmation transaction (`EngineClient::set_zero_conf_max_piconero`'s
/// own doc comment covers why `0` - what a blank field parses to here - is a
/// real, safe "disabled" value rather than something this handler needs to
/// special-case). This is a real double-spend exposure a merchant is opting
/// into for low-value/in-person orders, not a free lunch - the field help
/// text on this page says so; this handler only validates the amount itself.
pub async fn update_zero_conf_max_piconero(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(form): Form<UpdateZeroConfForm>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let trimmed = form.zero_conf_max_xmr.trim();
    let zero_conf_max_piconero = if trimmed.is_empty() {
        0
    } else {
        match shared::xmr_amount::parse_xmr_to_piconero(trimmed) {
            Ok(piconero) => piconero,
            Err(e) => {
                return render_store_detail_page(&state, row, &user, None, Some(format!("Enter a valid XMR amount: {e}"))).await;
            }
        }
    };

    let sk = match decrypt_sk(&state, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    match state.engine_client.set_zero_conf_max_piconero(&sk, zero_conf_max_piconero).await {
        Ok(_) => redirect_302(&format!("/dashboard/connections/{id}")),
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            render_store_detail_page(&state, row, &user, None, Some(message)).await
        }
        Err(_) => {
            render_store_detail_page(&state, row, &user, None, Some("Something went wrong. Please try again.".to_string())).await
        }
    }
}

#[derive(Deserialize)]
pub struct CreateConfirmationThresholdForm {
    pub unit_amount: String,
    pub confirmations_required: String,
}

/// `POST /dashboard/connections/{id}/settings/confirmation-thresholds` - adds
/// one custom, amount-tiered confirmation threshold, one at a time (the same
/// "add form, real POST, redirect back" shape webhooks already use). Three
/// validated properties, in order: `confirmations_required` is a whole
/// number; `unit_amount` is a real, non-negative decimal amount; this store
/// doesn't already have 5 custom thresholds. A duplicate amount is rejected
/// too, backstopped by `confirmation_thresholds`'s own `UNIQUE` constraint
/// (`Db::create_confirmation_threshold`'s own doc comment) - the message
/// here is just the friendlier surfaced form of that same rejection.
pub async fn create_confirmation_threshold(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(form): Form<CreateConfirmationThresholdForm>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let confirmations_required: u64 = match form.confirmations_required.trim().parse() {
        Ok(n) => n,
        Err(_) => {
            return render_store_detail_page(&state, row, &user, None, Some("Enter a whole number of confirmations.".to_string()))
                .await;
        }
    };

    let unit_amount = form.unit_amount.trim();
    match unit_amount.parse::<f64>() {
        Ok(n) if n.is_finite() && n >= 0.0 => {}
        _ => {
            return render_store_detail_page(&state, row, &user, None, Some("Enter a non-negative amount.".to_string())).await;
        }
    }

    let count = state.db.lock().unwrap().count_confirmation_thresholds(&row.id).unwrap_or(0);
    if count >= 5 {
        return render_store_detail_page(
            &state,
            row,
            &user,
            None,
            Some("You can define at most 5 custom thresholds. Delete one to add another.".to_string()),
        )
        .await;
    }

    let threshold_id = uuid::Uuid::new_v4().to_string();
    let create_result =
        state.db.lock().unwrap().create_confirmation_threshold(&threshold_id, &row.id, unit_amount, confirmations_required, crate::now_unix());
    match create_result {
        Ok(()) => redirect_302(&format!("/dashboard/connections/{id}")),
        Err(e) if e.is_unique_violation() => {
            render_store_detail_page(&state, row, &user, None, Some(format!("A threshold for {unit_amount} already exists.")))
                .await
        }
        Err(_) => {
            render_store_detail_page(&state, row, &user, None, Some("Something went wrong. Please try again.".to_string())).await
        }
    }
}

/// `POST /dashboard/connections/{id}/settings/confirmation-thresholds/{threshold_id}/delete` -
/// the default/fallback threshold isn't one of these rows at all (it's
/// `tenants.confirmations_required`, edited via `update_confirmations_required`
/// instead), so there is no way to reach this handler for it - "cannot be
/// deleted" is true by construction, not an extra check here.
pub async fn delete_confirmation_threshold(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, threshold_id)): Path<(String, String)>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    state.db.lock().unwrap().delete_confirmation_threshold(&row.id, &threshold_id).ok();
    redirect_302(&format!("/dashboard/connections/{id}"))
}

/// `POST /dashboard/connections/{id}/settings/confirmation-thresholds/save` -
/// the dashboard's condensed "Confirmation Thresholds" table posts here as
/// one form with one Save button, rather than the default-update,
/// add-threshold and per-row-delete forms each posting to their own route
/// (those three still exist unchanged above, for API/e2e compatibility -
/// `crates/mock-woocommerce/tests/e2e_stagenet_confirmation_threshold.rs`
/// posts to `create_confirmation_threshold` directly). Fields arrive as a
/// loose `HashMap` rather than a typed form because the delete checkboxes'
/// field names are dynamic, one per existing threshold id
/// (`delete_{threshold.id}`), which a fixed `#[derive(Deserialize)]` struct
/// can't express. Order of operations: update the default, then delete
/// checked rows, then (space permitting) add the new row - so deleting a
/// row and immediately reusing its amount for the new row in the same Save
/// works.
pub async fn save_confirmation_thresholds(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(raw): Form<HashMap<String, String>>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let confirmations_required: u64 =
        match raw.get("confirmations_required").map(|s| s.trim()).unwrap_or("").parse() {
            Ok(n) => n,
            Err(_) => {
                return render_store_detail_page(&state, row, &user, None, Some("Enter a whole number of confirmations.".to_string()))
                    .await;
            }
        };

    let sk = match decrypt_sk(&state, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if let Err(e) = state.engine_client.set_confirmations_required(&sk, confirmations_required).await {
        let message = match e {
            EngineClientError::EngineError { status, message } if status == reqwest::StatusCode::BAD_REQUEST => message,
            _ => "Something went wrong. Please try again.".to_string(),
        };
        return render_store_detail_page(&state, row, &user, None, Some(message)).await;
    }

    let existing = state.db.lock().unwrap().list_confirmation_thresholds(&row.id).unwrap_or_default();
    for threshold in &existing {
        if raw.contains_key(&format!("delete_{}", threshold.id)) {
            state.db.lock().unwrap().delete_confirmation_threshold(&row.id, &threshold.id).ok();
        }
    }

    let new_unit_amount = raw.get("new_unit_amount").map(|s| s.trim()).unwrap_or("");
    let new_confirmations_required = raw.get("new_confirmations_required").map(|s| s.trim()).unwrap_or("");
    if !new_unit_amount.is_empty() || !new_confirmations_required.is_empty() {
        let new_confirmations_required: u64 = match new_confirmations_required.parse() {
            Ok(n) => n,
            Err(_) => {
                return render_store_detail_page(
                    &state,
                    row,
                    &user,
                    None,
                    Some("Enter a whole number of confirmations for the new threshold.".to_string()),
                )
                .await;
            }
        };
        match new_unit_amount.parse::<f64>() {
            Ok(n) if n.is_finite() && n >= 0.0 => {}
            _ => {
                return render_store_detail_page(
                    &state,
                    row,
                    &user,
                    None,
                    Some("Enter a non-negative amount for the new threshold.".to_string()),
                )
                .await;
            }
        }

        let count = state.db.lock().unwrap().count_confirmation_thresholds(&row.id).unwrap_or(0);
        if count >= 5 {
            return render_store_detail_page(
                &state,
                row,
                &user,
                None,
                Some("You can define at most 5 custom thresholds. Delete one to add another.".to_string()),
            )
            .await;
        }

        let threshold_id = uuid::Uuid::new_v4().to_string();
        let create_result = state.db.lock().unwrap().create_confirmation_threshold(
            &threshold_id,
            &row.id,
            new_unit_amount,
            new_confirmations_required,
            crate::now_unix(),
        );
        if let Err(e) = create_result {
            let message = if e.is_unique_violation() {
                format!("A threshold for {new_unit_amount} already exists.")
            } else {
                "Something went wrong. Please try again.".to_string()
            };
            return render_store_detail_page(&state, row, &user, None, Some(message)).await;
        }
    }

    redirect_302(&format!("/dashboard/connections/{id}"))
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
    use crate::views::orders::{OrderDetailData, OrderDetailViewModel};

    use super::super::{AppState, build_router};
    use super::parse_extra_headers;

    /// Same fixed-scalar construction `engine_client.rs`'s and
    /// `connections.rs`'s own tests use.
    const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
    const TEST_ENCRYPTION_KEY: [u8; 32] = [7u8; 32];

    /// `"XMR"` deliberately, not a fiat currency: these tests are about the
    /// dashboard's own order-creation/display plumbing, not about exercising
    /// a real (mocked) fiat provider - and an XMR-denominated order needs no
    /// provider configured at all (`XmrIdentityProvider`), so `TEST_STATE`
    /// stays free of any network dependency. `pay.rs`'s own tests are the
    /// ones that genuinely exercise a fiat quote.
    const TEST_CURRENCY: &str = "XMR";
    const TEST_RATE_PICONERO_PER_UNIT: u64 = 1_000_000_000_000;

    /// An `AppState.exchange_rate` with no fiat provider configured at all -
    /// every test in this module prices its orders in `TEST_CURRENCY`
    /// (`"XMR"`), which needs none.
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
        };
        (state, engine)
    }

    /// Same as [`test_state_with_real_engine`], but with a real (if inert)
    /// daemon wired into the engine's own `AppState::daemons` - needed by a
    /// caller that drives `admin::lookup_payment` through a genuine HTTP
    /// round trip, which reads that map unconditionally.
    async fn test_state_with_real_engine_and_admin_lookup_daemon() -> (AppState, scanner_test_support::TestEngineHandle)
    {
        let engine = scanner_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .with_admin_lookup_daemon()
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
            "base_currency": "XMR",
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
        // The engine has no concept of fiat any more (`docs/fx_refactor.md`
        // Phase 3) - 10.00 at `TEST_RATE_PICONERO_PER_UNIT` (1e12 piconero/USD).
        let response = reqwest::Client::new()
            .post(format!("http://{engine_addr}/api/v1/t/{public_key}/orders"))
            .json(&serde_json::json!({ "xmr_amount_piconero": 10 * TEST_RATE_PICONERO_PER_UNIT }))
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
                    .header("host", "test.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains(&payment_id), "expected the order's payment_id in its detail page, got: {html}");
        // Seeded directly against the engine's own public API, bypassing
        // monokulo's own `http::pay` endpoint - no local fiat metadata
        // was ever recorded for it, so the page must show a dash rather than
        // a fabricated amount (see `checkout.rs`'s own doc comment on the
        // same fallback).
        assert!(html.contains('—'), "expected a dash placeholder for the missing fiat quote, got: {html}");
        // Timestamps deliberately stay compact (raw Unix seconds), not a
        // human-readable date - a user-requested reversion of an earlier
        // attempt at this page.
        assert!(html.contains("Created at"), "expected the created-at row present, got: {html}");
        // `merchant_order_id` was never set on this seeded order - must show
        // a muted placeholder, not a blank cell.
        assert!(html.contains("muted"), "expected a muted placeholder for the unset merchant order id, got: {html}");
        assert!(html.contains(r#"<meta http-equiv="refresh""#), "expected an auto-refresh meta tag, got: {html}");
        assert!(html.contains("refreshes automatically"), "expected the refresh interval noted on the page, got: {html}");
        // The real point of this follow-up: a real, absolute, shareable
        // payment link for this exact order, built from the request's own
        // Host header (`test.example` here, set by `oneshot`'s default) -
        // not a placeholder or a bare relative path.
        assert!(
            html.contains(&format!("http://test.example/pay/{public_key}/orders/{payment_id}/share")),
            "expected a real absolute payment link, got: {html}"
        );
        // The real point of this follow-up: the link now lives as a share
        // icon in the title banner, not its own row in the details table -
        // and the title itself carries the real order id right alongside it.
        assert!(html.contains(r#"<h1 class="order-title">"#), "expected the title banner to carry the share button, got: {html}");
        assert!(
            html.contains(&format!(r#"<span>Order {payment_id}</span>"#)),
            "expected the order id inside the title banner, got: {html}"
        );
        assert!(
            html.contains(r#"id="share-payment-link""#) && html.contains("aria-label=\"Share payment link\""),
            "expected a real, labeled share button, got: {html}"
        );
        assert!(!html.contains("<th>Payment link</th>"), "the payment link must no longer be its own table row, got: {html}");
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

    #[test]
    fn parse_extra_headers_reads_one_header_name_value_pair_per_line() {
        let parsed = parse_extra_headers("X-Api-Key: secret123\nAnother-Header:  spaced value \n\n").unwrap();
        assert_eq!(parsed.get("X-Api-Key").map(String::as_str), Some("secret123"));
        assert_eq!(parsed.get("Another-Header").map(String::as_str), Some("spaced value"));
        assert_eq!(parsed.len(), 2, "blank lines must not produce a phantom entry");
    }

    #[test]
    fn parse_extra_headers_on_empty_input_returns_an_empty_map_not_an_error() {
        assert!(parse_extra_headers("").unwrap().is_empty());
        assert!(parse_extra_headers("   \n  \n").unwrap().is_empty());
    }

    #[test]
    fn parse_extra_headers_rejects_a_line_with_no_colon() {
        let err = parse_extra_headers("X-Api-Key: fine\nnot-a-valid-line").unwrap_err();
        assert!(err.contains("not-a-valid-line"), "expected the real offending line named in the error, got: {err}");
    }

    #[test]
    fn parse_extra_headers_rejects_an_empty_header_name() {
        let err = parse_extra_headers(": value-with-no-name").unwrap_err();
        assert!(err.contains(": value-with-no-name"), "expected the real offending line named in the error, got: {err}");
    }

    #[tokio::test]
    async fn creating_a_webhook_with_custom_headers_is_accepted_by_the_real_engine() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "webhook-headers@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/webhooks"),
                &session_token,
                &[
                    ("url", "https://merchant.example/moneropay-webhook"),
                    ("extra_headers", "X-Api-Key: secret123\nX-Another: value2"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "the engine must accept a real webhook with custom headers");
        let html = body_text(response).await;
        assert!(html.contains("Webhook created"), "expected the webhook actually created, got: {html}");
    }

    #[tokio::test]
    async fn creating_a_webhook_with_a_malformed_header_line_shows_a_clear_error_and_registers_nothing() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "webhook-bad-headers@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/webhooks"),
                &session_token,
                &[("url", "https://merchant.example/moneropay-webhook"), ("extra_headers", "not-a-valid-line")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("not-a-valid-line"), "expected the real offending line named in the error, got: {html}");
        assert!(!html.contains("Webhook created"), "a malformed headers submission must not register anything, got: {html}");

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
        assert!(
            !list_html.contains("merchant.example"),
            "the rejected webhook must not have been registered, got: {list_html}"
        );
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
                &[("amount", "10.00"), ("currency", TEST_CURRENCY), ("merchant_order_id", "order-5678")],
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
        // A real, previously-missing capability: `EngineClient::create_order`
        // used to silently drop `merchant_order_id` no matter what the
        // dashboard's own "create an order" form submitted - every order's
        // own field always showed as unset regardless of caller intent.
        assert!(html.contains("order-5678"), "expected the real merchant_order_id shown, not a dash, got: {html}");
        // The real point of this test: the exact rate used and which
        // provider quoted it are both recorded and shown, not just the
        // resulting amount - including for an XMR-denominated order, which
        // still records a real rate (the trivial 1:1 identity) and a real
        // provider name ("xmr"), not a blank/special-cased display.
        assert!(
            html.contains("1.000000000000 XMR per 1 XMR"),
            "expected the real exchange rate used (the identity rate) on the page, got: {html}"
        );
        assert!(html.contains("xmr"), "expected the real rate provider (\"xmr\", from the identity provider) on the page, got: {html}");
    }

    #[tokio::test]
    async fn creating_an_order_with_an_unknown_currency_shows_a_clear_error() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "order-create-bad-currency@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/orders/new"),
                &session_token,
                &[("amount", "10.00"), ("currency", "NOTREAL")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a validation error re-renders the page, it doesn't redirect");
        let html = body_text(response).await;
        assert!(html.contains("unknown currency"), "expected a clear unknown-currency error surfaced, got: {html}");
        assert!(html.contains("<form"), "the create-order form must still be present, got: {html}");
    }

    /// The other half of the same two-stage split - `USD` is a real, known
    /// currency; this test's own `test_state_with_real_engine` just has no
    /// rate provider enabled at all. Must surface as a distinct
    /// "unsupported", not "unknown", currency error.
    #[tokio::test]
    async fn creating_an_order_with_a_known_but_provider_unsupported_currency_gets_a_distinct_error() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "order-create-unsupported-currency@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/orders/new"),
                &session_token,
                &[("amount", "10.00"), ("currency", "USD")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a validation error re-renders the page, it doesn't redirect");
        let html = body_text(response).await;
        assert!(html.contains("unsupported currency"), "expected a clear unsupported-currency error surfaced, got: {html}");
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
                &[("amount", "10.00"), ("currency", TEST_CURRENCY)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn updating_the_confirmation_threshold_redirects_and_the_new_value_shows_on_the_store_page() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "confirmations-update@example.com", "correct horse battery staple")
                .await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/confirmations"),
                &session_token,
                &[("confirmations_required", "3")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND, "expected a redirect back to the store page");
        assert_eq!(
            response.headers().get("location").unwrap(),
            &format!("/dashboard/connections/{connection_id}"),
        );

        let store_page = router
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
        let html = body_text(store_page).await;
        assert!(html.contains(r#"value="3""#), "expected the real, updated confirmation threshold shown, got: {html}");
    }

    #[tokio::test]
    async fn updating_the_confirmation_threshold_to_zero_shows_the_engines_real_validation_error() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "confirmations-zero@example.com", "correct horse battery staple")
                .await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/confirmations"),
                &session_token,
                &[("confirmations_required", "0")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a validation error re-renders the page, it doesn't redirect");
        let html = body_text(response).await;
        assert!(
            html.to_lowercase().contains("0 would treat an unconfirmed transaction as final")
                || html.contains("class=\"error\""),
            "expected the engine's real validation error surfaced, got: {html}"
        );
    }

    #[tokio::test]
    async fn a_new_store_defaults_to_coingecko_and_offers_no_options_when_none_are_enabled() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "fx-provider-default@example.com", "correct horse battery staple")
                .await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

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
        let html = body_text(response).await;
        // Every new store is created with `fx_provider = "coingecko"` (the
        // only real provider left - see `Db::create_store_connection`), shown
        // as this store's current choice even though this test's instance
        // (`test_exchange_rate_provider()`, `xmr_only()`) never actually
        // enabled it - a store's own setting and what an instance currently
        // offers are two different things.
        assert!(html.contains("Exchange rate provider"), "expected the settings section present, got: {html}");
        // The dropdown itself must offer zero real `<option>`s - nothing is
        // enabled on this instance, and there is no "fixed" to fall back to
        // any more.
        assert!(!html.contains(r#"<option value="coingecko""#), "expected no coingecko <option> since this instance never enabled it, got: {html}");
        assert!(!html.contains(r#"<option value="fixed""#), "the removed \"fixed\" provider must never appear as a real option, got: {html}");
        // The real point of this follow-up: with no fiat provider available
        // at all, the "create an order" currency field must be a plain
        // readonly "XMR" field, not a one-option `<select>` (a dropdown with
        // nothing to actually choose between is misleading busywork).
        assert!(
            html.contains(r#"<input type="text" id="currency" name="currency" value="XMR" readonly>"#),
            "expected a readonly XMR currency field, got: {html}"
        );
        assert!(!html.contains(r#"<select id="currency""#), "expected no currency dropdown when only XMR is available, got: {html}");
    }

    /// A store with a real Coingecko-backed provider enabled must offer a
    /// real `<select>` for the "create an order" currency field, listing
    /// every currency this instance currently supports - not the readonly
    /// XMR-only field the no-provider case above shows.
    #[tokio::test]
    async fn a_store_with_coingecko_enabled_gets_a_real_currency_dropdown() {
        async fn supported_currencies() -> axum::response::Response {
            use axum::response::IntoResponse;
            ([("content-type", "application/json")], r#"["usd","eur"]"#).into_response()
        }
        let mock_coingecko = axum::Router::new()
            .route("/api/v3/simple/supported_vs_currencies", axum::routing::get(supported_currencies));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mock_coingecko_addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, mock_coingecko).await.unwrap();
        });

        let (mut state, _engine) = test_state_with_real_engine().await;
        state.exchange_rate = std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::coingecko_only(
            format!("http://{mock_coingecko_addr}"),
        ));
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "fx-provider-dropdown@example.com", "correct horse battery staple")
                .await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

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
        let html = body_text(response).await;
        assert!(html.contains(r#"<select id="currency" name="currency">"#), "expected a real currency dropdown, got: {html}");
        assert!(!html.contains(r#"id="currency" name="currency" value="XMR" readonly"#), "must not be readonly once a provider is enabled, got: {html}");
        assert!(html.contains(r#"<option value="XMR">XMR</option>"#), "expected XMR always offered, got: {html}");
        assert!(html.contains(r#"<option value="USD">USD</option>"#), "expected the live-discovered USD option, got: {html}");
        assert!(html.contains(r#"<option value="EUR">EUR</option>"#), "expected the live-discovered EUR option, got: {html}");
    }

    #[tokio::test]
    async fn updating_the_fx_provider_to_an_unconfigured_one_is_a_clear_error_not_silently_accepted() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "fx-provider-invalid@example.com", "correct horse battery staple")
                .await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/fx-provider"),
                &session_token,
                &[("fx_provider", "coingecko")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a rejected provider re-renders the page, it doesn't redirect");
        let html = body_text(response).await;
        assert!(
            html.contains("not an available exchange rate provider"),
            "expected a clear rejection message, got: {html}"
        );
    }

    #[tokio::test]
    async fn updating_the_base_currency_persists_and_shows_on_the_store_page() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "base-currency-update@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/base-currency"),
                &session_token,
                &[("base_currency", "EUR")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);

        let page = router
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
        let html = body_text(page).await;
        assert!(html.contains(r#"<option value="EUR" selected>"#), "expected EUR marked selected, got: {html}");
    }

    #[tokio::test]
    async fn setting_a_zero_conf_ceiling_persists_and_shows_on_the_store_page() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "zero-conf-update@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/zero-conf"),
                &session_token,
                &[("zero_conf_max_xmr", "0.5")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);

        let page = router
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
        let html = body_text(page).await;
        assert!(
            html.contains(r#"name="zero_conf_max_xmr" value="0.500000000000""#),
            "expected the new ceiling's own value to round-trip, got: {html}"
        );
    }

    #[tokio::test]
    async fn an_invalid_zero_conf_amount_is_rejected_with_a_clear_error() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "zero-conf-invalid@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/zero-conf"),
                &session_token,
                &[("zero_conf_max_xmr", "not-a-number")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a validation error re-renders the page, it doesn't redirect");
        let html = body_text(response).await;
        assert!(html.contains("Enter a valid XMR amount"), "expected a clear validation error, got: {html}");
    }

    #[tokio::test]
    async fn an_unknown_base_currency_is_rejected_and_the_existing_value_is_unchanged() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "base-currency-bad@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/base-currency"),
                &session_token,
                &[("base_currency", "NOTREAL")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a rejected currency re-renders the page, it doesn't redirect");
        let html = body_text(response).await;
        assert!(html.contains("not a known currency"), "expected a clear rejection message, got: {html}");
        assert!(html.contains(r#"<option value="XMR" selected>"#), "the store's base currency must still be the original default, got: {html}");
    }

    #[tokio::test]
    async fn a_known_currency_with_no_enabled_rate_provider_is_still_accepted_as_a_base_currency_change() {
        // Same decoupling proof as `connections.rs`'s own equivalent test,
        // exercised here for the *change* path rather than creation -
        // `test_state_with_real_engine`'s own `ExchangeRateProviders::xmr_only()`
        // has no provider enabled at all.
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "base-currency-decoupled@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/base-currency"),
                &session_token,
                &[("base_currency", "GBP")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND, "a known currency must be selectable regardless of provider support");
    }

    #[tokio::test]
    async fn changing_the_base_currency_deletes_every_custom_threshold() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token =
            signed_up_and_logged_in_session_token(&router, "base-currency-cascade@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/confirmation-thresholds"),
                &session_token,
                &[("unit_amount", "50.00"), ("confirmations_required", "20")],
            ))
            .await
            .unwrap();
        assert_eq!(state.db.lock().unwrap().count_confirmation_thresholds(&connection_id).unwrap(), 1);

        router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/base-currency"),
                &session_token,
                &[("base_currency", "EUR")],
            ))
            .await
            .unwrap();
        assert_eq!(
            state.db.lock().unwrap().count_confirmation_thresholds(&connection_id).unwrap(),
            0,
            "every custom threshold must be gone after a base currency change"
        );
    }

    #[tokio::test]
    async fn adding_and_deleting_a_custom_confirmation_threshold_round_trips_through_the_real_page() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token =
            signed_up_and_logged_in_session_token(&router, "threshold-add-delete@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/confirmation-thresholds"),
                &session_token,
                &[("unit_amount", "50.00"), ("confirmations_required", "20")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);

        let threshold_id = state.db.lock().unwrap().list_confirmation_thresholds(&connection_id).unwrap()[0].id.clone();

        let page = router
            .clone()
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
        let html = body_text(page).await;
        assert!(html.contains("50.00"), "expected the new threshold's amount shown, got: {html}");
        assert!(
            html.contains(&format!("name=\"delete_{threshold_id}\"")),
            "expected a real delete checkbox for the new threshold in the condensed table, got: {html}"
        );

        // The condensed table's own single Save button posts every row's
        // state (default + delete checkboxes + a possible new row) to one
        // route, not a per-row delete form - see `save_confirmation_thresholds`'s
        // own doc comment.
        let delete_field = format!("delete_{threshold_id}");
        let delete_response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/confirmation-thresholds/save"),
                &session_token,
                &[("confirmations_required", "10"), (delete_field.as_str(), "on")],
            ))
            .await
            .unwrap();
        assert_eq!(delete_response.status(), StatusCode::FOUND);
        assert_eq!(
            state.db.lock().unwrap().count_confirmation_thresholds(&connection_id).unwrap(),
            0,
            "expected the threshold to be gone"
        );

        let after = router
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
        let html = body_text(after).await;
        assert!(
            !html.contains("name=\"delete_"),
            "expected no delete checkboxes once every custom threshold is gone, got: {html}"
        );
    }

    #[tokio::test]
    async fn custom_thresholds_are_displayed_in_ascending_order_of_amount() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "threshold-ordering@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        for amount in ["100.00", "10.00", "50.00"] {
            router
                .clone()
                .oneshot(form_post_request(
                    &format!("/dashboard/connections/{connection_id}/settings/confirmation-thresholds"),
                    &session_token,
                    &[("unit_amount", amount), ("confirmations_required", "15")],
                ))
                .await
                .unwrap();
        }

        let page = router
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
        let html = body_text(page).await;
        let pos_10 = html.find("10.00").expect("10.00 shown");
        let pos_50 = html.find("50.00").expect("50.00 shown");
        let pos_100 = html.find("100.00").expect("100.00 shown");
        assert!(pos_10 < pos_50 && pos_50 < pos_100, "expected ascending amount order, got: {html}");
    }

    #[tokio::test]
    async fn a_sixth_custom_threshold_is_rejected() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "threshold-max-five@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        for i in 0..5 {
            let response = router
                .clone()
                .oneshot(form_post_request(
                    &format!("/dashboard/connections/{connection_id}/settings/confirmation-thresholds"),
                    &session_token,
                    &[("unit_amount", &format!("{i}.00")), ("confirmations_required", "15")],
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FOUND, "expected threshold {i} to be accepted");
        }

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/confirmation-thresholds"),
                &session_token,
                &[("unit_amount", "999.00"), ("confirmations_required", "15")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a rejected sixth threshold re-renders the page, it doesn't redirect");
        let html = body_text(response).await;
        assert!(html.contains("at most 5 custom thresholds"), "expected a clear rejection message, got: {html}");
    }

    #[tokio::test]
    async fn a_duplicate_unit_amount_is_rejected() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "threshold-duplicate@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/confirmation-thresholds"),
                &session_token,
                &[("unit_amount", "50.00"), ("confirmations_required", "20")],
            ))
            .await
            .unwrap();

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/confirmation-thresholds"),
                &session_token,
                &[("unit_amount", "50.00"), ("confirmations_required", "5")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a duplicate amount re-renders the page, it doesn't redirect");
        let html = body_text(response).await;
        assert!(html.contains("already exists"), "expected a clear rejection message, got: {html}");
    }

    #[tokio::test]
    async fn a_negative_unit_amount_is_rejected() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "threshold-negative@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/confirmation-thresholds"),
                &session_token,
                &[("unit_amount", "-5.00"), ("confirmations_required", "20")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a negative amount re-renders the page, it doesn't redirect");
        let html = body_text(response).await;
        assert!(html.contains("non-negative"), "expected a clear rejection message, got: {html}");
    }

    /// Proves the whole resolution chain end to end, against the real
    /// engine: an order priced *at or above* a custom threshold's own
    /// `unit_amount` gets that threshold's `confirmations_required` as its
    /// real per-order override on the engine (not just recorded locally) -
    /// and the local snapshot row records exactly how that was decided.
    /// `TEST_CURRENCY` ("XMR") is also this store's own base currency here
    /// (`create_connection`'s own `"base_currency": "XMR"`), so no rate
    /// lookup is needed for the base-currency conversion itself - see the
    /// next test's own doc comment for the cross-currency case.
    #[tokio::test]
    async fn an_order_priced_above_a_custom_threshold_gets_that_thresholds_confirmations_required_on_the_real_engine() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token =
            signed_up_and_logged_in_session_token(&router, "threshold-resolution-above@example.com", "correct horse battery staple").await;
        let (connection_id, public_key) = create_connection(&router, &session_token).await;

        router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/confirmation-thresholds"),
                &session_token,
                &[("unit_amount", "5.00"), ("confirmations_required", "20")],
            ))
            .await
            .unwrap();

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/orders/new"),
                &session_token,
                &[("amount", "10.00"), ("currency", TEST_CURRENCY)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let location = response.headers().get("location").unwrap().to_str().unwrap().to_string();
        let payment_id = location.rsplit('/').next().unwrap().to_string();

        let store = engine.store().lock().unwrap();
        let tenant_id = store.find_tenant_by_public_key(&public_key).unwrap().unwrap().id;
        let stored = store.get_order(&tenant_id, &payment_id).unwrap().unwrap();
        assert_eq!(
            stored.confirmations_required_override,
            Some(20),
            "a 10.00 XMR order against a 5.00-and-up threshold of 20 confirmations must use that threshold, not the default"
        );
        drop(store);

        let metadata = state.db.lock().unwrap().get_order_currency_metadata(&connection_id, &payment_id).unwrap().unwrap();
        assert_eq!(metadata.store_base_currency, Some("XMR".to_string()));
        assert_eq!(
            metadata.base_currency_piconero_per_unit, None,
            "the order's own currency already was the base currency, so no second conversion rate exists to snapshot"
        );
        assert_eq!(metadata.confirmations_required_applied, Some(20));

        // The whole point of the snapshot (WBS: "makes it clear how the
        // confirmation threshold was decided") - the order's own detail
        // page must actually show it, not just record it in the database.
        let detail_response = router
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
        assert_eq!(detail_response.status(), StatusCode::OK);
        let html = body_text(detail_response).await;
        assert!(html.contains("Confirmations required"), "expected the confirmations-required row's label, got: {html}");
        assert!(html.contains(">20<"), "expected the resolved threshold's own confirmations_required (20) shown, got: {html}");
        assert!(html.contains("Store base currency"), "expected the base-currency snapshot row's label, got: {html}");
        assert!(html.contains("same as order currency"), "expected the no-second-rate case shown plainly, got: {html}");
    }

    /// The order-below-every-threshold half of the same chain: a threshold
    /// exists, but the order's own amount never reaches it, so the store's
    /// plain default (fallback) confirmations_required - the real tenant
    /// value on the engine, 10 by the engine's own `create_tenant` default
    /// (`scanner::store::Db::create_tenant`) - is what's actually applied.
    #[tokio::test]
    async fn an_order_priced_below_every_custom_threshold_uses_the_stores_default_confirmations_required() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token =
            signed_up_and_logged_in_session_token(&router, "threshold-resolution-below@example.com", "correct horse battery staple").await;
        let (connection_id, public_key) = create_connection(&router, &session_token).await;

        router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/confirmation-thresholds"),
                &session_token,
                &[("unit_amount", "50.00"), ("confirmations_required", "99")],
            ))
            .await
            .unwrap();

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/orders/new"),
                &session_token,
                &[("amount", "10.00"), ("currency", TEST_CURRENCY)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let location = response.headers().get("location").unwrap().to_str().unwrap().to_string();
        let payment_id = location.rsplit('/').next().unwrap().to_string();

        let store = engine.store().lock().unwrap();
        let tenant_id = store.find_tenant_by_public_key(&public_key).unwrap().unwrap().id;
        let stored = store.get_order(&tenant_id, &payment_id).unwrap().unwrap();
        assert_eq!(
            stored.confirmations_required_override,
            Some(10),
            "a 10.00 XMR order below the only threshold's 50.00 must fall back to the store's own default"
        );
        drop(store);

        let metadata = state.db.lock().unwrap().get_order_currency_metadata(&connection_id, &payment_id).unwrap().unwrap();
        assert_eq!(metadata.confirmations_required_applied, Some(10));
    }

    #[tokio::test]
    async fn the_default_threshold_has_no_delete_control_and_is_not_a_row_in_the_custom_table() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "threshold-default-not-deletable@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let page = router
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
        let html = body_text(page).await;
        assert!(html.contains("Default (fallback)"), "expected the default threshold's own row, got: {html}");
        assert!(
            !html.contains("name=\"delete_"),
            "a fresh store has no custom thresholds, so no delete checkbox should exist yet, got: {html}"
        );
    }

    #[tokio::test]
    async fn updating_the_confirmation_threshold_with_non_numeric_input_shows_a_clear_error() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "confirmations-non-numeric@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/confirmations"),
                &session_token,
                &[("confirmations_required", "not-a-number")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("Enter a whole number of confirmations."), "expected a clear validation error, got: {html}");
    }

    #[tokio::test]
    async fn a_different_user_cannot_update_confirmations_on_someone_elses_connection() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let owner_token = signed_up_and_logged_in_session_token(
            &router,
            "confirmations-owner@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &owner_token).await;

        let intruder_token = signed_up_and_logged_in_session_token(
            &router,
            "confirmations-intruder@example.com",
            "correct horse battery staple",
        )
        .await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/settings/confirmations"),
                &intruder_token,
                &[("confirmations_required", "3")],
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

    /// `docs/txid_lookup_and_scan_chunking_wbs.md` Part B.3 - the direct
    /// replacement for the rescan trigger form above. `with_admin_lookup_
    /// daemon`'s inert `NoopDaemonClient` always reports a txid as not found,
    /// which is real, deterministic behavior to assert against rather than a
    /// guess - the real-match/no-match cases are already exhaustively covered
    /// at the engine's own `http/tests.rs` level.
    #[tokio::test]
    async fn lookup_payment_reshows_the_orders_page_with_a_not_found_message() {
        let (state, _engine) = test_state_with_real_engine_and_admin_lookup_daemon().await;
        let router = build_router(state);
        let session_token =
            signed_up_and_logged_in_session_token(&router, "lookup-owner@example.com", "correct horse battery staple")
                .await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let txid = "a".repeat(64);
        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/connections/{connection_id}/orders/lookup"),
                &session_token,
                &[("txid", &txid)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a lookup re-renders the orders page, it doesn't redirect");
        let html = body_text(response).await;
        assert!(html.contains("No transaction with that ID was found"), "expected the not-found message, got: {html}");
        assert!(html.contains(&txid), "the submitted txid must repopulate the form's own input");
    }

    // -- "Scan range" row (`docs/order_rescan_wbs.md` Phase 5.4) ------------

    #[tokio::test]
    async fn scan_range_row_shows_a_muted_dash_for_an_order_with_no_first_tick_yet() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "scan-range-fresh-owner@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) = create_connection(&router, &session_token).await;
        let payment_id = seed_real_order(engine.addr, &public_key).await;
        // Deliberately no `bump_scanned_range_for_order` call - this harness runs
        // no background scan loop, so a freshly seeded order genuinely has never
        // been examined by anything yet.

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/connections/{connection_id}/orders/{payment_id}"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .header("host", "test.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("Scan range"), "expected the row's own label, got: {html}");
        assert!(html.contains(r#"<span class="muted">-</span>"#), "expected the muted-dash fallback, got: {html}");
    }

    #[tokio::test]
    async fn scan_range_row_shows_a_growing_range_while_the_order_is_still_being_watched() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "scan-range-growing-owner@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) = create_connection(&router, &session_token).await;
        let payment_id = seed_real_order(engine.addr, &public_key).await;
        // Still `pending` (non-terminal) - genuinely still in scope, so the range
        // must read as still growing ("N+"), not a closed span. Two calls to the
        // live scanner's own bulk bump (its only mover now that the manual rescan
        // feature is gone) rather than a single-order setter: the first (COALESCE)
        // establishes `first_scanned_height`, the second only advances `last_
        // scanned_height`, matching exactly how two real scan ticks would move it.
        {
            let s = engine.store().lock().unwrap();
            let tenant = s.find_tenant_by_public_key(&public_key).unwrap().unwrap();
            s.bump_scanned_heights_for_tenant(&tenant.id, 100, crate::now_unix(), 0).unwrap();
            s.bump_scanned_heights_for_tenant(&tenant.id, 250, crate::now_unix(), 0).unwrap();
        }

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/connections/{connection_id}/orders/{payment_id}"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .header("host", "test.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("100+"), "expected the still-growing range display, got: {html}");
        assert!(!html.contains("100 - 250"), "must not show a closed range while still in scope, got: {html}");
    }

    /// `currently_scanning: false` (a closed, no-longer-growing range) needs an
    /// order genuinely past both its own deadline *and* the grace window in real
    /// wall-clock terms - this harness runs no background scan loop and seeds
    /// orders with a real ~30-minute-out `expires_at`, so reaching that state
    /// against a real order would mean an actual wait. Tested directly against the
    /// template instead, the same way this page's other conditional rows
    /// (`order_detail_hides_the_double_spend_row_entirely_when_none_was_detected`)
    /// already are - `display_scan_range`'s own unit-level correctness is what
    /// this is really about, and the wiring that reaches it is already proven by
    /// the two tests above.
    #[test]
    fn scan_range_row_shows_a_closed_range_once_no_longer_being_watched() {
        let order = OrderDetailData {
            payment_id: "pay_abc123".to_string(),
            merchant_order_id: None,
            address: "addr".to_string(),
            currency: "XMR".to_string(),
            amount: "0.5".to_string(),
            rate_display: "1.000000000000 XMR per 1 XMR".to_string(),
            rate_provider: "xmr".to_string(),
            xmr_amount_piconero: 500_000_000_000,
            amount_received_piconero: 500_000_000_000,
            status: "paid".to_string(),
            confirmations: 10,
            confirmations_required_display: "10".to_string(),
            base_currency_display: "XMR".to_string(),
            base_currency_rate_display: "same as order currency".to_string(),
            double_spend_detected_at: None,
            double_spend_detected_at_display: crate::templates::display_timestamp_or_dash(None),
            refund_address: None,
            created_at_display: "1000".to_string(),
            expires_at_display: "2000".to_string(),
            updated_at_display: "1000".to_string(),
            payments: vec![],
            payment_link: "http://127.0.0.1:8081/pay/pk_abc123/orders/pay_abc123/share".to_string(),
            scan_range_display: crate::templates::display_scan_range(Some(100), Some(250), false),
        };
        let data = OrderDetailViewModel { connection_id: "conn_1".to_string(), order: Some(order), meta_refresh_secs: 15 };
        let chrome = crate::views::PageChrome::from_user(None, "");
        let html = crate::views::orders::detail_page(&chrome, &data).into_string();
        assert!(html.contains("100 - 250"), "expected the closed range display, got: {html}");
        assert!(!html.contains("100+"), "must not show a still-growing range once no longer being watched, got: {html}");
    }
}
