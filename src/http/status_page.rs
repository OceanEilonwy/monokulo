//! `GET /status` - a machine-readable (JSON) report of every configured
//! Monero node's live reachability/height and the chain-scanner loop's own
//! recent tick history, across every configured network at once.
//!
//! Deliberately **data only, no HTML** - this used to render a full page
//! directly on the engine, which was the wrong layer for it: the engine has
//! no product-facing visual identity of its own (its only other HTML
//! surface, the checkout page, is per-tenant customizable, not "the site's
//! look"), and the control-plane is the one place a merchant-facing status
//! page actually belongs, styled to match everything else there. The
//! control-plane's own `GET /status` (`control-plane/src/http/status_page.rs`)
//! is the real page now - it calls this endpoint via `EngineClient::get_status`
//! and renders it with the control-plane's own templates.
//!
//! Every node's height is queried live, on every request, the same
//! "the next real call is the health check" philosophy
//! `daemon_fallback`'s own module doc comment states explicitly - this
//! reports what's true right now, not a cached belief about what was true
//! at some earlier point. A per-node timeout keeps one unreachable node
//! from making the whole response hang. Deliberately unauthenticated (no
//! `sk_`/`pk_` involved) and outside the `/api/v1/...`/`/pay/v1/...`
//! version prefixes: this reports on the instance as a whole, not any one
//! tenant, the same way a service's own `/healthz` typically sits outside
//! its versioned API.
//!
//! `is_stale`/`last_tick_ok` are computed here (the engine knows its own
//! `scan_poll_interval_secs`, which "stale" is defined relative to) but
//! human-readable formatting (relative "3s ago" style timestamps, an
//! overall healthy/stale/failing label) is deliberately left to whichever
//! caller renders this for a person - presentation logic belongs in the
//! presentation layer, and there is now exactly one of those (control-plane).

use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};
use monero::Network;
use serde::Serialize;

use crate::network::network_str;

use super::AppState;

const NODE_HEIGHT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize)]
pub struct NodeStatus {
    pub label: String,
    pub is_active: bool,
    pub height: Option<u64>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct ScannerStatusView {
    pub ever_ticked: bool,
    pub last_tick_started_at: Option<i64>,
    pub last_tick_finished_at: Option<i64>,
    pub tick_count: u64,
    pub tenants_scanned: usize,
    pub last_tick_ok: bool,
    pub last_error: Option<String>,
    /// More than 3x `poll_interval_secs` since the last tick finished (or
    /// never ticked at all) - see `is_stale`'s own doc comment for why 3x.
    pub is_stale: bool,
}

#[derive(Serialize)]
pub struct NetworkStatus {
    pub network: String,
    pub nodes: Vec<NodeStatus>,
    pub scanner: ScannerStatusView,
}

#[derive(Serialize)]
pub struct EngineStatusResponse {
    pub networks: Vec<NetworkStatus>,
    pub poll_interval_secs: u64,
    pub generated_at: i64,
}

/// A network's scan loop is considered stale (not just "last tick failed" -
/// genuinely not ticking at all) once it's gone more than 3x its own
/// configured poll interval without a tick - generous enough that normal
/// jitter (a slow node, GC pause, whatever) never falsely reads as stuck,
/// while still catching a genuinely wedged loop well before an operator
/// would otherwise notice only via a merchant complaint (the exact failure
/// mode `main.rs::supervise`'s own doc comment names as the reason that
/// function exists at all).
fn is_stale(now: i64, last_tick_finished_at: i64, poll_interval_secs: u64) -> bool {
    let staleness_threshold = (poll_interval_secs as i64).saturating_mul(3).max(1);
    now.saturating_sub(last_tick_finished_at) > staleness_threshold
}

pub async fn status_page(State(state): State<AppState>) -> Response {
    let now = crate::now_unix();

    let mut networks: Vec<(Network, _)> = state.daemons.iter().map(|(n, d)| (*n, d.clone())).collect();
    networks.sort_by_key(|(network, _)| network_str(*network));

    let mut network_views = Vec::with_capacity(networks.len());
    for (network, daemon) in networks {
        let current_index = daemon.current_index();
        let mut nodes = Vec::with_capacity(daemon.nodes().len());
        for (i, node) in daemon.nodes().iter().enumerate() {
            let (height, error) = match tokio::time::timeout(NODE_HEIGHT_TIMEOUT, node.client.get_height()).await {
                Ok(Ok(h)) => (Some(h), None),
                Ok(Err(e)) => (None, Some(e.to_string())),
                Err(_) => (None, Some(format!("timed out after {}s", NODE_HEIGHT_TIMEOUT.as_secs()))),
            };
            nodes.push(NodeStatus { label: node.label.clone(), is_active: i == current_index, height, error });
        }

        let scan_status = state.scanner_status.read().unwrap().get(&network).cloned();
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
                    is_stale: is_stale(now, finished_at, state.scan_poll_interval_secs),
                }
            }
        };

        network_views.push(NetworkStatus { network: network_str(network).to_string(), nodes, scanner });
    }

    Json(EngineStatusResponse { networks: network_views, poll_interval_secs: state.scan_poll_interval_secs, generated_at: now }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_stale_uses_three_times_the_configured_poll_interval_as_its_threshold() {
        assert!(!is_stale(1000, 995, 2), "5s since the last tick, 2s interval x3 = 6s threshold - not stale yet");
        assert!(is_stale(1000, 990, 2), "10s since the last tick, 2s interval x3 = 6s threshold - genuinely stale");
        assert!(!is_stale(1000, 1000, 0), "a poll_interval of 0 must not make every tick instantly stale");
    }
}
