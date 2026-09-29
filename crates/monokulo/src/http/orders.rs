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

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::crypto;
use crate::db::{StoreConnectionRow, UserRow};
use crate::engine_client::{EngineClientError, PaymentLookupView};
use crate::templates::{display_or_dash, display_scan_range};
use crate::views;
use crate::views::orders::{OrderDetailData, OrderDetailViewModel, OrderRowViewModel, OrdersViewModel, PaymentRowViewModel};

use super::dashboard::redirect_302;
use super::fx::FxRequest;
use super::{AppState, AuthedUser};
use crate::views::store_settings::StoreSection;

/// Looks up `store_connections` row `id` and confirms it belongs to `user`.
/// `Ok(None)` covers *both* "no such row" and "exists but belongs to someone
/// else" - callers must map that uniformly to `404` (see this module's own
/// doc comment), never distinguishing the two. `Err(())` is a real database
/// failure - the caller's problem, not the requester's.
pub(super) fn load_owned_connection(state: &AppState, user: &UserRow, id: &str) -> Result<Option<StoreConnectionRow>, ()> {
    let row = state.db.lock().get_store_connection_by_id(id).map_err(|_| ())?;
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

/// Shared by `orders_list` - the "real orders, real fiat metadata" view
/// model for the plain orders list page.
async fn build_orders_view_model(
    state: &AppState,
    row: &StoreConnectionRow,
    sk: &str,
    search: &str,
    page: u32,
) -> Result<OrdersViewModel, ()> {
    use views::orders::ORDERS_PER_PAGE;
    let term = Some(search.trim()).filter(|term| !term.is_empty());
    // One more than a page, to know whether there is an older page.
    let mut orders = state.engine_client.list_orders_page(sk, false, term, ORDERS_PER_PAGE + 1, page * ORDERS_PER_PAGE).await.map_err(|_| ())?;
    let has_more = orders.len() > ORDERS_PER_PAGE as usize;
    orders.truncate(ORDERS_PER_PAGE as usize);
    Ok(OrdersViewModel {
        connection_id: row.id.clone(),
        display_name: display_name_for(&row.site_url),
        orders: order_rows(state, row, orders),
        search: search.trim().to_string(),
        page,
        has_more,
    })
}

/// The orders table's rows: the engine's view of each order, with what only
/// monokulo knows - its fiat amount, where it came from, and whether the
/// POS cancelled it.
pub(super) fn order_rows(state: &AppState, row: &StoreConnectionRow, orders: Vec<crate::engine_client::OrderView>) -> Vec<OrderRowViewModel> {
    let ids: Vec<String> = orders.iter().map(|o| o.order_id.clone()).collect();
    let (fiat_metadata, details) = {
        let db = state.db.lock();
        (db.list_order_currency_metadata_for_connection(&row.id).unwrap_or_default(), db.order_listing_details(&row.id, &ids).unwrap_or_default())
    };
    orders
        .into_iter()
        .map(|o| {
            let (amount, currency) = match fiat_metadata.get(&o.order_id) {
                Some(m) => (m.amount.clone(), m.currency.clone()),
                None => ("—".to_string(), "".to_string()),
            };
            let detail = details.get(&o.order_id).cloned().unwrap_or_default();
            // A POS cancellation only matters while nothing was paid: a
            // payment that arrived anyway keeps the engine's status.
            let status = if detail.pos_cancelled_at.is_some() && matches!(o.status.as_str(), "pending" | "expired") {
                "cancelled".to_string()
            } else {
                o.status
            };
            OrderRowViewModel {
                order_id: o.order_id,
                reference: o.merchant_order_id,
                source: source_label(detail.source.as_deref(), &row.platform).to_string(),
                status,
                amount,
                currency,
                created_at: o.created_at,
            }
        })
        .collect()
}

/// How an order's source reads in the orders table.
fn source_label(source: Option<&str>, platform: &str) -> &'static str {
    match source {
        Some("pos") => "POS",
        Some("dashboard") => "Dashboard",
        Some("api") if platform == "woocommerce" => "WooCommerce",
        Some("api") => "Store API",
        Some("website") => "Website",
        _ => "—",
    }
}

#[derive(Deserialize)]
pub struct OrdersListQuery {
    q: Option<String>,
    page: Option<u32>,
}

pub async fn orders_list(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    fx: FxRequest,
    Path(id): Path<String>,
    Query(query): Query<OrdersListQuery>,
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
    let search: String = query.q.unwrap_or_default().chars().take(120).collect();
    let view_model = match build_orders_view_model(&state, &row, &sk, &search, query.page.unwrap_or(0).min(10_000)).await {
        Ok(vm) => vm,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if fx.0 {
        return axum::response::Html(views::orders::list_results(&view_model, &views::time::Clock::for_user(&user)).into_string()).into_response();
    }
    let chrome = super::page_chrome(&state, Some(&user), format!("/dashboard/stores/{id}/orders"));
    views::orders::list_page(&chrome, &view_model).into_response()
}

#[derive(Deserialize)]
pub struct LookupPaymentForm {
    txid: String,
}

/// The engine call shared by `lookup_payment`'s only caller
/// (`views::store_detail`'s own "Look up a transaction" card, folded there from
/// the standalone orders list page it used to live on) - a plain,
/// human-readable result message plus the matched order's id, if any.
async fn perform_payment_lookup(state: &AppState, sk: &str, txid: &str) -> Result<(String, Option<String>), ()> {
    match state.engine_client.lookup_payment(sk, txid).await {
        Ok(PaymentLookupView::NotFoundOnChain) => {
            Ok(("No transaction with that ID was found on the network.".to_string(), None))
        }
        Ok(PaymentLookupView::NoMatchingOrder) => {
            Ok(("That transaction exists, but doesn't pay any of this store's orders.".to_string(), None))
        }
        Ok(PaymentLookupView::Matched { order_ids }) => {
            Ok(("Match found and recorded.".to_string(), order_ids.into_iter().next()))
        }
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            Ok((format!("Couldn't look that up: {message}"), None))
        }
        Err(_) => Err(()),
    }
}

/// `POST /dashboard/stores/{id}/orders/lookup` - `docs/txid_lookup_and_
/// scan_chunking_wbs.md` Part B.3, the direct replacement for the old
/// manual rescan feature. Re-renders the store overview page with the
/// result shown inline in its "Recent orders" section (a plain message,
/// plus a link to the matched order if any) - recompute and redisplay,
/// never a redirect that would lose the result.
pub async fn lookup_payment(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    fx: FxRequest,
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
    let (message, found_order_id) = match perform_payment_lookup(&state, &sk, &txid).await {
        Ok(result) => result,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    if fx.0 {
        // Just the card, with its answer.
        let card = views::orders::lookup_payment_card(
            &format!("/dashboard/stores/{id}/orders/lookup"),
            &txid,
            &Some(message),
            &found_order_id,
            |order_id| format!("/dashboard/stores/{id}/orders/{order_id}"),
        );
        return axum::response::Html(card.into_string()).into_response();
    }
    render_store_detail_page(&state, row, &user, txid, Some(message), found_order_id).await
}

/// `GET /dashboard/stores/{id}/orders/{order_id}` - the order's full
/// detail (every `OrderView` field plus its `payments` list). A
/// `order_id` the engine doesn't recognize for this tenant (unknown, or
/// belonging to a different one) renders a clear "not found" state with a
/// real `404`, not a raw `500` - the engine's own `404` is distinguished
/// from every other non-success status the same way
/// `connections::create_connection_for_user` already distinguishes the
/// engine's `400` from everything else.
pub async fn order_detail(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, order_id)): Path<(String, String)>,
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
    let chrome = super::page_chrome(&state, Some(&user), format!("/dashboard/stores/{id}/orders/{order_id}"));
    let payment_link = payment_link_for(&headers, &row.tenant_public_key, &order_id);

    match order_detail_data(&state, &row, &sk, &order_id, payment_link).await {
        Ok(Some(order)) => {
            let view_model = OrderDetailViewModel { connection_id: id.to_string(), display_name: display_name_for(&row.site_url), order: Some(order) };
            views::orders::detail_page(&chrome, &view_model).into_response()
        }
        Ok(None) => {
            let view_model = OrderDetailViewModel { connection_id: id.to_string(), display_name: display_name_for(&row.site_url), order: None };
            (StatusCode::NOT_FOUND, views::orders::detail_page(&chrome, &view_model)).into_response()
        }
        Err(()) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Everything the order detail page shows about one order, `None` when
/// the engine has no such order.
async fn order_detail_data(
    state: &AppState,
    row: &StoreConnectionRow,
    sk: &str,
    order_id: &str,
    payment_link: String,
) -> Result<Option<OrderDetailData>, ()> {
    match state.engine_client.get_order_detail(sk, order_id).await {
        Ok(detail) => {
            // The engine has no concept of fiat any more (`docs/fx_refactor.md`
            // Phase 3) - fiat display comes entirely from monokulo's own
            // local `order_currency_metadata`, absent for any order that predates
            // this record (falls back to a dash rather than failing the page).
            let metadata = state.db.lock().get_order_currency_metadata(&row.id, &order_id).ok().flatten();
            let (amount, currency) = match &metadata {
                Some(m) => (m.amount.clone(), m.currency.clone()),
                None => ("—".to_string(), "".to_string()),
            };
            let (rate_display, rate_provider) = match &metadata {
                Some(m) => (
                    format!("{} XMR per 1 {}", crate::views::trim_xmr(&shared::exchange_rate::format_piconero_as_xmr(m.piconero_per_unit)), m.currency),
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
                        format!("{} XMR per 1 {base_currency}", crate::views::trim_xmr(&shared::exchange_rate::format_piconero_as_xmr(rate)))
                    }
                    None => "same as order currency".to_string(),
                },
                None => "—".to_string(),
            };
            let from_pos = matches!(state.db.lock().get_pos_order(&row.id, order_id), Ok(Some(_)));
            Ok(Some(OrderDetailData {
                from_pos,
                order_id: detail.order.order_id,
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
                refund_address: detail.order.refund_address,
                created_at: detail.order.created_at,
                expires_at: detail.order.expires_at,
                updated_at: detail.order.updated_at,
                payments: detail
                    .payments
                    .into_iter()
                    .map(|p| PaymentRowViewModel {
                        txid: p.txid,
                        output_index: p.output_index,
                        amount_piconero: p.amount_piconero,
                        first_seen_at: p.first_seen_at,
                        block_height_display: display_or_dash(p.block_height.map(|h| h.to_string()).as_deref()),
                        voided_at: p.voided_at,
                    })
                    .collect(),
                payment_link,
                scan_range_display: display_scan_range(
                    detail.order.first_scanned_height,
                    detail.order.last_scanned_height,
                    detail.order.currently_scanning,
                ),
            }))
        }
        Err(EngineClientError::EngineError { status, .. }) if status == reqwest::StatusCode::NOT_FOUND => Ok(None),
        Err(_) => Err(()),
    }
}

/// `GET /dashboard/stores/{id}/orders/{order_id}/events`: the order detail
/// page's live part, re-rendered whenever the order changes (and every
/// 30 s, for the clock), as ssexi JSON-routed events replacing
/// `#order-live`. Ends with a `done` event once the order can't change
/// any more, so the page stops reconnecting (`static/fx-glue.js`).
pub async fn order_detail_events(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, order_id)): Path<(String, String)>,
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
    let payment_link = payment_link_for(&headers, &row.tenant_public_key, &order_id);
    let subscription = state.engine_client.subscribe_order(&row.id, &sk, &order_id);
    let row = std::sync::Arc::new(row);
    // Times in the viewer's zone, as the page itself shows them.
    let clock = views::time::Clock::for_user(&user);
    crate::live::live_events(subscription, std::time::Duration::from_secs(30), move || {
        let (state, row, sk, order_id, payment_link, clock) = (state.clone(), row.clone(), sk.clone(), order_id.clone(), payment_link.clone(), clock.clone());
        async move {
            let order = order_detail_data(&state, &row, &sk, &order_id, payment_link).await.ok()??;
            let (_, _, terminal) = crate::views::order_state(&order.status);
            let html = views::orders::live_fragment(&order, &clock).into_string();
            let mut events =
                vec![axum::response::sse::Event::default().event(r##"{"target":"#order-live","swap":"outerHTML"}"##).data(html.clone())];
            if terminal {
                events.push(axum::response::sse::Event::default().event("done").data("final"));
            }
            Some(crate::live::LiveSnapshot { events, fingerprint: html, terminal })
        }
    })
}

/// A real, absolute, copy-pasteable URL for paying an order - not just the
/// path - since the whole point is something a merchant can paste into an
/// email or chat to someone who isn't already looking at this dashboard.
/// This instance has no configured "external base URL" of its own yet, so
/// it's built from the incoming request's own `Host` header (what the
/// merchant's browser just used to reach this page) plus
/// `X-Forwarded-Proto` if a reverse proxy set it, falling back to plain
/// `http` for local/dev use.
fn payment_link_for(headers: &HeaderMap, public_key: &str, order_id: &str) -> String {
    let host = headers.get(axum::http::header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let scheme = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()).unwrap_or("http");
    format!("{scheme}://{host}/pay/{public_key}/orders/{order_id}/share")
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

/// `POST /dashboard/stores/{id}/webhooks` - registers a new webhook via
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
    fx: FxRequest,
    Path(id): Path<String>,
    Form(form): Form<CreateWebhookForm>,
) -> Response {
    const SECTION: StoreSection = StoreSection::Webhooks;
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
        return render_store_settings_page(&state, row, &user, Some("Enter a webhook URL.".to_string()), None, Some((SECTION, fx))).await;
    }
    let extra_headers = match parse_extra_headers(&form.extra_headers) {
        Ok(headers) => headers,
        Err(message) => return render_store_settings_page(&state, row, &user, Some(message), None, Some((SECTION, fx))).await,
    };

    match state.engine_client.create_webhook(&sk, url, &extra_headers).await {
        Ok((_webhook_id, signing_secret)) => render_store_settings_page(&state, row, &user, None, Some(signing_secret), Some((SECTION, fx))).await,
        // The engine's own validation (a malformed URL, a non-http(s) scheme -
        // `src/http/admin.rs::create_webhook` at the repo root) - the
        // caller's mistake, surfaced verbatim, same convention
        // `connections::create_connection_for_user` already applies to the
        // engine's tenant-creation `400`s.
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            render_store_settings_page(&state, row, &user, Some(message), None, Some((SECTION, fx))).await
        }
        Err(_) => render_store_settings_page(&state, row, &user, Some("Something went wrong. Please try again.".to_string()), None, Some((SECTION, fx))).await,
    }
}

/// `POST /dashboard/stores/{id}/settings/webhooks/{webhook_id}/delete` -
/// a POST (not a real `DELETE`) because a plain HTML `<form>` can only submit
/// `GET`/`POST`. Redirects back to the settings page on success
/// (POST-redirect-GET - refreshing the page after a delete must not risk
/// resubmitting it) or on the engine's own `404` for an unknown/not-this-
/// tenant's `webhook_id`; only a genuine internal error re-renders the page
/// with a visible error.
pub async fn webhooks_delete(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    fx: FxRequest,
    Path((id, webhook_id)): Path<(String, String)>,
) -> Response {
    const SECTION: StoreSection = StoreSection::Webhooks;
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
        Ok(()) => saved(&state, row, &user, SECTION, fx, &format!("/dashboard/stores/{id}/settings")).await,
        Err(EngineClientError::EngineError { status, .. }) if status == reqwest::StatusCode::NOT_FOUND => {
            saved(&state, row, &user, SECTION, fx, &format!("/dashboard/stores/{id}/settings")).await
        }
        Err(_) => {
            render_store_settings_page(&state, row, &user, Some("Could not delete that webhook. Please try again.".to_string()), None, Some((SECTION, fx))).await
        }
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

/// `GET /dashboard/stores/{id}` - the store overview page: identity,
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
            let chrome = super::page_chrome(&state, Some(&user), format!("/dashboard/stores/{id}"));
            let data = views::store_detail::StoreDetailViewModel { store: None };
            return (StatusCode::NOT_FOUND, views::store_detail::page(&chrome, &data)).into_response();
        }
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render_store_detail_page(&state, row, &user, String::new(), None, None).await
}

/// Shared by `store_detail` and `lookup_payment` - both end by showing the
/// same store overview page, the latter with a real "look up a payment"
/// result overlaid on it rather than a redirect that would lose it. Takes an
/// already ownership-checked row rather than re-checking it, since every
/// caller has already done that.
async fn render_store_detail_page(
    state: &AppState,
    row: StoreConnectionRow,
    user: &UserRow,
    lookup_txid_value: String,
    lookup_message: Option<String>,
    lookup_found_order_id: Option<String>,
) -> Response {
    let chrome = super::page_chrome(state, Some(user), format!("/dashboard/stores/{}", row.id));
    let sk = match decrypt_sk(state, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let tenant_result = state.engine_client.get_tenant(&sk).await;
    let (health, health_label) = health_of_tenant_lookup(&tenant_result);

    // A store whose engine is currently unreachable still gets a real page -
    // just with no order data available, rather than a hard error. The
    // health tag above is what actually communicates the problem.
    let recent_orders = match state.engine_client.list_orders_page(&sk, false, None, 10, 0).await {
        Ok(orders) => order_rows(&state, &row, orders),
        Err(_) => Vec::new(),
    };

    let is_woocommerce = row.platform == "woocommerce";
    let embed_warnings = super::embed_domains::store_page_warnings(state, &row.id, crate::now_unix());
    let view_model = views::store_detail::StoreDetailViewModel {
        store: Some(views::store_detail::StoreDetailData {
            connection_id: row.id,
            display_name: display_name_for(&row.site_url),
            platform: row.platform,
            site_url: row.site_url,
            public_key: row.tenant_public_key,
            public_url: crate::settings::public_url(&state.db.lock()),
            base_currency: row.base_currency,
            health,
            health_label,
            created_at: row.created_at,
            recent_orders,
            is_woocommerce,
            lookup_txid_value,
            lookup_message,
            lookup_found_order_id,
            embed_warnings,
        }),
    };
    views::store_detail::page(&chrome, &view_model).into_response()
}

/// `GET /dashboard/stores/{id}/settings` - base currency, confirmation
/// thresholds (0-conf included), exchange rate provider, and webhooks, all
/// split out from the store overview page onto their own settings page.
pub async fn store_settings(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render_store_settings_page(&state, row, &user, None, None, None).await
}

/// Shared by every settings mutation below (base currency, confirmation
/// thresholds/0-conf, fx provider, webhooks) - all of them end by showing a
/// fresh copy of this same page, optionally with a validation error or a
/// just-created webhook signing secret. Takes an already ownership-checked
/// row rather than re-checking it, since every caller has already done
/// that.
///
/// `from` names the section a form was posted from and whether fixi sent
/// it: its error shows there, and fixi gets only that section back (with
/// any section the save changed too, out of band), `422` when refused.
pub(super) async fn render_store_settings_page(
    state: &AppState,
    row: StoreConnectionRow,
    user: &UserRow,
    settings_error: Option<String>,
    created_webhook_signing_secret: Option<String>,
    from: Option<(StoreSection, FxRequest)>,
) -> Response {
    let chrome = super::page_chrome(state, Some(user), format!("/dashboard/stores/{}/settings", row.id));
    let sk = match decrypt_sk(state, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let tenant_result = state.engine_client.get_tenant(&sk).await;
    if tenant_result.as_ref().is_ok_and(|t| t.key_custody_backend.is_some()) {
        // The backends come from the engine's status: fetched now if it
        // isn't cached (it was just invalidated by a move, for one), but
        // never holding the page up for long.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(1500), super::status_page::get_status_cached(state)).await;
    }
    let key_storage = tenant_result.as_ref().ok().and_then(|t| t.key_custody_backend.clone()).and_then(|current| {
        let enabled = super::status_page::known_enabled_custody_backends(state);
        let move_to: Vec<views::connect::CustodyChoice> = enabled
            .iter()
            .filter(|b| **b != current)
            .enumerate()
            .map(|(i, backend)| views::connect::CustodyChoice {
                backend: backend.clone(),
                label: super::status_page::custody_backend_label(backend),
                selected: i == 0,
            })
            .collect();
        (!move_to.is_empty()).then(|| views::store_settings::KeyStorageView {
            current: super::status_page::custody_backend_label(&current),
            current_disabled: !enabled.contains(&current),
            move_to,
        })
    });
    let confirmations_required = tenant_result.as_ref().map(|t| t.confirmations_required).unwrap_or(10);
    // Native 0-conf: the Default row's own checkbox is checked exactly when the
    // tenant's default confirmations count already is 0 - no separate engine
    // field to read.
    let zero_conf_enabled = tenant_result.as_ref().is_ok_and(|t| t.confirmations_required == 0);

    let fx_provider_options = fx_provider_options(&state.exchange_rate.available_providers(), &row.fx_providers);
    let (base_currency_options, confirmation_thresholds) = {
        let db = state.db.lock();
        let options = crate::currencies::currency_options(&db, &row.base_currency).unwrap_or_default();
        let thresholds = db
            .list_confirmation_thresholds(&row.id)
            .unwrap_or_default()
            .into_iter()
            .map(|t| views::store_settings::ConfirmationThresholdView { id: t.id, unit_amount: t.unit_amount, confirmations_required: t.confirmations_required })
            .collect::<Vec<_>>();
        (options, thresholds)
    };
    let confirmation_thresholds_at_max = confirmation_thresholds.len() >= 5;
    let (embed_restricted, embed_domain_rows, client_logging) = {
        let db = state.db.lock();
        (
            db.embed_restricted(&row.id).unwrap_or(false),
            db.list_store_domains(&row.id).unwrap_or_default(),
            db.client_logging(&row.id).unwrap_or(false),
        )
    };
    let now = crate::now_unix();
    let embed_can_restrict = embed_domain_rows.iter().any(|domain| crate::embed_domains::DomainState::of(domain, now).counts());
    let embed_domains = super::embed_domains::domain_views(embed_domain_rows, now);

    let webhooks = match state.engine_client.list_webhooks(&sk).await {
        Ok(webhooks) => webhooks
            .into_iter()
            .map(|w| views::store_settings::WebhookRowViewModel {
                webhook_id: w.webhook_id,
                url: w.url,
                enabled: w.enabled,
                created_at: w.created_at,
            })
            .collect(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let view_model = views::store_settings::StoreSettingsViewModel {
        store: Some(views::store_settings::StoreSettingsData {
            clock: chrome.clock.clone(),
            connection_id: row.id,
            display_name: display_name_for(&row.site_url),
            confirmations_required,
            fx_provider_options,
            base_currency: row.base_currency,
            base_currency_options,
            confirmation_thresholds,
            confirmation_thresholds_at_max,
            zero_conf_enabled,
            webhooks,
            created_webhook_signing_secret,
            settings_error,
            embed_domains,
            embed_restricted,
            embed_can_restrict,
            key_storage,
            active_section: from.map(|(section, _)| section),
            client_logging,
        }),
    };
    if let (Some((section, FxRequest(true))), Some(store)) = (from, &view_model.store) {
        let refused = store.settings_error.is_some();
        let fragment = maud::html! {
            (views::store_settings::section(store, section, false))
            @for other in section.also_changes() { (views::store_settings::section(store, *other, true)) }
        };
        return if refused { super::fx::invalid(fragment) } else { axum::response::Html(fragment.into_string()).into_response() };
    }
    views::store_settings::page(&chrome, &view_model).into_response()
}

/// After a successful save: for fixi, the saved section as it is now;
/// otherwise the usual redirect back to the page.
pub(super) async fn saved(state: &AppState, row: StoreConnectionRow, user: &UserRow, section: StoreSection, fx: FxRequest, redirect_to: &str) -> Response {
    if fx.0 {
        // Read again: `row` is from before the save.
        let row = match load_owned_connection(state, user, &row.id) {
            Ok(Some(fresh)) => fresh,
            _ => row,
        };
        render_store_settings_page(state, row, user, None, None, Some((section, fx))).await
    } else {
        redirect_302(redirect_to)
    }
}

/// Every currency this store's "create an order" form can offer right now -
/// shared by `create_order_page` and `create_order`'s own validation-error
/// re-render, so the two can never drift on what the dropdown looks like.
async fn order_currency_options_for(state: &AppState, row: &StoreConnectionRow) -> (Vec<String>, bool) {
    // A live Coingecko failure here degrades to "XMR only" rather than
    // failing this whole page - same "show something real-ish rather than
    // fail outright" approach the store overview page's own health check
    // already takes for its own engine-reachability failures.
    let known_currencies: Vec<String> = state
        .db
        .lock()
        .list_currencies()
        .map(|rows| rows.into_iter().map(|c| c.canonical_code).collect())
        .unwrap_or_default();
    let order_currency_options =
        state.exchange_rate.supported_currencies_for(row, &known_currencies).await.unwrap_or_else(|_| vec!["XMR".to_string()]);
    let order_currency_is_locked_to_xmr = order_currency_options.len() == 1 && order_currency_options[0] == "XMR";
    (order_currency_options, order_currency_is_locked_to_xmr)
}

/// `GET /dashboard/stores/{id}/orders/new` - the "create an order"
/// widget page, reached from the store overview page's own widget tile (the
/// same shape as the POS terminal's own tile/page).
pub async fn create_order_page(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
) -> Response {
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render_create_order_page(&state, row, &user, None).await
}

/// Shared by `create_order_page` and `create_order`'s own validation-error
/// branches - both end by showing a fresh copy of this same page.
async fn render_create_order_page(state: &AppState, row: StoreConnectionRow, user: &UserRow, order_creation_error: Option<String>) -> Response {
    let chrome = super::page_chrome(state, Some(user), format!("/dashboard/stores/{}/orders/new", row.id));
    let (order_currency_options, order_currency_is_locked_to_xmr) = order_currency_options_for(state, &row).await;
    let data = views::create_order::CreateOrderData {
        connection_id: row.id.clone(),
        display_name: display_name_for(&row.site_url),
        order_creation_error,
        order_currency_options,
        order_currency_is_locked_to_xmr,
    };
    views::create_order::page(&chrome, &data).into_response()
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

/// `POST /dashboard/stores/{id}/orders/new` - creates a real order
/// directly from the dashboard, via the engine's authenticated
/// order-creation API (`EngineClient::create_order`) - lets a merchant try the
/// payment flow without wiring up a storefront first. Redirects straight to
/// the new order's own detail page on success (POST-redirect-GET); a
/// validation error (unsupported currency, unparseable amount) re-renders
/// this same "create an order" page with the engine's real message, same
/// convention `webhooks_create` already applies.
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

    let policy_lock = crate::confirmation_thresholds::policy_lock(&row.tenant_public_key);
    let _policy_guard = policy_lock.lock().await;
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let amount = form.amount.trim();
    let currency = form.currency.trim();
    if amount.is_empty() || currency.is_empty() {
        return render_create_order_page(&state, row, &user, Some("Enter an amount and a currency.".to_string())).await;
    }

    // Selection-time validation first, entirely independent of whether any
    // provider can actually price it - see `crate::currencies`'s own doc
    // comment and `http::pay::create_order`'s matching check for why
    // "unknown currency" and "unsupported currency" are kept as distinct
    // messages rather than collapsed into one.
    let currency_known = crate::currencies::is_known_currency(&state.db.lock(), currency);
    match currency_known {
        Ok(true) => {}
        Ok(false) => return render_create_order_page(&state, row, &user, Some(format!("unknown currency: {currency}"))).await,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }

    // The engine has no concept of currency any more (`docs/fx_refactor.md`
    // Phase 3) - monokulo's own exchange rate does the same computation
    // `http::pay::create_order` does for a real storefront call. `"XMR"`
    // always uses the trivial identity rate regardless of this store's
    // chosen ordered `fx_providers`; every other currency goes through it.
    let (piconero_per_unit, provider) = match state.exchange_rate.piconero_per_unit_for(&row, currency).await {
        Ok(Some(result)) => result,
        Ok(None) => {
            return render_create_order_page(&state, row, &user, Some(format!("unsupported currency: {currency}"))).await
        }
        Err(crate::exchange_rate_config::ExchangeRateLookupError::ProviderNotConfigured(_)) => {
            // Not a real failure - this store's provider (or no provider at
            // all) simply can't price this currency on this instance, same
            // user-facing meaning as `Ok(None)` above.
            return render_create_order_page(&state, row, &user, Some(format!("unsupported currency: {currency}"))).await
        }
        Err(e) => {
            tracing::error!(store.id = %row.id, currency = ?currency, error = %e, "exchange rate lookup failed");
            return render_create_order_page(&state, row, &user, Some("Something went wrong looking up the exchange rate. Please try again.".to_string()))
                .await;
        }
    };
    let xmr_amount_piconero = match shared::exchange_rate::compute_order_amount(currency, amount, piconero_per_unit) {
        Ok(amount) => amount,
        Err(e) => return render_create_order_page(&state, row, &user, Some(e.to_string())).await,
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
        Err(message) => return render_create_order_page(&state, row, &user, Some(message)).await,
    };

    match state
        .engine_client
        .create_order(&sk, xmr_amount_piconero, merchant_order_id, Some(resolution.confirmations_required))
        .await
    {
        Ok(order) => {
            if let Err(e) = state.db.lock().create_order_currency_metadata(
                &row.id,
                &order.order_id,
                currency,
                amount,
                piconero_per_unit,
                provider,
                crate::now_unix(),
                &resolution.base_currency,
                resolution.base_currency_piconero_per_unit,
                resolution.confirmations_required,
                // The merchant's own signed-in session: as trusted as the key.
                true,
            ) {
                tracing::error!(
                    order.id = %order.order_id,
                    store.id = %row.id,
                    error = %e,
                    "failed to record local fiat metadata - the real order still exists on the engine and this \
                     response is still correct, but its fiat display on monokulo's own pages will be missing"
                );
            }
            let _ = state.db.lock().set_order_source(&row.id, &order.order_id, "dashboard");
            redirect_302(&format!("/dashboard/stores/{id}/orders/{}", order.order_id))
        }
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            render_create_order_page(&state, row, &user, Some(message)).await
        }
        Err(_) => {
            render_create_order_page(&state, row, &user, Some("Something went wrong. Please try again.".to_string())).await
        }
    }
}

#[derive(Deserialize)]
pub struct UpdateConfirmationsForm {
    pub confirmations_required: String,
    pub zero_conf_enabled: Option<String>,
    /// Distinguishes an unchecked dashboard checkbox from a numeric API update.
    #[serde(default)]
    pub zero_conf_checkbox_present: bool,
}

/// `POST /dashboard/stores/{id}/settings/confirmations` - updates the
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
    fx: FxRequest,
    Path(id): Path<String>,
    Form(form): Form<UpdateConfirmationsForm>,
) -> Response {
    const SECTION: StoreSection = StoreSection::Confirmations;
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let confirmations_required: u64 = if form.zero_conf_enabled.is_some() {
        0
    } else {
        match form.confirmations_required.trim().parse() {
            // An unchecked dashboard checkbox restores the ordinary default.
            Ok(0) if form.zero_conf_checkbox_present => 10,
            Ok(n) if n <= 720 => n,
            Ok(_) => {
                return render_store_settings_page(&state, row, &user, Some("Enter a whole number of confirmations from 0 to 720.".to_string()), None, Some((SECTION, fx))).await;
            }
            Err(_) => return render_store_settings_page(&state, row, &user, Some("Enter a whole number of confirmations.".to_string()), None, Some((SECTION, fx))).await,
        }
    };

    let sk = match decrypt_sk(&state, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let policy_lock = crate::confirmation_thresholds::policy_lock(&row.tenant_public_key);
    let _policy_guard = policy_lock.lock().await;
    match state.engine_client.set_confirmations_required(&sk, confirmations_required).await {
        Ok(_) => saved(&state, row, &user, SECTION, fx, &format!("/dashboard/stores/{id}/settings")).await,
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            render_store_settings_page(&state, row, &user, Some(message), None, Some((SECTION, fx))).await
        }
        Err(_) => {
            render_store_settings_page(&state, row, &user, Some("Something went wrong. Please try again.".to_string()), None, Some((SECTION, fx))).await
        }
    }
}

#[derive(Deserialize)]
pub struct MoveKeyStorageForm {
    pub backend: String,
    pub view_key_hex: String,
    pub spend_pubkey_hex: String,
}

/// `POST /dashboard/stores/{id}/settings/key-custody` - moves the store's
/// keys to another key custody backend (task 5.6). The engine checks the
/// keys are this store's own wallet's before anything moves; a rejection
/// re-renders the page with the reason and empty key fields.
pub async fn move_key_storage(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    fx: FxRequest,
    Path(id): Path<String>,
    Form(form): Form<MoveKeyStorageForm>,
) -> Response {
    const SECTION: StoreSection = StoreSection::KeyStorage;
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let sk = match decrypt_sk(&state, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    match state
        .engine_client
        .switch_key_custody(&sk, form.backend.trim(), form.view_key_hex.trim(), form.spend_pubkey_hex.trim())
        .await
    {
        Ok(_) => {
            // The engine's status (and so any "keys unavailable" alert) is
            // re-read on the next page rather than waiting out the cache.
            super::status_page::invalidate_status_cache(&state);
            saved(&state, row, &user, SECTION, fx, &format!("/dashboard/stores/{id}/settings#key-storage")).await
        }
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            render_store_settings_page(&state, row, &user, Some(message), None, Some((SECTION, fx))).await
        }
        Err(_) => {
            render_store_settings_page(&state, row, &user, Some("Something went wrong moving the keys. Check where they are kept below, and try again if they haven't moved.".to_string()), None, Some((SECTION, fx)))
                .await
        }
    }
}

#[derive(Deserialize)]
pub struct DiagnosticsForm {
    /// `"on"` or `"off"`.
    pub client_logging: String,
}

/// `POST /dashboard/stores/{id}/settings/diagnostics` - turns the store's
/// client logs on or off (`db::Db::client_logging`).
pub async fn update_diagnostics(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    fx: FxRequest,
    Path(id): Path<String>,
    Form(form): Form<DiagnosticsForm>,
) -> Response {
    const SECTION: StoreSection = StoreSection::Diagnostics;
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let on = form.client_logging == "on";
    let update_result = state.db.lock().set_client_logging(&row.id, on);
    match update_result {
        Ok(()) => {
            tracing::info!(store.id = %row.id, client_logging = on, "store diagnostics changed");
            saved(&state, row, &user, SECTION, fx, &format!("/dashboard/stores/{id}/settings")).await
        }
        Err(_) => {
            render_store_settings_page(&state, row, &user, Some("Something went wrong. Please try again.".to_string()), None, Some((SECTION, fx))).await
        }
    }
}

/// Every provider this instance offers, for the store settings form: the
/// store's enabled ones first in the store's own order, then the rest in the
/// instance's order. `selected` means "the store uses it".
fn fx_provider_options(available: &[&'static str], enabled: &[String]) -> Vec<views::store_settings::FxProviderOption> {
    let mut options: Vec<views::store_settings::FxProviderOption> = enabled
        .iter()
        .filter(|name| available.contains(&name.as_str()))
        .map(|name| views::store_settings::FxProviderOption { name: name.clone(), selected: true })
        .collect();
    for name in available {
        if !options.iter().any(|o| o.name == *name) {
            options.push(views::store_settings::FxProviderOption { name: name.to_string(), selected: false });
        }
    }
    options
}

/// Reads the store settings form (`use_<provider>` checkbox and
/// `position_<provider>` number per provider) into the store's ordered
/// provider list: the ticked providers, sorted by their position number,
/// with ties (and a missing or unreadable number) falling back to the order
/// the instance lists them in. A field naming a provider this instance
/// hasn't enabled is an error rather than being ignored - it means the form
/// is stale or forged, and silently dropping it would save something other
/// than what the merchant asked for.
fn parse_fx_providers_form(available: &[&'static str], form: &HashMap<String, String>) -> Result<Vec<String>, String> {
    for key in form.keys() {
        let Some(name) = key.strip_prefix("use_").or_else(|| key.strip_prefix("position_")) else { continue };
        if !available.contains(&name) {
            return Err(format!("{name:?} is not an available exchange rate provider on this instance."));
        }
    }
    let mut chosen: Vec<(u32, usize, &str)> = available
        .iter()
        .enumerate()
        .filter(|(_, name)| form.get(&format!("use_{name}")).is_some())
        .map(|(index, name)| {
            let position = form.get(&format!("position_{name}")).and_then(|p| p.trim().parse::<u32>().ok()).unwrap_or(u32::MAX);
            (position, index, *name)
        })
        .collect();
    chosen.sort();
    Ok(chosen.into_iter().map(|(_, _, name)| name.to_string()).collect())
}

/// `POST /dashboard/stores/{id}/settings/fx-provider` - a per-store
/// choice of which exchange-rate providers to use and in what order (a real
/// follow-up to `docs/fx_refactor.md`: "the FX provider should be
/// configurable on a per-store basis"). Unlike `update_confirmations_required`,
/// this never calls the engine at all - the provider list is entirely
/// monokulo's own concept (`db::StoreConnectionRow::fx_providers`), so a
/// plain local update plus a redirect is the whole handler. Every provider is
/// validated against `ExchangeRateProviders::available_providers` (this
/// instance's own real configuration) rather than accepted verbatim - a
/// merchant selecting a provider this instance has not enabled would
/// otherwise silently create orders that skip it, instead of being told
/// clearly, right here, that the choice doesn't work.
///
/// Takes effect on the very next order: order creation reads the store row
/// afresh on every request and nothing between caches it.
pub async fn update_fx_providers(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    fx: FxRequest,
    Path(id): Path<String>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    const SECTION: StoreSection = StoreSection::FxProvider;
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let providers = match parse_fx_providers_form(&state.exchange_rate.available_providers(), &form) {
        Ok(providers) => providers,
        Err(message) => return render_store_settings_page(&state, row, &user, Some(message), None, Some((SECTION, fx))).await,
    };

    // Bound to a local first, not matched on directly: a `MutexGuard`
    // temporary created in a `match` scrutinee is kept alive for every arm
    // of that match (a real Rust footgun, not an oversight) - held across
    // the `Err` arm's own `.await` below, it would make this handler's
    // future `!Send` and fail to compile as an axum route at all.
    let update_result = state.db.lock().update_store_connection_fx_providers(&row.id, &providers);
    match update_result {
        Ok(()) => saved(&state, row, &user, SECTION, fx, &format!("/dashboard/stores/{id}/settings")).await,
        Err(_) => {
            render_store_settings_page(&state, row, &user, Some("Something went wrong. Please try again.".to_string()), None, Some((SECTION, fx))).await
        }
    }
}

#[derive(Deserialize)]
pub struct UpdateBaseCurrencyForm {
    pub base_currency: String,
}

/// `POST /dashboard/stores/{id}/settings/base-currency` - selection-time
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
    fx: FxRequest,
    Path(id): Path<String>,
    Form(form): Form<UpdateBaseCurrencyForm>,
) -> Response {
    const SECTION: StoreSection = StoreSection::BaseCurrency;
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // Bound to a local first, not matched on directly - see
    // `update_fx_providers`'s own doc comment on exactly this footgun
    // (a `MutexGuard` temporary in a `match` scrutinee stays alive across
    // every arm, including one that `.await`s, which would make this
    // handler's future `!Send`).
    let resolved = crate::currencies::resolve_currency(&state.db.lock(), &form.base_currency);
    let base_currency = match resolved {
        Ok(Some(code)) => code,
        Ok(None) => {
            return render_store_settings_page(
                &state,
                row,
                &user,
                Some(format!("{:?} is not a known currency.", form.base_currency)),
                None,
            Some((SECTION, fx)),
        )
            .await;
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let policy_lock = crate::confirmation_thresholds::policy_lock(&row.tenant_public_key);
    let _policy_guard = policy_lock.lock().await;
    let update_result = state.db.lock().update_store_connection_base_currency(&row.id, &base_currency);
    match update_result {
        Ok(()) => saved(&state, row, &user, SECTION, fx, &format!("/dashboard/stores/{id}/settings")).await,
        Err(_) => {
            render_store_settings_page(&state, row, &user, Some("Something went wrong. Please try again.".to_string()), None, Some((SECTION, fx))).await
        }
    }
}

#[derive(Deserialize)]
pub struct CreateConfirmationThresholdForm {
    pub unit_amount: String,
    pub confirmations_required: String,
}

/// `POST /dashboard/stores/{id}/settings/confirmation-thresholds` - adds
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
    fx: FxRequest,
    Path(id): Path<String>,
    Form(form): Form<CreateConfirmationThresholdForm>,
) -> Response {
    const SECTION: StoreSection = StoreSection::Confirmations;
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let confirmations_required: u64 = match form.confirmations_required.trim().parse() {
        Ok(n) if n <= 720 => n,
        Ok(_) => {
            return render_store_settings_page(&state, row, &user, Some("Enter a whole number of confirmations from 0 to 720.".to_string()), None, Some((SECTION, fx)))
                .await;
        }
        Err(_) => return render_store_settings_page(&state, row, &user, Some("Enter a whole number of confirmations.".to_string()), None, Some((SECTION, fx))).await,
    };

    let unit_amount = match crate::confirmation_thresholds::ThresholdAmount::parse(&form.unit_amount) {
        Ok(amount) => amount,
        Err(_) => {
            return render_store_settings_page(&state, row, &user, Some("Enter a non-negative amount.".to_string()), None, Some((SECTION, fx))).await;
        }
    };
    let canonical_amount = unit_amount.canonical();

    let policy_lock = crate::confirmation_thresholds::policy_lock(&row.tenant_public_key);
    let _policy_guard = policy_lock.lock().await;
    let existing = match state.db.lock().list_confirmation_thresholds(&row.id) {
        Ok(rows) => rows,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if existing.iter().any(|threshold| crate::confirmation_thresholds::ThresholdAmount::parse(&threshold.unit_amount).is_err()) {
        return render_store_settings_page(&state, row, &user, Some("An existing threshold amount is invalid. Delete it before adding another.".to_string()), None, Some((SECTION, fx))).await;
    }
    if existing.iter().any(|threshold| crate::confirmation_thresholds::ThresholdAmount::parse(&threshold.unit_amount).is_ok_and(|amount| amount == unit_amount)) {
        return render_store_settings_page(&state, row, &user, Some(format!("A threshold for {canonical_amount} already exists.")), None, Some((SECTION, fx))).await;
    }
    let threshold_id = uuid::Uuid::new_v4().to_string();
    let create_result = state.db.lock().create_confirmation_threshold_with_limit(&threshold_id, &row.id, &canonical_amount, confirmations_required, crate::now_unix());
    match create_result {
        Ok(true) => saved(&state, row, &user, SECTION, fx, &format!("/dashboard/stores/{id}/settings")).await,
        Ok(false) => render_store_settings_page(&state, row, &user, Some("You can define at most 5 custom thresholds. Delete one to add another.".to_string()), None, Some((SECTION, fx))).await,
        Err(e) if e.is_unique_violation() => {
            render_store_settings_page(&state, row, &user, Some(format!("A threshold for {canonical_amount} already exists.")), None, Some((SECTION, fx)))
                .await
        }
        Err(_) => {
            render_store_settings_page(&state, row, &user, Some("Something went wrong. Please try again.".to_string()), None, Some((SECTION, fx))).await
        }
    }
}

/// `POST /dashboard/stores/{id}/settings/confirmation-thresholds/{threshold_id}/delete` -
/// the default/fallback threshold isn't one of these rows at all (it's
/// `tenants.confirmations_required`, edited via `update_confirmations_required`
/// instead), so there is no way to reach this handler for it - "cannot be
/// deleted" is true by construction, not an extra check here.
pub async fn delete_confirmation_threshold(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    fx: FxRequest,
    Path((id, threshold_id)): Path<(String, String)>,
) -> Response {
    const SECTION: StoreSection = StoreSection::Confirmations;
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let policy_lock = crate::confirmation_thresholds::policy_lock(&row.tenant_public_key);
    let _policy_guard = policy_lock.lock().await;
    if state.db.lock().delete_confirmation_threshold(&row.id, &threshold_id).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    saved(&state, row, &user, SECTION, fx, &format!("/dashboard/stores/{id}/settings")).await
}

/// Saves custom threshold additions and deletions in one SQLite transaction.
/// The default has its own form and engine update, so neither save needs to
/// coordinate writes across the control-plane and engine databases.
/// Dynamic `delete_{id}` checkbox names require a map rather than a fixed form.
pub async fn save_confirmation_thresholds(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    fx: FxRequest,
    Path(id): Path<String>,
    Form(raw): Form<HashMap<String, String>>,
) -> Response {
    const SECTION: StoreSection = StoreSection::Confirmations;
    let row = match load_owned_connection(&state, &user, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let new_unit_amount = raw.get("new_unit_amount").map(|s| s.trim()).unwrap_or("");
    let new_confirmations_text = raw.get("new_confirmations_required").map(|s| s.trim()).unwrap_or("");
    let new_confirmations = if new_unit_amount.is_empty() && new_confirmations_text.is_empty() {
        None
    } else {
        let confirmations: u64 = match new_confirmations_text.parse() {
            Ok(n) if n <= 720 => n,
            _ => return render_store_settings_page(&state, row, &user, Some("Enter a whole number of confirmations from 0 to 720 for the new threshold.".to_string()), None, Some((SECTION, fx))).await,
        };
        match crate::confirmation_thresholds::ThresholdAmount::parse(new_unit_amount) {
            Ok(_) => Some(confirmations),
            Err(_) => return render_store_settings_page(&state, row, &user, Some("Enter a non-negative amount for the new threshold.".to_string()), None, Some((SECTION, fx))).await,
        }
    };
    let canonical_new_amount = new_confirmations.map(|_| crate::confirmation_thresholds::ThresholdAmount::parse(new_unit_amount).unwrap().canonical());
    let policy_lock = crate::confirmation_thresholds::policy_lock(&row.tenant_public_key);
    let _policy_guard = policy_lock.lock().await;
    let existing = match state.db.lock().list_confirmation_thresholds(&row.id) {
        Ok(rows) => rows,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let deleted_ids: Vec<String> = existing.iter().filter(|threshold| raw.contains_key(&format!("delete_{}", threshold.id))).map(|threshold| threshold.id.clone()).collect();
    if existing.iter().any(|threshold| !deleted_ids.contains(&threshold.id) && crate::confirmation_thresholds::ThresholdAmount::parse(&threshold.unit_amount).is_err()) {
        return render_store_settings_page(&state, row, &user, Some("An existing threshold amount is invalid. Delete it before saving.".to_string()), None, Some((SECTION, fx))).await;
    }
    if new_confirmations.is_some() {
        if existing.len() - deleted_ids.len() >= 5 {
            return render_store_settings_page(&state, row, &user, Some("You can define at most 5 custom thresholds. Delete one to add another.".to_string()), None, Some((SECTION, fx))).await;
        }
        if existing.iter().any(|threshold| !deleted_ids.contains(&threshold.id) && crate::confirmation_thresholds::ThresholdAmount::parse(&threshold.unit_amount).is_ok_and(|amount| Some(amount.canonical()) == canonical_new_amount)) {
            return render_store_settings_page(&state, row, &user, Some(format!("A threshold for {} already exists.", canonical_new_amount.as_deref().unwrap())), None, Some((SECTION, fx))).await;
        }
    }

    let threshold_id = uuid::Uuid::new_v4().to_string();
    let new_threshold = new_confirmations.map(|n| (threshold_id.as_str(), canonical_new_amount.as_deref().unwrap(), n, crate::now_unix()));
    let update_result = state.db.lock().replace_confirmation_thresholds(&row.id, &deleted_ids, new_threshold);
    if !matches!(update_result, Ok(true)) {
        let message = match update_result {
            Ok(false) => "You can define at most 5 custom thresholds. Delete one to add another.".to_string(),
            Err(ref e) if e.is_unique_violation() => format!("A threshold for {} already exists.", canonical_new_amount.as_deref().unwrap_or(new_unit_amount)),
            _ => "Something went wrong. Please try again.".to_string(),
        };
        return render_store_settings_page(&state, row, &user, Some(message), None, Some((SECTION, fx))).await;
    }

    saved(&state, row, &user, SECTION, fx, &format!("/dashboard/stores/{id}/settings")).await
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::engine_client::EngineClient;
    use crate::views::orders::{OrderDetailData, OrderDetailViewModel};

    use super::super::{AppState, build_router};
    use super::{fx_provider_options, parse_fx_providers_form};
    use crate::views;
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

    async fn test_state_with_real_engine() -> (AppState, scanner_test_support::TestEngineHandle) {
        let engine = scanner_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .spawn()
            .await;
        let engine_client = EngineClient::new(format!("http://{}", engine.addr));
        let state = AppState {
            engine_client,
            ..AppState::for_tests()
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
            engine_client,
            ..AppState::for_tests()
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
            "domains": [],
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

    /// Seeds a real order directly on the spawned engine through its admin
    /// API (`POST /api/v1/admin/tenant/orders`), authenticated with the store's
    /// own `sk_` decrypted from monokulo's database - the same way monokulo
    /// itself reaches the engine. Deliberately bypasses monokulo's own order
    /// creation, so no local currency metadata exists for the order. 10.00 at
    /// `TEST_RATE_PICONERO_PER_UNIT` (1e12 piconero/USD); the engine only knows XMR.
    async fn seed_real_order(state: &AppState, engine_addr: std::net::SocketAddr, public_key: &str) -> String {
        let row = state.db.lock().get_store_connection_by_public_key(public_key).unwrap().expect("connection exists");
        let sk = crate::crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted).unwrap();
        let response = reqwest::Client::new()
            .post(format!("http://{engine_addr}/api/v1/admin/tenant/orders"))
            .bearer_auth(sk)
            .json(&serde_json::json!({ "xmr_amount_piconero": 10 * TEST_RATE_PICONERO_PER_UNIT }))
            .send()
            .await
            .expect("seeding a real order against the engine's admin API failed");
        assert_eq!(response.status(), reqwest::StatusCode::OK, "expected the engine to accept the seeded order");
        let body: serde_json::Value = response.json().await.unwrap();
        body.as_object().unwrap().get("order_id").unwrap().as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn orders_list_shows_a_real_order_seeded_against_the_engines_public_api() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token =
            signed_up_and_logged_in_session_token(&router, "orders-owner@example.com", "correct horse battery staple")
                .await;
        let (connection_id, public_key) = create_connection(&router, &session_token).await;

        let order_id = seed_real_order(&state, engine.addr, &public_key).await;

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{connection_id}/orders"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains(&order_id), "expected the seeded order's order_id in the response, got: {html}");
        assert!(html.contains(r##"fx-target="#orders-results" fx-push-url"##), "the search is enhanced: {html}");

        // A fixi search gets only the results, filtered.
        let search = |q: &str| {
            fixi(
                Request::builder()
                    .uri(format!("/dashboard/stores/{connection_id}/orders?q={q}"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
        };
        let html = body_text(router.clone().oneshot(search(&order_id)).await.unwrap()).await;
        assert!(html.starts_with(r#"<div id="orders-results">"#) && html.contains(&order_id), "{html}");
        let html = body_text(router.clone().oneshot(search("no-such-order")).await.unwrap()).await;
        assert!(html.contains("No orders match") && !html.contains("<html"), "{html}");
    }

    /// With JavaScript, the order detail page streams its changing part
    /// (structured_logging.md part 6): the page names the stream, and the
    /// stream starts with that part, routed to replace itself.
    #[tokio::test]
    async fn order_detail_streams_its_live_part_to_its_owner_only() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(&router, "order-events@example.com", "correct horse battery staple").await;
        let (connection_id, public_key) = create_connection(&router, &session_token).await;
        let order_id = seed_real_order(&state, engine.addr, &public_key).await;
        let get = |uri: String, token: &str| {
            Request::builder().uri(uri).header("authorization", format!("Bearer {token}")).header("host", "test.example").body(Body::empty()).unwrap()
        };

        let page = body_text(router.clone().oneshot(get(format!("/dashboard/stores/{connection_id}/orders/{order_id}"), &session_token)).await.unwrap()).await;
        let events_url = format!("/dashboard/stores/{connection_id}/orders/{order_id}/events");
        assert!(page.contains(&format!(r#"fx-action="{events_url}" fx-trigger="fx:inited""#)), "{page}");
        assert!(page.contains(r#"<div id="order-live">"#), "{page}");

        let response = router.clone().oneshot(get(events_url.clone(), &session_token)).await.unwrap();
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let mut body = response.into_body();
        let mut text = String::new();
        while !text.contains("\n\n") {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(10), body.frame()).await.unwrap().unwrap().unwrap();
            if let Some(data) = frame.data_ref() {
                text.push_str(std::str::from_utf8(data).unwrap());
            }
        }
        assert!(text.starts_with(r##"event: {"target":"#order-live","swap":"outerHTML"}"##), "{text}");
        assert!(text.contains(r#"data: <div id="order-live">"#) && text.contains(&order_id), "{text}");

        let stranger = signed_up_and_logged_in_session_token(&router, "order-events-stranger@example.com", "correct horse battery staple").await;
        let response = router.oneshot(get(events_url, &stranger)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "someone else's store");
    }

    #[tokio::test]
    async fn order_detail_shows_the_seeded_orders_full_detail() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "order-detail-owner@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) = create_connection(&router, &session_token).await;

        let order_id = seed_real_order(&state, engine.addr, &public_key).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{connection_id}/orders/{order_id}"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .header("host", "test.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains(&order_id), "expected the order's order_id in its detail page, got: {html}");
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
        assert!(!html.contains(r#"http-equiv="refresh""#), "a point-in-time page without JS, never a meta refresh: {html}");
        assert!(html.contains(r#"class="btn btn-secondary reload""#), "a Reload button instead: {html}");
        // The real point of this follow-up: a real, absolute, shareable
        // payment link for this exact order, built from the request's own
        // Host header (`test.example` here, set by `oneshot`'s default) -
        // not a placeholder or a bare relative path.
        assert!(
            html.contains(&format!("http://test.example/pay/{public_key}/orders/{order_id}/share")),
            "expected a real absolute payment link, got: {html}"
        );
        // The share icon and order id live in the order title.
        assert!(html.contains(r#"<h1 class="order-title">"#), "expected the title banner to carry the share button, got: {html}");
        assert!(
            html.contains(r#"<code class="order-title-id"><span class="mid-ellipsis""#) && html.contains(&format!(r#"title="{order_id}""#)),
            "expected the order id inside the title banner, got: {html}"
        );
        assert!(
            html.contains(r#"id="share-payment-link""#) && html.contains("aria-label=\"Share payment link\""),
            "expected a real, labeled share button, got: {html}"
        );
        assert!(!html.contains("<th>Payment link</th>"), "the payment link must no longer be its own table row, got: {html}");
    }

    #[tokio::test]
    async fn order_detail_for_an_unknown_order_id_renders_a_clear_not_found_state() {
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
                    .uri(format!("/dashboard/stores/{connection_id}/orders/no-such-payment-id"))
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
                    .uri(format!("/dashboard/stores/{connection_id}/settings"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("No webhooks yet."), "expected a real, valid settings page even with no webhooks, got: {html}");
    }

    #[tokio::test]
    async fn a_different_user_hitting_the_first_users_connection_gets_404_not_their_data() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let owner_token =
            signed_up_and_logged_in_session_token(&router, "cross-user-owner@example.com", "correct horse battery staple")
                .await;
        let (connection_id, public_key) = create_connection(&router, &owner_token).await;
        seed_real_order(&state, engine.addr, &public_key).await;

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
                    .uri(format!("/dashboard/stores/{connection_id}/orders"))
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
            format!("/dashboard/stores/{fake_id}/orders"),
            format!("/dashboard/stores/{fake_id}/orders/some-payment-id"),
            format!("/dashboard/stores/{fake_id}/settings"),
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
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(&router, "store-detail@example.com", "correct horse battery staple").await;
        let (connection_id, public_key) = create_connection(&router, &session_token).await;
        let order_id = seed_real_order(&state, engine.addr, &public_key).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{connection_id}"))
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
        assert!(html.contains(&order_id), "expected the seeded order in the recent-orders list, got: {html}");
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

        for uri in [format!("/dashboard/stores/{connection_id}"), "/dashboard/stores/nonexistent".to_string()] {
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

    fn form(fields: &[(&str, &str)]) -> HashMap<String, String> {
        fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn the_provider_form_yields_the_ticked_providers_in_position_order() {
        let available = ["coingecko", "coinmarketcap"];
        let both_swapped =
            form(&[("use_coingecko", "on"), ("position_coingecko", "2"), ("use_coinmarketcap", "on"), ("position_coinmarketcap", "1")]);
        assert_eq!(parse_fx_providers_form(&available, &both_swapped).unwrap(), vec!["coinmarketcap", "coingecko"]);

        // An unticked provider is off whatever its position says.
        let one_off = form(&[("use_coinmarketcap", "on"), ("position_coinmarketcap", "1"), ("position_coingecko", "1")]);
        assert_eq!(parse_fx_providers_form(&available, &one_off).unwrap(), vec!["coinmarketcap"]);

        // Ties and unreadable positions fall back to the instance's order.
        let tied = form(&[("use_coingecko", "on"), ("position_coingecko", "1"), ("use_coinmarketcap", "on"), ("position_coinmarketcap", "1")]);
        assert_eq!(parse_fx_providers_form(&available, &tied).unwrap(), vec!["coingecko", "coinmarketcap"]);
        let junk = form(&[("use_coingecko", "on"), ("position_coingecko", "abc"), ("use_coinmarketcap", "on"), ("position_coinmarketcap", "3")]);
        assert_eq!(parse_fx_providers_form(&available, &junk).unwrap(), vec!["coinmarketcap", "coingecko"]);

        assert!(parse_fx_providers_form(&available, &form(&[])).unwrap().is_empty(), "everything unticked turns every provider off");
    }

    #[test]
    fn the_provider_form_rejects_a_provider_the_instance_does_not_offer() {
        let err = parse_fx_providers_form(&["coingecko"], &form(&[("use_haveno", "on")])).unwrap_err();
        assert!(err.contains("not an available exchange rate provider"), "{err}");
        assert!(parse_fx_providers_form(&["coingecko"], &form(&[("position_haveno", "1")])).is_err());
    }

    #[test]
    fn the_provider_options_list_the_stores_choices_first_in_its_order_then_the_rest() {
        let names = |options: Vec<views::store_settings::FxProviderOption>| -> Vec<(String, bool)> {
            options.into_iter().map(|o| (o.name, o.selected)).collect()
        };
        let available = ["coingecko", "coinmarketcap"];
        assert_eq!(
            names(fx_provider_options(&available, &["coinmarketcap".to_string()])),
            vec![("coinmarketcap".to_string(), true), ("coingecko".to_string(), false)]
        );
        // A stale name the instance no longer offers is dropped.
        assert_eq!(
            names(fx_provider_options(&available, &["fixed".to_string(), "coingecko".to_string()])),
            vec![("coingecko".to_string(), true), ("coinmarketcap".to_string(), false)]
        );
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
                &format!("/dashboard/stores/{connection_id}/settings/webhooks"),
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

    /// The merchant's own server receives the payment notification: a
    /// webhook added on the settings page with a custom Authorization header
    /// (their endpoint rejects anything without it) gets `order.paid` when the
    /// customer pays, carrying that header and a signature that verifies with
    /// the secret the page showed once.
    #[tokio::test]
    async fn a_merchants_webhook_endpoint_receives_the_paid_notification_with_its_custom_header_and_a_valid_signature() {
        use axum::http::HeaderMap;
        type Received = std::sync::Arc<parking_lot::Mutex<Vec<(HeaderMap, String)>>>;
        let received: Received = Default::default();
        let app = Router::new().route("/hook", axum::routing::post(|axum::extract::State(received): axum::extract::State<Received>, headers: HeaderMap, body: String| async move {
            received.lock().push((headers, body));
            StatusCode::OK
        })).with_state(received.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hook = format!("http://{}/hook", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let engine = scanner_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet]).with_background_loops().without_background_scan_loop().spawn().await;
        let state = AppState {
            engine_client: EngineClient::new(format!("http://{}", engine.addr)),
            ..AppState::for_tests()
        };
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(&router, "webhook-delivery@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;
        let html = body_text(router.clone().oneshot(form_post_request(&format!("/dashboard/stores/{connection_id}/settings/webhooks"),
            &session_token, &[("url", &hook), ("extra_headers", "Authorization: Bearer shop-endpoint-secret")])).await.unwrap()).await;
        let secret = html.split("<pre>").nth(1).and_then(|rest| rest.split("</pre>").next()).expect("the signing secret is shown once").to_string();

        let response = router.clone().oneshot(form_post_request(&format!("/dashboard/stores/{connection_id}/orders/new"),
            &session_token, &[("amount", "0.5"), ("currency", "XMR"), ("merchant_order_id", "wc-1042")])).await.unwrap();
        let order_id = response.headers()["location"].to_str().unwrap().rsplit('/').next().unwrap().to_string();
        engine.mark_order_paid(&order_id).unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let (headers, body) = loop {
            if let Some(first) = received.lock().first().cloned() { break first; }
            assert!(std::time::Instant::now() < deadline, "no webhook delivered");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        assert_eq!(headers["authorization"], "Bearer shop-endpoint-secret");
        assert_eq!(headers["x-monokulo-event"], "order.paid");
        assert_eq!(headers["x-monokulo-signature"].to_str().unwrap(), shared::webhook_sign::sign_payload(&secret, body.as_bytes()));
        let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(payload["order_id"], order_id.as_str(), "{payload}");
        assert_eq!(headers["x-monokulo-event-id"].to_str().unwrap(), payload["event_id"].as_str().unwrap());
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
                &format!("/dashboard/stores/{connection_id}/settings/webhooks"),
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
                    .uri(format!("/dashboard/stores/{connection_id}/settings"))
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
                &format!("/dashboard/stores/{connection_id}/settings/webhooks"),
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
                    .uri(format!("/dashboard/stores/{connection_id}/settings"))
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
            .oneshot(form_post_request(&format!("/dashboard/stores/{connection_id}/settings/webhooks"), &session_token, &[("url", "")]))
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
                &format!("/dashboard/stores/{connection_id}/settings/webhooks"),
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
                &format!("/dashboard/stores/{connection_id}/settings/webhooks"),
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
                    .uri(format!("/dashboard/stores/{connection_id}/settings"))
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
                &format!("/dashboard/stores/{connection_id}/settings/webhooks/{webhook_id}/delete"),
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
                    .uri(format!("/dashboard/stores/{connection_id}/settings"))
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
                &format!("/dashboard/stores/{connection_id}/orders/new"),
                &session_token,
                &[("amount", "10.00"), ("currency", TEST_CURRENCY), ("merchant_order_id", "order-5678")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND, "expected a redirect to the new order's own detail page");
        let location = response.headers().get("location").unwrap().to_str().unwrap().to_string();
        assert!(
            location.starts_with(&format!("/dashboard/stores/{connection_id}/orders/")),
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
            html.contains("1 XMR per 1 XMR"),
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
                &format!("/dashboard/stores/{connection_id}/orders/new"),
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
                &format!("/dashboard/stores/{connection_id}/orders/new"),
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
                &format!("/dashboard/stores/{connection_id}/orders/new"),
                &intruder_token,
                &[("amount", "10.00"), ("currency", TEST_CURRENCY)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// A real engine offering two key custody backends, with monokulo's
    /// status cache already holding its status (as it would after any page
    /// load), so forms know the choices.
    async fn test_state_with_two_custody_backends() -> (AppState, scanner_test_support::TestEngineHandle) {
        let engine = scanner_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .with_two_custody_backends()
            .spawn()
            .await;
        let (mut state, _unused_engine) = test_state_with_real_engine().await;
        state.engine_client = EngineClient::new(format!("http://{}", engine.addr));
        crate::http::status_page::get_status_cached(&state).await.expect("engine status");
        (state, engine)
    }

    async fn get_page(router: &Router, session_token: &str, uri: &str) -> String {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
    }

    async fn engine_backend_of(state: &AppState, public_key: &str) -> Option<String> {
        let row = state.db.lock().get_store_connection_by_public_key(public_key).unwrap().unwrap();
        let sk = crate::crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted).unwrap();
        state.engine_client.get_tenant(&sk).await.unwrap().key_custody_backend
    }

    #[tokio::test]
    async fn a_store_can_move_its_keys_to_another_backend_from_its_settings_page_without_js() {
        let (state, _engine) = test_state_with_two_custody_backends().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(&router, "key-storage@example.com", "correct horse battery staple").await;
        let (connection_id, public_key) = create_connection(&router, &session_token).await;
        assert_eq!(engine_backend_of(&state, &public_key).await.as_deref(), Some("plain"));

        let settings_uri = format!("/dashboard/stores/{connection_id}/settings");
        let html = get_page(&router, &session_token, &settings_uri).await;
        assert!(html.contains(r#"<section id="key-storage"><h2>Key storage</h2>"#), "{html}");
        assert!(html.contains("In the engine (simplest)"), "the current place is described: {html}");
        assert!(html.contains(r#"<option value="socket" selected>"#), "the other backend is offered: {html}");
        assert!(!html.contains(TEST_VIEW_KEY_HEX), "keys are never echoed back");

        // Keys of another wallet are refused, and nothing moves.
        let wrong_view_key = format!("01{}", "0".repeat(62));
        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("{settings_uri}/key-custody"),
                &session_token,
                &[("backend", "socket"), ("view_key_hex", wrong_view_key.as_str()), ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(html.contains("different wallet"), "{html}");
        assert!(!html.contains(&wrong_view_key), "keys are never echoed back, even on an error");
        assert_eq!(engine_backend_of(&state, &public_key).await.as_deref(), Some("plain"));

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("{settings_uri}/key-custody"),
                &session_token,
                &[("backend", "socket"), ("view_key_hex", TEST_VIEW_KEY_HEX), ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.headers().get("location").unwrap(), &format!("{settings_uri}#key-storage"));
        assert_eq!(engine_backend_of(&state, &public_key).await.as_deref(), Some("socket"));

        // Straight after the move (which empties the status cache), as the
        // redirect lands.
        let html = get_page(&router, &session_token, &settings_uri).await;
        assert!(html.contains("In a separate key storage service"), "{html}");
        assert!(html.contains(r#"<option value="plain" selected>"#), "and it can move back: {html}");

        // Still takes orders from its new place.
        let order_id = seed_real_order(&state, _engine.addr, &public_key).await;
        assert!(!order_id.is_empty());
    }

    #[tokio::test]
    async fn a_new_store_can_choose_where_its_keys_are_kept_when_there_is_a_choice() {
        let (state, _engine) = test_state_with_two_custody_backends().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(&router, "key-choice@example.com", "correct horse battery staple").await;

        let html = get_page(&router, &session_token, "/dashboard/connect").await;
        assert!(html.contains(r#"<select name="key_custody_backend">"#), "{html}");
        assert!(html.contains(r#"<option value="plain" selected>"#), "the default is preselected: {html}");

        let response = router
            .clone()
            .oneshot(form_post_request(
                "/dashboard/connect",
                &session_token,
                &[
                    ("site_url", "https://kept-apart.example.com"),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                    ("network", "mainnet"),
                    ("base_currency", "XMR"),
                    ("key_custody_backend", "socket"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let public_key = {
            let db = state.db.lock();
            let user = db.get_user_by_email("key-choice@example.com").unwrap().unwrap();
            db.list_store_connections_for_user(&user.id).unwrap()[0].tenant_public_key.clone()
        };
        assert_eq!(engine_backend_of(&state, &public_key).await.as_deref(), Some("socket"));
    }

    #[tokio::test]
    async fn with_a_single_key_storage_backend_no_choice_is_shown() {
        let (state, _engine) = test_state_with_real_engine().await;
        crate::http::status_page::get_status_cached(&state).await.unwrap();
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(&router, "no-choice@example.com", "correct horse battery staple").await;
        let (connection_id, _) = create_connection(&router, &session_token).await;
        assert!(!get_page(&router, &session_token, "/dashboard/connect").await.contains("key_custody_backend"));
        assert!(!get_page(&router, &session_token, &format!("/dashboard/stores/{connection_id}/settings")).await.contains("Key storage"));
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
                &format!("/dashboard/stores/{connection_id}/settings/confirmations"),
                &session_token,
                &[("confirmations_required", "3")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND, "expected a redirect back to the settings page");
        assert_eq!(
            response.headers().get("location").unwrap(),
            &format!("/dashboard/stores/{connection_id}/settings"),
        );

        let settings_page = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{connection_id}/settings"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(settings_page).await;
        assert!(html.contains(r#"value="3""#), "expected the real, updated confirmation threshold shown, got: {html}");
    }

    #[tokio::test]
    async fn updating_the_confirmation_threshold_to_zero_is_accepted() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "confirmations-zero@example.com", "correct horse battery staple")
                .await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router.clone()
            .oneshot(form_post_request(
                &format!("/dashboard/stores/{connection_id}/settings/confirmations"),
                &session_token,
                &[("confirmations_required", "0")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let settings = router.oneshot(Request::builder().uri(format!("/dashboard/stores/{connection_id}/settings")).header("cookie", format!("session={session_token}")).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(settings.status(), StatusCode::OK);
        let html = body_text(settings).await;
        assert!(html.contains("name=\"zero_conf_enabled\" checked"), "expected native 0-conf to be enabled: {html}");
    }

    #[tokio::test]
    async fn a_new_store_defaults_to_coingecko_and_offers_no_options_when_none_are_enabled() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "fx-provider-default@example.com", "correct horse battery staple")
                .await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let settings_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{connection_id}/settings"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let settings_html = body_text(settings_response).await;
        // Every new store is created with `fx_providers = ["coingecko"]` (the
        // only real provider left - see `Db::create_store_connection`), shown
        // as this store's current choice even though this test's instance
        // (`test_exchange_rate_provider()`, `xmr_only()`) never actually
        // enabled it - a store's own setting and what an instance currently
        // offers are two different things.
        assert!(settings_html.contains("Exchange rate provider"), "expected the settings section present, got: {settings_html}");
        // The dropdown itself must offer zero real `<option>`s - nothing is
        // enabled on this instance, and there is no "fixed" to fall back to
        // any more.
        assert!(!settings_html.contains(r#"<option value="coingecko""#), "expected no coingecko <option> since this instance never enabled it, got: {settings_html}");
        assert!(!settings_html.contains(r#"<option value="fixed""#), "the removed \"fixed\" provider must never appear as a real option, got: {settings_html}");

        // The real point of this follow-up: with no fiat provider available
        // at all, the "create an order" currency field must be a plain
        // readonly "XMR" field, not a one-option `<select>` (a dropdown with
        // nothing to actually choose between is misleading busywork).
        let create_order_response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{connection_id}/orders/new"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let create_order_html = body_text(create_order_response).await;
        assert!(
            create_order_html.contains(r#"<input type="text" id="currency" name="currency" value="XMR" readonly>"#),
            "expected a readonly XMR currency field, got: {create_order_html}"
        );
        assert!(!create_order_html.contains(r#"<select id="currency""#), "expected no currency dropdown when only XMR is available, got: {create_order_html}");
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
                    .uri(format!("/dashboard/stores/{connection_id}/orders/new"))
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
    async fn choosing_an_fx_provider_the_instance_has_not_enabled_is_a_clear_error_not_silently_accepted() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "fx-provider-invalid@example.com", "correct horse battery staple")
                .await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/stores/{connection_id}/settings/fx-provider"),
                &session_token,
                &[("use_coingecko", "on"), ("position_coingecko", "1")],
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

    fn fixi(mut request: Request<Body>) -> Request<Body> {
        request.headers_mut().insert("FX-Request", "true".parse().unwrap());
        request
    }

    /// With fixi, every store settings form answers with just its own
    /// section (structured_logging.md part 6): saved state, errors inside
    /// it and focused, and any other section the save changed, out of band.
    #[tokio::test]
    async fn store_settings_forms_answer_fixi_with_their_section() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(&router, "settings-sections@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;
        let url = |path: &str| format!("/dashboard/stores/{connection_id}/settings/{path}");
        let send = |path: &str, fields: &[(&str, &str)]| router.clone().oneshot(fixi(form_post_request(&url(path), &session_token, fields)));

        let response = send("base-currency", &[("base_currency", "EUR")]).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.starts_with(r#"<section id="base-currency">"#), "{html}");
        assert!(html.contains(r#"<section id="confirmation-thresholds" data-fx-oob>"#), "thresholds change with it: {html}");
        assert!(html.contains("Amount (EUR)"), "{html}");
        assert!(!html.contains("<html") && !html.contains(r#"id="webhooks""#), "{html}");

        let response = send("confirmations", &[("zero_conf_checkbox_present", "true"), ("confirmations_required", "abc")]).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let html = body_text(response).await;
        assert!(html.starts_with(r#"<section id="confirmation-thresholds">"#), "{html}");
        assert!(html.contains(r#"<div class="error" role="alert" data-fx-focus tabindex="-1">Enter a whole number"#), "the error is in the section: {html}");

        let response = send("webhooks", &[("url", "https://hooks.example.com/monokulo")]).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.starts_with(r#"<section id="webhooks">"#) && html.contains("Webhook created"), "the secret shows once: {html}");

        let response = send("domains", &[("domain", "shop.example")]).await.unwrap();
        let html = body_text(response).await;
        assert!(html.starts_with(r#"<section id="verified-domains">"#) && html.contains("shop.example"), "{html}");

        // Without fixi: the same error is at the top of the whole page, with
        // a link to its form.
        let response = router
            .clone()
            .oneshot(form_post_request(&url("confirmations"), &session_token, &[("zero_conf_checkbox_present", "true"), ("confirmations_required", "abc")]))
            .await
            .unwrap();
        let html = body_text(response).await;
        assert!(html.contains(r##"Enter a whole number of confirmations. <a href="#confirmation-thresholds">Go to the form</a>"##), "{html}");
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
                &format!("/dashboard/stores/{connection_id}/settings/base-currency"),
                &session_token,
                &[("base_currency", "EUR")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);

        let page = router.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{connection_id}/settings"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(page).await;
        assert!(html.contains(r#"<option value="EUR" selected>"#), "expected EUR marked selected, got: {html}");
    }

    /// The default form owns the zero-conf checkbox; custom tiers save separately.
    #[tokio::test]
    async fn ticking_the_zero_conf_checkbox_persists_and_shows_as_checked_on_the_settings_page() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let session_token =
            signed_up_and_logged_in_session_token(&router, "zero-conf-update@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/stores/{connection_id}/settings/confirmations"),
                &session_token,
                &[("confirmations_required", "10"), ("zero_conf_enabled", "on")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);

        let page = router.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{connection_id}/settings"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(page).await;
        assert!(
            html.contains(r#"<input type="checkbox" name="zero_conf_enabled" checked form="default-confirmations">"#),
            "expected the 0-conf checkbox to show as checked once enabled, got: {html}"
        );

        // The browser still submits the visible numeric value (0) when the
        // merchant unchecks the box. That action must restore confirmations.
        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/stores/{connection_id}/settings/confirmations"),
                &session_token,
                &[("confirmations_required", "0"), ("zero_conf_checkbox_present", "true")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let page = router
            .oneshot(
                Request::builder()
                    .uri(format!("/dashboard/stores/{connection_id}/settings"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(page).await;
        assert!(html.contains(r#"name="confirmations_required" value="10""#), "expected the ordinary default after unchecking: {html}");
        assert!(html.contains(r#"<input type="checkbox" name="zero_conf_enabled" form="default-confirmations">"#), "expected 0-conf to be disabled: {html}");
    }

    #[tokio::test]
    async fn custom_confirmation_thresholds_save_without_contacting_the_engine() {
        let (mut state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let token = signed_up_and_logged_in_session_token(&router, "local-policy-save@example.com", "correct horse battery staple").await;
        let (connection_id, _) = create_connection(&router, &token).await;
        state.db.lock().create_confirmation_threshold("old", &connection_id, "50", 20, crate::now_unix()).unwrap();
        // A custom-tier save must not depend on engine availability.
        state.engine_client = EngineClient::new("http://127.0.0.1:0");
        let response = build_router(state.clone()).oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds/save"),
            &token,
            &[("delete_old", "on"), ("new_unit_amount", "100"), ("new_confirmations_required", "30")],
        )).await.unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let thresholds = state.db.lock().list_confirmation_thresholds(&connection_id).unwrap();
        assert_eq!(thresholds.len(), 1);
        assert_eq!(thresholds[0].unit_amount, "100");
        assert_eq!(thresholds[0].confirmations_required, 30);
    }

    #[tokio::test]
    async fn default_confirmation_save_leaves_custom_thresholds_unchanged() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let token = signed_up_and_logged_in_session_token(&router, "default-policy-save@example.com", "correct horse battery staple").await;
        let (connection_id, _) = create_connection(&router, &token).await;
        state.db.lock().create_confirmation_threshold("keep", &connection_id, "50", 20, crate::now_unix()).unwrap();
        let response = router.oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/settings/confirmations"),
            &token,
            &[("confirmations_required", "10"), ("zero_conf_enabled", "on"), ("zero_conf_checkbox_present", "true")],
        )).await.unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let row = state.db.lock().get_store_connection_by_id(&connection_id).unwrap().unwrap();
        let sk = crate::crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted).unwrap();
        assert_eq!(state.engine_client.get_tenant(&sk).await.unwrap().confirmations_required, 0);
        let thresholds = state.db.lock().list_confirmation_thresholds(&connection_id).unwrap();
        assert_eq!(thresholds.len(), 1);
        assert_eq!(thresholds[0].id, "keep");
        assert_eq!(thresholds[0].confirmations_required, 20);
    }

    #[tokio::test]
    async fn custom_confirmation_save_fails_closed_when_thresholds_cannot_be_read() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let token = signed_up_and_logged_in_session_token(&router, "broken-policy-save@example.com", "correct horse battery staple").await;
        let (connection_id, _) = create_connection(&router, &token).await;
        state.db.lock().break_confirmation_thresholds_for_test();
        let response = router.oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds/save"), &token, &[],
        )).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let row = state.db.lock().get_store_connection_by_id(&connection_id).unwrap().unwrap();
        let sk = crate::crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted).unwrap();
        assert_eq!(state.engine_client.get_tenant(&sk).await.unwrap().confirmations_required, 10);
    }

    #[tokio::test]
    async fn invalid_new_threshold_does_not_publish_zero_conf_or_delete_existing_rows() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(&router, "invalid-threshold-save@example.com", "correct horse battery staple").await;
        let (connection_id, _) = create_connection(&router, &session_token).await;
        let existing_id = "existing-threshold";
        state.db.lock().create_confirmation_threshold(existing_id, &connection_id, "50", 20, crate::now_unix()).unwrap();
        let delete_field = format!("delete_{existing_id}");
        let response = router.oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds/save"),
            &session_token,
            &[("confirmations_required", "10"), ("zero_conf_enabled", "on"), (delete_field.as_str(), "on"), ("new_unit_amount", "100"), ("new_confirmations_required", "oops")],
        )).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let row = state.db.lock().get_store_connection_by_id(&connection_id).unwrap().unwrap();
        let sk = crate::crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted).unwrap();
        assert_eq!(state.engine_client.get_tenant(&sk).await.unwrap().confirmations_required, 10);
        let thresholds = state.db.lock().list_confirmation_thresholds(&connection_id).unwrap();
        assert_eq!(thresholds.len(), 1);
        assert_eq!(thresholds[0].id, existing_id);
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
                &format!("/dashboard/stores/{connection_id}/settings/base-currency"),
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
                &format!("/dashboard/stores/{connection_id}/settings/base-currency"),
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
                &format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds"),
                &session_token,
                &[("unit_amount", "50.00"), ("confirmations_required", "20")],
            ))
            .await
            .unwrap();
        assert_eq!(state.db.lock().count_confirmation_thresholds(&connection_id).unwrap(), 1);

        router
            .oneshot(form_post_request(
                &format!("/dashboard/stores/{connection_id}/settings/base-currency"),
                &session_token,
                &[("base_currency", "EUR")],
            ))
            .await
            .unwrap();
        assert_eq!(
            state.db.lock().count_confirmation_thresholds(&connection_id).unwrap(),
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
                &format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds"),
                &session_token,
                &[("unit_amount", "50.00"), ("confirmations_required", "20")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);

        let threshold_id = state.db.lock().list_confirmation_thresholds(&connection_id).unwrap()[0].id.clone();

        let page = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{connection_id}/settings"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(page).await;
        assert!(html.contains(">50</td>"), "expected the canonical threshold amount shown, got: {html}");
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
                &format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds/save"),
                &session_token,
                &[("confirmations_required", "10"), (delete_field.as_str(), "on")],
            ))
            .await
            .unwrap();
        assert_eq!(delete_response.status(), StatusCode::FOUND);
        assert_eq!(
            state.db.lock().count_confirmation_thresholds(&connection_id).unwrap(),
            0,
            "expected the threshold to be gone"
        );

        let after = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{connection_id}/settings"))
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
    async fn equivalent_decimal_thresholds_are_rejected_by_both_forms() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(&router, "threshold-decimal-alias@example.com", "correct horse battery staple").await;
        let (connection_id, _) = create_connection(&router, &session_token).await;
        let direct_path = format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds");
        let response = router.clone().oneshot(form_post_request(&direct_path, &session_token, &[("unit_amount", "50"), ("confirmations_required", "20")])).await.unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let response = router.clone().oneshot(form_post_request(&direct_path, &session_token, &[("unit_amount", "50.0"), ("confirmations_required", "0")])).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_text(response).await.contains("already exists"));
        let response = router.clone().oneshot(form_post_request(&direct_path, &session_token, &[("unit_amount", "5e1"), ("confirmations_required", "0")])).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = router.oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds/save"),
            &session_token,
            &[("new_unit_amount", "50.00"), ("new_confirmations_required", "0")],
        )).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_text(response).await.contains("already exists"));
        let rows = state.db.lock().list_confirmation_thresholds(&connection_id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].unit_amount, "50");
        assert_eq!(rows[0].confirmations_required, 20);
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
                    &format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds"),
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
                    .uri(format!("/dashboard/stores/{connection_id}/settings"))
                    .header("authorization", format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(page).await;
        let pos_10 = html.find(">10</td>").expect("canonical 10 shown");
        let pos_50 = html.find(">50</td>").expect("canonical 50 shown");
        let pos_100 = html.find(">100</td>").expect("canonical 100 shown");
        assert!(pos_10 < pos_50 && pos_50 < pos_100, "expected ascending amount order, got: {html}");
    }

    #[tokio::test]
    async fn a_sixth_custom_threshold_is_rejected_until_one_is_deleted_in_the_same_save() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token =
            signed_up_and_logged_in_session_token(&router, "threshold-max-five@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;
        // The settings page's one form: its "new threshold" fields, and a
        // delete checkbox per existing row.
        let save = format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds/save");
        for i in 1..=5 {
            let response = router.clone().oneshot(form_post_request(&save, &session_token,
                &[("new_unit_amount", &format!("{i}0.00")), ("new_confirmations_required", "15")])).await.unwrap();
            assert_eq!(response.status(), StatusCode::FOUND, "expected threshold {i} to be accepted");
        }

        let response = router.clone().oneshot(form_post_request(&save, &session_token,
            &[("new_unit_amount", "999.00"), ("new_confirmations_required", "15")])).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a rejected sixth threshold re-renders the page, it doesn't redirect");
        let html = body_text(response).await;
        assert!(html.contains("at most 5 custom thresholds"), "expected a clear rejection message, got: {html}");

        // Ticking one row's delete box while adding the new one fits.
        let doomed = state.db.lock().list_confirmation_thresholds(&connection_id).unwrap()[0].clone();
        let response = router.oneshot(form_post_request(&save, &session_token,
            &[(&format!("delete_{}", doomed.id), "on"), ("new_unit_amount", "999.00"), ("new_confirmations_required", "15")])).await.unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let amounts: Vec<String> = state.db.lock().list_confirmation_thresholds(&connection_id).unwrap().into_iter().map(|t| t.unit_amount).collect();
        assert_eq!(amounts.len(), 5);
        assert!(amounts.contains(&"999".to_string()) && !amounts.contains(&doomed.unit_amount), "{amounts:?}");
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
                &format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds"),
                &session_token,
                &[("unit_amount", "50.00"), ("confirmations_required", "20")],
            ))
            .await
            .unwrap();

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds"),
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
    async fn a_negative_or_out_of_range_new_threshold_is_rejected() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token =
            signed_up_and_logged_in_session_token(&router, "threshold-negative@example.com", "correct horse battery staple").await;
        let (connection_id, _public_key) = create_connection(&router, &session_token).await;
        let save = format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds/save");

        for (amount, confirmations, message) in [
            ("-5.00", "20", "non-negative"),
            ("50.00", "721", "from 0 to 720"),
            ("50.00", "", "from 0 to 720"),
        ] {
            let response = router.clone().oneshot(form_post_request(&save, &session_token,
                &[("new_unit_amount", amount), ("new_confirmations_required", confirmations)])).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{amount}/{confirmations} re-renders the page, it doesn't redirect");
            let html = body_text(response).await;
            assert!(html.contains(message), "{amount}/{confirmations}: expected {message:?}, got: {html}");
        }
        assert_eq!(state.db.lock().count_confirmation_thresholds(&connection_id).unwrap(), 0);
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
                &format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds"),
                &session_token,
                &[("unit_amount", "5.00"), ("confirmations_required", "20")],
            ))
            .await
            .unwrap();

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/stores/{connection_id}/orders/new"),
                &session_token,
                &[("amount", "10.00"), ("currency", TEST_CURRENCY)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let location = response.headers().get("location").unwrap().to_str().unwrap().to_string();
        let order_id = location.rsplit('/').next().unwrap().to_string();

        let store = engine.store().lock();
        let tenant_id = store.find_tenant_by_public_key(&public_key).unwrap().unwrap().id;
        let stored = store.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(
            stored.confirmations_required_override,
            Some(20),
            "a 10.00 XMR order against a 5.00-and-up threshold of 20 confirmations must use that threshold, not the default"
        );
        drop(store);

        let metadata = state.db.lock().get_order_currency_metadata(&connection_id, &order_id).unwrap().unwrap();
        assert_eq!(metadata.store_base_currency, Some("XMR".to_string()));
        assert_eq!(
            metadata.base_currency_piconero_per_unit, None,
            "the order's own currency already was the base currency, so no second conversion rate exists to snapshot"
        );
        assert_eq!(metadata.confirmations_required_applied, Some(20));
        assert!(metadata.created_with_key, "a dashboard order is the merchant's own, as trusted as the key");

        // The whole point of the snapshot (WBS: "makes it clear how the
        // confirmation threshold was decided") - the order's own detail
        // page must actually show it, not just record it in the database.
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
                &format!("/dashboard/stores/{connection_id}/settings/confirmation-thresholds"),
                &session_token,
                &[("unit_amount", "50.00"), ("confirmations_required", "99")],
            ))
            .await
            .unwrap();

        let response = router
            .oneshot(form_post_request(
                &format!("/dashboard/stores/{connection_id}/orders/new"),
                &session_token,
                &[("amount", "10.00"), ("currency", TEST_CURRENCY)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let location = response.headers().get("location").unwrap().to_str().unwrap().to_string();
        let order_id = location.rsplit('/').next().unwrap().to_string();

        let store = engine.store().lock();
        let tenant_id = store.find_tenant_by_public_key(&public_key).unwrap().unwrap().id;
        let stored = store.get_order(&tenant_id, &order_id).unwrap().unwrap();
        assert_eq!(
            stored.confirmations_required_override,
            Some(10),
            "a 10.00 XMR order below the only threshold's 50.00 must fall back to the store's own default"
        );
        drop(store);

        let metadata = state.db.lock().get_order_currency_metadata(&connection_id, &order_id).unwrap().unwrap();
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
                    .uri(format!("/dashboard/stores/{connection_id}/settings"))
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
                &format!("/dashboard/stores/{connection_id}/settings/confirmations"),
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
                &format!("/dashboard/stores/{connection_id}/settings/confirmations"),
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
                &format!("/dashboard/stores/{connection_id}/settings/webhooks"),
                &intruder_token,
                &[("url", "https://attacker.example/steal")],
            ))
            .await
            .unwrap();
        assert_eq!(create_response.status(), StatusCode::NOT_FOUND);

        let delete_response = router
            .oneshot(form_post_request(
                &format!("/dashboard/stores/{connection_id}/settings/webhooks/some-webhook-id/delete"),
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
    /// The store's Orders page is the full history: every order with its
    /// reference and where it came from (POS, dashboard, the WooCommerce
    /// plugin with the store key, a browser on the website), a POS
    /// cancellation shown as cancelled, a search by reference or order id,
    /// and pages of 50 newest first.
    #[tokio::test]
    async fn the_orders_page_lists_sources_and_cancellations_searches_and_pages() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(&router, "orders-page@example.com", "correct horse battery staple").await;
        let (id, pk) = create_connection(&router, &session_token).await;
        let bearer = format!("Bearer {session_token}");
        let json_post = |uri: String, auth: Option<String>, body: serde_json::Value| {
            let mut builder = Request::builder().method("POST").uri(uri).header("content-type", "application/json");
            if let Some(auth) = auth { builder = builder.header("authorization", auth); }
            builder.body(Body::from(body.to_string())).unwrap()
        };
        let created_id = |response: axum::response::Response| async move { body_json(response).await["order_id"].as_str().unwrap().to_string() };

        let pos = created_id(router.clone().oneshot(json_post(format!("/dashboard/stores/{id}/pos/orders"), Some(bearer.clone()),
            serde_json::json!({ "amount": "0.3", "merchant_order_id": "Table 4" }))).await.unwrap()).await;
        let cancelled = created_id(router.clone().oneshot(json_post(format!("/dashboard/stores/{id}/pos/orders"), Some(bearer.clone()),
            serde_json::json!({ "amount": "0.2", "merchant_order_id": "Table 9" }))).await.unwrap()).await;
        router.clone().oneshot(Request::builder().method("POST").uri(format!("/dashboard/stores/{id}/pos/orders/{cancelled}/cancel"))
            .header("authorization", bearer.clone()).body(Body::empty()).unwrap()).await.unwrap();
        let dashboard = router.clone().oneshot(form_post_request(&format!("/dashboard/stores/{id}/orders/new"), &session_token,
            &[("amount", "1.00"), ("currency", "XMR"), ("merchant_order_id", "invoice-7")])).await.unwrap();
        let dashboard = dashboard.headers()["location"].to_str().unwrap().rsplit('/').next().unwrap().to_string();
        let row = state.db.lock().get_store_connection_by_id(&id).unwrap().unwrap();
        let secret = crate::crypto::decrypt(&TEST_ENCRYPTION_KEY, &row.tenant_secret_token_encrypted).unwrap();
        let plugin = created_id(router.clone().oneshot(json_post(format!("/pay/{pk}/orders"), Some(format!("Bearer {secret}")),
            serde_json::json!({ "amount": "2.00", "currency": "XMR", "merchant_order_id": "wc-1042" }))).await.unwrap()).await;
        let website = created_id(router.clone().oneshot(json_post(format!("/pay/{pk}/orders"), None,
            serde_json::json!({ "amount": "3.00", "currency": "XMR" }))).await.unwrap()).await;

        let page = |query: &str| {
            let (router, bearer, uri) = (router.clone(), bearer.clone(), format!("/dashboard/stores/{id}/orders{query}"));
            async move { body_text(router.oneshot(Request::builder().uri(uri).header("authorization", bearer).body(Body::empty()).unwrap()).await.unwrap()).await }
        };
        let row_of = |html: &str, order_id: &str| -> String {
            let start = html.find(&format!("/orders/{order_id}\"")).unwrap_or_else(|| panic!("{order_id} not listed"));
            html[start..].split("</tr>").next().unwrap().to_string()
        };
        let html = page("").await;
        for (order_id, source, reference, status) in [
            (&pos, "POS", "Table 4", "Waiting for payment"),
            (&cancelled, "POS", "Table 9", "Cancelled"),
            (&dashboard, "Dashboard", "invoice-7", "Waiting for payment"),
            (&plugin, "WooCommerce", "wc-1042", "Waiting for payment"),
            (&website, "Website", "—", "Waiting for payment"),
        ] {
            let row = row_of(&html, order_id);
            for expected in [source, reference, status] {
                assert!(row.contains(&format!(">{expected}<")), "{order_id}: expected {expected} in {row}");
            }
            // The status is the same badge the checkout and the POS show.
            let class = if status == "Cancelled" { "state-cancelled" } else { "state-pending" };
            assert!(row.contains(&format!(r#"<span class="tag {class}">"#)), "{order_id}: expected a {class} badge in {row}");
        }

        let html = page("?q=table").await;
        assert!(html.contains(&pos) && html.contains(&cancelled) && !html.contains(&plugin), "search by reference");
        assert!(html.contains(r#"value="table""#), "the search stays in the box");
        let html = page(&format!("?q={}", &plugin[6..16])).await;
        assert!(html.contains(&plugin) && !html.contains(&pos), "search by order id");
        let html = page("?q=nothing-like-this").await;
        assert!(html.contains("No orders match “nothing-like-this”."));

        // 5 so far; 46 more make 51: the first page has 50 and an Older link.
        for i in 0..46 {
            router.clone().oneshot(json_post(format!("/pay/{pk}/orders"), None, serde_json::json!({ "amount": format!("0.{i:02}1"), "currency": "XMR" }))).await.unwrap();
        }
        let first = page("").await;
        assert_eq!(first.matches(r#"<tr><td class="card-title"><a"#).count(), 50);
        assert!(first.contains(&format!(r#"href="/dashboard/stores/{id}/orders?page=1" rel="next""#)) && !first.contains(r#"rel="prev""#));
        let second = page("?page=1").await;
        assert_eq!(second.matches(r#"<tr><td class="card-title"><a"#).count(), 1);
        assert!(second.contains(r#"rel="prev""#) && !second.contains(r#"rel="next""#));
    }

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
                &format!("/dashboard/stores/{connection_id}/orders/lookup"),
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

    /// A customer says they paid and sends the txid; the scanner has not
    /// matched it (their wallet paid while the node was behind, say). The
    /// merchant pastes it into the store page's lookup, which finds the order
    /// it pays and records the payment; the order's page then lists it.
    /// `subaddress_tx.hex` pays subaddress 0/1 of this view/spend key pair,
    /// and a store's first order gets subaddress index 1.
    #[tokio::test]
    async fn merchant_recovers_a_missed_payment_by_looking_up_the_customers_txid() {
        use monero::cryptonote::hash::Hashable;
        let (state, engine) = test_state_with_real_engine_and_admin_lookup_daemon().await;
        let router = build_router(state);
        let session_token = signed_up_and_logged_in_session_token(&router, "lookup-recover@example.com", "correct horse battery staple").await;
        let view_key = monero::PrivateKey::from_slice(&hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07").unwrap()).unwrap();
        let spend_key = monero::PrivateKey::from_slice(&hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907").unwrap()).unwrap();
        let connect = |view: String, spend: String| Request::builder().method("POST").uri("/connections")
            .header("content-type", "application/json").header("authorization", format!("Bearer {session_token}"))
            .body(Body::from(serde_json::json!({
                "platform": "custom", "site_url": "https://shop.example.com", "view_key_hex": view,
                "spend_pubkey_hex": spend, "network": "mainnet", "domains": [], "base_currency": "XMR",
            }).to_string())).unwrap();
        let response = router.clone().oneshot(connect(hex::encode(view_key.to_bytes()),
            hex::encode(monero::PublicKey::from_private_key(&spend_key).to_bytes()))).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let connection_id = body_json(response).await["connection_id"].as_str().unwrap().to_string();
        let response = router.clone().oneshot(form_post_request(&format!("/dashboard/stores/{connection_id}/orders/new"),
            &session_token, &[("amount", "0.000000000001"), ("currency", "XMR")])).await.unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let order_page = response.headers()["location"].to_str().unwrap().to_string();

        let tx: monero::Transaction = monero::consensus::deserialize(
            &hex::decode(include_str!("../../../scanner/tests/fixtures/subaddress_tx.hex").trim()).unwrap()).unwrap();
        let txid = hex::encode(tx.hash().to_bytes());
        engine.add_mempool_transaction(tx);
        let lookup = format!("/dashboard/stores/{connection_id}/orders/lookup");
        let html = body_text(router.clone().oneshot(form_post_request(&lookup, &session_token, &[("txid", &txid)])).await.unwrap()).await;
        assert!(html.contains("Match found and recorded."), "got: {html}");
        let order_id = order_page.rsplit('/').next().unwrap();
        assert!(html.contains(&format!("/dashboard/stores/{connection_id}/orders/{order_id}")), "links to the matched order: {html}");

        let order = body_text(router.clone().oneshot(Request::builder().uri(&order_page)
            .header("authorization", format!("Bearer {session_token}")).body(Body::empty()).unwrap()).await.unwrap()).await;
        assert!(order.contains(&txid), "the order page lists the recorded payment: {order}");

        // The same transaction looked up from a store it does not pay.
        let (other_store, _) = create_connection(&router, &session_token).await;
        let html = body_text(router.clone().oneshot(form_post_request(&format!("/dashboard/stores/{other_store}/orders/lookup"),
            &session_token, &[("txid", &txid)])).await.unwrap()).await;
        assert!(html.contains("That transaction exists, but doesn&#39;t pay any of this store&#39;s orders.")
            || html.contains("That transaction exists, but doesn't pay any of this store's orders."), "got: {html}");

        // A txid mangled when pasted is refused with the engine's reason.
        let html = body_text(router.clone().oneshot(form_post_request(&lookup, &session_token, &[("txid", "not-a-txid")])).await.unwrap()).await;
        assert!(html.contains("Couldn&#39;t look that up:") || html.contains("Couldn't look that up:"), "got: {html}");

        // With fixi, only the card comes back, answer in it.
        let html = body_text(router.oneshot(fixi(form_post_request(&lookup, &session_token, &[("txid", &txid)]))).await.unwrap()).await;
        assert!(html.starts_with(r#"<div class="card" id="payment-lookup">"#) && html.contains(&txid), "got: {html}");
    }

    // -- "Scan range" row (`docs/order_rescan_wbs.md` Phase 5.4) ------------

    #[tokio::test]
    async fn scan_range_row_shows_a_muted_dash_for_an_order_with_no_first_tick_yet() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "scan-range-fresh-owner@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) = create_connection(&router, &session_token).await;
        let order_id = seed_real_order(&state, engine.addr, &public_key).await;
        // Deliberately no `bump_scanned_range_for_order` call - this harness runs
        // no background scan loop, so a freshly seeded order genuinely has never
        // been examined by anything yet.

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{connection_id}/orders/{order_id}"))
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
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "scan-range-growing-owner@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) = create_connection(&router, &session_token).await;
        let order_id = seed_real_order(&state, engine.addr, &public_key).await;
        // Still `pending` (non-terminal) - genuinely still in scope, so the range
        // must read as still growing ("N+"), not a closed span. Two calls to the
        // live scanner's own bulk bump (its only mover now that the manual rescan
        // feature is gone) rather than a single-order setter: the first (COALESCE)
        // establishes `first_scanned_height`, the second only advances `last_
        // scanned_height`, matching exactly how two real scan ticks would move it.
        {
            let s = engine.store().lock();
            let tenant = s.find_tenant_by_public_key(&public_key).unwrap().unwrap();
            s.bump_scanned_heights_for_tenant(&tenant.id, 100, crate::now_unix(), 0).unwrap();
            s.bump_scanned_heights_for_tenant(&tenant.id, 250, crate::now_unix(), 0).unwrap();
        }

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/dashboard/stores/{connection_id}/orders/{order_id}"))
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
            from_pos: false,
            order_id: "pay_abc123".to_string(),
            merchant_order_id: None,
            address: "addr".to_string(),
            currency: "XMR".to_string(),
            amount: "0.5".to_string(),
            rate_display: "1 XMR per 1 XMR".to_string(),
            rate_provider: "xmr".to_string(),
            xmr_amount_piconero: 500_000_000_000,
            amount_received_piconero: 500_000_000_000,
            status: "paid".to_string(),
            confirmations: 10,
            confirmations_required_display: "10".to_string(),
            base_currency_display: "XMR".to_string(),
            base_currency_rate_display: "same as order currency".to_string(),
            double_spend_detected_at: None,
            refund_address: None,
            created_at: 1000,
            expires_at: 2000,
            updated_at: 1000,
            payments: vec![],
            payment_link: "http://127.0.0.1:8081/pay/pk_abc123/orders/pay_abc123/share".to_string(),
            scan_range_display: crate::templates::display_scan_range(Some(100), Some(250), false),
        };
        let data = OrderDetailViewModel { connection_id: "conn_1".to_string(), display_name: "shop.example.com".to_string(), order: Some(order) };
        let chrome = crate::views::PageChrome::from_user(None, "");
        let html = crate::views::orders::detail_page(&chrome, &data).into_string();
        assert!(html.contains("100 - 250"), "expected the closed range display, got: {html}");
        assert!(!html.contains("100+"), "must not show a still-growing range once no longer being watched, got: {html}");
    }
}
