//! Live, in-memory status of the chain-scanner background loop
//! (`main.rs::run_scanner_loop`) - for the status page (`http/status_page.rs`)
//! to answer "is scanning actually happening, and did the last tick
//! succeed" without inventing a second monitoring system. Purely
//! observational: nothing here feeds back into scanning behavior itself,
//! and losing it (a restart) loses only history, never anything the
//! scanner's own correctness depends on - the same relationship
//! `daemon_fallback`'s own `eprintln!` diagnostics have to real behavior,
//! just queryable instead of log-only.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use monero::Network;

/// One network's most recent scan-tick outcome. `Default` (all `None`/zero)
/// is the real, honest state before this network has ever ticked even
/// once - the status page renders that as "never ticked yet" rather than
/// guessing.
#[derive(Debug, Clone, Default)]
pub struct NetworkScanStatus {
    pub last_tick_started_at: Option<i64>,
    pub last_tick_finished_at: Option<i64>,
    pub last_tick_ok: bool,
    /// `Display` of the `ScannerError` the last tick failed with, if it did -
    /// a plain `String` rather than the real error type since this crosses
    /// into an HTTP view model, the same boundary `ApiError` itself draws.
    pub last_error: Option<String>,
    pub tick_count: u64,
    /// How many tenants `run_scan_tick` was actually asked to scan on the
    /// most recent tick - lets the status page show "0 tenants" as the
    /// (unremarkable) reason a network with real nodes still never
    /// matches anything, rather than that being indistinguishable from a
    /// stuck scanner.
    pub tenants_scanned: usize,
}

pub type ScannerStatusMap = Arc<RwLock<HashMap<Network, NetworkScanStatus>>>;

pub fn new_scanner_status_map() -> ScannerStatusMap {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Records one network's tick outcome - called by `run_scanner_loop` after
/// every real `run_scan_tick` call, success or failure alike.
pub fn record_tick(
    map: &ScannerStatusMap,
    network: Network,
    started_at: i64,
    finished_at: i64,
    tenants_scanned: usize,
    result: &Result<(), impl std::fmt::Display>,
) {
    let mut guard = map.write().unwrap();
    let entry = guard.entry(network).or_default();
    entry.last_tick_started_at = Some(started_at);
    entry.last_tick_finished_at = Some(finished_at);
    entry.tick_count += 1;
    entry.tenants_scanned = tenants_scanned;
    match result {
        Ok(()) => {
            entry.last_tick_ok = true;
            entry.last_error = None;
        }
        Err(e) => {
            entry.last_tick_ok = false;
            entry.last_error = Some(e.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_network_that_has_never_ticked_reports_the_real_absence_of_history() {
        let map = new_scanner_status_map();
        assert!(map.read().unwrap().get(&Network::Stagenet).is_none());
    }

    #[test]
    fn record_tick_tracks_success_then_failure_then_success_correctly() {
        let map = new_scanner_status_map();

        record_tick(&map, Network::Stagenet, 1000, 1001, 3, &Ok::<(), String>(()));
        {
            let guard = map.read().unwrap();
            let status = guard.get(&Network::Stagenet).unwrap();
            assert_eq!(status.tick_count, 1);
            assert!(status.last_tick_ok);
            assert!(status.last_error.is_none());
            assert_eq!(status.tenants_scanned, 3);
            assert_eq!(status.last_tick_started_at, Some(1000));
            assert_eq!(status.last_tick_finished_at, Some(1001));
        }

        record_tick(&map, Network::Stagenet, 1010, 1012, 3, &Err("node unreachable".to_string()));
        {
            let guard = map.read().unwrap();
            let status = guard.get(&Network::Stagenet).unwrap();
            assert_eq!(status.tick_count, 2, "a failed tick still counts as a tick");
            assert!(!status.last_tick_ok);
            assert_eq!(status.last_error.as_deref(), Some("node unreachable"));
        }

        record_tick(&map, Network::Stagenet, 1020, 1021, 5, &Ok::<(), String>(()));
        let guard = map.read().unwrap();
        let status = guard.get(&Network::Stagenet).unwrap();
        assert_eq!(status.tick_count, 3);
        assert!(status.last_tick_ok, "a later success must clear the earlier failure");
        assert!(status.last_error.is_none(), "the stale error must not linger past a real success");
        assert_eq!(status.tenants_scanned, 5);
    }

    #[test]
    fn different_networks_are_tracked_independently() {
        let map = new_scanner_status_map();
        record_tick(&map, Network::Mainnet, 1000, 1001, 1, &Ok::<(), String>(()));
        record_tick(&map, Network::Stagenet, 2000, 2005, 2, &Err("down".to_string()));

        let guard = map.read().unwrap();
        assert!(guard.get(&Network::Mainnet).unwrap().last_tick_ok);
        assert!(!guard.get(&Network::Stagenet).unwrap().last_tick_ok);
        assert!(guard.get(&Network::Testnet).is_none());
    }
}
