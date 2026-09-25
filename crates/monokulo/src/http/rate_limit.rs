//! Per-source-IP rate limiting for monokulo's own new public,
//! unauthenticated order-creation endpoint (`docs/fx_refactor.md` Phase
//! 1.3/1.4) - monokulo never had a public, state-changing,
//! unauthenticated surface before this (every other route is either behind
//! [`super::AuthedUser`] or read-only), so no rate limiter existed here at
//! all until now.
//!
//! Uses `shared::rate_limit::RateLimiter<IpAddr>` directly - the exact same
//! implementation the engine's own public endpoints use (moved to `shared`
//! in this same refactor's Phase 0.1 specifically so this crate could reuse
//! it rather than reimplementing it).

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::AppState;

/// Same `ConnectInfo`-from-extensions pattern (and the same reasoning for
/// it) as the engine's own `rate_limit_middleware` - see that function's
/// doc comment. Production must serve via
/// `into_make_service_with_connect_info::<SocketAddr>()` for this to ever
/// see a real peer address (`main.rs`); it's absent only in tests driven
/// directly through the router via `tower::ServiceExt::oneshot`, where
/// failing open trades nothing real.
pub async fn rate_limit_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let peer_ip = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|ci| ci.0.ip());
    if let Some(ip) = peer_ip {
        if !state.rate_limiter.check(ip, crate::now_unix()) {
            return (StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({ "error": "rate limit exceeded" }))).into_response();
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::middleware;
    use axum::routing::get;
    use tower::ServiceExt;

    use crate::db::Db;
    use crate::engine_client::EngineClient;
    use crate::http::AppState;

    use shared::rate_limit::RateLimiter;

    use super::rate_limit_middleware;

    const TEST_ENCRYPTION_KEY: [u8; 32] = [7u8; 32];

    fn test_state_with_limit(limit_per_minute: u32) -> AppState {
        AppState {
            db: { let db = Db::open_in_memory().unwrap(); db.seed_test_admin(); db.into_shared() },
            engine_client: EngineClient::new("http://127.0.0.1:1"),
            encryption_key: TEST_ENCRYPTION_KEY,
            status_cache: crate::http::status_page::new_status_cache(),
            exchange_rate: std::sync::Arc::new(crate::exchange_rate_config::ExchangeRateProviders::xmr_only()),
            rate_limiter: std::sync::Arc::new(RateLimiter::new(limit_per_minute)),
            event_streams: Default::default(),
            dns: std::sync::Arc::new(crate::embed_domains::UnavailableDns("DNS is not available in tests".to_string())),
        }
    }

    /// A tiny one-route router with this middleware directly layered on -
    /// not `build_router` (the real production router), since this
    /// middleware isn't wired to any real route yet (`docs/fx_refactor.md`
    /// Phase 1.4 is the real public order-creation endpoint it exists to
    /// protect). This proves the middleware's own wiring - real `AppState`,
    /// real `ConnectInfo` extraction, real `RateLimiter` - independent of
    /// which real route ends up behind it.
    async fn dummy_ok() -> &'static str {
        "ok"
    }

    fn test_router(state: AppState) -> Router {
        Router::new()
            .route("/dummy", get(dummy_ok))
            .layer(middleware::from_fn_with_state(state.clone(), rate_limit_middleware))
            .with_state(state)
    }

    #[tokio::test]
    async fn rate_limit_middleware_rejects_after_the_limit_with_a_real_connect_info() {
        let router = test_router(test_state_with_limit(2));

        let peer: std::net::SocketAddr = "10.0.0.1:12345".parse().unwrap();
        let make_request = || {
            let mut req = HttpRequest::builder().method("GET").uri("/dummy").body(Body::empty()).unwrap();
            req.extensions_mut().insert(axum::extract::ConnectInfo(peer));
            req
        };

        let r1 = router.clone().oneshot(make_request()).await.unwrap();
        let r2 = router.clone().oneshot(make_request()).await.unwrap();
        let r3 = router.oneshot(make_request()).await.unwrap();

        assert_eq!(r1.status(), axum::http::StatusCode::OK);
        assert_eq!(r2.status(), axum::http::StatusCode::OK);
        assert_eq!(r3.status(), axum::http::StatusCode::TOO_MANY_REQUESTS, "the third request must be rate-limited");
    }

    #[tokio::test]
    async fn different_ips_have_independent_budgets_through_the_real_middleware() {
        let router = test_router(test_state_with_limit(1));
        let make_request = |ip: &str| {
            let mut req = HttpRequest::builder().method("GET").uri("/dummy").body(Body::empty()).unwrap();
            req.extensions_mut().insert(axum::extract::ConnectInfo(format!("{ip}:1").parse::<std::net::SocketAddr>().unwrap()));
            req
        };

        let a1 = router.clone().oneshot(make_request("10.0.0.1")).await.unwrap();
        let b1 = router.clone().oneshot(make_request("10.0.0.2")).await.unwrap();
        assert_eq!(a1.status(), axum::http::StatusCode::OK);
        assert_eq!(b1.status(), axum::http::StatusCode::OK, "a different IP must not be affected by the first one's usage");
    }

    #[tokio::test]
    async fn a_request_with_no_connect_info_fails_open_rather_than_panicking() {
        // The exact `tower::ServiceExt::oneshot`-with-no-`ConnectInfo` case
        // this middleware's own doc comment names - real production traffic
        // always has one (`main.rs`'s `into_make_service_with_connect_info`).
        let router = test_router(test_state_with_limit(0)); // a limit of 0 would reject every IP-keyed request
        let response =
            router.oneshot(HttpRequest::builder().method("GET").uri("/dummy").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK, "no ConnectInfo must fail open, not reject");
    }
}
