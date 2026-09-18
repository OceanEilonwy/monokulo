use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Json;
use serde::{Deserialize, Serialize};

use crate::key_custody::SubaddressIndex;
use crate::store::{NewOrder, Tenant};

use super::{parse_network, AppState, ApiError, now_unix, resolve_wallet_handle};

/// Resolves a tenant by its public key (not a secret - safe to look up directly
/// from a path parameter) and enforces the origin allowlist independently of
/// whatever CORS header this response also carries, per `docs/DESIGN.md` §12: CORS
/// is a browser-enforced courtesy, not a server-side guarantee, so a script that
/// simply doesn't run in a browser is not stopped by it. When no `Origin` header is
/// present at all (non-browser clients, curl, server-to-server), the request is
/// allowed through - this endpoint has no secret to protect, only a "which sites can
/// call this on a customer's behalf" concern that only applies to browser contexts.
async fn resolve_public_tenant(state: &AppState, pk: &str, origin: Option<&str>) -> Result<Tenant, ApiError> {
    let tenant = state.store.lock().unwrap().find_tenant_by_public_key(pk)?.ok_or(ApiError::NotFound)?;
    if let Some(origin) = origin {
        if !tenant.allowed_origins.iter().any(|o| o == origin) {
            return Err(ApiError::Forbidden("origin not allowed for this tenant".into()));
        }
    }
    Ok(tenant)
}

/// XMR-only, per `docs/fx_refactor.md` Phase 3: this process has no concept of fiat
/// or exchange rates at all any more. A caller (in practice, only the monokulo's
/// own `POST /pay/{pk}/orders`, which looks up its own rate and computes this amount
/// before ever calling here) supplies the exact `xmr_amount_piconero` an order is
/// worth; this engine only ever watches the chain for that amount arriving. Any fiat
/// display a customer sees is entirely the monokulo's responsibility, backed by
/// its own local `order_fiat_metadata` record - this engine's `orders` table no
/// longer stores fiat fields at all (see migration `0005_drop_order_fiat_columns.sql`).
#[derive(Deserialize)]
pub struct CreateOrderRequest {
    merchant_order_id: Option<String>,
    xmr_amount_piconero: u64,
    description: Option<String>,
    /// A per-order confirmation-count override - `None` for every caller
    /// that doesn't need one (this engine's tenant-level
    /// `confirmations_required` still applies). Set by monokulo's own
    /// amount-tiered "Confirmation Thresholds" feature, which resolves the
    /// right value itself before ever calling here - this engine has no
    /// concept of currency or amount tiers, it only ever locks in the
    /// single number it's given. Same validation bound as the tenant-level
    /// setting (`http::admin::validate_tenant_settings`).
    #[serde(default)]
    confirmations_required: Option<u64>,
}

#[derive(Serialize)]
pub struct CreateOrderResponse {
    payment_id: String,
    address: String,
    xmr_amount_piconero: u64,
    expires_at: i64,
}

pub async fn create_order(
    Path(pk): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateOrderRequest>,
) -> Result<Json<CreateOrderResponse>, ApiError> {
    let origin = headers.get(axum::http::header::ORIGIN).and_then(|v| v.to_str().ok());
    let tenant = resolve_public_tenant(&state, &pk, origin).await?;

    if req.xmr_amount_piconero == 0 {
        return Err(ApiError::BadRequest("xmr_amount_piconero must be greater than zero".into()));
    }
    if let Some(confirmations) = req.confirmations_required {
        if confirmations == 0 || confirmations > super::admin::MAX_CONFIRMATIONS_REQUIRED {
            return Err(ApiError::BadRequest(format!(
                "confirmations_required must be between 1 and {}; 0 would treat an unconfirmed transaction as final",
                super::admin::MAX_CONFIRMATIONS_REQUIRED
            )));
        }
    }

    let handle = resolve_wallet_handle(&state, &tenant).await?;
    // tenant.network was validated against a configured node at tenant-creation
    // time - a parse failure here means the stored value is corrupt, not that the
    // customer did anything wrong.
    let network = parse_network(&tenant.network)
        .map_err(|e| ApiError::Internal(format!("tenant has an invalid stored network: {e}")))?;

    // Peek at the index, derive its address, then claim the index and insert the
    // order together in one lock hold - never allocate first and insert later.
    // `next_minor_index` is what the scanner reads to decide which subaddresses it
    // scans, so between an eager allocation and the order row's insertion there is a
    // window in which a scanner tick will match a real output against an index no
    // order exists for and silently drop it. Inside a mined block that loss is
    // permanent: blocks are scanned exactly once, and the scanner marks the height
    // scanned whether or not the match was recorded. Deriving the address before
    // claiming keeps the `.await` (which cannot happen under the store lock) outside
    // the atomic part.
    //
    // A losing racer re-derives against the next index; it never burns one, so the
    // loop cannot walk the counter forward on contention. The bound only exists so a
    // pathological hot tenant can't spin here forever.
    let now = now_unix();
    let mut created = None;
    for _ in 0..8 {
        let minor_index = state.store.lock().unwrap().peek_next_minor_index(&tenant.id)?;
        let address = state
            .key_custody
            .derive_subaddress(handle, SubaddressIndex { major: 0, minor: minor_index }, network)
            .await?;
        let order = state.store.lock().unwrap().create_order_claiming_minor_index(
            minor_index,
            NewOrder {
                confirmations_required_override: req.confirmations_required,
                tenant_id: tenant.id.clone(),
                merchant_order_id: req.merchant_order_id.clone(),
                minor_index,
                address: address.to_string(),
                xmr_amount_piconero: req.xmr_amount_piconero,
                description: req.description.clone(),
                created_at: now,
                expires_at: now + tenant.order_expiry_seconds,
            },
        )?;
        if let Some(order) = order {
            created = Some(order);
            break;
        }
    }
    let order = created.ok_or_else(|| {
        ApiError::Internal("could not claim a subaddress index for this order - too much concurrent contention".into())
    })?;

    Ok(Json(CreateOrderResponse {
        payment_id: order.id,
        address: order.address,
        xmr_amount_piconero: order.xmr_amount_piconero,
        expires_at: order.expires_at,
    }))
}

#[derive(Serialize)]
pub struct OrderStatusResponse {
    payment_id: String,
    status: String,
    address: String,
    confirmations: u64,
    amount_received_piconero: u64,
    xmr_amount_piconero: u64,
    double_spend_detected_at: Option<i64>,
    expires_at: i64,
}

/// Note: only `pk_` and `payment_id` scope this lookup - there is no `sk_` to check
/// here by design, since a payment_id is an unguessable random identifier the
/// customer already holds (from the order-creation response), not a secret this
/// server needs to authenticate. The equivalent IDOR concern for the *admin* surface
/// (see `docs/DESIGN.md` §10.1) doesn't apply the same way here: this route's whole
/// job is to let anyone holding a payment_id check its status.
pub async fn get_order_status(
    Path((pk, payment_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<OrderStatusResponse>, ApiError> {
    let store = state.store.lock().unwrap();
    let tenant = store.find_tenant_by_public_key(&pk)?.ok_or(ApiError::NotFound)?;
    let order = store.get_order(&tenant.id, &payment_id)?.ok_or(ApiError::NotFound)?;
    Ok(Json(OrderStatusResponse {
        payment_id: order.id,
        status: order.status.as_str().to_string(),
        address: order.address,
        confirmations: order.confirmations,
        amount_received_piconero: order.amount_received_piconero,
        xmr_amount_piconero: order.xmr_amount_piconero,
        double_spend_detected_at: order.double_spend_detected_at,
        expires_at: order.expires_at,
    }))
}

#[derive(Deserialize)]
pub struct SetRefundAddressRequest {
    refund_address: String,
}

pub async fn set_refund_address(
    Path((pk, payment_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(req): Json<SetRefundAddressRequest>,
) -> Result<(), ApiError> {
    let store = state.store.lock().unwrap();
    let tenant = store.find_tenant_by_public_key(&pk)?.ok_or(ApiError::NotFound)?;
    let updated = store.set_refund_address(&tenant.id, &payment_id, &req.refund_address)?;
    if updated {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}
