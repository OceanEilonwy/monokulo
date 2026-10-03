//! The engine page's link to the engine: one poller per network while
//! anyone watches, feeding that network's [`History`] and sending every
//! viewer the same frames. Ten open pages cost the engine one request every
//! [`POLL`], however many viewers there are.
//!
//! A network's poller starts with its first viewer, reading the engine's
//! whole record (the last 30 minutes), and stops [`LINGER`] after its last
//! viewer leaves (so reloading the page doesn't start the history over),
//! letting its history go.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::{broadcast, RwLock};
use tokio::time::Instant;

use super::history::{Frame, History, Update};
use crate::engine_client::{EngineClient, EngineClientError};

/// How often a watched network's record is asked for.
pub const POLL: Duration = Duration::from_millis(500);
/// How long a network is still followed after its last viewer left.
pub const LINGER: Duration = Duration::from_secs(60);
/// Messages a slow viewer may fall behind by before it skips ahead.
const BACKLOG: usize = 64;

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
    poll: Duration,
    linger: Duration,
}

struct Channel {
    network: String,
    history: RwLock<Option<History>>,
    messages: broadcast::Sender<Message>,
    viewers: AtomicUsize,
    /// When the last viewer left, while there are none.
    idle_since: Mutex<Option<Instant>>,
    polling: Mutex<bool>,
}

/// A viewer's hold on a network: frames arrive on `messages`. Dropping it
/// lets the network's poller stop once nobody else watches.
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

impl Relay {
    pub fn new(client: EngineClient) -> Self {
        Self::with_timing(client, POLL, LINGER)
    }

    /// [`Self::new`] polling every `poll`, lingering `linger`: for tests,
    /// which can't wait a minute for a poller to stop.
    pub fn with_timing(client: EngineClient, poll: Duration, linger: Duration) -> Self {
        Self {
            client,
            networks: Mutex::default(),
            poll,
            linger,
        }
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
                        polling: Mutex::new(false),
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
        self.ensure_history(&channel).await?;
        self.ensure_polling(&channel);
        Ok(watch)
    }

    /// Reads `network`'s history with `read`, reading it from the engine
    /// first if nobody is watching. A read keeps the network followed for
    /// [`LINGER`] after it, as a viewer leaving does: the page without
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
            self.ensure_history(&channel).await?;
            self.ensure_polling(&channel);
            // The poller may have let the history go in between (its last
            // reader's linger ran out): read it again.
            if let Some(history) = channel.history.read().await.as_ref() {
                if let Some(read) = read.take() {
                    return Ok(read(history));
                }
            }
        }
    }

    async fn ensure_history(&self, channel: &Channel) -> Result<(), EngineClientError> {
        if channel.history.read().await.is_some() {
            return Ok(());
        }
        let mut history = channel.history.write().await;
        if history.is_none() {
            let page = self.client.engine_activity(&channel.network, None).await?;
            *history = Some(History::new(page));
        }
        Ok(())
    }

    fn ensure_polling(self: &Arc<Self>, channel: &Arc<Channel>) {
        let mut polling = channel.polling.lock();
        if *polling {
            return;
        }
        *polling = true;
        let (relay, channel) = (Arc::clone(self), Arc::clone(channel));
        tokio::spawn(async move { relay.poll(channel).await });
    }

    /// Follows `channel`'s network until nobody has watched for the linger.
    async fn poll(&self, channel: Arc<Channel>) {
        loop {
            tokio::time::sleep(self.poll).await;
            let idle = channel
                .idle_since
                .lock()
                .is_some_and(|since| since.elapsed() >= self.linger);
            let stop = idle && {
                let mut polling = channel.polling.lock();
                // A viewer that arrived meanwhile keeps it going.
                let nobody = channel.viewers.load(Ordering::SeqCst) == 0;
                if nobody {
                    *polling = false;
                }
                nobody
            };
            if stop {
                *channel.history.write().await = None;
                return;
            }
            let from = channel.history.read().await.as_ref().map(History::next);
            let message = match self.client.engine_activity(&channel.network, from).await {
                Ok(page) => {
                    let mut history = channel.history.write().await;
                    match history.as_mut() {
                        Some(history) => match history.update(page) {
                            Update::Frame(frame) => serde_json::to_string(&frame)
                                .ok()
                                .map(|json| Message::Frame(Arc::from(frame), Arc::from(json))),
                            Update::Restarted => Some(Message::Restarted),
                            Update::Nothing => None,
                        },
                        None => {
                            *history = Some(History::new(page));
                            Some(Message::Restarted)
                        }
                    }
                }
                Err(error) => {
                    shared::throttled!(
                        format!("engine-page-poll:{}", channel.network),
                        warn,
                        network = %channel.network,
                        error = %error,
                        "the engine page couldn't read the engine's activity (retried)"
                    );
                    Some(Message::Unreachable(error.to_string()))
                }
            };
            if let Some(message) = message {
                // Nobody listening is fine: the history keeps it.
                let _ = channel.messages.send(message);
            }
        }
    }

    /// Whether `network` is being followed now: for tests.
    #[cfg(test)]
    fn polling(&self, network: &str) -> bool {
        *self.channel(network).polling.lock()
    }
}

#[cfg(test)]
mod tests {
    use monero::Network;
    use shared::activity::Event;

    use super::*;

    fn relay(engine: &engine_test_support::TestEngineHandle, linger: Duration) -> Arc<Relay> {
        Arc::new(Relay::with_timing(
            EngineClient::embedded_for_tests(engine.router()),
            Duration::from_millis(20),
            linger,
        ))
    }

    async fn next_frame(watch: &mut Watch) -> Arc<Frame> {
        loop {
            match tokio::time::timeout(Duration::from_secs(10), watch.messages.recv())
                .await
                .expect("a message within 10 s")
                .expect("the channel is open")
            {
                Message::Frame(frame, _) => return frame,
                Message::Restarted | Message::Unreachable(_) => {}
            }
        }
    }

    /// A viewer gets the network's history from the real engine, then what
    /// the engine records next as frames; a second viewer shares the same
    /// history.
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
        drop((watch, other));
    }

    /// Reading without watching (the page without JavaScript, scrubbing)
    /// keeps the network followed: a later read sees what the engine
    /// recorded since. Once nobody has read it for the linger, it is let go.
    #[tokio::test]
    async fn reads_alone_keep_the_history_current_then_let_it_go() {
        let engine = engine_test_support::spawn_test_engine().await;
        let activity = engine.activity(Network::Stagenet);
        activity.record(Event::Snapshot(Box::default()));
        let relay = relay(&engine, Duration::from_millis(300));
        let reorg = |history: &History| history.live().reorg.is_some();
        assert!(!relay.read("stagenet", reorg).await.unwrap());
        assert!(relay.polling("stagenet"), "a read starts following");

        activity.record(Event::ReorgFound { fork: 100 });
        let deadline = Instant::now() + Duration::from_secs(10);
        while !relay.read("stagenet", reorg).await.unwrap() {
            assert!(Instant::now() < deadline, "the read never caught up");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        while relay.polling("stagenet") {
            assert!(Instant::now() < deadline, "still polling");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // The poller lets the history go just after it stops.
        while relay.channel("stagenet").history.read().await.is_some() {
            assert!(Instant::now() < deadline, "the history was kept");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A network nobody watches is let go of after the linger; watching it
    /// again starts over from the engine. An engine that isn't there is an
    /// error for the viewer, not a panic.
    #[tokio::test]
    async fn a_network_nobody_watches_is_let_go_of() {
        let engine = engine_test_support::spawn_test_engine().await;
        engine
            .activity(Network::Stagenet)
            .record(Event::Snapshot(Box::default()));
        let relay = relay(&engine, Duration::from_millis(50));
        let watch = relay.watch("stagenet").await.unwrap();
        assert!(relay.polling("stagenet"));
        drop(watch);
        let deadline = Instant::now() + Duration::from_secs(10);
        while relay.polling("stagenet") {
            assert!(Instant::now() < deadline, "still polling");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // The poller lets the history go just after it stops.
        while relay.channel("stagenet").history.read().await.is_some() {
            assert!(Instant::now() < deadline, "the history was kept");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let again = relay.watch("stagenet").await.unwrap();
        assert!(relay.polling("stagenet"));
        drop(again);

        let unconfigured = relay.watch("mainnet").await;
        assert!(unconfigured.is_err(), "the engine scans no mainnet");
        let nowhere = Arc::new(Relay::new(EngineClient::for_tests("http://127.0.0.1:9")));
        assert!(nowhere.watch("stagenet").await.is_err());
    }
}
