//! Waking a network's scan loops as soon as its node has news, instead of at
//! the next poll (`docs/monero_zmq.md`).
//!
//! The loops still poll: an announcement only says "ask now". What is
//! scanned and recorded always comes from the node's RPC answers, on the
//! node the loop pins for its pass, exactly as when nothing announces
//! anything. So a lost, late, duplicated or forged announcement costs at
//! most a poll interval or one early request, never a wrong answer.
//!
//! With the `zmq` feature, [`run_subscriber`] listens to the `zmq_pub` of
//! every node configured for a network (monerod's `--zmq-pub`) and wakes
//! that network's loops through its [`NodeWakes`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use shared::announcements::Announcements;
use tokio::sync::Notify;

/// The shortest gap between two passes, however often the node announces:
/// a burst of pool transactions is taken in a few passes, not one each.
const MIN_GAP: Duration = Duration::from_millis(20);

/// What a network's node has announced since its loops last looked.
///
/// Each is a single stored wake-up: announcements made while a pass is
/// running end the wait that follows it at once, however many there were.
///
/// Also what `/status` reports about the announcements: each publisher
/// listened to, and how many waits they cut short.
#[derive(Debug, Default)]
pub struct NodeWakes {
    pool: Notify,
    chain: Notify,
    /// The chain too, for proof-of-work checking's own loop: one `Notify`
    /// wakes one waiter, and the scan loop waits on `chain`.
    proof: Notify,
    pool_passes_woken: AtomicU64,
    rounds_woken: AtomicU64,
    publishers: parking_lot::Mutex<Vec<shared::announcements::Publisher>>,
}

impl NodeWakes {
    /// A transaction entered the node's pool.
    pub fn pool_changed(&self) {
        self.pool.notify_one();
    }

    /// The node's chain has a new tip.
    pub fn chain_changed(&self) {
        self.chain.notify_one();
        self.proof.notify_one();
    }

    /// Waits `interval`, or until the pool changes if sooner (but at least
    /// [`MIN_GAP`]). `true` if it was the pool.
    pub async fn pool_or(&self, interval: Duration) -> bool {
        counted(&self.pool_passes_woken, wait(&self.pool, interval).await)
    }

    /// [`Self::pool_or`] for the chain.
    pub async fn chain_or(&self, interval: Duration) -> bool {
        counted(&self.rounds_woken, wait(&self.chain, interval).await)
    }

    /// [`Self::chain_or`] for proof-of-work checking (not counted).
    pub async fn proof_or(&self, interval: Duration) -> bool {
        wait(&self.proof, interval).await
    }

    /// For `/status`: `None` while no publisher is configured.
    pub fn announcements(&self) -> Option<Announcements> {
        let publishers = self.publishers.lock().clone();
        (!publishers.is_empty()).then(|| Announcements {
            publishers,
            pool_passes_woken: self.pool_passes_woken.load(Ordering::Relaxed),
            rounds_woken: self.rounds_woken.load(Ordering::Relaxed),
        })
    }

    /// The publishers now configured, each `(node, endpoint)`: one already
    /// listened to keeps its figures (marked not connected, as it is about
    /// to connect again); one no longer configured goes.
    #[cfg(feature = "zmq")]
    fn listen_to(&self, configured: &[(String, String)]) {
        let mut publishers = self.publishers.lock();
        let mut previous = std::mem::take(&mut *publishers);
        for (node, endpoint) in configured {
            let mut publisher = previous
                .iter()
                .position(|p| p.endpoint == *endpoint)
                .map_or_default(|at| previous.swap_remove(at));
            publisher.node.clone_from(node);
            publisher.endpoint.clone_from(endpoint);
            publisher.connected = false;
            publisher.connected_since = None;
            publishers.push(publisher);
        }
    }

    #[cfg(feature = "zmq")]
    fn publisher(
        &self,
        endpoint: &str,
        update: impl FnOnce(&mut shared::announcements::Publisher),
    ) {
        if let Some(publisher) = self
            .publishers
            .lock()
            .iter_mut()
            .find(|p| p.endpoint == endpoint)
        {
            update(publisher);
        }
    }
}

fn counted(counter: &AtomicU64, woke: bool) -> bool {
    if woke {
        counter.fetch_add(1, Ordering::Relaxed);
    }
    woke
}

async fn wait(notify: &Notify, interval: Duration) -> bool {
    let gap = MIN_GAP.min(interval);
    tokio::time::sleep(gap).await;
    tokio::select! {
        biased;
        () = notify.notified() => true,
        () = tokio::time::sleep(interval.saturating_sub(gap)) => false,
    }
}

#[cfg(feature = "zmq")]
pub use subscriber::run_subscriber;

#[cfg(feature = "zmq")]
mod subscriber {
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt as _;
    use zeromq::{Socket as _, SocketEvent, SocketRecv as _, SubSocket};

    use super::NodeWakes;
    use crate::engine_settings::EngineSettings;

    /// monerod's topics for a transaction entering the pool and for a new
    /// main-chain tip, in their small forms: only the topic is read.
    pub(super) const POOL_TOPIC: &str = "json-minimal-txpool_add";
    pub(super) const CHAIN_TOPIC: &str = "json-minimal-chain_main";

    const FIRST_RETRY: Duration = Duration::from_secs(1);
    const LAST_RETRY: Duration = Duration::from_secs(60);

    #[derive(Debug, PartialEq)]
    pub(super) enum Announcement {
        Pool,
        Chain,
    }

    /// What one message announces. monerod sends each as one frame,
    /// `<topic>:<json>`; the JSON is not read.
    pub(super) fn announcement(frame: &[u8]) -> Option<Announcement> {
        let end = frame.iter().position(|&b| b == b':')?;
        match &frame[..end] {
            topic if topic == POOL_TOPIC.as_bytes() => Some(Announcement::Pool),
            topic if topic == CHAIN_TOPIC.as_bytes() => Some(Announcement::Chain),
            _ => None,
        }
    }

    /// Every `zmq_pub` configured for `network`'s node and its fallbacks,
    /// with its node's `host:port`; one named twice is listened to once.
    fn publishers(settings: &EngineSettings, network: monero::Network) -> Vec<(String, String)> {
        let nodes = settings.nodes.load();
        let Some(node) = nodes.nodes.get(crate::network::network_str(network)) else {
            return Vec::new();
        };
        let mut publishers: Vec<(String, String)> = Vec::new();
        for node in std::iter::once(node).chain(&node.fallbacks) {
            if let Some(endpoint) = &node.zmq_pub {
                if !publishers.iter().any(|(_, e)| e == endpoint) {
                    publishers.push((format!("{}:{}", node.host, node.port), endpoint.clone()));
                }
            }
        }
        publishers
    }

    /// Listens to `network`'s publishers for as long as it runs, starting
    /// over with the new list whenever node settings are saved. Run under
    /// the network's stop signal, beside its loops.
    pub async fn run_subscriber(
        network: monero::Network,
        wakes: Arc<NodeWakes>,
        settings: Arc<EngineSettings>,
    ) {
        let mut saved = settings.nodes.subscribe();
        loop {
            let configured = publishers(&settings, network);
            wakes.listen_to(&configured);
            let listeners: Vec<_> = configured
                .into_iter()
                .map(|(_, endpoint)| listen(network, endpoint, Arc::clone(&wakes)))
                .collect();
            // Each listener runs until the settings change; with none, this
            // only waits for that.
            let listening = async {
                futures_util::future::join_all(listeners).await;
                std::future::pending::<()>().await;
            };
            tokio::select! {
                () = listening => {}
                changed = saved.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
    }

    /// Listens to one publisher, connecting again (after a growing pause)
    /// whenever it can't be reached or the connection fails. Never returns.
    #[expect(clippy::infinite_loop, reason = "listens for the life of the process")]
    pub(super) async fn listen(network: monero::Network, endpoint: String, wakes: Arc<NodeWakes>) {
        let mut retry = FIRST_RETRY;
        loop {
            let error = session(network, &endpoint, &wakes, &mut retry).await;
            wakes.publisher(&endpoint, |p| {
                p.connected = false;
                p.connected_since = None;
                p.last_error = Some(error.clone());
                p.last_error_at = Some(shared::time::now_unix());
            });
            shared::throttled!(
                format!("zmq-failed:{endpoint}"),
                warn,
                network = ?network,
                endpoint = %endpoint,
                error = %error,
                retry_in = ?retry,
                "lost the node's ZMQ announcements; polling alone until they're back"
            );
            tokio::time::sleep(retry).await;
            retry = (retry * 2).min(LAST_RETRY);
        }
    }

    async fn session(
        network: monero::Network,
        endpoint: &str,
        wakes: &NodeWakes,
        retry: &mut Duration,
    ) -> String {
        let mut socket = SubSocket::new();
        // The socket would reconnect by itself, silently: its events say
        // when the node went, so this session ends and `listen` reconnects
        // (and `/status` says it was lost meanwhile).
        let mut events = socket.monitor();
        for topic in [POOL_TOPIC, CHAIN_TOPIC] {
            if let Err(error) = socket.subscribe(topic).await {
                return error.to_string();
            }
        }
        if let Err(error) = socket.connect(endpoint).await {
            return error.to_string();
        }
        *retry = FIRST_RETRY;
        wakes.publisher(endpoint, |p| {
            p.connected = true;
            p.connected_since = Some(shared::time::now_unix());
            p.connections += 1;
        });
        tracing::info!(network = ?network, endpoint = %endpoint, "listening to the node's ZMQ announcements");
        // Whatever was announced while nobody listened.
        wakes.pool_changed();
        wakes.chain_changed();
        loop {
            let message = tokio::select! {
                received = socket.recv() => match received {
                    Ok(message) => message,
                    Err(error) => return error.to_string(),
                },
                event = events.next() => match event {
                    Some(SocketEvent::Disconnected(_)) | None => {
                        return "the node closed the connection".to_owned();
                    }
                    Some(_) => continue,
                },
            };
            let Some(announced) = message.get(0).and_then(|frame| announcement(frame)) else {
                continue;
            };
            wakes.publisher(endpoint, |p| {
                match announced {
                    Announcement::Pool => p.pool_announcements += 1,
                    Announcement::Chain => p.block_announcements += 1,
                }
                p.last_announcement_at = Some(shared::time::now_unix());
            });
            match announced {
                Announcement::Pool => wakes.pool_changed(),
                Announcement::Chain => wakes.chain_changed(),
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test(start_paused = true)]
    async fn a_wait_runs_its_interval_when_nothing_is_announced() {
        let wakes = NodeWakes::default();
        let started = tokio::time::Instant::now();
        assert!(!wakes.pool_or(Duration::from_millis(250)).await);
        assert_eq!(started.elapsed(), Duration::from_millis(250));
    }

    #[tokio::test(start_paused = true)]
    async fn an_announcement_ends_the_wait_after_the_minimum_gap() {
        let wakes = Arc::new(NodeWakes::default());
        let started = tokio::time::Instant::now();
        let waiter = tokio::spawn({
            let wakes = Arc::clone(&wakes);
            async move { wakes.chain_or(Duration::from_secs(1)).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        wakes.chain_changed();
        assert!(waiter.await.unwrap());
        assert_eq!(started.elapsed(), Duration::from_millis(100));
    }

    #[tokio::test(start_paused = true)]
    async fn an_announcement_during_a_pass_is_kept_for_the_wait_after_it() {
        let wakes = NodeWakes::default();
        // Several, while nobody waits: one wake-up, then the full interval.
        wakes.pool_changed();
        wakes.pool_changed();
        let started = tokio::time::Instant::now();
        assert!(wakes.pool_or(Duration::from_secs(1)).await);
        assert_eq!(started.elapsed(), MIN_GAP);
        assert!(!wakes.pool_or(Duration::from_secs(1)).await);
    }

    #[tokio::test(start_paused = true)]
    async fn pool_and_chain_announcements_wake_only_their_own_wait() {
        let wakes = NodeWakes::default();
        wakes.chain_changed();
        assert!(!wakes.pool_or(Duration::from_millis(250)).await);
        assert!(wakes.chain_or(Duration::from_secs(1)).await);
    }

    #[tokio::test(start_paused = true)]
    async fn an_interval_shorter_than_the_gap_is_kept() {
        let wakes = NodeWakes::default();
        wakes.pool_changed();
        let started = tokio::time::Instant::now();
        wakes.pool_or(Duration::from_millis(5)).await;
        assert_eq!(started.elapsed(), Duration::from_millis(5));
    }

    #[tokio::test(start_paused = true)]
    async fn only_waits_cut_short_are_counted_and_only_reported_with_a_publisher() {
        let wakes = NodeWakes::default();
        wakes.pool_changed();
        wakes.pool_or(Duration::from_secs(1)).await;
        wakes.pool_or(Duration::from_secs(1)).await;
        wakes.chain_or(Duration::from_secs(1)).await;
        assert_eq!(
            wakes.announcements(),
            None,
            "nothing to report without a publisher"
        );
        wakes
            .publishers
            .lock()
            .push(shared::announcements::Publisher::default());
        let reported = wakes.announcements().unwrap();
        assert_eq!((reported.pool_passes_woken, reported.rounds_woken), (1, 0));
    }

    #[cfg(feature = "zmq")]
    #[test]
    fn a_publisher_still_configured_keeps_its_figures_and_one_removed_goes() {
        let wakes = NodeWakes::default();
        let pair = |node: &str, endpoint: &str| (node.to_owned(), endpoint.to_owned());
        wakes.listen_to(&[pair("a:1", "tcp://a:2"), pair("b:1", "tcp://b:2")]);
        wakes.publisher("tcp://a:2", |p| {
            p.connected = true;
            p.pool_announcements = 7;
        });
        wakes.listen_to(&[pair("c:1", "tcp://c:2"), pair("a2:1", "tcp://a:2")]);
        let publishers = wakes.announcements().unwrap().publishers;
        assert_eq!(publishers.len(), 2);
        assert_eq!(publishers[0].endpoint, "tcp://c:2");
        let kept = &publishers[1];
        assert_eq!((kept.node.as_str(), kept.pool_announcements), ("a2:1", 7));
        assert!(!kept.connected, "about to connect again");
    }

    #[cfg(feature = "zmq")]
    mod zmq {
        use std::sync::Arc;
        use std::time::Duration;

        use zeromq::{PubSocket, Socket as _, SocketSend as _, ZmqMessage};

        use super::super::subscriber::{
            announcement, listen, Announcement, CHAIN_TOPIC, POOL_TOPIC,
        };
        use super::super::NodeWakes;

        #[test]
        fn only_the_two_topics_are_announcements() {
            let pool = format!(r#"{POOL_TOPIC}:[{{"id":"ab","blob_size":1,"weight":1,"fee":1}}]"#);
            let chain =
                format!(r#"{CHAIN_TOPIC}:{{"first_height":1,"first_prev_id":"ab","ids":["cd"]}}"#);
            assert_eq!(announcement(pool.as_bytes()), Some(Announcement::Pool));
            assert_eq!(announcement(chain.as_bytes()), Some(Announcement::Chain));
            for other in [
                "json-full-txpool_add:[]",
                "json-full-chain_main:[]",
                "json-full-miner_data:{}",
                "json-minimal-txpool_add",
                "json-minimal-txpool_addx:[]",
                "",
            ] {
                assert_eq!(announcement(other.as_bytes()), None, "{other}");
            }
        }

        /// The subscriber listens to the publisher a fallback's setting names.
        #[tokio::test]
        async fn the_subscriber_listens_where_the_node_settings_say() {
            let mut publisher = PubSocket::new();
            let endpoint = publisher
                .bind("tcp://127.0.0.1:0")
                .await
                .unwrap()
                .to_string();
            let node = |zmq_pub: Option<String>| crate::settings::MoneroNodeSetting {
                host: "127.0.0.1".to_owned(),
                port: 9,
                ssl: false,
                accept_self_signed_certs: true,
                fallbacks: vec![],
                zmq_pub,
            };
            let mut primary = node(None);
            primary.fallbacks.push(node(Some(endpoint)));
            let defaults = crate::engine_settings::EngineSettings::defaults();
            let settings = Arc::new(crate::engine_settings::EngineSettings {
                registry: None,
                env: live_settings::Env::fixed(Vec::<(String, String)>::new()),
                nodes: live_settings::Live::new(crate::engine_settings::NodeConfig {
                    nodes: [("stagenet", primary)].into(),
                    strict_tls: false,
                }),
                scan: defaults.scan.clone(),
                webhooks: defaults.webhooks.clone(),
                limits: defaults.limits.clone(),
                tenant_defaults: defaults.tenant_defaults.clone(),
                runtime: defaults.runtime.clone(),
                custody: defaults.custody.clone(),
                embedded: false,
            });
            let wakes = Arc::new(NodeWakes::default());
            let subscriber = tokio::spawn(super::super::run_subscriber(
                monero::Network::Stagenet,
                Arc::clone(&wakes),
                settings,
            ));
            assert!(
                wakes.chain_or(Duration::from_secs(5)).await,
                "connecting wakes the loops"
            );
            let publisher_status = || wakes.announcements().unwrap().publishers[0].clone();
            let connected = publisher_status();
            assert_eq!(connected.node, "127.0.0.1:9", "the fallback's");
            assert!(connected.connected && connected.connected_since.is_some());
            assert_eq!(connected.connections, 1);
            assert_eq!(wakes.announcements().unwrap().rounds_woken, 1);

            // Announcements are counted by kind once they land.
            for _ in 0..100 {
                publisher
                    .send(ZmqMessage::from(format!("{POOL_TOPIC}:[]")))
                    .await
                    .unwrap();
                if wakes.pool_or(Duration::from_millis(50)).await
                    && publisher_status().pool_announcements > 0
                {
                    break;
                }
            }
            let counted = publisher_status();
            assert!(counted.pool_announcements > 0 && counted.last_announcement_at.is_some());
            assert_eq!(counted.block_announcements, 0);

            // The publisher goes away: shown as lost, with why.
            drop(publisher);
            for _ in 0..100 {
                if !publisher_status().connected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let lost = publisher_status();
            assert!(!lost.connected && lost.last_error.is_some(), "{lost:?}");
            assert!(lost.pool_announcements > 0, "its figures are kept");
            subscriber.abort();
        }

        /// A real publisher, as monerod would be: each topic wakes its own
        /// wait. A publisher drops what it sends before the subscription
        /// reaches it, so each is sent until it lands.
        #[tokio::test]
        async fn a_publisher_s_announcements_wake_the_loops() {
            let mut publisher = PubSocket::new();
            let endpoint = publisher
                .bind("tcp://127.0.0.1:0")
                .await
                .unwrap()
                .to_string();
            let wakes = Arc::new(NodeWakes::default());
            let listener = tokio::spawn(listen(
                monero::Network::Stagenet,
                endpoint,
                Arc::clone(&wakes),
            ));
            // Connecting wakes both, for whatever was missed meanwhile.
            assert!(wakes.pool_or(Duration::from_secs(5)).await);
            assert!(wakes.chain_or(Duration::from_secs(5)).await);
            for topic in [POOL_TOPIC, CHAIN_TOPIC] {
                let mut landed = false;
                for _ in 0..100 {
                    publisher
                        .send(ZmqMessage::from(format!("{topic}:[]")))
                        .await
                        .unwrap();
                    let wait = Duration::from_millis(50);
                    let woke = if topic == POOL_TOPIC {
                        wakes.pool_or(wait).await
                    } else {
                        wakes.chain_or(wait).await
                    };
                    if woke {
                        landed = true;
                        break;
                    }
                }
                assert!(landed, "{topic} never woke its wait");
            }
            listener.abort();
        }
    }
}
