//! Live order updates: the engine's per-tenant change stream
//! (`GET /api/v1/admin/tenant/events`) fanned out to every browser watching
//! one of that tenant's orders over Server-Sent Events.
//!
//! One upstream connection per store connection, opened lazily when the
//! first watcher of any of its orders arrives and closed when the last one
//! leaves - a store with a hundred open checkout pages still holds a single
//! connection to the engine. The engine only ever says "order X changed";
//! each watcher re-reads the order itself ([`live_events`]), so an event that
//! arrives twice or for nothing costs one read and a missed one is covered by
//! the resync that follows every reconnect.
//!
//! If the upstream stream can't be opened (engine down, or an engine that
//! predates the endpoint), every watcher of that store is woken on each retry
//! instead, which degrades to server-side polling at the retry interval rather
//! than to no updates at all.

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use tokio::sync::watch;

use crate::engine_client::EngineClient;

/// Longest wait between upstream reconnect attempts - and so the effective
/// polling interval while the engine's event stream is unavailable.
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(10);
/// The engine sends a keep-alive comment every 15s; this much silence means
/// the connection is dead even if TCP hasn't noticed yet.
const UPSTREAM_READ_TIMEOUT: Duration = Duration::from_secs(45);
/// Browser-side keep-alive, well inside common proxy idle timeouts.
const DOWNSTREAM_KEEP_ALIVE: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct LiveHub {
    stores: Mutex<HashMap<String, StoreWatch>>,
}

struct StoreWatch {
    orders: HashMap<String, OrderWatch>,
    upstream: tokio::task::JoinHandle<()>,
}

struct OrderWatch {
    changed: watch::Sender<u64>,
    watchers: usize,
}

/// A live handle on one order. Wakes on every change the engine reports for
/// it; dropping it (the browser disconnecting) releases the order, and the
/// store's upstream connection with the last one.
pub struct OrderSubscription {
    hub: Arc<LiveHub>,
    connection_id: String,
    order_id: String,
    changed: watch::Receiver<u64>,
}

impl LiveHub {
    /// `engine` is the client whose events stream is opened for this store if
    /// nobody is watching it yet; `sk` authenticates it.
    pub fn subscribe(self: &Arc<Self>, engine: &EngineClient, connection_id: &str, sk: &str, order_id: &str) -> OrderSubscription {
        let mut stores = self.stores.lock().unwrap();
        let store = stores.entry(connection_id.to_string()).or_insert_with(|| StoreWatch {
            orders: HashMap::new(),
            upstream: tokio::spawn(run_upstream(Arc::downgrade(self), engine.clone(), connection_id.to_string(), sk.to_string())),
        });
        let order = store
            .orders
            .entry(order_id.to_string())
            .or_insert_with(|| OrderWatch { changed: watch::channel(0).0, watchers: 0 });
        order.watchers += 1;
        OrderSubscription {
            hub: self.clone(),
            connection_id: connection_id.to_string(),
            order_id: order_id.to_string(),
            changed: order.changed.subscribe(),
        }
    }

    fn release(&self, connection_id: &str, order_id: &str) {
        let mut stores = self.stores.lock().unwrap();
        let Some(store) = stores.get_mut(connection_id) else { return };
        if let Some(order) = store.orders.get_mut(order_id) {
            order.watchers -= 1;
            if order.watchers == 0 {
                store.orders.remove(order_id);
            }
        }
        if store.orders.is_empty() {
            if let Some(store) = stores.remove(connection_id) {
                store.upstream.abort();
            }
        }
    }

    fn wake(&self, connection_id: &str, order_id: Option<&str>) {
        let stores = self.stores.lock().unwrap();
        let Some(store) = stores.get(connection_id) else { return };
        for (id, order) in &store.orders {
            if order_id.is_none_or(|wanted| wanted == id) {
                order.changed.send_modify(|n| *n = n.wrapping_add(1));
            }
        }
    }

    /// How many stores currently hold an upstream connection - for tests.
    pub fn upstream_count(&self) -> usize {
        self.stores.lock().unwrap().len()
    }
}

impl Drop for OrderSubscription {
    fn drop(&mut self) {
        self.hub.release(&self.connection_id, &self.order_id);
    }
}

/// Holds only a weak reference to the hub: the hub owns (and aborts) this
/// task, so a strong one would keep both alive forever.
async fn run_upstream(hub: std::sync::Weak<LiveHub>, engine: EngineClient, connection_id: String, sk: String) {
    let mut delay = Duration::from_secs(1);
    loop {
        if let Ok(mut response) = engine.open_order_events(&sk).await {
            let mut parser = SseParser::default();
            loop {
                let chunk = match tokio::time::timeout(UPSTREAM_READ_TIMEOUT, response.chunk()).await {
                    Ok(Ok(Some(chunk))) => chunk,
                    _ => break,
                };
                for (event, data) in parser.push(&chunk) {
                    let Some(hub) = hub.upgrade() else { return };
                    match event.as_str() {
                        // Connected: anything before this was missed. Only
                        // a connection that actually delivered resets the
                        // backoff.
                        "ready" | "resync" => {
                            delay = Duration::from_secs(1);
                            hub.wake(&connection_id, None);
                        }
                        "order" => {
                            let order_id = serde_json::from_str::<serde_json::Value>(&data)
                                .ok()
                                .and_then(|v| v.get("order_id").and_then(|id| id.as_str()).map(str::to_string));
                            if let Some(order_id) = order_id {
                                hub.wake(&connection_id, Some(&order_id));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        // Disconnected, or never connected: changes may have been missed in
        // the meantime, so every watcher re-reads now and again on each retry.
        let Some(strong) = hub.upgrade() else { return };
        strong.wake(&connection_id, None);
        drop(strong);
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAX_RECONNECT_DELAY);
    }
}

/// Just enough of the SSE wire format for the engine's own stream: `event:`
/// and `data:` fields, blank-line terminated, comments ignored.
#[derive(Default)]
struct SseParser {
    buffer: Vec<u8>,
    event: String,
    data: String,
}

impl SseParser {
    fn push(&mut self, chunk: &[u8]) -> Vec<(String, String)> {
        self.buffer.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !self.event.is_empty() || !self.data.is_empty() {
                    let event = if self.event.is_empty() { "message".to_string() } else { std::mem::take(&mut self.event) };
                    out.push((event, std::mem::take(&mut self.data)));
                }
            } else if let Some(value) = line.strip_prefix("event:") {
                self.event = value.trim_start().to_string();
            } else if let Some(value) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(value.strip_prefix(' ').unwrap_or(value));
            }
        }
        out
    }
}

/// What a watcher currently has to say about its order.
pub struct LiveSnapshot {
    pub events: Vec<Event>,
    /// Compared against the previous snapshot's; nothing is sent when equal.
    pub fingerprint: String,
    /// Sent once more, then the stream ends.
    pub terminal: bool,
}

/// An SSE response for one order: a snapshot immediately, then another each
/// time the order changes or `refresh_every` passes (for values that move
/// with the clock, like time until expiry), sent only when it differs from
/// the last one. `snapshot` returning `None` (the engine briefly
/// unreachable) sends nothing and waits for the next wake.
pub fn live_events<F, Fut>(
    subscription: OrderSubscription,
    refresh_every: Duration,
    snapshot: F,
) -> Response
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Option<LiveSnapshot>> + Send + 'static,
{
    sse(snapshot_stream(subscription, refresh_every, snapshot))
}

/// An SSE response over `stream`, with the keep-alive every live stream here uses.
pub fn sse<S>(stream: S) -> Response
where
    S: Stream<Item = Result<Event, Infallible>> + Send + 'static,
{
    Sse::new(stream).keep_alive(KeepAlive::new().interval(DOWNSTREAM_KEEP_ALIVE)).into_response()
}

/// The stream behind [`live_events`], for callers merging several orders
/// into one response.
pub fn snapshot_stream<F, Fut>(
    subscription: OrderSubscription,
    refresh_every: Duration,
    snapshot: F,
) -> impl Stream<Item = Result<Event, Infallible>> + Send
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Option<LiveSnapshot>> + Send + 'static,
{
    struct State<F> {
        subscription: OrderSubscription,
        snapshot: F,
        last: Option<String>,
        first: bool,
        done: bool,
    }
    let state = State { subscription, snapshot, last: None, first: true, done: false };
    let batches = futures_util::stream::unfold(state, move |mut state| async move {
        loop {
            if state.done {
                return None;
            }
            if !state.first {
                tokio::select! {
                    changed = state.subscription.changed.changed() => {
                        if changed.is_err() {
                            return None;
                        }
                    }
                    _ = tokio::time::sleep(refresh_every) => {}
                }
            }
            state.first = false;
            let Some(snapshot) = (state.snapshot)().await else { continue };
            state.done = snapshot.terminal;
            if state.last.as_deref() != Some(snapshot.fingerprint.as_str()) {
                state.last = Some(snapshot.fingerprint);
                return Some((snapshot.events, state));
            }
        }
    });
    futures_util::StreamExt::flat_map(batches, |events| futures_util::stream::iter(events.into_iter().map(Ok)))
}

/// Reads SSE frames from `body` until one full event arrives, returning its
/// `(event, data)` - `None` once the stream has ended.
#[cfg(test)]
pub(crate) async fn next_sse_event(body: &mut axum::body::Body, parser_buffer: &mut Vec<(String, String)>, parser: &mut SseTestParser) -> Option<(String, String)> {
    use http_body_util::BodyExt;
    loop {
        if !parser_buffer.is_empty() {
            return Some(parser_buffer.remove(0));
        }
        let frame = tokio::time::timeout(Duration::from_secs(10), body.frame()).await.expect("timed out waiting for an SSE event")?;
        if let Ok(bytes) = frame.unwrap().into_data() {
            parser_buffer.extend(parser.0.push(&bytes));
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct SseTestParser(SseParser);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_handles_split_chunks_comments_and_crlf() {
        let mut parser = SseParser::default();
        assert!(parser.push(b": keep-alive\n\nevent: ord").is_empty());
        let events = parser.push(b"er\r\ndata: {\"order_id\":\"o1\"}\r\n\r\nevent: ready\ndata: {}\n\n");
        assert_eq!(
            events,
            vec![("order".to_string(), "{\"order_id\":\"o1\"}".to_string()), ("ready".to_string(), "{}".to_string())]
        );
    }

    #[tokio::test]
    async fn last_subscription_dropped_closes_the_upstream() {
        let hub = Arc::new(LiveHub::default());
        // Nothing listens here; the upstream task just keeps retrying.
        let engine = EngineClient::new("http://127.0.0.1:9");
        let a = hub.subscribe(&engine, "conn", "sk_x", "o1");
        let b = hub.subscribe(&engine, "conn", "sk_x", "o2");
        assert_eq!(hub.upstream_count(), 1);
        drop(a);
        assert_eq!(hub.upstream_count(), 1);
        drop(b);
        assert_eq!(hub.upstream_count(), 0);
    }

    #[tokio::test]
    async fn wake_reaches_only_the_named_order() {
        let hub = Arc::new(LiveHub::default());
        let engine = EngineClient::new("http://127.0.0.1:9");
        let mut a = hub.subscribe(&engine, "conn", "sk_x", "o1");
        let mut b = hub.subscribe(&engine, "conn", "sk_x", "o2");
        a.changed.mark_unchanged();
        b.changed.mark_unchanged();
        hub.wake("conn", Some("o1"));
        assert!(a.changed.has_changed().unwrap());
        assert!(!b.changed.has_changed().unwrap());
    }
}
