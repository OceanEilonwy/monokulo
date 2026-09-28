//! The HTTP API surface. See `docs/DESIGN.md` §10.
//!
//! Three simplifications worth naming up front, all reasonable for this pass and
//! all noted rather than hidden:
//!
//! 1. `Store` access from handlers goes through `state.store.lock()`
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
mod logs;
mod orders;
pub mod rate_limit;
mod status_page;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use parking_lot::RwLock;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::{StatusCode, header, request::Parts};
use axum::middleware;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use serde_json::json;
use tower_http::limit::RequestBodyLimitLayer;

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
    /// One `FallbackDaemonClient` per configured network, swapped whole when
    /// node settings are saved (admin_settings_v2.md task 2.1). A network
    /// is "configured" exactly when it has a client here: a tenant can only
    /// be created for one (`admin::create_tenant`), otherwise its address
    /// would be derived but never scanned. The same clients the scan loops
    /// use, so `GET /status` reports on the nodes scanning really uses.
    pub daemons: crate::engine_settings::Daemons,
    /// Live scan-tick history per network, updated by `main.rs`'s own scan
    /// loop after every tick - see `scanner_status`'s own module doc
    /// comment.
    pub scanner_status: ScannerStatusMap,
    /// Every engine setting, live (admin_settings_v2.md part 1): handlers
    /// and loops read the current value of what they need on each use.
    pub settings: Arc<crate::engine_settings::EngineSettings>,
    /// This process's log store, read by `GET /api/v1/admin/logs` for
    /// monokulo's Logs page (structured_logging.md 3.3). `None` in tests
    /// and when it couldn't be opened.
    pub log_store: Option<telemetry::store::LogStore>,
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
        .route("/api/v1/admin/key-custody", get(admin::key_custody_options))
        .layer(middleware::from_fn_with_state(state.clone(), admin_rate_limit_middleware));

    // Every route here requires a real `Authorization: Bearer sk_...` (see
    // `AuthedTenant`), so each gets its own per-token budget.
    let admin_router = Router::new()
        .route(
            "/api/v1/admin/tenant",
            get(admin::get_own_tenant).patch(admin::patch_own_tenant).delete(admin::delete_own_tenant),
        )
        .route("/api/v1/admin/tenant/rotate-secret", post(admin::rotate_secret))
        .route("/api/v1/admin/tenant/key-custody", axum::routing::put(admin::switch_key_custody))
        .route("/api/v1/admin/tenant/orders", get(admin::list_orders).post(orders::create_order_for_admin))
        .route("/api/v1/admin/tenant/orders/{order_id}", get(admin::get_order_detail))
        .route(
            "/api/v1/admin/tenant/orders/{order_id}/refund-address",
            post(admin::set_order_refund_address),
        )
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
        // This engine's logs, for monokulo's Logs page (structured_logging.md 3.3).
        .route("/api/v1/admin/logs", get(logs::list))
        .route("/api/v1/admin/logs/trace/{trace_id}", get(logs::trace))
        .route("/api/v1/admin/logs/histogram", get(logs::histogram))
        .route("/api/v1/admin/logs/attributes", get(logs::attributes))
        .layer(middleware::from_fn_with_state(state.clone(), admin_rate_limit_middleware));

    // The long-lived order-event stream (one per store monokulo is watching)
    // is kept out of the request limits below: a request timeout would cut
    // streams off, and a shared concurrency limit would fill up with them.
    // Streams get a cap of their own instead (task 7.10).
    let limits = RequestLimits::default();
    let events_router = Router::new()
        .route("/api/v1/admin/tenant/events", get(admin::order_events))
        .layer(middleware::from_fn_with_state(state.clone(), admin_rate_limit_middleware))
        .layer(middleware::from_fn_with_state(limits.clone(), stream_limit_middleware));

    unauthenticated_router
        .merge(admin_router)
        .merge(instance_admin_router)
        .layer(middleware::from_fn_with_state(limits, request_limit_middleware))
        .merge(events_router)
        .layer(middleware::from_fn_with_state(state.clone(), body_limit_middleware))
        // A fixed outer ceiling; the live limit above (server.max_body_bytes,
        // task 2.6) is what normally applies.
        .layer(RequestBodyLimitLayer::new(max_body_bytes))
        // Outermost, so every line from the layers above carries the
        // request's span (structured_logging.md 2.1).
        .layer(middleware::from_fn(telemetry::http::server))
        .with_state(state)
}

/// Refuses a request whose body is larger than the current
/// `server.max_body_bytes`, read on every request so a saved change applies
/// to the next one (task 2.6). A declared length over the limit is refused
/// before anything is read. A body of unknown length (chunked) is read here
/// up to the limit and refused with `413` if it goes over, or can't be read.
async fn body_limit_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    use http_body::Body as _;
    let limit = state.settings.limits.load().max_body_bytes as u64;
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| request.body().size_hint().exact());
    if declared.is_some_and(|len| len > limit) {
        return (StatusCode::PAYLOAD_TOO_LARGE, Json(serde_json::json!({ "error": "request body too large" })))
            .into_response();
    }
    if declared.is_some() {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    match axum::body::to_bytes(body, usize::try_from(limit).unwrap_or(usize::MAX)).await {
        Ok(bytes) => next.run(axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes))).await,
        Err(_) => (StatusCode::PAYLOAD_TOO_LARGE, Json(serde_json::json!({ "error": "request body too large" }))).into_response(),
    }
}

/// Longest an ordinary API request may take before it gets `503` (task 7.10).
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Most ordinary API requests handled at once; more get `503` straight away
/// rather than queueing without bound.
pub const MAX_CONCURRENT_REQUESTS: usize = 256;
/// Most order-event streams open at once.
pub const MAX_OPEN_STREAMS: usize = 4096;

/// Shared limits for one router (task 7.10). The engine degrades under
/// pressure by refusing work with `503`, never by piling requests up.
#[derive(Clone)]
pub struct RequestLimits {
    requests: Arc<tokio::sync::Semaphore>,
    streams: Arc<tokio::sync::Semaphore>,
    timeout: std::time::Duration,
}

impl Default for RequestLimits {
    fn default() -> Self {
        RequestLimits::new(MAX_CONCURRENT_REQUESTS, MAX_OPEN_STREAMS, REQUEST_TIMEOUT)
    }
}

impl RequestLimits {
    pub fn new(max_requests: usize, max_streams: usize, timeout: std::time::Duration) -> Self {
        RequestLimits {
            requests: Arc::new(tokio::sync::Semaphore::new(max_requests)),
            streams: Arc::new(tokio::sync::Semaphore::new(max_streams)),
            timeout,
        }
    }
}

fn service_unavailable(message: &str) -> axum::response::Response {
    (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({ "error": message }))).into_response()
}

async fn request_limit_middleware(
    axum::extract::State(limits): axum::extract::State<RequestLimits>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let Ok(_permit) = limits.requests.clone().try_acquire_owned() else {
        return service_unavailable("the engine is busy, try again shortly");
    };
    match tokio::time::timeout(limits.timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => service_unavailable("the request took too long"),
    }
}

async fn stream_limit_middleware(
    axum::extract::State(limits): axum::extract::State<RequestLimits>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let Ok(permit) = limits.streams.clone().try_acquire_owned() else {
        return service_unavailable("too many open event streams, try again shortly");
    };
    let (parts, body) = next.run(request).await.into_parts();
    // The permit lives as long as the stream's body, and is released when
    // the client disconnects and the body is dropped.
    axum::response::Response::from_parts(parts, axum::body::Body::new(PermitBody { inner: body, _permit: permit }))
}

/// A response body that holds a semaphore permit until it is dropped.
struct PermitBody {
    inner: axum::body::Body,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl http_body::Body for PermitBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
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
            .find_tenant_by_secret_token(token)?
            .ok_or(ApiError::Unauthorized)?;
        Ok(AuthedTenant(tenant))
    }
}

/// Ensures a tenant has a live `WalletHandle` in this process, registering it with
/// `KeyCustody` from its sealed material on first use if it doesn't yet.
pub async fn resolve_wallet_handle(state: &AppState, tenant: &Tenant) -> Result<WalletHandle, ApiError> {
    let known = state.wallet_handles.read().get(&tenant.id).copied();
    match known {
        Some(handle) if state.key_custody.handle_is_live(handle) => return Ok(handle),
        // Its backend lost it (restarted, or was disabled): register again.
        Some(handle) => forget_wallet_handle(state, &tenant.id, handle),
        None => {}
    }
    // The row as it is now, not as it was when the request was
    // authenticated: the store may have just moved to another backend.
    let current = state.store.lock().get_tenant_by_id(&tenant.id)?;
    let tenant = current.as_ref().unwrap_or(tenant);
    // The registration can't happen under the lock (it's `async`, and holding a
    // std `RwLock` across an `.await` would be a deadlock waiting to happen), so two
    // concurrent first-uses of the same tenant can both reach here. Re-check under
    // the write lock and keep whichever landed first: without this, the loser's
    // handle is silently overwritten in the map while the key material it registered
    // stays live in `KeyCustody` forever, unreachable and unremovable - a leaked
    // extra copy of a tenant's private view key, and one that `delete_own_tenant`
    // would then fail to clean up on offboarding.
    // In the tenant's own backend (part 5). A backend that isn't enabled
    // refuses, so a disabled backend's store isn't quietly brought back.
    let handle = state.key_custody.unseal_and_register_in(&tenant.key_custody_backend, &tenant.sealed_key_material).await?;
    let winner = {
        let mut handles = state.wallet_handles.write();
        *handles.entry(tenant.id.clone()).or_insert(handle)
    };
    if winner != handle {
        let _ = state.key_custody.remove_wallet(handle).await;
    }
    Ok(winner)
}

/// Drops `handle` as `tenant_id`'s live handle, if it still is, so the next
/// `resolve_wallet_handle` registers the store's keys again.
pub fn forget_wallet_handle(state: &AppState, tenant_id: &str, handle: WalletHandle) {
    let mut handles = state.wallet_handles.write();
    if handles.get(tenant_id) == Some(&handle) {
        handles.remove(tenant_id);
    }
}

/// How many times a request re-resolves a store's handle when its backend
/// says the handle is unknown - it was dropped mid-request by a backend
/// restart or a key custody switch (task 5.3).
pub const UNKNOWN_WALLET_RETRIES: usize = 3;

#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    NotFound,
    Forbidden(String),
    BadRequest(String),
    Internal(String),
    /// Something this request needs is down for now (the database is full
    /// or locked, a key-custody backend is unreachable): `503`, so callers
    /// know to retry (task 7.7).
    Unavailable(String),
}

/// SQLite failures that are about the environment (disk, locks, I/O) rather
/// than the request, and so worth a retry later.
fn is_transient_sqlite(e: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode::*;
    matches!(
        e.sqlite_error_code(),
        Some(DiskFull | DatabaseBusy | DatabaseLocked | SystemIoFailure | CannotOpen | OutOfMemory)
    )
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => ApiError::NotFound,
            StoreError::Sqlite(e) if is_transient_sqlite(&e) => ApiError::Unavailable(e.to_string()),
            StoreError::Sqlite(e) => ApiError::Internal(e.to_string()),
        }
    }
}

impl From<KeyCustodyError> for ApiError {
    fn from(e: KeyCustodyError) -> Self {
        match e {
            // The store exists; its keys just aren't registered in this
            // process right now (a backend restart, a switch in progress).
            KeyCustodyError::UnknownWallet => ApiError::Unavailable("this store's keys are not available right now".to_string()),
            KeyCustodyError::BackendUnavailable(m) => ApiError::Unavailable(m),
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
            ApiError::Unavailable(m) => (StatusCode::SERVICE_UNAVAILABLE, m),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
