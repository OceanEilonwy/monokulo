//! The monokulo's HTTP API surface.
//!
//! Mirrors the engine's own `scanner::http` module (see its doc
//! comment) in shape, not content: a `Clone`-able `AppState` carrying
//! shared, lock-guarded storage, and a `build_router(state) -> Router`
//! function so any caller — the real binary in `src/main.rs`, or this
//! module's own tests — can construct the exact same router. Tests drive it
//! through `tower::ServiceExt::oneshot` with no bound socket, the same
//! pattern as the engine's `src/http/tests.rs`; that's the right pattern
//! here specifically because these tests only ever need to exercise the
//! monokulo's own router in-process, unlike `scanner-test-support`,
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
//! monokulo session at all — and redeems that token exactly once.

mod admin_settings;
mod admin_setup;
mod checkout;
mod connect;
mod connections;
mod dashboard;
mod home;
mod invites;
mod login;
mod logout;
mod orders;
mod pay;
mod pos;
pub mod rate_limit;
mod signup;
pub mod embed_domains;
pub mod status_page;
pub mod stream_limit;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Router;
use axum::extract::FromRequestParts;
use axum::http::{HeaderMap, StatusCode, header, request::Parts};
use axum::middleware;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum_extra::extract::CookieJar;
use serde_json::json;

use crate::db::{SharedDb, UserRow};
use crate::engine_client::EngineClient;

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
    /// Short-TTL cache of the engine's own `GET /status` response, shared by
    /// every viewer - see `http::status_page`'s own module doc comment for
    /// why this exists (a real incident: the nav bar's status dot alone
    /// turned "one user browsing the dashboard" into enough engine requests
    /// to trip its own rate limiter).
    pub status_cache: status_page::StatusCache,
    /// Fiat-to-XMR conversion for monokulo's own order-creation
    /// endpoint (`docs/fx_refactor.md` Phase 1.4) - the engine no longer
    /// has any concept of this (per that document's own resolved
    /// decisions), so monokulo computes the XMR amount itself before
    /// ever calling the engine. Dispatches per-request to whichever
    /// provider the *store* (not this instance globally) has chosen - see
    /// `exchange_rate_config::ExchangeRateProviders` and
    /// `db::StoreConnectionRow::fx_provider`.
    pub exchange_rate: Arc<crate::exchange_rate_config::ExchangeRateProviders>,
    /// Per-source-IP budget for monokulo's own new public,
    /// unauthenticated endpoints (`docs/fx_refactor.md` Phase 1.3/1.4) -
    /// see `http::rate_limit`'s own module doc comment for why monokulo
    /// needs this at all now, and `http::build_router` for which routes it's
    /// actually layered onto.
    pub rate_limiter: Arc<shared::rate_limit::RateLimiter<std::net::IpAddr>>,
    /// Open checkout live-update streams per `(source IP, store pk)` - see
    /// `http::stream_limit`.
    pub event_streams: Arc<stream_limit::StreamLimiter>,
    /// TXT lookups for verified embed domains (`crate::embed_domains`) -
    /// the machine's own resolver in the real binary, a fake in tests.
    pub dns: Arc<dyn crate::embed_domains::TxtLookup>,
}

pub fn build_router(state: AppState) -> Router {
    let router = Router::new()
        .route("/", axum::routing::get(home::landing))
        .route("/admin/setup", axum::routing::get(admin_setup::setup_form).post(admin_setup::setup_submit))
        .route("/dashboard/admin/settings", axum::routing::get(admin_settings::page).post(admin_settings::save_monokulo))
        .route("/dashboard/admin/scanner-settings", axum::routing::post(admin_settings::save_scanner))
        .route("/request-invite", axum::routing::get(invites::request_invite_form).post(invites::request_invite_submit))
        .route("/dashboard/admin/invites", axum::routing::get(invites::invites_page))
        .route("/dashboard/admin/invites/create-link", axum::routing::post(invites::create_invite_link))
        .route("/dashboard/admin/invites/delete-all", axum::routing::post(invites::delete_all_invite_requests))
        .route("/dashboard/admin/invites/{id}/delete", axum::routing::post(invites::delete_invite_request))
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
        .route("/dashboard/theme", axum::routing::post(dashboard::theme_submit))
        .route("/dashboard/connect", axum::routing::get(dashboard::connect_form).post(dashboard::connect_submit))
        .route("/dashboard/stores/new", axum::routing::get(home::new_store_picker))
        .route("/dashboard/stores/new/woocommerce", axum::routing::get(home::woocommerce_instructions))
        .route("/dashboard/stores/{id}", axum::routing::get(orders::store_detail))
        .route("/dashboard/stores/{id}/settings", axum::routing::get(orders::store_settings))
        .route("/dashboard/stores/{id}/settings/domains", post(embed_domains::add_domain))
        .route("/dashboard/stores/{id}/settings/domains/{domain_id}/check", post(embed_domains::check_domain))
        .route("/dashboard/stores/{id}/settings/domains/{domain_id}/delete", post(embed_domains::delete_domain))
        .route("/dashboard/stores/{id}/settings/embed-restriction", post(embed_domains::set_embed_restriction))
        .route("/dashboard/stores/{id}/embed-warning/dismiss", post(embed_domains::dismiss_embed_warning))
        .route(
            "/dashboard/stores/{id}/orders/new",
            axum::routing::get(orders::create_order_page).post(orders::create_order),
        )
        .route(
            "/dashboard/stores/{id}/settings/confirmations",
            axum::routing::post(orders::update_confirmations_required),
        )
        .route(
            "/dashboard/stores/{id}/settings/fx-provider",
            axum::routing::post(orders::update_fx_provider),
        )
        .route(
            "/dashboard/stores/{id}/settings/base-currency",
            axum::routing::post(orders::update_base_currency),
        )
        .route(
            "/dashboard/stores/{id}/settings/confirmation-thresholds",
            axum::routing::post(orders::create_confirmation_threshold),
        )
        .route(
            "/dashboard/stores/{id}/settings/confirmation-thresholds/{threshold_id}/delete",
            axum::routing::post(orders::delete_confirmation_threshold),
        )
        .route(
            "/dashboard/stores/{id}/settings/confirmation-thresholds/save",
            axum::routing::post(orders::save_confirmation_thresholds),
        )
        .route(
            "/dashboard/stores/{id}/settings/webhooks",
            axum::routing::post(orders::webhooks_create),
        )
        .route(
            "/dashboard/stores/{id}/settings/webhooks/{webhook_id}/delete",
            axum::routing::post(orders::webhooks_delete),
        )
        .route("/dashboard/stores/{id}/pos", axum::routing::get(pos::pos_page))
        .route("/dashboard/stores/{id}/pos/orders", axum::routing::post(pos::create_order))
        .route("/dashboard/stores/{id}/pos/orders/{order_id}/status", axum::routing::get(pos::order_status))
        .route("/dashboard/stores/{id}/pos/events", axum::routing::get(pos::order_events))
        .route("/dashboard/stores/{id}/orders", axum::routing::get(orders::orders_list))
        .route("/dashboard/stores/{id}/orders/lookup", axum::routing::post(orders::lookup_payment))
        .route("/dashboard/stores/{id}/orders/{order_id}", axum::routing::get(orders::order_detail))
        .route("/connect/{platform}", axum::routing::get(connect::start).post(connect::confirm_submit))
        .route("/connect/{platform}/finish", post(connect::finish));

    // `POST /pay/{pk}/orders` (`docs/fx_refactor.md` Phase 1.4) is
    // monokulo's first genuinely public, unauthenticated,
    // state-changing endpoint - a separate sub-router purely so
    // `rate_limit::rate_limit_middleware` layers onto *only* this route,
    // not the authenticated `/dashboard/*` routes or the admin-proxy ones
    // above, which don't need (and shouldn't share a budget via) an
    // IP-keyed limit - see `http::rate_limit`'s own module doc comment.
    let pay_router = Router::new()
        .route("/pay/{pk}/orders", post(pay::create_order))
        .route("/pay/{pk}/orders/{order_id}", axum::routing::get(checkout::checkout_page))
        .route("/pay/{pk}/orders/{order_id}/status", axum::routing::get(checkout::checkout_status))
        .route("/pay/{pk}/orders/{order_id}/events", axum::routing::get(checkout::checkout_events))
        .route(
            "/pay/{pk}/orders/{order_id}/refund-address",
            axum::routing::post(checkout::set_refund_address),
        )
        // A real follow-up to `docs/fx_refactor.md`: a nav-bearing,
        // shareable page wrapping the (nav-less) checkout page above in an
        // iframe - see `checkout::checkout_share_page`'s own doc comment.
        .route("/pay/{pk}/orders/{order_id}/share", axum::routing::get(checkout::checkout_share_page))
        .layer(middleware::from_fn_with_state(state.clone(), embed_domains::embed_policy_middleware))
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit::rate_limit_middleware))
        // Outside the rate limit, so a preflight never spends budget and a
        // `429` still carries the headers a cross-origin caller needs to read it.
        .layer(embed_cors_layer(&state));

    let router = router.merge(pay_router);

    // A plain static file, not state-changing - no rate limiter needed
    // (`docs/fx_refactor.md` Phase 4.3), same as the engine's original.
    let router = router.route("/static/monokulo-client.js", axum::routing::get(pay::client_library).layer(any_origin_cors_layer()));
    let router = router.route("/static/checkout.js", axum::routing::get(pay::checkout_script));
    let router = router.route("/static/jsQR.js", axum::routing::get(pay::qr_decoder_script));
    let router = router.route("/static/logo.svg", axum::routing::get(pay::logo_svg));
    let router = router.route("/static/logo-inverted.svg", axum::routing::get(pay::logo_inverted_svg));
    let router = router.route("/static/favicon.svg", axum::routing::get(pay::favicon_svg));
    let router = router.route("/static/manrope-500.woff2", axum::routing::get(pay::manrope_500_woff2));
    let router = router.route("/static/manrope-700.woff2", axum::routing::get(pay::manrope_700_woff2));
    let router = router.route("/static/manrope-800.woff2", axum::routing::get(pay::manrope_800_woff2));

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

/// Same credential resolution as [`AuthedUser`], plus a real
/// `user.is_admin` check - the gate for the admin settings page
/// (`http/admin_settings.rs`, WBS: "only display an 'admin' nav menu entry
/// if the authenticated user is the admin account" extended to the route
/// itself, not just the nav link). A missing/invalid session still rejects
/// with the same `401` `AuthedUser` would; a *valid* session that just isn't
/// the admin account gets `403`, not `401` - see [`ApiError::Forbidden`]'s
/// own doc comment for why those are kept distinct.
pub struct AuthedAdmin(pub UserRow, pub String);

impl FromRequestParts<AppState> for AuthedAdmin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let AuthedUser(user, hash) = AuthedUser::from_request_parts(parts, state).await?;
        if !user.is_admin {
            return Err(ApiError::Forbidden);
        }
        Ok(AuthedAdmin(user, hash))
    }
}

/// CORS for the public `/pay/{pk}/...` routes a merchant's page uses to
/// embed checkout (order creation from `monokulo-client.js`, order status
/// and its live stream). A store that hasn't restricted embedding answers
/// any origin - a clearnet shop, a `.onion` one, a sandboxed frame whose
/// origin is `null` - and one that has answers only its verified domains
/// (`embed_domains::EmbedPolicy::allows_origin`). Safe to open this wide by
/// default because none of these routes use cookies or any other ambient
/// credential (credentials are never allowed), and anything they do is
/// equally possible from a script outside a browser. Private-network
/// preflights are answered too, so a public page can embed a monokulo that
/// runs on a local network address.
fn embed_cors_layer(state: &AppState) -> tower_http::cors::CorsLayer {
    use tower_http::cors::AllowOrigin;
    let db = state.db.clone();
    cors_layer_base().allow_origin(AllowOrigin::predicate(move |origin, parts| {
        let Some(public_key) = embed_domains::public_key_of_pay_path(parts.uri.path()) else { return true };
        let Ok(origin) = origin.to_str() else { return false };
        match crate::embed_domains::policy_for_public_key(&db, public_key) {
            Some(policy) => policy.allows_origin(origin, crate::now_unix()),
            None => true,
        }
    }))
}

/// CORS for the client library itself, for a page that loads it with
/// `crossorigin` or as a module: any origin, since loading the library
/// alone does nothing for any store.
fn any_origin_cors_layer() -> tower_http::cors::CorsLayer {
    cors_layer_base().allow_origin(tower_http::cors::Any)
}

fn cors_layer_base() -> tower_http::cors::CorsLayer {
    tower_http::cors::CorsLayer::new()
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
        .allow_headers([header::CONTENT_TYPE, header::ACCEPT])
        .allow_private_network(true)
        .max_age(std::time::Duration::from_secs(24 * 60 * 60))
}

/// Page chrome for a page with the site nav (or another status
/// indicator): `views::PageChrome::from_user` plus the engine's last known
/// health (`status_page::known_health`).
pub(crate) fn page_chrome(state: &AppState, user: Option<&crate::db::UserRow>, current_path: impl Into<String>) -> crate::views::PageChrome {
    crate::views::PageChrome::from_user(user, current_path).with_health(status_page::known_health(state))
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
    /// Added for `http::pay`'s new public order-creation endpoint
    /// (`docs/fx_refactor.md` Phase 1.4) - an unknown `pk_...` gets a plain
    /// `404` with a generic message, the same enumeration-defense principle
    /// `AuthedUser`/every dashboard route's "missing vs. not-yours" `404`
    /// already applies, extended to "does this tenant even exist" for a
    /// route with no owner to check against in the first place.
    NotFound,
    /// [`AuthedAdmin`]'s own rejection for a real, valid session that simply
    /// isn't the instance's one admin account - deliberately distinct from
    /// `Unauthorized` (no/invalid session at all): a merchant hitting an
    /// admin-only route has a perfectly valid session, they just aren't
    /// allowed here, which is exactly what `403` (not `401`) means.
    Forbidden,
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::Conflict => (StatusCode::CONFLICT, "email already in use".to_string()),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            ApiError::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden".to_string()),
            ApiError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string()),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
