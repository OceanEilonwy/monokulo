use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::auth::generate_webhook_secret;
use crate::daemon::MoneroDaemonClient as _;
use crate::engine_settings::EngineSettings;
use crate::key_custody::transport::{Action, Bundle, Envelope, HandoffAnswer};
use crate::key_custody::{
    remove_wallet_logged, KeyCustodyError, SubaddressIndex, WalletHandle, WalletMaterial,
};
use crate::status::OrderStatus;
use crate::store::{Database, NewTenant, Order, OrderPaymentRow, TenantConfigPatch, Webhook};

use super::{
    network_str, now_unix, parse_network, parse_status_query, resolve_wallet_handle, ApiError,
    AppState, AuthedTenant, Custody,
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
        other @ (KeyCustodyError::UnknownWallet
        | KeyCustodyError::BackendUnavailable(_)
        | KeyCustodyError::ScanFailed(_)) => other.into(),
    }
}

/// No `allowed_origins` any more: the engine is private and keeps no
/// per-tenant origin list (embedding policy is monokulo's). Like every
/// request body here, unknown fields are ignored, so an old caller still
/// sending one is not refused.
#[derive(Deserialize)]
pub(super) struct CreateTenantRequest {
    #[serde(flatten)]
    keys: KeysIn,
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
    let enabled = state.custody.backends.enabled_backends();
    if enabled.is_empty() {
        // A single backend, not a router (some tests): it is the only one.
        return Ok(requested.map_or_else(|| state.custody.default_backend.clone(), str::to_owned));
    }
    let backend = requested.map_or_else(
        || state.settings.custody.load().default.as_str().to_owned(),
        str::to_owned,
    );
    if !enabled.contains(&backend) {
        return Err(ApiError::BadRequest(format!(
            "key custody backend {backend:?} is not enabled on this instance (enabled: {})",
            enabled.join(", ")
        )));
    }
    Ok(backend)
}

#[derive(Serialize)]
pub(super) struct CreateTenantResponse {
    tenant_id: crate::store::TenantId,
    public_key: String,
    secret_token: String,
}

/// Runs `work` to its end even if the request it serves is dropped (the
/// request timeout fired, the caller went away). Registering keys in a
/// backend, sealing them, writing the row and swapping the live handle
/// are one sequence: abandoned between two of its steps, the row would
/// name a backend the live handle isn't in, and keys registered in the new
/// one would be held with nothing able to remove them.
async fn to_completion<T: Send + 'static>(
    work: impl std::future::Future<Output = Result<T, ApiError>> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::spawn(work)
        .await
        .map_err(|e| ApiError::Internal(format!("key custody work panicked: {e}")))?
}

pub(super) async fn create_tenant(
    State(state): State<AppState>,
    Json(req): Json<CreateTenantRequest>,
) -> Result<Json<CreateTenantResponse>, ApiError> {
    to_completion(create_tenant_to_completion(state, req)).await
}

/// A store's keys as a request carries them: in the clear, for a backend
/// that takes them that way (`plain`), or encrypted to the backend
/// (`encrypted_keys`, an envelope from `key-custody-cli` or the key entry
/// form) for one that takes them only that way (`snp`).
#[derive(Deserialize, Default)]
pub(super) struct KeysIn {
    #[serde(default)]
    view_key_hex: String,
    #[serde(default)]
    spend_pubkey_hex: String,
    #[serde(default)]
    encrypted_keys: Option<String>,
}

/// A store's keys, registered: the handle, the sealed bytes to store, and
/// (keys given in the clear) the keys themselves, for the caller's checks.
struct Registered {
    handle: WalletHandle,
    sealed: Vec<u8>,
    material: Option<WalletMaterial>,
}

/// Registers `keys` in `backend` and seals them, in the form the backend
/// takes, refusing the other form. Errors are the caller's (`400`) where the
/// keys are at fault. Nothing stays registered if it fails.
async fn register_keys(
    state: &AppState,
    backend: &str,
    keys: &KeysIn,
    action: Action,
    store: Option<&str>,
) -> Result<Registered, ApiError> {
    let custody = state.custody.backends.as_ref();
    let encrypted = keys
        .encrypted_keys
        .as_deref()
        .filter(|e| !e.trim().is_empty());
    if custody.takes_raw_keys_in(backend) {
        if encrypted.is_some() {
            return Err(ApiError::BadRequest(format!(
                "the {backend} key custody backend takes keys as they are, not encrypted"
            )));
        }
        let material = WalletMaterial::from_hex(&keys.view_key_hex, &keys.spend_pubkey_hex)
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        let handle = custody
            .register_wallet_in(backend, material.clone())
            .await
            .map_err(key_custody_error_for_new_tenant)?;
        return match custody.seal_in(backend, &material).await {
            Ok(sealed) => Ok(Registered {
                handle,
                sealed,
                material: Some(material),
            }),
            Err(e) => {
                remove_wallet_logged(custody, handle, None, "sealing a store's keys failed").await;
                Err(key_custody_error_for_new_tenant(e))
            }
        };
    }
    if !keys.view_key_hex.trim().is_empty() {
        return Err(ApiError::BadRequest(format!(
            "the {backend} key custody backend takes keys only encrypted to it (key-custody-cli or the key entry form); the keys sent in the clear were not used"
        )));
    }
    let envelope = Envelope::from_text(encrypted.ok_or_else(|| {
        ApiError::BadRequest(format!(
            "the {backend} key custody backend needs the store's keys encrypted to it (encrypted_keys)"
        ))
    })?)
    .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let (handle, sealed) = custody
        .register_envelope_in(backend, &envelope, action, store)
        .await
        .map_err(key_custody_error_for_new_tenant)?;
    Ok(Registered {
        handle,
        sealed,
        material: None,
    })
}

async fn create_tenant_to_completion(
    state: AppState,
    req: CreateTenantRequest,
) -> Result<Json<CreateTenantResponse>, ApiError> {
    let network = parse_network(req.network.as_deref().unwrap_or("mainnet"))
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    if !state.networks.daemons.is_configured(network) {
        return Err(ApiError::BadRequest(format!(
            "no monero_node is configured for network {:?} on this instance",
            network_str(network)
        )));
    }

    validate_tenant_settings(req.confirmations_required, req.order_expiry_seconds)?;
    let backend = chosen_backend(&state, req.key_custody_backend.as_deref())?;

    let Registered {
        handle,
        sealed,
        material: _,
    } = register_keys(&state, &backend, &req.keys, Action::Create, None).await?;
    let primary_address = match state
        .custody
        .backends
        .derive_subaddress(handle, SubaddressIndex::default(), network)
        .await
    {
        Ok(address) => address,
        Err(e) => {
            remove_wallet_logged(
                state.custody.backends.as_ref(),
                handle,
                None,
                "creating a store, deriving its address failed",
            )
            .await;
            return Err(key_custody_error_for_new_tenant(e));
        }
    };
    let defaults = state.settings.tenant_defaults.load();

    let new_tenant = NewTenant {
        // The backend that sealed these keys and holds them (part 5).
        key_custody_backend: backend.clone(),
        sealed_key_material: sealed,
        primary_address: primary_address.to_string(),
        network: network_str(network).to_owned(),
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
        .db
        .write(move |s| s.create_tenant(&new_tenant, now_unix()))
        .await;
    // A failed insert leaves a registered wallet nothing holds a handle to - key
    // material live in `KeyCustody` for the rest of the process's life, with no
    // tenant row to ever offboard it. Hand it back before returning the error.
    let created = match created {
        Ok(created) => created,
        Err(e) => {
            remove_wallet_logged(
                state.custody.backends.as_ref(),
                handle,
                None,
                "creating a store, saving it failed",
            )
            .await;
            return Err(e.into());
        }
    };

    state
        .custody
        .wallet_handles
        .write()
        .insert(created.tenant.id.clone(), handle);

    Ok(Json(CreateTenantResponse {
        tenant_id: created.tenant.id,
        public_key: created.tenant.public_key,
        secret_token: created.secret_token.expose().to_owned(),
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
pub(crate) const MAX_CONFIRMATIONS_REQUIRED: u64 = shared::order_status::MAX_CONFIRMATIONS_REQUIRED;

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
pub(super) struct TenantView {
    tenant_id: crate::store::TenantId,
    public_key: String,
    primary_address: String,
    network: String,
    confirmations_required: u64,
    order_expiry_seconds: i64,
    key_custody_backend: String,
}

impl From<crate::store::Tenant> for TenantView {
    fn from(t: crate::store::Tenant) -> Self {
        Self {
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
pub(super) struct SwitchKeyCustodyRequest {
    backend: String,
    #[serde(flatten)]
    keys: KeysIn,
}

/// Whether `material` is the wallet `primary_address` belongs to, on
/// `network` (task 5.3). Compares keys, not address strings: the stored
/// address may be in any valid form (the bootstrap CLI stores what was
/// typed). The public spend key must match, and so must the public view key
/// derived from the given private view key.
pub(super) fn is_same_wallet(
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
pub(super) async fn switch_key_custody(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<SwitchKeyCustodyRequest>,
) -> Result<Json<TenantView>, ApiError> {
    to_completion(switch_key_custody_to_completion(state, tenant, req)).await
}

async fn switch_key_custody_to_completion(
    state: AppState,
    tenant: crate::store::Tenant,
    req: SwitchKeyCustodyRequest,
) -> Result<Json<TenantView>, ApiError> {
    // One switch at a time: two overlapping switches of the same store could
    // otherwise leave its row saying one backend while its live handle is
    // in the other. Switches are rare, so one lock for all of them is fine.
    static SWITCHING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let network = parse_network(&tenant.network).map_err(|e| ApiError::Internal(e.to_string()))?;
    let backend = chosen_backend(&state, Some(&req.backend))?;

    let _switching = SWITCHING.lock().await;

    let store = tenant.id.as_str().to_owned();
    let Registered {
        handle,
        sealed,
        material,
    } = register_keys(&state, &backend, &req.keys, Action::Move, Some(&store)).await?;
    // Keys given in the clear are checked against the store's wallet by
    // their keys; keys that only the backend can see, by the address it
    // derives from them.
    let same = match material {
        Some(material) => is_same_wallet(&material, &tenant.primary_address, network),
        None => derives_primary_address(&state, handle, &tenant.primary_address, network).await,
    };
    if !matches!(same, Ok(true)) {
        remove_wallet_logged(
            state.custody.backends.as_ref(),
            handle,
            Some(tenant.id.as_str()),
            "moving a store's keys, they were refused",
        )
        .await;
        same?;
        return Err(ApiError::BadRequest(
            "These keys belong to a different wallet from the one this store uses.".to_owned(),
        ));
    }
    let (id, chosen) = (tenant.id.clone(), backend.clone());
    let updated = state
        .db
        .write(move |s| s.update_tenant_key_custody(&id, &chosen, &sealed))
        .await;
    if let Err(e) = updated {
        remove_wallet_logged(
            state.custody.backends.as_ref(),
            handle,
            Some(tenant.id.as_str()),
            "moving a store's keys, saving the move failed",
        )
        .await;
        return Err(e.into());
    }
    let previous = state
        .custody
        .wallet_handles
        .write()
        .insert(tenant.id.clone(), handle);
    if let Some(previous) = previous.filter(|p| *p != handle) {
        remove_wallet_logged(
            state.custody.backends.as_ref(),
            previous,
            Some(tenant.id.as_str()),
            "moved a store's keys, removing them from its old backend",
        )
        .await;
    }
    let id = tenant.id.clone();
    let refetched = state
        .db
        .write(move |s| s.get_tenant_by_id(&id))
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(TenantView::from(refetched)))
}

/// Whether the wallet registered as `handle` has `primary_address` as its
/// primary address on `network`: compared by keys, not spelling.
async fn derives_primary_address(
    state: &AppState,
    handle: WalletHandle,
    primary_address: &str,
    network: monero::Network,
) -> Result<bool, ApiError> {
    let derived = state
        .custody
        .backends
        .derive_subaddress(handle, SubaddressIndex::default(), network)
        .await
        .map_err(key_custody_error_for_new_tenant)?;
    let stored: monero::Address = primary_address
        .parse()
        .map_err(|e| ApiError::Internal(format!("the store's address doesn't parse: {e}")))?;
    Ok(derived.network == stored.network
        && derived.public_spend == stored.public_spend
        && derived.public_view == stored.public_view)
}

#[derive(Deserialize, Default)]
pub(super) struct KeyBundleRequest {
    /// The backend the keys are for; the default when absent.
    backend: Option<String>,
}

/// Which engine images a client should trust with keys: what it checks a
/// bundle's report against.
#[derive(Serialize)]
pub(super) struct TrustView {
    /// SHA-384 of the ID key the engine image must be signed with, hex.
    id_key_digest: String,
    /// Whether that is the official one built into this release (and so
    /// into the matching `key-custody-cli`): if not, merchants pass it to
    /// the CLI with `--trust-id-key`.
    official: bool,
    min_guest_svn: u32,
    /// The lowest firmware trusted: `bootloader,tee,snp,microcode`, or empty.
    min_tcb: String,
}

#[derive(Serialize)]
pub(super) struct KeyBundleResponse {
    bundle: Bundle,
    trust: TrustView,
}

fn trust_view(state: &AppState) -> Result<TrustView, ApiError> {
    let snp = state
        .custody
        .snp
        .as_ref()
        .and_then(|slot| slot.backend())
        .ok_or_else(|| ApiError::Unavailable("the snp key custody backend isn't running".into()))?;
    let trust = snp.config().trust;
    Ok(TrustView {
        id_key_digest: hex::encode(trust.id_key_digest),
        official: crate::key_custody::transport::official_id_key_digest()
            == Some(trust.id_key_digest),
        min_guest_svn: trust.min_guest_svn,
        min_tcb: trust.min_tcb.to_text(),
    })
}

async fn key_bundle(
    state: &AppState,
    backend: Option<&str>,
    action: Action,
    store: Option<&str>,
) -> Result<Json<KeyBundleResponse>, ApiError> {
    let backend = chosen_backend(state, backend)?;
    let bundle = state
        .custody
        .backends
        .key_bundle_in(&backend, action, store)
        .await
        .map_err(|e| match e {
            KeyCustodyError::InvalidKeyMaterial(m) => ApiError::BadRequest(m),
            other @ (KeyCustodyError::UnknownWallet
            | KeyCustodyError::BackendUnavailable(_)
            | KeyCustodyError::ScanFailed(_)) => other.into(),
        })?;
    Ok(Json(KeyBundleResponse {
        bundle,
        trust: trust_view(state)?,
    }))
}

/// `POST /api/v1/admin/key-custody/bundle` - a bundle to encrypt a new
/// store's keys against, for a backend that takes keys only encrypted to it
/// (`key_custody::transport`). One per key entry form: its challenge is
/// accepted once.
pub(super) async fn create_key_bundle(
    State(state): State<AppState>,
    Json(req): Json<KeyBundleRequest>,
) -> Result<Json<KeyBundleResponse>, ApiError> {
    key_bundle(&state, req.backend.as_deref(), Action::Create, None).await
}

/// `POST /api/v1/admin/tenant/key-custody/bundle` - as `create_key_bundle`,
/// for moving the authenticated store's keys: the challenge is for this
/// store only.
pub(super) async fn move_key_bundle(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<KeyBundleRequest>,
) -> Result<Json<KeyBundleResponse>, ApiError> {
    let store = tenant.id.as_str().to_owned();
    key_bundle(&state, req.backend.as_deref(), Action::Move, Some(&store)).await
}

/// `POST /api/v1/admin/key-custody/handoff` - an upgraded engine asking this
/// one for the snp master key (`key_custody::snp`). Answered only for an
/// engine image signed by this engine's own ID key at its security version
/// or later: encrypted to that engine alone, and attested by this one.
pub(super) async fn answer_handoff(
    State(state): State<AppState>,
    Json(bundle): Json<Bundle>,
) -> Result<Json<HandoffAnswer>, ApiError> {
    let slot = state.custody.snp.as_ref().ok_or_else(|| {
        ApiError::Unavailable("this engine has no snp key custody backend".into())
    })?;
    let snp = slot.backend().ok_or_else(|| {
        ApiError::Unavailable("this engine's snp key custody backend isn't running".into())
    })?;
    let answer = snp
        .answer_handoff(&bundle, slot.anchor(), crate::key_custody::snp::unix_now())
        .map_err(|e| match e {
            KeyCustodyError::InvalidKeyMaterial(m) => ApiError::Forbidden(m),
            other @ (KeyCustodyError::UnknownWallet
            | KeyCustodyError::BackendUnavailable(_)
            | KeyCustodyError::ScanFailed(_)) => other.into(),
        })?;
    tracing::info!("handed the snp master key over to an upgraded engine image");
    Ok(Json(answer))
}

pub(super) async fn get_own_tenant(AuthedTenant(tenant): AuthedTenant) -> Json<TenantView> {
    Json(TenantView::from(tenant))
}

#[derive(Deserialize, Default)]
pub(super) struct PatchTenantRequest {
    confirmations_required: Option<u64>,
    order_expiry_seconds: Option<i64>,
}

pub(super) async fn patch_own_tenant(
    AuthedTenant(tenant): AuthedTenant,
    State(db): State<Database>,
    Json(req): Json<PatchTenantRequest>,
) -> Result<Json<TenantView>, ApiError> {
    validate_tenant_settings(req.confirmations_required, req.order_expiry_seconds)?;
    let patch = TenantConfigPatch {
        confirmations_required: req.confirmations_required,
        order_expiry_seconds: req.order_expiry_seconds,
    };
    let id = tenant.id.clone();
    let refetched = db
        .write(move |s| {
            s.update_tenant_config(&id, &patch)?;
            s.get_tenant_by_id(&id)
        })
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(TenantView::from(refetched)))
}

#[derive(Serialize)]
pub(super) struct RotateSecretResponse {
    secret_token: String,
}

pub(super) async fn rotate_secret(
    AuthedTenant(tenant): AuthedTenant,
    State(db): State<Database>,
) -> Result<Json<RotateSecretResponse>, ApiError> {
    let id = tenant.id.clone();
    let new_secret = db.write(move |s| s.rotate_tenant_secret(&id)).await?;
    Ok(Json(RotateSecretResponse {
        secret_token: new_secret.expose().to_owned(),
    }))
}

/// The row is disabled first, then the keys leave custody: a request that
/// authenticated just before this one finds the store gone when it looks
/// for a handle (`resolve_wallet_handle`), instead of registering the keys
/// again after they were removed.
pub(super) async fn delete_own_tenant(
    AuthedTenant(tenant): AuthedTenant,
    State(custody): State<Custody>,
    State(db): State<Database>,
) -> Result<StatusCode, ApiError> {
    let id = tenant.id.clone();
    db.write(move |s| s.disable_tenant(&id, now_unix())).await?;
    let removed_handle = custody.wallet_handles.write().remove(&tenant.id);
    if let Some(handle) = removed_handle {
        remove_wallet_logged(
            custody.backends.as_ref(),
            handle,
            Some(tenant.id.as_str()),
            "deleting a store",
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub(super) struct OrderView {
    order_id: crate::store::OrderId,
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

/// Builds an `OrderView`, including the one field (`currently_scanning`)
/// that isn't a column: it depends on wall-clock time (the grace window),
/// which is why this isn't a plain `From<Order>`.
fn build_order_view(order: Order, now: i64, grace_period_seconds: i64) -> OrderView {
    let currently_scanning = order.in_scan_window(now, grace_period_seconds);
    OrderView {
        order_id: order.id,
        merchant_order_id: order.merchant_order_id,
        address: order.address,
        xmr_amount_piconero: order.xmr_amount_piconero,
        amount_received_piconero: order.amount_received_piconero,
        status: order.status.as_str().to_owned(),
        confirmations: order.confirmations,
        double_spend_detected_at: order.double_spend_detected_at,
        refund_address: order.refund_address,
        created_at: order.created_at,
        expires_at: order.expires_at,
        updated_at: order.updated_at,
        first_scanned_height: order.first_scanned_height,
        last_scanned_height: order.last_scanned_height,
        currently_scanning,
    }
}

#[derive(Deserialize)]
pub(super) struct ListOrdersQuery {
    status: Option<String>,
    /// `created_at` of the last order of the previous page, with its id in
    /// `cursor_id`: the next page starts after that order. Several orders
    /// can share a `created_at` second, so a cursor without the id skips the
    /// rest of that second.
    cursor: Option<i64>,
    cursor_id: Option<String>,
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
pub(super) const MAX_LIST_ORDER_IDS: usize = 100;

pub(super) async fn list_orders(
    AuthedTenant(tenant): AuthedTenant,
    State(db): State<Database>,
    State(settings): State<Arc<EngineSettings>>,
    Query(q): Query<ListOrdersQuery>,
) -> Result<Json<Vec<OrderView>>, ApiError> {
    let status_filter: Option<OrderStatus> =
        q.status.as_deref().map(parse_status_query).transpose()?;
    let limit = q.limit.unwrap_or(50).min(200);
    let now = now_unix();
    let ids: Option<Vec<crate::store::OrderId>> = q.ids.as_deref().map(|ids| {
        ids.split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(crate::store::OrderId::new)
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
    let paged = q.open.unwrap_or(false) || search.is_some() || q.offset.is_some();
    if paged && (q.status.is_some() || q.cursor.is_some() || q.cursor_id.is_some()) {
        return Err(ApiError::BadRequest(
            "status and cursor can't be combined with open, search or offset".into(),
        ));
    }
    let (open, offset, cursor, tenant_id) = (
        q.open.unwrap_or(false),
        q.offset.unwrap_or(0),
        q.cursor
            .map(|at| (at, q.cursor_id.clone().unwrap_or_default())),
        tenant.id.clone(),
    );
    let grace = settings.scan.load().expired_order_grace_period_seconds;
    let views = db
        .read(move |store| {
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
                None => store.list_orders(
                    &tenant_id,
                    status_filter,
                    limit,
                    cursor.as_ref().map(|(at, id)| (*at, id.as_str())),
                )?,
            };
            Ok(orders
                .into_iter()
                .map(|o| build_order_view(o, now, grace))
                .collect::<Vec<_>>())
        })
        .await?;
    Ok(Json(views))
}

#[derive(Serialize)]
pub(super) struct PaymentView {
    txid: String,
    output_index: i64,
    amount_piconero: u64,
    first_seen_at: i64,
    block_height: Option<i64>,
    voided_at: Option<i64>,
    /// Voided because another payment of the same output (its one-time key)
    /// is the one credited, not as a double spend.
    superseded: bool,
}

impl From<OrderPaymentRow> for PaymentView {
    fn from(p: OrderPaymentRow) -> Self {
        Self {
            txid: p.txid,
            output_index: p.output_index,
            amount_piconero: p.amount_piconero,
            first_seen_at: p.first_seen_at,
            block_height: p.block_height,
            voided_at: p.voided_at,
            superseded: p.superseded_by.is_some(),
        }
    }
}

#[derive(Serialize)]
pub(super) struct OrderDetailResponse {
    #[serde(flatten)]
    order: OrderView,
    payments: Vec<PaymentView>,
}

pub(super) async fn get_order_detail(
    AuthedTenant(tenant): AuthedTenant,
    Path(order_id): Path<crate::store::OrderId>,
    State(db): State<Database>,
    State(settings): State<Arc<EngineSettings>>,
) -> Result<Json<OrderDetailResponse>, ApiError> {
    let grace = settings.scan.load().expired_order_grace_period_seconds;
    let tenant_id = tenant.id.clone();
    let found = db
        .read(move |store| {
            let Some(order) = store.get_order(&tenant_id, &order_id)? else {
                return Ok(None);
            };
            let payments = store.get_all_payments(&order.id)?;
            Ok(Some((build_order_view(order, now_unix(), grace), payments)))
        })
        .await?;
    let (order_view, payments) = found.ok_or(ApiError::NotFound)?;
    Ok(Json(OrderDetailResponse {
        order: order_view,
        payments: payments.into_iter().map(PaymentView::from).collect(),
    }))
}

#[derive(Deserialize)]
pub(super) struct SetRefundAddressRequest {
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
pub(super) async fn set_order_refund_address(
    AuthedTenant(tenant): AuthedTenant,
    Path(order_id): Path<crate::store::OrderId>,
    State(db): State<Database>,
    Json(req): Json<SetRefundAddressRequest>,
) -> Result<(), ApiError> {
    let id = tenant.id.clone();
    let updated = db
        .write(move |s| s.set_refund_address(&id, &order_id, &req.refund_address))
        .await?;
    if updated {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

#[derive(Deserialize)]
pub(super) struct CreateWebhookRequest {
    url: String,
    extra_headers: Option<serde_json::Value>,
}

/// Most extra headers one webhook may carry, and most bytes of names and
/// values together.
const MAX_EXTRA_HEADERS: usize = 20;
const MAX_EXTRA_HEADER_BYTES: usize = 4 * 1024;

/// Header names the engine sets itself on every delivery, or that the HTTP
/// client sets from the request; a merchant header of one of these would
/// replace or break what the delivery carries.
const RESERVED_HEADERS: [&str; 5] = [
    "host",
    "content-length",
    "content-type",
    "transfer-encoding",
    "connection",
];

/// The `extra_headers` a webhook is saved with: a JSON object of string
/// values, each a valid header name and value (what `reqwest` would
/// otherwise refuse at every delivery, failing them all with a cryptic
/// error until the webhook is deleted), none of them the engine's own, in
/// lowercase. Nothing given is an empty object.
fn validate_extra_headers(extra_headers: Option<serde_json::Value>) -> Result<String, ApiError> {
    let Some(extra_headers) = extra_headers else {
        return Ok("{}".to_owned());
    };
    let serde_json::Value::Object(map) = extra_headers else {
        return Err(ApiError::BadRequest(
            "extra_headers must be an object of string values".into(),
        ));
    };
    if map.len() > MAX_EXTRA_HEADERS {
        return Err(ApiError::BadRequest(format!(
            "extra_headers may hold at most {MAX_EXTRA_HEADERS} headers"
        )));
    }
    let mut checked = serde_json::Map::with_capacity(map.len());
    let mut bytes = 0;
    for (name, value) in map {
        let Some(value) = value.as_str() else {
            return Err(ApiError::BadRequest(format!(
                "extra header {name:?} must be a string"
            )));
        };
        let name = axum::http::HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
            ApiError::BadRequest(format!("{name:?} is not a valid header name: {e}"))
        })?;
        axum::http::HeaderValue::from_str(value).map_err(|e| {
            ApiError::BadRequest(format!(
                "the value of extra header {name} is not valid: {e}"
            ))
        })?;
        let name = name.as_str();
        if RESERVED_HEADERS.contains(&name) || name.starts_with("x-monokulo-") {
            return Err(ApiError::BadRequest(format!(
                "extra header {name} is set by the engine itself"
            )));
        }
        bytes += name.len() + value.len();
        if bytes > MAX_EXTRA_HEADER_BYTES {
            return Err(ApiError::BadRequest(format!(
                "extra_headers may hold at most {MAX_EXTRA_HEADER_BYTES} bytes"
            )));
        }
        checked.insert(name.to_owned(), serde_json::Value::String(value.to_owned()));
    }
    Ok(serde_json::Value::Object(checked).to_string())
}

#[derive(Serialize)]
pub(super) struct CreateWebhookResponse {
    webhook_id: crate::store::WebhookId,
    signing_secret: String,
}

pub(super) async fn create_webhook(
    AuthedTenant(tenant): AuthedTenant,
    State(db): State<Database>,
    Json(req): Json<CreateWebhookRequest>,
) -> Result<Json<CreateWebhookResponse>, ApiError> {
    let parsed =
        url::Url::parse(&req.url).map_err(|e| ApiError::BadRequest(format!("invalid url: {e}")))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(ApiError::BadRequest(
            "only http/https URLs are allowed".into(),
        ));
    }
    // Resolved-IP SSRF validation (see webhook_sign::is_disallowed_address) happens
    // at delivery time in the not-yet-built delivery worker, not here - DNS can
    // change between registration and delivery, so a registration-time-only check
    // would be insufficient on its own regardless.
    let extra_headers_json = validate_extra_headers(req.extra_headers)?;
    let secret = generate_webhook_secret();
    let (tenant_id, url, signing_secret) = (tenant.id.clone(), req.url.clone(), secret.clone());
    let webhook = db
        .write(move |store| {
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
pub(super) struct WebhookView {
    webhook_id: crate::store::WebhookId,
    url: String,
    enabled: bool,
    created_at: i64,
}

impl From<Webhook> for WebhookView {
    fn from(w: Webhook) -> Self {
        Self {
            webhook_id: w.id,
            url: w.url,
            enabled: w.enabled,
            created_at: w.created_at,
        }
    }
}

pub(super) async fn list_webhooks(
    AuthedTenant(tenant): AuthedTenant,
    State(db): State<Database>,
) -> Result<Json<Vec<WebhookView>>, ApiError> {
    let id = tenant.id.clone();
    let webhooks = db.read(move |s| s.list_webhooks(&id)).await?;
    Ok(Json(webhooks.into_iter().map(WebhookView::from).collect()))
}

pub(super) async fn delete_webhook(
    AuthedTenant(tenant): AuthedTenant,
    Path(webhook_id): Path<crate::store::WebhookId>,
    State(db): State<Database>,
) -> Result<StatusCode, ApiError> {
    let id = tenant.id.clone();
    let deleted = db
        .write(move |s| s.delete_webhook(&id, &webhook_id))
        .await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

#[derive(Deserialize)]
pub(super) struct LookupPaymentRequest {
    txid: String,
}

/// `docs/txid_lookup_and_scan_chunking_wbs.md` Part B's own direct replacement
/// for the manual chain-rescan feature above: no block-range walk, no
/// background job, no per-tenant "one at a time" guardrail to enforce - one
/// daemon call for the transaction and where it is (`find_transaction`), one
/// for the chain height if it pays an order, and the same
/// `scan_transaction`/`record_scan_match` primitives the live scanner already
/// uses, narrowed to nothing (this scans the tenant's *whole* address range,
/// not one order's `minor_index` - see below for why).
#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(super) enum PaymentLookupView {
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
    Matched {
        order_ids: Vec<crate::store::OrderId>,
    },
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
/// Uses `state.networks.daemons` (the live scanner's own pool), not a separate one:
/// this is two quick calls, not a bulk historical walk, so it poses none of
/// the sustained-request-volume contention `AppState::rescan_daemons` exists
/// to prevent - see that field's own doc comment for the class of problem
/// this endpoint is deliberately too small to be.
pub(super) async fn lookup_payment(
    AuthedTenant(tenant): AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<LookupPaymentRequest>,
) -> Result<Json<PaymentLookupView>, ApiError> {
    let txid = req.txid.trim().to_lowercase();
    if txid.len() != 64 || !txid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::BadRequest(
            "txid must be 64 hex characters".to_owned(),
        ));
    }

    let network = parse_network(&tenant.network).map_err(|e| {
        ApiError::Internal(format!(
            "tenant has an unrecognized network {:?}: {e}",
            tenant.network
        ))
    })?;
    let daemon = state.networks.daemons.get(network).ok_or_else(|| {
        ApiError::Unavailable(format!(
            "no Monero node is configured for network {network:?}"
        ))
    })?;

    // The transaction and where it is, in one answer. It may come pruned
    // (all a scan reads), which is why its id is passed along with it.
    let found = daemon.find_transaction(&txid).await?;
    let (tx, block_height) = match found {
        None | Some((_, crate::daemon::TxLocation::NotFound)) => {
            return Ok(Json(PaymentLookupView::NotFoundOnChain))
        }
        Some((fetched, crate::daemon::TxLocation::InPool)) => (fetched.tx, None),
        Some((fetched, crate::daemon::TxLocation::InBlock(h))) => (fetched.tx, Some(h)),
    };
    let mut handle = resolve_wallet_handle(&state, &tenant).await?;
    let now = now_unix();

    // Computed (async, no `&Store` held) then persisted (sync, brief lock) as
    // two separate steps, same as the scheduler/`engine::rescan_order`
    // already do everywhere else in this codebase - never a single
    // await-spanning call holding the store's lock.
    let mut retries = 0;
    let scan = loop {
        match crate::scanner::scan_transaction_as(
            state.custody.backends.as_ref(),
            handle,
            &txid,
            &tx,
            0..tenant.next_minor_index,
        )
        .await
        {
            Ok(scan) => break scan,
            Err(crate::scanner::ScannerError::KeyCustody(KeyCustodyError::UnknownWallet))
                if retries < super::UNKNOWN_WALLET_RETRIES =>
            {
                retries += 1;
                super::forget_wallet_handle(&state, &tenant.id, handle);
                handle = resolve_wallet_handle(&state, &tenant).await?;
            }
            Err(e) => return Err(e.into()),
        }
    };
    // The block's own list of transactions, so that under proof-of-work
    // checking the payment settles only if that block is the proven one
    // (docs/proof_of_work.md).
    let found_in = match block_height {
        Some(height) => crate::work::chain::block_holding(daemon.as_ref(), &txid, height).await,
        None => None,
    };
    let id = tenant.id.clone();
    let touched = state
        .db
        .write(move |store| {
            let touched = crate::scanner::record_scan_match(store, &id, &scan, now, block_height)?;
            if let (false, Some(height), Some(hash)) = (touched.is_empty(), block_height, &found_in)
            {
                store.attest_payment_block(&scan.txid, height, hash)?;
            }
            Ok::<_, crate::scanner::ScannerError>(touched)
        })
        .await?;

    if touched.is_empty() {
        return Ok(Json(PaymentLookupView::NoMatchingOrder));
    }

    // A current tip for `recompute_order_status` to derive confirmation counts
    // against - `block_height` itself is not a safe substitute (a mempool
    // match has none, and even a mined match's own height could already be
    // behind the real tip by an unrelated confirmation or two).
    let current_height = daemon.get_height().await?;
    let orders: Vec<crate::store::OrderId> = touched.iter().cloned().collect();
    state
        .db
        .write(move |store| {
            for order_id in &orders {
                crate::scanner::recompute_and_notify(store, order_id, current_height, now)?;
            }
            Ok::<(), crate::scanner::ScannerError>(())
        })
        .await?;

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
pub(super) async fn order_events(
    AuthedTenant(tenant): AuthedTenant,
    State(db): State<Database>,
) -> axum::response::sse::Sse<
    impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
> {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use tokio::sync::broadcast::error::RecvError;

    let receiver = db.subscribe_order_changes();
    let ready =
        futures_util::stream::once(async { Ok(Event::default().event("ready").data("{}")) });
    let changes =
        futures_util::stream::unfold((receiver, tenant.id), async |(mut receiver, tenant_id)| {
            loop {
                match receiver.recv().await {
                    Ok(change) if change.tenant_id == tenant_id => {
                        let data = serde_json::json!({ "order_id": change.order_id }).to_string();
                        return Some((
                            Ok(Event::default().event("order").data(data)),
                            (receiver, tenant_id),
                        ));
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {
                        return Some((
                            Ok(Event::default().event("resync").data("{}")),
                            (receiver, tenant_id),
                        ));
                    }
                    Err(RecvError::Closed) => return None,
                }
            }
        });
    Sse::new(futures_util::StreamExt::chain(ready, changes))
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
}
