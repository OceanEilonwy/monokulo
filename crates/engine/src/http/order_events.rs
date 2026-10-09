//! `GET /api/v1/admin/order-events`: the order-event log
//! (`store::order_events`) as server-sent events, for monokulo, which
//! delivers every store's webhooks from it (`docs/DESIGN.md` §11).
//!
//! The protocol:
//!
//! - The reader says where it is with `Last-Event-ID` (as `EventSource`
//!   reconnects do) or, failing that, `?after=<seq>`; neither is `0`, the
//!   start of the log.
//! - Each event is an `order_event` whose `id:` is its `seq` and whose data
//!   is its JSON ([`crate::store::OrderEvent::wire_json`]). Every kept event
//!   after the reader's position is sent first, oldest first, then each new
//!   one as it commits.
//! - If some events after the reader's position are gone (pruned after
//!   `order_events.retention_days`, or the position is past anything this
//!   log ever had), a single `events_lost` comes first, its `id:` where the
//!   stream carries on from and its data
//!   `{"requested_after": n, "resume_after": m}`: the reader missed every
//!   event in between, and should say so loudly.
//! - A keep-alive comment every 15 s.
//!
//! Engine token only, like every engine route: the log covers every store.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;

use crate::store::{Database, ResumePoint};

use super::ApiError;

/// Most events read from the log at once.
const BATCH: usize = 256;
/// How often the log is read even when no commit said so: a safety net,
/// since every append also wakes the stream.
const POLL_EVERY: Duration = Duration::from_secs(10);

#[derive(serde::Deserialize)]
pub(super) struct AfterQuery {
    after: Option<i64>,
}

/// The reader's position: `Last-Event-ID`, else `?after=`, else 0.
fn position(headers: &HeaderMap, query: &AfterQuery) -> Result<i64, ApiError> {
    let header = headers
        .get("last-event-id")
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|text| text.trim().parse::<i64>().ok())
                .ok_or_else(|| ApiError::BadRequest("Last-Event-ID must be an event's id".into()))
        })
        .transpose()?;
    let after = header.or(query.after).unwrap_or(0);
    if after < 0 {
        return Err(ApiError::BadRequest("an event id is never negative".into()));
    }
    Ok(after)
}

/// What the stream keeps between reads of the log.
struct Reader {
    db: Database,
    appended: tokio::sync::watch::Receiver<u64>,
    after: i64,
    /// The last batch was full: read again before waiting.
    more: bool,
}

pub(super) async fn order_events(
    State(db): State<Database>,
    headers: HeaderMap,
    Query(query): Query<AfterQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let requested = position(&headers, &query)?;
    // Subscribed before the first read, so nothing committed in between is
    // missed.
    let mut appended = db.subscribe_order_events();
    appended.mark_unchanged();
    let span = db.read(crate::store::Store::order_event_span).await?;
    let (lost, after) = match span.resume(requested) {
        ResumePoint::Complete => (None, requested),
        ResumePoint::Lost { resume_after } => {
            tracing::warn!(
                order_events.requested_after = requested,
                order_events.resume_after = resume_after,
                "an order-event reader asked from events the log no longer holds"
            );
            let data = serde_json::json!({
                "requested_after": requested,
                "resume_after": resume_after,
            });
            let event = Event::default()
                .event("events_lost")
                .id(resume_after.to_string())
                .data(data.to_string());
            (Some(event), resume_after)
        }
    };

    let reader = Reader {
        db,
        appended,
        after,
        more: true,
    };
    let events = futures_util::stream::unfold(reader, async |mut reader| {
        loop {
            if !reader.more {
                tokio::select! {
                    changed = reader.appended.changed() => {
                        if changed.is_err() {
                            return None;
                        }
                    }
                    () = tokio::time::sleep(POLL_EVERY) => {}
                }
            }
            reader.appended.mark_unchanged();
            let after = reader.after;
            let batch = match reader
                .db
                .read(move |s| s.order_events_after(after, BATCH))
                .await
            {
                Ok(batch) => batch,
                Err(error) => {
                    // Ends the stream: the reader reconnects from its
                    // position.
                    tracing::warn!(error = %error, "could not read the order-event log");
                    return None;
                }
            };
            reader.more = batch.len() == BATCH;
            let Some(newest) = batch.last() else {
                continue;
            };
            reader.after = newest.seq;
            let events: Vec<Result<Event, Infallible>> = batch
                .iter()
                .map(|event| {
                    Ok(Event::default()
                        .event("order_event")
                        .id(event.seq.to_string())
                        .data(event.wire_json()))
                })
                .collect();
            return Some((futures_util::stream::iter(events), reader));
        }
    });
    let events = futures_util::StreamExt::flatten(events);
    let lost = futures_util::stream::iter(lost.map(Ok));
    Ok(Sse::new(futures_util::StreamExt::chain(lost, events))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
