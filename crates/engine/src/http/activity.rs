//! A network's activity record, streamed to monokulo's engine page.
//! The engine token only, like the logs API.
//!
//! `GET /api/v1/admin/engine/activity/stream?network=stagenet&from=812`:
//! server-sent events, each an `activity` whose data is one
//! `engine_protocol::activity::ActivityPage` (JSON) and whose `id:` is the sequence
//! number of the last event in it (none when it has no events).
//!
//! - The first message comes at once: the events from sequence number
//!   `from` on, or, without `from` (or when it has left the record, or
//!   belongs to an engine since restarted), everything from the oldest
//!   snapshot kept, with `gap` set when `from` was given but couldn't be
//!   followed, so the reader starts over. `Last-Event-ID: n` (as an
//!   `EventSource` reconnecting sends) means `from=n+1`.
//! - Then each event as the scan loop records it, woken by the record
//!   ([`Activity::subscribe`]): no polling. Events recorded within
//!   [`StreamTuning::coalesce`] of the first go in one message, so a burst
//!   of hundreds is one message, not hundreds.
//! - A keep-alive comment every [`StreamTuning::keepalive`].
//! - A reader that falls [`StreamTuning::buffer`] messages behind is cut
//!   off: it reconnects with its last id and catches up from the record.
//!   Nothing is buffered without bound.
//!
//! Only memory is read: the snapshots in the record are taken by the scan
//! loop, so a stream costs no database work. A stream counts as someone
//! watching while it is open ([`Activity::watching`]).

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use engine_protocol::activity::{ActivityPage, Tier, Tuning};
use futures_util::Stream;
use serde::Deserialize;
use tokio::sync::mpsc;

use super::{ApiError, AppState};
use crate::activity::{Activity, Page, StreamTuning};
use crate::engine_settings::EngineSettings;
use crate::work::{millis, ScanTuning};

#[derive(Debug, Deserialize)]
pub(super) struct StreamQuery {
    network: String,
    from: Option<u64>,
}

/// Where the reader is: just after `Last-Event-ID`, else `?from=`.
fn position(headers: &HeaderMap, query: &StreamQuery) -> Result<Option<u64>, ApiError> {
    match headers.get("last-event-id") {
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|text| text.trim().parse::<u64>().ok())
            .map(|seq| Some(seq.saturating_add(1)))
            .ok_or_else(|| ApiError::BadRequest("Last-Event-ID must be an event's id".into())),
        None => Ok(query.from),
    }
}

pub(super) async fn stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    let tuning = state.activity_stream;
    tuning
        .validate()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let network = engine_protocol::network::parse_network(&query.network)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let from = position(&headers, &query)?;
    let activity = state
        .networks
        .scanner_status
        .read()
        .get(&network)
        .map(|status| Arc::clone(&status.activity))
        .ok_or(ApiError::NotFound)?;

    // Subscribed before the first read, so nothing recorded in between is
    // missed.
    let mut appended = activity.subscribe();
    appended.mark_unchanged();
    let page = activity.page(from);
    let next = page.next;
    let first = message(&state.settings, network, &activity, page)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let (messages, received) = mpsc::channel(tuning.buffer);
    // The buffer is empty and holds at least one (`validate`).
    let _ = messages.try_send(first);
    let follower = Follower {
        _watching: activity.watching(),
        activity,
        appended,
        next,
        messages,
        tuning,
        settings: Arc::clone(&state.settings),
        network,
    };
    tokio::spawn(follower.run());

    let received = futures_util::stream::unfold(received, async |mut received| {
        received.recv().await.map(|message| (Ok(message), received))
    });
    Ok(Sse::new(received).keep_alive(KeepAlive::new().interval(tuning.keepalive)))
}

/// One stream's work: sends what the record gains, until the reader goes
/// or falls too far behind.
struct Follower {
    activity: Arc<Activity>,
    /// Keeps the network watched while the stream is open.
    _watching: crate::activity::Watching,
    appended: tokio::sync::watch::Receiver<u64>,
    /// The sequence number to send from next.
    next: u64,
    messages: mpsc::Sender<SseEvent>,
    tuning: StreamTuning,
    settings: Arc<EngineSettings>,
    network: monero::Network,
}

impl Follower {
    async fn run(mut self) {
        loop {
            tokio::select! {
                changed = self.appended.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                () = self.messages.closed() => return,
            }
            // Let the burst gather: everything recorded meanwhile goes in
            // this one message.
            tokio::select! {
                () = tokio::time::sleep(self.tuning.coalesce) => {}
                () = self.messages.closed() => return,
            }
            self.appended.mark_unchanged();
            let page = self.activity.page(Some(self.next));
            if page.events.is_empty() && !page.gap {
                continue;
            }
            self.next = page.next;
            let Ok(message) = message(&self.settings, self.network, &self.activity, page) else {
                return;
            };
            // Full: the reader isn't keeping up. Ending here ends the
            // stream once it has read what is buffered; it reconnects from
            // the last id it got and loses nothing. Closed: it went.
            if self.messages.try_send(message).is_err() {
                return;
            }
        }
    }
}

/// `page` of `network`'s record as one `activity` message.
fn message(
    settings: &EngineSettings,
    network: monero::Network,
    activity: &Activity,
    page: Page,
) -> Result<SseEvent, axum::Error> {
    let scan = settings.scan.load();
    // The scanner runs the default tuning (`work::tuning`); only tests and
    // the round length sweep run others.
    let tuning = ScanTuning::DEFAULT;
    let last = page.events.last().map(|e| e.seq);
    let page = ActivityPage {
        network: engine_protocol::network::network_str(network).to_owned(),
        epoch: activity.epoch().to_owned(),
        now_ms: crate::activity::now_ms(),
        tuning: Tuning {
            round_ms: millis(tuning.round_budget),
            shares: Tier::ALL.map(|tier| tuning.shares.percent(tier)),
            group_page: u64::try_from(tuning.group_page).unwrap_or(u64::MAX),
            blocks_per_unit: u64::try_from(tuning.blocks_per_unit).unwrap_or(u64::MAX),
            reorg_check_depth: scan.reorg_check_depth,
            poll_ms: millis(scan.poll_interval),
        },
        events: page.events,
        next: page.next,
        gap: page.gap,
    };
    let event = SseEvent::default().event("activity").json_data(&page)?;
    Ok(match last {
        Some(seq) => event.id(seq.to_string()),
        None => event,
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use engine_protocol::activity::Event;
    use futures_util::StreamExt as _;
    use tower::ServiceExt as _;

    use super::super::{build_router, TEST_ENGINE_TOKEN};
    use super::*;

    /// What a stream sends: a message, or a keep-alive comment.
    #[derive(Debug)]
    enum Sent {
        Page { id: Option<u64>, page: ActivityPage },
        Comment,
    }

    /// The test end of one stream, read as monokulo reads it.
    struct Reader {
        body: axum::body::BodyDataStream,
        text: String,
    }

    impl Reader {
        /// What comes next, or `None` once the stream has ended.
        async fn next(&mut self) -> Option<Sent> {
            loop {
                if let Some(end) = self.text.find("\n\n") {
                    let block: String = self.text.drain(..end + 2).collect();
                    let lines: Vec<&str> = block.lines().filter(|l| !l.is_empty()).collect();
                    if lines.iter().all(|line| line.starts_with(':')) {
                        return Some(Sent::Comment);
                    }
                    let field = |name: &str| {
                        lines
                            .iter()
                            .find_map(|line| line.strip_prefix(name))
                            .map(str::trim)
                    };
                    assert_eq!(field("event:"), Some("activity"), "{block}");
                    return Some(Sent::Page {
                        id: field("id:").map(|id| id.parse().unwrap()),
                        page: serde_json::from_str(field("data:").unwrap()).unwrap(),
                    });
                }
                let chunk = tokio::time::timeout(Duration::from_secs(10), self.body.next())
                    .await
                    .expect("the stream sent something within 10 s")?
                    .unwrap();
                self.text.push_str(std::str::from_utf8(&chunk).unwrap());
            }
        }

        /// The next message, skipping keep-alives.
        async fn page(&mut self) -> (Option<u64>, ActivityPage) {
            loop {
                match self.next().await.expect("the stream is open") {
                    Sent::Page { id, page } => return (id, page),
                    Sent::Comment => {}
                }
            }
        }
    }

    /// Fast enough for a test to see coalescing and keep-alives, set
    /// explicitly; the engine itself runs [`StreamTuning::DEFAULT`].
    const TEST_TUNING: StreamTuning = StreamTuning {
        coalesce: Duration::from_millis(50),
        keepalive: Duration::from_secs(5),
        buffer: 16,
    };

    fn state(tuning: StreamTuning) -> (AppState, Arc<Activity>) {
        let state = AppState {
            activity_stream: tuning,
            ..AppState::for_tests()
        };
        let activity = crate::scanner_status::activity_of(
            &state.networks.scanner_status,
            monero::Network::Stagenet,
        );
        (state, activity)
    }

    async fn open(state: &AppState, uri: &str, last_event_id: Option<u64>) -> (StatusCode, Reader) {
        let mut request = Request::builder()
            .uri(uri)
            .header(credentials::auth::ENGINE_TOKEN_HEADER, TEST_ENGINE_TOKEN);
        if let Some(id) = last_event_id {
            request = request.header("last-event-id", id.to_string());
        }
        let response = build_router(state.clone(), 1_000_000)
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        (
            response.status(),
            Reader {
                body: response.into_body().into_data_stream(),
                text: String::new(),
            },
        )
    }

    fn seqs(page: &ActivityPage) -> Vec<u64> {
        page.events.iter().map(|e| e.seq).collect()
    }

    fn checked() -> Event {
        Event::ChainChecked {
            agrees: true,
            looked_up: true,
        }
    }

    /// A reader asking from `from` gets the record from there, in order,
    /// then each event the scan loop records after, as it records it; the
    /// scanner's tuning comes with every message.
    #[tokio::test]
    async fn the_record_is_replayed_from_from_then_new_events_are_pushed() {
        let (state, activity) = state(TEST_TUNING);
        activity.record(Event::Snapshot(Box::default()));
        for _ in 0..3 {
            activity.record(checked());
        }
        let (status, mut reader) = open(
            &state,
            "/api/v1/admin/engine/activity/stream?network=stagenet&from=1",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (id, page) = reader.page().await;
        assert_eq!(seqs(&page), [1, 2, 3]);
        assert_eq!((id, page.next, page.gap), (Some(3), 4, false));
        assert_eq!(page.network, "stagenet");
        assert_eq!(page.epoch, activity.epoch());
        assert_eq!(page.tuning.round_ms, 10_000);
        assert_eq!(page.tuning.shares, [20, 40, 15, 20, 5]);
        assert_eq!(activity.streams(), 1, "an open stream is watching");

        activity.record(Event::ReorgCollected);
        activity.record(Event::Vanished { looked: 2 });
        let (id, page) = reader.page().await;
        assert_eq!(seqs(&page), [4, 5]);
        assert_eq!(page.events[0].event, Event::ReorgCollected);
        assert_eq!((id, page.next, page.gap), (Some(5), 6, false));

        drop(reader);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while activity.streams() > 0 {
            assert!(tokio::time::Instant::now() < deadline, "still watching");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Without `from`, the record from its oldest snapshot; with a `from`
    /// that has left the record (or another engine's numbering), the same,
    /// flagged so the reader starts over.
    #[tokio::test]
    async fn a_from_that_left_the_record_starts_over_from_the_oldest_snapshot() {
        let (state, activity) = state(TEST_TUNING);
        activity.record(checked());
        activity.record(Event::Snapshot(Box::default()));
        activity.record(Event::Vanished { looked: 3 });
        let uri = "/api/v1/admin/engine/activity/stream?network=stagenet";
        let (_, mut fresh) = open(&state, uri, None).await;
        let (id, page) = fresh.page().await;
        assert_eq!((seqs(&page), id, page.gap), (vec![1, 2], Some(2), false));

        let (_, mut behind) = open(&state, &format!("{uri}&from=99"), None).await;
        let (_, page) = behind.page().await;
        assert_eq!((seqs(&page), page.gap), (vec![1, 2], true));

        // An `EventSource` reconnecting says where it got to by id.
        let (_, mut resumed) = open(&state, uri, Some(1)).await;
        let (_, page) = resumed.page().await;
        assert_eq!((seqs(&page), page.gap), (vec![2], false));
    }

    /// Hundreds of events recorded at once arrive in a few messages, not
    /// one each.
    #[tokio::test]
    async fn a_burst_arrives_in_a_few_messages() {
        let (state, activity) = state(TEST_TUNING);
        activity.record(Event::Snapshot(Box::default()));
        let (_, mut reader) = open(
            &state,
            "/api/v1/admin/engine/activity/stream?network=stagenet",
            None,
        )
        .await;
        let (_, first) = reader.page().await;
        assert_eq!(seqs(&first), [0]);
        for _ in 0..500 {
            activity.record(checked());
        }
        let mut seen = Vec::new();
        let mut messages = 0;
        while seen.len() < 500 {
            let (_, page) = reader.page().await;
            seen.extend(seqs(&page));
            messages += 1;
        }
        assert_eq!(seen, (1..=500).collect::<Vec<_>>());
        assert!(messages <= 3, "{messages} messages for one burst");
    }

    /// A reader that stops reading is cut off once its buffer is full,
    /// after what was buffered; reconnecting with the last id it got, it
    /// carries on from there without losing an event.
    #[tokio::test]
    async fn a_slow_reader_is_cut_off_then_resumes_without_loss() {
        let (state, activity) = state(StreamTuning {
            coalesce: Duration::from_millis(20),
            buffer: 2,
            ..TEST_TUNING
        });
        activity.record(Event::Snapshot(Box::default()));
        let uri = "/api/v1/admin/engine/activity/stream?network=stagenet";
        let (_, mut reader) = open(&state, uri, None).await;
        reader.page().await;
        // Five bursts, each its own message, none read meanwhile.
        for _ in 0..5 {
            activity.record(checked());
            activity.record(checked());
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let mut seen = vec![0];
        let mut last = None;
        while let Some(sent) = reader.next().await {
            if let Sent::Page { id, page } = sent {
                seen.extend(seqs(&page));
                last = id;
            }
        }
        assert_eq!(
            seen.len(),
            1 + 2 * 2,
            "the buffer's two messages, then the end"
        );

        let (_, mut again) = open(&state, uri, last).await;
        let (_, page) = again.page().await;
        assert!(!page.gap);
        seen.extend(seqs(&page));
        assert_eq!(seen, (0..=10).collect::<Vec<_>>());
    }

    /// A quiet stream sends a keep-alive comment.
    #[tokio::test]
    async fn a_quiet_stream_sends_keep_alives() {
        let (state, activity) = state(StreamTuning {
            keepalive: Duration::from_millis(100),
            ..TEST_TUNING
        });
        activity.record(Event::Snapshot(Box::default()));
        let (_, mut reader) = open(
            &state,
            "/api/v1/admin/engine/activity/stream?network=stagenet",
            None,
        )
        .await;
        reader.page().await;
        assert!(matches!(reader.next().await, Some(Sent::Comment)));
    }

    /// A network nobody scans has no record, a name that isn't a network
    /// and an id that isn't one are refused, and nothing is answered
    /// without the engine token.
    #[tokio::test]
    async fn a_stream_needs_the_engine_token_and_a_scanned_network() {
        let (state, _) = state(TEST_TUNING);
        let base = "/api/v1/admin/engine/activity/stream";
        let (status, _) = open(&state, &format!("{base}?network=mainnet"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = open(&state, &format!("{base}?network=moon"), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let not_an_id = build_router(state.clone(), 1_000_000)
            .oneshot(
                Request::builder()
                    .uri(format!("{base}?network=stagenet"))
                    .header(credentials::auth::ENGINE_TOKEN_HEADER, TEST_ENGINE_TOKEN)
                    .header("last-event-id", "soon")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(not_an_id.status(), StatusCode::BAD_REQUEST);
        let unauthenticated = build_router(state, 1_000_000)
            .oneshot(
                Request::builder()
                    .uri(format!("{base}?network=stagenet"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    }
}
