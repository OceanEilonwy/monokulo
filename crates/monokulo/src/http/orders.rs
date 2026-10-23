//! A store's pages on the dashboard: its orders (list and detail), creating
//! an order, looking a payment up by txid, its webhooks, and its settings
//! (confirmations, thresholds, exchange-rate providers, key custody). All
//! of it goes through the engine's admin API with the connection's
//! decrypted `sk_...` token; the engine owns order state, and nothing here
//! changes an order the engine created except through that API.
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
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::crypto;
use crate::db::{StoreConnectionRow, UserRow};
use crate::engine_client::{EngineClientError, PaymentLookupView};
use crate::network::OnNetwork;
use crate::templates::{display_or_dash, display_scan_range};
use crate::views;
use crate::views::orders::{
    OrderDetailData, OrderDetailViewModel, OrderRowViewModel, OrdersViewModel, PaymentRowViewModel,
};

use super::auth::redirect_302;
use super::fx::FxRequest;
use super::{AppState, AuthedUser};
use crate::views::store_settings::{StoreOutcome, StoreSection, StoreTab};

/// Looks up `store_connections` row `id` and confirms it belongs to `user`.
/// `Ok(None)` covers *both* "no such row" and "exists but belongs to someone
/// else" - callers must map that uniformly to `404` (see this module's own
/// doc comment), never distinguishing the two. `Err(())` is a real database
/// failure - the caller's problem, not the requester's.
pub(super) async fn load_owned_connection(
    db: &crate::db::Database,
    user: &UserRow,
    id: &crate::db::ConnectionId,
) -> Result<Option<super::OwnedStore>, ()> {
    let id = id.clone();
    let row = db
        .read(move |db| db.get_store_connection_by_id(&id))
        .await
        .map_err(|_| ())?;
    Ok(row.and_then(|row| super::OwnedStore::check(row, &user.id)))
}

/// Decrypts the connection's stored `sk_...` token under `state`'s
/// encryption key. A failure here means a row this service itself wrote and
/// encrypted can't be decrypted with its own key - shouldn't happen, but
/// handled as a plain internal error rather than unwrapped/panicked on.
pub(crate) fn decrypt_sk(
    encryption_key: &crate::crypto::AtRestKey,
    row: &StoreConnectionRow,
) -> Result<shared::auth::RawToken, ()> {
    crypto::decrypt(
        encryption_key,
        crypto::Binding::StoreSecret(row.id.as_str()),
        &row.tenant_secret_token_encrypted,
    )
    .map(|sk| shared::auth::RawToken::presented(&sk))
    .map_err(|_| ())
}

/// Shared by `orders_list` - the "real orders, real fiat metadata" view
/// model for the plain orders list page.
async fn build_orders_view_model(
    state: &AppState,
    row: &super::OwnedStore,
    sk: &shared::auth::RawToken,
    search: &str,
    page: u32,
) -> Result<OrdersViewModel, ()> {
    use views::orders::ORDERS_PER_PAGE;
    let term = Some(search.trim()).filter(|term| !term.is_empty());
    // One more than a page, to know whether there is an older page.
    let mut orders = state
        .engine
        .client
        .list_orders_page(sk, false, term, ORDERS_PER_PAGE + 1, page * ORDERS_PER_PAGE)
        .await
        .map_err(|_| ())?;
    let has_more = orders.len() > ORDERS_PER_PAGE as usize;
    orders.truncate(ORDERS_PER_PAGE as usize);
    Ok(OrdersViewModel {
        connection_id: row.id.clone(),
        display_name: row.name.clone(),
        orders: order_rows(state, row, orders).await,
        search: search.trim().to_string(),
        page,
        has_more,
    })
}

/// The orders table's rows: the engine's view of each order, with what only
/// monokulo knows - its fiat amount, where it came from, and whether the
/// POS cancelled it.
pub(super) async fn order_rows(
    state: &AppState,
    row: &super::OwnedStore,
    orders: Vec<crate::engine_client::OrderView>,
) -> Vec<OrderRowViewModel> {
    let ids: Vec<crate::db::OrderId> = orders.iter().map(|o| o.order_id.clone()).collect();
    let id = row.id.clone();
    let (fiat_metadata, details, plugins) = state
        .db
        .read(move |db| {
            let plugins = db
                .list_integrations(&id)
                .inspect_err(|e| tracing::error!(error = %e, "could not read the store's plugins"))
                .unwrap_or_default();
            // Missing metadata shows as "—"; the failure is logged rather
            // than hidden.
            let metadata = db
                .order_currency_metadata_for(&id, &ids)
                .inspect_err(
                    |e| tracing::error!(error = %e, "could not read order currency metadata"),
                )
                .unwrap_or_default();
            let details = db
                .order_listing_details(&id, &ids)
                .inspect_err(|e| tracing::error!(error = %e, "could not read order details"))
                .unwrap_or_default();
            Ok::<_, crate::db::DbError>((metadata, details, plugins))
        })
        .await
        .unwrap_or_default();
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
            let status = if detail.pos_cancelled_at.is_some()
                && matches!(
                    o.status,
                    shared::order_status::OrderStatus::Pending
                        | shared::order_status::OrderStatus::Expired
                ) {
                crate::views::DisplayStatus::Cancelled
            } else {
                o.status.into()
            };
            OrderRowViewModel {
                order_id: o.order_id,
                reference: o.merchant_order_id,
                source: source_label(detail.source.as_deref(), &plugins),
                status,
                amount,
                currency,
                created_at: o.created_at,
            }
        })
        .collect()
}

/// How an order's source reads in the orders table. Any other source is a
/// plugin's slug (`api::v1::orders::order_source`), shown by the name the
/// store's newest integration of that plugin gave (`plugins`, newest
/// first), else by the slug.
fn source_label(source: Option<&str>, plugins: &[crate::db::StoreIntegrationRow]) -> String {
    match source {
        Some("pos") => "POS".to_owned(),
        Some("dashboard") => "Dashboard".to_owned(),
        Some("api") => "Store API".to_owned(),
        Some("website") => "Website".to_owned(),
        Some(slug) => {
            let name = plugins
                .iter()
                .find(|i| i.kind == slug)
                .map_or("", |i| i.name.as_str());
            crate::plugins::Plugin::stored(slug, name).to_string()
        }
        None => "—".to_owned(),
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
    Path(id): Path<crate::db::ConnectionId>,
    Query(query): Query<OrdersListQuery>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let sk = match decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let search: String = query.q.unwrap_or_default().chars().take(120).collect();
    let view_model = match build_orders_view_model(
        &state,
        &row,
        &sk,
        &search,
        query.page.unwrap_or(0).min(10_000),
    )
    .await
    {
        Ok(vm) => vm,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if fx.0 {
        return axum::response::Html(
            views::orders::list_results(&view_model, &views::time::Clock::for_user(&user))
                .into_string(),
        )
        .into_response();
    }
    let chrome = super::page_chrome(&state, Some(&user), format!("/stores/{id}/orders")).await;
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
async fn perform_payment_lookup(
    state: &AppState,
    sk: &shared::auth::RawToken,
    txid: &str,
) -> Result<(String, Option<String>), ()> {
    match state.engine.client.lookup_payment(sk, txid).await {
        Ok(PaymentLookupView::NotFoundOnChain) => Ok((
            "No transaction with that ID was found on the network.".to_string(),
            None,
        )),
        Ok(PaymentLookupView::NoMatchingOrder) => Ok((
            "That transaction exists, but doesn't pay any of this store's orders.".to_string(),
            None,
        )),
        Ok(PaymentLookupView::Matched { order_ids }) => Ok((
            "Match found and recorded.".to_string(),
            order_ids.into_iter().next(),
        )),
        Err(EngineClientError::EngineError { status, message })
            if status == reqwest::StatusCode::BAD_REQUEST =>
        {
            Ok((format!("Couldn't look that up: {message}"), None))
        }
        Err(_) => Err(()),
    }
}

/// `POST /stores/{id}/orders/lookup` - `docs/txid_lookup_and_
/// scan_chunking_wbs.md` Part B.3, the direct replacement for the old
/// manual rescan feature. Re-renders the store overview page with the
/// result shown inline in its "Recent orders" section (a plain message,
/// plus a link to the matched order if any) - recompute and redisplay,
/// never a redirect that would lose the result.
pub async fn lookup_payment(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    fx: FxRequest,
    Path(id): Path<crate::db::ConnectionId>,
    Form(form): Form<LookupPaymentForm>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let sk = match decrypt_sk(&state.encryption_key, &row) {
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
            &format!("/stores/{id}/orders/lookup"),
            &txid,
            &Some(message),
            &found_order_id,
            |order_id| format!("/stores/{id}/orders/{order_id}"),
        );
        return axum::response::Html(card.into_string()).into_response();
    }
    render_store_detail_page(&state, row, &user, txid, Some(message), found_order_id).await
}

/// `GET /stores/{id}/orders/{order_id}` - the order's full
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
    Path((id, order_id)): Path<(crate::db::ConnectionId, crate::db::OrderId)>,
    on: OnNetwork,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let sk = match decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let chrome = super::page_chrome(
        &state,
        Some(&user),
        format!("/stores/{id}/orders/{order_id}"),
    )
    .await;
    let payment_link = payment_link_for(&on, &order_id);

    match order_detail_data(&state, &row, &sk, &order_id, payment_link).await {
        Ok(Some(order)) => {
            let view_model = OrderDetailViewModel {
                connection_id: id.clone(),
                display_name: row.name.clone(),
                order: Some(order),
            };
            views::orders::detail_page(&chrome, &view_model).into_response()
        }
        Ok(None) => {
            let view_model = OrderDetailViewModel {
                connection_id: id.clone(),
                display_name: row.name.clone(),
                order: None,
            };
            (
                StatusCode::NOT_FOUND,
                views::orders::detail_page(&chrome, &view_model),
            )
                .into_response()
        }
        Err(()) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Everything the order detail page shows about one order, `None` when
/// the engine has no such order.
async fn order_detail_data(
    state: &AppState,
    row: &super::OwnedStore,
    sk: &shared::auth::RawToken,
    order_id: &crate::db::OrderId,
    payment_link: String,
) -> Result<Option<OrderDetailData>, ()> {
    match state.engine.client.get_order_detail(sk, order_id).await {
        Ok(detail) => {
            // The engine has no concept of fiat - fiat display comes entirely
            // from monokulo's own
            // local `order_currency_metadata`, absent for any order that predates
            // this record (falls back to a dash rather than failing the page).
            let (id, order) = (row.id.clone(), order_id.clone());
            let (metadata, pos_order) = state
                .db
                .read(move |db| {
                    Ok::<_, crate::db::DbError>((
                        db.get_order_currency_metadata(&id, &order).ok().flatten(),
                        db.get_pos_order(&id, &order),
                    ))
                })
                .await
                .map_err(|_| ())?;
            let (amount, currency) = match &metadata {
                Some(m) => (m.amount.clone(), m.currency.clone()),
                None => ("—".to_string(), "".to_string()),
            };
            let (rate_display, rate_provider) = match &metadata {
                Some(m) => (
                    format!(
                        "{} XMR per 1 {}",
                        crate::views::trim_xmr(&shared::exchange_rate::format_piconero_as_xmr(
                            m.piconero_per_unit.get()
                        )),
                        m.currency
                    ),
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
            let base_currency_display = metadata
                .as_ref()
                .and_then(|m| m.store_base_currency.clone())
                .unwrap_or_else(|| "—".to_string());
            let base_currency_rate_display = match metadata
                .as_ref()
                .and_then(|m| m.store_base_currency.clone())
            {
                Some(base_currency) => match metadata
                    .as_ref()
                    .and_then(|m| m.base_currency_piconero_per_unit)
                {
                    Some(rate) => {
                        format!(
                            "{} XMR per 1 {base_currency}",
                            crate::views::trim_xmr(&shared::exchange_rate::format_piconero_as_xmr(
                                rate.get()
                            ))
                        )
                    }
                    None => "same as order currency".to_string(),
                },
                None => "—".to_string(),
            };
            let from_pos = matches!(pos_order, Ok(Some(_)));
            Ok(Some(OrderDetailData {
                from_pos,
                order_id: detail.order.order_id,
                merchant_order_id: detail.order.merchant_order_id,
                show_merchant_order_id: metadata.as_ref().is_none_or(|m| m.show_merchant_order_id),
                description: detail.order.description,
                address: detail.order.address,
                currency,
                amount,
                rate_display,
                rate_provider,
                xmr_amount_piconero: detail.order.xmr_amount_piconero,
                amount_received_piconero: detail.order.amount_received_piconero,
                status: detail.order.status.into(),
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
                        block_height_display: display_or_dash(
                            p.block_height.map(|h| h.to_string()).as_deref(),
                        ),
                        voided_at: p.voided_at,
                        superseded: p.superseded,
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
        Err(EngineClientError::EngineError { status, .. })
            if status == reqwest::StatusCode::NOT_FOUND =>
        {
            Ok(None)
        }
        Err(_) => Err(()),
    }
}

/// `GET /stores/{id}/orders/{order_id}/events`: the order detail
/// page's live part, re-rendered whenever the order changes (and every
/// 30 s, for the clock), as ssexi JSON-routed events replacing
/// `#order-live`. Ends with a `done` event once the order can't change
/// any more, so the page stops reconnecting (`static/fx-glue.js`).
pub async fn order_detail_events(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, order_id)): Path<(crate::db::ConnectionId, crate::db::OrderId)>,
    on: OnNetwork,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let sk = match decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let payment_link = payment_link_for(&on, &order_id);
    let subscription = state.engine.client.subscribe_order(&row.id, &sk, &order_id);
    let row = std::sync::Arc::new(row);
    // Times in the viewer's zone, as the page itself shows them.
    let clock = views::time::Clock::for_user(&user);
    crate::live::live_events(
        subscription,
        std::time::Duration::from_secs(30),
        move || {
            let (state, row, sk, order_id, payment_link, clock) = (
                state.clone(),
                row.clone(),
                sk.clone(),
                order_id.clone(),
                payment_link.clone(),
                clock.clone(),
            );
            async move {
                let order = order_detail_data(&state, &row, &sk, &order_id, payment_link)
                    .await
                    .ok()??;
                let (_, _, terminal) = crate::views::order_state(order.status);
                let html = views::orders::live_fragment(&order, &clock).into_string();
                let mut events = vec![axum::response::sse::Event::default()
                    .event(r##"{"target":"#order-live","swap":"outerHTML"}"##)
                    .data(html.clone())];
                if terminal {
                    events.push(
                        axum::response::sse::Event::default()
                            .event("done")
                            .data("final"),
                    );
                }
                Some(crate::live::LiveSnapshot {
                    events,
                    fingerprint: html,
                    terminal,
                })
            }
        },
    )
}

/// The link a customer is sent to pay order `order_id`: on the address
/// the merchant is using (`crate::network::OnNetwork::link`), so one made
/// on the onion address is the onion one and one made on the web is the
/// web one.
fn payment_link_for(on: &OnNetwork, order_id: &crate::db::OrderId) -> String {
    on.link(
        &super::paths::CheckoutSharePath {
            order_id: order_id.clone(),
        }
        .to_string(),
    )
}

/// The only store-health signal available without this service also probing
/// the merchant's own `site_url` (nothing here does that): whether the
/// engine actually answers `GET /api/v1/admin/tenant` for this connection's
/// `sk_...`. `("ok", "healthy")`/`("error", "unreachable")` are used
/// directly as a CSS class suffix (`tag-ok`, `tag-error`)
/// and a human label respectively - kept as two separate strings rather than
/// deriving one from the other so the template never has to.
pub(super) fn health_of_tenant_lookup<T>(
    result: &Result<T, EngineClientError>,
) -> (String, String) {
    match result {
        Ok(_) => ("ok".to_string(), "healthy".to_string()),
        Err(_) => ("error".to_string(), "unreachable".to_string()),
    }
}

/// `GET /stores/{id}` - the store overview page: identity,
/// health, recent orders, and a Help link to the docs.
pub async fn store_detail(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<crate::db::ConnectionId>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let chrome = super::page_chrome(&state, Some(&user), format!("/stores/{id}")).await;
            let data = views::store_detail::StoreDetailViewModel { store: None };
            return (
                StatusCode::NOT_FOUND,
                views::store_detail::page(&chrome, &data),
            )
                .into_response();
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
    row: super::OwnedStore,
    user: &UserRow,
    lookup_txid_value: String,
    lookup_message: Option<String>,
    lookup_found_order_id: Option<String>,
) -> Response {
    let chrome = super::page_chrome(state, Some(user), format!("/stores/{}", row.id)).await;
    let sk = match decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let tenant_result = state.engine.client.get_tenant(&sk).await;
    let (health, health_label) = health_of_tenant_lookup(&tenant_result);

    // A store whose engine is currently unreachable still gets a real page -
    // just with no order data available, rather than a hard error. The
    // health tag above is what actually communicates the problem.
    let recent_orders = match state
        .engine
        .client
        .list_orders_page(&sk, false, None, 10, 0)
        .await
    {
        Ok(orders) => order_rows(state, &row, orders).await,
        Err(_) => Vec::new(),
    };

    let wallet = match row.wallet_id.clone() {
        Some(wallet_id) => {
            let user_id = user.id.clone();
            state
                .db
                .read(move |db| db.get_wallet(&user_id, &wallet_id))
                .await
                .ok()
                .flatten()
                .map(|w| (w.id.to_string(), w.name))
        }
        None => None,
    };
    let embed_warnings =
        super::embed_domains::store_page_warnings(state, &row.id, crate::now_unix()).await;
    let row = row.into_row();
    let view_model = views::store_detail::StoreDetailViewModel {
        store: Some(views::store_detail::StoreDetailData {
            connection_id: row.id,
            display_name: row.name.clone(),
            site: row.site,
            public_key: row.tenant_public_key,
            base_currency: row.base_currency,
            health,
            health_label,
            created_at: row.created_at,
            recent_orders,
            lookup_txid_value,
            lookup_message,
            lookup_found_order_id,
            embed_warnings,
            wallet,
        }),
    };
    views::store_detail::page(&chrome, &view_model).into_response()
}

/// `GET /stores/{id}/settings` - base currency, confirmation
/// thresholds (0-conf included), exchange rate provider, and webhooks, all
/// split out from the store overview page onto their own settings page.
#[derive(Default, Deserialize)]
pub struct StoreSettingsQuery {
    saved: Option<String>,
    /// The tab to show (`StoreTab::from_id`).
    tab: Option<String>,
}

pub async fn store_settings(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<crate::db::ConnectionId>,
    Query(query): Query<StoreSettingsQuery>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let outcome = match query.saved.as_deref() {
        None => None,
        Some("nothing") => Some(StoreOutcome::Unchanged),
        Some(saved) => {
            let sections: Vec<StoreSection> =
                saved.split(',').filter_map(StoreSection::from_id).collect();
            (!sections.is_empty()).then_some(StoreOutcome::Saved(sections))
        }
    };
    render_store_settings_page_with(
        &state,
        row,
        &user,
        PageState {
            outcome,
            tab: query.tab.as_deref().and_then(StoreTab::from_id),
            ..Default::default()
        },
    )
    .await
}

/// Shared by every action on the page (the wallet, the keys, the domains,
/// the webhooks) - each ends by showing a fresh copy of this same page,
/// optionally with the error that refused it (on `from`'s card, `422`) or a
/// just-created webhook signing secret. Takes an already ownership-checked
/// row rather than re-checking it, since every caller has already done
/// that.
pub(super) async fn render_store_settings_page(
    state: &AppState,
    row: super::OwnedStore,
    user: &UserRow,
    error: Option<String>,
    created_webhook_signing_secret: Option<String>,
    from: Option<StoreSection>,
) -> Response {
    let outcome = match (from, error, &created_webhook_signing_secret) {
        (Some(section), Some(message), _) => Some(StoreOutcome::Refused {
            section,
            message,
            saved: Vec::new(),
        }),
        (_, None, Some(_)) => Some(StoreOutcome::Saved(vec![StoreSection::Webhooks])),
        _ => None,
    };
    render_store_settings_page_with(
        state,
        row,
        user,
        PageState {
            outcome,
            created_webhook_signing_secret,
            // On the action's own card's tab, whatever came of it.
            tab: from.map(StoreTab::of),
            ..Default::default()
        },
    )
    .await
}

/// What the page shows beyond the store as it is saved.
#[derive(Default)]
pub(super) struct PageState {
    pub(super) outcome: Option<StoreOutcome>,
    /// The settings form as sent, after a save it refused.
    pub(super) sent: Option<Vec<(String, String)>>,
    pub(super) created_webhook_signing_secret: Option<String>,
    pub(super) wallet: WalletState,
    /// The tab asked for; else the one the outcome was on.
    pub(super) tab: Option<StoreTab>,
}

/// What the Wallet section shows beyond the store's wallets: a change
/// waiting to be confirmed, or one just made.
#[derive(Default)]
pub(super) struct WalletState {
    pending: Option<views::store_settings::PendingWalletChange>,
    changed: Option<String>,
}

/// The Wallet section's data: the store's wallet, the account's wallets to
/// change to, and its history.
async fn store_wallet_view(
    state: &AppState,
    row: &crate::db::StoreConnectionRow,
    user: &UserRow,
    extra: WalletState,
) -> views::store_settings::StoreWalletView {
    let (user_id, connection_id) = (user.id.clone(), row.id.clone());
    let read = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                db.list_wallets(&user_id)?,
                db.store_wallet_periods(&connection_id)?,
            ))
        })
        .await;
    let Ok((wallets, periods)) = read else {
        return views::store_settings::StoreWalletView {
            pending: extra.pending,
            changed: extra.changed,
            ..Default::default()
        };
    };
    let current = row.wallet_id.as_ref().and_then(|id| {
        let wallet = wallets.iter().find(|w| &w.wallet.id == id)?;
        let since = periods
            .iter()
            .find(|p| p.until.is_none())
            .map_or(row.created_at, |p| p.from);
        Some(views::store_settings::CurrentWallet {
            id: id.to_string(),
            name: wallet.wallet.name.clone(),
            since,
        })
    });
    let network = current
        .as_ref()
        .and_then(|c| wallets.iter().find(|w| w.wallet.id.as_str() == c.id))
        .map(|w| w.wallet.network.clone());
    // A store's network can't change (`change_store_wallet` refuses it):
    // only wallets on its own are offered.
    let mut choices: Vec<views::store_settings::WalletChoice> = wallets
        .iter()
        .filter(|w| network.as_ref().is_none_or(|n| *n == w.wallet.network))
        .map(|w| {
            let is_current = row.wallet_id.as_ref() == Some(&w.wallet.id);
            views::store_settings::WalletChoice {
                id: w.wallet.id.to_string(),
                name: w.wallet.name.clone(),
                short_address: views::short_address_text(&w.wallet.primary_address),
                current: is_current,
                network: w.wallet.network.clone(),
                other_stores: w.store_count.saturating_sub(u64::from(is_current)),
            }
        })
        .collect();
    choices.sort_by_key(|c| !c.current);
    views::store_settings::StoreWalletView {
        current,
        choices,
        pending: extra.pending,
        history: periods
            .into_iter()
            .map(|p| views::store_settings::WalletPeriodView {
                wallet_id: p.wallet_id.map(|id| id.to_string()),
                wallet_name: p.wallet_name,
                wallet_retired: p.wallet_retired,
                from: p.from,
                until: p.until,
                orders: p.orders,
            })
            .collect(),
        changed: extra.changed,
    }
}

pub(super) async fn render_store_settings_page_with(
    state: &AppState,
    row: super::OwnedStore,
    user: &UserRow,
    page: PageState,
) -> Response {
    let PageState {
        outcome,
        sent,
        created_webhook_signing_secret,
        wallet: wallet_state,
        tab,
    } = page;
    let tab = StoreTab::pick(tab, outcome.as_ref());
    let chrome =
        super::page_chrome(state, Some(user), format!("/stores/{}/settings", row.id)).await;
    let sk = match decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let tenant_result = state.engine.client.get_tenant(&sk).await;
    if tenant_result
        .as_ref()
        .is_ok_and(|t| t.key_custody_backend.is_some())
    {
        // The backends come from the engine's status: fetched now if it
        // isn't cached (it was just invalidated by a move, for one), but
        // never holding the page up for long.
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(1500),
            super::status_page::get_status_cached(&state.engine),
        )
        .await;
    }
    let key_storage = tenant_result
        .as_ref()
        .ok()
        .and_then(|t| t.key_custody_backend.clone())
        .and_then(|current| {
            let enabled = super::status_page::known_enabled_custody_backends(&state.engine);
            let usable = super::key_entry::usable_custody_backends(state);
            let move_to: Vec<views::connect::CustodyChoice> = usable
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
                snp_entry: None,
            })
        });
    let mut key_storage = key_storage;
    if let Some(view) = key_storage.as_mut() {
        let targets: Vec<String> = view.move_to.iter().map(|c| c.backend.clone()).collect();
        view.snp_entry = super::key_entry::prepare(
            state,
            &user.id,
            super::key_entry::Purpose::Move(&sk),
            &targets,
        )
        .await;
    }
    let confirmations_required = tenant_result
        .as_ref()
        .map(|t| t.confirmations_required)
        .unwrap_or(crate::confirmation_thresholds::FALLBACK_CONFIRMATIONS);
    // Native 0-conf: the Default row's own checkbox is checked exactly when the
    // tenant's default confirmations count already is 0 - no separate engine
    // field to read.
    let zero_conf_enabled = tenant_result
        .as_ref()
        .is_ok_and(|t| t.confirmations_required == 0);

    let fx_provider_options = fx_provider_options(
        &state.exchange_rate.available_providers(),
        &row.fx_providers,
    );
    let haveno_settings = state
        .exchange_rate
        .is_available(crate::exchange_rate_config::HAVENO)
        .then(|| views::store_settings::HavenoSettingsView::from(&row.fx_provider_settings.haveno));
    let (id, base_currency) = (row.id.clone(), row.base_currency.clone());
    let (base_currency_options, thresholds, embed_restricted, embed_domain_rows, client_logging) =
        state
            .db
            .read(move |db| {
                Ok::<_, crate::db::DbError>((
                    crate::currencies::currency_options(db, &base_currency).unwrap_or_default(),
                    db.list_confirmation_thresholds(&id).unwrap_or_default(),
                    db.embed_restricted(&id).unwrap_or(false),
                    db.list_store_domains(&id).unwrap_or_default(),
                    db.client_logging(&id).unwrap_or(false),
                ))
            })
            .await
            .unwrap_or_default();
    let confirmation_thresholds = thresholds
        .into_iter()
        .map(|t| views::store_settings::ConfirmationThresholdView {
            id: t.id,
            unit_amount: t.unit_amount,
            confirmations_required: t.confirmations_required,
        })
        .collect::<Vec<_>>();
    let confirmation_thresholds_at_max = confirmation_thresholds.len() >= 5;
    let now = crate::now_unix();
    let embed_can_restrict = embed_domain_rows
        .iter()
        .any(|domain| crate::embed_domains::DomainState::of(domain, now).counts());
    let embed_domains = super::embed_domains::domain_views(embed_domain_rows, now);

    let webhooks =
        super::webhooks::card(state, &row, &chrome.clock, created_webhook_signing_secret).await;

    let row = row.into_row();
    let wallet = store_wallet_view(state, &row, user, wallet_state).await;
    let sites = super::store_site::site_view(state, &row, &sk, &chrome.clock).await;
    let apps = super::apps::apps_view(state, &row, &chrome.clock).await;
    let view_model = views::store_settings::StoreSettingsViewModel {
        store: Some(views::store_settings::StoreSettingsData {
            clock: chrome.clock.clone(),
            connection_id: row.id,
            display_name: row.name.clone(),
            site: crate::stores::stored_site(row.site.as_ref()).to_owned(),
            payment_description: row.payment_description.clone(),
            sites,
            apps,
            confirmations_required,
            fx_provider_options,
            haveno_settings,
            base_currency: row.base_currency,
            base_currency_options,
            confirmation_thresholds,
            confirmation_thresholds_at_max,
            zero_conf_enabled,
            webhooks,
            outcome,
            sent,
            embed_domains,
            embed_restricted,
            embed_can_restrict,
            key_storage,
            client_logging,
            wallet,
            tab,
        }),
    };
    let refused = matches!(
        view_model.store.as_ref().and_then(|s| s.outcome.as_ref()),
        Some(StoreOutcome::Refused { .. })
    );
    let page = views::store_settings::page(&chrome, &view_model);
    if refused {
        (StatusCode::UNPROCESSABLE_ENTITY, page).into_response()
    } else {
        page.into_response()
    }
}

/// After a successful action: back to the page (303), whose toast says
/// what was done, at the action's card.
pub(super) fn saved(id: &crate::db::ConnectionId, section: StoreSection) -> Response {
    super::auth::redirect_303(&format!(
        "/stores/{id}/settings?saved={}#{}",
        section.id(),
        views::settings::card_id(section.id())
    ))
}

/// Every currency this store's "create an order" form can offer right now -
/// shared by `create_order_page` and `create_order`'s own validation-error
/// re-render, so the two can never drift on what the dropdown looks like.
async fn order_currency_options_for(
    state: &AppState,
    row: &StoreConnectionRow,
) -> (Vec<String>, bool) {
    // A live Coingecko failure here degrades to "XMR only" rather than
    // failing this whole page - same "show something real-ish rather than
    // fail outright" approach the store overview page's own health check
    // already takes for its own engine-reachability failures.
    let known_currencies: Vec<String> = state
        .db
        .read(|db| db.list_currencies())
        .await
        .map(|rows| rows.into_iter().map(|c| c.canonical_code).collect())
        .unwrap_or_default();
    let order_currency_options = state
        .exchange_rate
        .supported_currencies_for(row, &known_currencies)
        .await
        .unwrap_or_else(|_| vec!["XMR".to_string()]);
    let order_currency_is_locked_to_xmr =
        order_currency_options.len() == 1 && order_currency_options[0] == "XMR";
    (order_currency_options, order_currency_is_locked_to_xmr)
}

/// `GET /stores/{id}/orders/new` - the "create an order"
/// widget page, reached from the store overview page's own widget tile (the
/// same shape as the POS terminal's own tile/page).
pub async fn create_order_page(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<crate::db::ConnectionId>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render_create_order_page(&state, row, &user, None, None).await
}

/// Shared by `create_order_page` and `create_order`'s own validation-error
/// branches - both end by showing a fresh copy of this same page.
async fn render_create_order_page(
    state: &AppState,
    row: super::OwnedStore,
    user: &UserRow,
    submitted: Option<&CreateOrderForm>,
    order_creation_error: Option<String>,
) -> Response {
    let chrome =
        super::page_chrome(state, Some(user), format!("/stores/{}/orders/new", row.id)).await;
    let (order_currency_options, order_currency_is_locked_to_xmr) =
        order_currency_options_for(state, &row).await;
    // A rejected submission keeps what was typed, and its request key: a
    // retry after a lost answer then repeats the key, and gets the order
    // the first attempt may already have made.
    let request_key = submitted
        .map(|form| form.request_key.trim())
        .filter(|key| crate::engine_client::valid_idempotency_key(key))
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let default_currency = if order_currency_options.contains(&row.base_currency) {
        row.base_currency.clone()
    } else {
        "XMR".to_string()
    };
    let data = views::create_order::CreateOrderData {
        connection_id: row.id.clone(),
        display_name: row.name.clone(),
        order_creation_error,
        order_currency_options,
        order_currency_is_locked_to_xmr,
        amount: submitted.map_or_else(|| "10.00".to_string(), |form| form.amount.clone()),
        currency: submitted
            .map(|form| form.currency.clone())
            .unwrap_or(default_currency),
        merchant_order_id: submitted
            .map(|form| form.merchant_order_id.clone())
            .unwrap_or_default(),
        description: submitted
            .map(|form| form.description.clone())
            .unwrap_or_default(),
        show_merchant_order_id: submitted.is_none_or(CreateOrderForm::shows_merchant_order_id),
        request_key,
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
    /// What the order is for, for the payer; blank is none, the same as
    /// `merchant_order_id`.
    #[serde(default)]
    pub description: String,
    /// The "Show the reference to the payer" switch: `Some("true")` when
    /// on, absent when off.
    #[serde(default)]
    pub show_merchant_order_id: Option<String>,
    /// The switches the form had (`views::controls::switch`), so an
    /// absent switch reads as off only when the form drew it.
    #[serde(default)]
    pub switches: Option<String>,
    /// One per rendering of the form (a hidden field): the order's
    /// idempotency key, so submitting twice (a retry after a timeout, a
    /// double click) makes one order.
    #[serde(default)]
    pub request_key: String,
}

impl CreateOrderForm {
    /// Whether the checkout shows the payer the reference: the switch is
    /// on, or a form without it (a script) left the default.
    fn shows_merchant_order_id(&self) -> bool {
        self.show_merchant_order_id.is_some()
            || self.switches.as_deref() != Some("show_merchant_order_id")
    }
}

/// `POST /stores/{id}/orders/new` - creates a real order
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
    Path(id): Path<crate::db::ConnectionId>,
    Form(form): Form<CreateOrderForm>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let policy = crate::confirmation_thresholds::lock_policy(&row.tenant_public_key).await;
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // Bind the key to the merchant's input, rather than a changing FX quote.
    // An unchanged retry returns its existing order before repricing it; a
    // changed form restored by browser history is a different request.
    use sha2::{Digest, Sha256};
    let request_key =
        crate::engine_client::valid_idempotency_key(form.request_key.trim()).then(|| {
            let payload = serde_json::to_vec(&(
                form.request_key.trim(),
                form.amount.trim(),
                form.currency.trim(),
                form.merchant_order_id.trim(),
                form.description.trim(),
                form.shows_merchant_order_id(),
            ))
            .expect("strings serialize");
            format!("dash:{}", hex::encode(Sha256::digest(payload)))
        });
    if let Some(key) = &request_key {
        let (store_id, key) = (id.clone(), key.clone());
        match state
            .db
            .read(move |db| db.dashboard_order_request(&store_id, &key))
            .await
        {
            Ok(Some(order)) => return redirect_302(&format!("/stores/{id}/orders/{order}")),
            Ok(None) => {}
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }

    let amount = form.amount.trim();
    let currency = form.currency.trim();
    if amount.is_empty() || currency.is_empty() {
        return render_create_order_page(
            &state,
            row,
            &user,
            Some(&form),
            Some("Enter an amount and a currency.".to_string()),
        )
        .await;
    }

    // Selection-time validation first, entirely independent of whether any
    // provider can actually price it - see `crate::currencies`'s own doc
    // comment and `api::v1::orders::create`'s matching check for why
    // "unknown currency" and "unsupported currency" are kept as distinct
    // messages rather than collapsed into one.
    let wanted = currency.to_string();
    let currency_known = state
        .db
        .read(move |db| crate::currencies::is_known_currency(db, &wanted))
        .await;
    match currency_known {
        Ok(true) => {}
        Ok(false) => {
            return render_create_order_page(
                &state,
                row,
                &user,
                Some(&form),
                Some(format!("unknown currency: {currency}")),
            )
            .await
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }

    // The engine has no concept of currency - monokulo's own exchange rate
    // does the same computation `api::v1::orders::create` does for a real
    // storefront call. `"XMR"`
    // always uses the trivial identity rate regardless of this store's
    // chosen ordered `fx_providers`; every other currency goes through it.
    let (piconero_per_unit, provider) = match state
        .exchange_rate
        .piconero_per_unit_for(&row, currency)
        .await
    {
        Ok(Some(result)) => result,
        Ok(None) => {
            return render_create_order_page(
                &state,
                row,
                &user,
                Some(&form),
                Some(format!("unsupported currency: {currency}")),
            )
            .await
        }
        Err(crate::exchange_rate_config::ExchangeRateLookupError::ProviderNotConfigured(_)) => {
            // Not a real failure - this store's provider (or no provider at
            // all) simply can't price this currency on this instance, same
            // user-facing meaning as `Ok(None)` above.
            return render_create_order_page(
                &state,
                row,
                &user,
                Some(&form),
                Some(format!("unsupported currency: {currency}")),
            )
            .await;
        }
        Err(e) => {
            tracing::error!(store.id = %row.id, currency = ?currency, error = %e, "exchange rate lookup failed");
            return render_create_order_page(
                &state,
                row,
                &user,
                Some(&form),
                Some(
                    "Something went wrong looking up the exchange rate. Please try again."
                        .to_string(),
                ),
            )
            .await;
        }
    };
    let xmr_amount_piconero =
        match shared::exchange_rate::compute_order_amount(currency, amount, piconero_per_unit) {
            Ok(amount) => amount,
            Err(e) => {
                return render_create_order_page(
                    &state,
                    row,
                    &user,
                    Some(&form),
                    Some(e.to_string()),
                )
                .await
            }
        };
    let merchant_order_id = {
        let trimmed = form.merchant_order_id.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };
    let description = match crate::stores::clean_description(&form.description) {
        Ok(description) => description,
        Err(problem) => {
            return render_create_order_page(
                &state,
                row,
                &user,
                Some(&form),
                Some(format!("What it's for must be {}.", problem.requirement())),
            )
            .await
        }
    };
    let show_merchant_order_id = form.shows_merchant_order_id();

    let sk = match decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let resolution = match crate::confirmation_thresholds::resolve_for_order(
        &state,
        &row,
        &policy,
        &sk,
        currency,
        piconero_per_unit,
        xmr_amount_piconero,
    )
    .await
    {
        Ok(resolution) => resolution,
        Err(message) => {
            return render_create_order_page(&state, row, &user, Some(&form), Some(message)).await
        }
    };

    match state
        .engine
        .client
        .create_order(
            &sk,
            shared::xmr_amount::Piconero(xmr_amount_piconero),
            merchant_order_id,
            description,
            Some(resolution.confirmations_required),
            request_key.clone(),
        )
        .await
    {
        Ok(order) => {
            if let Some(key) = request_key {
                let (store_id, order_id) = (id.clone(), order.order_id.clone());
                if let Err(error) = state
                    .db
                    .write(move |db| db.record_dashboard_order_request(&store_id, &key, &order_id))
                    .await
                {
                    tracing::error!(%error, "could not record dashboard order retry");
                }
            }
            let (store_id, order_id, currency, amount, provider) = (
                row.id.clone(),
                order.order_id.clone(),
                currency.to_string(),
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
                        &currency,
                        &amount,
                        shared::xmr_amount::Piconero(piconero_per_unit),
                        &provider,
                        crate::now_unix(),
                        &base_currency,
                        base_rate,
                        confirmations,
                        // The merchant's own signed-in session: as trusted as
                        // the key.
                        true,
                        Some("dashboard"),
                        show_merchant_order_id,
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
            redirect_302(&format!("/stores/{id}/orders/{}", order.order_id))
        }
        Err(EngineClientError::EngineError { status, message })
            if status == reqwest::StatusCode::BAD_REQUEST =>
        {
            render_create_order_page(&state, row, &user, Some(&form), Some(message)).await
        }
        Err(_) => {
            render_create_order_page(
                &state,
                row,
                &user,
                Some(&form),
                Some("Something went wrong. Please try again.".to_string()),
            )
            .await
        }
    }
}

#[derive(Deserialize)]
pub struct MoveKeyStorageForm {
    pub backend: String,
    #[serde(default)]
    pub view_key_hex: String,
    #[serde(default)]
    pub spend_pubkey_hex: String,
    /// The keys encrypted for SEV-SNP key storage (`key_entry`), instead of
    /// the two above.
    #[serde(default)]
    pub encrypted_keys: Option<String>,
}

/// `POST /stores/{id}/settings/key-custody` - moves the store's
/// keys to another key custody backend (task 5.6). The engine checks the
/// keys are this store's own wallet's before anything moves; a rejection
/// re-renders the page with the reason and empty key fields.
pub async fn move_key_storage(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<crate::db::ConnectionId>,
    Form(form): Form<MoveKeyStorageForm>,
) -> Response {
    const SECTION: StoreSection = StoreSection::KeyStorage;
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let sk = match decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let backend = form.backend.trim();
    let keys = match super::key_entry::store_keys(
        &state,
        Some(backend),
        &form.view_key_hex,
        &form.spend_pubkey_hex,
        form.encrypted_keys.as_deref(),
    ) {
        Ok((_, keys)) => keys,
        Err(message) => {
            return render_store_settings_page(
                &state,
                row,
                &user,
                Some(message),
                None,
                Some(SECTION),
            )
            .await
        }
    };
    match state
        .engine.client
        .switch_key_custody(&sk, backend, &keys)
        .await
    {
        Ok(_) => {
            // The engine's status (and so any "keys unavailable" alert) is
            // re-read on the next page rather than waiting out the cache.
            super::status_page::invalidate_status_cache(&state.engine);
            saved(&id, SECTION)
        }
        Err(EngineClientError::EngineError { status, message }) if status == reqwest::StatusCode::BAD_REQUEST => {
            render_store_settings_page(&state, row, &user, Some(message), None, Some(SECTION)).await
        }
        Err(_) => {
            render_store_settings_page(&state, row, &user, Some("Something went wrong moving the keys. Check where they are kept below, and try again if they haven't moved.".to_string()), None, Some(SECTION))
                .await
        }
    }
}

#[derive(Deserialize)]
pub struct ChangeWalletForm {
    #[serde(default)]
    pub wallet_id: String,
    /// `yes` once the change was confirmed; without it the section asks.
    #[serde(default)]
    pub confirm: String,
}

/// `POST /stores/{id}/settings/wallet` - changes the wallet the
/// store takes payments into. Picking a wallet first shows what will happen, with the
/// store's open orders counted; confirming makes the change in the engine
/// and then records the new period here. A wallet on another network is
/// refused, here and by the engine.
pub async fn change_store_wallet(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<crate::db::ConnectionId>,
    Form(form): Form<ChangeWalletForm>,
) -> Response {
    const SECTION: StoreSection = StoreSection::Wallet;
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let refuse = |message: String, row| {
        let (state, user) = (&state, &user);
        async move {
            render_store_settings_page(state, row, user, Some(message), None, Some(SECTION)).await
        }
    };
    let (user_id, wallet_id) = (
        user.id.clone(),
        crate::db::WalletId::new(form.wallet_id.trim().to_owned()),
    );
    let chosen = state
        .db
        .read(move |db| db.get_wallet(&user_id, &wallet_id))
        .await
        .ok()
        .flatten()
        .filter(|w| w.retired_at.is_none());
    let Some(chosen) = chosen else {
        return refuse("Choose one of your wallets.".to_owned(), row).await;
    };
    let current = match &row.wallet_id {
        Some(current_id) => {
            let (user_id, current_id) = (user.id.clone(), current_id.clone());
            state
                .db
                .read(move |db| db.get_wallet(&user_id, &current_id))
                .await
                .ok()
                .flatten()
        }
        None => None,
    };
    if current.as_ref().is_some_and(|c| c.id == chosen.id) {
        // Picked the one it already uses: nothing to ask.
        return render_store_settings_page(&state, row, &user, None, None, Some(SECTION)).await;
    }
    if let Some(current) = &current {
        if current.network != chosen.network {
            return refuse(
                format!(
                    "{} is a {} wallet and this store takes {} payments. A store's network can't change.",
                    chosen.name, chosen.network, current.network
                ),
                row,
            )
            .await;
        }
    }
    let sk = match decrypt_sk(&state.encryption_key, &row) {
        Ok(sk) => sk,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if form.confirm != "yes" {
        let open_orders = state
            .engine
            .client
            .list_orders_page(&sk, true, None, 200, 0)
            .await
            .ok()
            .map(|orders| orders.len());
        let pending = views::store_settings::PendingWalletChange {
            wallet_id: chosen.id.to_string(),
            wallet_name: chosen.name.clone(),
            open_orders,
        };
        return render_store_settings_page_with(
            &state,
            row,
            &user,
            PageState {
                wallet: WalletState {
                    pending: Some(pending),
                    changed: None,
                },
                tab: Some(StoreTab::Wallet),
                ..Default::default()
            },
        )
        .await;
    }
    let moved = match state
        .engine
        .client
        .change_wallet(&sk, &chosen.engine_wallet_id)
        .await
    {
        Ok(answer) => answer.orders_on_previous_wallet,
        Err(EngineClientError::EngineError { status, message })
            if status == reqwest::StatusCode::CONFLICT
                || status == reqwest::StatusCode::NOT_FOUND =>
        {
            return refuse(format!("The wallet couldn't be changed: {message}."), row).await;
        }
        Err(e) => {
            tracing::error!(store.id = %row.id, error = %e, "the engine could not change a store's wallet");
            return refuse(
                "The wallet couldn't be changed right now. Nothing changed; try again in a minute."
                    .to_owned(),
                row,
            )
            .await;
        }
    };
    let (connection_id, wallet_id) = (row.id.clone(), chosen.id.clone());
    if let Err(e) = state
        .db
        .write(move |db| db.change_store_wallet(&connection_id, &wallet_id, crate::now_unix()))
        .await
    {
        // The engine changed it: the next page load shows the engine's
        // wallet, so say so rather than pretending nothing happened.
        tracing::error!(store.id = %row.id, error = %e, "the store's new wallet could not be recorded");
    }
    super::status_page::invalidate_status_cache(&state.engine);
    let changed = match (&current, moved) {
        (Some(current), 0) => format!(
            "Changed to {}. No orders were left on {}.",
            chosen.name, current.name
        ),
        (Some(current), 1) => format!(
            "Changed to {}. The 1 order it took on {} is still watched there.",
            chosen.name, current.name
        ),
        (Some(current), n) => format!(
            "Changed to {}. The {n} orders it took on {} are still watched there.",
            chosen.name, current.name
        ),
        (None, _) => format!("This store now takes payments into {}.", chosen.name),
    };
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(fresh)) => fresh,
        _ => row,
    };
    // The page itself, not a redirect: what happened to the orders is said
    // once, in the card.
    render_store_settings_page_with(
        &state,
        row,
        &user,
        PageState {
            outcome: Some(StoreOutcome::Saved(vec![SECTION])),
            wallet: WalletState {
                pending: None,
                changed: Some(changed),
            },
            ..Default::default()
        },
    )
    .await
}

/// Every provider this instance offers, for the store settings form: the
/// store's enabled ones first in the store's own order, then the rest in the
/// instance's order. `selected` means "the store uses it".
pub(super) fn fx_provider_options(
    available: &[&'static str],
    enabled: &[String],
) -> Vec<views::store_settings::FxProviderOption> {
    let mut options: Vec<views::store_settings::FxProviderOption> = enabled
        .iter()
        .filter(|name| available.contains(&name.as_str()))
        .map(|name| views::store_settings::FxProviderOption {
            name: name.clone(),
            selected: true,
        })
        .collect();
    for name in available {
        if !options.iter().any(|o| o.name == *name) {
            options.push(views::store_settings::FxProviderOption {
                name: name.to_string(),
                selected: false,
            });
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
pub(super) fn parse_fx_providers_form(
    available: &[&'static str],
    form: &HashMap<String, String>,
) -> Result<Vec<String>, String> {
    for key in form.keys() {
        let Some(name) = key
            .strip_prefix("use_")
            .or_else(|| key.strip_prefix("position_"))
        else {
            continue;
        };
        if !available.contains(&name) {
            return Err(format!(
                "{name:?} is not an available exchange rate provider on this instance."
            ));
        }
    }
    let mut chosen: Vec<(u32, usize, &str)> = available
        .iter()
        .enumerate()
        .filter(|(_, name)| form.get(&format!("use_{name}")).is_some())
        .map(|(index, name)| {
            let position = form
                .get(&format!("position_{name}"))
                .and_then(|p| p.trim().parse::<u32>().ok())
                .unwrap_or(u32::MAX);
            (position, index, *name)
        })
        .collect();
    chosen.sort();
    Ok(chosen
        .into_iter()
        .map(|(_, _, name)| name.to_string())
        .collect())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::engine_client::EngineClient;
    use crate::views::orders::{OrderDetailData, OrderDetailViewModel};

    use super::super::{build_router, AppState};
    use super::{fx_provider_options, parse_fx_providers_form};
    use crate::views;

    use crate::http::test_support::{TEST_SPEND_PUBKEY_HEX, TEST_VIEW_KEY_HEX};
    const TEST_ENCRYPTION_KEY: crate::crypto::AtRestKey = crate::crypto::AtRestKey::new([7u8; 32]);

    /// `"XMR"` deliberately, not a fiat currency: these tests are about the
    /// dashboard's own order-creation/display plumbing, not about exercising
    /// a real (mocked) fiat provider - and an XMR-denominated order needs no
    /// provider configured at all (`XmrIdentityProvider`), so `TEST_STATE`
    /// stays free of any network dependency. `pay.rs`'s own tests are the
    /// ones that genuinely exercise a fiat quote.
    const TEST_CURRENCY: &str = "XMR";
    const TEST_RATE_PICONERO_PER_UNIT: u64 = 1_000_000_000_000;

    async fn test_state_with_real_engine() -> (AppState, engine_test_support::TestEngineHandle) {
        let engine = engine_test_support::TestEngineConfig::new()
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

    /// Same as [`test_state_with_real_engine`], but with a real (if inert)
    /// daemon wired into the engine's own `AppState::daemons` - needed by a
    /// caller that drives `admin::lookup_payment` through a genuine HTTP
    /// round trip, which reads that map unconditionally.
    async fn test_state_with_real_engine_and_admin_lookup_daemon(
    ) -> (AppState, engine_test_support::TestEngineHandle) {
        let engine = engine_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .with_admin_lookup_daemon()
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

    /// Makes a real store for the given session token through store setup
    /// (an XMR store on the test wallet), returning `(store_id, public_key)`.
    async fn create_connection(
        router: &Router,
        state: &AppState,
        session_token: &str,
    ) -> (String, String) {
        let store = crate::http::test_support::store_with_keys(
            router,
            state,
            session_token,
            "https://shop.example.com",
            "XMR",
        )
        .await;
        (store.id, store.public_key)
    }

    /// Seeds a real order directly on the spawned engine through its admin
    /// API (`POST /api/v1/admin/tenant/orders`), authenticated with the store's
    /// own `sk_` decrypted from monokulo's database - the same way monokulo
    /// itself reaches the engine. Deliberately bypasses monokulo's own order
    /// creation, so no local currency metadata exists for the order. 10.00 at
    /// `TEST_RATE_PICONERO_PER_UNIT` (1e12 piconero/USD); the engine only knows XMR.
    async fn seed_real_order(
        state: &AppState,
        engine_addr: std::net::SocketAddr,
        public_key: &str,
    ) -> String {
        let row = state
            .db
            .lock()
            .get_store_connection_by_public_key(public_key)
            .unwrap()
            .expect("connection exists");
        let sk = crate::crypto::decrypt(
            &state.encryption_key,
            crate::crypto::Binding::StoreSecret(row.id.as_str()),
            &row.tenant_secret_token_encrypted,
        )
        .unwrap();
        let response = engine_test_support::engine_http_client()
            .post(format!("http://{engine_addr}/api/v1/admin/tenant/orders"))
            .bearer_auth(sk)
            .json(&serde_json::json!({ "xmr_amount_piconero": 10 * TEST_RATE_PICONERO_PER_UNIT }))
            .send()
            .await
            .expect("seeding a real order against the engine's admin API failed");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::OK,
            "expected the engine to accept the seeded order"
        );
        let body: serde_json::Value = response.json().await.unwrap();
        body.as_object()
            .unwrap()
            .get("order_id")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn dashboard_order_retries_reuse_the_order_and_changed_history_forms_create_another() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session = signed_up_and_logged_in_session_token(
            &router,
            "history@example.com",
            "correct horse battery staple",
        )
        .await;
        let (id, _) = create_connection(&router, &state, &session).await;
        let path = format!("/stores/{id}/orders/new");
        let fields = [
            ("amount", "1.00"),
            ("currency", "XMR"),
            ("request_key", "history-request"),
        ];
        let first = router
            .clone()
            .oneshot(form_post_request(&path, &session, &fields))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::FOUND);
        let retry = router
            .clone()
            .oneshot(form_post_request(&path, &session, &fields))
            .await
            .unwrap();
        assert_eq!(retry.status(), StatusCode::FOUND);
        assert_eq!(retry.headers()["location"], first.headers()["location"]);
        let changed = router
            .clone()
            .oneshot(form_post_request(
                &path,
                &session,
                &[
                    ("amount", "2.00"),
                    ("currency", "XMR"),
                    ("request_key", "history-request"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(changed.status(), StatusCode::FOUND);
        assert_ne!(changed.headers()["location"], first.headers()["location"]);
        let changed_retry = router
            .oneshot(form_post_request(
                &path,
                &session,
                &[
                    ("amount", "2.00"),
                    ("currency", "XMR"),
                    ("request_key", "history-request"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(
            changed_retry.headers()["location"],
            changed.headers()["location"]
        );
    }

    #[tokio::test]
    async fn orders_list_shows_a_real_order_seeded_against_the_engines_public_api() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "orders-owner@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) = create_connection(&router, &state, &session_token).await;

        let order_id = seed_real_order(&state, engine.addr, &public_key).await;

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/orders"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains(&order_id),
            "expected the seeded order's order_id in the response, got: {html}"
        );
        assert!(
            html.contains(r##"fx-target="#orders-results" fx-push-url"##),
            "the search is enhanced: {html}"
        );

        // A fixi search gets only the results, filtered.
        let search = |q: &str| {
            fixi(
                Request::builder()
                    .uri(format!("/stores/{connection_id}/orders?q={q}"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
        };
        let html = body_text(router.clone().oneshot(search(&order_id)).await.unwrap()).await;
        assert!(
            html.starts_with(r#"<div id="orders-results">"#) && html.contains(&order_id),
            "{html}"
        );
        let html = body_text(
            router
                .clone()
                .oneshot(search("no-such-order"))
                .await
                .unwrap(),
        )
        .await;
        assert!(
            html.contains("No orders match") && !html.contains("<html"),
            "{html}"
        );
    }

    /// With JavaScript, the order detail page streams its changing part
    /// (structured_logging.md part 6): the page names the stream, and the
    /// stream starts with that part, routed to replace itself.
    #[tokio::test]
    async fn order_detail_streams_its_live_part_to_its_owner_only() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "order-events@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) = create_connection(&router, &state, &session_token).await;
        let order_id = seed_real_order(&state, engine.addr, &public_key).await;
        let get = |uri: String, token: &str| {
            Request::builder()
                .uri(uri)
                .header("cookie", format!("session={token}"))
                .header("host", "test.example")
                .body(Body::empty())
                .unwrap()
        };

        let page = body_text(
            router
                .clone()
                .oneshot(get(
                    format!("/stores/{connection_id}/orders/{order_id}"),
                    &session_token,
                ))
                .await
                .unwrap(),
        )
        .await;
        let events_url = format!("/stores/{connection_id}/orders/{order_id}/events");
        assert!(
            page.contains(&format!(
                r#"fx-action="{events_url}" fx-trigger="fx:inited""#
            )),
            "{page}"
        );
        assert!(page.contains(r#"<div id="order-live">"#), "{page}");

        let response = router
            .clone()
            .oneshot(get(events_url.clone(), &session_token))
            .await
            .unwrap();
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let mut body = response.into_body();
        let mut text = String::new();
        while !text.contains("\n\n") {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(60), body.frame())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let Some(data) = frame.data_ref() {
                text.push_str(std::str::from_utf8(data).unwrap());
            }
        }
        assert!(
            text.starts_with(r##"event: {"target":"#order-live","swap":"outerHTML"}"##),
            "{text}"
        );
        assert!(
            text.contains(r#"data: <div id="order-live">"#) && text.contains(&order_id),
            "{text}"
        );

        let stranger = signed_up_and_logged_in_session_token(
            &router,
            "order-events-stranger@example.com",
            "correct horse battery staple",
        )
        .await;
        let response = router.oneshot(get(events_url, &stranger)).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "someone else's store"
        );
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
        let (connection_id, public_key) = create_connection(&router, &state, &session_token).await;

        let order_id = seed_real_order(&state, engine.addr, &public_key).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/orders/{order_id}"))
                    .header("cookie", format!("session={session_token}"))
                    .header("host", "test.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains(&order_id),
            "expected the order's order_id in its detail page, got: {html}"
        );
        // Seeded directly against the engine's own admin API, bypassing
        // monokulo's own order creation - no local fiat metadata
        // was ever recorded for it, so the page must show a dash rather than
        // a fabricated amount (see `checkout.rs`'s own doc comment on the
        // same fallback).
        assert!(
            html.contains('—'),
            "expected a dash placeholder for the missing fiat quote, got: {html}"
        );
        // Timestamps deliberately stay compact (raw Unix seconds), not a
        // human-readable date - a user-requested reversion of an earlier
        // attempt at this page.
        assert!(
            html.contains("Created at"),
            "expected the created-at row present, got: {html}"
        );
        // `merchant_order_id` was never set on this seeded order - must show
        // a muted placeholder, not a blank cell.
        assert!(
            html.contains("muted"),
            "expected a muted placeholder for the unset merchant order id, got: {html}"
        );
        assert!(
            !html.contains(r#"http-equiv="refresh""#),
            "a point-in-time page without JS, never a meta refresh: {html}"
        );
        assert!(
            html.contains(r#"aria-label="Refresh page" title="Refresh page" data-reload"#),
            "a Reload button instead: {html}"
        );
        // The point: a real, absolute, shareable
        // payment link for this exact order, built from the request's own
        // Host header (`test.example` here, set by `oneshot`'s default) -
        // not a placeholder or a bare relative path.
        assert!(
            html.contains(&format!("http://test.example/checkout/{order_id}/share")),
            "expected a real absolute payment link, got: {html}"
        );
        // The share icon and order id live in the order title.
        assert!(
            html.contains(r#"<h1>Order <span class="order-title-id">"#) && html.contains(r#"<div class="mk-page-actions"><a class="mk-btn" href="http://test.example/checkout/"#),
            "expected the title banner to carry the share button, got: {html}"
        );
        assert!(
            html.contains(&format!(
                r#"<span class="order-title-id"><span class="short-value" title="{order_id}"><span class="short-value-text" aria-hidden="true">{}</span>"#,
                views::short_id_text(order_id.as_str())
            )),
            "expected the order id inside the title banner, got: {html}"
        );
        assert!(
            html.contains(r#"id="share-payment-link""#) && html.contains("</svg>Share</a>"),
            "expected a real, labeled share button, got: {html}"
        );
        assert!(
            !html.contains("<th>Payment link</th>"),
            "the payment link must no longer be its own table row, got: {html}"
        );
    }

    #[tokio::test]
    async fn order_detail_for_an_unknown_order_id_renders_a_clear_not_found_state() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "order-detail-missing@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/orders/no-such-payment-id"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "expected a real 404, not a raw 500"
        );
        let html = body_text(response).await;
        assert!(
            html.to_lowercase().contains("not found"),
            "expected a clear not-found state, got: {html}"
        );
    }

    #[tokio::test]
    async fn webhooks_list_on_a_connection_with_none_registered_renders_an_empty_but_valid_page() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "webhooks-empty@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/settings?tab=connections"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("No webhooks yet."),
            "expected a real, valid settings page even with no webhooks, got: {html}"
        );
    }

    #[tokio::test]
    async fn a_different_user_hitting_the_first_users_connection_gets_404_not_their_data() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let owner_token = signed_up_and_logged_in_session_token(
            &router,
            "cross-user-owner@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) = create_connection(&router, &state, &owner_token).await;
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
                    .uri(format!("/stores/{connection_id}/orders"))
                    .header("cookie", format!("session={other_token}"))
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
        let router = build_router(state.clone());

        // A connection id doesn't even need to exist for this - `AuthedUser`
        // must reject before the handler ever looks it up.
        let fake_id = "nonexistent-connection-id";

        for uri in [
            format!("/stores/{fake_id}/orders"),
            format!("/stores/{fake_id}/orders/some-payment-id"),
            format!("/stores/{fake_id}/settings"),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(uri.clone())
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "expected 401 for {uri} with no session"
            );
        }
    }

    #[tokio::test]
    async fn store_detail_shows_the_real_connected_stores_overview_and_integration_help() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "store-detail@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) = create_connection(&router, &state, &session_token).await;
        let order_id = seed_real_order(&state, engine.addr, &public_key).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;

        assert!(
            html.contains(&public_key),
            "expected the store's public key, got: {html}"
        );
        assert!(
            html.contains("<h1>https://shop.example.com</h1>"),
            "the store's name heads its page, got: {html}"
        );
        assert!(
            html.contains("tag-ok"),
            "the engine is genuinely reachable, so health must render as ok, got: {html}"
        );
        assert!(
            html.contains(&order_id),
            "expected the seeded order in the recent-orders list, got: {html}"
        );
        // Help is the docs, which say how to add the checkout.
        assert!(
            html.contains(crate::views::DOCS_URL),
            "expected the Help link to the docs, got: {html}"
        );
    }

    #[tokio::test]
    async fn store_detail_for_an_unowned_or_unknown_connection_returns_404() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let owner_token = signed_up_and_logged_in_session_token(
            &router,
            "store-detail-owner@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &owner_token).await;

        let other_token = signed_up_and_logged_in_session_token(
            &router,
            "store-detail-intruder@example.com",
            "correct horse battery staple",
        )
        .await;

        for uri in [
            format!("/stores/{connection_id}"),
            "/stores/nonexistent".to_string(),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(uri.clone())
                        .header("cookie", format!("session={other_token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "expected 404 for {uri}"
            );
        }
    }

    use crate::http::test_support::urlencoding_encode;

    /// A state whose only rate provider is Haveno (never contacted by these
    /// tests - nothing here asks for a quote).
    async fn haveno_state() -> (AppState, engine_test_support::TestEngineHandle) {
        let (mut state, engine) = test_state_with_real_engine().await;
        state.exchange_rate = std::sync::Arc::new(
            crate::exchange_rate_config::ExchangeRateProviders::haveno_only("http://127.0.0.1:1"),
        );
        (state, engine)
    }

    async fn get_with_session(router: &Router, uri: &str, session: &str) -> String {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header("cookie", format!("session={session}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        body_text(response).await
    }

    fn haveno_form<'a>(
        currencies: &'a str,
        spread: &'a str,
        offers: &'a str,
        depth: &'a str,
    ) -> Vec<(&'a str, &'a str)> {
        vec![
            ("use_haveno", "on"),
            ("position_haveno", "1"),
            ("haveno_currencies", currencies),
            ("haveno_max_spread_pct", spread),
            ("haveno_min_offers_per_side", offers),
            ("haveno_min_depth_xmr_per_side", depth),
        ]
    }

    #[tokio::test]
    async fn saving_the_providers_waits_for_the_stores_policy_lock() {
        let (state, _engine) = haveno_state().await;
        let router = build_router(state.clone());
        let session = signed_up_and_logged_in_session_token(
            &router,
            "fx-policy-lock@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, pk) = create_connection(&router, &state, &session).await;
        let fx_uri = format!("/stores/{connection_id}/settings");

        let guard = crate::confirmation_thresholds::lock_policy(&pk).await;
        let request = form_post_request(&fx_uri, &session, &haveno_form("eur", "2.5", "3", "1.5"));
        let mut task = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut task)
                .await
                .is_err(),
            "the save waits while an order is being priced"
        );
        drop(guard);
        let response = task.await.unwrap();
        // Saved: a plain form post is redirected; a rejected one would be
        // re-rendered with 200, so 200 is not accepted here.
        assert!(
            response.status().is_redirection(),
            "got {}",
            response.status()
        );
    }

    #[tokio::test]
    async fn the_haveno_limits_are_shown_and_saved_with_the_provider_list() {
        let (state, _engine) = haveno_state().await;
        let router = build_router(state.clone());
        let session = signed_up_and_logged_in_session_token(
            &router,
            "haveno-limits@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _pk) = create_connection(&router, &state, &session).await;
        let fx_uri = format!("/stores/{connection_id}/settings");
        let settings_uri = format!("{fx_uri}?tab=payments");

        let html = get_with_session(&router, &settings_uri, &session).await;
        assert!(
            html.contains("Haveno (RetoSwap) limits"),
            "the limits are offered while haveno is: {html}"
        );
        assert!(
            html.contains(r#"name="haveno_max_spread_pct" id="haveno-max-spread-pct" value="5""#),
            "defaults are shown: {html}"
        );

        let response = router
            .clone()
            .oneshot(form_post_request(
                &fx_uri,
                &session,
                &haveno_form("eur, $", "2.5", "3", "1.5"),
            ))
            .await
            .unwrap();
        assert!(
            response.status().is_redirection() || response.status() == StatusCode::OK,
            "got {}",
            response.status()
        );

        let row = state
            .db
            .lock()
            .get_store_connection_by_id(&shared::ids::ConnectionId::new(connection_id.to_string()))
            .unwrap()
            .unwrap();
        assert_eq!(row.fx_providers, vec!["haveno"]);
        let haveno = row.fx_provider_settings.haveno;
        assert_eq!(
            haveno.currencies,
            vec!["EUR", "USD"],
            "tickers resolve to canonical codes"
        );
        assert_eq!(
            (
                haveno.max_spread_pct,
                haveno.min_offers_per_side,
                haveno.min_depth_xmr_per_side
            ),
            (2.5, 3, 1.5)
        );

        let html = get_with_session(&router, &settings_uri, &session).await;
        assert!(
            html.contains(r#"name="haveno_currencies" id="haveno-currencies" value="EUR, USD""#),
            "the saved values come back: {html}"
        );
        assert!(
            html.contains(r#"name="haveno_max_spread_pct" id="haveno-max-spread-pct" value="2.5""#),
            "{html}"
        );
    }

    #[tokio::test]
    async fn an_invalid_haveno_limit_is_rejected_and_nothing_is_saved() {
        let (state, _engine) = haveno_state().await;
        let router = build_router(state.clone());
        let session = signed_up_and_logged_in_session_token(
            &router,
            "haveno-invalid@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _pk) = create_connection(&router, &state, &session).await;
        let fx_uri = format!("/stores/{connection_id}/settings");
        let before = state
            .db
            .lock()
            .get_store_connection_by_id(&shared::ids::ConnectionId::new(connection_id.to_string()))
            .unwrap()
            .unwrap();

        for (form, expected) in [
            (
                haveno_form("USD, ZZZ", "5", "1", "0"),
                "not a known currency",
            ),
            (
                haveno_form("USD, XMR", "5", "1", "0"),
                "XMR is never priced",
            ),
            (haveno_form("", "0", "1", "0"), "Maximum spread"),
            (haveno_form("", "5", "0", "0"), "Minimum offers"),
            (haveno_form("", "5", "1", "-1"), "Minimum XMR"),
        ] {
            let response = router
                .clone()
                .oneshot(form_post_request(&fx_uri, &session, &form))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "a rejected form re-renders the page"
            );
            let html = body_text(response).await;
            assert!(
                html.contains(expected),
                "expected {expected:?}, got: {html}"
            );
            let after = state
                .db
                .lock()
                .get_store_connection_by_id(&shared::ids::ConnectionId::new(
                    connection_id.to_string(),
                ))
                .unwrap()
                .unwrap();
            assert_eq!(
                after.fx_providers, before.fx_providers,
                "nothing is saved on a rejected form ({expected})"
            );
            assert_eq!(
                after.fx_provider_settings, before.fx_provider_settings,
                "nothing is saved on a rejected form ({expected})"
            );
        }
    }

    #[tokio::test]
    async fn a_form_without_the_haveno_fields_keeps_the_stores_haveno_limits() {
        let (state, _engine) = haveno_state().await;
        let router = build_router(state.clone());
        let session = signed_up_and_logged_in_session_token(
            &router,
            "haveno-keep@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _pk) = create_connection(&router, &state, &session).await;
        let fx_uri = format!("/stores/{connection_id}/settings");
        router
            .clone()
            .oneshot(form_post_request(
                &fx_uri,
                &session,
                &haveno_form("USD", "2", "2", "1"),
            ))
            .await
            .unwrap();

        // Only the provider fields: the limits are not part of this submission.
        router
            .clone()
            .oneshot(form_post_request(
                &fx_uri,
                &session,
                &[("use_haveno", "on"), ("position_haveno", "1")],
            ))
            .await
            .unwrap();

        let haveno = state
            .db
            .lock()
            .get_store_connection_by_id(&shared::ids::ConnectionId::new(connection_id.to_string()))
            .unwrap()
            .unwrap()
            .fx_provider_settings
            .haveno;
        assert_eq!(
            (
                haveno.currencies,
                haveno.max_spread_pct,
                haveno.min_offers_per_side
            ),
            (vec!["USD".to_string()], 2.0, 2)
        );
    }

    #[tokio::test]
    async fn haveno_fields_are_ignored_and_not_shown_when_the_instance_does_not_offer_haveno() {
        let (mut state, _engine) = test_state_with_real_engine().await;
        state.exchange_rate = std::sync::Arc::new(
            crate::exchange_rate_config::ExchangeRateProviders::coingecko_only(
                "http://127.0.0.1:1",
            ),
        );
        let router = build_router(state.clone());
        let session = signed_up_and_logged_in_session_token(
            &router,
            "haveno-off@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _pk) = create_connection(&router, &state, &session).await;

        let html = get_with_session(
            &router,
            &format!("/stores/{connection_id}/settings?tab=payments"),
            &session,
        )
        .await;
        assert!(
            !html.contains("haveno_max_spread_pct"),
            "no haveno limits without haveno: {html}"
        );

        let fx_uri = format!("/stores/{connection_id}/settings");
        let mut form = haveno_form("USD", "1", "1", "0");
        form.retain(|(k, _)| k.starts_with("haveno_"));
        form.push(("use_coingecko", "on"));
        router
            .clone()
            .oneshot(form_post_request(&fx_uri, &session, &form))
            .await
            .unwrap();
        let row = state
            .db
            .lock()
            .get_store_connection_by_id(&shared::ids::ConnectionId::new(connection_id.to_string()))
            .unwrap()
            .unwrap();
        assert_eq!(
            row.fx_provider_settings,
            crate::fx_provider_settings::FxProviderSettings::default(),
            "forged haveno fields change nothing"
        );
        assert_eq!(row.fx_providers, vec!["coingecko"]);
    }

    #[tokio::test]
    async fn the_order_currency_dropdown_for_a_haveno_store_is_its_own_list_without_asking_haveno()
    {
        let (state, _engine) = haveno_state().await; // haveno at an address nothing listens on
        let router = build_router(state.clone());
        let session = signed_up_and_logged_in_session_token(
            &router,
            "haveno-dropdown@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _pk) = create_connection(&router, &state, &session).await;
        let fx_uri = format!("/stores/{connection_id}/settings");
        let orders_uri = format!("/stores/{connection_id}/orders/new");

        // Empty list: every currency in the table.
        router
            .clone()
            .oneshot(form_post_request(
                &fx_uri,
                &session,
                &haveno_form("", "5", "1", "0"),
            ))
            .await
            .unwrap();
        let html = get_with_session(&router, &orders_uri, &session).await;
        for code in ["XMR", "USD", "EUR", "GBP"] {
            assert!(
                html.contains(&format!(
                    r#"<option value="{code}"{}>{code}</option>"#,
                    if code == "XMR" { " selected" } else { "" }
                )),
                "{code} missing: {html}"
            );
        }

        // A list: only those (and XMR).
        router
            .clone()
            .oneshot(form_post_request(
                &fx_uri,
                &session,
                &haveno_form("EUR", "5", "1", "0"),
            ))
            .await
            .unwrap();
        let html = get_with_session(&router, &orders_uri, &session).await;
        assert!(
            html.contains(r#"<option value="EUR">EUR</option>"#)
                && html.contains(r#"<option value="XMR" selected>XMR</option>"#),
            "{html}"
        );
        assert!(
            !html.contains(r#"<option value="USD">USD</option>"#),
            "USD is off the store's list: {html}"
        );
    }

    fn form(fields: &[(&str, &str)]) -> HashMap<String, String> {
        fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_provider_form_yields_the_ticked_providers_in_position_order() {
        let available = ["coingecko", "coinmarketcap"];
        let both_swapped = form(&[
            ("use_coingecko", "on"),
            ("position_coingecko", "2"),
            ("use_coinmarketcap", "on"),
            ("position_coinmarketcap", "1"),
        ]);
        assert_eq!(
            parse_fx_providers_form(&available, &both_swapped).unwrap(),
            vec!["coinmarketcap", "coingecko"]
        );

        // An unticked provider is off whatever its position says.
        let one_off = form(&[
            ("use_coinmarketcap", "on"),
            ("position_coinmarketcap", "1"),
            ("position_coingecko", "1"),
        ]);
        assert_eq!(
            parse_fx_providers_form(&available, &one_off).unwrap(),
            vec!["coinmarketcap"]
        );

        // Ties and unreadable positions fall back to the instance's order.
        let tied = form(&[
            ("use_coingecko", "on"),
            ("position_coingecko", "1"),
            ("use_coinmarketcap", "on"),
            ("position_coinmarketcap", "1"),
        ]);
        assert_eq!(
            parse_fx_providers_form(&available, &tied).unwrap(),
            vec!["coingecko", "coinmarketcap"]
        );
        let junk = form(&[
            ("use_coingecko", "on"),
            ("position_coingecko", "abc"),
            ("use_coinmarketcap", "on"),
            ("position_coinmarketcap", "3"),
        ]);
        assert_eq!(
            parse_fx_providers_form(&available, &junk).unwrap(),
            vec!["coinmarketcap", "coingecko"]
        );

        assert!(
            parse_fx_providers_form(&available, &form(&[]))
                .unwrap()
                .is_empty(),
            "everything unticked turns every provider off"
        );
    }

    #[test]
    fn the_provider_form_rejects_a_provider_the_instance_does_not_offer() {
        let err =
            parse_fx_providers_form(&["coingecko"], &form(&[("use_haveno", "on")])).unwrap_err();
        assert!(
            err.contains("not an available exchange rate provider"),
            "{err}"
        );
        assert!(
            parse_fx_providers_form(&["coingecko"], &form(&[("position_haveno", "1")])).is_err()
        );
    }

    #[test]
    fn the_provider_options_list_the_stores_choices_first_in_its_order_then_the_rest() {
        let names = |options: Vec<views::store_settings::FxProviderOption>| -> Vec<(String, bool)> {
            options.into_iter().map(|o| (o.name, o.selected)).collect()
        };
        let available = ["coingecko", "coinmarketcap"];
        assert_eq!(
            names(fx_provider_options(
                &available,
                &["coinmarketcap".to_string()]
            )),
            vec![
                ("coinmarketcap".to_string(), true),
                ("coingecko".to_string(), false)
            ]
        );
        // A stale name the instance no longer offers is dropped.
        assert_eq!(
            names(fx_provider_options(
                &available,
                &["fixed".to_string(), "coingecko".to_string()]
            )),
            vec![
                ("coingecko".to_string(), true),
                ("coinmarketcap".to_string(), false)
            ]
        );
    }

    fn form_post_request(uri: &str, session: &str, fields: &[(&str, &str)]) -> Request<Body> {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("cookie", format!("session={session}"))
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn creating_an_order_from_the_dashboard_redirects_to_its_real_detail_page() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "order-create@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/orders/new"),
                &session_token,
                &[
                    ("amount", "10.00"),
                    ("currency", TEST_CURRENCY),
                    ("merchant_order_id", "order-5678"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "expected a redirect to the new order's own detail page"
        );
        let location = response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(
            location.starts_with(&format!("/stores/{connection_id}/orders/")),
            "expected a redirect into this store's own orders, got: {location}"
        );

        let detail_response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(location)
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail_response.status(), StatusCode::OK);
        let html = body_text(detail_response).await;
        assert!(
            html.contains(TEST_CURRENCY),
            "expected the real, just-created order's detail page, got: {html}"
        );
        // A real, previously-missing capability: `EngineClient::create_order`
        // used to silently drop `merchant_order_id` no matter what the
        // dashboard's own "create an order" form submitted - every order's
        // own field always showed as unset regardless of caller intent.
        assert!(
            html.contains("order-5678"),
            "expected the real merchant_order_id shown, not a dash, got: {html}"
        );
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

    /// The form's "What it's for" and "Show the reference to the payer":
    /// one too long comes back with what was typed and the switch as it
    /// was; a good one makes an order whose page shows its description,
    /// and its reference marked as hidden from the payer.
    #[tokio::test]
    async fn the_create_order_form_describes_the_order_and_can_hide_its_reference() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "order-describe@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;
        let path = format!("/stores/{connection_id}/orders/new");
        let too_long = "x".repeat(141);
        let response = router
            .clone()
            .oneshot(form_post_request(
                &path,
                &session_token,
                &[
                    ("amount", "10.00"),
                    ("currency", TEST_CURRENCY),
                    ("description", &too_long),
                    ("merchant_order_id", "inv-77"),
                    // The switch drawn, and off.
                    ("switches", "show_merchant_order_id"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("What it's for must be at most 140 characters."),
            "{html}"
        );
        assert!(html.contains(&format!(r#"value="{too_long}""#)), "{html}");
        assert!(
            html.contains(r#"id="show-merchant-order-id" aria-describedby"#),
            "the switch stays off: {html}"
        );

        let response = router
            .clone()
            .oneshot(form_post_request(
                &path,
                &session_token,
                &[
                    ("amount", "10.00"),
                    ("currency", TEST_CURRENCY),
                    ("description", "  Table 4: two coffees "),
                    ("merchant_order_id", "inv-77"),
                    ("switches", "show_merchant_order_id"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let location = response.headers()["location"].to_str().unwrap().to_string();
        let html = body_text(
            router
                .oneshot(
                    Request::builder()
                        .uri(location)
                        .header("cookie", format!("session={session_token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert!(html.contains(">Description</th>"), "{html}");
        assert!(html.contains("Table 4: two coffees"), "{html}");
        assert!(html.contains("inv-77"), "{html}");
        assert!(html.contains("(hidden from the payer)"), "{html}");
    }

    #[tokio::test]
    async fn creating_an_order_with_an_unknown_currency_shows_a_clear_error() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "order-create-bad-currency@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/orders/new"),
                &session_token,
                &[("amount", "10.00"), ("currency", "NOTREAL")],
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a validation error re-renders the page, it doesn't redirect"
        );
        let html = body_text(response).await;
        assert!(
            html.contains("unknown currency"),
            "expected a clear unknown-currency error surfaced, got: {html}"
        );
        assert!(
            html.contains("<form"),
            "the create-order form must still be present, got: {html}"
        );
    }

    /// The other half of the same two-stage split - `USD` is a real, known
    /// currency; this test's own `test_state_with_real_engine` just has no
    /// rate provider enabled at all. Must surface as a distinct
    /// "unsupported", not "unknown", currency error.
    #[tokio::test]
    async fn creating_an_order_with_a_known_but_provider_unsupported_currency_gets_a_distinct_error(
    ) {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "order-create-unsupported-currency@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/orders/new"),
                &session_token,
                &[("amount", "10.00"), ("currency", "USD")],
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a validation error re-renders the page, it doesn't redirect"
        );
        let html = body_text(response).await;
        assert!(
            html.contains("unsupported currency"),
            "expected a clear unsupported-currency error surfaced, got: {html}"
        );
    }

    #[tokio::test]
    async fn a_different_user_cannot_create_an_order_on_someone_elses_connection() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let owner_token = signed_up_and_logged_in_session_token(
            &router,
            "order-create-owner@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &owner_token).await;

        let intruder_token = signed_up_and_logged_in_session_token(
            &router,
            "order-create-intruder@example.com",
            "correct horse battery staple",
        )
        .await;

        let response = router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/orders/new"),
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
    #[cfg(feature = "snp")]
    async fn test_state_with_two_custody_backends(
    ) -> (AppState, engine_test_support::TestEngineHandle) {
        test_state_with_two_custody_backends_trusting(engine_test_support::snp_test_trust()).await
    }

    /// The same, with this site's key entry trusting `trust`, which may
    /// not be what the engine trusts.
    #[cfg(feature = "snp")]
    async fn test_state_with_two_custody_backends_trusting(
        trust: key_custody::transport::TrustPolicy,
    ) -> (AppState, engine_test_support::TestEngineHandle) {
        let engine = engine_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .with_snp_backend()
            .spawn()
            .await;
        let state = AppState {
            engine: crate::http::Engine::new(EngineClient::embedded_for_tests(engine.router())),
            settings: crate::settings::MonokuloSettings::fixed_with_snp_entry(
                crate::settings::PerRequest {
                    signup_mode: crate::settings::SignupMode::Public,
                    public_uri: String::new(),
                },
                crate::settings::SnpEntryPolicy {
                    trust: Some(trust),
                    required: false,
                },
            ),
            ..AppState::for_tests()
        };
        crate::http::status_page::get_status_cached(&state.engine)
            .await
            .expect("engine status");
        (state, engine)
    }

    async fn get_page(router: &Router, session_token: &str, uri: &str) -> String {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap()
    }

    #[cfg(feature = "snp")]
    async fn engine_backend_of(state: &AppState, public_key: &str) -> Option<String> {
        let row = state
            .db
            .lock()
            .get_store_connection_by_public_key(public_key)
            .unwrap()
            .unwrap();
        let sk = crate::crypto::decrypt(
            &state.encryption_key,
            crate::crypto::Binding::StoreSecret(row.id.as_str()),
            &row.tenant_secret_token_encrypted,
        )
        .unwrap();
        state
            .engine
            .client
            .get_tenant(&shared::auth::RawToken::presented(&sk))
            .await
            .unwrap()
            .key_custody_backend
    }

    /// What `key-custody-cli` does on the merchant's computer, with the
    /// test engine's stand-in for AMD: fetches the bundle the page links,
    /// checks it and encrypts the keys. Returns what it would print.
    #[cfg(feature = "snp")]
    async fn cli_seal(
        router: &Router,
        engine: &engine_test_support::TestEngineHandle,
        html: &str,
        view_key_hex: &str,
    ) -> String {
        let at = html
            .find("/key-custody/bundles/")
            .expect("the page links its bundle");
        let path = &html[at..at + "/key-custody/bundles/".len() + 32];
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let bundle: engine::key_custody::transport::Bundle =
            serde_json::from_slice(&bytes).unwrap();
        engine.seal_keys_for_snp(&bundle, view_key_hex, TEST_SPEND_PUBKEY_HEX)
    }

    /// Without JavaScript: the keys move to SEV-SNP key storage encrypted
    /// with key-custody-cli against the bundle the page links. Keys typed
    /// in the clear for it are refused and not sent on; another wallet's
    /// keys are refused by the engine; nothing moves until the right ones
    /// come.
    #[cfg(feature = "snp")]
    #[tokio::test]
    async fn a_store_can_move_its_keys_to_snp_from_its_settings_page_without_js() {
        let (state, engine) = test_state_with_two_custody_backends().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "key-storage@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) = create_connection(&router, &state, &session_token).await;
        assert_eq!(
            engine_backend_of(&state, &public_key).await.as_deref(),
            Some("plain")
        );

        let settings_uri = format!("/stores/{connection_id}/settings");
        let wallet_tab = format!("{settings_uri}?tab=wallet");
        let html = get_page(&router, &session_token, &wallet_tab).await;
        assert!(
            html.contains(r#"<section id="card-key-storage" class="settings-card" aria-labelledby="card-key-storage-title"><header class="card-head"><h3 id="card-key-storage-title">Key storage</h3>"#),
            "{html}"
        );
        assert!(
            html.contains("In the engine (simplest)"),
            "the current place is described: {html}"
        );
        assert!(
            html.contains(r#"<option value="snp" selected>"#),
            "the other backend is offered: {html}"
        );
        assert!(html.contains("key-custody-cli seal --bundle"), "{html}");
        assert!(html.contains(r#"name="encrypted_keys""#), "{html}");
        assert!(
            !html.contains(TEST_VIEW_KEY_HEX),
            "keys are never echoed back"
        );

        // Typed in the clear: refused here, never sent on.
        let move_uri = format!("{settings_uri}/key-custody");
        let response = router
            .clone()
            .oneshot(form_post_request(
                &move_uri,
                &session_token,
                &[
                    ("backend", "snp"),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                ],
            ))
            .await
            .unwrap();
        let html = body_text(response).await;
        assert!(html.contains("were not sent"), "{html}");
        assert!(!html.contains(TEST_VIEW_KEY_HEX), "{html}");

        // Another wallet's keys, encrypted: the engine refuses them.
        let wrong_view_key = format!("01{}", "0".repeat(62));
        let encrypted = cli_seal(&router, &engine, &html, &wrong_view_key).await;
        let response = router
            .clone()
            .oneshot(form_post_request(
                &move_uri,
                &session_token,
                &[("backend", "snp"), ("encrypted_keys", encrypted.as_str())],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let html = body_text(response).await;
        assert!(html.contains("different wallet"), "{html}");
        assert_eq!(
            engine_backend_of(&state, &public_key).await.as_deref(),
            Some("plain")
        );

        // The store's own keys, encrypted against the page's new bundle.
        let encrypted = cli_seal(&router, &engine, &html, TEST_VIEW_KEY_HEX).await;
        let response = router
            .clone()
            .oneshot(form_post_request(
                &move_uri,
                &session_token,
                &[("backend", "snp"), ("encrypted_keys", encrypted.as_str())],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers().get("location").unwrap(),
            &format!("{settings_uri}?saved=key-storage#card-key-storage")
        );
        assert_eq!(
            engine_backend_of(&state, &public_key).await.as_deref(),
            Some("snp")
        );

        // Straight after the move (which empties the status cache), as the
        // redirect lands.
        let html = get_page(&router, &session_token, &wallet_tab).await;
        assert!(html.contains("AMD SEV-SNP confidential machine"), "{html}");
        assert!(
            html.contains(r#"<option value="plain" selected>"#),
            "and it can move back: {html}"
        );

        // Still takes orders from its new place.
        let order_id = seed_real_order(&state, engine.addr, &public_key).await;
        assert!(!order_id.is_empty());
    }

    /// The key storage choice is made when a wallet is added (its keys are
    /// registered once, for every store on it), and a store on that wallet
    /// keeps its keys there.
    #[cfg(feature = "snp")]
    #[tokio::test]
    async fn a_new_wallet_can_choose_where_its_keys_are_kept_when_there_is_a_choice() {
        let (state, engine) = test_state_with_two_custody_backends().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "key-choice@example.com",
            "correct horse battery staple",
        )
        .await;

        let html = get_page(&router, &session_token, "/account/wallets/import").await;
        assert!(
            html.contains(r#"<select class="mk-input" name="key_custody_backend""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<option value="plain" selected>"#),
            "the default is preselected: {html}"
        );
        assert!(
            html.contains(r#"data-key-custody-backend-field="key_custody_backend""#),
            "the script encrypts only when snp is chosen: {html}"
        );

        let encrypted = cli_seal(&router, &engine, &html, TEST_VIEW_KEY_HEX).await;
        let added = router
            .clone()
            .oneshot(form_post_request(
                "/account/wallets/import",
                &session_token,
                &[
                    ("name", "Kept apart"),
                    ("view_key_hex", ""),
                    ("spend_pubkey_hex", ""),
                    ("encrypted_keys", encrypted.as_str()),
                    ("network", "mainnet"),
                    ("key_custody_backend", "snp"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(added.status(), StatusCode::SEE_OTHER);
        let wallet_id = {
            let db = state.db.lock();
            let user = db
                .get_user_by_email("key-choice@example.com")
                .unwrap()
                .unwrap();
            db.list_wallets(&user.id).unwrap()[0].wallet.id.to_string()
        };

        let response = crate::http::test_support::set_up_store_on_wallet(
            &router,
            &format!("session={session_token}"),
            "Kept apart",
            "kept-apart.example.com",
            wallet_id.as_str(),
        )
        .await;
        crate::http::test_support::store_made(&response);
        let public_key = {
            let db = state.db.lock();
            let user = db
                .get_user_by_email("key-choice@example.com")
                .unwrap()
                .unwrap();
            db.list_store_connections_for_user(&user.id).unwrap()[0]
                .tenant_public_key
                .clone()
        };
        assert_eq!(
            engine_backend_of(&state, &public_key).await.as_deref(),
            Some("snp")
        );
    }

    #[tokio::test]
    async fn an_unknown_bundle_is_not_found_and_the_key_checker_is_served_as_webassembly() {
        let router = build_router(AppState::for_tests());
        let missing = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/key-custody/bundles/{}", "0".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let module = router
            .oneshot(
                Request::builder()
                    .uri(crate::assets::url("key-custody.wasm"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(module.status(), StatusCode::OK);
        assert_eq!(module.headers()["content-type"], "application/wasm");
        let bytes = module.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..4], b"\0asm");
    }

    #[tokio::test]
    async fn with_a_single_key_storage_backend_no_choice_is_shown() {
        let (state, _engine) = test_state_with_real_engine().await;
        crate::http::status_page::get_status_cached(&state.engine)
            .await
            .unwrap();
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "no-choice@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _) = create_connection(&router, &state, &session_token).await;
        assert!(!get_page(
            &router,
            &session_token,
            "/setup/wallet/keys?store_name=x&store_site=x.example&name=Other"
        )
        .await
        .contains("key_custody_backend"));
        assert!(!get_page(
            &router,
            &session_token,
            &format!("/stores/{connection_id}/settings?tab=wallet")
        )
        .await
        .contains("Key storage"));
    }

    #[tokio::test]
    async fn updating_the_confirmation_threshold_redirects_and_the_new_value_shows_on_the_store_page(
    ) {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "confirmations-update@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[("confirmations_required", "3")],
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::SEE_OTHER,
            "expected a redirect back to the settings page"
        );
        assert_eq!(
            response.headers().get("location").unwrap(),
            &format!("/stores/{connection_id}/settings?saved=confirmation-thresholds"),
        );

        let settings_page = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/settings?tab=payments"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(settings_page).await;
        assert!(
            html.contains(r#"value="3""#),
            "expected the real, updated confirmation threshold shown, got: {html}"
        );
    }

    #[tokio::test]
    async fn updating_the_confirmation_threshold_to_zero_is_accepted() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "confirmations-zero@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[("confirmations_required", "0")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let settings = router
            .oneshot(
                Request::builder()
                    .uri(format!("/stores/{connection_id}/settings?tab=payments"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(settings.status(), StatusCode::OK);
        let html = body_text(settings).await;
        assert!(
            html.contains(r#"name="zero_conf_enabled" value="true" id="zero-conf" checked"#),
            "expected native 0-conf to be enabled: {html}"
        );
    }

    #[tokio::test]
    async fn a_new_store_defaults_to_coingecko_and_offers_no_options_when_none_are_enabled() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "fx-provider-default@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let settings_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/settings?tab=payments"))
                    .header("cookie", format!("session={session_token}"))
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
        assert!(
            settings_html.contains("Exchange rate provider"),
            "expected the settings section present, got: {settings_html}"
        );
        // The dropdown itself must offer zero real `<option>`s - nothing is
        // enabled on this instance, and there is no "fixed" to fall back to
        // any more.
        assert!(!settings_html.contains(r#"<option value="coingecko""#), "expected no coingecko <option> since this instance never enabled it, got: {settings_html}");
        assert!(!settings_html.contains(r#"<option value="fixed""#), "the removed \"fixed\" provider must never appear as a real option, got: {settings_html}");

        // The point: with no fiat provider available
        // at all, the "create an order" currency field must be a plain
        // readonly "XMR" field, not a one-option `<select>` (a dropdown with
        // nothing to actually choose between is misleading busywork).
        let create_order_response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/orders/new"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let create_order_html = body_text(create_order_response).await;
        assert!(
            create_order_html.contains(
                r#"<input class="mk-input" type="text" name="currency" id="currency" value="XMR" readonly>"#
            ),
            "expected a readonly XMR currency field, got: {create_order_html}"
        );
        assert!(
            !create_order_html.contains(r#"<select id="currency""#),
            "expected no currency dropdown when only XMR is available, got: {create_order_html}"
        );
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
        let mock_coingecko = axum::Router::new().route(
            "/api/v3/simple/supported_vs_currencies",
            axum::routing::get(supported_currencies),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mock_coingecko_addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, mock_coingecko).await.unwrap();
        });

        let (mut state, _engine) = test_state_with_real_engine().await;
        state.exchange_rate = std::sync::Arc::new(
            crate::exchange_rate_config::ExchangeRateProviders::coingecko_only(format!(
                "http://{mock_coingecko_addr}"
            )),
        );
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "fx-provider-dropdown@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/orders/new"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(response).await;
        assert!(
            html.contains(r#"<select class="mk-input" name="currency" id="currency" data-mk="2">"#),
            "expected a real currency dropdown, got: {html}"
        );
        assert!(
            !html.contains(r#"id="currency" name="currency" value="XMR" readonly"#),
            "must not be readonly once a provider is enabled, got: {html}"
        );
        assert!(
            html.contains(r#"<option value="XMR" selected>XMR</option>"#),
            "expected XMR always offered, got: {html}"
        );
        assert!(
            html.contains(r#"<option value="USD">USD</option>"#),
            "expected the live-discovered USD option, got: {html}"
        );
        assert!(
            html.contains(r#"<option value="EUR">EUR</option>"#),
            "expected the live-discovered EUR option, got: {html}"
        );
    }

    #[tokio::test]
    async fn choosing_an_fx_provider_the_instance_has_not_enabled_is_a_clear_error_not_silently_accepted(
    ) {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "fx-provider-invalid@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[("use_coingecko", "on"), ("position_coingecko", "1")],
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "a rejected provider re-renders the page, it doesn't redirect"
        );
        let html = body_text(response).await;
        assert!(
            html.contains("not an available exchange rate provider"),
            "expected a clear rejection message, got: {html}"
        );
    }

    fn fixi(mut request: Request<Body>) -> Request<Body> {
        request
            .headers_mut()
            .insert("FX-Request", "true".parse().unwrap());
        request
    }

    /// The settings form saves the cards whose values changed, says which
    /// in `saved=`, and saves nothing when a card's values are refused:
    /// that card says why and the page keeps what was sent.
    #[tokio::test]
    async fn the_settings_form_saves_the_changed_cards_or_nothing() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "settings-form@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;
        let settings = format!("/stores/{connection_id}/settings");
        let send = |fields: &[(&str, &str)]| {
            router
                .clone()
                .oneshot(form_post_request(&settings, &session_token, fields))
        };
        let page = |uri: String| {
            router.clone().oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
        };
        let everything = |base: &'static str, confirmations: &'static str| {
            vec![
                ("base_currency", base),
                ("confirmations_required", confirmations),
                ("switches", "zero_conf_enabled"),
                ("switches", "client_logging"),
                ("client_logging", "true"),
            ]
        };

        let response = send(&everything("EUR", "3")).await.unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers().get("location").unwrap(),
            &format!("{settings}?saved=base-currency,confirmation-thresholds,diagnostics")
        );
        let html = body_text(
            page(format!("{settings}?saved=base-currency,diagnostics"))
                .await
                .unwrap(),
        )
        .await;
        assert!(html.contains("<strong>Settings saved</strong>"), "{html}");
        assert_eq!(
            html.matches(r#"data-card-saved data-mk="4">Saved<"#)
                .count(),
            2,
            "{html}"
        );

        // The same values again: nothing to save.
        let response = send(&everything("EUR", "3")).await.unwrap();
        assert_eq!(
            response.headers().get("location").unwrap(),
            &format!("{settings}?saved=nothing")
        );
        // Sent from a tab's form, it comes back to that tab.
        let mut fields = everything("EUR", "3");
        fields.push(("tab", "payments"));
        let response = send(&fields).await.unwrap();
        assert_eq!(
            response.headers().get("location").unwrap(),
            &format!("{settings}?saved=nothing&tab=payments")
        );

        // A refused card saves nothing, the base currency included.
        let response = send(&everything("USD", "abc")).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let html = body_text(response).await;
        assert!(
            html.contains(
                r#"<section id="card-confirmation-thresholds" class="settings-card is-failed""#
            ),
            "{html}"
        );
        assert!(
            html.contains("Enter a whole number of confirmations."),
            "{html}"
        );
        assert!(
            html.contains(
                r#"name="confirmations_required" id="confirmations-required" value="abc""#
            ),
            "what was sent: {html}"
        );
        let html = body_text(page(format!("{settings}?tab=general")).await.unwrap()).await;
        assert!(
            html.contains(r#"<option value="EUR" selected data-label="Euro""#),
            "the base currency wasn't saved either: {html}"
        );
        let html = body_text(page(format!("{settings}?tab=payments")).await.unwrap()).await;
        assert!(html.contains("New tier's amount (EUR)"), "{html}");

        // A new base currency deletes the custom thresholds: they aren't
        // changed in the same save.
        let mut fields = everything("USD", "3");
        fields.extend([
            ("new_unit_amount", "50"),
            ("new_confirmations_required", "4"),
        ]);
        let response = send(&fields).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_text(response)
            .await
            .contains("save it on its own first"));
    }

    #[tokio::test]
    async fn updating_the_base_currency_persists_and_shows_on_the_store_page() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "base-currency-update@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[("base_currency", "EUR")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let page = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/settings"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(page).await;
        assert!(
            html.contains(r#"<option value="EUR" selected data-label="Euro""#),
            "expected EUR marked selected, got: {html}"
        );
    }

    /// The default form owns the zero-conf checkbox; custom tiers save separately.
    #[tokio::test]
    async fn ticking_the_zero_conf_checkbox_persists_and_shows_as_checked_on_the_settings_page() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "zero-conf-update@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[
                    ("confirmations_required", "10"),
                    ("zero_conf_enabled", "true"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let page = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/settings?tab=payments"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(page).await;
        assert!(
            html.contains(r#"name="zero_conf_enabled" value="true" id="zero-conf" checked"#),
            "expected the 0-conf checkbox to show as checked once enabled, got: {html}"
        );

        // The browser still submits the visible numeric value (0) when the
        // merchant unchecks the box. That action must restore confirmations.
        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[
                    ("confirmations_required", "0"),
                    ("switches", "zero_conf_enabled"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let page = router
            .oneshot(
                Request::builder()
                    .uri(format!("/stores/{connection_id}/settings?tab=payments"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(page).await;
        assert!(
            html.contains(
                r#"name="confirmations_required" id="confirmations-required" value="10""#
            ),
            "expected the ordinary default after unchecking: {html}"
        );
        assert!(
            html.contains(
                r#"name="zero_conf_enabled" value="true" id="zero-conf" aria-describedby"#
            ),
            "expected 0-conf to be disabled: {html}"
        );
    }

    #[tokio::test]
    async fn custom_confirmation_thresholds_save_without_contacting_the_engine() {
        let (mut state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let token = signed_up_and_logged_in_session_token(
            &router,
            "local-policy-save@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _) = create_connection(&router, &state, &token).await;
        state
            .db
            .lock()
            .replace_confirmation_thresholds(
                crate::confirmation_thresholds::PolicyProof::for_test(),
                &shared::ids::ConnectionId::new(connection_id.to_string()),
                &[],
                Some(("old", "50", 20, crate::now_unix())),
            )
            .unwrap();
        // A custom-tier save must not depend on engine availability.
        state.engine.client = EngineClient::for_tests("http://127.0.0.1:0");
        let response = build_router(state.clone())
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &token,
                &[
                    ("delete_old", "on"),
                    ("new_unit_amount", "100"),
                    ("new_confirmations_required", "30"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let thresholds = state
            .db
            .lock()
            .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string(),
            ))
            .unwrap();
        assert_eq!(thresholds.len(), 1);
        assert_eq!(thresholds[0].unit_amount, "100");
        assert_eq!(thresholds[0].confirmations_required, 30);
    }

    #[tokio::test]
    async fn default_confirmation_save_leaves_custom_thresholds_unchanged() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let token = signed_up_and_logged_in_session_token(
            &router,
            "default-policy-save@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _) = create_connection(&router, &state, &token).await;
        state
            .db
            .lock()
            .replace_confirmation_thresholds(
                crate::confirmation_thresholds::PolicyProof::for_test(),
                &shared::ids::ConnectionId::new(connection_id.to_string()),
                &[],
                Some(("keep", "50", 20, crate::now_unix())),
            )
            .unwrap();
        let response = router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &token,
                &[
                    ("confirmations_required", "10"),
                    ("zero_conf_enabled", "true"),
                    ("switches", "zero_conf_enabled"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let row = state
            .db
            .lock()
            .get_store_connection_by_id(&shared::ids::ConnectionId::new(connection_id.to_string()))
            .unwrap()
            .unwrap();
        let sk = crate::crypto::decrypt(
            &state.encryption_key,
            crate::crypto::Binding::StoreSecret(row.id.as_str()),
            &row.tenant_secret_token_encrypted,
        )
        .unwrap();
        assert_eq!(
            state
                .engine
                .client
                .get_tenant(&shared::auth::RawToken::presented(&sk))
                .await
                .unwrap()
                .confirmations_required,
            0
        );
        let thresholds = state
            .db
            .lock()
            .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string(),
            ))
            .unwrap();
        assert_eq!(thresholds.len(), 1);
        assert_eq!(thresholds[0].id, "keep");
        assert_eq!(thresholds[0].confirmations_required, 20);
    }

    #[tokio::test]
    async fn custom_confirmation_save_fails_closed_when_thresholds_cannot_be_read() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let token = signed_up_and_logged_in_session_token(
            &router,
            "broken-policy-save@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _) = create_connection(&router, &state, &token).await;
        state.db.lock().break_confirmation_thresholds_for_test();
        let response = router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &token,
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let row = state
            .db
            .lock()
            .get_store_connection_by_id(&shared::ids::ConnectionId::new(connection_id.to_string()))
            .unwrap()
            .unwrap();
        let sk = crate::crypto::decrypt(
            &state.encryption_key,
            crate::crypto::Binding::StoreSecret(row.id.as_str()),
            &row.tenant_secret_token_encrypted,
        )
        .unwrap();
        assert_eq!(
            state
                .engine
                .client
                .get_tenant(&shared::auth::RawToken::presented(&sk))
                .await
                .unwrap()
                .confirmations_required,
            10
        );
    }

    #[tokio::test]
    async fn invalid_new_threshold_does_not_publish_zero_conf_or_delete_existing_rows() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "invalid-threshold-save@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _) = create_connection(&router, &state, &session_token).await;
        let existing_id = "existing-threshold";
        state
            .db
            .lock()
            .replace_confirmation_thresholds(
                crate::confirmation_thresholds::PolicyProof::for_test(),
                &shared::ids::ConnectionId::new(connection_id.to_string()),
                &[],
                Some((existing_id, "50", 20, crate::now_unix())),
            )
            .unwrap();
        let delete_field = format!("delete_{existing_id}");
        let response = router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[
                    ("confirmations_required", "10"),
                    ("zero_conf_enabled", "true"),
                    (delete_field.as_str(), "on"),
                    ("new_unit_amount", "100"),
                    ("new_confirmations_required", "oops"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let row = state
            .db
            .lock()
            .get_store_connection_by_id(&shared::ids::ConnectionId::new(connection_id.to_string()))
            .unwrap()
            .unwrap();
        let sk = crate::crypto::decrypt(
            &state.encryption_key,
            crate::crypto::Binding::StoreSecret(row.id.as_str()),
            &row.tenant_secret_token_encrypted,
        )
        .unwrap();
        assert_eq!(
            state
                .engine
                .client
                .get_tenant(&shared::auth::RawToken::presented(&sk))
                .await
                .unwrap()
                .confirmations_required,
            10
        );
        let thresholds = state
            .db
            .lock()
            .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string(),
            ))
            .unwrap();
        assert_eq!(thresholds.len(), 1);
        assert_eq!(thresholds[0].id, existing_id);
    }

    #[tokio::test]
    async fn an_unknown_base_currency_is_rejected_and_the_existing_value_is_unchanged() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "base-currency-bad@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[("base_currency", "NOTREAL")],
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "a rejected currency re-renders the page, it doesn't redirect"
        );
        let html = body_text(response).await;
        assert!(
            html.contains("not a known currency"),
            "expected a clear rejection message, got: {html}"
        );
        assert!(
            html.contains(
                r#"name="base_currency" id="base-currency" data-mk="2" data-saved="XMR""#
            ),
            "the store's base currency must still be the original default, got: {html}"
        );
    }

    #[tokio::test]
    async fn a_known_currency_with_no_enabled_rate_provider_is_still_accepted_as_a_base_currency_change(
    ) {
        // A base currency doesn't need a rate provider, proved here for the
        // *change* path rather than creation -
        // `test_state_with_real_engine`'s own `ExchangeRateProviders::xmr_only()`
        // has no provider enabled at all.
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "base-currency-decoupled@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[("base_currency", "GBP")],
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::SEE_OTHER,
            "a known currency must be selectable regardless of provider support"
        );
    }

    #[tokio::test]
    async fn changing_the_base_currency_deletes_every_custom_threshold() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "base-currency-cascade@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[
                    ("new_unit_amount", "50.00"),
                    ("new_confirmations_required", "20"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(
            state
                .db
                .lock()
                .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                    connection_id.to_string()
                ))
                .map(|rows| rows.len())
                .unwrap(),
            1
        );

        router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[("base_currency", "EUR")],
            ))
            .await
            .unwrap();
        assert_eq!(
            state
                .db
                .lock()
                .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                    connection_id.to_string()
                ))
                .map(|rows| rows.len())
                .unwrap(),
            0,
            "every custom threshold must be gone after a base currency change"
        );
    }

    #[tokio::test]
    async fn adding_and_deleting_a_custom_confirmation_threshold_round_trips_through_the_real_page()
    {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "threshold-add-delete@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[
                    ("new_unit_amount", "50.00"),
                    ("new_confirmations_required", "20"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let threshold_id = state
            .db
            .lock()
            .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string(),
            ))
            .unwrap()[0]
            .id
            .clone();

        let page = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/settings?tab=payments"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(page).await;
        assert!(
            html.contains("<strong>From 50 "),
            "expected the canonical threshold amount shown, got: {html}"
        );
        assert!(
            html.contains(&format!(
                r#"<input type="checkbox" name="delete_{threshold_id}" value="remove" data-rows-remove="">"#
            )),
            "expected a real remove checkbox on the new tier, got: {html}"
        );

        // The Payments form's single Save button posts every tier's state
        // (default + remove checkboxes + a possible new tier) to one route,
        // not a per-row delete form - see `save_confirmation_thresholds`'s
        // own doc comment.
        let delete_field = format!("delete_{threshold_id}");
        let delete_response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[
                    ("confirmations_required", "10"),
                    (delete_field.as_str(), "remove"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(delete_response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            state
                .db
                .lock()
                .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                    connection_id.to_string()
                ))
                .map(|rows| rows.len())
                .unwrap(),
            0,
            "expected the threshold to be gone"
        );

        let after = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/settings?tab=payments"))
                    .header("cookie", format!("session={session_token}"))
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
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "threshold-decimal-alias@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _) = create_connection(&router, &state, &session_token).await;
        let direct_path = format!("/stores/{connection_id}/settings");
        let response = router
            .clone()
            .oneshot(form_post_request(
                &direct_path,
                &session_token,
                &[
                    ("new_unit_amount", "50"),
                    ("new_confirmations_required", "20"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let response = router
            .clone()
            .oneshot(form_post_request(
                &direct_path,
                &session_token,
                &[
                    ("new_unit_amount", "50.0"),
                    ("new_confirmations_required", "0"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_text(response).await.contains("already exists"));
        let response = router
            .clone()
            .oneshot(form_post_request(
                &direct_path,
                &session_token,
                &[
                    ("new_unit_amount", "5e1"),
                    ("new_confirmations_required", "0"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let response = router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[
                    ("new_unit_amount", "50.00"),
                    ("new_confirmations_required", "0"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_text(response).await.contains("already exists"));
        let rows = state
            .db
            .lock()
            .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string(),
            ))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].unit_amount, "50");
        assert_eq!(rows[0].confirmations_required, 20);
    }

    #[tokio::test]
    async fn custom_thresholds_are_displayed_in_ascending_order_of_amount() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "threshold-ordering@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        for amount in ["100.00", "10.00", "50.00"] {
            router
                .clone()
                .oneshot(form_post_request(
                    &format!("/stores/{connection_id}/settings"),
                    &session_token,
                    &[
                        ("new_unit_amount", amount),
                        ("new_confirmations_required", "15"),
                    ],
                ))
                .await
                .unwrap();
        }

        let page = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/settings?tab=payments"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(page).await;
        let pos_10 = html.find("<strong>From 10 ").expect("canonical 10 shown");
        let pos_50 = html.find("<strong>From 50 ").expect("canonical 50 shown");
        let pos_100 = html.find("<strong>From 100 ").expect("canonical 100 shown");
        assert!(
            pos_10 < pos_50 && pos_50 < pos_100,
            "expected ascending amount order, got: {html}"
        );
    }

    #[tokio::test]
    async fn a_sixth_custom_threshold_is_rejected_until_one_is_deleted_in_the_same_save() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "threshold-max-five@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;
        // The settings page's one form: its "new threshold" fields, and a
        // delete checkbox per existing row.
        let save = format!("/stores/{connection_id}/settings");
        for i in 1..=5 {
            let response = router
                .clone()
                .oneshot(form_post_request(
                    &save,
                    &session_token,
                    &[
                        ("new_unit_amount", &format!("{i}0.00")),
                        ("new_confirmations_required", "15"),
                    ],
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::SEE_OTHER,
                "expected threshold {i} to be accepted"
            );
        }

        let response = router
            .clone()
            .oneshot(form_post_request(
                &save,
                &session_token,
                &[
                    ("new_unit_amount", "999.00"),
                    ("new_confirmations_required", "15"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "a rejected sixth threshold re-renders the page, it doesn't redirect"
        );
        let html = body_text(response).await;
        assert!(
            html.contains("at most 5 custom thresholds"),
            "expected a clear rejection message, got: {html}"
        );

        // Ticking one row's delete box while adding the new one fits.
        let doomed = state
            .db
            .lock()
            .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string(),
            ))
            .unwrap()[0]
            .clone();
        let response = router
            .oneshot(form_post_request(
                &save,
                &session_token,
                &[
                    (&format!("delete_{}", doomed.id), "on"),
                    ("new_unit_amount", "999.00"),
                    ("new_confirmations_required", "15"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let amounts: Vec<String> = state
            .db
            .lock()
            .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string(),
            ))
            .unwrap()
            .into_iter()
            .map(|t| t.unit_amount)
            .collect();
        assert_eq!(amounts.len(), 5);
        assert!(
            amounts.contains(&"999".to_string()) && !amounts.contains(&doomed.unit_amount),
            "{amounts:?}"
        );
    }

    #[tokio::test]
    async fn a_duplicate_unit_amount_is_rejected() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "threshold-duplicate@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[
                    ("new_unit_amount", "50.00"),
                    ("new_confirmations_required", "20"),
                ],
            ))
            .await
            .unwrap();

        let response = router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[
                    ("new_unit_amount", "50.00"),
                    ("new_confirmations_required", "5"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "a duplicate amount re-renders the page, it doesn't redirect"
        );
        let html = body_text(response).await;
        assert!(
            html.contains("already exists"),
            "expected a clear rejection message, got: {html}"
        );
    }

    #[tokio::test]
    async fn a_negative_or_out_of_range_new_threshold_is_rejected() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "threshold-negative@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;
        let save = format!("/stores/{connection_id}/settings");

        for (amount, confirmations, message) in [
            ("-5.00", "20", "non-negative"),
            ("50.00", "721", "from 0 to 720"),
            ("50.00", "", "from 0 to 720"),
        ] {
            let response = router
                .clone()
                .oneshot(form_post_request(
                    &save,
                    &session_token,
                    &[
                        ("new_unit_amount", amount),
                        ("new_confirmations_required", confirmations),
                    ],
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{amount}/{confirmations} re-renders the page, it doesn't redirect"
            );
            let html = body_text(response).await;
            assert!(
                html.contains(message),
                "{amount}/{confirmations}: expected {message:?}, got: {html}"
            );
        }
        assert_eq!(
            state
                .db
                .lock()
                .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                    connection_id.to_string()
                ))
                .map(|rows| rows.len())
                .unwrap(),
            0
        );
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
    async fn an_order_priced_above_a_custom_threshold_gets_that_thresholds_confirmations_required_on_the_real_engine(
    ) {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "threshold-resolution-above@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) = create_connection(&router, &state, &session_token).await;

        router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[
                    ("new_unit_amount", "5.00"),
                    ("new_confirmations_required", "20"),
                ],
            ))
            .await
            .unwrap();

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/orders/new"),
                &session_token,
                &[("amount", "10.00"), ("currency", TEST_CURRENCY)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let location = response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let order_id = location.rsplit('/').next().unwrap().to_string();

        {
            let store = engine.store().lock();
            let tenant_id = store
                .list_active_tenants()
                .unwrap()
                .into_iter()
                .find(|t| t.public_key == public_key)
                .unwrap()
                .id;
            let stored = store
                .get_order(&tenant_id, &shared::ids::OrderId::new(order_id.to_string()))
                .unwrap()
                .unwrap();
            assert_eq!(
                stored.confirmations_required_override,
                Some(20),
                "a 10.00 XMR order against a 5.00-and-up threshold of 20 confirmations must use that threshold, not the default"
            );
        }

        let metadata = state
            .db
            .lock()
            .get_order_currency_metadata(
                &shared::ids::ConnectionId::new(connection_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(metadata.store_base_currency, Some("XMR".to_string()));
        assert_eq!(
            metadata.base_currency_piconero_per_unit, None,
            "the order's own currency already was the base currency, so no second conversion rate exists to snapshot"
        );
        assert_eq!(metadata.confirmations_required_applied, Some(20));
        assert!(
            metadata.created_with_key,
            "a dashboard order is the merchant's own, as trusted as the key"
        );

        // The whole point of the snapshot (making it clear how the
        // confirmation threshold was decided) - the order's own detail
        // page must actually show it, not just record it in the database.
        let detail_response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/orders/{order_id}"))
                    .header("cookie", format!("session={session_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail_response.status(), StatusCode::OK);
        let html = body_text(detail_response).await;
        assert!(
            html.contains("Confirmations required"),
            "expected the confirmations-required row's label, got: {html}"
        );
        assert!(
            html.contains(">20<"),
            "expected the resolved threshold's own confirmations_required (20) shown, got: {html}"
        );
        assert!(
            html.contains("Store base currency"),
            "expected the base-currency snapshot row's label, got: {html}"
        );
        assert!(
            html.contains("same as order currency"),
            "expected the no-second-rate case shown plainly, got: {html}"
        );
    }

    /// The order-below-every-threshold half of the same chain: a threshold
    /// exists, but the order's own amount never reaches it, so the store's
    /// plain default (fallback) confirmations_required - the real tenant
    /// value on the engine, 10 by the engine's own `create_tenant` default
    /// (`engine::store::Db::create_tenant`) - is what's actually applied.
    #[tokio::test]
    async fn an_order_priced_below_every_custom_threshold_uses_the_stores_default_confirmations_required(
    ) {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "threshold-resolution-below@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, public_key) = create_connection(&router, &state, &session_token).await;

        router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[
                    ("new_unit_amount", "50.00"),
                    ("new_confirmations_required", "99"),
                ],
            ))
            .await
            .unwrap();

        let response = router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/orders/new"),
                &session_token,
                &[("amount", "10.00"), ("currency", TEST_CURRENCY)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let location = response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let order_id = location.rsplit('/').next().unwrap().to_string();

        let store = engine.store().lock();
        let tenant_id = store
            .list_active_tenants()
            .unwrap()
            .into_iter()
            .find(|t| t.public_key == public_key)
            .unwrap()
            .id;
        let stored = store
            .get_order(&tenant_id, &shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.confirmations_required_override,
            Some(10),
            "a 10.00 XMR order below the only threshold's 50.00 must fall back to the store's own default"
        );
        drop(store);

        let metadata = state
            .db
            .lock()
            .get_order_currency_metadata(
                &shared::ids::ConnectionId::new(connection_id.to_string()),
                &shared::ids::OrderId::new(order_id.to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(metadata.confirmations_required_applied, Some(10));
    }

    /// The default is the line above the tiers, never a tier of its own:
    /// it has no remove checkbox, before or after a tier is added, and
    /// says which orders it covers.
    #[tokio::test]
    async fn the_default_threshold_has_no_remove_control_and_is_not_a_tier() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "threshold-default-not-deletable@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;
        let page = || async {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(format!("/stores/{connection_id}/settings?tab=payments"))
                        .header("cookie", format!("session={session_token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            body_text(response).await
        };

        let html = page().await;
        assert!(
            html.contains("Default confirmations"),
            "expected the default threshold's own field, got: {html}"
        );
        assert!(
            html.contains(
                r#"<p class="tier-default"><strong>Every order</strong>: the default, 10 confirmations "#
            ),
            "with no tiers the default covers every order, got: {html}"
        );
        assert!(
            !html.contains("name=\"delete_"),
            "a fresh store has no custom thresholds, so no remove checkbox should exist yet, got: {html}"
        );

        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[
                    ("new_unit_amount", "50"),
                    ("new_confirmations_required", "20"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let html = page().await;
        assert!(
            html.contains(r#"<p class="tier-default"><strong>Under 50 "#),
            "the default covers the orders below the lowest tier, got: {html}"
        );
        assert_eq!(
            html.matches("name=\"delete_").count(),
            1,
            "only the tier is removable, not the default: {html}"
        );
    }

    #[tokio::test]
    async fn updating_the_confirmation_threshold_with_non_numeric_input_shows_a_clear_error() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "confirmations-non-numeric@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let response = router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &session_token,
                &[("confirmations_required", "not-a-number")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let html = body_text(response).await;
        assert!(
            html.contains("Enter a whole number of confirmations."),
            "expected a clear validation error, got: {html}"
        );
    }

    #[tokio::test]
    async fn a_different_user_cannot_update_confirmations_on_someone_elses_connection() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let owner_token = signed_up_and_logged_in_session_token(
            &router,
            "confirmations-owner@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &owner_token).await;

        let intruder_token = signed_up_and_logged_in_session_token(
            &router,
            "confirmations-intruder@example.com",
            "correct horse battery staple",
        )
        .await;

        let response = router
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/settings"),
                &intruder_token,
                &[("confirmations_required", "3")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// The store's Orders page is the full history: every order with its
    /// reference and where it came from (POS, dashboard, the store's
    /// connected plugin with the store key and its slug in the
    /// `Monokulo-Client` header, any other caller with the key, a browser
    /// on the website), a POS
    /// cancellation shown as cancelled, a search by reference or order id,
    /// and pages of 50 newest first.
    #[tokio::test]
    async fn the_orders_page_lists_sources_and_cancellations_searches_and_pages() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "orders-page@example.com",
            "correct horse battery staple",
        )
        .await;
        let (id, pk) = create_connection(&router, &state, &session_token).await;
        let cookie = format!("session={session_token}");
        // The POS posts with the merchant's session cookie.
        let pos_post = |uri: String, body: serde_json::Value| {
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .header("cookie", cookie.clone())
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        // The API takes the store's secret key, or its public key from a
        // website.
        let api_post = |key: &str, body: serde_json::Value| {
            crate::http::test_support::api_request("POST", "/api/v1/orders", Some(key), Some(body))
        };
        let created_id = |response: axum::response::Response| async move {
            body_json(response).await["order_id"]
                .as_str()
                .unwrap()
                .to_string()
        };

        let pos = created_id(
            router
                .clone()
                .oneshot(pos_post(
                    format!("/stores/{id}/pos/orders"),
                    serde_json::json!({ "amount": "0.3", "merchant_order_id": "Table 4" }),
                ))
                .await
                .unwrap(),
        )
        .await;
        let cancelled = created_id(
            router
                .clone()
                .oneshot(pos_post(
                    format!("/stores/{id}/pos/orders"),
                    serde_json::json!({ "amount": "0.2", "merchant_order_id": "Table 9" }),
                ))
                .await
                .unwrap(),
        )
        .await;
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/stores/{id}/pos/orders/{cancelled}/cancel"))
                    .header("cookie", cookie.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let dashboard = router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{id}/orders/new"),
                &session_token,
                &[
                    ("amount", "1.00"),
                    ("currency", "XMR"),
                    ("merchant_order_id", "invoice-7"),
                ],
            ))
            .await
            .unwrap();
        let dashboard = dashboard.headers()["location"]
            .to_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_string();
        let row = state
            .db
            .lock()
            .get_store_connection_by_id(&shared::ids::ConnectionId::new(id.to_string()))
            .unwrap()
            .unwrap();
        let secret = crate::crypto::decrypt(
            &TEST_ENCRYPTION_KEY,
            crate::crypto::Binding::StoreSecret(row.id.as_str()),
            &row.tenant_secret_token_encrypted,
        )
        .unwrap();
        // The store's connected plugin, which named itself.
        state
            .db
            .lock()
            .connect_integration(&crate::db::NewStoreIntegration {
                store_id: &row.id,
                plugin: &crate::plugins::Plugin::parse("acme-cart", "Acme Cart").unwrap(),
                site: row.site.as_ref(),
                version: "0.3.0",
                webhook_id: None,
                webhook_url: None,
                at: 1_000,
            })
            .unwrap();
        let keyed = |reference: &str, client: &str| {
            let mut request = api_post(
                &secret,
                serde_json::json!({ "amount": "2.00", "currency": "XMR", "merchant_order_id": reference }),
            );
            request.headers_mut().insert(
                crate::api::runtime::client::CLIENT_HEADER,
                client.parse().unwrap(),
            );
            request
        };
        // The plugin's slug in the header, in any case, is the plugin's.
        let plugin = created_id(
            router
                .clone()
                .oneshot(keyed("ac-1042", "Acme-Cart/0.4.0"))
                .await
                .unwrap(),
        )
        .await;
        // A client the store hasn't connected is the Store API.
        let other = created_id(
            router
                .clone()
                .oneshot(keyed("oc-7", "opencart/1.0.0"))
                .await
                .unwrap(),
        )
        .await;
        let api = created_id(
            router
                .clone()
                .oneshot(api_post(
                    &secret,
                    serde_json::json!({ "amount": "2.50", "currency": "XMR", "merchant_order_id": "srv-9" }),
                ))
                .await
                .unwrap(),
        )
        .await;
        // The header alone, from a browser, names nothing.
        let mut claimed = api_post(
            &pk,
            serde_json::json!({ "amount": "2.75", "currency": "XMR", "merchant_order_id": "web-3" }),
        );
        claimed.headers_mut().insert(
            crate::api::runtime::client::CLIENT_HEADER,
            "acme-cart/0.4.0".parse().unwrap(),
        );
        let claimed = created_id(router.clone().oneshot(claimed).await.unwrap()).await;
        let website = created_id(
            router
                .clone()
                .oneshot(api_post(
                    &pk,
                    serde_json::json!({ "amount": "3.00", "currency": "XMR" }),
                ))
                .await
                .unwrap(),
        )
        .await;

        let page = |query: &str| {
            let (router, cookie, uri) = (
                router.clone(),
                cookie.clone(),
                format!("/stores/{id}/orders{query}"),
            );
            async move {
                body_text(
                    router
                        .oneshot(
                            Request::builder()
                                .uri(uri)
                                .header("cookie", cookie)
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap(),
                )
                .await
            }
        };
        let row_of = |html: &str, order_id: &str| -> String {
            let start = html
                .find(&format!("/orders/{order_id}\""))
                .unwrap_or_else(|| panic!("{order_id} not listed"));
            html[start..].split("</tr>").next().unwrap().to_string()
        };
        let html = page("").await;
        for (order_id, source, reference, status) in [
            (&pos, "POS", "Table 4", "Waiting for payment"),
            (&cancelled, "POS", "Table 9", "Cancelled"),
            (&dashboard, "Dashboard", "invoice-7", "Waiting for payment"),
            (&plugin, "Acme Cart", "ac-1042", "Waiting for payment"),
            (&other, "Store API", "oc-7", "Waiting for payment"),
            (&api, "Store API", "srv-9", "Waiting for payment"),
            (&claimed, "Website", "web-3", "Waiting for payment"),
            (&website, "Website", "—", "Waiting for payment"),
        ] {
            let row = row_of(&html, order_id);
            for expected in [source, reference, status] {
                assert!(
                    row.contains(&format!(">{expected}<")),
                    "{order_id}: expected {expected} in {row}"
                );
            }
            // The status is the same badge the checkout and the POS show.
            let class = if status == "Cancelled" {
                "state-cancelled"
            } else {
                "state-pending"
            };
            assert!(
                row.contains(&format!(r#"<span class="tag {class}">"#)),
                "{order_id}: expected a {class} badge in {row}"
            );
        }

        let html = page("?q=table").await;
        assert!(
            html.contains(&pos) && html.contains(&cancelled) && !html.contains(&plugin),
            "search by reference"
        );
        assert!(
            html.contains(r#"value="table""#),
            "the search stays in the box"
        );
        let html = page(&format!("?q={}", &plugin[6..16])).await;
        assert!(
            html.contains(&plugin) && !html.contains(&pos),
            "search by order id"
        );
        let html = page("?q=nothing-like-this").await;
        assert!(html.contains("No orders match “nothing-like-this”."));

        // 8 so far; 43 more make 51: the first page has 50 and an Older link.
        for i in 0..43 {
            router
                .clone()
                .oneshot(api_post(
                    &pk,
                    serde_json::json!({ "amount": format!("0.{i:02}1"), "currency": "XMR" }),
                ))
                .await
                .unwrap();
        }
        let first = page("").await;
        assert_eq!(
            first.matches(r#"<tr><td class="card-title"><a"#).count(),
            50
        );
        assert!(
            first.contains(&format!(r#"href="/stores/{id}/orders?page=1" rel="next""#))
                && !first.contains(r#"rel="prev""#)
        );
        let second = page("?page=1").await;
        assert_eq!(
            second.matches(r#"<tr><td class="card-title"><a"#).count(),
            1
        );
        assert!(second.contains(r#"rel="prev""#) && !second.contains(r#"rel="next""#));
    }

    #[tokio::test]
    async fn lookup_payment_reshows_the_orders_page_with_a_not_found_message() {
        let (state, _engine) = test_state_with_real_engine_and_admin_lookup_daemon().await;
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "lookup-owner@example.com",
            "correct horse battery staple",
        )
        .await;
        let (connection_id, _public_key) = create_connection(&router, &state, &session_token).await;

        let txid = "a".repeat(64);
        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/orders/lookup"),
                &session_token,
                &[("txid", &txid)],
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a lookup re-renders the orders page, it doesn't redirect"
        );
        let html = body_text(response).await;
        assert!(
            html.contains("No transaction with that ID was found"),
            "expected the not-found message, got: {html}"
        );
        assert!(
            html.contains(&txid),
            "the submitted txid must repopulate the form's own input"
        );
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
        let router = build_router(state.clone());
        let session_token = signed_up_and_logged_in_session_token(
            &router,
            "lookup-recover@example.com",
            "correct horse battery staple",
        )
        .await;
        let view_key = monero::PrivateKey::from_slice(
            &hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07")
                .unwrap(),
        )
        .unwrap();
        let spend_key = monero::PrivateKey::from_slice(
            &hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907")
                .unwrap(),
        )
        .unwrap();
        let cookie = format!("session={session_token}");
        let connect = |site: &'static str, view: String, spend: String| {
            let cookie = cookie.clone();
            let router = router.clone();
            async move {
                let response = crate::http::test_support::set_up_store_with_keys(
                    &router,
                    &cookie,
                    &[
                        ("store_name", site),
                        ("store_site", site),
                        ("base_currency", "XMR"),
                        ("view_key_hex", &view),
                        ("spend_pubkey_hex", &spend),
                        ("network", "mainnet"),
                        ("name", site),
                    ],
                )
                .await;
                crate::http::test_support::store_made(&response)
            }
        };
        let connection_id = connect(
            "https://recovered.example.com",
            hex::encode(view_key.to_bytes()),
            hex::encode(monero::PublicKey::from_private_key(&spend_key).to_bytes()),
        )
        .await;
        let response = router
            .clone()
            .oneshot(form_post_request(
                &format!("/stores/{connection_id}/orders/new"),
                &session_token,
                &[("amount", "0.000000000001"), ("currency", "XMR")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let order_page = response.headers()["location"].to_str().unwrap().to_string();

        let tx: monero::Transaction = monero::consensus::deserialize(
            &hex::decode(include_str!("../../../engine/tests/fixtures/subaddress_tx.hex").trim())
                .unwrap(),
        )
        .unwrap();
        let txid = hex::encode(tx.hash().to_bytes());
        engine.add_mempool_transaction(tx);
        let lookup = format!("/stores/{connection_id}/orders/lookup");
        let html = body_text(
            router
                .clone()
                .oneshot(form_post_request(
                    &lookup,
                    &session_token,
                    &[("txid", &txid)],
                ))
                .await
                .unwrap(),
        )
        .await;
        assert!(html.contains("Match found and recorded."), "got: {html}");
        let order_id = order_page.rsplit('/').next().unwrap();
        assert!(
            html.contains(&format!("/stores/{connection_id}/orders/{order_id}")),
            "links to the matched order: {html}"
        );

        let order = body_text(
            router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(&order_page)
                        .header("cookie", format!("session={session_token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert!(
            order.contains(&txid),
            "the order page lists the recorded payment: {order}"
        );

        // The same transaction looked up from a store it does not pay.
        let other_store = connect(
            "https://other.example.com",
            TEST_VIEW_KEY_HEX.to_owned(),
            TEST_SPEND_PUBKEY_HEX.to_owned(),
        )
        .await;
        let html = body_text(
            router
                .clone()
                .oneshot(form_post_request(
                    &format!("/stores/{other_store}/orders/lookup"),
                    &session_token,
                    &[("txid", &txid)],
                ))
                .await
                .unwrap(),
        )
        .await;
        assert!(
            html.contains(
                "That transaction exists, but doesn&#39;t pay any of this store&#39;s orders."
            ) || html
                .contains("That transaction exists, but doesn't pay any of this store's orders."),
            "got: {html}"
        );

        // A txid mangled when pasted is refused with the engine's reason.
        let html = body_text(
            router
                .clone()
                .oneshot(form_post_request(
                    &lookup,
                    &session_token,
                    &[("txid", "not-a-txid")],
                ))
                .await
                .unwrap(),
        )
        .await;
        assert!(
            html.contains("Couldn&#39;t look that up:") || html.contains("Couldn't look that up:"),
            "got: {html}"
        );

        // With fixi, only the card comes back, answer in it.
        let html = body_text(
            router
                .oneshot(fixi(form_post_request(
                    &lookup,
                    &session_token,
                    &[("txid", &txid)],
                )))
                .await
                .unwrap(),
        )
        .await;
        assert!(
            html.starts_with(r#"<div class="card" id="payment-lookup">"#) && html.contains(&txid),
            "got: {html}"
        );
    }

    // -- "Scan range" row ------------

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
        let (connection_id, public_key) = create_connection(&router, &state, &session_token).await;
        let order_id = seed_real_order(&state, engine.addr, &public_key).await;
        // Deliberately no `bump_scanned_range_for_order` call - this harness runs
        // no background scan loop, so a freshly seeded order genuinely has never
        // been examined by anything yet.

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/orders/{order_id}"))
                    .header("cookie", format!("session={session_token}"))
                    .header("host", "test.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("Scan range"),
            "expected the row's own label, got: {html}"
        );
        assert!(
            html.contains(r#"<span class="muted">-</span>"#),
            "expected the muted-dash fallback, got: {html}"
        );
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
        let (connection_id, public_key) = create_connection(&router, &state, &session_token).await;
        let order_id = seed_real_order(&state, engine.addr, &public_key).await;
        // Still `pending` (non-terminal) - genuinely still in scope, so the range
        // must read as still growing ("N+"), not a closed span. Two calls to the
        // live scanner's own bulk bump (its only mover now that the manual rescan
        // feature is gone) rather than a single-order setter: the first (COALESCE)
        // establishes `first_scanned_height`, the second only advances `last_
        // scanned_height`, matching exactly how two real scan ticks would move it.
        {
            let s = engine.store().lock();
            let tenant = s
                .list_active_tenants()
                .unwrap()
                .into_iter()
                .find(|t| t.public_key == public_key)
                .unwrap();
            s.bump_scanned_heights_for_tenant(&tenant.id, 100, crate::now_unix(), 0)
                .unwrap();
            s.bump_scanned_heights_for_tenant(&tenant.id, 250, crate::now_unix(), 0)
                .unwrap();
        }

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/stores/{connection_id}/orders/{order_id}"))
                    .header("cookie", format!("session={session_token}"))
                    .header("host", "test.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("100+"),
            "expected the still-growing range display, got: {html}"
        );
        assert!(
            !html.contains("100 - 250"),
            "must not show a closed range while still in scope, got: {html}"
        );
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
            order_id: shared::ids::OrderId::new("pay_abc123".to_string()),
            merchant_order_id: None,
            show_merchant_order_id: true,
            description: None,
            address: "addr".to_string(),
            currency: "XMR".to_string(),
            amount: "0.5".to_string(),
            rate_display: "1 XMR per 1 XMR".to_string(),
            rate_provider: "xmr".to_string(),
            xmr_amount_piconero: 500_000_000_000,
            amount_received_piconero: 500_000_000_000,
            status: shared::order_status::OrderStatus::Paid.into(),
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
            payment_link: "http://127.0.0.1:8081/checkout/pay_abc123/share".to_string(),
            scan_range_display: crate::templates::display_scan_range(Some(100), Some(250), false),
        };
        let data = OrderDetailViewModel {
            connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
            display_name: "shop.example.com".to_string(),
            order: Some(order),
        };
        let chrome = crate::views::PageChrome::from_user(None, "");
        let html = crate::views::orders::detail_page(&chrome, &data).into_string();
        assert!(
            html.contains("100 - 250"),
            "expected the closed range display, got: {html}"
        );
        assert!(
            !html.contains("100+"),
            "must not show a still-growing range once no longer being watched, got: {html}"
        );
    }
}
