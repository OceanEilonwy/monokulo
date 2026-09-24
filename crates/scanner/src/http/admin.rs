use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};

use crate::auth::generate_webhook_secret;
use crate::daemon::MoneroDaemonClient;
use crate::key_custody::{KeyCustodyError, SubaddressIndex, WalletMaterial};
use crate::status::OrderStatus;
use crate::store::{NewTenant, Order, OrderPaymentRow, Store, TenantConfigPatch, Webhook};

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
/// this is indistinguishable from "never settles". `pub(crate)` so
/// `http::public::create_order`'s own per-order override can validate against
/// the exact same bound rather than a duplicated magic number.
pub(crate) const MAX_CONFIRMATIONS_REQUIRED: u64 = 720;

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

#[derive(Deserialize)]
pub struct LookupPaymentRequest {
    txid: String,
}

/// `docs/txid_lookup_and_scan_chunking_wbs.md` Part B's own direct replacement
/// for the manual chain-rescan feature above: no block-range walk, no
/// background job, no per-tenant "one at a time" guardrail to enforce - two
/// daemon calls (`locate_transaction`, `get_transaction`) and the same
/// `scan_transaction`/`record_scan_match` primitives the live scanner already
/// uses, narrowed to nothing (this scans the tenant's *whole* address range,
/// not one order's `minor_index` - see below for why).
#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PaymentLookupView {
    /// The daemon has no record of this txid at all, in a block or the pool.
    NotFoundOnChain,
    /// The transaction is real and was decoded, but none of its outputs
    /// decrypt against this tenant's wallet - a genuine, honest "not yours"
    /// rather than a guess.
    NoMatchingOrder,
    /// At least one output matched one of this tenant's orders and was
    /// recorded (idempotently - looking up an already-applied txid a second
    /// time is a safe no-op that still reports the same match). A list, not a
    /// single id: a transaction can in principle pay more than one of a
    /// tenant's subaddresses in one output set.
    Matched { order_ids: Vec<String> },
}

/// `POST /api/v1/admin/tenant/payments/lookup` - `docs/txid_lookup_and_scan_
/// chunking_wbs.md` Part B.2. Unlike the rescan trigger above, this is scoped
/// to the whole tenant (`0..tenant.next_minor_index`), not one specific
/// order's `minor_index`: a merchant who has a customer's txid doesn't
/// necessarily know which order it belongs to ahead of time - that's exactly
/// the real support scenario this exists for ("I paid, here's my txid," not
/// "I paid order X"). No `Expired`-only restriction either, for the same
/// reason: a real match is a real match regardless of the order's current
/// status.
///
/// Uses `state.daemons` (the live scanner's own pool), not a separate one:
/// this is two quick calls, not a bulk historical walk, so it poses none of
/// the sustained-request-volume contention `AppState::rescan_daemons` exists
/// to prevent - see that field's own doc comment for the class of problem
/// this endpoint is deliberately too small to be.
pub async fn lookup_payment(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<LookupPaymentRequest>,
) -> Result<Json<PaymentLookupView>, ApiError> {
    let txid = req.txid.trim().to_lowercase();
    if txid.len() != 64 || !txid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::BadRequest("txid must be 64 hex characters".to_string()));
    }

    let network = parse_network(&tenant.network)
        .map_err(|e| ApiError::Internal(format!("tenant has an unrecognized network {:?}: {e}", tenant.network)))?;
    let daemon = state
        .daemons
        .get(&network)
        .ok_or_else(|| ApiError::Internal(format!("no daemon configured for network {network:?}")))?
        .clone();

    let location = daemon.locate_transaction(&txid).await.map_err(|e| ApiError::Internal(e.to_string()))?;
    let block_height = match location {
        crate::daemon::TxLocation::NotFound => return Ok(Json(PaymentLookupView::NotFoundOnChain)),
        crate::daemon::TxLocation::InPool => None,
        crate::daemon::TxLocation::InBlock(h) => Some(h),
    };

    let tx = daemon.get_transaction(&txid).await.map_err(|e| ApiError::Internal(e.to_string()))?;
    let handle = resolve_wallet_handle(&state, &tenant).await?;
    let now = now_unix();

    // Computed (async, no `&Store` held) then persisted (sync, brief lock) as
    // two separate steps, same as `run_scan_tick`/`scanner::rescan_order`
    // already do everywhere else in this codebase - never a single
    // await-spanning call holding the store's lock.
    let scan = crate::scanner::scan_transaction(state.key_custody.as_ref(), handle, &tx, 0..tenant.next_minor_index)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let touched = {
        let store = state.store.lock().unwrap();
        crate::scanner::record_scan_match(&store, &tenant.id, &scan, now, block_height).map_err(|e| ApiError::Internal(e.to_string()))?
    };

    if touched.is_empty() {
        return Ok(Json(PaymentLookupView::NoMatchingOrder));
    }

    // A current tip for `recompute_order_status` to derive confirmation counts
    // against - `block_height` itself is not a safe substitute (a mempool
    // match has none, and even a mined match's own height could already be
    // behind the real tip by an unrelated confirmation or two).
    let current_height = daemon.get_height().await.map_err(|e| ApiError::Internal(e.to_string()))?;
    {
        let store = state.store.lock().unwrap();
        for order_id in &touched {
            crate::scanner::recompute_and_notify(&store, order_id, current_height, now)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
    }

    Ok(Json(PaymentLookupView::Matched { order_ids: touched.into_iter().collect() }))
}

