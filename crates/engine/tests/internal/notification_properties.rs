//! Notifications affect latency, while RPC/polling remain the source of truth.
use super::*;
use crate::property_support::{config, runtime};
use proptest::prelude::*;
use std::sync::Arc;

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn notification_histories_match_pending_permits(data in prop::collection::vec(any::<u8>(),0..385)) {
        crate::exploration::notifications(&data);
    }
    #[test]
    fn arbitrary_topics_match_exact_prefixes(data in prop::collection::vec(any::<u8>(),0..4097)) {
        let expected = data.iter().position(|&b|b==b':').and_then(|at| match &data[..at] { b"json-minimal-txpool_add"=>Some(Announcement::Pool),b"json-minimal-chain_main"=>Some(Announcement::Chain),_=>None });
        prop_assert_eq!(announcement(&data),expected);
    }
    #[test]
    fn stale_generation_announcements_do_not_wake_replacements(burst in 1usize..257) {
        runtime().block_on(async {
            tokio::time::pause();
            let old = Arc::new(NodeWakes::default());
            let new = Arc::new(NodeWakes::default());
            for _ in 0..burst { old.pool_changed(); old.chain_changed(); }
            assert!(!new.pool_or(Duration::from_millis(50)).await);
            assert!(!new.chain_or(Duration::from_millis(50)).await);
            assert!(!new.proof_or(Duration::from_millis(50)).await);
            new.chain_changed();
            assert!(new.chain_or(Duration::from_millis(50)).await);
            assert!(new.proof_or(Duration::from_millis(50)).await);
        });
    }
}

#[test]
fn reviewed_notification_histories_replay() {
    for data in [
        include_bytes!("../../../../fuzz/seeds/notifications/burst-isolation").as_slice(),
        include_bytes!("../../../../fuzz/seeds/notifications/topic-pool").as_slice(),
        include_bytes!("../../../../fuzz/seeds/notifications/topic-chain").as_slice(),
        include_bytes!("../../../../fuzz/seeds/notifications/cancel-assigned").as_slice(),
        include_bytes!("../../../../fuzz/seeds/notifications/replace-pending").as_slice(),
    ] {
        crate::exploration::notifications(data);
    }
}

struct Task(tokio::task::JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn poll_recovery(mode: u8, burst: usize) {
    use crate::engine_settings::{Daemons, EngineSettings};
    use crate::node_test_support::{AdversarialNode, Rpc};
    use crate::store::{Db, Store};
    let store = Store::open_in_memory().unwrap().into_shared();
    let custody = Arc::new(crate::key_custody::PlainKeyCustody::default());
    let (tenant, handle, order) =
        crate::scanner::tests::fixture_tenant_shared(&store, custody.as_ref(), i64::MAX).await;
    let node = Arc::new(AdversarialNode::new());
    let daemon = Arc::new(crate::daemon_fallback::FallbackDaemonClient::new(vec![
        crate::daemon_fallback::FallbackNode {
            label: "notification-node".into(),
            client: Arc::<AdversarialNode>::clone(&node),
        },
    ]));
    let daemons = Daemons::fixed([(monero::Network::Mainnet, daemon)].into());
    let mut settings = EngineSettings::defaults();
    let scan = crate::engine_settings::ScanConfig {
        poll_interval: Duration::from_millis(50),
        ..settings.scan.load().as_ref().clone()
    };
    Arc::get_mut(&mut settings).unwrap().scan = live_settings::Live::new(scan);
    let wakes = Arc::new(NodeWakes::default());
    let state = Arc::new(crate::work::ScanState::default().with_wakes(Arc::clone(&wakes)));
    let handles = Arc::new(parking_lot::RwLock::new([(tenant, handle)].into()));
    let mut task = Task(tokio::spawn(crate::loops::run_fast_mempool_loop(
        state,
        Db::over_shared(Arc::clone(&store)),
        custody,
        monero::Network::Mainnet,
        daemons,
        handles,
        settings,
    )));
    tokio::time::timeout(Duration::from_secs(5), async {
        while node.counts(Rpc::Pool).completed == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Announcements request an RPC pass; they cannot themselves create money.
    let before = node.counts(Rpc::Pool).completed;
    wakes.pool_changed();
    wakes.chain_changed();
    tokio::time::timeout(Duration::from_secs(5), async {
        while node.counts(Rpc::Pool).completed == before {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        store.lock().get_all_payments(&order).unwrap().is_empty(),
        "an announcement credited money without RPC evidence"
    );
    node.fake
        .set_mempool(vec![crate::scanner::tests::fixture_tx()]);
    if mode == 3 {
        node.fake.set_online(false);
        let before = node.counts(Rpc::Pool).attempted;
        tokio::time::timeout(Duration::from_secs(5), async {
            while node.counts(Rpc::Pool).attempted == before {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        node.fake.set_online(true);
    }
    let wrong = NodeWakes::default();
    for _ in 0..burst {
        if mode == 1 {
            wakes.pool_changed();
            wakes.chain_changed();
        }
        if mode == 2 {
            wrong.pool_changed();
            wrong.chain_changed();
        }
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while store.lock().get_all_payments(&order).unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(store.lock().get_all_payments(&order).unwrap().len(), 1);
    task.0.abort();
    // Await abortion before asserting that stale wakes cannot restart this loop.
    let _ = (&mut task.0).await;
    let calls = node.counts(Rpc::Pool).attempted;
    for _ in 0..burst {
        wakes.pool_changed();
    }
    tokio::time::sleep(Duration::from_millis(70)).await;
    assert_eq!(node.counts(Rpc::Pool).attempted, calls);
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn actual_mempool_loop_detects_payments_despite_missing_wrong_or_burst_notifications(mode in 0u8..4,burst in 1usize..65) {
        runtime().block_on(poll_recovery(mode,burst));
    }
}
#[test]
fn all_polling_recovery_modes_run() {
    runtime().block_on(async {
        for mode in 0..4 {
            poll_recovery(mode, 8).await;
        }
    });
}

#[cfg(feature = "zmq")]
mod zmq {
    use super::*;
    use zeromq::{PubSocket, Socket as _, SocketSend as _, ZmqMessage};

    async fn until(wakes: &NodeWakes, condition: impl Fn(&Announcements) -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if wakes.announcements().as_ref().is_some_and(&condition) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn save(
        settings: &crate::engine_settings::EngineSettings,
        endpoint: Option<&str>,
        duplicate: bool,
    ) {
        let value=endpoint.map(|endpoint|serde_json::json!({"host":"127.0.0.1","port":9,"zmq_pub":endpoint,"fallbacks":if duplicate {vec![serde_json::json!({"host":"127.0.0.1","port":10,"zmq_pub":endpoint})]} else {vec![]}}));
        settings
            .registry
            .as_ref()
            .unwrap()
            .save(vec![(
                "monero_node.mainnet".into(),
                value.map(|v| v.to_string()),
            )])
            .await
            .unwrap();
    }
    async fn send_until(publisher: &mut PubSocket, wakes: &NodeWakes, topic: &str) {
        let before = wakes.announcements().unwrap().publishers[0].clone();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                publisher
                    .send(ZmqMessage::from(format!(
                        "{topic}:deliberately ignored JSON"
                    )))
                    .await
                    .unwrap();
                let now = wakes.announcements().unwrap().publishers[0].clone();
                if now.pool_announcements > before.pool_announcements
                    || now.block_announcements > before.block_announcements
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn history(actions: Vec<(u8, bool, u8)>) {
        let mut publishers = [PubSocket::new(), PubSocket::new()];
        let first = publishers[0]
            .bind("tcp://127.0.0.1:0")
            .await
            .unwrap()
            .to_string();
        let second = publishers[1]
            .bind("tcp://127.0.0.1:0")
            .await
            .unwrap()
            .to_string();
        let endpoints = [first, second];
        let settings = crate::engine_settings::EngineSettings::load_with(
            crate::store::Store::open_in_memory().unwrap().into_shared(),
            None,
            Arc::new(crate::http::rate_limit::RateLimiter::new(100_000)),
            live_settings::Env::fixed(Vec::<(String, String)>::new()),
        )
        .await
        .unwrap();
        let wakes = Arc::new(NodeWakes::default());
        let mut subscriber = Task(tokio::spawn(run_subscriber(
            monero::Network::Mainnet,
            Arc::clone(&wakes),
            Arc::clone(&settings),
        )));
        save(&settings, Some(&endpoints[0]), true).await;
        until(&wakes, |a| {
            a.publishers.len() == 1 && a.publishers[0].connected
        })
        .await;
        send_until(&mut publishers[0], &wakes, POOL_TOPIC).await;
        let before = wakes.announcements().unwrap().publishers[0].clone();
        // A real transport disconnect and bind to the same endpoint: recovery
        // must happen without saving settings or replacing NodeWakes.
        let old = std::mem::replace(&mut publishers[0], PubSocket::new());
        drop(old);
        until(&wakes, |a| {
            !a.publishers[0].connected && a.publishers[0].last_error.is_some()
        })
        .await;
        publishers[0].bind(&endpoints[0]).await.unwrap();
        until(&wakes, |a| {
            a.publishers[0].connected && a.publishers[0].connections > before.connections
        })
        .await;
        assert!(
            wakes.announcements().unwrap().publishers[0].pool_announcements
                >= before.pool_announcements
        );
        send_until(&mut publishers[0], &wakes, CHAIN_TOPIC).await;
        // A pool frame on this same ordered connection acknowledges that all
        // earlier chain frames were processed before taking topic baselines.
        send_until(&mut publishers[0], &wakes, POOL_TOPIC).await;
        for (endpoint, duplicate, burst) in actions.into_iter().chain([(2, false, 1)]) {
            if endpoint == 2 {
                save(&settings, None, false).await;
                tokio::time::timeout(Duration::from_secs(5), async {
                    while wakes.announcements().is_some() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                continue;
            }
            let index = usize::from(endpoint);
            save(&settings, Some(&endpoints[index]), duplicate).await;
            until(&wakes, |a| {
                a.publishers.len() == 1
                    && a.publishers[0].endpoint == endpoints[index]
                    && a.publishers[0].connected
            })
            .await;
            send_until(&mut publishers[index], &wakes, POOL_TOPIC).await;
            let before = wakes.announcements().unwrap().publishers[0].clone();
            for _ in 0..burst {
                publishers[index]
                    .send(ZmqMessage::from("json-minimal-txpool_addx:{}".to_owned()))
                    .await
                    .unwrap();
            }
            send_until(&mut publishers[index], &wakes, POOL_TOPIC).await;
            let after = wakes.announcements().unwrap().publishers[0].clone();
            assert_eq!(
                after.block_announcements, before.block_announcements,
                "pool and invalid messages changed chain count"
            );
            assert!(after.pool_announcements > before.pool_announcements);
            // Previously configured publishers must no longer affect this one.
            let count = after.block_announcements;
            publishers[1 - index]
                .send(ZmqMessage::from(format!("{CHAIN_TOPIC}:{{}}")))
                .await
                .unwrap();
            send_until(&mut publishers[index], &wakes, POOL_TOPIC).await;
            assert_eq!(
                wakes.announcements().unwrap().publishers[0].block_announcements,
                count
            );
        }
        subscriber.0.abort();
        let _ = (&mut subscriber.0).await;
    }
    proptest! {
        #![proptest_config(persisted_config(config()))]
        #[test]
        fn real_subscriber_recovers_and_tracks_generated_publisher_configuration(actions in prop::collection::vec((0u8..3,any::<bool>(),1u8..9),1..8)) {
            runtime().block_on(history(actions));
        }
    }
    #[test]
    fn publisher_switch_disable_reenable_and_duplicate_endpoints() {
        runtime().block_on(history(vec![
            (1, true, 4),
            (2, false, 1),
            (0, false, 4),
            (1, false, 4),
        ]));
    }
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/notification_properties.txt"
        ),
    )
}
