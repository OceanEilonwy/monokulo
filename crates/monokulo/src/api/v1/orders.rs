use super::order;
use crate::api::{self, prelude::*};

api::resource!("/api/v1/orders", rel = "orders");

/// The store's orders, newest first, a page at a time.
///
/// `next` is the next page, while there may be one. Make an order with the
/// `create-order` form.
#[api::body]
struct Body {
    /// This page's orders.
    orders: Vec<order::Body>,
    next: Option<Link<Path>>,
}

/// The most orders one page holds.
pub const MAX_PAGE: u32 = 200;
const DEFAULT_PAGE: u32 = 50;

/// List the store's orders, newest first.
#[get(server, problems = [EngineUnavailable])]
async fn list(orders: Orders, page: Page) -> api::Result<Body> {
    let limit = page.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let offset = page.offset.unwrap_or(0);
    let state = &orders.state;
    let onion = state.onion_uri();
    let store = &orders.store;
    let views = match state
        .engine
        .client
        .list_orders_page(&orders.sk, false, None, limit, offset)
        .await
    {
        Ok(views) => views,
        Err(e) => {
            tracing::warn!(store.id = %store.id, error = %e, "the store's orders could not be read for the API");
            return Err(crate::http::ApiError::EngineUnavailable.into());
        }
    };
    let (id, ids) = (
        store.id.clone(),
        views.iter().map(|o| o.order_id.clone()).collect::<Vec<_>>(),
    );
    let metadata = state
        .db
        .read(move |db| db.order_currency_metadata_for(&id, &ids))
        .await
        .unwrap_or_default();
    // An order without its own snapshot uses the store's present
    // threshold, read once for the page and only when one needs it.
    let fallback = if views.iter().any(|view| {
        metadata
            .get(&view.order_id)
            .and_then(|m| m.confirmations_required_applied)
            .is_none()
    }) {
        state
            .engine
            .client
            .get_tenant(&orders.sk)
            .await
            .map(|t| t.confirmations_required)
            .unwrap_or(crate::confirmation_thresholds::FALLBACK_CONFIRMATIONS)
    } else {
        crate::confirmation_thresholds::FALLBACK_CONFIRMATIONS
    };
    let full = u32::try_from(views.len()).unwrap_or(u32::MAX) >= limit;
    Ok(Body {
        orders: views
            .iter()
            .map(|view| {
                let metadata = metadata.get(&view.order_id);
                let required = metadata
                    .and_then(|m| m.confirmations_required_applied)
                    .unwrap_or(fallback);
                order::of(&store.id, view, metadata, required, onion.as_deref())
            })
            .collect(),
        next: full.then(|| {
            Link::with_query(
                Path {},
                &crate::api::prelude::Page {
                    limit: Some(limit),
                    offset: Some(offset.saturating_add(limit)),
                },
            )
        }),
    })
}

/// An order to make.
#[api::input]
struct Input {
    /// The price, in `currency`.
    amount: Amount,
    /// The currency of `amount`.
    currency: CurrencyCode,
    /// The shop's own reference for the order (its order or cart number).
    merchant_order_id: Option<MerchantRef>,
    /// What the order is for, shown to the payer under the store's name.
    /// Without one the checkout shows the store's payment description.
    description: Option<Description>,
    /// Whether the checkout shows the payer `merchant_order_id`; true when
    /// left out. A shop whose reference means nothing to its customers
    /// sends false.
    show_merchant_order_id: Option<bool>,
    /// The caller's key for this one purchase: a retry with the same key
    /// and the same order gets the order the first attempt made, never a
    /// second one, at the price it was made at. With the public key, keys
    /// are the website's own, apart from the server's and the apps', so
    /// make them unguessable.
    idempotency_key: Option<IdempotencyKey>,
}

/// Make an order.
///
/// With the store's secret key or an app's (a shop's server, a till) it is
/// recorded as made with the key; with the public key it's the website's,
/// and a store that takes orders only from its verified websites refuses it
/// from anywhere else. A shop plugin names itself in `Monokulo-Client:
/// <slug>/<version>`. Answers `201 Created` with the order and its
/// `Location`.
#[post(
    any_key,
    form = "create-order",
    problems = [
        InvalidAmount,
        RatesUnavailable,
        IdempotencyConflict,
        InvalidRequest,
        EngineUnavailable,
        Internal,
    ]
)]
async fn create(
    caller: Caller,
    orders: Orders,
    client: Client,
    input: Input,
) -> api::Result<Created<order::Body>> {
    let state = orders.state.clone();
    let onion = state.onion_uri();
    let keyed = input.idempotency_key.is_some();
    let request = crate::orders::OrderRequest {
        amount: input.amount.into_string(),
        currency: input.currency.into_string(),
        merchant_order_id: input.merchant_order_id.map(MerchantRef::into_string),
        description: input.description.map(Description::into_string),
        show_merchant_order_id: input.show_merchant_order_id.unwrap_or(true),
        idempotency_key: input.idempotency_key,
    };
    let origin = crate::orders::Origin::Api(Box::new(crate::orders::ApiCaller {
        credential: caller.0.credential,
        client: client.0,
    }));
    let made = crate::orders::make(&state, &orders.store, origin, request)
        .await
        .map_err(|e| e.problem())?;
    let order = match made {
        crate::orders::Made::Created(order) => order,
        // The order an earlier attempt made, as it is now.
        crate::orders::Made::Found(order_id) => {
            let body = order::load(&orders, &order_id, None)
                .await?
                .ok_or_else(internal)?;
            return Ok(Created(body));
        }
    };
    // A retry with the same idempotency key whose first answer was lost
    // gets the order the first attempt made, which may have moved on
    // since: read it as it is now. A new order is as the engine just made
    // it.
    let now = match keyed {
        true => order::load(&orders, &order.order_id, None)
            .await
            .ok()
            .flatten(),
        false => None,
    };
    let body = now.unwrap_or_else(|| {
        let view = crate::engine_client::OrderView {
            order_id: order.order_id.clone(),
            merchant_order_id: order.merchant_order_id.clone(),
            description: order.description.clone(),
            address: order.address.clone(),
            xmr_amount_piconero: order.xmr_amount_piconero,
            amount_received_piconero: 0,
            status: shared::order_status::OrderStatus::Pending,
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
        let store = &orders.store.id;
        order::of(
            store,
            &view,
            Some(&order.metadata(store)),
            order.price.confirmations_required,
            onion.as_deref(),
        )
    });
    Ok(Created(body))
}
