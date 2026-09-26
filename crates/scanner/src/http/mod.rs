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
//! 3. Rate limiting (`rate_limit`) is a fixed-window per-token counter, not a proper
//!    token bucket - simpler, and sufficient for the actual goal (§DESIGN.md §12).
//!
//! **The engine is private** (`docs/DESIGN.md` §4 and the monokulo boundary
//! section): the only thing meant to reach it is monokulo, through the
//! `sk_`-authenticated admin API plus `/status`. There are no public
//! (`/api/v1/t/{pk}/...`) routes, no CORS and no per-origin checks; everything
//! a customer's browser, a merchant or a plugin touches is served by monokulo.
//!
//! Not implemented in this pass: TLS termination (expected to sit behind a reverse
//! proxy or terminate via `rustls` in `main`, not implemented here).

mod admin;
pub mod instance_admin;
mod orders;
pub mod rate_limit;
mod status_page;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::extract::FromRequestParts;
use axum::http::{StatusCode, header, request::Parts};
use axum::middleware;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use serde_json::json;
use tower_http::limit::RequestBodyLimitLayer;

use crate::daemon_fallback::FallbackDaemonClient;
use crate::key_custody::{KeyCustody, KeyCustodyError, WalletHandle};
use crate::scanner_status::ScannerStatusMap;
use crate::status::OrderStatus;
use crate::store::{SharedStore, StoreError, Tenant};

use rate_limit::{admin_rate_limit_middleware, RateLimiter};

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
    /// Per-`sk_`-token budget for every route (falling back to the caller's
    /// address for a request without a token) - see `http::rate_limit`'s own
    /// module doc comment for why token-keying is the right shape here.
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
    // The engine is private: every route here is for monokulo (or an operator
    // on the same private network), never a browser. No CORS layer, no
    // public routes.
    //
    // Unauthenticated, but reachable only by monokulo now (see the module doc
    // comment): tenant creation (monokulo provisions a store's tenant before
    // it has any `sk_`) and `/status` (a JSON status *API* - monokulo's own
    // `GET /status` calls it and renders the real, styled page; it reports
    // node/scanner health across every configured network, not tenant-scoped
    // data, so it sits outside the versioned `/api/v1/...` prefix like a
    // service's own `/healthz`). Both share the admin limiter, which keys a
    // token-less request on its address.
    let unauthenticated_router = Router::new()
        .route("/api/v1/admin/tenants", post(admin::create_tenant))
        .route("/status", get(status_page::status_page))
        .layer(middleware::from_fn_with_state(state.clone(), admin_rate_limit_middleware));

    // Every route here requires a real `Authorization: Bearer sk_...` (see
    // `AuthedTenant`), so each gets its own per-token budget.
    let admin_router = Router::new()
        .route(
            "/api/v1/admin/tenant",
            get(admin::get_own_tenant).patch(admin::patch_own_tenant).delete(admin::delete_own_tenant),
        )
        .route("/api/v1/admin/tenant/rotate-secret", post(admin::rotate_secret))
        .route("/api/v1/admin/tenant/orders", get(admin::list_orders).post(orders::create_order_for_admin))
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
    // per-token rate limit rather than a bucket of its own - this is
    // exactly the kind of low-volume, human-driven traffic
    // (`admin_rate_limit_middleware`'s own generous default) that budget
    // already exists for.
    let instance_admin_router = Router::new()
        .route(
            "/api/v1/admin/settings",
            get(instance_admin::get_settings).post(instance_admin::update_settings),
        )
        .layer(middleware::from_fn_with_state(state.clone(), admin_rate_limit_middleware));

    unauthenticated_router
        .merge(admin_router)
        .merge(instance_admin_router)
        .layer(RequestBodyLimitLayer::new(max_body_bytes))
        .with_state(state)
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
