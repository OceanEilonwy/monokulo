//! Order creation for the `sk_`-authenticated admin API
//! (`POST /api/v1/admin/tenant/orders`) - the only way orders are created now
//! the engine is private. Monokulo calls it for every order (its public
//! checkout API, the dashboard and the POS) after pricing the order in XMR.

use axum::extract::State;
use axum::response::Json;
use serde::{Deserialize, Serialize};

use crate::key_custody::SubaddressIndex;
use crate::store::{NewOrder, Tenant};

use super::{parse_network, AppState, ApiError, AuthedTenant, now_unix, resolve_wallet_handle};

/// XMR-only, per `docs/fx_refactor.md` Phase 3: this process has no concept of fiat
/// or exchange rates at all any more. The caller (monokulo) supplies the exact
/// `xmr_amount_piconero` an order is worth; this engine only ever watches the
/// chain for that amount arriving. Any fiat
/// display a customer sees is entirely the monokulo's responsibility, backed by
/// its own local `order_fiat_metadata` record - this engine's `orders` table no
/// longer stores fiat fields at all (see migration `0005_drop_order_fiat_columns.sql`).
#[derive(Deserialize)]
pub struct CreateOrderRequest {
    merchant_order_id: Option<String>,
    xmr_amount_piconero: u64,
    description: Option<String>,
    /// A per-order confirmation-count override. Set by monokulo's
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
    order_id: String,
    address: String,
    xmr_amount_piconero: u64,
    expires_at: i64,
}

pub async fn create_order_for_admin(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<CreateOrderRequest>,
) -> Result<Json<CreateOrderResponse>, ApiError> {
    create_order_for_tenant(state, tenant, req).await
}

async fn create_order_for_tenant(state: AppState, tenant: Tenant, req: CreateOrderRequest) -> Result<Json<CreateOrderResponse>, ApiError> {
    if req.xmr_amount_piconero == 0 {
        return Err(ApiError::BadRequest("xmr_amount_piconero must be greater than zero".into()));
    }
    super::admin::validate_confirmations_required(req.confirmations_required)?;

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
        order_id: order.id,
        address: order.address,
        xmr_amount_piconero: order.xmr_amount_piconero,
        expires_at: order.expires_at,
    }))
}
