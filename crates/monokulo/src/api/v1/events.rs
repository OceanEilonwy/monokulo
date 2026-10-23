use std::convert::Infallible;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use futures_util::stream;
use tokio::sync::broadcast::error::RecvError;

use crate::api::{self, prelude::*};
use crate::http::store_key::Credential;
use crate::now_unix;

api::resource!(
    "/api/v1/events",
    rel = "events",
    kind = stream,
    doc = "The store's sale events, as server-sent events: one stream for all its orders.\n\nIt starts with `ready`; then each sale event is an `event:` named after it (`sale.waiting`, `sale.paid`, `sale.expired`, ...) with its JSON as data. `lagged` means some were missed: read the orders again. A connected app's stream ends when the app is disconnected."
);

/// The comment an open stream sends while nothing happens, inside common
/// proxy idle timeouts.
const KEEP_ALIVE: Duration = Duration::from_secs(25);

/// Follow the store's sale events as server-sent events.
#[get(server)]
async fn get(caller: Caller, instance: Instance, slot: StreamSlot) -> api::Result<EventStream> {
    let state = instance.0;
    let caller = caller.0;
    let receiver = state.apps.subscribe();
    let disconnects = state.apps.subscribe_disconnects();
    let pk = caller.store.tenant_public_key.clone();
    let app_id = match caller.credential {
        Credential::App(app) => Some(app.id),
        _ => None,
    };
    let permit = slot.0;
    let ready = Event::default()
        .event("ready")
        .data(serde_json::json!({ "at": now_unix() }).to_string());
    let events = stream::unfold(
        (receiver, disconnects, pk, app_id, permit),
        |(mut receiver, mut disconnects, pk, app_id, permit)| async move {
            loop {
                tokio::select! {
                    gone = disconnects.recv() => match gone {
                        Ok(gone) if Some(&gone) == app_id.as_ref() => return None,
                        Ok(_) | Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => return None,
                    },
                    announced = receiver.recv() => match announced {
                        Ok(announced) if announced.public_key == pk => {
                            let data = serde_json::to_string(&announced.event).unwrap_or_default();
                            let event = Event::default().event(announced.event.event).data(data);
                            return Some((Ok::<_, Infallible>(event), (receiver, disconnects, pk, app_id, permit)));
                        }
                        Ok(_) => {}
                        Err(RecvError::Lagged(missed)) => {
                            let event = Event::default()
                                .event("lagged")
                                .data(serde_json::json!({ "missed": missed }).to_string());
                            return Some((Ok(event), (receiver, disconnects, pk, app_id, permit)));
                        }
                        Err(RecvError::Closed) => return None,
                    },
                }
            }
        },
    );
    let events = futures_util::StreamExt::chain(stream::once(async { Ok(ready) }), events);
    Ok(EventStream(
        Sse::new(events)
            .keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
            .into_response(),
    ))
}
