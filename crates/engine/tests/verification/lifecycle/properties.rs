//! Generation lifecycle through the production supervisor and network owner.
use super::*;
use crate::property_support::{config, runtime};
use proptest::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn generations_stop_before_replacement_and_recover_from_panics(
        actions in prop::collection::vec((0usize..3, any::<bool>(), 0u8..3), 1..40),
    ) {
        runtime().block_on(async {
            tokio::time::pause();
            let counts: [Arc<AtomicUsize>; 3] = std::array::from_fn(|_| Arc::new(AtomicUsize::new(0)));
            let mut generations: [Option<RunningNetwork>; 3] = std::array::from_fn(|_| None);
            for (network, enabled, failure) in actions {
                if let Some(old) = generations[network].take() { old.stopped().await; }
                assert_eq!(counts[network].load(Ordering::SeqCst), 0);
                if enabled {
                    let (stop, stopped) = tokio::sync::watch::channel(false);
                    let mut tasks = Vec::new();
                    for _ in 0..3 {
                        let count = Arc::clone(&counts[network]);
                        let attempt = Arc::new(AtomicUsize::new(0));
                        let (entered, ready) = tokio::sync::mpsc::unbounded_channel();
                        let mut ready = ready;
                        tasks.push(supervise_until("property network generation", stopped.clone(), move || {
                            let guard = Active(Arc::clone(&count));
                            count.fetch_add(1, Ordering::SeqCst);
                            let first = attempt.fetch_add(1, Ordering::SeqCst) == 0;
                            let entered = entered.clone();
                            async move {
                                let _guard = guard;
                                entered.send(()).unwrap();
                                assert!(!(first && failure == 1), "first generation attempt");
                                if first && failure == 2 { return; }
                                std::future::pending::<()>().await;
                            }
                        }));
                        ready.recv().await.unwrap();
                        if failure != 0 {
                            tokio::time::advance(Duration::from_secs(6)).await;
                            ready.recv().await.unwrap();
                        }
                    }
                    assert_eq!(counts[network].load(Ordering::SeqCst), 3);
                    generations[network] = Some(RunningNetwork { stop, tasks });
                }
                for i in 0..3 {
                    assert_eq!(counts[i].load(Ordering::SeqCst), if generations[i].is_some() { 3 } else { 0 });
                }
            }
            for generation in generations.into_iter().flatten() { generation.stopped().await; }
            assert!(counts.iter().all(|c| c.load(Ordering::SeqCst) == 0));
        });
    }

    #[test]
    fn dropping_a_manager_generation_stops_its_children(tasks in 1usize..5) {
        runtime().block_on(async {
            let active = Arc::new(AtomicUsize::new(0));
            let (stop, stopped) = tokio::sync::watch::channel(false);
            let mut handles = Vec::new();
            for _ in 0..tasks {
                let active = Arc::clone(&active);
                let (entered, ready) = tokio::sync::oneshot::channel();
                let entered = Arc::new(parking_lot::Mutex::new(Some(entered)));
                handles.push(supervise_until("property manager dropped", stopped.clone(), move || {
                    let guard = Active(Arc::clone(&active));
                    active.fetch_add(1, Ordering::SeqCst);
                    let sender = entered.lock().take();
                    if let Some(s) = sender { let _ = s.send(()); }
                    async move { let _guard = guard; std::future::pending::<()>().await; }
                }));
                ready.await.unwrap();
            }
            assert_eq!(active.load(Ordering::SeqCst), tasks);
            drop(RunningNetwork { stop, tasks: handles });
            tokio::time::timeout(Duration::from_secs(10), async {
                while active.load(Ordering::SeqCst) != 0 { tokio::task::yield_now().await; }
            }).await.unwrap();
        });
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn real_network_manager_follows_generated_node_configuration(actions in prop::collection::vec((0u8..8,any::<bool>()),1..12)) {
        runtime().block_on(async {
            use axum::{body::Body,http::{Request,StatusCode}};
            use tower::ServiceExt as _;
            use crate::http::rate_limit::RateLimiter;
            // A node for each network, and a fourth that every network's
            // fallback names; what reaches the fourth isn't checked.
            let entered: [Arc<AtomicUsize>;4] = std::array::from_fn(|_| Arc::new(AtomicUsize::new(0)));
            let mut ports = [0;4];
            let mut servers = Vec::new();
            for i in 0..4 {
                let counter = Arc::clone(&entered[i]);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                ports[i] = listener.local_addr().unwrap().port();
                // Reject admin get_info probes immediately; hang the scanner's
                // plain RPC endpoints so configuration histories exercise real
                // work cancellation without three seconds per API save. The
                // fallback is one of these too, not a closed port: Windows
                // takes two seconds to refuse each save's probe of one.
                let endpoint = axum::Router::new()
                    .route("/json_rpc",axum::routing::post(async || { StatusCode::SERVICE_UNAVAILABLE }))
                    .fallback(move || {
                        let counter = Arc::clone(&counter);
                        async move {
                            counter.fetch_add(1,Ordering::SeqCst);
                            std::future::pending::<StatusCode>().await
                        }
                    });
                servers.push(tokio::spawn(async move { axum::serve(listener,endpoint).await.unwrap(); }));
            }
            let store = crate::store::Store::open_in_memory().unwrap().into_shared();
            let daemons = Daemons::default();
            let limiter = Arc::new(RateLimiter::new(100_000));
            let settings = EngineSettings::load_with(Arc::clone(&store),Some(crate::engine_settings::NodesReloadable { daemons: daemons.clone() }),Arc::clone(&limiter),live_settings::Env::fixed(Vec::<(String,String)>::new())).await.unwrap();
            let custody: Arc<dyn KeyCustody> = Arc::new(crate::key_custody::PlainKeyCustody::default());
            let wallet_handles = Arc::new(RwLock::new(HashMap::new()));
            let status = scanner_status::new_scanner_status_map();
            let state = crate::http::AppState {
                db: crate::store::Database::inline(Arc::clone(&store)),admin_rate_limiter:limiter,log_store:None,
                engine_token:Arc::new(shared::auth::RawToken::presented(crate::http::TEST_ENGINE_TOKEN).hash()),
                settings:Arc::clone(&settings),
                custody:crate::http::Custody { snp:None,backends:Arc::clone(&custody),default_backend:"plain".into(),wallet_handles:Arc::clone(&wallet_handles) },
                networks:crate::http::Networks { daemons:daemons.clone(),scanner_status:Arc::clone(&status) },
            };
            let router = crate::http::build_router(state,1<<20);
            let manager = tokio::spawn(manage_network_loops(Db::over_shared(Arc::clone(&store)),custody,daemons.clone(),wallet_handles,Arc::clone(&status),settings));
            let networks = [Network::Mainnet,Network::Stagenet,Network::Testnet];
            let mut previous = 0u8;
            for (mask,fallback) in actions.into_iter().chain([(0,false)]) {
                let before: [usize;3] = std::array::from_fn(|i| entered[i].load(Ordering::SeqCst));
                let mut nodes = serde_json::Map::new();
                for (i,network) in networks.iter().enumerate() {
                    nodes.insert(crate::network::network_str(*network).to_owned(),if mask & (1<<i)==0 { serde_json::Value::Null }
                        else { serde_json::json!({"host":"127.0.0.1","port":ports[i],"ssl":false,"fallbacks":if fallback {vec![serde_json::json!({"host":"127.0.0.1","port":ports[3]})]} else {vec![]}}) });
                }
                let response = router.clone().oneshot(Request::builder().method("POST").uri("/api/v1/admin/settings")
                    .header("content-type","application/json").header(shared::auth::ENGINE_TOKEN_HEADER,crate::http::TEST_ENGINE_TOKEN)
                    .body(Body::from(serde_json::json!({"monero_node":nodes}).to_string())).unwrap()).await.unwrap();
                assert_eq!(response.status(),StatusCode::OK);
                tokio::time::timeout(Duration::from_secs(10),async {
                    loop {
                        let correct = { let s=status.read(); networks.iter().enumerate().all(|(i,n)| s.contains_key(n)==(mask & (1<<i)!=0)) };
                        if correct { break; }
                        tokio::task::yield_now().await;
                    }
                }).await.unwrap();
                for (i,network) in networks.iter().enumerate() {
                    let client = daemons.get(*network);
                    assert_eq!(client.as_ref().map(|c| c.nodes().len()),if mask & (1<<i)==0 {None} else {Some(if fallback {2} else {1})});
                }
                if mask & !previous != 0 {
                    // Prove a newly started network reached a real hanging RPC
                    // before subsequent disabling or reconfiguration actions.
                    tokio::time::timeout(Duration::from_secs(10),async {
                        for i in 0..3 {
                            if mask & !previous & (1<<i) != 0 {
                                while entered[i].load(Ordering::SeqCst)==before[i] { tokio::task::yield_now().await; }
                            }
                        }
                    }).await.unwrap();
                }
                previous = mask;
            }
            manager.abort(); let _ = manager.await;
            for server in servers { server.abort(); let _ = server.await; }
            assert!(status.read().is_empty());
        });
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn stopping_during_factory_or_loop_restart_backoff_is_immediate(factory_panic in any::<bool>(), failures in 1usize..8) {
        runtime().block_on(async {
            tokio::time::pause();
            let (stop, stopped) = tokio::sync::watch::channel(false);
            let (entered, mut ready) = tokio::sync::mpsc::unbounded_channel();
            let task = supervise_until("property stopping backoff",stopped,move || {
                entered.send(()).unwrap();
                assert!(!factory_panic, "injected factory panic");
                async { panic!("injected loop panic"); }
            });
            for _ in 0..failures {
                ready.recv().await.unwrap();
                // Let the supervisor enter its restart wait. Receiver waiting
                // for the next iteration advances paused time automatically.
                for _ in 0..4 { tokio::task::yield_now().await; }
            }
            let started = tokio::time::Instant::now();
            stop.send(true).unwrap();
            task.await.unwrap();
            assert!(started.elapsed() < Duration::from_secs(1), "stop waited through restart backoff");
        });
    }
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/loop_properties.txt"
        ),
    )
}
