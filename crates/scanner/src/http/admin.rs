use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};

use crate::auth::generate_webhook_secret;
use crate::daemon::MoneroDaemonClient;
use crate::key_custody::{KeyCustodyError, SubaddressIndex, WalletMaterial};
use crate::status::OrderStatus;
use crate::store::{NewTenant, Order, OrderPaymentRow, Store, TenantConfigPatch, Webhook};

use super::{
    network_str, now_unix, parse_network, parse_status_query, resolve_wallet_handle, ApiError,
    AppState, AuthedTenant,
};

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
        KeyCustodyError::InvalidKeyMaterial(m) => {
            ApiError::BadRequest(format!("invalid key material: {m}"))
        }
        other => other.into(),
    }
}

/// No `allowed_origins` any more: the engine is private and keeps no
/// per-tenant origin list (embedding policy is monokulo's). Like every
/// request body here, unknown fields are ignored, so an old caller still
/// sending one is not refused.
#[derive(Deserialize)]
pub struct CreateTenantRequest {
    view_key_hex: String,
    spend_pubkey_hex: String,
    network: Option<String>,
    confirmations_required: Option<u64>,
    order_expiry_seconds: Option<i64>,
    /// Which key custody backend holds this store's keys (part 5); the
    /// instance's default when absent.
    key_custody_backend: Option<String>,
}

/// The backend a new or moving store's keys go to: `requested`, or the
/// default. Refused if it isn't enabled.
fn chosen_backend(state: &AppState, requested: Option<&str>) -> Result<String, ApiError> {
    let enabled = state.key_custody.enabled_backends();
    if enabled.is_empty() {
        // A single backend, not a router (some tests): it is the only one.
        return Ok(requested
            .map(str::to_string)
            .unwrap_or_else(|| state.key_custody_backend.clone()));
    }
    let backend = requested
        .map(str::to_string)
        .unwrap_or_else(|| state.settings.custody.load().default.as_str().to_string());
    if !enabled.contains(&backend) {
        return Err(ApiError::BadRequest(format!(
            "key custody backend {backend:?} is not enabled on this instance (enabled: {})",
            enabled.join(", ")
        )));
    }
    Ok(backend)
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
    let network = parse_network(req.network.as_deref().unwrap_or("mainnet"))
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    if !state.daemons.is_configured(network) {
        return Err(ApiError::BadRequest(format!(
            "no monero_node is configured for network {:?} on this instance",
            network_str(network)
        )));
    }

    validate_tenant_settings(req.confirmations_required, req.order_expiry_seconds)?;
    let backend = chosen_backend(&state, req.key_custody_backend.as_deref())?;

    let handle = state
        .key_custody
        .register_wallet_in(&backend, material.clone())
        .await
        .map_err(key_custody_error_for_new_tenant)?;
    let primary_address = state
        .key_custody
        .derive_subaddress(handle, SubaddressIndex::default(), network)
        .await
        .map_err(key_custody_error_for_new_tenant)?;
    let sealed = match state.key_custody.seal_in(&backend, &material).await {
        Ok(sealed) => sealed,
        Err(e) => {
            let _ = state.key_custody.remove_wallet(handle).await;
            return Err(key_custody_error_for_new_tenant(e));
        }
    };
    let defaults = state.settings.tenant_defaults.load();

    let new_tenant = NewTenant {
        // The backend that sealed these keys and holds them (part 5).
        key_custody_backend: backend.clone(),
        sealed_key_material: sealed,
        primary_address: primary_address.to_string(),
        network: network_str(network).to_string(),
        // Values the request doesn't give come from the instance's
        // current defaults (task 2.9), not a hardcoded number.
        confirmations_required: Some(
            req.confirmations_required
                .unwrap_or(defaults.confirmations_required),
        ),
        order_expiry_seconds: Some(
            req.order_expiry_seconds
                .unwrap_or(defaults.order_expiry_seconds),
        ),
    };
    let created = state
        .write_store(move |s| s.create_tenant(new_tenant, now_unix()))
        .await;
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

    state
        .wallet_handles
        .write()
        .insert(created.tenant.id.clone(), handle);

    Ok(Json(CreateTenantResponse {
        tenant_id: created.tenant.id,
        public_key: created.tenant.public_key,
        secret_token: created.secret_token.expose().to_string(),
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
/// `http::public::create_order_for_admin`'s per-order override can validate against
/// the exact same bound rather than a duplicated magic number. `0` is a legal
/// lower bound - native 0-conf, see `status::derive_status`'s own doc comment for
/// why a `confirmations_required = 0` tier needs no special handling to be safe.
pub(crate) const MAX_CONFIRMATIONS_REQUIRED: u64 = 720;

/// Shared by the tenant-level default (here) and `http::public::create_order_for_admin`'s
/// per-order override - the exact same bound, so it's enforced in exactly one place
/// rather than as two copies that could drift.
pub(crate) fn validate_confirmations_required(
    confirmations_required: Option<u64>,
) -> Result<(), ApiError> {
    if let Some(confirmations) = confirmations_required {
        if confirmations > MAX_CONFIRMATIONS_REQUIRED {
            return Err(ApiError::BadRequest(format!(
                "confirmations_required must be at most {MAX_CONFIRMATIONS_REQUIRED}"
            )));
        }
    }
    Ok(())
}

/// Shared by tenant creation and tenant patching, because both write the same two
/// columns and a bound enforced on only one of them is not a bound.
///
/// Neither value is dangerous to *us* - a tenant can only misconfigure their own
/// orders - but `order_expiry_seconds <= 0` has a silent failure mode rather than a
/// loud one (orders that are already expired when the customer first loads the
/// payment page), which is what makes it worth rejecting at the edge.
fn validate_tenant_settings(
    confirmations_required: Option<u64>,
    order_expiry_seconds: Option<i64>,
) -> Result<(), ApiError> {
    validate_confirmations_required(confirmations_required)?;
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
    order_expiry_seconds: i64,
    key_custody_backend: String,
}

impl From<crate::store::Tenant> for TenantView {
    fn from(t: crate::store::Tenant) -> Self {
        TenantView {
            tenant_id: t.id,
            public_key: t.public_key,
            primary_address: t.primary_address,
            network: t.network,
            confirmations_required: t.confirmations_required,
            order_expiry_seconds: t.order_expiry_seconds,
            key_custody_backend: t.key_custody_backend,
        }
    }
}

#[derive(Deserialize)]
pub struct SwitchKeyCustodyRequest {
    backend: String,
    view_key_hex: String,
    spend_pubkey_hex: String,
}

/// Whether `material` is the wallet `primary_address` belongs to, on
/// `network` (task 5.3). Compares keys, not address strings: the stored
/// address may be in any valid form (the bootstrap CLI stores what was
/// typed). The public spend key must match, and so must the public view key
/// derived from the given private view key.
pub fn is_same_wallet(
    material: &WalletMaterial,
    primary_address: &str,
    network: monero::Network,
) -> Result<bool, ApiError> {
    // The keys were already parsed by `WalletMaterial::from_hex`, so an
    // error here is the stored address.
    crate::key_custody::wallet_matches_address(material, primary_address, network)
        .map_err(ApiError::Internal)
}

/// `PUT /api/v1/admin/tenant/key-custody` - moves the authenticated store to
/// another key custody backend (task 5.3, decision D3). The store's keys are
/// entered again - nothing is ever copied between backends - and must be
/// the same wallet as the store's own. They're registered in the new
/// backend and sealed there, the row is updated in one statement, the live
/// handle is swapped, and only then is the old registration removed, so
/// scanning and order creation always have a valid handle.
pub async fn switch_key_custody(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<SwitchKeyCustodyRequest>,
) -> Result<Json<TenantView>, ApiError> {
    let material = WalletMaterial::from_hex(&req.view_key_hex, &req.spend_pubkey_hex)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let network = parse_network(&tenant.network).map_err(|e| ApiError::Internal(e.to_string()))?;
    if !is_same_wallet(&material, &tenant.primary_address, network)? {
        return Err(ApiError::BadRequest(
            "These keys belong to a different wallet from the one this store uses.".to_string(),
        ));
    }
    let backend = chosen_backend(&state, Some(&req.backend))?;

    // One switch at a time: two overlapping switches of the same store could
    // otherwise leave its row saying one backend while its live handle is
    // in the other. Switches are rare, so one lock for all of them is fine.
    static SWITCHING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _switching = SWITCHING.lock().await;

    let handle = state
        .key_custody
        .register_wallet_in(&backend, material.clone())
        .await
        .map_err(ApiError::from)?;
    let sealed = match state.key_custody.seal_in(&backend, &material).await {
        Ok(sealed) => sealed,
        Err(e) => {
            let _ = state.key_custody.remove_wallet(handle).await;
            return Err(e.into());
        }
    };
    let (id, chosen) = (tenant.id.clone(), backend.clone());
    let updated = state
        .write_store(move |s| s.update_tenant_key_custody(&id, &chosen, &sealed))
        .await;
    if let Err(e) = updated {
        let _ = state.key_custody.remove_wallet(handle).await;
        return Err(e.into());
    }
    let previous = state
        .wallet_handles
        .write()
        .insert(tenant.id.clone(), handle);
    if let Some(previous) = previous.filter(|p| *p != handle) {
        if let Err(e) = state.key_custody.remove_wallet(previous).await {
            // The old backend is down: it loses the copy when it restarts.
            tracing::warn!(store.id = %tenant.id, error = %e, "moved a store's keys, but removing them from its old key custody backend failed");
        }
    }
    let id = tenant.id.clone();
    let refetched = state
        .write_store(move |s| s.get_tenant_by_id(&id))
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(TenantView::from(refetched)))
}

#[derive(Serialize)]
pub struct KeyCustodyBackendView {
    name: &'static str,
    description: &'static str,
}

#[derive(Serialize)]
pub struct KeyCustodyView {
    enabled: Vec<KeyCustodyBackendView>,
    default: String,
}

fn backend_description(name: &str) -> &'static str {
    match name {
        "plain" => "In the engine's own memory. Simple; anyone who controls the engine's machine can read the keys.",
        "socket" => "In a separate key-custody-server process, so the engine itself never holds the keys.",
        _ => "",
    }
}

/// `GET /api/v1/admin/key-custody` - the backends a store can choose from
/// and the default (task 5.4). Not secret: any caller that reaches the
/// private engine API may ask.
pub async fn key_custody_options(State(state): State<AppState>) -> Json<KeyCustodyView> {
    let mut enabled_names = state.key_custody.enabled_backends();
    if enabled_names.is_empty() {
        enabled_names.push(state.key_custody_backend.clone());
    }
    let enabled = enabled_names
        .iter()
        .filter_map(|name| match name.as_str() {
            "plain" => Some(KeyCustodyBackendView {
                name: "plain",
                description: backend_description("plain"),
            }),
            "socket" => Some(KeyCustodyBackendView {
                name: "socket",
                description: backend_description("socket"),
            }),
            _ => None,
        })
        .collect();
    let default = match state.key_custody.enabled_backends().is_empty() {
        true => state.key_custody_backend.clone(),
        false => state.settings.custody.load().default.as_str().to_string(),
    };
    Json(KeyCustodyView { enabled, default })
}

pub async fn get_own_tenant(AuthedTenant(tenant): AuthedTenant) -> Json<TenantView> {
    Json(TenantView::from(tenant))
}

#[derive(Deserialize, Default)]
pub struct PatchTenantRequest {
    confirmations_required: Option<u64>,
    order_expiry_seconds: Option<i64>,
}

pub async fn patch_own_tenant(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<PatchTenantRequest>,
) -> Result<Json<TenantView>, ApiError> {
    validate_tenant_settings(req.confirmations_required, req.order_expiry_seconds)?;
    let patch = TenantConfigPatch {
        confirmations_required: req.confirmations_required,
        order_expiry_seconds: req.order_expiry_seconds,
    };
    let id = tenant.id.clone();
    let refetched = state
        .write_store(move |s| {
            s.update_tenant_config(&id, patch)?;
            s.get_tenant_by_id(&id)
        })
        .await?
        .ok_or(ApiError::NotFound)?;
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
    let id = tenant.id.clone();
    let new_secret = state
        .write_store(move |s| s.rotate_tenant_secret(&id))
        .await?;
    Ok(Json(RotateSecretResponse {
        secret_token: new_secret.expose().to_string(),
    }))
}

pub async fn delete_own_tenant(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    let removed_handle = state.wallet_handles.write().remove(&tenant.id);
    if let Some(handle) = removed_handle {
        // Best-effort: an already-unknown handle is not an error worth surfacing here.
        let _ = state.key_custody.remove_wallet(handle).await;
    }
    let id = tenant.id.clone();
    state
        .write_store(move |s| s.disable_tenant(&id, now_unix()))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct OrderView {
    order_id: String,
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
    let currently_scanning =
        store.is_order_currently_scanning(&order.id, now, grace_period_seconds)?;
    Ok(OrderView {
        order_id: order.id,
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
    /// Comma-separated order ids: returns exactly those orders (this
    /// tenant's only, in the order asked, unknown ids left out) instead of a
    /// page. One request for a set of orders a caller already knows, so a
    /// screen watching many orders does not spend one rate-limited request
    /// per order.
    ids: Option<String>,
    /// `open=true`: only orders still open (pending, unconfirmed,
    /// confirming, partial) - what a point of sale is still waiting on.
    open: Option<bool>,
    /// Orders whose id or merchant order id contains this, ignoring case.
    search: Option<String>,
    /// Orders to skip, for paging with `open`/`search`.
    offset: Option<u32>,
}

/// Most order ids one `ids=` request may name.
pub const MAX_LIST_ORDER_IDS: usize = 100;

pub async fn list_orders(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
    Query(q): Query<ListOrdersQuery>,
) -> Result<Json<Vec<OrderView>>, ApiError> {
    let status_filter: Option<OrderStatus> =
        q.status.as_deref().map(parse_status_query).transpose()?;
    let limit = q.limit.unwrap_or(50).min(200);
    let now = now_unix();
    let ids: Option<Vec<String>> = q.ids.as_deref().map(|ids| {
        ids.split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect()
    });
    if ids
        .as_ref()
        .is_some_and(|ids| ids.len() > MAX_LIST_ORDER_IDS)
    {
        return Err(ApiError::BadRequest(format!(
            "ids may name at most {MAX_LIST_ORDER_IDS} orders"
        )));
    }
    let search: Option<String> = q
        .search
        .as_deref()
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(str::to_owned);
    if search
        .as_deref()
        .is_some_and(|term| term.chars().count() > 120)
    {
        return Err(ApiError::BadRequest("search is too long".into()));
    }
    let paged = q.open.unwrap_or(false) || q.search.is_some() || q.offset.is_some();
    let (open, offset, cursor, tenant_id) = (
        q.open.unwrap_or(false),
        q.offset.unwrap_or(0),
        q.cursor,
        tenant.id.clone(),
    );
    let grace = state
        .settings
        .scan
        .load()
        .expired_order_grace_period_seconds;
    let views = state
        .read_store(move |store| {
            let orders = match ids {
                Some(ids) => {
                    let mut orders = Vec::with_capacity(ids.len());
                    for id in ids {
                        if let Some(order) = store.get_order(&tenant_id, &id)? {
                            orders.push(order);
                        }
                    }
                    orders
                }
                None if paged => {
                    store.list_orders_page(&tenant_id, open, search.as_deref(), limit, offset)?
                }
                None => store.list_orders(&tenant_id, status_filter, limit, cursor)?,
            };
            orders
                .into_iter()
                .map(|o| build_order_view(store, o, now, grace))
                .collect::<std::result::Result<Vec<_>, _>>()
        })
        .await?;
    Ok(Json(views))
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
    Path(order_id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<OrderDetailResponse>, ApiError> {
    let grace = state
        .settings
        .scan
        .load()
        .expired_order_grace_period_seconds;
    let tenant_id = tenant.id.clone();
    let found = state
        .read_store(move |store| {
            let Some(order) = store.get_order(&tenant_id, &order_id)? else {
                return Ok(None);
            };
            let payments = store.get_all_payments(&order.id)?;
            Ok(Some((
                build_order_view(store, order, now_unix(), grace)?,
                payments,
            )))
        })
        .await?;
    let (order_view, payments) = found.ok_or(ApiError::NotFound)?;
    Ok(Json(OrderDetailResponse {
        order: order_view,
        payments: payments.into_iter().map(PaymentView::from).collect(),
    }))
}

#[derive(Deserialize)]
pub struct SetRefundAddressRequest {
    refund_address: String,
}

/// `POST /api/v1/admin/tenant/orders/{order_id}/refund-address` - records
/// where a refund for one of the authenticated tenant's orders should go.
/// Monokulo's checkout calls this on the customer's behalf (after checking
/// the address parses for the order's network itself), so the engine needs
/// no public route for it. Like every other free-text field here the value
/// is stored verbatim: a human reviews it before any refund is sent. The
/// tenant comes from the `sk_` alone, so an `order_id` belonging to another
/// tenant is simply not found.
pub async fn set_order_refund_address(
    AuthedTenant(tenant): AuthedTenant,
    Path(order_id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<SetRefundAddressRequest>,
) -> Result<(), ApiError> {
    let id = tenant.id.clone();
    let updated = state
        .write_store(move |s| s.set_refund_address(&id, &order_id, &req.refund_address))
        .await?;
    if updated {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
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
    let parsed =
        url::Url::parse(&req.url).map_err(|e| ApiError::BadRequest(format!("invalid url: {e}")))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(ApiError::BadRequest(
            "only http/https URLs are allowed".into(),
        ));
    }
    // Resolved-IP SSRF validation (see webhook_sign::validate_webhook_url) happens
    // at delivery time in the not-yet-built delivery worker, not here - DNS can
    // change between registration and delivery, so a registration-time-only check
    // would be insufficient on its own regardless.
    let secret = generate_webhook_secret();
    let extra_headers_json = req
        .extra_headers
        .map(|v| v.to_string())
        .unwrap_or_else(|| "{}".to_string());
    let (tenant_id, url, signing_secret) = (tenant.id.clone(), req.url.clone(), secret.clone());
    let webhook = state
        .write_store(move |store| {
            store.create_webhook(
                &tenant_id,
                &url,
                &extra_headers_json,
                &signing_secret,
                now_unix(),
            )
        })
        .await?;
    Ok(Json(CreateWebhookResponse {
        webhook_id: webhook.id,
        signing_secret: secret,
    }))
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
        WebhookView {
            webhook_id: w.id,
            url: w.url,
            enabled: w.enabled,
            created_at: w.created_at,
        }
    }
}

pub async fn list_webhooks(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
) -> Result<Json<Vec<WebhookView>>, ApiError> {
    let id = tenant.id.clone();
    let webhooks = state.read_store(move |s| s.list_webhooks(&id)).await?;
    Ok(Json(webhooks.into_iter().map(WebhookView::from).collect()))
}

pub async fn delete_webhook(
    AuthedTenant(tenant): AuthedTenant,
    Path(webhook_id): Path<String>,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    let id = tenant.id.clone();
    let deleted = state
        .write_store(move |s| s.delete_webhook(&id, &webhook_id))
        .await?;
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
        return Err(ApiError::BadRequest(
            "txid must be 64 hex characters".to_string(),
        ));
    }

    let network = parse_network(&tenant.network).map_err(|e| {
        ApiError::Internal(format!(
            "tenant has an unrecognized network {:?}: {e}",
            tenant.network
        ))
    })?;
    let daemon = state.daemons.get(network).ok_or_else(|| {
        ApiError::Unavailable(format!(
            "no Monero node is configured for network {network:?}"
        ))
    })?;

    let location = daemon
        .locate_transaction(&txid)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let block_height = match location {
        crate::daemon::TxLocation::NotFound => return Ok(Json(PaymentLookupView::NotFoundOnChain)),
        crate::daemon::TxLocation::InPool => None,
        crate::daemon::TxLocation::InBlock(h) => Some(h),
    };

    let tx = daemon
        .get_transaction(&txid)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut handle = resolve_wallet_handle(&state, &tenant).await?;
    let now = now_unix();

    // Computed (async, no `&Store` held) then persisted (sync, brief lock) as
    // two separate steps, same as the scheduler/`scanner::rescan_order`
    // already do everywhere else in this codebase - never a single
    // await-spanning call holding the store's lock.
    let mut retries = 0;
    let scan = loop {
        match crate::scanner::scan_transaction(
            state.key_custody.as_ref(),
            handle,
            &tx,
            0..tenant.next_minor_index,
        )
        .await
        {
            Ok(scan) => break scan,
            Err(crate::scanner::ScannerError::KeyCustody(
                crate::key_custody::KeyCustodyError::UnknownWallet,
            )) if retries < super::UNKNOWN_WALLET_RETRIES => {
                retries += 1;
                super::forget_wallet_handle(&state, &tenant.id, handle);
                handle = resolve_wallet_handle(&state, &tenant).await?;
            }
            Err(crate::scanner::ScannerError::KeyCustody(e)) => return Err(e.into()),
            Err(e) => return Err(ApiError::Internal(e.to_string())),
        }
    };
    let id = tenant.id.clone();
    let touched = state
        .write_store(move |store| {
            crate::scanner::record_scan_match(store, &id, &scan, now, block_height)
        })
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    if touched.is_empty() {
        return Ok(Json(PaymentLookupView::NoMatchingOrder));
    }

    // A current tip for `recompute_order_status` to derive confirmation counts
    // against - `block_height` itself is not a safe substitute (a mempool
    // match has none, and even a mined match's own height could already be
    // behind the real tip by an unrelated confirmation or two).
    let current_height = daemon
        .get_height()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let orders: Vec<String> = touched.iter().cloned().collect();
    state
        .write_store(move |store| {
            for order_id in &orders {
                crate::scanner::recompute_and_notify(store, order_id, current_height, now)?;
            }
            Ok::<(), crate::scanner::ScannerError>(())
        })
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(PaymentLookupView::Matched {
        order_ids: touched.into_iter().collect(),
    }))
}

/// `GET /api/v1/admin/tenant/events` - a Server-Sent Events stream of this
/// tenant's order changes, so a client that shows live order state (monokulo's
/// checkout and POS pages) can re-read an order when it actually changes
/// instead of polling it.
///
/// Events:
/// - `ready` once, as soon as the stream is subscribed. Anything that changed
///   before this was missed, so a client re-reads everything it watches here.
/// - `order` with `{"order_id": "..."}` - that order changed; re-read it.
/// - `resync` - this stream fell behind and dropped changes; re-read
///   everything, exactly as on `ready`.
///
/// Carries no order state itself - see [`crate::store::OrderChange`].
pub async fn order_events(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
) -> axum::response::sse::Sse<
    impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
> {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use tokio::sync::broadcast::error::RecvError;

    let receiver = state.db.subscribe_order_changes();
    let ready =
        futures_util::stream::once(async { Ok(Event::default().event("ready").data("{}")) });
    let changes = futures_util::stream::unfold(
        (receiver, tenant.id),
        |(mut receiver, tenant_id)| async move {
            loop {
                match receiver.recv().await {
                    Ok(change) if change.tenant_id == tenant_id => {
                        let data = serde_json::json!({ "order_id": change.order_id }).to_string();
                        return Some((
                            Ok(Event::default().event("order").data(data)),
                            (receiver, tenant_id),
                        ));
                    }
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => {
                        return Some((
                            Ok(Event::default().event("resync").data("{}")),
                            (receiver, tenant_id),
                        ));
                    }
                    Err(RecvError::Closed) => return None,
                }
            }
        },
    );
    Sse::new(futures_util::StreamExt::chain(ready, changes))
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
}
