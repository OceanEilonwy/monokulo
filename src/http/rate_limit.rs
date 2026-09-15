//! Rate limiting for two genuinely different kinds of caller, each with its own
//! limiter and its own key:
//!
//! - **Per-IP**, for the state-changing, necessarily-unauthenticated public
//!   endpoints (order creation chief among them - a static site has nowhere to
//!   keep a secret, so these can't require auth) plus `/status`. See
//!   `docs/DESIGN.md` §12.
//! - **Per-token**, for the `sk_`-authenticated admin API. IP-based limiting is
//!   meaningless there: a hosted control plane calls this API on behalf of
//!   every one of *its own* users from one proxy IP, so a per-IP budget caps
//!   all of them combined rather than any one abusive caller (a real incident -
//!   see `work_notes.md`'s note on the control-plane status page tripping this
//!   for a single real user). Keying on the presented `sk_...` token instead
//!   gives each tenant its own independent budget, the same guarantee per-IP
//!   limiting gives a direct, unproxied caller. A request with no/malformed
//!   token still falls back to its IP (see `admin_rate_limit_middleware`) so
//!   anonymous guessing against these endpoints is still capped by *something*.
//!
//! Both are fixed-window counters, not a proper token bucket - simpler, and
//! sufficient for "stop one source from hammering this endpoint," which is the
//! actual goal. A smarter algorithm is a reasonable future improvement, not a
//! v1 requirement.
//!
//! The limiter itself (`RateLimiter<K>`, generic windowing/pruning/ceiling
//! logic, no HTTP dependency) moved to `shared::rate_limit`
//! (`docs/fx_refactor.md` Phase 0.1) so the control-plane can reuse the exact
//! same, already-proven implementation for its own new public endpoints
//! rather than reimplementing it - re-exported here so every existing
//! `rate_limit::RateLimiter` reference in this crate keeps compiling
//! unchanged. Only the axum-specific middleware (which key to extract from a
//! real request) stays local to this crate.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::AppState;

pub use shared::rate_limit::RateLimiter;

/// `ConnectInfo` is read directly from the request's extensions rather than taken
/// as a typed extractor (axum has no `Option<ConnectInfo<T>>` extractor impl) so
/// this degrades gracefully when it's absent - which only happens when a request is
/// driven directly through the router without a real accepted connection
/// (`tower::ServiceExt::oneshot` in tests). Production always serves via
/// `into_make_service_with_connect_info::<SocketAddr>()`, which guarantees it's
/// present, so failing open here trades nothing in production.
pub async fn rate_limit_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let peer_ip = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|ci| ci.0.ip());
    if let Some(ip) = peer_ip {
        if !state.rate_limiter.check(ip, super::now_unix()) {
            return (StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({ "error": "rate limit exceeded" }))).into_response();
        }
    }
    next.run(req).await
}

/// Same shape as [`rate_limit_middleware`], but for the `sk_`-authenticated admin
/// API - keyed on the presented `Authorization: Bearer sk_...` token itself, not
/// the source IP. See this module's own doc comment for why: a hosted control
/// plane calls this API for many real tenants from one proxy IP, so an IP-keyed
/// budget would cap all of them together instead of each tenant independently.
/// A request with no/malformed `Authorization` header has no token to key on -
/// falls back to IP (same as [`rate_limit_middleware`]) so it's still capped by
/// *something* rather than exempted entirely; a real `sk_...` is opaque and
/// server-minted, not guessable, so this isn't meaningfully weaker than IP-keying
/// would be for that case. Does not itself validate the token - an invalid one
/// still consumes its own budget bucket (keyed on its literal bytes) and is
/// rejected downstream by `AuthedTenant`, same as a valid one would be rejected
/// downstream for an unrelated reason; this middleware only ever answers "is this
/// key over budget," never "is this key valid."
pub async fn admin_rate_limit_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let token = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
    let key = match token {
        Some(token) => token.to_string(),
        None => match req.extensions().get::<ConnectInfo<SocketAddr>>() {
            Some(ci) => ci.0.ip().to_string(),
            None => return next.run(req).await, // see rate_limit_middleware's own doc comment
        },
    };
    if !state.admin_rate_limiter.check(key, super::now_unix()) {
        return (StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({ "error": "rate limit exceeded" }))).into_response();
    }
    next.run(req).await
}

// `RateLimiter<K>`'s own unit tests moved to `shared::rate_limit` along with
// the type itself (see this module's own doc comment) - the middleware
// wiring tests (real router, real `ConnectInfo`) stay in
// `src/http/tests.rs`, unaffected by this move.
