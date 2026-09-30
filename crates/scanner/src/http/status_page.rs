//! `GET /status` - a machine-readable (JSON) report of every configured
//! Monero node's live reachability/height and the chain-scanner loop's own
//! recent tick history, across every configured network at once.
//!
//! Deliberately **data only, no HTML** - this used to render a full page
//! directly on the engine, which was the wrong layer for it: the engine has
//! no product-facing visual identity or HTML surface of its own at all any
//! more (`docs/fx_refactor.md` moved the checkout page to the monokulo
//! too), and the monokulo is the one place a merchant-facing status
//! page actually belongs, styled to match everything else there. The
//! monokulo's own `GET /status` (`monokulo/src/http/status_page.rs`)
//! is the real page now - it calls this endpoint via `EngineClient::get_status`
//! and renders it with the monokulo's own templates.
//!
//! Every node's height is queried live, on every request, the same
//! "the next real call is the health check" philosophy
//! `daemon_fallback`'s own module doc comment states explicitly - this
//! reports what's true right now, not a cached belief about what was true
//! at some earlier point. A per-node timeout keeps one unreachable node
//! from making the whole response hang. Deliberately unauthenticated (no
//! `sk_`/`pk_` involved) and outside the `/api/v1/...` version prefix:
//! this reports on the instance as a whole, not any one
//! tenant, the same way a service's own `/healthz` typically sits outside
//! its versioned API.
//!
//! `is_stale`/`last_tick_ok` are computed here (the engine knows its own
//! `scan_poll_interval_secs`, which "stale" is defined relative to) but
//! human-readable formatting (relative "3s ago" style timestamps, an
//! overall healthy/stale/failing label) is deliberately left to whichever
//! caller renders this for a person - presentation logic belongs in the
//! presentation layer, and there is now exactly one of those (monokulo).

use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};
use monero::Network;
use serde::Serialize;

use crate::network::network_str;

use super::AppState;

const NODE_HEIGHT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize, Clone)]
pub struct NodeStatus {
    pub label: String,
    pub is_active: bool,
    /// Skipped for now after failing (task 7.6).
    pub in_cooldown: bool,
    pub height: Option<u64>,
    pub error: Option<String>,
    /// The network the node says it's on (`get_info`'s `nettype`:
    /// `"mainnet"`, `"stagenet"`, `"testnet"` or `"fakechain"`), or `None`
    /// when it didn't say. The admin page shows a node on the wrong one.
    pub network: Option<String>,
    /// What the engine has asked this node since the node was configured
    /// (or the engine started), by endpoint, busiest first.
    pub rpc: Vec<crate::daemon::EndpointStats>,
}

/// One node's height, the error if it couldn't be read, and its network.
///
/// `get_info` says both the network and the height, so a node is asked once;
/// only a client whose `get_info` has no height (or a node that failed it) is
/// asked for the height on its own, which is also where the error shown comes
/// from.
async fn probe_node(
    client: &dyn crate::daemon::MoneroDaemonClient,
) -> (Option<u64>, Option<String>, Option<String>) {
    let info = tokio::time::timeout(NODE_HEIGHT_TIMEOUT, client.get_info()).await;
    let (network, height) = match info {
        Ok(Ok(info)) => (
            Some(info.nettype).filter(|nettype| nettype != crate::daemon::DaemonInfo::UNKNOWN),
            info.height,
        ),
        _ => (None, None),
    };
    if height.is_some() {
        return (height, None, network);
    }
    match tokio::time::timeout(NODE_HEIGHT_TIMEOUT, client.get_height()).await {
        Ok(Ok(height)) => (Some(height), None, network),
        Ok(Err(e)) => (None, Some(e.to_string()), network),
        Err(_) => (
            None,
            Some(format!(
                "timed out after {}s",
                NODE_HEIGHT_TIMEOUT.as_secs()
            )),
            network,
        ),
    }
}

#[derive(Serialize, Clone)]
pub struct ScannerStatusView {
    pub ever_ticked: bool,
    pub last_tick_started_at: Option<i64>,
    pub last_tick_finished_at: Option<i64>,
    pub tick_count: u64,
    pub tenants_scanned: usize,
    pub last_tick_ok: bool,
    pub last_error: Option<String>,
    /// More than `is_stale`'s own threshold since the last tick finished
    /// (or never ticked at all) - see that function's doc comment for the
    /// real formula and why.
    pub is_stale: bool,
}

#[derive(Serialize, Clone)]
pub struct NetworkStatus {
    pub network: String,
    pub nodes: Vec<NodeStatus>,
    pub scanner: ScannerStatusView,
    /// Tenants not yet scanned up to this network's latest scanned block
    /// (task 5.0): their payments may be detected late until they catch up.
    pub lagging_tenants: usize,
    /// How far behind the furthest-behind tenant is, in blocks.
    pub max_blocks_behind: u64,
}

#[derive(Serialize)]
pub struct EngineStatusResponse {
    pub networks: Vec<NetworkStatus>,
    pub poll_interval_secs: u64,
    pub generated_at: i64,
    /// How many times each background loop has been restarted after a panic
    /// or an unexpected return since the engine started (task 7.9). Empty
    /// while nothing has failed.
    pub loop_restarts: Vec<LoopRestarts>,
    /// Webhook deliveries due and not yet sent (task 7.13).
    pub webhook_backlog: WebhookBacklog,
    /// Every store that can't be scanned right now, and why (task 3.7):
    /// monokulo shows each one's owner an alert.
    pub unserved_tenants: Vec<UnservedTenant>,
    /// Each enabled key custody backend and whether it answers (task 5.5).
    pub key_custody: Vec<CustodyBackendStatus>,
    /// The backend a new store's keys go to unless it asks for another;
    /// `None` when there's no choice (a single backend).
    pub key_custody_default: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct CustodyBackendStatus {
    pub backend: String,
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct UnservedTenant {
    pub public_key: String,
    pub network: String,
    /// `"no_reachable_node"` (its network has no node configured, or none
    /// answers), `"catching_up"` (it fell behind and is being caught up),
    /// `"custody_disabled"` (its keys are in a key custody backend that is
    /// turned off) or `"custody_unavailable"` (that backend doesn't answer).
    pub reason: &'static str,
    /// For `catching_up`: how many blocks behind.
    pub blocks_behind: Option<u64>,
}

#[derive(Serialize)]
pub struct WebhookBacklog {
    pub due: u64,
    /// How long the oldest due delivery has been waiting, in seconds.
    pub oldest_waiting_secs: Option<i64>,
}

#[derive(Serialize)]
pub struct LoopRestarts {
    pub name: &'static str,
    pub restarts: u64,
}

/// A network's scan loop is considered stale (not just "last tick failed" -
/// genuinely not ticking at all) once it's gone more than
/// `max(5x poll_interval_secs, 15s)` without a tick.
///
/// **Real bug this threshold fixes**: the original formula (3x
/// `poll_interval_secs`, no floor) only accounts for the sleep *between*
/// ticks, not how long a tick itself actually takes - and a real tick
/// against a real, live public node over the real internet routinely takes
/// several seconds (`daemon_fallback`'s own per-request round trip, not
/// something this loop controls). Against this repo's own dev config
/// (`poll_interval_secs = 2`, so a 3x threshold of 6s), a perfectly healthy
/// scanner ticking every ~7-8s in practice - confirmed directly against the
/// real running engine, not a guess - read as "stale" on every other tick,
/// which is exactly the status-page flicker (healthy -> stale -> healthy)
/// a real user reported. The 5x multiplier plus a 15s floor gives real,
/// live-node tick durations comfortable headroom regardless of how short
/// the configured interval is, while still catching a genuinely wedged
/// loop well before an operator would otherwise notice only via a merchant
/// complaint (the exact failure mode `main.rs::supervise`'s own doc comment
/// names as the reason that function exists at all).
fn is_stale(now: i64, last_tick_finished_at: i64, poll_interval_secs: u64) -> bool {
    let staleness_threshold = (poll_interval_secs as i64).saturating_mul(5).max(15);
    now.saturating_sub(last_tick_finished_at) > staleness_threshold
}

pub async fn status_page(State(state): State<AppState>) -> Response {
    let now = crate::now_unix();

    let mut networks: Vec<(Network, _)> = state
        .networks
        .daemons
        .snapshot()
        .iter()
        .map(|(n, d)| (*n, d.clone()))
        .collect();
    let poll_interval_secs = state.settings.scan.load().poll_interval.as_secs();
    networks.sort_by_key(|(network, _)| network_str(*network));

    let mut network_views = Vec::with_capacity(networks.len());
    for (network, daemon) in networks {
        let current_index = daemon.current_index();
        // Every node at once: the page waits for the slowest node, not for
        // each in turn.
        let probes = futures_util::future::join_all(
            daemon
                .nodes()
                .iter()
                .map(|node| probe_node(node.client.as_ref())),
        )
        .await;
        let nodes: Vec<NodeStatus> = daemon
            .nodes()
            .iter()
            .zip(probes)
            .enumerate()
            .map(|(i, (node, (height, error, network)))| NodeStatus {
                label: node.label.clone(),
                is_active: i == current_index,
                in_cooldown: daemon.in_cooldown(i),
                height,
                error,
                network,
                rpc: node.client.rpc_stats(),
            })
            .collect();

        let scan_status = state.networks.scanner_status.read().get(&network).cloned();
        let scanner = match scan_status {
            None => ScannerStatusView {
                ever_ticked: false,
                last_tick_started_at: None,
                last_tick_finished_at: None,
                tick_count: 0,
                tenants_scanned: 0,
                last_tick_ok: false,
                last_error: None,
                is_stale: true,
            },
            Some(s) => {
                let finished_at = s.last_tick_finished_at.unwrap_or(now);
                ScannerStatusView {
                    ever_ticked: true,
                    last_tick_started_at: s.last_tick_started_at,
                    last_tick_finished_at: s.last_tick_finished_at,
                    tick_count: s.tick_count,
                    tenants_scanned: s.tenants_scanned,
                    last_tick_ok: s.last_tick_ok,
                    last_error: s.last_error,
                    is_stale: is_stale(now, finished_at, poll_interval_secs),
                }
            }
        };

        let (lagging_tenants, max_blocks_behind) = state
            .db
            .read(move |store| {
                let high_water = store.max_scanned_height(network)?.unwrap_or(0);
                let lagging = store.lagging_tenants(network)?;
                let behind = lagging
                    .iter()
                    .map(|(_, cursor)| high_water.saturating_sub(*cursor))
                    .max()
                    .unwrap_or(0);
                Ok((lagging.len(), behind))
            })
            .await
            .unwrap_or((0, 0));
        network_views.push(NetworkStatus {
            network: network_str(network).to_string(),
            nodes,
            scanner,
            lagging_tenants,
            max_blocks_behind,
        });
    }

    let loop_restarts = shared::supervise::restart_counts()
        .into_iter()
        .map(|(name, restarts)| LoopRestarts { name, restarts })
        .collect();
    let (due, oldest) = state
        .db
        .read(move |store| store.webhook_backlog(now))
        .await
        .unwrap_or((0, None));
    let key_custody: Vec<CustodyBackendStatus> = state
        .custody
        .backends
        .backend_health()
        .await
        .into_iter()
        .map(|(backend, error)| CustodyBackendStatus { backend, error })
        .collect();
    let networks_for_read = network_views.clone();
    let mut unserved_tenants = state
        .db
        .read(move |store| Ok(unserved_tenants(store, &networks_for_read)))
        .await
        .unwrap_or_default();
    let key_custody_for_read = key_custody.clone();
    unserved_tenants.extend(
        state
            .db
            .read(move |store| Ok(custody_unserved_tenants(store, &key_custody_for_read)))
            .await
            .unwrap_or_default(),
    );
    Json(EngineStatusResponse {
        networks: network_views,
        poll_interval_secs,
        generated_at: now,
        loop_restarts,
        webhook_backlog: WebhookBacklog {
            due,
            oldest_waiting_secs: oldest.map(|at| now - at),
        },
        unserved_tenants,
        key_custody_default: (!key_custody.is_empty())
            .then(|| state.settings.custody.load().default.as_str().to_string()),
        key_custody,
    })
    .into_response()
}

/// Stores whose keys are in a backend that is turned off or not answering
/// (task 5.5). Only with a router: a single backend has no per-store choice.
fn custody_unserved_tenants(
    store: &crate::store::Store,
    health: &[CustodyBackendStatus],
) -> Vec<UnservedTenant> {
    if health.is_empty() {
        return Vec::new();
    }
    let tenants = store.tenant_custody_backends().unwrap_or_default();
    tenants
        .into_iter()
        .filter_map(|(public_key, network, backend)| {
            let reason = match health.iter().find(|h| h.backend == backend) {
                None => "custody_disabled",
                Some(h) if h.error.is_some() => "custody_unavailable",
                Some(_) => return None,
            };
            Some(UnservedTenant {
                public_key,
                network,
                reason,
                blocks_behind: None,
            })
        })
        .collect()
}

/// How far behind a store must be before it's reported as catching up: a
/// block or two behind is normal while a tick is part-way through.
pub const CATCHING_UP_REPORT_BLOCKS: u64 = 3;

/// Whether a network's stores are cut off: no node answered this probe,
/// and the scan loop isn't succeeding either (its last tick failed, it's
/// stale, or it never ran). A probe that fails once while scanning works
/// is a blip, not something to alert every store owner about.
fn network_unreachable(status: Option<&NetworkStatus>) -> bool {
    let Some(status) = status else { return true };
    let probe_failed = !status.nodes.iter().any(|node| node.error.is_none());
    let scanning_works =
        status.scanner.ever_ticked && status.scanner.last_tick_ok && !status.scanner.is_stale;
    probe_failed && !scanning_works
}

/// Stores that can't be scanned right now (task 3.7): those on a network
/// with no node configured or none answering, and those still catching up
/// after falling behind by more than a couple of blocks.
fn unserved_tenants(
    store: &crate::store::Store,
    networks: &[NetworkStatus],
) -> Vec<UnservedTenant> {
    let mut unserved = Vec::new();
    let with_tenants = store.count_tenants_by_network().unwrap_or_default();
    for (network, count) in with_tenants {
        // A name no network has is a corrupted row: nothing to report on.
        let Ok(parsed) = crate::network::parse_network(&network) else {
            continue;
        };
        if count == 0 {
            continue;
        }
        if network_unreachable(networks.iter().find(|n| n.network == network)) {
            for public_key in store
                .tenant_public_keys_on_network(parsed)
                .unwrap_or_default()
            {
                unserved.push(UnservedTenant {
                    public_key,
                    network: network.clone(),
                    reason: "no_reachable_node",
                    blocks_behind: None,
                });
            }
            continue;
        }
        let high_water = store.max_scanned_height(parsed).ok().flatten().unwrap_or(0);
        for (public_key, cursor) in store.lagging_tenant_keys(parsed).unwrap_or_default() {
            if high_water.saturating_sub(cursor) < CATCHING_UP_REPORT_BLOCKS {
                continue;
            }
            unserved.push(UnservedTenant {
                public_key,
                network: network.clone(),
                reason: "catching_up",
                blocks_behind: Some(high_water.saturating_sub(cursor)),
            });
        }
    }
    unserved
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn network(probe_ok: bool, last_tick_ok: bool, is_stale: bool) -> NetworkStatus {
        NetworkStatus {
            network: "stagenet".to_string(),
            nodes: vec![NodeStatus {
                label: "node".to_string(),
                is_active: true,
                in_cooldown: false,
                height: probe_ok.then_some(100),
                error: (!probe_ok).then(|| "timed out".to_string()),
                network: None,
                rpc: Vec::new(),
            }],
            scanner: ScannerStatusView {
                ever_ticked: true,
                last_tick_started_at: Some(0),
                last_tick_finished_at: Some(0),
                tick_count: 1,
                tenants_scanned: 1,
                last_tick_ok,
                last_error: None,
                is_stale,
            },
            lagging_tenants: 0,
            max_blocks_behind: 0,
        }
    }

    #[test]
    fn one_failed_probe_while_scanning_works_is_not_reported_as_unreachable() {
        assert!(
            !network_unreachable(Some(&network(false, true, false))),
            "a blip"
        );
        assert!(
            !network_unreachable(Some(&network(true, false, false))),
            "the probe answers"
        );
        assert!(
            network_unreachable(Some(&network(false, false, false))),
            "probe and scan both failing"
        );
        assert!(
            network_unreachable(Some(&network(false, true, true))),
            "the scan loop has stopped ticking"
        );
        assert!(network_unreachable(None), "no node configured");
    }

    #[test]
    fn is_stale_uses_five_times_the_configured_poll_interval_with_a_15s_floor() {
        // 2s interval -> 5x = 10s, below the 15s floor, so the floor wins.
        assert!(
            !is_stale(1000, 990, 2),
            "10s since the last tick, below the 15s floor - not stale yet"
        );
        assert!(
            is_stale(1000, 984, 2),
            "16s since the last tick, above the 15s floor - genuinely stale"
        );
        // A large enough interval that 5x actually exceeds the floor.
        assert!(
            !is_stale(1000, 970, 10),
            "30s since the last tick, 10s interval x5 = 50s threshold - not stale yet"
        );
        assert!(
            is_stale(1000, 940, 10),
            "60s since the last tick, 10s interval x5 = 50s threshold - genuinely stale"
        );
        assert!(
            !is_stale(1000, 1000, 0),
            "a poll_interval of 0 must still get the 15s floor, not read as instantly stale"
        );
    }

    /// A node for `probe_node`: `get_info` says (or doesn't) its height and
    /// network, and every request for the height alone is counted.
    struct ProbedNode {
        info: Option<crate::daemon::DaemonInfo>,
        height: Option<u64>,
        height_requests: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl crate::daemon::MoneroDaemonClient for ProbedNode {
        async fn get_info(&self) -> Result<crate::daemon::DaemonInfo, crate::daemon::DaemonError> {
            self.info
                .clone()
                .ok_or_else(|| crate::daemon::DaemonError::Request("no info".into()))
        }
        async fn get_height(&self) -> Result<u64, crate::daemon::DaemonError> {
            self.height_requests
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.height
                .ok_or_else(|| crate::daemon::DaemonError::Request("connection refused".into()))
        }
        async fn get_block_hash(&self, _: u64) -> Result<String, crate::daemon::DaemonError> {
            unimplemented!("not probed")
        }
        async fn get_chain_blocks(
            &self,
            _start_height: u64,
            _count: u64,
        ) -> Result<Vec<crate::daemon::ChainBlock>, crate::daemon::DaemonError> {
            unimplemented!("not probed")
        }
        async fn get_mempool_txids(&self) -> Result<Vec<String>, crate::daemon::DaemonError> {
            unimplemented!("not probed")
        }
        async fn get_transactions_with_ids(
            &self,
            _txids: &[String],
        ) -> Result<Vec<crate::daemon::FetchedTx>, crate::daemon::DaemonError> {
            unimplemented!("not probed")
        }
        async fn locate_transaction(
            &self,
            _: &str,
        ) -> Result<crate::daemon::TxLocation, crate::daemon::DaemonError> {
            unimplemented!("not probed")
        }
        async fn is_key_image_spent(
            &self,
            _: &[String],
        ) -> Result<Vec<crate::daemon::KeyImageStatus>, crate::daemon::DaemonError> {
            unimplemented!("not probed")
        }
    }

    /// A node whose `get_info` says its height is asked once, not twice; one
    /// whose `get_info` doesn't (or fails) is asked for the height, and that
    /// request's failure is the error shown.
    #[tokio::test]
    async fn a_node_is_asked_once_when_its_info_says_its_height() {
        let node = |info: Option<(&str, Option<u64>)>, height| ProbedNode {
            info: info.map(|(nettype, height)| crate::daemon::DaemonInfo {
                nettype: nettype.to_string(),
                height,
            }),
            height,
            height_requests: Default::default(),
        };
        let asked = |node: &ProbedNode| {
            node.height_requests
                .load(std::sync::atomic::Ordering::Relaxed)
        };

        let says_both = node(Some(("stagenet", Some(7))), Some(99));
        assert_eq!(
            probe_node(&says_both).await,
            (Some(7), None, Some("stagenet".to_string()))
        );
        assert_eq!(asked(&says_both), 0);

        let no_height = node(Some(("unknown", None)), Some(9));
        assert_eq!(probe_node(&no_height).await, (Some(9), None, None));
        assert_eq!(asked(&no_height), 1);

        let down = node(None, None);
        let (height, error, network) = probe_node(&down).await;
        assert_eq!((height, network), (None, None));
        assert!(error.unwrap().contains("connection refused"));
        assert_eq!(asked(&down), 1);
    }

    /// The real bug this whole formula change fixes, reproduced directly:
    /// this repo's own dev config (`poll_interval_secs = 2`) with a real,
    /// observed ~7-8s tick cadence against live remote nodes must never
    /// read as stale under the new threshold, even though it did under the
    /// old 3x-with-no-floor one.
    #[test]
    fn a_real_observed_slow_tick_cadence_against_the_dev_config_no_longer_reads_as_stale() {
        assert!(
            !is_stale(1008, 1000, 2),
            "8s since the last tick at a 2s configured interval must not be stale"
        );
    }
}
