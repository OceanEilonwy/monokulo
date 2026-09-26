//! The HTTP API surface. See `docs/DESIGN.md` §10.
//!
//! Three simplifications worth naming up front, all reasonable for this pass and
//! all noted rather than hidden:
//!
//! 1. `Store` access from handlers goes through `state.store.lock().unwrap()`
//!    directly rather than a dedicated writer-actor thread reached over a channel.
//!    SQLite disallows concurrent writers regardless, so this is a correct - if
//!    simpler - realization of "single writer" (same reasoning as the concurrency
//!    stress test in `store.rs`). It does mean a handler briefly blocks the async
//!    worker thread it's running on for the duration of a query; fine for
//!    single-digit-millisecond local SQLite access, worth revisiting with
//!    `spawn_blocking` if a slower storage backend ever sits behind this trait.
//! 2. `wallet_handles` is populated eagerly at boot by `main` (via
//!    `Store::list_active_tenants` + `KeyCustody::unseal_and_register`), with the
//!    lazy path in `resolve_wallet_handle` kept as a fallback for a tenant created
//!    after boot.
//! 3. Rate limiting (`rate_limit`) is a fixed-window per-IP counter, not a proper
//!    token bucket - simpler, and sufficient for the actual goal (§DESIGN.md §12).
//!    The PoW-challenge fallback for sustained load is not implemented.
//! 4. CORS (`build_cors_layer`) re-derives the same `allowed_origins` check
//!    `public::resolve_public_tenant` already does server-side (§DESIGN.md §12: the
//!    app-layer check is the actual guarantee, this layer only makes the browser's
//!    *own* enforcement work at all) via one extra `Store` lookup per preflight -
//!    negligible against local SQLite, and it's what lets a genuinely cross-origin
//!    merchant site's `fetch()` calls succeed instead of being silently blocked by
//!    the browser for lacking `Access-Control-Allow-Origin`.
//!
//! Not implemented in this pass: TLS termination (expected to sit behind a reverse
//! proxy or terminate via `rustls` in `main`, not implemented here).

mod admin;
pub mod instance_admin;
mod public;
pub mod rate_limit;
mod status_page;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::extract::FromRequestParts;
use axum::http::{Method, StatusCode, header, request::Parts};
use axum::middleware;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use serde_json::json;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;

use crate::daemon_fallback::FallbackDaemonClient;
use crate::key_custody::{KeyCustody, KeyCustodyError, WalletHandle};
use crate::scanner_status::ScannerStatusMap;
use crate::status::OrderStatus;
use crate::store::{SharedStore, StoreError, Tenant};

use rate_limit::{admin_rate_limit_middleware, rate_limit_middleware, RateLimiter};

#[derive(Clone)]
pub struct AppState {
    pub store: SharedStore,
    pub key_custody: Arc<dyn KeyCustody>,
    /// The `[key_custody].backend` value that produced `key_custody` above -
    /// `"plain"` or `"socket"` - so `admin::create_tenant` can record which
    /// backend actually sealed a *newly created* tenant's key material in
    /// `tenants.key_custody_backend` (see that column's own comment in
    /// `migrations/0001_init.sql`) instead of the pre-WBS-2.1.3 hardcoded
    /// `"plain"` literal, which would otherwise misrepresent every tenant
    /// created while this instance is running with `backend = "socket"`
    /// configured. A plain `String`, not a re-derivation from `key_custody`'s
    /// own concrete type: nothing about `Arc<dyn KeyCustody>` lets a caller ask
    /// "which implementation is this," by design (see `key_custody`'s own module
    /// doc comment) - `main.rs` already knows the answer from `Config` at boot,
    /// so it hands it down alongside the trait object rather than reconstructing
    /// it via some new downcast/introspection surface this boundary was
    /// deliberately never given.
    pub key_custody_backend: String,
    pub wallet_handles: Arc<RwLock<HashMap<String, WalletHandle>>>,
    /// Per-source-IP budget for the public/unauthenticated endpoints and
    /// `/status` - see `http::rate_limit`'s own module doc comment.
    pub rate_limiter: Arc<RateLimiter>,
    /// Per-`sk_`-token budget for the admin API - a separate, independent
    /// limiter from `rate_limiter` above, not a second view of the same
    /// data. See `http::rate_limit`'s own module doc comment for why IP-
    /// keying is the wrong shape for this particular surface.
    pub admin_rate_limiter: Arc<RateLimiter<String>>,
    /// Which networks this instance can actually scan - i.e. which
    /// `[monero_node.<network>]` sections are configured. A tenant can only be
    /// created for a network in this set; otherwise its address would be derived
    /// but never scanned by anything; see `docs/DESIGN.md` §7 (multi-network) and
    /// `admin::create_tenant`.
    pub configured_networks: Arc<std::collections::HashSet<monero::Network>>,
    /// One `FallbackDaemonClient` per configured network - the same
    /// instances the chain-scanner loop itself uses (`main.rs` clones the
    /// `Arc` into both places), so `GET /status` (`status_page.rs` - a JSON
    /// status *API*; the real, styled status *page* is served by the
    /// monokulo, which calls this endpoint) reports on the real node
    /// list scanning is actually happening against, not a second,
    /// separately-configured view of it. Concrete `Arc<FallbackDaemonClient>`,
    /// not `Arc<dyn MoneroDaemonClient>` - only `FallbackDaemonClient`
    /// exposes its own node list (`FallbackDaemonClient::nodes`), which is
    /// exactly what `/status` needs to report on each node individually
    /// rather than only the aggregate view `MoneroDaemonClient`'s own trait
    /// methods give.
    pub daemons: Arc<HashMap<monero::Network, Arc<FallbackDaemonClient>>>,
    /// Live scan-tick history per network, updated by `main.rs`'s own scan
    /// loop after every tick - see `scanner_status`'s own module doc
    /// comment.
    pub scanner_status: ScannerStatusMap,
    /// How often the scan loop sleeps between full sweeps
    /// (`config.payment.mempool_poll_interval_ms`, converted once at boot) -
    /// purely so the status page can say *how* overdue a tick that hasn't
    /// happened in a while actually is, relative to what's actually
    /// configured, rather than against an arbitrary hardcoded guess.
    pub scan_poll_interval_secs: u64,
    /// `docs/order_rescan_wbs.md` Phase 4/5.3 -
    /// `config.payment.expired_order_grace_period_minutes * 60`, threaded down the
    /// same way the two lookback-day fields above already are. Needed here (not
    /// only inside `scanner::run_scan_tick`) because `admin::build_order_view`'s
    /// `currently_scanning` computation uses the identical widened in-scope
    /// predicate the live scanner itself uses.
    pub expired_order_grace_period_seconds: i64,
}

pub fn build_router(state: AppState, max_body_bytes: usize) -> Router {
    // Two sub-routers, each with its own rate-limit middleware and key - see
    // `http::rate_limit`'s own module doc comment for why these need to be
    // separate rather than one shared limiter over the whole API.
    //
    // `POST /api/v1/admin/tenants` (tenant creation) sits in the *public*
    // group despite living under `/api/v1/admin/...`: it takes no
    // `Authorization` header at all (see `EngineClient`'s own doc comment on
    // why - admin-API network isolation is an ops-level concern, not one
    // this endpoint enforces itself), so it has no token to key a per-token
    // limit on, and it's exactly the kind of unauthenticated, state-changing
    // request this repo's rate limiting exists to bound in the first place.
    let public_router = Router::new()
        .route("/api/v1/admin/tenants", post(admin::create_tenant))
        .route("/api/v1/t/{pk}/orders", post(public::create_order))
        .route("/api/v1/t/{pk}/orders/{order_id}", get(public::get_order_status))
        .route(
            "/api/v1/t/{pk}/orders/{order_id}/refund-address",
            post(public::set_refund_address),
        )
        // A JSON status *API*, not a page - the monokulo's own
        // `GET /status` calls this and renders the real, styled page.
        // Deliberately unauthenticated (no `sk_`/`pk_` involved) and outside
        // the `/api/v1/...` version prefix those doc comments explain the
        // reasoning for - this reports node/scanner
        // health across *every* configured network at once, not tenant-
        // scoped API surface, the same way a service's own `/healthz`
        // typically sits outside its versioned API.
        .route("/status", get(status_page::status_page))
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit_middleware));

    // Every route here requires a real `Authorization: Bearer sk_...` (see
    // `AuthedTenant`), so each gets its own per-token budget instead of
    // sharing the public group's per-IP one.
    let admin_router = Router::new()
        .route(
            "/api/v1/admin/tenant",
            get(admin::get_own_tenant).patch(admin::patch_own_tenant).delete(admin::delete_own_tenant),
        )
        .route("/api/v1/admin/tenant/rotate-secret", post(admin::rotate_secret))
        .route("/api/v1/admin/tenant/orders", get(admin::list_orders).post(public::create_order_for_admin))
        .route("/api/v1/admin/tenant/orders/{order_id}", get(admin::get_order_detail))
        .route(
            "/api/v1/admin/tenant/orders/{order_id}/refund-address",
            post(admin::set_order_refund_address),
        )
        .route("/api/v1/admin/tenant/events", get(admin::order_events))
        .route("/api/v1/admin/tenant/payments/lookup", post(admin::lookup_payment))
        .route(
            "/api/v1/admin/tenant/webhooks",
            get(admin::list_webhooks).post(admin::create_webhook),
        )
        .route("/api/v1/admin/tenant/webhooks/{webhook_id}", delete(admin::delete_webhook))
        .layer(middleware::from_fn_with_state(state.clone(), admin_rate_limit_middleware));

    // The instance-wide settings API - a different credential (the instance
    // admin token, `AuthedInstanceAdmin`) from every route above, which all
    // authenticate as one specific *tenant*. Shares the admin group's
    // per-token rate limit rather than a third bucket of its own - this is
    // exactly the kind of low-volume, human-driven traffic
    // (`admin_rate_limit_middleware`'s own generous default) that budget
    // already exists for.
    let instance_admin_router = Router::new()
        .route(
            "/api/v1/admin/settings",
            get(instance_admin::get_settings).post(instance_admin::update_settings),
        )
        .layer(middleware::from_fn_with_state(state.clone(), admin_rate_limit_middleware));

    public_router
        .merge(admin_router)
        .merge(instance_admin_router)
        .layer(RequestBodyLimitLayer::new(max_body_bytes))
        .layer(build_cors_layer(state.store.clone()))
        .with_state(state)
}

/// Only the `/api/v1/t/{pk}/orders...` family needs real cross-origin browser
/// support: it's the surface a caller's own (necessarily different-origin) site can
/// call directly via `fetch()` for a custom integration (`docs/fx_refactor.md`
/// decision 2/3: the hosted checkout UI and its embed library now live on the
/// monokulo, which calls this API server-to-server, not from a browser - this
/// grant exists for a self-hoster's own direct browser-side integration instead).
/// The admin API is deliberately left with no CORS grant at all: it's a
/// backend-to-backend surface authenticated by a secret token, never meant to be
/// called from an arbitrary browser tab.
fn build_cors_layer(store: SharedStore) -> CorsLayer {
    CorsLayer::new()
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE])
        .allow_origin(AllowOrigin::predicate(move |origin, parts| {
            let Some(pk) = public_orders_route_pk(parts.uri.path()) else {
                return false;
            };
            let Ok(origin_str) = origin.to_str() else {
                return false;
            };
            let store = store.lock().unwrap();
            matches!(
                store.find_tenant_by_public_key(pk),
                Ok(Some(tenant)) if tenant.allowed_origins.iter().any(|o| o == origin_str)
            )
        }))
}

/// Extracts `{pk}` from a path if it matches `/api/v1/t/{pk}/orders...` - shared by
/// `build_cors_layer`'s preflight check (which runs before axum's own path-param
/// extraction, on the raw `Parts`) so the same route family stays defined in one
/// place rather than drifting out of sync with the `.route(...)` calls above.
fn public_orders_route_pk(path: &str) -> Option<&str> {
    let mut segments = path.trim_start_matches('/').split('/');
    match (segments.next(), segments.next(), segments.next(), segments.next(), segments.next()) {
        (Some("api"), Some("v1"), Some("t"), Some(pk), Some("orders")) => Some(pk),
        _ => None,
    }
}

pub use crate::now_unix;

pub use crate::network::{network_str, parse_network};

pub fn parse_status_query(s: &str) -> Result<OrderStatus, ApiError> {
    match s {
        "pending" => Ok(OrderStatus::Pending),
        "unconfirmed" => Ok(OrderStatus::Unconfirmed),
        "confirming" => Ok(OrderStatus::Confirming),
        "paid" => Ok(OrderStatus::Paid),
        "partial" => Ok(OrderStatus::Partial),
        "overpaid" => Ok(OrderStatus::Overpaid),
        "expired" => Ok(OrderStatus::Expired),
        other => Err(ApiError::BadRequest(format!("unknown status filter: {other}"))),
    }
}

/// Resolves a tenant *entirely* from the presented `sk_` bearer token - never from
/// any path parameter. This is the structural fix from `docs/DESIGN.md` §10.1: there
/// is nothing in a `/api/v1/admin/tenant/*` route for a leaked or guessed identifier
/// to authorize, because none of them accept one.
pub struct AuthedTenant(pub Tenant);

impl FromRequestParts<AppState> for AuthedTenant {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let header_value = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(ApiError::Unauthorized)?;
        let token = header_value.strip_prefix("Bearer ").ok_or(ApiError::Unauthorized)?;
        let tenant = state
            .store
            .lock()
            .unwrap()
            .find_tenant_by_secret_token(token)?
            .ok_or(ApiError::Unauthorized)?;
        Ok(AuthedTenant(tenant))
    }
}

/// Ensures a tenant has a live `WalletHandle` in this process, registering it with
/// `KeyCustody` from its sealed material on first use if it doesn't yet.
pub async fn resolve_wallet_handle(state: &AppState, tenant: &Tenant) -> Result<WalletHandle, ApiError> {
    if let Some(handle) = state.wallet_handles.read().unwrap().get(&tenant.id).copied() {
        return Ok(handle);
    }
    // The registration can't happen under the lock (it's `async`, and holding a
    // std `RwLock` across an `.await` would be a deadlock waiting to happen), so two
    // concurrent first-uses of the same tenant can both reach here. Re-check under
    // the write lock and keep whichever landed first: without this, the loser's
    // handle is silently overwritten in the map while the key material it registered
    // stays live in `KeyCustody` forever, unreachable and unremovable - a leaked
    // extra copy of a tenant's private view key, and one that `delete_own_tenant`
    // would then fail to clean up on offboarding.
    let handle = state.key_custody.unseal_and_register(&tenant.sealed_key_material).await?;
    let winner = {
        let mut handles = state.wallet_handles.write().unwrap();
        *handles.entry(tenant.id.clone()).or_insert(handle)
    };
    if winner != handle {
        let _ = state.key_custody.remove_wallet(handle).await;
    }
    Ok(winner)
}

#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    NotFound,
    Forbidden(String),
    BadRequest(String),
    Internal(String),
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => ApiError::NotFound,
            StoreError::Sqlite(e) => ApiError::Internal(e.to_string()),
        }
    }
}

impl From<KeyCustodyError> for ApiError {
    fn from(e: KeyCustodyError) -> Self {
        match e {
            KeyCustodyError::UnknownWallet => ApiError::NotFound,
            other => ApiError::Internal(other.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
