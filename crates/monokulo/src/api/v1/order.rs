use engine_protocol::order_status::OrderStatus;

use super::{order_events, orders};
use crate::api::{self, prelude::*};
use crate::api::{checkout, dashboard_order};
use crate::db::{ConnectionId, OrderCurrencyMetadataRow};
use crate::engine_client::{EngineClientError, OrderView};

api::resource!(
    "/api/v1/orders/{order_id}",
    rel = "order",
    params(order_id: OrderId)
);

/// One order: what to pay, where, and how far the payment has got.
///
/// Its status moves from `pending` through `unconfirmed` and `confirming`
/// to `paid` (or `partial`, `overpaid`, `expired`, or `cancelled` when the
/// merchant called it off before anything was paid); `is_final` says it
/// won't change again. Follow `mk:order-events` to hear each change as it
/// happens, and send the payer to `mk:checkout`, or, when they're on Tor,
/// to `mk:onion-checkout`. The store's secret key also gets
/// `mk:dashboard-order`, the order's page for the merchant.
#[api::body]
struct Body {
    /// The order's id: `order_` and 32 hex digits.
    order_id: OrderId,
    /// Where the payment is.
    status: OrderStatus,
    /// Whether the status can still change.
    is_final: bool,
    /// The Monero address to pay: a subaddress of the store's wallet, this
    /// order's alone.
    address: String,
    /// What to pay, in piconero (10^-12 XMR).
    xmr_amount_piconero: u64,
    /// What has arrived so far, in piconero.
    amount_received_piconero: u64,
    /// The price the order was made with, in `currency`; left out for an
    /// order Monokulo has no record of.
    amount: Option<String>,
    /// The currency of `amount`.
    currency: Option<String>,
    /// The shop's own reference for the order.
    merchant_order_id: Option<String>,
    /// What the order is for, as the payer sees it.
    description: Option<String>,
    /// Whether the checkout shows the payer `merchant_order_id`.
    show_merchant_order_id: bool,
    /// Confirmations the payment has.
    confirmations: u64,
    /// Confirmations it needs to count as paid.
    confirmations_required: u64,
    /// When it was made, in Unix seconds.
    created_at: i64,
    /// When it expires unpaid, in Unix seconds. A cancelled order's is
    /// when it was cancelled.
    expires_at: i64,
    /// When the merchant cancelled it, in Unix seconds; left out unless they
    /// did. A payment sent anyway is still recorded: in
    /// `amount_received_piconero` while the order stays `cancelled`, and,
    /// paid in full, the order is paid like any other and keeps this time.
    cancelled_at: Option<i64>,
    collection: Link<orders::Path>,
    events: Link<order_events::Path>,
    checkout: Link<checkout::Path>,
    dashboard: Link<dashboard_order::Path>,
    /// The order's checkout page on this Monokulo's onion address.
    ///
    /// An absolute URL, `http://….onion/checkout/{order_id}`, present while
    /// the Monokulo has an onion address, whichever address the order was
    /// read at; absent otherwise.
    ///
    /// Send a payer who is on Tor here, never to `mk:checkout`: a shop
    /// plugin whose visitor is browsing the shop's own `.onion` address
    /// calls Monokulo from its server over the web address, where
    /// `mk:checkout` resolves to the web checkout, and sending the visitor
    /// there would take them off Tor. This link keeps them on it. A client
    /// that itself called over the onion address can follow `mk:checkout`
    /// as well: it resolves to the same page.
    onion_checkout: Option<OnionLink<checkout::Path>>,
}

/// `order`, of the store `store`, as the engine reports it, priced by
/// `metadata`, with its onion checkout on `onion` (`AppState::onion_uri`)
/// when there is one.
pub(crate) fn of(
    store: &ConnectionId,
    order: &OrderView,
    metadata: Option<&OrderCurrencyMetadataRow>,
    confirmations_required: u64,
    onion: Option<&str>,
) -> Body {
    let (_, _, is_final) = crate::views::order_state(order.status);
    let order_id = order.order_id.clone();
    Body {
        order_id: order_id.clone(),
        status: order.status,
        is_final,
        address: order.address.clone(),
        xmr_amount_piconero: order.xmr_amount_piconero,
        amount_received_piconero: order.amount_received_piconero,
        amount: metadata.map(|m| m.amount.clone()),
        currency: metadata.map(|m| m.currency.clone()),
        merchant_order_id: order.merchant_order_id.clone(),
        description: order.description.clone(),
        show_merchant_order_id: metadata.is_none_or(|m| m.show_merchant_order_id),
        confirmations: order.confirmations,
        confirmations_required,
        created_at: order.created_at,
        expires_at: order.expires_at,
        cancelled_at: order.cancelled_at,
        collection: Link::new(orders::Path {}),
        events: Link::new(order_events::Path {
            order_id: order_id.clone(),
        }),
        checkout: Link::new(checkout::Path {
            order_id: order_id.clone(),
        }),
        dashboard: Link::new(dashboard_order::Path {
            id: store.clone(),
            order_id: order_id.clone(),
        }),
        onion_checkout: OnionLink::new(onion, checkout::Path { order_id }),
    }
}

/// The store's order `order_id`, as the engine has it now; `None` for an
/// order that isn't the store's. Reads through `shared` when given (a
/// live stream's re-read, shared by every stream on the order).
pub(crate) async fn load(
    orders: &Orders,
    order_id: &OrderId,
    shared: Option<&crate::live::SharedDetail>,
) -> api::Result<Option<Body>> {
    let state = &orders.state;
    let read = state.engine.client.get_order_detail(&orders.sk, order_id);
    let detail = match shared {
        Some(shared) => shared.get(read).await,
        None => read.await,
    };
    let detail = match detail {
        Ok(detail) => detail,
        Err(EngineClientError::EngineError { status, .. })
            if status == reqwest::StatusCode::NOT_FOUND =>
        {
            return Ok(None)
        }
        Err(e) => {
            tracing::warn!(order.id = %order_id, error = %e, "an order could not be read for the API");
            return Err(crate::http::ApiError::EngineUnavailable.into());
        }
    };
    let confirmations_required =
        crate::http::resolve_confirmations_required(state, &orders.store.id, &orders.sk, order_id)
            .await;
    let (id, order) = (orders.store.id.clone(), order_id.clone());
    let metadata = state
        .db
        .read(move |db| db.get_order_currency_metadata(&id, &order))
        .await
        .ok()
        .flatten();
    Ok(Some(of(
        &orders.store.id,
        &detail.order,
        metadata.as_ref(),
        confirmations_required,
        state.onion_uri().as_deref(),
    )))
}

/// Read one of the store's orders.
#[get(any_key, problems = [NotFound, EngineUnavailable])]
async fn get(path: Path, orders: Orders) -> api::Result<Body> {
    load(&orders, &path.order_id, None)
        .await?
        .ok_or_else(not_found)
}
