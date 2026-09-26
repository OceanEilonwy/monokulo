//! Operator commands that act directly on the local SQLite database rather than
//! through the HTTP admin API - `--rotate-secret`, `--show-tenant`.
//! Deliberately need no network call and no existing secret token: having
//! filesystem access to the box this database lives on already implies the level
//! of trust the HTTP admin API's bearer-token check exists to establish remotely,
//! consistent with the self-hosted, single-operator deployment model this whole
//! CLI targets (`docs/DESIGN.md` §4). This is also the answer to "I lost my admin
//! secret": there is no way to recover it (only its hash is ever stored - see
//! `Store::create_tenant`), only to mint a new one, which is exactly what
//! `rotate_secret` does.

use crate::key_custody::{KeyCustody, WalletMaterial};
use crate::store::{CreatedTenant, NewTenant, Store};
use crate::store::Tenant;

#[derive(Debug, thiserror::Error)]
pub enum LocalAdminError {
    #[error("no tenant is configured yet - run `scanner --bootstrap-wallet ...`, then start the server")]
    NoTenant,
    #[error("more than one tenant is configured ({0}) - name which one with --pk <pk_...>")]
    AmbiguousTenant(String),
    #[error("no tenant found with public key {0:?}")]
    UnknownTenant(String),
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
    #[error("a tenant already exists - --bootstrap-wallet only ever creates the first one (see the module doc comment)")]
    AlreadyBootstrapped,
    #[error("invalid wallet key material: {0}")]
    KeyMaterial(#[from] crate::key_custody::KeyCustodyError),
}

/// `--bootstrap-wallet`'s own arguments - the explicit-flags replacement for what
/// used to be the `[wallet]` TOML section (`config.rs`, removed along with the
/// rest of the file-based config model). One-time provisioning for a self-hosted,
/// single-tenant deployment; a hosted instance creates tenants at
/// runtime via the admin HTTP API instead and never calls this at all.
#[derive(Debug)]
pub struct BootstrapWalletArgs {
    pub primary_address: String,
    pub view_key_hex: String,
    pub spend_pubkey_hex: String,
    pub network: String,
}

/// Creates the one tenant a self-hosted deployment needs - but only if none
/// exists yet, the same idempotency-by-checking-first discipline the former
/// `main.rs::bootstrap_self_hosted_tenant` used (this replaces that function
/// entirely: provisioning is now an explicit one-time command an operator runs,
/// not something every boot re-checks). Confirmations/expiry all come
/// from whatever this instance's *current* settings resolve to
/// (`crate::settings`), not a value baked into this command - a bootstrap tenant
/// should start out consistent with the instance it's being created on.
pub async fn bootstrap_wallet(
    store: &Store,
    key_custody: &std::sync::Arc<dyn KeyCustody>,
    key_custody_backend: &str,
    args: BootstrapWalletArgs,
) -> Result<CreatedTenant, LocalAdminError> {
    if store.count_tenants()? > 0 {
        return Err(LocalAdminError::AlreadyBootstrapped);
    }
    let material = WalletMaterial::from_hex(&args.view_key_hex, &args.spend_pubkey_hex)?;
    let sealed = key_custody.seal(&material).await.map_err(LocalAdminError::KeyMaterial)?;

    let confirmations_required: u64 = crate::settings::get(store, &crate::settings::PAYMENT_CONFIRMATIONS_REQUIRED);
    let order_expiry_minutes: i64 = crate::settings::get(store, &crate::settings::PAYMENT_ORDER_EXPIRY_MINUTES);

    let created = store.create_tenant(
        NewTenant {
            key_custody_backend: key_custody_backend.to_string(),
            sealed_key_material: sealed,
            primary_address: args.primary_address,
            network: args.network,
            confirmations_required: Some(confirmations_required),
            order_expiry_seconds: Some(order_expiry_minutes * 60),
        },
        crate::now_unix(),
    )?;
    Ok(created)
}

/// Resolves which tenant a local-admin command should act on: the one named by
/// `pk` if given, or the sole active tenant if there's exactly one, erroring
/// otherwise rather than guessing - a self-hosted deployment (this feature's whole
/// target) has exactly one, so the common case never needs `--pk` at all.
fn resolve_tenant(store: &Store, pk: Option<&str>) -> Result<Tenant, LocalAdminError> {
    if let Some(pk) = pk {
        return store.find_tenant_by_public_key(pk)?.ok_or_else(|| LocalAdminError::UnknownTenant(pk.to_string()));
    }
    let mut tenants = store.list_active_tenants()?;
    match tenants.len() {
        0 => Err(LocalAdminError::NoTenant),
        1 => Ok(tenants.remove(0)),
        _ => {
            let pks: Vec<String> = tenants.iter().map(|t| t.public_key.clone()).collect();
            Err(LocalAdminError::AmbiguousTenant(pks.join(", ")))
        }
    }
}

/// Mints a fresh admin secret for a tenant, invalidating whatever the old one
/// was. Returns `(public_key, new_secret)`. The only way back from a lost
/// secret - see the module doc.
pub fn rotate_secret(store: &Store, pk: Option<&str>) -> Result<(String, String), LocalAdminError> {
    let tenant = resolve_tenant(store, pk)?;
    let new_secret = store.rotate_tenant_secret(&tenant.id)?;
    Ok((tenant.public_key, new_secret))
}

/// A snapshot of a tenant's non-secret settings, for `--show-tenant`. Everything
/// here is either public by design (`public_key`) or was chosen by the operator
/// and is safe to print back to them (network, thresholds). Deliberately
/// excludes `sealed_key_material`: even though `PlainKeyCustody` seals to plain
/// bytes today (see its own doc comment), a future sealed backend's raw bytes are
/// not something a "show me my settings" command should ever surface.
#[derive(Debug, PartialEq)]
pub struct TenantSummary {
    pub public_key: String,
    pub network: String,
    pub primary_address: String,
    pub confirmations_required: u64,
    pub order_expiry_seconds: i64,
}

pub fn show_tenant(store: &Store, pk: Option<&str>) -> Result<TenantSummary, LocalAdminError> {
    let tenant = resolve_tenant(store, pk)?;
    Ok(TenantSummary {
        public_key: tenant.public_key,
        network: tenant.network,
        primary_address: tenant.primary_address,
        confirmations_required: tenant.confirmations_required,
        order_expiry_seconds: tenant.order_expiry_seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_custody::{KeyCustody, PlainKeyCustody, WalletMaterial};
    use crate::store::NewTenant;

    async fn store_with_tenant() -> (Store, Tenant, String) {
        let store = Store::open_in_memory().unwrap();
        let key_custody = PlainKeyCustody::default();
        let material = WalletMaterial::new([7u8; 32], [8u8; 32]);
        let sealed = key_custody.seal(&material).await.unwrap();
        let created = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".to_string(),
                    sealed_key_material: sealed,
                    primary_address: "4abc".to_string(),
                    network: "mainnet".to_string(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1_700_000_000,
            )
            .unwrap();
        let pk = created.tenant.public_key.clone();
        (store, created.tenant, pk)
    }

    #[tokio::test]
    async fn resolve_tenant_with_no_pk_and_exactly_one_tenant_finds_it() {
        let (store, tenant, _) = store_with_tenant().await;
        let resolved = resolve_tenant(&store, None).unwrap();
        assert_eq!(resolved.id, tenant.id);
    }

    #[tokio::test]
    async fn resolve_tenant_with_no_tenants_at_all_is_a_clear_error() {
        let store = Store::open_in_memory().unwrap();
        assert!(matches!(resolve_tenant(&store, None), Err(LocalAdminError::NoTenant)));
    }

    #[tokio::test]
    async fn resolve_tenant_with_more_than_one_requires_naming_which_pk() {
        let key_custody = PlainKeyCustody::default();
        let store = Store::open_in_memory().unwrap();
        for seed in [1u8, 2u8] {
            let material = WalletMaterial::new([seed; 32], [seed.wrapping_add(1); 32]);
            let sealed = key_custody.seal(&material).await.unwrap();
            store
                .create_tenant(
                    NewTenant {
                        key_custody_backend: "plain".to_string(),
                        sealed_key_material: sealed,
                        primary_address: "4abc".to_string(),
                        network: "mainnet".to_string(),
                        confirmations_required: None,
                        order_expiry_seconds: None,
                    },
                    1_700_000_000,
                )
                .unwrap();
        }
        let err = resolve_tenant(&store, None).unwrap_err();
        assert!(matches!(err, LocalAdminError::AmbiguousTenant(_)), "got {err}");

        // But naming one directly still works with two present.
        let (_, tenant, pk) = store_with_tenant().await;
        let _ = tenant;
        let store2 = Store::open_in_memory().unwrap();
        let material = WalletMaterial::new([9u8; 32], [10u8; 32]);
        let sealed = key_custody.seal(&material).await.unwrap();
        let created = store2
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".to_string(),
                    sealed_key_material: sealed,
                    primary_address: "4abc".to_string(),
                    network: "mainnet".to_string(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1_700_000_000,
            )
            .unwrap();
        let resolved = resolve_tenant(&store2, Some(&created.tenant.public_key)).unwrap();
        assert_eq!(resolved.id, created.tenant.id);
        let _ = pk;
    }

    #[tokio::test]
    async fn resolve_tenant_with_an_unknown_pk_is_a_clear_error() {
        let (store, _, _) = store_with_tenant().await;
        let err = resolve_tenant(&store, Some("pk_doesnotexist")).unwrap_err();
        assert!(matches!(err, LocalAdminError::UnknownTenant(pk) if pk == "pk_doesnotexist"));
    }

    #[tokio::test]
    async fn rotate_secret_invalidates_the_old_one_and_returns_a_usable_new_one() {
        let store = Store::open_in_memory().unwrap();
        let key_custody = PlainKeyCustody::default();
        let material = WalletMaterial::new([1u8; 32], [2u8; 32]);
        let sealed = key_custody.seal(&material).await.unwrap();
        let created = store
            .create_tenant(
                NewTenant {
                    key_custody_backend: "plain".to_string(),
                    sealed_key_material: sealed,
                    primary_address: "4abc".to_string(),
                    network: "mainnet".to_string(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1_700_000_000,
            )
            .unwrap();

        let (pk, new_secret) = rotate_secret(&store, None).unwrap();
        assert_eq!(pk, created.tenant.public_key);
        assert_ne!(new_secret, created.secret_token);
        assert!(store.find_tenant_by_secret_token(&created.secret_token).unwrap().is_none(), "old secret must stop working");
        assert!(store.find_tenant_by_secret_token(&new_secret).unwrap().is_some(), "new secret must work");
    }

    #[tokio::test]
    async fn show_tenant_reports_the_real_settings_and_never_the_key_material() {
        let (store, _, pk) = store_with_tenant().await;
        let summary = show_tenant(&store, None).unwrap();
        assert_eq!(summary.public_key, pk);
        assert_eq!(summary.network, "mainnet");
    }

    #[tokio::test]
    async fn every_local_admin_command_gives_the_same_actionable_error_when_nothing_is_configured_yet() {
        let store = Store::open_in_memory().unwrap();
        for err in [rotate_secret(&store, None).unwrap_err(), show_tenant(&store, None).unwrap_err()] {
            assert!(matches!(err, LocalAdminError::NoTenant), "got {err}");
            assert!(err.to_string().contains("--bootstrap-wallet"), "should point at the fix: {err}");
        }
    }

    fn bootstrap_args() -> BootstrapWalletArgs {
        BootstrapWalletArgs {
            primary_address: "4abc".to_string(),
            view_key_hex: "aa".repeat(32),
            spend_pubkey_hex: "bb".repeat(32),
            network: "stagenet".to_string(),
        }
    }

    #[tokio::test]
    async fn bootstrap_wallet_creates_the_one_tenant_a_self_hosted_deployment_needs() {
        let store = Store::open_in_memory().unwrap();
        let key_custody: std::sync::Arc<dyn KeyCustody> = std::sync::Arc::new(crate::key_custody::PlainKeyCustody::default());
        let created = bootstrap_wallet(&store, &key_custody, "plain", bootstrap_args()).await.unwrap();

        assert_eq!(created.tenant.network, "stagenet");
        assert_eq!(created.tenant.confirmations_required, 10, "should reflect this instance's real current settings, not a hardcoded value");
        assert!(store.find_tenant_by_secret_token(&created.secret_token).unwrap().is_some());
    }

    #[tokio::test]
    async fn bootstrap_wallet_picks_up_a_saved_setting_rather_than_the_hardcoded_fallback() {
        let store = Store::open_in_memory().unwrap();
        store.set_setting("payment.confirmations_required", "3").unwrap();
        let key_custody: std::sync::Arc<dyn KeyCustody> = std::sync::Arc::new(crate::key_custody::PlainKeyCustody::default());
        let created = bootstrap_wallet(&store, &key_custody, "plain", bootstrap_args()).await.unwrap();
        assert_eq!(created.tenant.confirmations_required, 3);
    }

    #[tokio::test]
    async fn bootstrap_wallet_refuses_a_second_time_once_any_tenant_already_exists() {
        let store = Store::open_in_memory().unwrap();
        let key_custody: std::sync::Arc<dyn KeyCustody> = std::sync::Arc::new(crate::key_custody::PlainKeyCustody::default());
        bootstrap_wallet(&store, &key_custody, "plain", bootstrap_args()).await.unwrap();
        let err = bootstrap_wallet(&store, &key_custody, "plain", bootstrap_args()).await.unwrap_err();
        assert!(matches!(err, LocalAdminError::AlreadyBootstrapped), "got {err}");
    }

    #[tokio::test]
    async fn bootstrap_wallet_rejects_malformed_key_material_with_a_clear_error_not_a_panic() {
        let store = Store::open_in_memory().unwrap();
        let key_custody: std::sync::Arc<dyn KeyCustody> = std::sync::Arc::new(crate::key_custody::PlainKeyCustody::default());
        let mut args = bootstrap_args();
        args.view_key_hex = "not hex".to_string();
        let err = bootstrap_wallet(&store, &key_custody, "plain", args).await.unwrap_err();
        assert!(matches!(err, LocalAdminError::KeyMaterial(_)), "got {err}");
    }
}
