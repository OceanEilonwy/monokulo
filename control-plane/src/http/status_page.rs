//! `GET /status` - the real, styled operator status page: every configured
//! Monero node's live reachability/height and the chain-scanner loop's own
//! recent tick history, across every network the engine is configured for.
//!
//! This used to live directly on the engine (`src/http/status_page.rs` at
//! the repo root) - the wrong layer, since the engine has no product-facing
//! visual identity of its own. The engine's `/status` is now a plain JSON
//! API (`EngineClient::get_status`); this module is the one place that data
//! gets turned into something a person actually reads, styled to match
//! every other control-plane page.
//!
//! Deliberately unauthenticated, same as the engine's own `/status` - it
//! reports on infrastructure health, not any one merchant's private data,
//! and the nav bar's glowing status indicator (`GET /status/summary`, also
//! this module) needs to be checkable from any page, logged in or not.
//!
//! If the engine itself can't be reached at all, that's shown as a plain
//! error banner rather than a 500 or a fabricated "everything's fine" page -
//! the engine being unreachable is itself the most important fact this page
//! can report.
//!
//! **A real incident, and why [`StatusCache`] exists**: the nav bar's status
//! dot (`GET /status/summary`) fires on *every* page load, for every
//! visitor - which turned out to mean one real person just browsing the
//! dashboard could, by itself, exceed the engine's own per-IP rate limit for
//! `/status` (control-plane is the engine's one caller, so every browser's
//! traffic arrives from the same source IP - see `http::rate_limit`'s own
//! module doc comment, which this incident is also what motivated). A short,
//! shared, in-memory TTL cache means every viewer within the TTL window gets
//! one real engine request between them, not one each - the fix that
//! actually addresses the request *volume*, independent of whichever engine-
//! side limit is configured.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::response::{Html, IntoResponse, Json, Response};
use serde_json::json;

use crate::engine_client::{EngineClientError, EngineStatusResponse, NetworkStatus};
use crate::templates::{StatusNetworkView, StatusNodeView, StatusPageViewModel, StatusScannerView};

use super::AppState;

/// How long a fetched status is trusted before the next request triggers a
/// fresh one. Short enough that the page (which also has its own 30s meta
/// refresh) never looks stale to a person watching it; long enough that a
/// burst of page loads - many browser tabs, many users, the nav dot firing
/// on every one - costs the engine one real request, not one per view.
const CACHE_TTL: Duration = Duration::from_secs(10);

pub struct CachedStatus {
    fetched_at: Instant,
    // `String`, not `EngineClientError` - the cached value has to be
    // `Clone` to hand out without holding the lock across an `.await`, and
    // `reqwest::Error` (inside `EngineClientError::Request`) isn't.
    result: Result<EngineStatusResponse, String>,
}

pub type StatusCache = Arc<Mutex<Option<CachedStatus>>>;

pub fn new_status_cache() -> StatusCache {
    Arc::new(Mutex::new(None))
}

/// Returns the cached engine status if it's still fresh, otherwise fetches a
/// real one and caches it before returning. The lock is only ever held for
/// the plain read/write, never across the `.await` itself - two requests
/// racing past a just-expired cache both fetch and both cache, which is
/// simpler than a mutex-held-across-await or a dedicated refresh task, and
/// "occasionally two real fetches instead of one" is a fine outcome for what
/// this exists to bound (typical page-view volume, not a flood).
async fn get_status_cached(state: &AppState) -> Result<EngineStatusResponse, String> {
    if let Some(cached) = state.status_cache.lock().unwrap().as_ref() {
        if cached.fetched_at.elapsed() < CACHE_TTL {
            return cached.result.clone();
        }
    }
    let result = state.engine_client.get_status().await.map_err(|e| describe_engine_error(&e));
    *state.status_cache.lock().unwrap() = Some(CachedStatus { fetched_at: Instant::now(), result: result.clone() });
    result
}

/// `GET /status` - the full page. Unauthenticated, so `logged_in` is a real
/// per-request check (see `StatusPageViewModel::logged_in`'s own doc
/// comment), not a fixed literal like most other pages.
pub async fn status_page(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Response {
    let mut view_model = match get_status_cached(&state).await {
        Ok(status) => build_view_model(status),
        Err(message) => StatusPageViewModel { engine_error: Some(message), ..Default::default() },
    };
    view_model.logged_in = super::resolve_authed_user(&state, &headers).is_some();
    let html = state.templates.render_status(&view_model).expect("the built-in status template must always render");
    Html(html).into_response()
}

/// `GET /status/summary` - a small, cheap JSON endpoint the nav bar's status
/// dot fetches on every page load to decide its color/glow, without every
/// page having to pull in the full status page's own engine round trip.
/// `healthy: false` covers both "the engine is unreachable" and "the engine
/// answered but reports a real problem" - the nav dot only ever needs to
/// distinguish "everything's fine" from "go look", the detail lives on the
/// full page.
pub async fn status_summary(State(state): State<AppState>) -> Response {
    match get_status_cached(&state).await {
        Ok(status) => {
            // `Iterator::all` is vacuously true on an empty list - an engine
            // reporting zero configured networks is not "everything's fine",
            // it's nothing to be fine *about*, so that case is excluded
            // explicitly rather than trusted to fall out of `all` on its own.
            let healthy = !status.networks.is_empty()
                && status
                    .networks
                    .iter()
                    .all(|n| n.nodes.iter().any(|node| node.error.is_none()) && !n.scanner.is_stale && n.scanner.last_tick_ok);
            Json(json!({ "healthy": healthy })).into_response()
        }
        Err(_) => Json(json!({ "healthy": false })).into_response(),
    }
}

fn describe_engine_error(err: &EngineClientError) -> String {
    match err {
        EngineClientError::Request(_) | EngineClientError::Middleware(_) => "the engine could not be reached".to_string(),
        EngineClientError::EngineError { status, .. } => format!("the engine responded with an error ({status})"),
    }
}

fn build_view_model(status: EngineStatusResponse) -> StatusPageViewModel {
    let now = crate::now_unix();
    let networks = status.networks.into_iter().map(|n| build_network_view(n, now)).collect();
    StatusPageViewModel {
        engine_error: None,
        networks,
        poll_interval_secs: status.poll_interval_secs,
        generated_at_display: relative_time(now, status.generated_at),
        // Overwritten by `status_page`'s own real per-request check right
        // after this returns - a placeholder here, never the value shown.
        logged_in: false,
    }
}

fn build_network_view(network: NetworkStatus, now: i64) -> StatusNetworkView {
    let nodes = network
        .nodes
        .into_iter()
        .map(|node| StatusNodeView {
            label: node.label,
            is_active: node.is_active,
            is_reachable: node.error.is_none(),
            height_display: node.height.map(|h| h.to_string()).unwrap_or_else(|| "-".to_string()),
            error: node.error,
        })
        .collect();

    let scanner = network.scanner;
    let (status_label, status_tag_class) = if !scanner.ever_ticked {
        ("has not been scanned yet", "tag-unknown")
    } else if scanner.is_stale {
        ("stale", "tag-error")
    } else if !scanner.last_tick_ok {
        ("tick failing", "tag-error")
    } else {
        ("healthy", "tag-ok")
    };

    StatusNetworkView {
        network: network.network,
        nodes,
        scanner: StatusScannerView {
            ever_ticked: scanner.ever_ticked,
            status_label: status_label.to_string(),
            status_tag_class: status_tag_class.to_string(),
            last_tick_display: scanner
                .last_tick_finished_at
                .map(|t| relative_time(now, t))
                .unwrap_or_else(|| "never".to_string()),
            tick_count: scanner.tick_count,
            tenants_scanned: scanner.tenants_scanned,
            last_error: scanner.last_error,
        },
    }
}

/// A plain, four-bucket "Ns/Nm/Nh/Nd ago" formatter - the same
/// presentation-belongs-to-the-renderer split `http/status_page.rs`'s
/// module doc comment describes, now actually living in the renderer.
/// Buckets are half-open on their upper bound (`< 60`, `< 3600`, `< 86400`)
/// so a value exactly on a boundary (e.g. precisely 3600s) falls into the
/// *next* bucket up, never double-counted or skipped.
fn relative_time(now: i64, at: i64) -> String {
    let diff = (now - at).max(0);
    if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_picks_the_right_bucket() {
        assert_eq!(relative_time(100, 100), "0s ago");
        assert_eq!(relative_time(159, 100), "59s ago");
        assert_eq!(relative_time(160, 100), "1m ago");
        assert_eq!(relative_time(3700, 100), "1h ago");
        assert_eq!(relative_time(3600, 0), "1h ago");
        assert_eq!(relative_time(100_000, 0), "1d ago");
        assert_eq!(relative_time(90_000, 0), "1d ago");
        assert_eq!(relative_time(20_000, 0), "5h ago");
    }

    #[test]
    fn relative_time_never_goes_negative_for_a_clock_that_moved_backward() {
        assert_eq!(relative_time(100, 200), "0s ago");
    }

    mod http_tests {
        use axum::Router;
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        use crate::db::Db;
        use crate::engine_client::EngineClient;
        use crate::http::{AppState, build_router};

        use super::super::{get_status_cached, new_status_cache};

        const TEST_ENCRYPTION_KEY: [u8; 32] = [7u8; 32];

        /// See `AppState`'s own doc comment on `exchange_rate`.
        fn test_exchange_rate_provider() -> std::sync::Arc<crate::exchange_rate_config::ExchangeRateProviders> {
            std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::xmr_only())
        }

        /// The real fix this cache exists for (see the module's own doc
        /// comment on the incident): a second request arriving within the
        /// TTL must reuse the first's real fetch rather than making its own
        /// - proven by reading the cache's own `fetched_at` back rather than
        /// just checking both calls "look the same" (which a coincidental
        /// same-second real refetch could also produce).
        #[tokio::test]
        async fn get_status_cached_reuses_a_fresh_fetch_instead_of_refetching() {
            let engine = engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
            let state = state_with_engine(EngineClient::new(format!("http://{}", engine.addr)));

            let first = get_status_cached(&state).await.expect("first fetch should succeed");
            let fetched_at_after_first = state.status_cache.lock().unwrap().as_ref().unwrap().fetched_at;

            let second = get_status_cached(&state).await.expect("second fetch should succeed");
            let fetched_at_after_second = state.status_cache.lock().unwrap().as_ref().unwrap().fetched_at;

            assert_eq!(
                fetched_at_after_first, fetched_at_after_second,
                "a second call within the TTL must reuse the cached fetch, not trigger a new one"
            );
            assert_eq!(first.generated_at, second.generated_at, "a reused cache entry must hand back the exact same response");
        }

        fn state_with_engine(engine_client: EngineClient) -> AppState {
            AppState {
                db: Db::open_in_memory().unwrap().into_shared(),
                engine_client,
                encryption_key: TEST_ENCRYPTION_KEY,
                templates: std::sync::Arc::new(crate::templates::TemplateEngine::new().unwrap()),
                status_cache: new_status_cache(),
                exchange_rate: test_exchange_rate_provider(),
                rate_limiter: std::sync::Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
            }
        }

        async fn body_text(response: axum::response::Response) -> String {
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            String::from_utf8(bytes.to_vec()).unwrap()
        }

        async fn body_json(response: axum::response::Response) -> serde_json::Value {
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice(&bytes).unwrap()
        }

        /// Real engine, no daemons configured (`engine_test_support`'s harness
        /// never populates them - see `get_status_round_trips_against_a_real_engine`'s
        /// own doc comment) - proves the page renders the honest "no nodes
        /// configured" state end to end, not a fabricated one.
        #[tokio::test]
        async fn status_page_is_reachable_with_no_authentication_and_shows_no_configured_networks() {
            let engine = engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
            let state = state_with_engine(EngineClient::new(format!("http://{}", engine.addr)));
            let router: Router = build_router(state);

            let response =
                router.oneshot(Request::builder().method("GET").uri("/status").body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let html = body_text(response).await;
            assert!(html.contains("Engine status"), "expected the real status page, got: {html}");
            assert!(html.contains("No Monero nodes are configured"), "expected the honest empty state, got: {html}");
        }

        #[tokio::test]
        async fn status_summary_reports_unhealthy_when_there_are_no_configured_networks() {
            let engine = engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
            let state = state_with_engine(EngineClient::new(format!("http://{}", engine.addr)));
            let router: Router = build_router(state);

            let response = router
                .oneshot(Request::builder().method("GET").uri("/status/summary").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = body_json(response).await;
            // An empty `networks` list vacuously satisfies `Iterator::all`, so
            // this must be pinned down explicitly rather than assumed - an
            // engine reporting no networks at all is not "everything's fine".
            assert_eq!(body["healthy"], false, "an engine with zero configured networks must not read as healthy, got: {body}");
        }

        /// The real degradation path: the engine is entirely unreachable (no
        /// listener at all at this port) - the page must show a plain error
        /// banner, not a 500 or a fabricated healthy page.
        #[tokio::test]
        async fn status_page_shows_a_plain_error_banner_when_the_engine_is_unreachable() {
            let state = state_with_engine(EngineClient::new("http://127.0.0.1:1"));
            let router: Router = build_router(state);

            let response =
                router.oneshot(Request::builder().method("GET").uri("/status").body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "an unreachable engine is not this page's own server error");
            let html = body_text(response).await;
            assert!(html.contains("could not be reached"), "expected a plain error banner, got: {html}");
        }

        #[tokio::test]
        async fn status_summary_reports_unhealthy_when_the_engine_is_unreachable() {
            let state = state_with_engine(EngineClient::new("http://127.0.0.1:1"));
            let router: Router = build_router(state);

            let response = router
                .oneshot(Request::builder().method("GET").uri("/status/summary").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = body_json(response).await;
            assert_eq!(body["healthy"], false);
        }
    }
}
