use std::time::Duration;

use axum::response::sse::Event;

use super::order;
use crate::api::runtime::auth;
use crate::api::{self, prelude::*};

api::resource!(
    "/api/v1/orders/{order_id}/events",
    rel = "order-events",
    params(order_id: OrderId),
    kind = stream,
    doc = "One order's changes, as server-sent events.\n\nAn `order` event carries the order (the same JSON as reading it) each time it changes, and every 30 seconds as its time to pay runs down; the stream ends once the order is final, after a last `order` event. `done` with `not_found` ends it for an order that's gone."
);

/// What an order's event stream sends.
pub(crate) struct OrderEvents;

impl Events for OrderEvents {
    fn events() -> Vec<StreamEvent> {
        vec![
            StreamEvent {
                name: "order",
                doc: "The order as it is now: first, each time it changes, and every 30 seconds. After a final one (`is_final`) the stream ends.",
                data: |g| g.subschema_for::<order::Body>().to_value(),
            },
            StreamEvent {
                name: "done",
                doc: "The order is gone; the stream ends.",
                data: |_| serde_json::json!({"type": "string", "const": "not_found"}),
            },
        ]
    }
}

/// Follow one order's changes as server-sent events.
#[get(any_key, problems = [NotFound, EngineUnavailable])]
async fn get(
    path: Path,
    orders: Orders,
    slot: StreamSlot,
) -> api::Result<EventStream<OrderEvents>> {
    let order_id = path.order_id;
    if order::load(&orders, &order_id, None).await?.is_none() {
        return Err(not_found());
    }
    // Each event is written later, as this request's answer.
    let context = auth::context();
    let subscription =
        orders
            .state
            .engine
            .client
            .subscribe_order(&orders.store.id, &orders.sk, &order_id);
    let shared = subscription.shared_detail();
    let permit = slot.0;
    Ok(EventStream::new(crate::live::live_events(
        subscription,
        Duration::from_secs(30),
        move || {
            let _permit = &permit;
            let (orders, order_id, shared, context) = (
                orders.clone(),
                order_id.clone(),
                shared.clone(),
                context.clone(),
            );
            async move {
                let order = match order::load(&orders, &order_id, Some(&shared)).await {
                    Ok(Some(order)) => order,
                    Ok(None) => {
                        return Some(crate::live::LiveSnapshot {
                            events: vec![Event::default().event("done").data("not_found")],
                            fingerprint: "not_found".to_string(),
                            terminal: true,
                        })
                    }
                    Err(_) => return None,
                };
                let json = auth::as_caller(context, || serde_json::to_string(&order)).ok()?;
                Some(crate::live::LiveSnapshot {
                    events: vec![Event::default().event("order").data(json.clone())],
                    fingerprint: json,
                    terminal: order.is_final,
                })
            }
        },
    )))
}
