use shared::order_status::OrderStatus;

use super::{order_events, orders};
use crate::api::checkout;
use crate::api::{self, prelude::*};
use crate::db::OrderCurrencyMetadataRow;
use crate::engine_client::{EngineClientError, OrderView};

api::resource!(
    "/api/v1/orders/{order_id}",
    rel = "order",
    params(order_id: OrderId)
);

/// One order: what to pay, where, and how far the payment has got.
///
/// Its status moves from `pending` through `unconfirmed` and `confirming`
/// to `paid` (or `partial`, `overpaid`, `expired`); `is_final` says it
/// won't change again. Follow `mk:order-events` to hear each change as it
/// happens, and send the payer to `mk:checkout`.
#[api::body]
struct Body {
    /// The order's id: `order_` and a UUID.
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
    /// The price the order was made with, in `currency`; `null` for an
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
    /// When it expires unpaid, in Unix seconds.
    expires_at: i64,
    collection: Link<orders::Path>,
    events: Link<order_events::Path>,
    checkout: Link<checkout::Path>,
}

/// `order` as the engine reports it, priced by `metadata`.
pub(crate) fn of(
    order: &OrderView,
    metadata: Option<&OrderCurrencyMetadataRow>,
    confirmations_required: u64,
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
        collection: Link::new(orders::Path {}),
        events: Link::new(order_events::Path {
            order_id: order_id.clone(),
        }),
        checkout: Link::new(checkout::Path { order_id }),
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
        &detail.order,
        metadata.as_ref(),
        confirmations_required,
    )))
}

/// Read one of the store's orders.
#[get(any_key)]
async fn get(path: Path, orders: Orders) -> api::Result<Body> {
    load(&orders, &path.order_id, None)
        .await?
        .ok_or_else(not_found)
}
