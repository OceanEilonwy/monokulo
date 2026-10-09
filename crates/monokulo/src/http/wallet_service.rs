//! Adding a merchant's wallet (docs/wallets.md): registering its keys with
//! the engine once, naming it, and refusing a wallet the account already
//! has. Shared by the "Bring your own wallet" form, the "Create a new
//! wallet" page, and store creation from keys (`POST /connections`).

use crate::db::{EngineWalletId, NewWalletRow, UserRow, WalletId, WalletOrigin, WalletRow};
use crate::engine_client::{CreateWalletRequest, EngineClientError};
use crate::now_unix;

use super::AppState;

/// A wallet to add: its keys as the key entry fields take them, and what
/// monokulo records about it.
pub(crate) struct AddWallet {
    /// As typed; blank picks a friendly name.
    pub name: String,
    pub view_key_hex: String,
    pub spend_pubkey_hex: String,
    pub encrypted_keys: Option<String>,
    pub network: String,
    pub key_custody_backend: Option<String>,
    pub origin: WalletOrigin,
    pub backup: Option<String>,
    /// For a brought-in wallet: which app it's in, if said
    /// (`crate::wallets::is_known_app`).
    pub app: Option<String>,
    /// For a wallet made in the browser: the address the page worked out
    /// from the phrase, which must be the one the engine derives from the
    /// keys it was sent.
    pub expected_address: Option<String>,
}

pub(crate) enum AddWalletError {
    /// The merchant's to fix (bad keys, a name already used).
    Invalid(String),
    /// The account already has this wallet.
    AlreadyAdded(Box<WalletRow>),
    Internal,
}

/// Registers the wallet's keys with the engine and records it for `user`.
/// Nothing is left behind in the engine if it isn't recorded.
pub(crate) async fn add_wallet(
    state: &AppState,
    user: &UserRow,
    req: AddWallet,
) -> Result<WalletRow, AddWalletError> {
    let name = crate::wallets::clean_name(&req.name).map_err(AddWalletError::Invalid)?;
    if shared::network::parse_network(&req.network).is_err() {
        return Err(AddWalletError::Invalid(format!(
            "{:?} is not a Monero network",
            req.network
        )));
    }
    if let Some(backup) = req.backup.as_deref() {
        if !crate::wallets::is_known_backup(backup) {
            return Err(AddWalletError::Invalid(
                "That backup method isn't one this page offers.".to_owned(),
            ));
        }
    }
    if let Some(app) = req.app.as_deref() {
        if !crate::wallets::is_known_app(app) {
            return Err(AddWalletError::Invalid(
                "That wallet app isn't one this page offers.".to_owned(),
            ));
        }
    }
    // As for a store: the engine's default key storage decides how the
    // keys may travel, so its status is read first if it isn't known.
    if req.key_custody_backend.is_none() {
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(1500),
            super::status_page::get_status_cached(&state.engine),
        )
        .await;
    }
    let (key_custody_backend, keys) = super::key_entry::store_keys(
        state,
        req.key_custody_backend.as_deref(),
        &req.view_key_hex,
        &req.spend_pubkey_hex,
        req.encrypted_keys.as_deref(),
    )
    .map_err(AddWalletError::Invalid)?;
    let created = state
        .engine
        .client
        .create_wallet(CreateWalletRequest {
            keys,
            network: req.network.clone(),
            key_custody_backend,
        })
        .await
        .map_err(|e| match e {
            EngineClientError::EngineError { status, message }
                if status == reqwest::StatusCode::BAD_REQUEST =>
            {
                AddWalletError::Invalid(message)
            }
            other => {
                tracing::error!(error = %other, "the engine could not add a wallet");
                AddWalletError::Internal
            }
        })?;
    let engine_wallet = created.wallet_id.clone();
    let give_back = |why: &'static str| {
        let engine_wallet = engine_wallet.clone();
        async move {
            if let Err(e) = state.engine.client.retire_wallet(&engine_wallet).await {
                tracing::error!(error = %e, wallet = %engine_wallet, why, "an engine wallet nothing records could not be removed");
            }
        }
    };

    if let Some(expected) = req.expected_address.as_deref() {
        if expected != created.primary_address {
            give_back("the page's address didn't match").await;
            return Err(AddWalletError::Invalid(
                "The keys sent don't match the wallet this page made. Nothing was saved: reload the page and start again.".to_owned(),
            ));
        }
    }

    let (user_id, network, address) = (
        user.id.clone(),
        created.network.clone(),
        created.primary_address.clone(),
    );
    let existing = state
        .db
        .read(move |db| db.find_wallet_by_address(&user_id, &network, &address))
        .await
        .map_err(|_| AddWalletError::Internal)?;
    if let Some(existing) = existing {
        give_back("the account already has this wallet").await;
        return Err(AddWalletError::AlreadyAdded(Box::new(existing)));
    }

    let id = WalletId::new(format!("w_{}", uuid::Uuid::new_v4().simple()));
    let user_id = user.id.clone();
    let row_id = id.clone();
    let origin = req.origin;
    let backup = req.backup.clone();
    let app = req.app.clone();
    let recorded = state
        .db
        .write(move |db| {
            let name = match name {
                Some(name) => name,
                None => {
                    let taken = db.wallet_names(&user_id)?;
                    crate::wallets::friendly_name(&taken)
                }
            };
            db.create_wallet(&NewWalletRow {
                id: &row_id,
                user_id: &user_id,
                name: &name,
                network: &created.network,
                primary_address: &created.primary_address,
                engine_wallet_id: &created.wallet_id,
                origin,
                backup: backup.as_deref(),
                app: app.as_deref(),
                created_at: now_unix(),
            })?;
            Ok::<_, crate::db::DbError>(name)
        })
        .await;
    match recorded {
        Ok(_) => {}
        Err(e) if e.is_unique_violation() => {
            give_back("its name was already used").await;
            return Err(AddWalletError::Invalid(
                "You already have a wallet with that name. Pick another, or leave it blank for one to be chosen."
                    .to_owned(),
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "could not record a wallet");
            give_back("recording it failed").await;
            return Err(AddWalletError::Internal);
        }
    }
    let user_id = user.id.clone();
    state
        .db
        .read(move |db| db.get_wallet(&user_id, &id))
        .await
        .ok()
        .flatten()
        .ok_or(AddWalletError::Internal)
}

/// Gives every store of `user`'s made before wallets the wallet it takes
/// payments into: the engine says which wallet that is, and it's matched
/// to one the account already has by address, or added as a brought-in
/// wallet with a friendly name. A store the engine can't answer for is
/// left for next time.
pub(crate) async fn adopt_unlinked_stores(state: &AppState, user: &UserRow) {
    let user_id = user.id.clone();
    let Ok(stores) = state
        .db
        .read(move |db| db.list_store_connections_for_user(&user_id))
        .await
    else {
        return;
    };
    for store in stores.into_iter().filter(|s| s.wallet_id.is_none()) {
        let Ok(sk) = super::orders::decrypt_sk(&state.encryption_key, &store) else {
            continue;
        };
        let tenant = match state.engine.client.get_tenant(&sk).await {
            Ok(tenant) => tenant,
            Err(e) => {
                tracing::warn!(error = %e, store = %store.id, "could not read a store's wallet from the engine");
                continue;
            }
        };
        let Some(engine_wallet) = tenant.wallet_id else {
            continue;
        };
        let user_id = user.id.clone();
        let connection = store.id.clone();
        let adopted = state
            .db
            .write(move |db| {
                let wallet = match db.find_wallet_by_address(
                    &user_id,
                    &tenant.network,
                    &tenant.primary_address,
                )? {
                    Some(wallet) => wallet.id,
                    None => {
                        let taken = db.wallet_names(&user_id)?;
                        let id = WalletId::new(format!("w_{}", uuid::Uuid::new_v4().simple()));
                        db.create_wallet(&NewWalletRow {
                            id: &id,
                            user_id: &user_id,
                            name: &crate::wallets::friendly_name(&taken),
                            network: &tenant.network,
                            primary_address: &tenant.primary_address,
                            engine_wallet_id: &EngineWalletId::new(engine_wallet.as_str()),
                            origin: WalletOrigin::Imported,
                            backup: None,
                            app: None,
                            created_at: now_unix(),
                        })?;
                        id
                    }
                };
                db.adopt_store_wallet(&connection, &wallet, now_unix())
            })
            .await;
        if let Err(e) = adopted {
            tracing::warn!(error = %e, store = %store.id, "could not link a store to its wallet");
        }
    }
}
