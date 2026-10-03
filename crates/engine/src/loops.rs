//! The engine's background loops (tasks 2.1, 7.4, 7.9): one scheduler loop
//! and one fast mempool loop per network with a node configured (the
//! double-spend void recheck is the scheduler's upkeep tier), started and stopped as node settings are saved, and the
//! webhook delivery loop. `main` supervises them; they live here so they
//! can be tested.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use monero::Network;
use parking_lot::RwLock;
use shared::supervise::supervise_until;
use tracing::Instrument;

use crate::engine_settings::{Daemons, EngineSettings};
use crate::http::now_unix;
use crate::key_custody::{KeyCustody, WalletHandle};
use crate::scanner_status::{self, ScannerStatusMap};
use crate::store::Db;
use crate::webhook_delivery::run_delivery_tick_on;

/// Delivers due webhooks, waking as soon as the scanner enqueues one (`wake`)
/// and otherwise every few seconds (retries fall due with time).
pub async fn run_webhook_delivery_loop(
    db: Db,
    settings: Arc<EngineSettings>,
    wake: Arc<tokio::sync::Notify>,
) {
    // Building the client can only fail if the TLS backend can't initialise.
    // Retry rather than panic, so the supervisor isn't left in a crash loop.
    let client = loop {
        match crate::webhook_delivery::WebhookClient::build() {
            Ok(client) => break client,
            Err(e) => {
                tracing::error!(error = %e, "failed to build the webhook HTTP client, retrying in 30s");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    };

    loop {
        // Read every tick, so saved webhook settings apply to the next
        // attempt (task 2.4).
        let config = settings.webhooks.load();
        client.set_allow_private(config.allow_private_urls);
        let sent = match run_delivery_tick_on(
            &db,
            &client,
            config.delivery_timeout,
            config.max_attempts,
            now_unix(),
        )
        .await
        {
            Ok(sent) => sent,
            Err(e) => {
                tracing::warn!(error = %e, "webhook delivery tick failed");
                0
            }
        };
        // A full batch means a backlog: carry on straight away rather than
        // waiting, so it drains steadily.
        if sent < crate::webhook_delivery::DELIVERY_BATCH as usize {
            let _ = tokio::time::timeout(Duration::from_secs(5), wake.notified()).await;
        }
    }
}

/// A `'static` name per (loop, network) for `supervise`, which labels logs
/// and restart counts with it.
pub fn fast_mempool_loop_name(network: Network) -> &'static str {
    match network {
        Network::Mainnet => "fast mempool (mainnet)",
        Network::Stagenet => "fast mempool (stagenet)",
        Network::Testnet => "fast mempool (testnet)",
    }
}

/// How often the fast path polls the pool for new transactions: a fraction
/// of a second, but never more often than the scan settings' interval
/// allows if that is shorter still.
pub fn fast_mempool_interval(poll_interval: Duration) -> Duration {
    poll_interval.min(Duration::from_millis(250))
}

/// Looks at the pool every fraction of a second for transactions not seen
/// before, and records and settles any payment among them at once
/// (`work::fast_pass`). The network's round loop remains the safety net: it
/// rescans the pool's transactions on its own rotation.
pub async fn run_fast_mempool_loop(
    scan_state: Arc<crate::work::ScanState>,
    db: Db,
    key_custody: Arc<dyn KeyCustody>,
    network: Network,
    daemons: Daemons,
    wallet_handles: Arc<RwLock<HashMap<crate::store::TenantId, WalletHandle>>>,
    settings: Arc<EngineSettings>,
) {
    loop {
        let scan = settings.scan.load();
        let interval = fast_mempool_interval(scan.poll_interval);
        if let Some(daemon) = daemons.get(network) {
            let tenants: Vec<(crate::store::TenantId, WalletHandle)> = wallet_handles
                .read()
                .iter()
                .map(|(id, h)| (id.clone(), *h))
                .collect();
            let pinned = daemon.pin();
            let inputs = crate::work::RoundInputs {
                db: &db,
                custody: key_custody.as_ref(),
                daemon: &pinned,
                network,
                tenants: &tenants,
                reorg_check_depth: scan.reorg_check_depth,
                grace_period_seconds: scan.expired_order_grace_period_seconds,
                scan_chunk_memory_budget_mb: scan.scan_chunk_memory_budget_mb,
            };
            let pass = tokio::time::timeout(
                tick_deadline(scan.poll_interval),
                crate::work::fast_pass(&scan_state, &inputs),
            )
            .await;
            match pass {
                Ok(Some(report)) => {
                    if report.paid_orders > 0 {
                        tracing::debug!(network = ?network, orders = report.paid_orders, "payments seen in the mempool");
                    }
                }
                Ok(None) => {}
                // The round loop is the safety net, so this is not an
                // outage; but a pass that keeps hitting its deadline is a
                // node that keeps hanging, which an operator should see.
                Err(_) => shared::throttled!(
                    format!("fast-mempool-deadline:{network:?}"),
                    warn,
                    network = ?network,
                    "a fast mempool pass was abandoned at its deadline"
                ),
            }
        }
        // Sooner when the node announces a new pool transaction
        // (docs/monero_zmq.md): then its answer from a moment ago won't do.
        if scan_state.node_wakes().pool_or(interval).await {
            if let Some(daemon) = daemons.get(network) {
                crate::daemon::MoneroDaemonClient::pool_changed(daemon.as_ref());
            }
        }
    }
}

#[cfg(feature = "zmq")]
pub fn node_events_name(network: Network) -> &'static str {
    match network {
        Network::Mainnet => "node announcements (mainnet)",
        Network::Stagenet => "node announcements (stagenet)",
        Network::Testnet => "node announcements (testnet)",
    }
}

pub fn proof_loop_name(network: Network) -> &'static str {
    match network {
        Network::Mainnet => "proof of work (mainnet)",
        Network::Stagenet => "proof of work (stagenet)",
        Network::Testnet => "proof of work (testnet)",
    }
}

pub fn scanner_loop_name(network: Network) -> &'static str {
    match network {
        Network::Mainnet => "chain scanner (mainnet)",
        Network::Stagenet => "chain scanner (stagenet)",
        Network::Testnet => "chain scanner (testnet)",
    }
}

/// Longest one scan tick may run before it is abandoned and the next one
/// starts (task 7.9): a tick stuck on a call that never returns would
/// otherwise stop payment detection on its network without any error. Well
/// above a normal tick, which the per-call deadlines keep short.
pub fn tick_deadline(poll_interval: Duration) -> Duration {
    (poll_interval * 20).max(Duration::from_secs(120))
}

/// The outer deadline for one round on `daemon` given `budget`:
/// [`tick_deadline`], or room for the largest block request the node's link
/// allows under `tuning` (fetched and prefetched in one unit) after the
/// round's own budget, whichever is longer. A slow link's requests aren't
/// abandoned by the round around them (docs/engine_scaling.md section 2).
fn round_deadline(
    poll_interval: Duration,
    daemon: &dyn crate::daemon::MoneroDaemonClient,
    budget: Duration,
    tuning: &crate::work::ScanTuning,
) -> Duration {
    let largest = daemon.chain_blocks_timeout(tuning.chunk_max_blocks);
    tick_deadline(poll_interval).max(largest * 2 + budget)
}

/// Starts a scanner loop for each network that has a node configured, and stops them for a network whose node setting is
/// cleared, whenever node settings are saved (task 2.1). Runs for the life of
/// the process.
#[allow(clippy::too_many_arguments)] // the loops' genuinely independent shared handles
pub async fn manage_network_loops(
    db: Db,
    webhooks: Arc<tokio::sync::Notify>,
    key_custody: Arc<dyn KeyCustody>,
    daemons: Daemons,
    wallet_handles: Arc<RwLock<HashMap<crate::store::TenantId, WalletHandle>>>,
    scanner_status: ScannerStatusMap,
    settings: Arc<EngineSettings>,
) {
    let mut changed = settings.nodes.subscribe();
    let mut running: HashMap<Network, tokio::sync::watch::Sender<bool>> = HashMap::new();
    loop {
        let wanted: std::collections::HashSet<Network> = daemons.networks().into_iter().collect();
        running.retain(|network, stop| {
            let keep = wanted.contains(network);
            if !keep {
                let _ = stop.send(true);
                scanner_status.write().remove(network);
                tracing::info!(network = ?network, "stopped scanning: its node setting was cleared");
            }
            keep
        });
        for network in wanted {
            if running.contains_key(&network) {
                continue;
            }
            let (stop, stopped) = tokio::sync::watch::channel(false);
            // Checking on before any loop starts, so no order settles on
            // unchecked blocks while the proof loop gets going
            // (docs/proof_of_work.md). Its own rounds turn it off if not.
            if settings.scan.load().checks_proof_of_work(network) {
                let now = now_unix();
                if let Err(error) = db
                    .run(crate::store::db::Class::Scanner, move |s| {
                        s.enable_proof(network, now)
                    })
                    .await
                {
                    tracing::warn!(network = ?network, error = %error, "couldn't turn proof-of-work checking on before scanning; the proof loop does");
                }
            }
            // Shared by the network's round loop and its fast mempool loop.
            let scan_state = Arc::new(
                crate::work::ScanState::waking(webhooks.clone())
                    .with_progress(crate::scanner_status::progress_of(&scanner_status, network))
                    .with_wakes(crate::scanner_status::wakes_of(&scanner_status, network)),
            );
            #[cfg(feature = "zmq")]
            {
                let (wakes, settings) = (scan_state.node_wakes().clone(), settings.clone());
                supervise_until(node_events_name(network), stopped.clone(), move || {
                    crate::node_events::run_subscriber(network, wakes.clone(), settings.clone())
                });
            }
            {
                let (db, key_custody, daemons, wallet_handles, settings, scan_state) = (
                    db.clone(),
                    key_custody.clone(),
                    daemons.clone(),
                    wallet_handles.clone(),
                    settings.clone(),
                    scan_state.clone(),
                );
                supervise_until(
                    fast_mempool_loop_name(network),
                    stopped.clone(),
                    move || {
                        run_fast_mempool_loop(
                            scan_state.clone(),
                            db.clone(),
                            key_custody.clone(),
                            network,
                            daemons.clone(),
                            wallet_handles.clone(),
                            settings.clone(),
                        )
                    },
                );
            }
            {
                let (db, daemons, settings, status, wakes) = (
                    db.clone(),
                    daemons.clone(),
                    settings.clone(),
                    scanner_status.clone(),
                    scan_state.node_wakes().clone(),
                );
                supervise_until(proof_loop_name(network), stopped.clone(), move || {
                    crate::proof::run_loop(
                        network,
                        db.clone(),
                        daemons.clone(),
                        settings.clone(),
                        status.clone(),
                        wakes.clone(),
                        crate::proof::ProofTuning::DEFAULT,
                    )
                });
            }
            let (db, key_custody, daemons, wallet_handles, scanner_status, settings) = (
                db.clone(),
                key_custody.clone(),
                daemons.clone(),
                wallet_handles.clone(),
                scanner_status.clone(),
                settings.clone(),
            );
            supervise_until(scanner_loop_name(network), stopped, move || {
                run_scanner_loop(
                    scan_state.clone(),
                    db.clone(),
                    key_custody.clone(),
                    network,
                    daemons.clone(),
                    wallet_handles.clone(),
                    scanner_status.clone(),
                    settings.clone(),
                )
            });
            tracing::info!(network = ?network, "scanning");
            running.insert(network, stop);
        }
        if changed.changed().await.is_err() {
            return;
        }
    }
}

/// How often a scanner loop retries registering stores that have no live
/// handle - normally, and when a handle has just been lost.
pub const REGISTRATION_RETRY: Duration = Duration::from_secs(60);
pub const REGISTRATION_RETRY_AFTER_LOSS: Duration = Duration::from_secs(5);

/// Runs the scheduler's rounds (`work::run_round`) for one network, over
/// and over: at once while work is left, otherwise every poll interval.
/// Re-reads
/// `wallet_handles`, the network's node client and the scan settings every
/// round, so new tenants, saved node settings and saved scan settings all
/// apply from the next tick (tasks 2.1, 2.3). Each network has its own
/// loop (task 7.4), so a slow node on one never delays another.
#[allow(clippy::too_many_arguments)] // the loop's genuinely independent shared handles
pub async fn run_scanner_loop(
    scan_state: Arc<crate::work::ScanState>,
    db: Db,
    key_custody: Arc<dyn KeyCustody>,
    network: Network,
    daemons: Daemons,
    wallet_handles: Arc<RwLock<HashMap<crate::store::TenantId, WalletHandle>>>,
    scanner_status: ScannerStatusMap,
    settings: Arc<EngineSettings>,
) {
    // Keys that failed to register (at boot, or since) are retried here at
    // most once a minute, so a tenant whose key-custody backend comes back
    // is scanned again without anyone having to call the API for it.
    let mut last_registration_attempt: Option<tokio::time::Instant> = None;
    // The last pass left stores unregistered (their backend was down, say):
    // try again soon rather than in a minute.
    let mut registrations_failed = false;
    // `scan_state`: the scheduler's in-memory state, kept across rounds and
    // shared with the fast mempool loop (the mempool is fetched and scanned
    // incrementally, retry delays are remembered). Losing it costs only
    // repeated work: everything that matters is in the database.
    // Shared by every network's loop, so the handle map is cleared once per
    // lost-state epoch of the key-custody backend, not once per network.
    static HANDLED_CUSTODY_EPOCH: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);
    loop {
        // `debug`: a tick runs every few seconds, too often for a span at
        // `info` (each would become a stored trace). Lines inside carry
        // `network` themselves.
        let tick = tracing::debug_span!("scan tick", network = ?network);
        let scan = settings.scan.load();
        let Some(daemon) = daemons.get(network) else {
            // Being stopped: the node setting was just cleared.
            tokio::time::sleep(scan.poll_interval).await;
            continue;
        };
        // Sooner when a store's handle has just stopped being live (its
        // backend lost it, or was replaced or turned off): that store isn't
        // scanned until it's registered again.
        let lost_a_handle = wallet_handles
            .read()
            .values()
            .any(|handle| !key_custody.handle_is_live(*handle));
        let retry_after = if lost_a_handle || registrations_failed {
            REGISTRATION_RETRY_AFTER_LOSS
        } else {
            REGISTRATION_RETRY
        };
        if last_registration_attempt.is_none_or(|at| at.elapsed() >= retry_after) {
            last_registration_attempt = Some(tokio::time::Instant::now());
            let crate::scanner::Registration { registered, failed } =
                crate::scanner::register_missing_wallets_reporting(
                    &db,
                    key_custody.as_ref(),
                    &wallet_handles,
                    Some(&HANDLED_CUSTODY_EPOCH),
                    network,
                )
                .instrument(tick.clone())
                .await;
            registrations_failed = failed > 0;
            if registered > 0 {
                tracing::info!(network = ?network, stores = registered, "registered the keys of stores that had none");
            }
        }
        let tenants: Vec<(crate::store::TenantId, WalletHandle)> = wallet_handles
            .read()
            .iter()
            .map(|(id, h)| (id.clone(), *h))
            .collect();
        let started_at = now_unix();
        // One node for the whole tick (task 7.6), so answers from nodes at
        // different heights or on different forks are never mixed.
        let pinned = daemon.pin();
        let inputs = crate::work::RoundInputs {
            db: &db,
            custody: key_custody.as_ref(),
            daemon: &pinned,
            network,
            tenants: &tenants,
            reorg_check_depth: scan.reorg_check_depth,
            grace_period_seconds: scan.expired_order_grace_period_seconds,
            scan_chunk_memory_budget_mb: scan.scan_chunk_memory_budget_mb,
        };
        // The round keeps to its own budget (more only while one page of a
        // large block needs it); this outer deadline only catches a unit
        // stuck somewhere no inner deadline reaches.
        let budget = scan_state.round_budget(&pinned, tenants.len());
        let deadline = round_deadline(scan.poll_interval, &pinned, budget, scan_state.tuning());
        let (result, backlogged) = match tokio::time::timeout(
            deadline,
            crate::work::run_round(&scan_state, &inputs, budget).instrument(tick.clone()),
        )
        .await
        {
            Ok(report) => {
                let backlogged = report.backlogged();
                (report.into_status_result(), backlogged)
            }
            Err(_) => (
                Err(crate::scanner::ScannerError::Internal(format!(
                    "scan round did not finish within {deadline:?} and was abandoned"
                ))),
                false,
            ),
        };
        let finished_at = now_unix();
        if let Err(e) = &result {
            shared::throttled!(format!("tick-failed:{network:?}"), warn, network = ?network, error = %e, "scan tick failed");
        }
        // Not for a network whose node setting was cleared during this tick:
        // its status was removed when its loops were stopped.
        if daemons.get(network).is_some() {
            // The handles are one map for every network; the status row is
            // for this one.
            let on_network = db
                .run(crate::store::db::Class::Scanner, move |s| {
                    s.tenant_ids_on_network(network)
                })
                .await
                .unwrap_or_default();
            let scanned = tenants
                .iter()
                .filter(|(id, _)| on_network.contains(id))
                .count();
            scanner_status::record_tick(
                &scanner_status,
                network,
                started_at,
                finished_at,
                scanned,
                &result,
            );
        }
        // Work left over (a catch-up after downtime, a backlog of recomputes):
        // go again at once, yielding so other tasks run first.
        // Otherwise, sooner when the node announces a new block.
        if backlogged {
            tokio::task::yield_now().await;
        } else {
            scan_state.node_wakes().chain_or(scan.poll_interval).await;
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::daemon::fake::FakeDaemonClient;
    use crate::daemon_fallback::{FallbackDaemonClient, FallbackNode};
    use crate::http::rate_limit::RateLimiter;
    use crate::key_custody::{CustodyRouter, PlainKeyCustody, WalletMaterial};
    use crate::store::{NewTenant, Store};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// Waits up to 15s for `condition`.
    async fn eventually(what: &str, condition: impl Fn() -> bool) {
        for _ in 0..300 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    /// Webhook delivery acts on a newly enqueued webhook as soon as it is
    /// woken, not on its next few-second poll.
    #[tokio::test]
    async fn webhook_delivery_runs_as_soon_as_it_is_woken() {
        let store = Store::open_in_memory().unwrap();
        let tenant = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4wake".into(),
                    network: "mainnet".into(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap()
            .tenant;
        let index = store.allocate_minor_index(&tenant.id).unwrap();
        let order = store
            .create_order(crate::store::NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: tenant.id.clone(),
                merchant_order_id: None,
                minor_index: index,
                address: "a".into(),
                xmr_amount_piconero: 1,
                description: None,
                created_at: 1000,
                expires_at: i64::MAX / 4,
            })
            .unwrap();
        // A private address: refused, so the attempt is recorded at once
        // without any network wait.
        let webhook = store
            .create_webhook(&tenant.id, "http://127.0.0.1:9/hook", "{}", "secret", 1000)
            .unwrap();
        let store = store.into_shared();
        let wake = Arc::new(tokio::sync::Notify::new());
        let delivery = tokio::spawn(run_webhook_delivery_loop(
            Db::over_shared(store.clone()),
            EngineSettings::defaults(),
            wake.clone(),
        ));
        // Let its first pass find nothing and go to sleep.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let now = now_unix();
        store
            .lock()
            .enqueue_webhook_delivery(&webhook.id, &order.id, "order.paid", "{}", now)
            .unwrap();
        let woken_at = tokio::time::Instant::now();
        wake.notify_one();
        while !store
            .lock()
            .due_webhook_deliveries_for_test(now + 1, 10)
            .unwrap()
            .is_empty()
        {
            // Sooner than the loop's own 5 s idle wait could have run it:
            // only the wake explains it, however slow the machine.
            assert!(
                woken_at.elapsed() < Duration::from_millis(4500),
                "not attempted before the idle wait ran out, so the wake did nothing"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        delivery.abort();
    }

    #[tokio::test]
    async fn saving_a_node_starts_its_networks_loops_and_clearing_it_stops_them() {
        let store = Store::open_in_memory().unwrap().into_shared();
        let daemons = Daemons::default();
        let rate_limiter = Arc::new(RateLimiter::new(10_000));
        let settings = EngineSettings::load_with(
            store.clone(),
            Some(crate::engine_settings::NodesReloadable {
                daemons: daemons.clone(),
            }),
            rate_limiter.clone(),
            live_settings::Env::fixed(Vec::<(String, String)>::new()),
        )
        .await
        .unwrap();
        let key_custody: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let wallet_handles: Arc<RwLock<HashMap<crate::store::TenantId, WalletHandle>>> =
            Arc::default();
        let status = scanner_status::new_scanner_status_map();
        let state = crate::http::AppState {
            db: crate::store::Database::inline(store.clone()),
            admin_rate_limiter: rate_limiter,
            log_store: None,
            engine_token: Arc::new(
                shared::auth::RawToken::presented(crate::http::TEST_ENGINE_TOKEN).hash(),
            ),
            settings: settings.clone(),
            custody: crate::http::Custody {
                backends: key_custody.clone(),
                default_backend: "plain".to_string(),
                wallet_handles: wallet_handles.clone(),
            },
            networks: crate::http::Networks {
                daemons: daemons.clone(),
                scanner_status: status.clone(),
            },
        };
        let router = crate::http::build_router(state, 1 << 20);
        let db = Db::over_shared(store.clone());
        let manager = tokio::spawn(manage_network_loops(
            db,
            Arc::default(),
            key_custody,
            daemons,
            wallet_handles,
            status.clone(),
            settings,
        ));

        let save = |body: serde_json::Value| {
            router.clone().oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/admin/settings")
                    .header("content-type", "application/json")
                    .header(
                        shared::auth::ENGINE_TOKEN_HEADER,
                        crate::http::TEST_ENGINE_TOKEN,
                    )
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
        };
        // Nothing listens on port 9: the loop runs and records failing ticks.
        let node = serde_json::json!({ "host": "127.0.0.1", "port": 9, "ssl": false, "accept_self_signed_certs": true, "fallbacks": [] });
        let saved = save(serde_json::json!({ "monero_node": { "stagenet": node } }))
            .await
            .unwrap();
        assert_eq!(saved.status(), StatusCode::OK);
        eventually("the stagenet scanner to tick", || {
            status.read().contains_key(&Network::Stagenet)
        })
        .await;
        assert!(
            !status.read().contains_key(&Network::Mainnet),
            "only configured networks get loops"
        );

        let cleared = save(serde_json::json!({ "monero_node": { "stagenet": null } }))
            .await
            .unwrap();
        assert_eq!(cleared.status(), StatusCode::OK);
        eventually("the stagenet status to go", || {
            !status.read().contains_key(&Network::Stagenet)
        })
        .await;
        // And it stays gone: the stopped loop doesn't record another tick.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!status.read().contains_key(&Network::Stagenet));
        manager.abort();
    }

    fn material() -> WalletMaterial {
        let scalar = |seed: u8| {
            let mut b = [seed; 32];
            b[31] &= 0x0f;
            b
        };
        let spend = monero::PrivateKey::from_slice(&scalar(2)).unwrap();
        WalletMaterial::new(
            scalar(1),
            monero::PublicKey::from_private_key(&spend).to_bytes(),
        )
    }

    /// With an hour between polls, a round still runs as soon as the node
    /// announces a block (docs/monero_zmq.md).
    #[tokio::test]
    async fn the_node_s_announcements_cut_the_wait_between_polls_short() {
        let defaults = EngineSettings::defaults();
        let scan = crate::engine_settings::ScanConfig {
            poll_interval: Duration::from_secs(3600),
            ..(*defaults.scan.load()).clone()
        };
        let settings = Arc::new(EngineSettings {
            registry: None,
            env: live_settings::Env::fixed(Vec::<(String, String)>::new()),
            nodes: defaults.nodes.clone(),
            scan: live_settings::Live::new(scan),
            webhooks: defaults.webhooks.clone(),
            limits: defaults.limits.clone(),
            tenant_defaults: defaults.tenant_defaults.clone(),
            runtime: defaults.runtime.clone(),
            custody: defaults.custody.clone(),
        });
        let node = Arc::new(FakeDaemonClient::new());
        node.push_block("h1", vec![]);
        let daemons = Daemons::fixed(HashMap::from([(
            Network::Stagenet,
            Arc::new(FallbackDaemonClient::new(vec![FallbackNode {
                label: "fake".to_string(),
                client: node.clone(),
            }])),
        )]));
        let status = scanner_status::new_scanner_status_map();
        let scan_state: Arc<crate::work::ScanState> = Arc::default();
        let scan_loop = tokio::spawn(run_scanner_loop(
            scan_state.clone(),
            Db::over_shared(Store::open_in_memory().unwrap().into_shared()),
            Arc::new(PlainKeyCustody::default()),
            Network::Stagenet,
            daemons,
            Arc::default(),
            status.clone(),
            settings,
        ));
        let ticks = || {
            status
                .read()
                .get(&Network::Stagenet)
                .map_or(0, |s| s.tick_count)
        };
        eventually("the first round", || ticks() == 1).await;
        // Settled into its hour-long wait.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(ticks(), 1);

        node.push_block("h2", vec![]);
        scan_state.node_wakes().chain_changed();
        eventually("a round for the announced block", || ticks() == 2).await;
        scan_loop.abort();
    }

    #[tokio::test]
    async fn a_store_whose_backend_was_replaced_is_registered_again_within_seconds() {
        let plain: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let router = Arc::new(CustodyRouter::new(
            HashMap::from([("plain".to_string(), plain)]),
            "plain",
        ));
        let store = Store::open_in_memory().unwrap();
        let tenant = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: router.seal_in("plain", &material()).await.unwrap(),
                    primary_address: "5x".into(),
                    network: "stagenet".into(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1,
            )
            .unwrap()
            .tenant;
        let store = store.into_shared();
        let node = FakeDaemonClient::new();
        node.push_block("h1", vec![]);
        let daemons = Daemons::fixed(HashMap::from([(
            Network::Stagenet,
            Arc::new(FallbackDaemonClient::new(vec![FallbackNode {
                label: "fake".to_string(),
                client: Arc::new(node),
            }])),
        )]));
        let wallet_handles: Arc<RwLock<HashMap<crate::store::TenantId, WalletHandle>>> =
            Arc::default();
        let key_custody: Arc<dyn KeyCustody> = router.clone();
        let scan_loop = tokio::spawn(run_scanner_loop(
            Arc::default(),
            Db::over_shared(store),
            key_custody,
            Network::Stagenet,
            daemons,
            wallet_handles.clone(),
            scanner_status::new_scanner_status_map(),
            EngineSettings::defaults(),
        ));
        eventually("the store to be registered", || {
            wallet_handles.read().contains_key(&tenant.id)
        })
        .await;
        let first = wallet_handles.read()[&tenant.id];

        // The backend is replaced by a new instance: the old handle is gone.
        let fresh: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        router.replace(
            HashMap::from([("plain".to_string(), fresh.clone())]),
            "plain",
        );
        eventually("the store to be registered in the new instance", || {
            wallet_handles
                .read()
                .get(&tenant.id)
                .is_some_and(|h| *h != first && router.handle_is_live(*h))
        })
        .await;
        scan_loop.abort();
    }
}
