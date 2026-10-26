//! Adding a merchant's wallet: registering its keys with
//! the engine once, naming it, and refusing a wallet the account already
//! has. Shared by the "Bring your own wallet" form and the "Create a new
//! wallet" page.

use crate::db::{NewWalletRow, UserRow, WalletId, WalletOrigin, WalletRow};
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
    /// How a made wallet's recovery phrase was saved, as the form sent it
    /// (`crate::wallets::Backup`).
    pub backup: Option<String>,
    /// For a brought-in wallet: which app it's in, if said, as the form
    /// sent it (`crate::wallets::BroughtFrom`).
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
    let network = engine_protocol::network::parse_network(&req.network).map_err(|_| {
        AddWalletError::Invalid(format!("{:?} is not a Monero network", req.network))
    })?;
    let backup = match req.backup.as_deref() {
        Some(backup) => Some(crate::wallets::Backup::parse(backup).ok_or_else(|| {
            AddWalletError::Invalid("That backup method isn't one this page offers.".to_owned())
        })?),
        None => None,
    };
    let app = match req.app.as_deref() {
        Some(app) => Some(crate::wallets::BroughtFrom::parse(app).ok_or_else(|| {
            AddWalletError::Invalid("That wallet app isn't one this page offers.".to_owned())
        })?),
        None => None,
    };
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
            network: Some(req.network.clone()),
            key_custody_backend: key_custody_backend.map(Into::into),
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

    let id = WalletId::new(format!("w_{}", uuid::Uuid::new_v4().simple()));
    let user_id = user.id.clone();
    let row_id = id.clone();
    let origin = req.origin;
    // Whether the account has it already, and adding it, in one write: two
    // submissions of the same wallet at once find the first's row, not a
    // unique violation taken for the name's.
    let recorded = state
        .db
        .write(move |db| {
            if let Some(existing) =
                db.find_wallet_by_address(&user_id, &created.network, &created.primary_address)?
            {
                return Ok(Err(existing));
            }
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
                network,
                primary_address: &created.primary_address,
                engine_wallet_id: &created.wallet_id,
                origin,
                backup,
                app,
                created_at: now_unix(),
            })?;
            Ok::<_, crate::db::DbError>(Ok(name))
        })
        .await;
    match recorded {
        Ok(Ok(_)) => {}
        Ok(Err(existing)) => {
            give_back("the account already has this wallet").await;
            return Err(AddWalletError::AlreadyAdded(Box::new(existing)));
        }
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
/// to the one here standing for that engine wallet, or added as a
/// brought-in wallet with a friendly name. Engine migration 0028 gave each
/// such store an engine wallet of its own, so two of them on the same keys
/// are on two engine wallets: the second is moved onto the first's in the
/// engine (as "change the wallet" does) before it's linked, so a wallet
/// here always stands for one engine wallet and the stores on it. A store
/// the engine can't answer for, or whose keys are a wallet retired here,
/// is left for next time.
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
        let Some(engine_wallet) = tenant.wallet_id.clone() else {
            continue;
        };
        let Ok(tenant_network) = engine_protocol::network::parse_network(&tenant.network) else {
            tracing::warn!(store = %store.id, network = %tenant.network, "a store made before wallets is on a network monokulo doesn't know; left unlinked");
            continue;
        };
        let (user_id, on_it, network, address) = (
            user.id.clone(),
            engine_wallet.clone(),
            tenant.network.clone(),
            tenant.primary_address.clone(),
        );
        let matched = state
            .db
            .read(move |db| {
                Ok::<_, crate::db::DbError>(
                    match db.find_wallet_by_engine_wallet(&user_id, &on_it)? {
                        Some(wallet) => Some(wallet),
                        None => db.find_wallet_by_address(&user_id, &network, &address)?,
                    },
                )
            })
            .await;
        let wallet = match matched {
            Ok(None) => None,
            Ok(Some(wallet)) if wallet.engine_wallet_id == engine_wallet => Some(wallet.id),
            Ok(Some(wallet)) if wallet.retired_at.is_some() => {
                tracing::warn!(store = %store.id, wallet = %wallet.id, "a store made before wallets is on keys retired here; left unlinked");
                continue;
            }
            // The same keys under another engine wallet: the store moves
            // onto this one's, so both are counted on one wallet and take
            // addresses from its one counter.
            Ok(Some(wallet)) => {
                if let Err(e) = state
                    .engine
                    .client
                    .change_wallet(&sk, &wallet.engine_wallet_id)
                    .await
                {
                    tracing::warn!(error = %e, store = %store.id, wallet = %wallet.id, "could not move a store made before wallets onto its wallet");
                    continue;
                }
                Some(wallet.id)
            }
            Err(e) => {
                tracing::warn!(error = %e, store = %store.id, "could not look up a store's wallet");
                continue;
            }
        };
        let user_id = user.id.clone();
        let connection = store.id.clone();
        let adopted = state
            .db
            .write(move |db| {
                let wallet = match wallet {
                    Some(wallet) => wallet,
                    None => {
                        let taken = db.wallet_names(&user_id)?;
                        let id = WalletId::new(format!("w_{}", uuid::Uuid::new_v4().simple()));
                        db.create_wallet(&NewWalletRow {
                            id: &id,
                            user_id: &user_id,
                            name: &crate::wallets::friendly_name(&taken),
                            network: tenant_network,
                            primary_address: &tenant.primary_address,
                            engine_wallet_id: &engine_wallet,
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
