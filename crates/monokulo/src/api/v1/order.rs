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
/// `mk:dashboard-order`, the order's page for the merchant. The store's
/// secret key and its apps' keys get the `renew-order` form: an order that
/// expired with nothing paid to it can be given a new price and expiry,
/// keeping its id and address.
#[api::body]
struct Body {
    /// The order's id: `order_` and 32 hex digits.
    order_id: OrderId,
    /// Where the payment is.
    status: OrderStatus,
    /// Whether the status can still change. An `expired` or `cancelled`
    /// order isn't final while Monokulo still watches it for a payment that
    /// arrived late, which turns it paid.
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
    /// The device that made it, as the merchant named it, for an order one
    /// of the store's connected apps made; left out for any other.
    device_name: Option<String>,
    /// What the order is for, as the payer sees it.
    description: Option<String>,
    /// Whether the checkout shows the payer `merchant_order_id`.
    show_merchant_order_id: bool,
    /// Confirmations the payment has. It grows a block at a time while the
    /// order is `confirming`, with no sale event for each: read the order
    /// again, or follow `mk:order-events`, to count them.
    confirmations: u64,
    /// Confirmations it needs to count as paid: the store's threshold for
    /// its amount, as the order was made with it.
    confirmations_required: u64,
    /// When it was made, in Unix seconds.
    created_at: i64,
    /// When it expires unpaid, in Unix seconds. A cancelled order's is
    /// when it was cancelled.
    expires_at: i64,
    /// When a payment to it was found spent twice, in Unix seconds; left
    /// out unless one was. Don't hand anything over for it unless it's
    /// `paid` and `is_final` after that.
    double_spend_detected_at: Option<i64>,
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
    /// An absolute URL, `http://….onion/api/v1/checkout/{order_id}`, present while
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
/// `metadata`, made by the app on `device_name` when an app made it, with
/// its onion checkout on `onion` (`AppState::onion_uri`) when there is one.
pub(crate) fn of(
    store: &ConnectionId,
    order: &OrderView,
    metadata: Option<&OrderCurrencyMetadataRow>,
    device_name: Option<String>,
    confirmations_required: u64,
    onion: Option<&str>,
) -> Body {
    let is_final = order.is_final();
    let order_id = order.order_id.clone();
    Body {
        order_id: order_id.clone(),
        status: order.status,
        is_final,
        address: order.address.clone(),
        xmr_amount_piconero: order.xmr_amount_piconero.get(),
        amount_received_piconero: order.amount_received_piconero.get(),
        amount: metadata.map(|m| m.amount.clone()),
        currency: metadata.map(|m| m.currency.clone()),
        merchant_order_id: order.merchant_order_id.clone(),
        device_name,
        description: order.description.clone(),
        show_merchant_order_id: metadata.is_none_or(|m| m.show_merchant_order_id),
        confirmations: order.confirmations,
        confirmations_required,
        created_at: order.created_at,
        expires_at: order.expires_at,
        double_spend_detected_at: order.double_spend_detected_at,
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

/// `order`, just made for the store `store` (by the app on `device_name`,
/// when an app made it), as reading it now answers: waiting, nothing
/// received, no confirmations.
pub(crate) fn of_new(
    store: &ConnectionId,
    order: &crate::orders::NewOrder,
    device_name: Option<String>,
    onion: Option<&str>,
) -> Body {
    let view = OrderView {
        order_id: order.order_id.clone(),
        merchant_order_id: order.merchant_order_id.clone(),
        description: order.description.clone(),
        address: order.address.clone(),
        xmr_amount_piconero: order.xmr_amount_piconero,
        amount_received_piconero: pricing::xmr_amount::Piconero::ZERO,
        status: OrderStatus::Pending,
        confirmations: 0,
        double_spend_detected_at: None,
        cancelled_at: None,
        refund_address: None,
        created_at: order.created_at,
        expires_at: order.expires_at,
        updated_at: order.created_at,
        first_scanned_height: None,
        last_scanned_height: None,
        currently_scanning: false,
    };
    of(
        store,
        &view,
        Some(&order.metadata(store)),
        device_name,
        order.price.confirmations_required,
        onion,
    )
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
    let (metadata, device_name) = state
        .db
        .read(move |db| {
            let metadata = db.get_order_currency_metadata(&id, &order)?;
            let device = db.app_order_devices(&id, std::slice::from_ref(&order))?;
            Ok::<_, crate::db::DbError>((metadata, device.into_values().next()))
        })
        .await
        .unwrap_or_default();
    Ok(Some(of(
        &orders.store.id,
        &detail.order,
        metadata.as_ref(),
        device_name,
        confirmations_required,
        state.onion_uri().as_deref(),
    )))
}

/// The store's order `order_id`, read outside a request (a push queued as
/// its sale event is read), as [`load`] reads it: with the store's key as
/// `keys` reads it now, and its onion checkout on `onion`. `None` when the
/// store or the order is gone, or it couldn't be read.
pub(crate) async fn read_outside_request(
    db: &crate::db::Database,
    engine: &crate::engine_client::EngineClient,
    keys: &crate::http::store_key::StoreKeys,
    store_id: &ConnectionId,
    order_id: &OrderId,
    onion: Option<&str>,
) -> Option<Body> {
    let (_, sk) = keys.current(store_id).await.ok()??;
    let detail = engine.get_order_detail(&sk, order_id).await.ok()?;
    let (id, order) = (store_id.clone(), order_id.clone());
    let (metadata, device_name) = db
        .read(move |db| {
            let metadata = db.get_order_currency_metadata(&id, &order)?;
            let device = db.app_order_devices(&id, std::slice::from_ref(&order))?;
            Ok::<_, crate::db::DbError>((metadata, device.into_values().next()))
        })
        .await
        .unwrap_or_default();
    // As `http::resolve_confirmations_required` decides it.
    let confirmations_required = match metadata
        .as_ref()
        .and_then(|m| m.confirmations_required_applied)
    {
        Some(applied) => applied,
        None => engine
            .get_tenant(&sk)
            .await
            .map(|t| t.confirmations_required)
            .unwrap_or(crate::confirmation_thresholds::FALLBACK_CONFIRMATIONS),
    };
    Some(of(
        store_id,
        &detail.order,
        metadata.as_ref(),
        device_name,
        confirmations_required,
        onion,
    ))
}

/// Read one of the store's orders.
#[get(any_key, problems = [NotFound, EngineUnavailable])]
async fn get(path: Path, orders: Orders) -> api::Result<Body> {
    load(&orders, &path.order_id, None)
        .await?
        .ok_or_else(not_found)
}

/// What renewing an order needs: which of its quotes ran out.
#[api::input]
struct Renewal {
    /// The order's `expires_at` as you last read it: the quote that ran
    /// out. Only that quote is renewed, so a second request for it (another
    /// device, or a retry) gets the order as it is, renewed once.
    expires_at: i64,
}

/// Renew this order: a new price and expiry, the same order.
///
/// For an order that expired with nothing paid to it: Monokulo prices its
/// amount again, in its currency, at the store's rate now, and gives it a
/// fresh expiry, as long as a new order's. Its id, address, reference and
/// the confirmations it needs stay, so a till's sale keeps one order however
/// often the customer needs a new code. It is `pending` again, announced on
/// the store's event stream and pushes as `sale.waiting` with the new
/// `xmr_amount_piconero` and `expires_at`, and to webhooks as
/// `order.pending`.
///
/// Send the `expires_at` you last read: when the order's isn't that any more
/// (another device renewed it first), nothing changes, and the answer is the
/// order as it is. A payment still sent at the old price counts in full,
/// against the new one: the order is `partial` (the customer pays the rest
/// to the same address), `paid` or `overpaid`.
///
/// Refused (`409` `order-not-renewable`) once any payment has reached it,
/// even one still in the pool, and when it ended too long ago for Monokulo
/// to still watch its address (`payment.expired_order_grace_period_minutes`,
/// 6 hours unless the admin changed it). An app's key renews only orders
/// the store's apps made. Answers `200` with the order.
#[post(
    server,
    form = "renew-order",
    problems = [
        NotFound,
        Forbidden,
        OrderNotRenewable,
        RatesUnavailable,
        EngineUnavailable,
        Internal,
    ]
)]
async fn renew(path: Path, orders: Orders, caller: Caller, input: Renewal) -> api::Result<Body> {
    let app = match &caller.0.credential {
        crate::http::store_key::Credential::App(app) => Some(app),
        _ => None,
    };
    crate::orders::renew(
        &orders.state,
        &orders.store,
        &orders.sk,
        &path.order_id,
        input.expires_at,
        app,
    )
    .await
    .map_err(|e| e.problem())?;
    load(&orders, &path.order_id, None)
        .await?
        .ok_or_else(not_found)
}
