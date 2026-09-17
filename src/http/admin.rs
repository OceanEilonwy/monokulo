use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};

use crate::auth::generate_webhook_secret;
use crate::daemon::MoneroDaemonClient;
use crate::key_custody::{KeyCustodyError, SubaddressIndex, WalletMaterial};
use crate::status::OrderStatus;
use crate::store::{NewOrderRescan, NewTenant, Order, OrderPaymentRow, OrderRescan, RescanMode, Store, TenantConfigPatch, TriggerRescanOutcome, Webhook};

use super::{resolve_wallet_handle, AppState, ApiError, AuthedTenant, network_str, now_unix, parse_network, parse_status_query};

/// `KeyCustodyError::InvalidKeyMaterial` from `register_wallet`/`derive_subaddress`
/// below means *this request's* `view_key_hex`/`spend_pubkey_hex` decoded to the
/// right length (`WalletMaterial::from_hex` already checked that, a few lines up)
/// but isn't actually a valid point/scalar on the curve - e.g. `PublicKey::from_slice`
/// rejecting a well-formed-but-off-curve spend key. That's still the *caller's*
/// mistake, exactly like a bad hex string is - the blanket `From<KeyCustodyError>
/// for ApiError` (`http/mod.rs`) maps `InvalidKeyMaterial` to `Internal` by design,
/// because at most of its call sites (e.g. `resolve_wallet_handle` unsealing a
/// tenant's own already-validated stored material) that error really would mean
/// server-side data corruption, not a client mistake - so this endpoint needs its
/// own, more specific mapping rather than changing that default for every caller.
fn key_custody_error_for_new_tenant(e: KeyCustodyError) -> ApiError {
    match e {
        KeyCustodyError::InvalidKeyMaterial(m) => ApiError::BadRequest(format!("invalid key material: {m}")),
        other => other.into(),
    }
}

#[derive(Deserialize)]
pub struct CreateTenantRequest {
    view_key_hex: String,
    spend_pubkey_hex: String,
    network: Option<String>,
    allowed_origins: Vec<String>,
    confirmations_required: Option<u64>,
    zero_conf_max_piconero: Option<u64>,
    order_expiry_seconds: Option<i64>,
}

#[derive(Serialize)]
pub struct CreateTenantResponse {
    tenant_id: String,
    public_key: String,
    secret_token: String,
}

pub async fn create_tenant(
    State(state): State<AppState>,
    Json(req): Json<CreateTenantRequest>,
) -> Result<Json<CreateTenantResponse>, ApiError> {
    let material = WalletMaterial::from_hex(&req.view_key_hex, &req.spend_pubkey_hex)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let network = parse_network(req.network.as_deref().unwrap_or("mainnet")).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    if !state.configured_networks.contains(&network) {
        return Err(ApiError::BadRequest(format!(
            "no monero_node is configured for network {:?} on this instance",
            network_str(network)
        )));
    }

    validate_tenant_settings(req.confirmations_required, req.order_expiry_seconds)?;

    let handle =
        state.key_custody.register_wallet(material.clone()).await.map_err(key_custody_error_for_new_tenant)?;
    let primary_address = state
        .key_custody
        .derive_subaddress(handle, SubaddressIndex::default(), network)
        .await
        .map_err(key_custody_error_for_new_tenant)?;
    let sealed = state.key_custody.seal(&material).await.map_err(key_custody_error_for_new_tenant)?;

    let created = state.store.lock().unwrap().create_tenant(
        NewTenant {
            // Not hardcoded "plain" - see `AppState::key_custody_backend`'s own
            // doc comment: this instance may be running with `backend = "socket"`
            // configured, in which case `sealed` above was genuinely produced by
            // the remote `key-custody-server`, not this process.
            key_custody_backend: state.key_custody_backend.clone(),
            sealed_key_material: sealed,
            primary_address: primary_address.to_string(),
            network: network_str(network).to_string(),
            allowed_origins: req.allowed_origins,
            confirmations_required: req.confirmations_required,
            zero_conf_max_piconero: req.zero_conf_max_piconero,
            order_expiry_seconds: req.order_expiry_seconds,
        },
        now_unix(),
    );
    // A failed insert leaves a registered wallet nothing holds a handle to - key
    // material live in `KeyCustody` for the rest of the process's life, with no
    // tenant row to ever offboard it. Hand it back before returning the error.
    let created = match created {
        Ok(created) => created,
        Err(e) => {
            let _ = state.key_custody.remove_wallet(handle).await;
            return Err(e.into());
        }
    };

    state.wallet_handles.write().unwrap().insert(created.tenant.id.clone(), handle);

    Ok(Json(CreateTenantResponse {
        tenant_id: created.tenant.id,
        public_key: created.tenant.public_key,
        secret_token: created.secret_token,
    }))
}

/// Upper bound on a tenant-chosen order lifetime. Well past anything useful (30
/// days), and far enough below `i64::MAX` that `created_at + order_expiry_seconds`
/// cannot overflow for any wall-clock `created_at` this system will ever see -
/// that overflow panics in a debug build and wraps to a deadline *in the past* in a
/// release one, which would mark every one of that tenant's orders expired on
/// creation.
const MAX_ORDER_EXPIRY_SECONDS: i64 = 30 * 24 * 60 * 60;

/// Upper bound on tenant-chosen confirmations. ~24 hours of blocks; anything beyond
/// this is indistinguishable from "never settles".
const MAX_CONFIRMATIONS_REQUIRED: u64 = 720;

/// Shared by tenant creation and tenant patching, because both write the same two
/// columns and a bound enforced on only one of them is not a bound.
///
/// Neither value is dangerous to *us* - a tenant can only misconfigure their own
/// orders - but both have silent failure modes rather than loud ones, which is what
/// makes them worth rejecting at the edge. `confirmations_required = 0` marks an
/// order `Paid` off a transaction still sitting in the mempool, bypassing the
/// `zero_conf_max_piconero` ceiling that exists precisely to bound that risk;
/// `order_expiry_seconds <= 0` produces orders that are already expired when the
/// customer first loads the payment page.
fn validate_tenant_settings(
    confirmations_required: Option<u64>,
    order_expiry_seconds: Option<i64>,
) -> Result<(), ApiError> {
    if let Some(confirmations) = confirmations_required {
        if confirmations == 0 || confirmations > MAX_CONFIRMATIONS_REQUIRED {
            return Err(ApiError::BadRequest(format!(
                "confirmations_required must be between 1 and {MAX_CONFIRMATIONS_REQUIRED}; 0 would treat an unconfirmed transaction as final"
            )));
        }
    }
    if let Some(seconds) = order_expiry_seconds {
        if seconds <= 0 || seconds > MAX_ORDER_EXPIRY_SECONDS {
            return Err(ApiError::BadRequest(format!(
                "order_expiry_seconds must be between 1 and {MAX_ORDER_EXPIRY_SECONDS}; a non-positive value expires every order on creation"
            )));
        }
    }
    Ok(())
}

#[derive(Serialize)]
pub struct TenantView {
    tenant_id: String,
    public_key: String,
    primary_address: String,
    network: String,
    confirmations_required: u64,
    zero_conf_max_piconero: Option<u64>,
    order_expiry_seconds: i64,
    allowed_origins: Vec<String>,
}

impl From<crate::store::Tenant> for TenantView {
    fn from(t: crate::store::Tenant) -> Self {
        TenantView {
            tenant_id: t.id,
            public_key: t.public_key,
            primary_address: t.primary_address,
            network: t.network,
            confirmations_required: t.confirmations_required,
            zero_conf_max_piconero: t.zero_conf_max_piconero,
            order_expiry_seconds: t.order_expiry_seconds,
            allowed_origins: t.allowed_origins,
        }
    }
}

pub async fn get_own_tenant(AuthedTenant(tenant): AuthedTenant) -> Json<TenantView> {
    Json(TenantView::from(tenant))
}

#[derive(Deserialize, Default)]
pub struct PatchTenantRequest {
    allowed_origins: Option<Vec<String>>,
    confirmations_required: Option<u64>,
    zero_conf_max_piconero: Option<u64>,
    order_expiry_seconds: Option<i64>,
}

pub async fn patch_own_tenant(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<PatchTenantRequest>,
) -> Result<Json<TenantView>, ApiError> {
    validate_tenant_settings(req.confirmations_required, req.order_expiry_seconds)?;
    let patch = TenantConfigPatch {
        allowed_origins: req.allowed_origins,
        confirmations_required: req.confirmations_required,
        zero_conf_max_piconero_set: req.zero_conf_max_piconero.is_some(),
        zero_conf_max_piconero: req.zero_conf_max_piconero,
        order_expiry_seconds: req.order_expiry_seconds,
    };
    let store = state.store.lock().unwrap();
    store.update_tenant_config(&tenant.id, patch)?;
    let refetched = store.get_tenant_by_id(&tenant.id)?.ok_or(ApiError::NotFound)?;
    Ok(Json(TenantView::from(refetched)))
}

#[derive(Serialize)]
pub struct RotateSecretResponse {
    secret_token: String,
}

pub async fn rotate_secret(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
) -> Result<Json<RotateSecretResponse>, ApiError> {
    let new_secret = state.store.lock().unwrap().rotate_tenant_secret(&tenant.id)?;
    Ok(Json(RotateSecretResponse { secret_token: new_secret }))
}

pub async fn delete_own_tenant(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    let removed_handle = state.wallet_handles.write().unwrap().remove(&tenant.id);
    if let Some(handle) = removed_handle {
        // Best-effort: an already-unknown handle is not an error worth surfacing here.
        let _ = state.key_custody.remove_wallet(handle).await;
    }
    state.store.lock().unwrap().disable_tenant(&tenant.id, now_unix())?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct OrderView {
    payment_id: String,
    merchant_order_id: Option<String>,
    address: String,
    xmr_amount_piconero: u64,
    amount_received_piconero: u64,
    status: String,
    confirmations: u64,
    double_spend_detected_at: Option<i64>,
    refund_address: Option<String>,
    created_at: i64,
    expires_at: i64,
    updated_at: i64,
    /// `docs/order_rescan_wbs.md` Phase 5.3 - mirrors `Order::first_scanned_height`/
    /// `last_scanned_height` (Phase 5.1) unchanged.
    first_scanned_height: Option<i64>,
    last_scanned_height: Option<i64>,
    /// Computed, not a stored column - `true` if this order is presently examined
    /// by anything at all (`Store::is_order_currently_scanning`): the live
    /// scanner's own in-scope set (non-terminal, or `Expired` within its grace
    /// window), *or* a currently-`running` manual rescan. One engine-computed
    /// boolean rather than a caller re-deriving the same scope logic itself from
    /// the raw fields above.
    currently_scanning: bool,
}

/// Builds an `OrderView`, including the one field (`currently_scanning`) that
/// can't come from `Order` alone - a real `Store` query, since it also depends on
/// `order_rescans` and on wall-clock time (Phase 4's grace window). Deliberately
/// not a plain `From<Order>` impl for that reason - every call site needs the
/// same `now`/`grace_period_seconds` a caller-supplied `impl From` has no way to
/// thread through.
fn build_order_view(
    store: &Store,
    order: Order,
    now: i64,
    grace_period_seconds: i64,
) -> std::result::Result<OrderView, crate::store::StoreError> {
    let currently_scanning = store.is_order_currently_scanning(&order.id, now, grace_period_seconds)?;
    Ok(OrderView {
        payment_id: order.id,
        merchant_order_id: order.merchant_order_id,
        address: order.address,
        xmr_amount_piconero: order.xmr_amount_piconero,
        amount_received_piconero: order.amount_received_piconero,
        status: order.status.as_str().to_string(),
        confirmations: order.confirmations,
        double_spend_detected_at: order.double_spend_detected_at,
        refund_address: order.refund_address,
        created_at: order.created_at,
        expires_at: order.expires_at,
        updated_at: order.updated_at,
        first_scanned_height: order.first_scanned_height,
        last_scanned_height: order.last_scanned_height,
        currently_scanning,
    })
}

#[derive(Deserialize)]
pub struct ListOrdersQuery {
    status: Option<String>,
    cursor: Option<i64>,
    limit: Option<u32>,
}

pub async fn list_orders(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
    Query(q): Query<ListOrdersQuery>,
) -> Result<Json<Vec<OrderView>>, ApiError> {
    let status_filter: Option<OrderStatus> = q.status.as_deref().map(parse_status_query).transpose()?;
    let limit = q.limit.unwrap_or(50).min(200);
    let now = now_unix();
    let store = state.store.lock().unwrap();
    let orders = store.list_orders(&tenant.id, status_filter, limit, q.cursor)?;
    let views: std::result::Result<Vec<OrderView>, _> = orders
        .into_iter()
        .map(|o| build_order_view(&store, o, now, state.expired_order_grace_period_seconds))
        .collect();
    Ok(Json(views?))
}

#[derive(Serialize)]
pub struct PaymentView {
    txid: String,
    output_index: i64,
    amount_piconero: u64,
    first_seen_at: i64,
    block_height: Option<i64>,
    voided_at: Option<i64>,
}

impl From<OrderPaymentRow> for PaymentView {
    fn from(p: OrderPaymentRow) -> Self {
        PaymentView {
            txid: p.txid,
            output_index: p.output_index,
            amount_piconero: p.amount_piconero,
            first_seen_at: p.first_seen_at,
            block_height: p.block_height,
            voided_at: p.voided_at,
        }
    }
}

#[derive(Serialize)]
pub struct OrderDetailResponse {
    #[serde(flatten)]
    order: OrderView,
    payments: Vec<PaymentView>,
}

pub async fn get_order_detail(
    AuthedTenant(tenant): AuthedTenant,
    Path(payment_id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<OrderDetailResponse>, ApiError> {
    let store = state.store.lock().unwrap();
    let order = store.get_order(&tenant.id, &payment_id)?.ok_or(ApiError::NotFound)?;
    let payments = store.get_all_payments(&order.id)?;
    let order_view = build_order_view(&store, order, now_unix(), state.expired_order_grace_period_seconds)?;
    Ok(Json(OrderDetailResponse {
        order: order_view,
        payments: payments.into_iter().map(PaymentView::from).collect(),
    }))
}

#[derive(Deserialize)]
pub struct CreateWebhookRequest {
    url: String,
    extra_headers: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct CreateWebhookResponse {
    webhook_id: String,
    signing_secret: String,
}

pub async fn create_webhook(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<CreateWebhookRequest>,
) -> Result<Json<CreateWebhookResponse>, ApiError> {
    let parsed = url::Url::parse(&req.url).map_err(|e| ApiError::BadRequest(format!("invalid url: {e}")))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(ApiError::BadRequest("only http/https URLs are allowed".into()));
    }
    // Resolved-IP SSRF validation (see webhook_sign::validate_webhook_url) happens
    // at delivery time in the not-yet-built delivery worker, not here - DNS can
    // change between registration and delivery, so a registration-time-only check
    // would be insufficient on its own regardless.
    let secret = generate_webhook_secret();
    let extra_headers_json = req.extra_headers.map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string());
    let webhook = state
        .store
        .lock()
        .unwrap()
        .create_webhook(&tenant.id, &req.url, &extra_headers_json, &secret, now_unix())?;
    Ok(Json(CreateWebhookResponse { webhook_id: webhook.id, signing_secret: secret }))
}

#[derive(Serialize)]
pub struct WebhookView {
    webhook_id: String,
    url: String,
    enabled: bool,
    created_at: i64,
}

impl From<Webhook> for WebhookView {
    fn from(w: Webhook) -> Self {
        WebhookView { webhook_id: w.id, url: w.url, enabled: w.enabled, created_at: w.created_at }
    }
}

pub async fn list_webhooks(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
) -> Result<Json<Vec<WebhookView>>, ApiError> {
    let webhooks = state.store.lock().unwrap().list_webhooks(&tenant.id)?;
    Ok(Json(webhooks.into_iter().map(WebhookView::from).collect()))
}

pub async fn delete_webhook(
    AuthedTenant(tenant): AuthedTenant,
    Path(webhook_id): Path<String>,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    let deleted = state.store.lock().unwrap().delete_webhook(&tenant.id, &webhook_id)?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

// -- Order rescans (`docs/order_rescan_wbs.md` Phase 2) --------------------

const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Serialize)]
pub struct RescanStatusView {
    rescan_id: String,
    payment_id: String,
    mode: String,
    status: String,
    from_height: u64,
    to_height: u64,
    current_height: u64,
    /// Derived, not stored - `(current_height - from_height) / (to_height -
    /// from_height)`, clamped to `[0, 100]` so a resumed job's `current_height`
    /// (which starts equal to `from_height`, never below it) and a completed job's
    /// (snapped to `to_height` by `Store::complete_rescan`) both land cleanly at
    /// their ends rather than needing a caller to special-case `status` to read this.
    percent_complete: u8,
    /// `true` if this job is `status: "running"` but hasn't persisted any
    /// progress (`updated_at`) in at least [`RESCAN_STALL_THRESHOLD_SECS`] -
    /// purely informational, no behavior change. Every individual daemon call
    /// inside a rescan already has its own bounded retries and a 15s hard
    /// timeout (`RpcDaemonClient`), and the whole process is re-spawned on
    /// restart (`Store::list_running_rescans`), so a `running` row can't
    /// actually hang forever while the process stays up - this exists purely
    /// so a merchant or operator can *see* "this looks wrong" without having to
    /// infer it from a silent absence of progress, the same role `is_stale`
    /// already plays for the live scanner on `/status`
    /// (`http/status_page.rs::is_stale`).
    stalled: bool,
    error: Option<String>,
    started_at: i64,
    finished_at: Option<i64>,
}

/// How long a `running` job's `updated_at` can go without moving before it's
/// reported as `stalled` - deliberately much longer than
/// `RESCAN_PROGRESS_PERSIST_INTERVAL_BLOCKS`' own typical cadence (a handful of
/// blocks at real Monero block times is minutes, not seconds), so this never
/// flags an ordinary rescan that's just legitimately walking a wide range.
const RESCAN_STALL_THRESHOLD_SECS: i64 = 300;

fn rescan_percent_complete(job: &OrderRescan) -> u8 {
    if job.current_height >= job.to_height {
        return 100;
    }
    let span = job.to_height.saturating_sub(job.from_height);
    if span == 0 {
        return 100; // a degenerate zero-width range is trivially "done"
    }
    let done = job.current_height.saturating_sub(job.from_height);
    ((done as f64 / span as f64) * 100.0).clamp(0.0, 100.0) as u8
}

/// Builds a `RescanStatusView` from a real job row - a plain function rather
/// than `impl From<OrderRescan>` since `stalled` needs `now`, which a caller-
/// supplied `From` impl has no way to thread through (same reasoning as
/// `build_order_view`'s own doc comment, its closest sibling in this file).
fn build_rescan_status_view(job: OrderRescan, now: i64) -> RescanStatusView {
    let percent_complete = rescan_percent_complete(&job);
    let stalled =
        job.status == crate::store::RescanStatus::Running && now - job.updated_at > RESCAN_STALL_THRESHOLD_SECS;
    RescanStatusView {
        rescan_id: job.id,
        payment_id: job.order_id,
        mode: job.mode.as_str().to_string(),
        status: job.status.as_str().to_string(),
        from_height: job.from_height,
        to_height: job.to_height,
        current_height: job.current_height,
        percent_complete,
        stalled,
        error: job.error,
        started_at: job.started_at,
        finished_at: job.finished_at,
    }
}

#[derive(Deserialize)]
pub struct TriggerRescanRequest {
    /// `"simple"` or `"advanced"` - see `resolve_rescan_window`.
    mode: String,
    /// `advanced` mode only: unix timestamps, same convention `created_at`/
    /// `expires_at` already use everywhere else in this API.
    from: Option<i64>,
    to: Option<i64>,
}

/// Resolves a trigger request's requested window to concrete unix timestamps
/// (`(mode, from, to)`), enforcing WBS 2.1 decision 4's bounds: `simple` is always
/// `max(order.created_at, now - default_rescan_lookback_days)` through `now`;
/// `advanced` takes the caller's own `from`/`to`, rejected outright (a real `400`,
/// never silently clamped) if `from` is earlier than the later of the order's own
/// creation time and the `max_rescan_lookback_days` ceiling, if `to` is in the
/// future, or if `to` precedes `from`.
fn resolve_rescan_window(
    req: &TriggerRescanRequest,
    order: &Order,
    default_lookback_days: u32,
    max_lookback_days: u32,
    now: i64,
) -> Result<(RescanMode, i64, i64), ApiError> {
    let earliest_allowed = order.created_at.max(now - max_lookback_days as i64 * SECONDS_PER_DAY);
    match req.mode.as_str() {
        "simple" => {
            let from = order.created_at.max(now - default_lookback_days as i64 * SECONDS_PER_DAY);
            Ok((RescanMode::Simple, from, now))
        }
        "advanced" => {
            let from = req.from.ok_or_else(|| ApiError::BadRequest("advanced mode requires \"from\"".into()))?;
            let to = req.to.unwrap_or(now);
            if from < earliest_allowed {
                return Err(ApiError::BadRequest(format!(
                    "\"from\" ({from}) cannot be earlier than {earliest_allowed} - the later of this order's own \
                     creation time ({}) and the {max_lookback_days}-day lookback ceiling",
                    order.created_at
                )));
            }
            if to > now {
                return Err(ApiError::BadRequest(format!("\"to\" ({to}) cannot be in the future (now is {now})")));
            }
            if to < from {
                return Err(ApiError::BadRequest(format!("\"to\" ({to}) cannot be earlier than \"from\" ({from})")));
            }
            Ok((RescanMode::Advanced, from, to))
        }
        other => {
            Err(ApiError::BadRequest(format!("unknown rescan mode {other:?} - expected \"simple\" or \"advanced\"")))
        }
    }
}

/// `POST /api/v1/admin/tenant/orders/{payment_id}/rescan` - WBS 2.1. Resolves the
/// requested window to real block heights (0.2/1.1's timestamp->height lookup, with
/// 1.1's start-side cushion), inserts a durable job row, and spawns 1.3's runner.
/// A second trigger while one is already running for this same order is not an
/// error - it hands back that job's current state, same as a fresh trigger would
/// (WBS 2.1's own "job already-running is not an error" outcome).
pub async fn trigger_rescan(
    AuthedTenant(tenant): AuthedTenant,
    Path(payment_id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<TriggerRescanRequest>,
) -> Result<(StatusCode, Json<RescanStatusView>), ApiError> {
    let order = state.store.lock().unwrap().get_order(&tenant.id, &payment_id)?.ok_or(ApiError::NotFound)?;

    // Decision 5's guardrail: a rescan exists to find a late payment on an order the
    // merchant already gave up on waiting for - it has no meaning against an order
    // still being live-scanned normally.
    if order.status != OrderStatus::Expired {
        return Err(ApiError::BadRequest(format!(
            "a rescan can only be triggered for an expired order (this order is currently {})",
            order.status.as_str()
        )));
    }

    let now = now_unix();
    let (mode, from_ts, to_ts) =
        resolve_rescan_window(&req, &order, state.default_rescan_lookback_days, state.max_rescan_lookback_days, now)?;

    let network = parse_network(&tenant.network)
        .map_err(|e| ApiError::Internal(format!("tenant has an unrecognized network {:?}: {e}", tenant.network)))?;
    let daemon = state
        .daemons
        .get(&network)
        .ok_or_else(|| ApiError::Internal(format!("no daemon configured for network {network:?}")))?
        .clone();

    // 0.2's binary search, then 1.1's start-side-only safety cushion - see
    // `scanner::rescan_start_height`'s own doc comment for why the end side never
    // gets an equivalent buffer.
    let raw_from_height = daemon
        .find_height_at_or_before(from_ts.max(0) as u64)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to resolve the rescan's start height: {e}")))?;
    let from_height = crate::scanner::rescan_start_height(raw_from_height);
    let to_height = daemon
        .find_height_at_or_before(to_ts.max(0) as u64)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to resolve the rescan's end height: {e}")))?;

    // WBS 5.2's gap-prevention guardrail - advanced mode only (`simple`'s own `to`
    // is always "now" by construction, which can never be earlier than a height
    // this order was already scanned to). Without this, a merchant could pick a
    // narrow advanced-mode window ending before the order's existing
    // `last_scanned_height`, leaving a real, silent gap between the old high-water
    // mark and the new rescan's own end - one Phase 5.1's simple min/max range
    // would then hide entirely, displaying a continuous range that claims full
    // coverage across a span with an actual hole in it. Inclusive `>=`, not `>`:
    // `to` exactly equal to the existing high-water mark is a legitimate, gap-free
    // request, not a rejected one.
    if mode == RescanMode::Advanced {
        if let Some(last_scanned) = order.last_scanned_height {
            if (to_height as i64) < last_scanned {
                return Err(ApiError::BadRequest(format!(
                    "\"to\" resolves to block {to_height}, which is earlier than this order's already-scanned \
                     height {last_scanned} - choose a later end, or leave \"to\" at its default of now"
                )));
            }
        }
    }

    let handle = resolve_wallet_handle(&state, &tenant).await?;

    let outcome = state.store.lock().unwrap().trigger_rescan(
        NewOrderRescan {
            order_id: order.id.clone(),
            tenant_id: tenant.id.clone(),
            minor_index: order.minor_index,
            mode,
            from_height,
            to_height,
        },
        now,
    )?;

    let job = match outcome {
        // Only a genuinely new row needs a runner - see `TriggerRescanOutcome`'s own
        // doc comment for why spawning on `AlreadyRunning` too would race a second
        // runner against whichever one already owns this row.
        TriggerRescanOutcome::Started(job) => {
            crate::scanner::spawn_rescan_job(
                state.store.clone(),
                state.key_custody.clone(),
                daemon.clone() as std::sync::Arc<dyn crate::daemon::MoneroDaemonClient>,
                handle,
                job.id.clone(),
            );
            job
        }
        TriggerRescanOutcome::AlreadyRunning(job) if job.order_id == order.id => job,
        TriggerRescanOutcome::AlreadyRunning(job) => {
            return Err(ApiError::BadRequest(format!(
                "a rescan is already running for order {} - only one rescan may run per tenant at a time",
                job.order_id
            )));
        }
    };

    Ok((StatusCode::ACCEPTED, Json(build_rescan_status_view(job, now))))
}

/// `GET /api/v1/admin/tenant/orders/{payment_id}/rescan` - WBS 2.2. The most
/// recently triggered rescan for this order, whatever its current status - `404` if
/// none was ever triggered.
pub async fn get_rescan_status(
    AuthedTenant(tenant): AuthedTenant,
    Path(payment_id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<RescanStatusView>, ApiError> {
    let store = state.store.lock().unwrap();
    let order = store.get_order(&tenant.id, &payment_id)?.ok_or(ApiError::NotFound)?;
    let job = store.get_latest_rescan_for_order(&order.id)?.ok_or(ApiError::NotFound)?;
    Ok(Json(build_rescan_status_view(job, now_unix())))
}

/// How long a client may treat a `GET .../rescans` response as fresh without
/// re-asking - WBS 2.3's "a few seconds": long enough that a dashboard polling
/// every few seconds gets real cache hits, short enough that "is anything syncing"
/// never looks stale for long once a rescan finishes.
const RESCAN_LIST_CACHE_MAX_AGE_SECS: u64 = 3;

fn rescan_list_etag(running: &Option<OrderRescan>) -> String {
    match running {
        Some(job) => format!("\"{}:{}\"", job.id, job.updated_at),
        None => "\"none\"".to_string(),
    }
}

fn with_rescan_cache_headers(mut response: Response, etag: &str) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::ETAG,
        header::HeaderValue::from_str(etag).expect("etag is built from an id and an integer - always valid ASCII"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_str(&format!("max-age={RESCAN_LIST_CACHE_MAX_AGE_SECS}")).unwrap(),
    );
    response
}

/// `GET /api/v1/admin/tenant/rescans` - WBS 2.3, the dashboard-wide "is anything
/// syncing right now" check. Every currently-`running` rescan for this tenant - in
/// practice always zero or one, given the one-job-per-tenant guardrail
/// (`Store::trigger_rescan`). Real HTTP caching, not a bespoke in-process cache: a
/// cheap `ETag` derived from the running job's own `(id, updated_at)` (or the fixed
/// string `"none"` when nothing is running), honoring `If-None-Match` with a
/// bodyless `304` - the same mechanism a browser or CDN uses, applied here between
/// control-plane and the engine.
pub async fn list_rescans(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let running = state.store.lock().unwrap().get_running_rescan_for_tenant(&tenant.id)?;
    let etag = rescan_list_etag(&running);

    let if_none_match = headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok());
    if if_none_match == Some(etag.as_str()) {
        return Ok(with_rescan_cache_headers(StatusCode::NOT_MODIFIED.into_response(), &etag));
    }

    let now = now_unix();
    let body: Vec<RescanStatusView> = running.into_iter().map(|job| build_rescan_status_view(job, now)).collect();
    Ok(with_rescan_cache_headers((StatusCode::OK, Json(body)).into_response(), &etag))
}

