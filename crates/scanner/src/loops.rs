//! The engine's background loops (tasks 2.1, 7.4, 7.9): one chain scanner
//! and one double-spend revalidation loop per network with a node
//! configured, started and stopped as node settings are saved, and the
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
use crate::network::network_str;
use crate::scanner::revalidate_recent_double_spend_voids;
use crate::scanner_status::{self, ScannerStatusMap};
use crate::store::SharedStore;
use crate::webhook_delivery::run_delivery_tick;

pub async fn run_webhook_delivery_loop(store: SharedStore, settings: Arc<EngineSettings>) {
    // Building the client can only fail if the TLS backend can't initialise.
    // Retry rather than panic, so the supervisor isn't left in a crash loop.
    let client = loop {
        match reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build() {
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
        // `run_delivery_tick` locks the store only around its own brief synchronous
        // sections, never across the outbound HTTP `.await`s it performs per
        // delivery - see its doc comment for why that matters.
        let sent = match run_delivery_tick(
            &store,
            &client,
            config.allow_private_urls,
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
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
}

/// A `'static` name per (loop, network) for `supervise`, which labels logs
/// and restart counts with it.
pub fn loop_name(kind: &'static str, network: Network) -> &'static str {
    match (kind, network) {
        ("chain scanner", Network::Mainnet) => "chain scanner (mainnet)",
        ("chain scanner", Network::Stagenet) => "chain scanner (stagenet)",
        ("chain scanner", Network::Testnet) => "chain scanner (testnet)",
        (_, Network::Mainnet) => "double-spend revalidation (mainnet)",
        (_, Network::Stagenet) => "double-spend revalidation (stagenet)",
        (_, Network::Testnet) => "double-spend revalidation (testnet)",
    }
}

/// Longest one scan tick may run before it is abandoned and the next one
/// starts (task 7.9): a tick stuck on a call that never returns would
/// otherwise stop payment detection on its network without any error. Well
/// above a normal tick, which the per-call deadlines keep short.
pub fn tick_deadline(poll_interval: Duration) -> Duration {
    (poll_interval * 20).max(Duration::from_secs(120))
}

/// Starts a scanner loop and a revalidation loop for each network that has
/// a node configured, and stops them for a network whose node setting is
/// cleared, whenever node settings are saved (task 2.1). Runs for the life of
/// the process.
pub async fn manage_network_loops(
    store: SharedStore,
    key_custody: Arc<dyn KeyCustody>,
    daemons: Daemons,
    wallet_handles: Arc<RwLock<HashMap<String, WalletHandle>>>,
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
            let (revalidation_store, revalidation_daemons) = (store.clone(), daemons.clone());
            supervise_until(loop_name("double-spend revalidation", network), stopped.clone(), move || {
                run_double_spend_revalidation_loop(revalidation_store.clone(), network, revalidation_daemons.clone())
            });
            let (store, key_custody, daemons, wallet_handles, scanner_status, settings) = (
                store.clone(),
                key_custody.clone(),
                daemons.clone(),
                wallet_handles.clone(),
                scanner_status.clone(),
                settings.clone(),
            );
            supervise_until(loop_name("chain scanner", network), stopped, move || {
                run_scanner_loop(
                    store.clone(),
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

/// Runs `run_scan_tick` for one network, over and over. Re-reads
/// `wallet_handles`, the network's node client and the scan settings every
/// round, so new tenants, saved node settings and saved scan settings all
/// apply from the next tick (tasks 2.1, 2.3). Each network has its own
/// loop (task 7.4), so a slow node on one never delays another.
pub async fn run_scanner_loop(
    store: SharedStore,
    key_custody: Arc<dyn KeyCustody>,
    network: Network,
    daemons: Daemons,
    wallet_handles: Arc<RwLock<HashMap<String, WalletHandle>>>,
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
    // Kept across ticks so the mempool is fetched and scanned incrementally
    // (task 7.3). A panic restarts this loop with a fresh one, which only
    // means one full rescan of the pool.
    let mempool_memory = crate::scanner::MempoolMemory::default();
    // Shared by every network's loop, so the handle map is cleared once per
    // lost-state epoch of the key-custody backend, not once per network.
    static HANDLED_CUSTODY_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
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
        let lost_a_handle = wallet_handles.read().values().any(|handle| !key_custody.handle_is_live(*handle));
        let retry_after =
            if lost_a_handle || registrations_failed { REGISTRATION_RETRY_AFTER_LOSS } else { REGISTRATION_RETRY };
        if last_registration_attempt.is_none_or(|at| at.elapsed() >= retry_after) {
            last_registration_attempt = Some(tokio::time::Instant::now());
            let crate::scanner::Registration { registered, failed } = crate::scanner::register_missing_wallets_reporting(
                &store,
                key_custody.as_ref(),
                &wallet_handles,
                Some(&HANDLED_CUSTODY_EPOCH),
                network_str(network),
            )
            .instrument(tick.clone())
            .await;
            registrations_failed = failed > 0;
            if registered > 0 {
                tracing::info!(network = ?network, stores = registered, "registered the keys of stores that had none");
            }
        }
        let tenants: Vec<(String, WalletHandle)> =
            wallet_handles.read().iter().map(|(id, h)| (id.clone(), *h)).collect();
        let started_at = now_unix();
        // One node for the whole tick (task 7.6), so answers from nodes at
        // different heights or on different forks are never mixed.
        let pinned = daemon.pin();
        let result = match tokio::time::timeout(
            tick_deadline(scan.poll_interval),
            crate::scanner::run_scan_tick_with(
                &mempool_memory,
                &store,
                key_custody.as_ref(),
                &pinned,
                network_str(network),
                &tenants,
                scan.reorg_check_depth,
                scan.expired_order_grace_period_seconds,
                scan.scan_chunk_memory_budget_mb,
            )
            .instrument(tick.clone()),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(crate::scanner::ScannerError::Internal(format!(
                "scan tick did not finish within {:?} and was abandoned",
                tick_deadline(scan.poll_interval)
            ))),
        };
        let finished_at = now_unix();
        if let Err(e) = &result {
            shared::throttled!(format!("tick-failed:{network:?}"), warn, network = ?network, error = %e, "scan tick failed");
        }
        // Not for a network whose node setting was cleared during this tick:
        // its status was removed when its loops were stopped.
        if daemons.get(network).is_some() {
            scanner_status::record_tick(&scanner_status, network, started_at, finished_at, tenants.len(), &result);
        }
        tokio::time::sleep(scan.poll_interval).await;
    }
}

/// How often [`revalidate_recent_double_spend_voids`] sweeps each network - much
/// slower than the scan-tick/webhook-delivery loops above, since it exists to catch
/// a rare event (a wrongly-voided payment) within a wide, forgiving window
/// (`scanner::DOUBLE_SPEND_RECHECK_WINDOW_SECS`), not to react quickly. See that
/// function's own doc comment for why this is deliberately not folded into
/// `run_scanner_loop`'s tight per-second cadence.
pub const DOUBLE_SPEND_REVALIDATION_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub async fn run_double_spend_revalidation_loop(store: SharedStore, network: Network, daemons: Daemons) {
    loop {
        if let Some(daemon) = daemons.get(network) {
            match revalidate_recent_double_spend_voids(&store, daemon.as_ref(), network_str(network), now_unix()).await {
                Ok(recovered) if !recovered.is_empty() => {
                    tracing::info!(
                        network = ?network,
                        payments = recovered.len(),
                        orders = ?recovered,
                        "double-spend revalidation reversed previously-voided payments"
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(network = ?network, error = %e, "double-spend revalidation failed"),
            }
        }
        tokio::time::sleep(DOUBLE_SPEND_REVALIDATION_INTERVAL).await;
    }
}

#[cfg(test)]
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

    #[tokio::test]
    async fn saving_a_node_starts_its_networks_loops_and_clearing_it_stops_them() {
        let store = Store::open_in_memory().unwrap().into_shared();
        crate::http::instance_admin::seed_admin_token_for_tests(&store.lock(), "admin_test_token");
        let daemons = Daemons::default();
        let rate_limiter = Arc::new(RateLimiter::new(10_000));
        let settings = EngineSettings::load_with(
            store.clone(),
            Some(crate::engine_settings::NodesReloadable { daemons: daemons.clone(), strict_tls: false }),
            rate_limiter.clone(),
            live_settings::Env::fixed(Vec::<(String, String)>::new()),
        )
        .await
        .unwrap();
        let key_custody: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let wallet_handles: Arc<RwLock<HashMap<String, WalletHandle>>> = Arc::default();
        let status = scanner_status::new_scanner_status_map();
        let state = crate::http::AppState {
            store: store.clone(),
            key_custody: key_custody.clone(),
            key_custody_backend: "plain".to_string(),
            wallet_handles: wallet_handles.clone(),
            admin_rate_limiter: rate_limiter,
            daemons: daemons.clone(),
            scanner_status: status.clone(),
            log_store: None,
            settings: settings.clone(),
        };
        let router = crate::http::build_router(state, 1 << 20);
        let manager = tokio::spawn(manage_network_loops(store, key_custody, daemons, wallet_handles, status.clone(), settings));

        let save = |body: serde_json::Value| {
            router.clone().oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/admin/settings")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer admin_test_token")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
        };
        // Nothing listens on port 9: the loop runs and records failing ticks.
        let node = serde_json::json!({ "host": "127.0.0.1", "port": 9, "ssl": false, "accept_self_signed_certs": true, "fallbacks": [] });
        let saved = save(serde_json::json!({ "monero_node": { "stagenet": node } })).await.unwrap();
        assert_eq!(saved.status(), StatusCode::OK);
        eventually("the stagenet scanner to tick", || status.read().contains_key(&Network::Stagenet)).await;
        assert!(!status.read().contains_key(&Network::Mainnet), "only configured networks get loops");

        let cleared = save(serde_json::json!({ "monero_node": { "stagenet": null } })).await.unwrap();
        assert_eq!(cleared.status(), StatusCode::OK);
        eventually("the stagenet status to go", || !status.read().contains_key(&Network::Stagenet)).await;
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
        WalletMaterial::new(scalar(1), monero::PublicKey::from_private_key(&spend).to_bytes())
    }

    #[tokio::test]
    async fn a_store_whose_backend_was_replaced_is_registered_again_within_seconds() {
        let plain: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let router = Arc::new(CustodyRouter::new(HashMap::from([("plain".to_string(), plain)]), "plain"));
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
            Arc::new(FallbackDaemonClient::new(vec![FallbackNode { label: "fake".to_string(), client: Arc::new(node) }])),
        )]));
        let wallet_handles: Arc<RwLock<HashMap<String, WalletHandle>>> = Arc::default();
        let key_custody: Arc<dyn KeyCustody> = router.clone();
        let scan_loop = tokio::spawn(run_scanner_loop(
            store,
            key_custody,
            Network::Stagenet,
            daemons,
            wallet_handles.clone(),
            scanner_status::new_scanner_status_map(),
            EngineSettings::defaults(),
        ));
        eventually("the store to be registered", || wallet_handles.read().contains_key(&tenant.id)).await;
        let first = wallet_handles.read()[&tenant.id];

        // The backend is replaced by a new instance: the old handle is gone.
        let fresh: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        router.replace(HashMap::from([("plain".to_string(), fresh.clone())]), "plain");
        eventually("the store to be registered in the new instance", || {
            wallet_handles.read().get(&tenant.id).is_some_and(|h| *h != first && router.handle_is_live(*h))
        })
        .await;
        scan_loop.abort();
    }
}
