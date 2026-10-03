//! A network's activity record, for monokulo's engine page
//! (`docs/engine_visualizer.md`). The engine token only, like the logs API:
//! monokulo asks, the engine never pushes.
//!
//! - `GET /api/v1/admin/engine/activity?network=stagenet&from=812`: the
//!   events from sequence number `from` on, or, without `from` (or when it
//!   has left the record), everything from the oldest snapshot kept
//!   (`shared::activity::ActivityPage`).
//!
//! Only memory is read: the snapshots in the record are taken by the scan
//! loop, so a page asking every half second costs no database work.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::Json;
use serde::Deserialize;
use shared::activity::{ActivityPage, Tier, Tuning};

use super::{ApiError, AppState};
use crate::work::{millis, ScanTuning};

#[derive(Debug, Deserialize)]
pub(super) struct ActivityQuery {
    network: String,
    from: Option<u64>,
}

pub(super) async fn activity(
    State(state): State<AppState>,
    Query(query): Query<ActivityQuery>,
) -> Result<Json<ActivityPage>, ApiError> {
    let network = shared::network::parse_network(&query.network)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let activity = state
        .networks
        .scanner_status
        .read()
        .get(&network)
        .map(|status| Arc::clone(&status.activity))
        .ok_or(ApiError::NotFound)?;
    let page = activity.page(query.from);
    let scan = state.settings.scan.load();
    // The scanner runs the default tuning (`work::tuning`); only tests and
    // the round length sweep run others.
    let tuning = ScanTuning::DEFAULT;
    Ok(Json(ActivityPage {
        network: shared::network::network_str(network).to_owned(),
        epoch: activity.epoch().to_owned(),
        now_ms: crate::activity::now_ms(),
        tuning: Tuning {
            round_ms: millis(tuning.round_budget),
            shares: Tier::ALL.map(|tier| tuning.shares.percent(tier)),
            group_page: u64::try_from(tuning.group_page).unwrap_or(u64::MAX),
            blocks_per_unit: u64::try_from(tuning.blocks_per_unit).unwrap_or(u64::MAX),
            reorg_check_depth: scan.reorg_check_depth,
            poll_ms: millis(scan.poll_interval),
        },
        events: page.events,
        next: page.next,
        gap: page.gap,
    }))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use shared::activity::Event;
    use tower::ServiceExt as _;

    use super::super::{build_router, TEST_ENGINE_TOKEN};
    use super::*;

    async fn get(state: &AppState, uri: &str) -> (StatusCode, serde_json::Value) {
        let response = build_router(state.clone(), 1_000_000)
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(shared::auth::ENGINE_TOKEN_HEADER, TEST_ENGINE_TOKEN)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
        )
    }

    /// Monokulo reads a network's record from its snapshot, then follows
    /// it with `from`; the scanner's tuning comes with it.
    #[tokio::test]
    async fn a_network_s_record_is_read_from_its_snapshot_then_followed() {
        let state = AppState::for_tests();
        let activity = crate::scanner_status::activity_of(
            &state.networks.scanner_status,
            monero::Network::Stagenet,
        );
        activity.record(Event::ChainChecked {
            agrees: true,
            looked_up: true,
        });
        activity.record(Event::Snapshot(Box::default()));
        activity.record(Event::Vanished { looked: 3 });

        let (status, body) = get(&state, "/api/v1/admin/engine/activity?network=stagenet").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let page: ActivityPage = serde_json::from_value(body).unwrap();
        assert_eq!(page.network, "stagenet");
        assert_eq!(page.epoch, activity.epoch());
        assert_eq!(
            page.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [1, 2],
            "from the snapshot"
        );
        assert_eq!((page.next, page.gap), (3, false));
        assert_eq!(page.tuning.round_ms, 10_000);
        assert_eq!(page.tuning.shares, [20, 40, 15, 20, 5]);
        assert_eq!(page.tuning.group_page, 256);

        activity.record(Event::ReorgCollected);
        let (_, body) = get(
            &state,
            "/api/v1/admin/engine/activity?network=stagenet&from=3",
        )
        .await;
        let next: ActivityPage = serde_json::from_value(body).unwrap();
        assert_eq!(next.events.len(), 1);
        assert_eq!(next.events[0].event, Event::ReorgCollected);

        // A `from` from before a restart: start over, and say so.
        let (_, body) = get(
            &state,
            "/api/v1/admin/engine/activity?network=stagenet&from=99",
        )
        .await;
        let over: ActivityPage = serde_json::from_value(body).unwrap();
        assert!(over.gap);
        assert_eq!(over.events[0].seq, 1);
    }

    /// A network nobody scans has no record, and a name that isn't a
    /// network is refused; neither is answered without the engine token.
    #[tokio::test]
    async fn a_network_without_a_scanner_or_an_unknown_name_is_refused() {
        let state = AppState::for_tests();
        let (status, _) = get(&state, "/api/v1/admin/engine/activity?network=mainnet").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = get(&state, "/api/v1/admin/engine/activity?network=moon").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let unauthenticated = build_router(state, 1_000_000)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/admin/engine/activity?network=mainnet")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    }
}
