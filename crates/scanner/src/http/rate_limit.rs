//! Rate limiting for the engine's HTTP API, keyed on the presented
//! `Authorization: Bearer ...` token. IP-based limiting is meaningless here:
//! monokulo calls this API on behalf of every one of *its own* users from one
//! address, so a per-IP budget would cap all of them combined rather than any
//! one abusive caller (a real incident - see `work_notes.md`'s note on the
//! monokulo status page tripping the old per-IP limit for a single real
//! user). Keying on the presented `sk_...` token instead gives each tenant its
//! own independent budget. A request with no/malformed token (tenant
//! creation, `/status`) falls back to its IP (see `admin_rate_limit_middleware`)
//! so it's still capped by *something*.
//!
//! The per-IP limiter that used to guard the engine's public routes is gone
//! with those routes: the engine is private, and monokulo does per-client
//! limiting for everything public.
//!
//! A fixed-window counter, not a proper token bucket - simpler, and
//! sufficient for "stop one source from hammering this endpoint," which is the
//! actual goal. A smarter algorithm is a reasonable future improvement, not a
//! v1 requirement.
//!
//! The limiter itself (`RateLimiter<K>`, generic windowing/pruning/ceiling
//! logic, no HTTP dependency) moved to `shared::rate_limit`
//! (`docs/fx_refactor.md` Phase 0.1) so the monokulo can reuse the exact
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

/// Keyed on the presented `Authorization: Bearer sk_...` token itself, not the
/// source IP - see this module's own doc comment for why. A request with
/// no/malformed `Authorization` header has no token to key on - falls back to
/// IP so it's still capped by *something* rather than exempted entirely; a real `sk_...` is opaque and
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
            // Only absent when a test drives the router directly with no
            // accepted connection (`oneshot`); production always serves via
            // `into_make_service_with_connect_info`, so failing open here
            // trades nothing real.
            None => return next.run(req).await,
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
