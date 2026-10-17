//! The engine page's link to the engine: one activity stream per network
//! while anyone watches (`GET /api/v1/admin/engine/activity/stream`,
//! [`EngineClient::open_engine_activity`]), feeding that network's
//! [`History`] and sending every viewer the same frames. Ten open pages
//! cost the engine one open stream, however many viewers there are, and no
//! requests while it stays open: the engine pushes what it records, a few
//! messages a second at most.
//!
//! A network's stream opens with its first viewer (or read), its first
//! message the engine's whole record (the last 30 minutes), and closes
//! [`LINGER`] after its last viewer leaves (so reloading the page doesn't
//! start the history over), letting its history go.
//!
//! When the stream ends or breaks off, it is opened again from the history's
//! next sequence number, after a wait that doubles while the engine can't be
//! reached ([`Timing`]); viewers are told [`Message::Unreachable`] each time
//! it can't. An engine that restarted meanwhile (or a history that fell
//! behind its record) starts the history over, and viewers are told
//! [`Message::Restarted`].

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use parking_lot::Mutex;
use shared::activity::ActivityPage;
use tokio::sync::{broadcast, RwLock};
use tokio::time::Instant;

use super::history::{Frame, History, Update};
use crate::engine_client::{EngineClient, EngineClientError, EventStream};
use crate::live::{SseEvent, SseParser};

/// How long a network is still followed after its last viewer left.
pub const LINGER: Duration = Duration::from_secs(60);
/// Messages a slow viewer may fall behind by before it skips ahead.
const BACKLOG: usize = 64;

/// How the relay follows the engine. Monokulo runs [`Timing::DEFAULT`];
/// tests pass their own values explicitly ([`Relay::with_timing`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// How long a network is still followed after its last viewer (or
    /// read) left: [`LINGER`].
    pub linger: Duration,
    /// The wait before opening a stream again after it ended or couldn't be
    /// opened: 500 ms, so a stream the engine closed (a reader too slow for
    /// it) is back at once...
    pub backoff: Duration,
    /// ...doubling while it can't be, up to 10 s, so an engine that is down
    /// is asked a few times a minute, and the page recovers within seconds
    /// of it coming back.
    pub max_backoff: Duration,
    /// With nothing from the engine for this long, not even its keep-alive
    /// (every 15 s), the stream is taken for dead and opened again: 45 s,
    /// three keep-alives missed.
    pub quiet: Duration,
}

/// Why a [`Timing`] can't be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TimingError {
    #[error("the linger must be above zero")]
    Linger,
    #[error("the backoff must be above zero and at most its maximum")]
    Backoff,
    #[error("the quiet limit must be above zero")]
    Quiet,
}

impl Timing {
    pub const DEFAULT: Self = Self {
        linger: LINGER,
        backoff: Duration::from_millis(500),
        max_backoff: Duration::from_secs(10),
        quiet: Duration::from_secs(45),
    };

    pub const fn validate(&self) -> Result<(), TimingError> {
        if self.linger.is_zero() {
            Err(TimingError::Linger)
        } else if self.backoff.is_zero() || self.backoff.as_nanos() > self.max_backoff.as_nanos() {
            Err(TimingError::Backoff)
        } else if self.quiet.is_zero() {
            Err(TimingError::Quiet)
        } else {
            Ok(())
        }
    }

    /// How often a quiet stream checks whether it is still wanted.
    fn check_every(&self) -> Duration {
        self.linger.min(Duration::from_secs(1))
    }
}

const _: () = match Timing::DEFAULT.validate() {
    Ok(()) => (),
    Err(_) => panic!("the default relay timing is invalid"),
};

/// What viewers are sent.
#[derive(Clone, Debug)]
pub enum Message {
    /// New events: what the page draws now, and what led there; and the
    /// same as JSON, written once for every viewer.
    Frame(Arc<Frame>, Arc<str>),
    /// The engine restarted (or the history fell behind it): start over.
    Restarted,
    /// The engine didn't answer; the page keeps what it has.
    Unreachable(String),
}

pub struct Relay {
    client: EngineClient,
    networks: Mutex<HashMap<String, Arc<Channel>>>,
    timing: Timing,
}

/// One network. Its history is there exactly while a follower
/// ([`Relay::follow`]) holds a stream for it.
struct Channel {
    network: String,
    history: RwLock<Option<History>>,
    messages: broadcast::Sender<Message>,
    viewers: AtomicUsize,
    /// When the last viewer left, while there are none.
    idle_since: Mutex<Option<Instant>>,
}

/// A viewer's hold on a network: frames arrive on `messages`. Dropping it
/// lets the network's stream close once nobody else watches.
pub struct Watch {
    channel: Arc<Channel>,
    pub messages: broadcast::Receiver<Message>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        if self.channel.viewers.fetch_sub(1, Ordering::SeqCst) == 1 {
            *self.channel.idle_since.lock() = Some(Instant::now());
        }
    }
}

/// One open activity stream, read message by message.
struct Upstream {
    body: EventStream,
    parser: SseParser,
    parsed: VecDeque<SseEvent>,
    /// When the engine last sent anything, keep-alives included.
    heard: Instant,
}

impl Upstream {
    async fn open(
        client: &EngineClient,
        network: &str,
        from: Option<u64>,
    ) -> Result<Self, EngineClientError> {
        Ok(Self {
            body: client.open_engine_activity(network, from).await?,
            parser: SseParser::default(),
            parsed: VecDeque::new(),
            heard: Instant::now(),
        })
    }

    /// The next page, or `None` once the engine ended the stream. Safe to
    /// cancel: what was read stays for the next call.
    async fn next(&mut self, quiet: Duration) -> Result<Option<ActivityPage>, EngineClientError> {
        loop {
            while let Some(event) = self.parsed.pop_front() {
                if event.event == "activity" {
                    return serde_json::from_str(&event.data)
                        .map(Some)
                        .map_err(|e| EngineClientError::Unreadable(e.to_string()));
                }
            }
            match tokio::time::timeout_at(self.heard + quiet, self.body.next()).await {
                Err(_) => {
                    return Err(EngineClientError::Unreadable(format!(
                        "the activity stream sent nothing for {quiet:?}"
                    )))
                }
                Ok(None) => return Ok(None),
                Ok(Some(Err(error))) => return Err(error),
                Ok(Some(Ok(chunk))) => {
                    self.heard = Instant::now();
                    self.parsed.extend(self.parser.push(&chunk));
                }
            }
        }
    }
}

impl Relay {
    pub fn new(client: EngineClient) -> Self {
        Self {
            client,
            networks: Mutex::default(),
            timing: Timing::DEFAULT,
        }
    }

    /// [`Self::new`] following with `timing` instead of the default.
    pub fn with_timing(client: EngineClient, timing: Timing) -> Result<Self, TimingError> {
        timing.validate()?;
        Ok(Self {
            timing,
            ..Self::new(client)
        })
    }

    fn channel(&self, network: &str) -> Arc<Channel> {
        Arc::clone(
            self.networks
                .lock()
                .entry(network.to_owned())
                .or_insert_with(|| {
                    Arc::new(Channel {
                        network: network.to_owned(),
                        history: RwLock::new(None),
                        messages: broadcast::channel(BACKLOG).0,
                        viewers: AtomicUsize::new(0),
                        idle_since: Mutex::new(None),
                    })
                }),
        )
    }

    /// Starts watching `network`: its history read (from the engine, if
    /// nobody was watching), and what follows on the returned [`Watch`].
    pub async fn watch(self: &Arc<Self>, network: &str) -> Result<Watch, EngineClientError> {
        let channel = self.channel(network);
        channel.viewers.fetch_add(1, Ordering::SeqCst);
        *channel.idle_since.lock() = None;
        let watch = Watch {
            messages: channel.messages.subscribe(),
            channel: Arc::clone(&channel),
        };
        self.ensure_following(&channel).await?;
        Ok(watch)
    }

    /// Reads `network`'s history with `read`, reading it from the engine
    /// first if nobody is watching. A read keeps the network followed for
    /// the linger after it, as a viewer leaving does: the page without
    /// JavaScript, and scrubbing, ask without watching, and must see what
    /// happened since, not the history as it was first read.
    pub async fn read<T>(
        self: &Arc<Self>,
        network: &str,
        read: impl FnOnce(&History) -> T,
    ) -> Result<T, EngineClientError> {
        let channel = self.channel(network);
        let mut read = Some(read);
        loop {
            if channel.viewers.load(Ordering::SeqCst) == 0 {
                *channel.idle_since.lock() = Some(Instant::now());
            }
            self.ensure_following(&channel).await?;
            // The follower may have let the history go in between (its
            // last reader's linger ran out): read it again.
            if let Some(history) = channel.history.read().await.as_ref() {
                if let Some(read) = read.take() {
                    return Ok(read(history));
                }
            }
        }
    }

    /// Opens `channel`'s stream if nobody is following it: its first
    /// message is the history, and a follower takes the rest.
    async fn ensure_following(
        self: &Arc<Self>,
        channel: &Arc<Channel>,
    ) -> Result<(), EngineClientError> {
        if channel.history.read().await.is_some() {
            return Ok(());
        }
        let mut history = channel.history.write().await;
        if history.is_some() {
            return Ok(());
        }
        let mut upstream = Upstream::open(&self.client, &channel.network, None).await?;
        let page = upstream.next(self.timing.quiet).await?.ok_or_else(|| {
            EngineClientError::Unreadable(
                "the activity stream ended before its first message".into(),
            )
        })?;
        *history = Some(History::new(page));
        let (relay, channel) = (Arc::clone(self), Arc::clone(channel));
        tokio::spawn(async move { relay.follow(channel, upstream).await });
        Ok(())
    }

    /// Follows `channel`'s network on `upstream`, opening it again whenever
    /// it ends, until nobody has watched for the linger.
    async fn follow(&self, channel: Arc<Channel>, mut upstream: Upstream) {
        let mut backoff = self.timing.backoff;
        // One clock for the whole stream, not a sleep started afresh after
        // each message: on a busy network messages come more often than
        // the check, and the linger must still run out.
        let mut check = tokio::time::interval(self.timing.check_every());
        check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            loop {
                tokio::select! {
                    next = upstream.next(self.timing.quiet) => match next {
                        Ok(Some(page)) => {
                            backoff = self.timing.backoff;
                            self.take(&channel, page).await;
                        }
                        Ok(None) => break,
                        Err(error) => {
                            tracing::debug!(
                                network = %channel.network,
                                error = %error,
                                "the engine page's activity stream broke off (reopened)"
                            );
                            break;
                        }
                    },
                    _ = check.tick() => {
                        if self.let_go_if_idle(&channel).await {
                            return;
                        }
                    }
                }
            }
            upstream = loop {
                let until = Instant::now() + backoff;
                while Instant::now() < until {
                    tokio::time::sleep_until(until.min(Instant::now() + self.timing.check_every()))
                        .await;
                    if self.let_go_if_idle(&channel).await {
                        return;
                    }
                }
                let from = channel.history.read().await.as_ref().map(History::next);
                match Upstream::open(&self.client, &channel.network, from).await {
                    Ok(upstream) => break upstream,
                    Err(error) => {
                        shared::throttled!(
                            format!("engine-page-stream:{}", channel.network),
                            warn,
                            network = %channel.network,
                            error = %error,
                            "the engine page couldn't open the engine's activity stream (retried)"
                        );
                        // Nobody listening is fine: the history keeps
                        // what it has.
                        let _ = channel
                            .messages
                            .send(Message::Unreachable(error.to_string()));
                        backoff = (backoff * 2).min(self.timing.max_backoff);
                    }
                }
            };
        }
    }

    /// Takes a page from the stream into `channel`'s history and tells the
    /// viewers what it changed.
    async fn take(&self, channel: &Channel, page: ActivityPage) {
        let message = {
            let mut history = channel.history.write().await;
            let Some(history) = history.as_mut() else {
                return;
            };
            match history.update(page) {
                Update::Frame(frame) => serde_json::to_string(&frame)
                    .ok()
                    .map(|json| Message::Frame(Arc::from(frame), Arc::from(json))),
                Update::Restarted => Some(Message::Restarted),
                Update::Nothing => None,
            }
        };
        if let Some(message) = message {
            // Nobody listening is fine: the history keeps it.
            let _ = channel.messages.send(message);
        }
    }

    /// Lets `channel`'s history go if nobody has watched or read it for the
    /// linger: the follower then closes its stream.
    async fn let_go_if_idle(&self, channel: &Channel) -> bool {
        let idle = || {
            channel
                .idle_since
                .lock()
                .is_some_and(|since| since.elapsed() >= self.timing.linger)
        };
        if !idle() {
            return false;
        }
        let mut history = channel.history.write().await;
        // A viewer or read that arrived meanwhile keeps it going.
        if channel.viewers.load(Ordering::SeqCst) > 0 || !idle() {
            return false;
        }
        *history = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::response::{IntoResponse as _, Response};
    use monero::Network;
    use shared::activity::Event;
    use tower::ServiceExt as _;

    use super::*;

    /// Quick enough for tests, set explicitly; monokulo runs
    /// [`Timing::DEFAULT`].
    const TEST_TIMING: Timing = Timing {
        linger: LINGER,
        backoff: Duration::from_millis(20),
        max_backoff: Duration::from_millis(200),
        quiet: Duration::from_secs(45),
    };

    fn relay_over(router: axum::Router, timing: Timing) -> Arc<Relay> {
        Arc::new(Relay::with_timing(EngineClient::embedded_for_tests(router), timing).unwrap())
    }

    fn relay(engine: &engine_test_support::TestEngineHandle, linger: Duration) -> Arc<Relay> {
        relay_over(
            engine.router(),
            Timing {
                linger,
                ..TEST_TIMING
            },
        )
    }

    /// Whether `network` is being followed now (a stream open for it).
    async fn following(relay: &Relay, network: &str) -> bool {
        relay.channel(network).history.read().await.is_some()
    }

    async fn next_message(watch: &mut Watch) -> Message {
        tokio::time::timeout(Duration::from_secs(10), watch.messages.recv())
            .await
            .expect("a message within 10 s")
            .expect("the channel is open")
    }

    async fn next_frame(watch: &mut Watch) -> Arc<Frame> {
        loop {
            if let Message::Frame(frame, _) = next_message(watch).await {
                return frame;
            }
        }
    }

    async fn eventually(what: &str, mut done: impl AsyncFnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done().await {
            assert!(Instant::now() < deadline, "{what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// An engine under the relay that can be restarted, or taken down and
    /// brought back: a restart or an outage cuts off whatever the engine
    /// was still sending, as the process going away would, and an engine
    /// that is down answers nothing but 503.
    struct Switch {
        engine: Mutex<Option<axum::Router>>,
        changes: tokio::sync::watch::Sender<u64>,
    }

    impl Switch {
        fn new(engine: axum::Router) -> Arc<Self> {
            Arc::new(Self {
                engine: Mutex::new(Some(engine)),
                changes: tokio::sync::watch::Sender::new(0),
            })
        }

        fn set(&self, engine: Option<axum::Router>) {
            *self.engine.lock() = engine;
            self.changes.send_modify(|n| *n += 1);
        }

        fn router(self: &Arc<Self>) -> axum::Router {
            let switch = Arc::clone(self);
            axum::Router::new().fallback(move |request: axum::extract::Request| {
                let switch = Arc::clone(&switch);
                async move {
                    let Some(engine) = switch.engine.lock().clone() else {
                        return (
                            axum::http::StatusCode::SERVICE_UNAVAILABLE,
                            axum::Json(serde_json::json!({ "error": "the engine is down" })),
                        )
                            .into_response();
                    };
                    let mut changes = switch.changes.subscribe();
                    let (parts, body) = engine.oneshot(request).await.into_response().into_parts();
                    let cut = body.into_data_stream().take_until(async move {
                        let _ = changes.changed().await;
                    });
                    Response::from_parts(parts, Body::from_stream(cut))
                }
            })
        }
    }

    async fn engine_with_a_snapshot() -> engine_test_support::TestEngineHandle {
        let engine = engine_test_support::spawn_test_engine().await;
        engine
            .activity(Network::Stagenet)
            .record(Event::Snapshot(Box::default()));
        engine
    }

    /// A viewer gets the network's history from the real engine, then what
    /// the engine records next as frames, pushed on the one stream; a
    /// second viewer shares the same history.
    #[tokio::test]
    async fn viewers_get_the_engine_s_history_then_its_new_events() {
        let engine = engine_test_support::spawn_test_engine().await;
        let activity = engine.activity(Network::Stagenet);
        activity.record(Event::Snapshot(Box::new(shared::activity::Snapshot {
            high_water: Some(100),
            tip: Some(100),
            ..Default::default()
        })));
        activity.record(Event::Seeded { height: 100 });
        let relay = relay(&engine, LINGER);
        let mut watch = relay.watch("stagenet").await.unwrap();
        let marks = relay
            .read("stagenet", |history| {
                history.marks().cloned().collect::<Vec<_>>()
            })
            .await
            .unwrap();
        assert_eq!(marks.len(), 1);
        assert_eq!(
            crate::engine_view::present::mark_text(&marks[0].what),
            "Started scanning this network at block 100."
        );
        let requests = engine.request_count();

        activity.record(Event::ReorgFound { fork: 100 });
        let frame = next_frame(&mut watch).await;
        assert_eq!(frame.marks.len(), 1, "{frame:?}");
        assert!(frame.marks[0].key);
        assert_eq!(frame.view.summary[5].value, "Reorg");

        let other = relay.watch("stagenet").await.unwrap();
        let live = relay
            .read("stagenet", |history| history.live().reorg.is_some())
            .await
            .unwrap();
        assert!(live);
        assert_eq!(
            engine.request_count(),
            requests,
            "pushed on the stream already open, nothing asked for"
        );
        assert_eq!(activity.streams(), 1, "one stream for both viewers");
        drop((watch, other));
    }

    /// Watching a network for a minute costs the engine one request, the
    /// stream, not one every half second.
    #[tokio::test]
    async fn watching_for_a_minute_costs_one_request() {
        let engine = engine_with_a_snapshot().await;
        let activity = engine.activity(Network::Stagenet);
        let relay = relay(&engine, LINGER);
        let before = engine.request_count();
        let mut watch = relay.watch("stagenet").await.unwrap();
        tokio::time::pause();
        let start = Instant::now();
        for _ in 0..60 {
            activity.record(Event::ReorgCollected);
            next_frame(&mut watch).await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        assert!(start.elapsed() >= Duration::from_mins(1));
        assert_eq!(engine.request_count() - before, 1);
        assert_eq!(activity.streams(), 1);
    }

    /// Reading without watching (the page without JavaScript, scrubbing)
    /// keeps the network followed: a later read sees what the engine
    /// recorded since. Once nobody has read it for the linger, it is let go.
    #[tokio::test]
    async fn reads_alone_keep_the_history_current_then_let_it_go() {
        let engine = engine_with_a_snapshot().await;
        let activity = engine.activity(Network::Stagenet);
        let relay = relay(&engine, Duration::from_millis(300));
        let reorg = |history: &History| history.live().reorg.is_some();
        assert!(!relay.read("stagenet", reorg).await.unwrap());
        assert!(
            following(&relay, "stagenet").await,
            "a read starts following"
        );

        activity.record(Event::ReorgFound { fork: 100 });
        eventually("the read caught up", async || {
            relay.read("stagenet", reorg).await.unwrap()
        })
        .await;
        eventually("let go", async || !following(&relay, "stagenet").await).await;
        eventually("the stream closed", async || activity.streams() == 0).await;
    }

    /// Once its last viewer leaves, a network's stream stays open for the
    /// linger, then closes and its history goes, however busy the network
    /// is; watching it again starts over from the engine. An engine that
    /// isn't there, or doesn't scan the network, is an error for the
    /// viewer, not a panic.
    #[tokio::test]
    async fn the_stream_closes_a_linger_after_the_last_viewer_leaves() {
        let engine = engine_with_a_snapshot().await;
        let activity = engine.activity(Network::Stagenet);
        let linger = Duration::from_millis(300);
        let relay = relay(&engine, linger);
        let watch = relay.watch("stagenet").await.unwrap();
        assert_eq!(activity.streams(), 1);
        // A message more often than the relay checks whether it is wanted.
        let busy = tokio::spawn({
            let activity = Arc::clone(&activity);
            async move {
                loop {
                    activity.record(Event::ReorgCollected);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        });
        drop(watch);
        let left = Instant::now();
        eventually("the stream closed", async || activity.streams() == 0).await;
        assert!(left.elapsed() >= linger, "closed before the linger");
        assert!(!following(&relay, "stagenet").await);
        busy.abort();

        let again = relay.watch("stagenet").await.unwrap();
        assert_eq!(activity.streams(), 1);
        drop(again);

        let unconfigured = relay.watch("mainnet").await;
        assert!(unconfigured.is_err(), "the engine scans no mainnet");
        let nowhere = Arc::new(Relay::new(EngineClient::for_tests(format!(
            "http://{}",
            shared::unreachable::address()
        ))));
        assert!(nowhere.watch("stagenet").await.is_err());
    }

    /// The engine restarting under the relay: the stream is opened again,
    /// the new engine's record starts the history over, viewers are told,
    /// and what the new engine records reaches them.
    #[tokio::test]
    async fn an_engine_restart_starts_the_history_over() {
        let first = engine_with_a_snapshot().await;
        let switch = Switch::new(first.router());
        let relay = relay_over(switch.router(), TEST_TIMING);
        let mut watch = relay.watch("stagenet").await.unwrap();
        first
            .activity(Network::Stagenet)
            .record(Event::ReorgFound { fork: 100 });
        next_frame(&mut watch).await;

        let second = engine_with_a_snapshot().await;
        switch.set(Some(second.router()));
        loop {
            match next_message(&mut watch).await {
                Message::Restarted => break,
                Message::Frame(..) => {}
                Message::Unreachable(error) => panic!("the engine was up: {error}"),
            }
        }
        let epoch = relay
            .read("stagenet", |history| history.epoch().to_owned())
            .await
            .unwrap();
        assert_eq!(epoch, second.activity(Network::Stagenet).epoch());
        second
            .activity(Network::Stagenet)
            .record(Event::ReorgFound { fork: 200 });
        next_frame(&mut watch).await;
        let old = first.activity(Network::Stagenet);
        eventually("the old engine's stream closed", async || {
            old.streams() == 0
        })
        .await;
    }

    /// An engine that goes down: viewers are told it can't be reached, and
    /// once it is back the stream carries on from where the history was.
    #[tokio::test]
    async fn an_engine_that_is_down_is_unreachable_until_it_is_back() {
        let engine = engine_with_a_snapshot().await;
        let activity = engine.activity(Network::Stagenet);
        let switch = Switch::new(engine.router());
        let relay = relay_over(switch.router(), TEST_TIMING);
        let mut watch = relay.watch("stagenet").await.unwrap();

        switch.set(None);
        match next_message(&mut watch).await {
            Message::Unreachable(error) => {
                assert!(error.contains("the engine is down"), "{error}");
            }
            other => panic!("expected unreachable, got {other:?}"),
        }
        // Recorded while it was down: caught up from the record.
        activity.record(Event::ReorgFound { fork: 100 });
        switch.set(Some(engine.router()));
        let frame = loop {
            match next_message(&mut watch).await {
                Message::Frame(frame, _) => break frame,
                Message::Unreachable(_) => {}
                Message::Restarted => panic!("the same engine came back"),
            }
        };
        assert_eq!(frame.view.summary[5].value, "Reorg");
    }

    /// The default timing is valid; nothing that would never let go, retry
    /// at once forever, or never give up on a dead stream is.
    #[test]
    fn timing_is_validated() {
        let ok = Timing::DEFAULT;
        assert_eq!(ok.validate(), Ok(()));
        for (timing, error) in [
            (
                Timing {
                    linger: Duration::ZERO,
                    ..ok
                },
                TimingError::Linger,
            ),
            (
                Timing {
                    backoff: Duration::ZERO,
                    ..ok
                },
                TimingError::Backoff,
            ),
            (
                Timing {
                    max_backoff: Duration::from_millis(1),
                    ..ok
                },
                TimingError::Backoff,
            ),
            (
                Timing {
                    quiet: Duration::ZERO,
                    ..ok
                },
                TimingError::Quiet,
            ),
        ] {
            assert_eq!(timing.validate(), Err(error));
        }
    }
}
