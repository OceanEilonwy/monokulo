//! The control-plane's HTTP API surface.
//!
//! Mirrors the engine's own `moneropay_core::http` module (see its doc
//! comment) in shape, not content: a `Clone`-able `AppState` carrying
//! shared, lock-guarded storage, and a `build_router(state) -> Router`
//! function so any caller — the real binary in `src/main.rs`, or this
//! module's own tests — can construct the exact same router. Tests drive it
//! through `tower::ServiceExt::oneshot` with no bound socket, the same
//! pattern as the engine's `src/http/tests.rs`; that's the right pattern
//! here specifically because these tests only ever need to exercise the
//! control-plane's own router in-process, unlike `engine-test-support`,
//! which exists because *other* crates need a real socket to reach a
//! separately-deployed *engine* instance.
//!
//! `/signup` is necessarily unauthenticated (WBS 1.1.1). `/login` (1.1.2)
//! issues a session token; the [`AuthedUser`] extractor below resolves a
//! `Authorization: Bearer <session_token>` header back to a user, the same
//! pattern as the engine's own `AuthedTenant` (see `src/http/mod.rs` at the
//! repo root) resolves `Bearer sk_...`. `/logout` (1.1.3) reuses the same
//! extractor to find out which session to revoke.
//!
//! `/dashboard/signup` and `/dashboard/login` (`dashboard` module, WBS
//! 1.3.1) are a second, browser-facing surface over the same underlying
//! account/login logic — plain HTML forms instead of JSON, ending in a
//! `session` cookie instead of a JSON-body bearer token. [`AuthedUser`]
//! accepts either: a `Bearer` header (the JSON API's own clients) or a
//! `session` cookie (the browser flow) — same hash-and-look-up logic either
//! way, so this stays one auth system, not two.
//!
//! `/dashboard/connect` (`dashboard` module, WBS 1.3.2) is the same idea
//! applied to `/connections`: a form-post wrapper, behind [`AuthedUser`],
//! over `connections::create_connection_for_user` — the exact logic
//! `/connections` itself calls, not a reimplementation of it.
//!
//! `/connect/{platform}` and `/connect/{platform}/finish` (`connect` module,
//! WBS 1.4.1) are the generic, platform-agnostic "one-click install" flow
//! (`docs/WOOCOMMERCE_ROADMAP.md` Stage 6): a plugin sends the merchant's
//! browser to `GET /connect/{platform}`, which redirects to
//! `/dashboard/login` (carrying a validated `next`, see
//! `dashboard::login_submit`) if there's no session yet, or a confirm form
//! if there is; confirming calls the same `connections::create_connection_for_user`
//! every other surface uses, then redirects to the plugin's `return_url`
//! with a short-lived, single-use connect token instead of a raw `sk_...`.
//! `POST /connect/{platform}/finish` is deliberately *not* behind
//! [`AuthedUser`] — it's called server-to-server by the plugin, which has no
//! control-plane session at all — and redeems that token exactly once.

mod connect;
mod connections;
mod dashboard;
mod home;
mod login;
mod logout;
mod orders;
mod signup;
pub mod status_page;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Router;
use axum::extract::FromRequestParts;
use axum::http::{HeaderMap, StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum_extra::extract::CookieJar;
use serde_json::json;

use crate::db::{SharedDb, UserRow};
use crate::engine_client::EngineClient;
use crate::templates::TemplateEngine;

/// Name of the cookie the browser-facing login flow (`dashboard::login_submit`)
/// sets and [`AuthedUser`] reads back — a plain constant so the two sides
/// can't drift apart on the name.
pub(crate) const SESSION_COOKIE_NAME: &str = "session";

#[derive(Clone)]
pub struct AppState {
    pub db: SharedDb,
    pub engine_client: EngineClient,
    /// AES-256-GCM key (WBS 1.2.3) used to encrypt the engine's `sk_...`
    /// secret token before it's stored in `store_connections` — see
    /// `crate::crypto` and `http/connections.rs`. Sourced from an
    /// environment variable in the real binary (`main.rs`); tests just
    /// construct a fixed key directly.
    pub encryption_key: [u8; 32],
    /// Renders the two fixed browser-facing pages (WBS 1.3.1). `Arc`-wrapped
    /// since it's built once (parsing the two built-in templates) and only
    /// ever read afterward — cheap to clone into every `AppState` clone.
    pub templates: Arc<TemplateEngine>,
    /// Short-TTL cache of the engine's own `GET /status` response, shared by
    /// every viewer - see `http::status_page`'s own module doc comment for
    /// why this exists (a real incident: the nav bar's status dot alone
    /// turned "one user browsing the dashboard" into enough engine requests
    /// to trip its own rate limiter).
    pub status_cache: status_page::StatusCache,
}

pub fn build_router(state: AppState) -> Router {
    let router = Router::new()
        .route("/", axum::routing::get(home::landing))
        .route("/status", axum::routing::get(status_page::status_page))
        .route("/status/summary", axum::routing::get(status_page::status_summary))
        .route("/signup", post(signup::signup))
        .route("/login", post(login::login))
        .route("/logout", post(logout::logout))
        .route("/connections", post(connections::create_connection))
        .route("/dashboard", axum::routing::get(home::dashboard_home))
        .route("/dashboard/signup", axum::routing::get(dashboard::signup_form).post(dashboard::signup_submit))
        .route("/dashboard/login", axum::routing::get(dashboard::login_form).post(dashboard::login_submit))
        .route("/dashboard/logout", axum::routing::post(dashboard::logout_submit))
        .route("/dashboard/connect", axum::routing::get(dashboard::connect_form).post(dashboard::connect_submit))
        .route("/dashboard/connections/new", axum::routing::get(home::new_store_picker))
        .route("/dashboard/connections/new/woocommerce", axum::routing::get(home::woocommerce_instructions))
        .route("/dashboard/connections/{id}", axum::routing::get(orders::store_detail))
        .route("/dashboard/connections/{id}/orders/new", axum::routing::post(orders::create_order))
        .route(
            "/dashboard/connections/{id}/settings/confirmations",
            axum::routing::post(orders::update_confirmations_required),
        )
        .route("/dashboard/connections/{id}/orders", axum::routing::get(orders::orders_list))
        .route("/dashboard/connections/{id}/orders/{payment_id}", axum::routing::get(orders::order_detail))
        .route("/dashboard/connections/{id}/webhooks", axum::routing::get(orders::webhooks_list).post(orders::webhooks_create))
        .route("/dashboard/connections/{id}/webhooks/{webhook_id}/delete", axum::routing::post(orders::webhooks_delete))
        .route("/connect/{platform}", axum::routing::get(connect::start).post(connect::confirm_submit))
        .route("/connect/{platform}/finish", post(connect::finish));

    // Test-only route exercising `AuthedUser` - see its doc comment.
    // Compiled only under `#[cfg(test)]`, so it never exists in the real
    // binary; nothing outside this crate's own tests should ever reach it.
    #[cfg(test)]
    let router = router.route("/_test/whoami", axum::routing::get(test_whoami));

    router.with_state(state)
}

/// Resolves a session token to the user that session belongs to - either
/// from `Authorization: Bearer <session_token>` (the JSON API, WBS 1.1.2) or
/// from a `session` cookie (the browser flow, WBS 1.3.1's
/// `dashboard::login_submit`), checked in that order: a request carrying an
/// `Authorization` header is treated as an API client and only that header
/// is consulted, falling back to the cookie only when the header is absent
/// entirely. `401` for a missing/malformed header, missing cookie, or an
/// unknown/invalid token - never distinguishes any of these from each
/// other. Both paths converge on the exact same "hash the token, look up
/// the session" logic below - there is one auth system here, not two
/// parallel ones.
///
/// Also carries the presented session's `token_hash` (the same hash
/// `Db::find_session`/`Db::delete_session` key on) alongside the resolved
/// user - `/logout` (WBS 1.1.3) needs to know exactly which session row to
/// delete, and re-deriving it would mean re-parsing the credential a second
/// time outside this extractor.
pub struct AuthedUser(pub UserRow, pub String);

impl FromRequestParts<AppState> for AuthedUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        resolve_authed_user(state, &parts.headers).map(|(user, hash)| AuthedUser(user, hash)).ok_or(ApiError::Unauthorized)
    }
}

/// The actual "resolve a session to its user" logic [`AuthedUser`]'s
/// extractor uses - factored out so a handler that needs to know *whether*
/// the caller has a valid session, without failing the request outright when
/// they don't, can reuse the exact same header/cookie parsing and lookup
/// instead of a parallel reimplementation. `GET /connect/{platform}`
/// (`http/connect.rs`, WBS 1.4.1) is the first such caller: an unauthenticated
/// request there is a normal, expected case handled with a redirect to
/// `/dashboard/login`, not a bare `401` - genuinely different handling from
/// [`AuthedUser`]'s own rejection, but it must resolve a *valid* session
/// identically, or the two code paths could quietly drift apart on what
/// counts as "logged in."
///
/// `None` covers every reason a session doesn't resolve (missing/malformed
/// header, missing cookie, unknown/invalid token, a database error looking
/// either up) - never distinguished further, same as [`AuthedUser`] itself.
pub(crate) fn resolve_authed_user(state: &AppState, headers: &HeaderMap) -> Option<(UserRow, String)> {
    let token = if let Some(header_value) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        header_value.strip_prefix("Bearer ")?.to_string()
    } else {
        let jar = CookieJar::from_headers(headers);
        jar.get(SESSION_COOKIE_NAME)?.value().to_string()
    };
    let token_hash = shared::auth::hash_secret_token(&token);

    let db = state.db.lock().unwrap();
    let session = db.find_session(&token_hash).ok().flatten()?;
    let user = db.get_user_by_id(&session.user_id).ok().flatten()?;
    Some((user, token_hash))
}

/// Test-only dummy protected route (see WBS 1.1.2): its only purpose is
/// giving this task's own integration tests something protected by
/// [`AuthedUser`] to exercise, since no real protected endpoint exists yet.
/// Not a real API surface - do not build on it.
#[cfg(test)]
async fn test_whoami(AuthedUser(user, _): AuthedUser) -> Json<serde_json::Value> {
    Json(json!({ "user_id": user.id }))
}

/// `Conflict` (signup, duplicate email), `Unauthorized` (login, or a
/// missing/invalid session), `BadRequest` (a request the *caller* got
/// wrong in some caller-visible way — currently just the engine rejecting
/// `POST /connections`'s wallet fields, e.g. bad hex or an unconfigured
/// network), or `Internal` for anything else. Every message on
/// `Conflict`/`Unauthorized`/`Internal` is fixed and generic — `Internal`
/// never describes *why* the underlying operation failed, and
/// `Unauthorized` never distinguishes "wrong password" from "unknown
/// email" from "invalid session token" — so a client can't use
/// error-message differences to fingerprint internals or enumerate
/// accounts beyond what each variant already, unavoidably, signals.
/// `BadRequest` is the one variant that *does* carry a real message,
/// deliberately: it's surfacing the engine's own validation error back to
/// the caller who made the mistake, not leaking internal state.
pub enum ApiError {
    Conflict,
    Unauthorized,
    BadRequest(String),
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::Conflict => (StatusCode::CONFLICT, "email already in use".to_string()),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            ApiError::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            ApiError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string()),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
