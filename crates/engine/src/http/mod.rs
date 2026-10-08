//! The HTTP API surface. See `docs/DESIGN.md` §10.
//!
//! Three simplifications worth naming up front, all reasonable for this pass and
//! all noted rather than hidden:
//!
//! 1. Authentication and status reads use independent read-only SQLite
//!    connections on bounded worker threads. Other handlers still use the
//!    shared writer connection through `state.store.lock()`. Those synchronous
//!    calls can block an async worker during a slow database operation; moving
//!    the remaining writes to a dedicated worker is still required.
//! 2. `wallet_handles` is populated eagerly at boot by `main` (via
//!    `Store::list_active_tenants` + `KeyCustody::unseal_and_register`), with the
//!    lazy path in `resolve_wallet_handle` kept as a fallback for a tenant created
//!    after boot.
//! 3. Rate limiting (`rate_limit`) is a fixed-window per-token counter, not a proper
//!    token bucket - simpler, and sufficient for the actual goal (§DESIGN.md §12).
//!
//! **The engine is private** (`docs/DESIGN.md` §4 and the monokulo boundary
//! section): the only thing meant to reach it is monokulo. Every request
//! must carry the engine token (`ENGINE_TOKEN`) in
//! [`shared::auth::ENGINE_TOKEN_HEADER`], checked before any route
//! ([`engine_token_middleware`]); a store's routes also need that store's
//! `sk_`. There are no public (`/api/v1/t/{pk}/...`) routes, no CORS and no
//! per-origin checks; everything a customer's browser, a merchant or a
//! plugin touches is served by monokulo.
//!
//! The engine serves plain HTTP; TLS, where monokulo isn't on the same
//! machine, is a proxy or tunnel in front of it (`deploy/sev-snp/README.md`,
//! decision 40 of the snp-key-custody workpack).

mod activity;
mod admin;
pub mod instance_admin;
mod logs;
mod orders;
pub mod rate_limit;
mod status_page;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/verification/http/tests.rs"]
mod tests;

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{FromRef, FromRequestParts};
use axum::http::{header, request::Parts, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use serde_json::json;
use tower_http::limit::RequestBodyLimitLayer;

use crate::daemon::DaemonError;
use crate::key_custody::{KeyCustody, KeyCustodyError, WalletHandle};
use crate::scanner_status::ScannerStatusMap;
use crate::status::OrderStatus;
use crate::store::{StoreError, Tenant};

use rate_limit::{admin_rate_limit_middleware, RateLimiter};

/// Key custody as the API uses it.
#[derive(Clone)]
pub struct Custody {
    pub backends: Arc<dyn KeyCustody>,
    /// The `[key_custody].backend` value new tenants' keys are sealed in, so
    /// `admin::create_tenant` records which backend actually holds a new
    /// tenant's keys (`tenants.key_custody_backend`). Configured, not
    /// derived: nothing about `Arc<dyn KeyCustody>` says which
    /// implementation it is, by design.
    pub default_backend: String,
    /// Each registered tenant's wallet handle.
    pub wallet_handles: Arc<RwLock<HashMap<crate::store::TenantId, WalletHandle>>>,
    /// The `snp` backend's slot, for what only it does (its trust policy,
    /// handing its master key over); `None` where it can't run (tests,
    /// tools).
    pub snp: Option<Arc<crate::key_custody::SnpSlot>>,
}

/// The networks the engine serves.
#[derive(Clone)]
pub struct Networks {
    /// One `FallbackDaemonClient` per configured network, swapped whole when
    /// node settings are saved (`admin_settings_v2.md` task 2.1). A network is
    /// "configured" exactly when it has a client here: a tenant can only be
    /// created for one (`admin::create_tenant`), otherwise its address would
    /// be derived but never scanned. The same clients the scan loops use, so
    /// `GET /status` reports on the nodes scanning really uses.
    pub daemons: crate::engine_settings::Daemons,
    /// Live scan-tick history per network, updated by the scan loop after
    /// every tick - see `scanner_status`'s own module doc comment.
    pub scanner_status: ScannerStatusMap,
}

/// Everything the handlers share. A handler takes just the parts it uses
/// (`State<Database>`, `State<Custody>`, ...: see the `FromRef` derive), so
/// its signature says what it can touch.
#[derive(Clone, FromRef)]
pub struct AppState {
    /// The database: reads on the read pool, writes on the database worker
    /// (the `Admin` class, in turn with the scanner's and webhooks' work),
    /// and order-change notifications. Handlers never hold the shared store.
    pub db: crate::store::Database,
    /// Key custody: the backends, which one new stores use, and the
    /// wallet handles registered so far.
    pub custody: Custody,
    /// Per-`sk_`-token budget for every route (falling back to the caller's
    /// address for a request without a token) - see `http::rate_limit`'s own
    /// module doc comment for why token-keying is the right shape here.
    pub admin_rate_limiter: Arc<RateLimiter<String>>,
    /// The nodes of each network and the scanner's status on them.
    pub networks: Networks,
    /// Every engine setting, live (`admin_settings_v2.md` part 1): handlers
    /// and loops read the current value of what they need on each use.
    pub settings: Arc<crate::engine_settings::EngineSettings>,
    /// This process's log store, read by `GET /api/v1/admin/logs` for
    /// monokulo's Logs page (`structured_logging.md` 3.3). `None` in tests
    /// and when it couldn't be opened.
    pub log_store: Option<telemetry::store::LogStore>,
    /// The hash of the engine token (`ENGINE_TOKEN`), which
    /// every request must carry ([`engine_token_middleware`]).
    pub engine_token: Arc<shared::auth::TokenHash>,
}

/// The engine token every test state (and `engine-test-support`'s
/// engines) accepts.
#[cfg(any(test, feature = "test-support"))]
pub use shared::auth::TEST_ENGINE_TOKEN;

#[cfg(test)]
impl AppState {
    /// A state for tests: an in-memory store, plain key custody, a fake
    /// mainnet node (tenants default to mainnet, so it must be configured),
    /// a rate limit high enough that no test trips it by accident, default
    /// settings and no log store. A test that needs something else
    /// overrides just that field with struct update syntax:
    /// `AppState { log_store, ..AppState::for_tests() }`.
    pub fn for_tests() -> Self {
        Self::for_tests_with_store(crate::store::Store::open_in_memory().unwrap().into_shared())
    }

    /// [`AppState::for_tests`] around a store the test prepared itself (for
    /// example to load real settings from it first).
    pub fn for_tests_with_store(store: crate::store::SharedStore) -> Self {
        let mainnet_daemon = Arc::new(crate::daemon_fallback::FallbackDaemonClient::new(vec![
            crate::daemon_fallback::FallbackNode {
                label: "fake-node:18081".to_owned(),
                client: Arc::new(crate::daemon::fake::FakeDaemonClient::new()),
            },
        ]));
        Self {
            db: crate::store::Database::inline(store),
            admin_rate_limiter: Arc::new(RateLimiter::new(10_000)),
            settings: crate::engine_settings::EngineSettings::defaults(),
            log_store: None,
            engine_token: Arc::new(shared::auth::RawToken::presented(TEST_ENGINE_TOKEN).hash()),
            custody: Custody {
                backends: Arc::new(crate::key_custody::PlainKeyCustody::default()),
                default_backend: "plain".to_owned(),
                wallet_handles: Arc::default(),
                snp: None,
            },
            networks: Networks {
                daemons: crate::engine_settings::Daemons::fixed(HashMap::from([(
                    monero::Network::Mainnet,
                    mainnet_daemon,
                )])),
                scanner_status: crate::scanner_status::new_scanner_status_map(),
            },
        }
    }
}

pub fn build_router(state: AppState, max_body_bytes: usize) -> Router {
    // The engine is private: every route here is for monokulo (or an operator
    // on the same private network), never a browser. No CORS layer, no
    // public routes.
    //
    // No store credential (the engine token is checked for every route,
    // below): tenant creation (monokulo provisions a store's tenant before it
    // has any `sk_`) and `/status` (a JSON status *API* - monokulo's own
    // `GET /status` calls it and renders the real, styled page; it reports
    // node/scanner health across every configured network, not tenant-scoped
    // data, so it sits outside the versioned `/api/v1/...` prefix like a
    // service's own `/healthz`). Both share the admin limiter, which keys a
    // request without an `sk_` on its address.
    let unauthenticated_router = Router::new()
        .route("/api/v1/admin/tenants", post(admin::create_tenant))
        .route(
            "/api/v1/admin/key-custody/bundle",
            post(admin::create_key_bundle),
        )
        .route("/status", get(status_page::status_page));
    // An upgraded engine image asking for the snp master key.
    #[cfg(feature = "snp")]
    let unauthenticated_router = unauthenticated_router.route(
        "/api/v1/admin/key-custody/handoff",
        post(admin::answer_handoff),
    );
    let unauthenticated_router = unauthenticated_router.layer(middleware::from_fn_with_state(
        state.clone(),
        admin_rate_limit_middleware,
    ));

    // Every route here requires a real `Authorization: Bearer sk_...` (see
    // `AuthedTenant`), so each gets its own per-token budget.
    let admin_router = Router::new()
        .route(
            "/api/v1/admin/tenant",
            get(admin::get_own_tenant)
                .patch(admin::patch_own_tenant)
                .delete(admin::delete_own_tenant),
        )
        .route(
            "/api/v1/admin/tenant/rotate-secret",
            post(admin::rotate_secret),
        )
        .route(
            "/api/v1/admin/tenant/key-custody",
            axum::routing::put(admin::switch_key_custody),
        )
        .route(
            "/api/v1/admin/tenant/key-custody/bundle",
            post(admin::move_key_bundle),
        )
        .route(
            "/api/v1/admin/tenant/orders",
            get(admin::list_orders).post(orders::create_order_for_admin),
        )
        .route(
            "/api/v1/admin/tenant/orders/{order_id}",
            get(admin::get_order_detail),
        )
        .route(
            "/api/v1/admin/tenant/orders/{order_id}/refund-address",
            post(admin::set_order_refund_address),
        )
        .route(
            "/api/v1/admin/tenant/payments/lookup",
            post(admin::lookup_payment),
        )
        .route(
            "/api/v1/admin/tenant/webhooks",
            get(admin::list_webhooks).post(admin::create_webhook),
        )
        .route(
            "/api/v1/admin/tenant/webhooks/{webhook_id}",
            delete(admin::delete_webhook),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin_rate_limit_middleware,
        ));

    // The instance-wide settings and logs API: nothing beyond the engine
    // token every route needs, unlike the store routes above, which also
    // authenticate one specific *tenant* by its `sk_`. Shares the admin group's
    // per-token rate limit rather than a bucket of its own - this is
    // exactly the kind of low-volume, human-driven traffic
    // (`admin_rate_limit_middleware`'s own generous default) that budget
    // already exists for.
    let instance_admin_router = Router::new()
        .route(
            "/api/v1/admin/settings",
            get(instance_admin::get_settings).post(instance_admin::update_settings),
        )
        .route(
            "/api/v1/admin/settings/reload",
            post(instance_admin::reload_settings),
        )
        .route(
            "/api/v1/admin/settings/check",
            post(instance_admin::check_settings),
        )
        .route(
            "/api/v1/admin/proof/{network}/anchor",
            delete(instance_admin::forget_anchor),
        )
        // This engine's logs, for monokulo's Logs page (structured_logging.md 3.3).
        .route("/api/v1/admin/logs", get(logs::list))
        .route("/api/v1/admin/logs/trace/{trace_id}", get(logs::trace))
        .route("/api/v1/admin/logs/histogram", get(logs::histogram))
        .route("/api/v1/admin/logs/attributes", get(logs::attributes))
        // What the scanner has been doing, for monokulo's engine page
        // (docs/engine_visualizer.md).
        .route("/api/v1/admin/engine/activity", get(activity::activity))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin_rate_limit_middleware,
        ));

    // The long-lived order-event stream (one per store monokulo is watching)
    // is kept out of the request limits below: a request timeout would cut
    // streams off, and a shared concurrency limit would fill up with them.
    // Streams get a cap of their own instead (task 7.10).
    let limits = RequestLimits::default();
    let events_router = Router::new()
        .route("/api/v1/admin/tenant/events", get(admin::order_events))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin_rate_limit_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            limits.clone(),
            stream_limit_middleware,
        ));

    unauthenticated_router
        .merge(admin_router)
        .merge(instance_admin_router)
        .layer(middleware::from_fn_with_state(
            limits,
            request_limit_middleware,
        ))
        .merge(events_router)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            body_limit_middleware,
        ))
        // A fixed outer ceiling; the live limit above (server.max_body_bytes,
        // task 2.6) is what normally applies.
        .layer(RequestBodyLimitLayer::new(max_body_bytes))
        // Before anything else looks at a request, so nothing without the
        // engine token reaches a route, a limit or the database.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            engine_token_middleware,
        ))
        // Outermost, so every line from the layers above carries the
        // request's span (structured_logging.md 2.1).
        .layer(middleware::from_fn(telemetry::http::server))
        .with_state(state)
}

/// [`build_router`] as monokulo reaches it: every request carries the
/// engine token ([`TEST_ENGINE_TOKEN`], which the test states accept).
/// Tests of the token itself use [`build_router`].
#[cfg(test)]
pub(crate) fn router_as_monokulo(state: AppState, max_body_bytes: usize) -> Router {
    async fn with_engine_token(mut request: axum::extract::Request) -> axum::extract::Request {
        request
            .headers_mut()
            .entry(shared::auth::ENGINE_TOKEN_HEADER)
            .or_insert(axum::http::HeaderValue::from_static(TEST_ENGINE_TOKEN));
        request
    }
    build_router(state, max_body_bytes).layer(middleware::map_request(with_engine_token))
}

/// Refuses, with `401`, any request whose
/// [`shared::auth::ENGINE_TOKEN_HEADER`] isn't the engine token:
/// only monokulo, which is given it, may talk to the engine.
async fn engine_token_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    use subtle::ConstantTimeEq as _;
    let accepted = request
        .headers()
        .get(shared::auth::ENGINE_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|presented| {
            // Both sides hashed, so the comparison is over equal lengths.
            let presented = shared::auth::RawToken::presented(presented).hash();
            bool::from(
                presented
                    .as_str()
                    .as_bytes()
                    .ct_eq(state.engine_token.as_str().as_bytes()),
            )
        });
    if accepted {
        next.run(request).await
    } else {
        ApiError::Unauthorized.into_response()
    }
}

/// Refuses a request whose body is larger than the current
/// `server.max_body_bytes`, read on every request so a saved change applies
/// to the next one (task 2.6). A declared length over the limit is refused
/// before anything is read. A body of unknown length (chunked) is read here
/// up to the limit and refused with `413` if it goes over (`400` if it can't
/// be read at all).
async fn body_limit_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    use http_body::Body as _;
    let limit = state.settings.limits.load().max_body_bytes as u64;
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| request.body().size_hint().exact());
    if declared.is_some_and(|len| len > limit) {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({ "error": "request body too large" })),
        )
            .into_response();
    }
    if declared.is_some() {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    match axum::body::to_bytes(body, usize::try_from(limit).unwrap_or(usize::MAX)).await {
        Ok(bytes) => {
            next.run(axum::extract::Request::from_parts(
                parts,
                axum::body::Body::from(bytes),
            ))
            .await
        }
        // Over the limit is `413`; a connection that dropped or a body that
        // couldn't be read is the client's `400`, not an oversized request.
        Err(e)
            if std::error::Error::source(&e).is_some_and(
                <dyn std::error::Error + 'static>::is::<http_body_util::LengthLimitError>,
            ) =>
        {
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(serde_json::json!({ "error": "request body too large" })),
            )
                .into_response()
        }
        Err(_) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "request body could not be read" })),
        )
            .into_response(),
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
        Self::new(MAX_CONCURRENT_REQUESTS, MAX_OPEN_STREAMS, REQUEST_TIMEOUT)
    }
}

impl RequestLimits {
    pub fn new(max_requests: usize, max_streams: usize, timeout: std::time::Duration) -> Self {
        Self {
            requests: Arc::new(tokio::sync::Semaphore::new(max_requests)),
            streams: Arc::new(tokio::sync::Semaphore::new(max_streams)),
            timeout,
        }
    }
}

fn service_unavailable(message: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

async fn request_limit_middleware(
    axum::extract::State(limits): axum::extract::State<RequestLimits>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    let Ok(_permit) = Arc::clone(&limits.requests).try_acquire_owned() else {
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
) -> Response {
    let Ok(permit) = Arc::clone(&limits.streams).try_acquire_owned() else {
        return service_unavailable("too many open event streams, try again shortly");
    };
    let (parts, body) = next.run(request).await.into_parts();
    // The permit lives as long as the stream's body, and is released when
    // the client disconnects and the body is dropped.
    Response::from_parts(
        parts,
        axum::body::Body::new(PermitBody {
            inner: body,
            _permit: permit,
        }),
    )
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
        other => Err(ApiError::BadRequest(format!(
            "unknown status filter: {other}"
        ))),
    }
}

/// Resolves a tenant *entirely* from the presented `sk_` bearer token - never from
/// any path parameter.
///
/// This is the structural fix from `docs/DESIGN.md` §10.1: there is nothing in a
/// `/api/v1/admin/tenant/*` route for a leaked or guessed identifier to authorize,
/// because none of them accept one.
pub struct AuthedTenant(pub Tenant);

impl FromRequestParts<AppState> for AuthedTenant {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header_value = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(ApiError::Unauthorized)?;
        let token = shared::auth::RawToken::presented(
            header_value
                .strip_prefix("Bearer ")
                .ok_or(ApiError::Unauthorized)?,
        );
        let tenant = state
            .db
            .read(move |store| store.find_tenant_by_secret_token(&token))
            .await?
            .ok_or(ApiError::Unauthorized)?;
        // The request's lines name the store (`telemetry::http::server`).
        tracing::Span::current().record("store.id", tenant.id.as_str());
        Ok(Self(tenant))
    }
}

/// Ensures a tenant has a live `WalletHandle` in this process, registering it with
/// `KeyCustody` from its sealed material on first use if it doesn't yet.
pub async fn resolve_wallet_handle(
    state: &AppState,
    tenant: &Tenant,
) -> Result<WalletHandle, ApiError> {
    let known = state.custody.wallet_handles.read().get(&tenant.id).copied();
    match known {
        Some(handle) if state.custody.backends.handle_is_live(handle) => return Ok(handle),
        // Its backend lost it (restarted, or was disabled): register again.
        Some(handle) => forget_wallet_handle(state, &tenant.id, handle),
        None => {}
    }
    // The row as it is now, not as it was when the request was
    // authenticated: the store may have just moved to another backend, or
    // been deleted. A deleted store's keys are never registered again: the
    // request that authenticated just before the deletion would otherwise
    // put them back in custody with nothing left to ever remove them.
    let id = tenant.id.clone();
    let current = state.db.write(move |s| s.get_tenant_by_id(&id)).await?;
    let tenant = match current.as_ref() {
        Some(current) if current.disabled_at.is_none() => current,
        _ => return Err(ApiError::Unauthorized),
    };
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
    let handle = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        state.custody.backends.unseal_and_register_in_idempotent(
            &tenant.key_custody_backend,
            &tenant.sealed_key_material,
            tenant.id.as_str(),
        ),
    )
    .await
    .map_err(|elapsed| ApiError::Internal(format!("key custody registration: {elapsed}")))??;
    let winner = {
        let mut handles = state.custody.wallet_handles.write();
        *handles.entry(tenant.id.clone()).or_insert(handle)
    };
    if winner != handle {
        crate::key_custody::remove_wallet_logged(
            state.custody.backends.as_ref(),
            handle,
            Some(tenant.id.as_str()),
            "registering a store's keys, another request registered them first",
        )
        .await;
    }
    Ok(winner)
}

/// Drops `handle` as `tenant_id`'s live handle, if it still is, so the next
/// `resolve_wallet_handle` registers the store's keys again.
pub fn forget_wallet_handle(
    state: &AppState,
    tenant_id: &crate::store::TenantId,
    handle: WalletHandle,
) {
    let mut handles = state.custody.wallet_handles.write();
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
    /// The request clashes with one already made (an idempotency key
    /// reused for a different order): `409`.
    Conflict(String),
    Internal(String),
    /// Something this request needs is down for now (the database is full
    /// or locked, a key-custody backend is unreachable): `503`, so callers
    /// know to retry (task 7.7).
    Unavailable(String),
}

/// SQLite failures that are about the environment (disk, locks, I/O) rather
/// than the request, and so worth a retry later.
fn is_transient_sqlite(e: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode::{
        CannotOpen, DatabaseBusy, DatabaseLocked, DiskFull, OutOfMemory, SystemIoFailure,
    };
    matches!(
        e.sqlite_error_code(),
        Some(DiskFull | DatabaseBusy | DatabaseLocked | SystemIoFailure | CannotOpen | OutOfMemory)
    )
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => Self::NotFound,
            StoreError::AddressAllocation(e) => Self::Conflict(e),
            StoreError::Sqlite(e) if is_transient_sqlite(&e) => Self::Unavailable(e.to_string()),
            StoreError::Sqlite(e) => Self::Internal(e.to_string()),
            StoreError::WorkerUnavailable(e) => Self::Unavailable(e),
        }
    }
}

impl From<KeyCustodyError> for ApiError {
    fn from(e: KeyCustodyError) -> Self {
        match e {
            // The store exists; its keys just aren't registered in this
            // process right now (a backend restart, a switch in progress).
            KeyCustodyError::UnknownWallet => {
                Self::Unavailable("this store's keys are not available right now".to_owned())
            }
            KeyCustodyError::BackendUnavailable(m) => Self::Unavailable(m),
            other @ (KeyCustodyError::InvalidKeyMaterial(_) | KeyCustodyError::ScanFailed(_)) => {
                Self::Internal(other.to_string())
            }
        }
    }
}

/// A scan done on a caller's behalf (`admin::lookup_payment`): the node and
/// the database are the engine's to retry (`503`), the rest is its own
/// bug (`500`).
impl From<crate::scanner::ScannerError> for ApiError {
    fn from(e: crate::scanner::ScannerError) -> Self {
        use crate::scanner::ScannerError;
        match e {
            ScannerError::Daemon(e) => e.into(),
            ScannerError::Store(e) => e.into(),
            ScannerError::KeyCustody(e) => e.into(),
            other @ (ScannerError::InvalidPaymentEvidence(_) | ScannerError::Internal(_)) => {
                Self::Internal(other.to_string())
            }
        }
    }
}

/// A Monero node that didn't answer is down for now, not a bug: `503`.
impl From<DaemonError> for ApiError {
    fn from(e: DaemonError) -> Self {
        Self::Unavailable(format!("the Monero node did not answer: {e}"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_owned()),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found".to_owned()),
            Self::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::Conflict(m) => (StatusCode::CONFLICT, m),
            // What went wrong is for the engine's own log, not the caller:
            // a database or custody message can carry SQL, file paths and
            // node addresses.
            Self::Internal(m) => {
                tracing::error!(error = %m, "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_owned(),
                )
            }
            Self::Unavailable(m) => (StatusCode::SERVICE_UNAVAILABLE, m),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
