use std::convert::Infallible;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use futures_util::stream;
use tokio::sync::broadcast::error::RecvError;

use super::order;
use crate::api::runtime::auth;
use crate::api::{self, prelude::*};
use crate::apps::SaleEvent;
use crate::http::store_key::Credential;
use crate::now_unix;

api::resource!(
    "/api/v1/events",
    rel = "events",
    kind = stream,
    doc = "The store's sale events, as server-sent events: one stream for the orders its connected apps make.\n\nIt starts with `ready`; then each change to such an order is an `event:` named after it (`sale.waiting`, `sale.paid`, `sale.expired`, ...) with the sale as its JSON data, linking its order (`mk:order`). `lagged` means some were missed: read the orders again. A comment arrives every 25 seconds while nothing happens. A connected app's stream ends when the app is disconnected."
);

/// The comment an open stream sends while nothing happens, inside common
/// proxy idle timeouts.
const KEEP_ALIVE: Duration = Duration::from_secs(25);

/// The stream is open: what follows is new.
#[api::object]
struct Ready {
    /// When it opened, in Unix seconds.
    at: i64,
}

/// Some events were missed: the stream fell behind. Read the orders again.
#[api::object]
struct Lagged {
    /// How many.
    missed: u64,
}

/// A change to an order one of the store's apps made.
#[api::object]
struct Sale {
    /// What happened, the same as the `event:` name: `sale.waiting`,
    /// `sale.seen`, `sale.confirming`, `sale.paid`, `sale.partial`,
    /// `sale.expired` or `sale.double_spend`.
    event: String,
    /// The order's id.
    order_id: OrderId,
    /// The price, as the app sent it, in `currency`.
    amount: String,
    /// The currency of `amount`.
    currency: String,
    /// The Monero address to pay.
    address: String,
    /// What to pay, in piconero (10^-12 XMR).
    xmr_amount_piconero: u64,
    /// The app's own reference for the order.
    merchant_order_id: Option<String>,
    /// When it expires unpaid, in Unix seconds.
    expires_at: i64,
    /// The device that made the order.
    device_name: String,
    /// When this happened, in Unix seconds.
    at: i64,
    order: Link<order::Path>,
}

impl Sale {
    fn of(sale: &SaleEvent) -> Self {
        let order_id = OrderId::new(sale.order_id.clone());
        Sale {
            event: sale.event.to_owned(),
            order_id: order_id.clone(),
            amount: sale.amount.clone(),
            currency: sale.currency.clone(),
            address: sale.address.clone(),
            xmr_amount_piconero: sale.xmr_amount_piconero,
            merchant_order_id: sale.merchant_order_id.clone(),
            expires_at: sale.expires_at,
            device_name: sale.device_name.clone(),
            at: sale.at,
            order: Link::new(order::Path { order_id }),
        }
    }
}

/// What the store's event stream sends.
pub(crate) struct SaleEvents;

impl Events for SaleEvents {
    fn events() -> Vec<StreamEvent> {
        let mut events = vec![StreamEvent {
            name: "ready",
            doc: "First, once the stream is open.",
            data: |g| g.subschema_for::<Ready>().to_value(),
        }];
        events.extend(SaleEvent::NAMES.iter().map(|&name| StreamEvent {
            name,
            doc: sale_doc(name),
            data: |g| g.subschema_for::<Sale>().to_value(),
        }));
        events.push(StreamEvent {
            name: "lagged",
            doc: "Some events were missed: read the orders again.",
            data: |g| g.subschema_for::<Lagged>().to_value(),
        });
        events
    }
}

/// What the sale event `name` says happened.
fn sale_doc(name: &str) -> &'static str {
    match name {
        "sale.waiting" => "An app made the order; it waits for the payment.",
        "sale.seen" => "The payment is in the pool, not yet in a block.",
        "sale.confirming" => "The payment is in a block, with too few confirmations yet.",
        "sale.paid" => "Paid in full (or more), with enough confirmations.",
        "sale.partial" => {
            "Less than the amount has arrived; the rest can still come before it expires."
        }
        "sale.expired" => "It expired unpaid.",
        "sale.double_spend" => "The payment was spent twice: don't hand anything over.",
        _ => "",
    }
}

/// `data` as an event named `name`.
fn event(name: &str, data: &impl serde::Serialize) -> Event {
    Event::default()
        .event(name)
        .data(serde_json::to_string(data).unwrap_or_default())
}

/// Follow the store's sale events as server-sent events.
#[get(server)]
async fn get(
    caller: Caller,
    instance: Instance,
    slot: StreamSlot,
) -> api::Result<EventStream<SaleEvents>> {
    let state = instance.0;
    let caller = caller.0;
    let receiver = state.apps.subscribe();
    let disconnects = state.apps.subscribe_disconnects();
    let pk = caller.store.tenant_public_key.clone();
    let app_id = match caller.credential {
        Credential::App(app) => Some(app.id),
        _ => None,
    };
    // Each event is written later, as this request's answer.
    let context = auth::context();
    let permit = slot.0;
    let ready = event("ready", &Ready { at: now_unix() });
    let events = stream::unfold(
        (receiver, disconnects, pk, app_id, permit, context),
        |(mut receiver, mut disconnects, pk, app_id, permit, context)| async move {
            loop {
                tokio::select! {
                    gone = disconnects.recv() => match gone {
                        Ok(gone) if Some(&gone) == app_id.as_ref() => return None,
                        Ok(_) | Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => return None,
                    },
                    announced = receiver.recv() => match announced {
                        Ok(announced) if announced.public_key == pk => {
                            let sale = Sale::of(&announced.event);
                            let event = auth::as_caller(context.clone(), || event(announced.event.event, &sale));
                            return Some((Ok::<_, Infallible>(event), (receiver, disconnects, pk, app_id, permit, context)));
                        }
                        Ok(_) => {}
                        Err(RecvError::Lagged(missed)) => {
                            let event = event("lagged", &Lagged { missed });
                            return Some((Ok(event), (receiver, disconnects, pk, app_id, permit, context)));
                        }
                        Err(RecvError::Closed) => return None,
                    },
                }
            }
        },
    );
    let events = futures_util::StreamExt::chain(stream::once(async { Ok(ready) }), events);
    Ok(EventStream::new(
        Sse::new(events)
            .keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
            .into_response(),
    ))
}
