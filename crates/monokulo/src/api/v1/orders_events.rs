use std::collections::HashMap;
use std::convert::Infallible;

use axum::response::sse::Event;
use futures_util::stream;
use tokio::sync::broadcast::error::RecvError;

use super::events::{Lagged, Ready, KEEP_ALIVE};
use super::order;
use crate::api::runtime::auth;
use crate::api::{self, prelude::*};
use crate::live::{StoreChange, StoreSubscription};

api::resource!(
    "/api/v1/orders/events",
    rel = "orders-events",
    kind = stream,
    doc = "The orders the store's connected apps make, as server-sent events, each as the order itself: one stream for all of them.\n\nIt starts with `ready`; then each time such an order changes, an `order` event carries it, the same JSON as reading it (`mk:order`), read as it's sent: its status, what has arrived, and its confirmations, one event for each new one while it's `confirming`. An order that hasn't changed isn't sent again. `lagged` means some changes may have been missed: read the orders you're showing again. A comment arrives every 25 seconds while nothing happens. Its key is the store's secret key or one of its apps' keys, as for `mk:events`, and it ends once that key stops working: a connected app disconnected, or the store's secret key replaced."
);

/// How long `ready` waits for Monokulo's own view of the store's changes
/// to open. Past it, `ready` is sent anyway, and `lagged` follows once it
/// opens.
const READY_PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

/// What the store's order stream sends.
pub(crate) struct OrderStreamEvents;

impl Events for OrderStreamEvents {
    fn events() -> Vec<StreamEvent> {
        vec![
            StreamEvent {
                name: "ready",
                doc: "First, once the stream is open: read the orders you're showing, then follow the stream.",
                data: |g| g.subschema_for::<Ready>().to_value(),
            },
            StreamEvent {
                name: "order",
                doc: "An order one of the store's apps made, as it is now (the same JSON as reading it), each time it changes: made, paid to, each new confirmation, expired, renewed.",
                data: |g| g.subschema_for::<order::Body>().to_value(),
            },
            StreamEvent {
                name: "lagged",
                doc: "Some changes may have been missed: read the orders you're showing again.",
                data: |g| g.subschema_for::<Lagged>().to_value(),
            },
        ]
    }
}

/// What the stream holds between events.
struct Following {
    subscription: StoreSubscription,
    orders: Orders,
    context: auth::Context,
    /// The order last sent of each order, as sent: a change that leaves
    /// it the same sends nothing.
    sent: HashMap<OrderId, String>,
}

impl Following {
    /// The next event: an order that changed, or `lagged`; `None` when
    /// the store's changes end.
    async fn next(&mut self) -> Option<Event> {
        loop {
            match self.subscription.changes.recv().await {
                Ok(StoreChange::Order(order_id)) => {
                    if let Some(event) = self.read(&order_id).await {
                        return Some(event);
                    }
                }
                Ok(StoreChange::All) => return Some(lagged(0)),
                Err(RecvError::Lagged(missed)) => return Some(lagged(missed)),
                Err(RecvError::Closed) => return None,
            }
        }
    }

    /// `order_id` as an `order` event: `None` when it isn't an order of
    /// the store's apps, couldn't be read, or is as it was last sent.
    async fn read(&mut self, order_id: &OrderId) -> Option<Event> {
        let state = &self.orders.state;
        let id = order_id.clone();
        let app_order = state.db.read(move |db| db.app_order(&id)).await.ok()??;
        if app_order.store_id != self.orders.store.id {
            return None;
        }
        // The store's key as it is now: it may have been replaced since.
        let orders = self.orders.current().await?;
        let body = order::load(&orders, order_id, None).await.ok()??;
        let json = auth::as_caller(self.context.clone(), || serde_json::to_string(&body)).ok()?;
        if self.sent.get(order_id) == Some(&json) {
            return None;
        }
        self.sent.insert(order_id.clone(), json.clone());
        Some(Event::default().event("order").data(json))
    }
}

/// `lagged`, with how many changes were dropped: 0 when that isn't known
/// (Monokulo's own view of the store's changes opened again).
fn lagged(missed: u64) -> Event {
    Event::default()
        .event("lagged")
        .data(serde_json::to_string(&Lagged { missed }).unwrap_or_default())
}

/// Follow the orders the store's apps make, each as the order itself.
#[get(server)]
async fn get(orders: Orders, slot: StreamSlot) -> api::Result<EventStream<OrderStreamEvents>> {
    let state = orders.state.clone();
    // Subscribed before `ready` is sent, so nothing after it is missed.
    let subscription = state
        .engine
        .client
        .subscribe_store(&state.store_keys(), &orders.store.id);
    // Each event is written later, as this request's answer.
    let context = auth::context();
    let following = Following {
        subscription,
        orders,
        context,
        sent: HashMap::new(),
    };
    let clock = state.clock.clone();
    // `ready` once Monokulo's own view of the store's changes is open (or
    // has had its chance to be), so a change after it is never missed
    // without `lagged`; then the changes.
    let events = stream::unfold((following, false), move |(mut following, open)| {
        let clock = clock.clone();
        async move {
            if !open {
                following.subscription.ready(READY_PATIENCE).await;
                let ready = Event::default().event("ready").data(
                    serde_json::to_string(&Ready {
                        at: clock.now().as_secs(),
                    })
                    .unwrap_or_default(),
                );
                return Some((Ok::<_, Infallible>(ready), (following, true)));
            }
            let event = following.next().await?;
            Some((Ok(event), (following, true)))
        }
    });
    // It ends once its key stops working: the app disconnected, or the
    // store's secret key replaced.
    let events = slot.bound(events);
    Ok(EventStream::new(crate::live::sse_with_keep_alive(
        &state.stopping,
        events,
        KEEP_ALIVE,
    )))
}
