//! Making a store: a real engine tenant on one of the
//! merchant's wallets, and a `store_connections` row pointing at it. Store
//! setup (`http::setup`) calls [`create_connection_for_user`].
//!
//! The engine's `secret_token` (`sk_...`) is never shown to the merchant;
//! monokulo keeps it for its own server-to-server use, encrypted at rest
//! via [`crate::crypto::encrypt`] under
//! `AppState::encryption_key`, with its hash beside it for the API to find
//! the store by (`http::store_key`). `Db` itself never sees a real key or
//! does any crypto: encryption happens here, so `Db` stays a dumb
//! persistence layer.

use uuid::Uuid;

use crate::crypto;
use crate::db::UserRow;
use crate::engine_client::{CreateTenantOnWalletRequest, EngineClientError};
use crate::now_unix;
use crate::stores::Site;

use super::AppState;

/// What store setup decided the store is.
pub(super) struct CreateConnectionFields {
    /// Already tidied (`crate::stores::clean_name`).
    pub name: String,
    /// The store's site, if it has one.
    pub site: Option<Site>,
    pub confirmations_required: Option<u64>,
    pub order_expiry_seconds: Option<i64>,
    pub base_currency: String,
    /// The merchant's wallet the store takes payments into.
    pub wallet_id: crate::db::WalletId,
}

/// The store made.
pub(super) struct CreateConnectionOutcome {
    pub connection_id: crate::db::ConnectionId,
}

/// The ways making a store can fail, for setup's form to show.
pub(super) enum CreateConnectionError {
    BadRequest(String),
    /// Another store on this instance already has the site.
    SiteTaken(SiteTaken),
    Internal,
}

/// Whose store already has a site: no two stores on an instance share one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SiteTaken {
    /// The merchant's own store: the error links to it.
    Yours {
        id: crate::db::ConnectionId,
        name: String,
    },
    /// Another account's, which is all it says.
    Someone,
}

impl SiteTaken {
    /// Who has `site`, from `user_id`'s point of view, if anyone does.
    pub(crate) fn of(
        db: &crate::db::Db,
        user_id: &crate::db::UserId,
        site: &Site,
    ) -> Result<Option<Self>, crate::db::DbError> {
        Ok(db.find_store_by_site(site)?.map(|store| {
            if &store.user_id == user_id {
                SiteTaken::Yours {
                    id: store.id,
                    name: store.name,
                }
            } else {
                SiteTaken::Someone
            }
        }))
    }

    /// What the merchant is told, in plain text (the API's error).
    pub(crate) fn message(&self, site: &Site) -> String {
        match self {
            SiteTaken::Yours { name, .. } => format!(
                "Your store {name} already uses {site}. Open it, or use a different site."
            ),
            SiteTaken::Someone => format!(
                "{site} is already connected to Monokulo here. If it's yours, ask the person who connected it."
            ),
        }
    }
}

/// Makes a store: provisions a real engine tenant on the chosen wallet
/// ([`crate::engine_client::EngineClient::create_tenant_on_wallet`]),
/// encrypts the returned `secret_token`, and stores a `store_connections`
/// row.
pub(super) async fn create_connection_for_user(
    state: &AppState,
    user: &UserRow,
    req: CreateConnectionFields,
) -> Result<CreateConnectionOutcome, CreateConnectionError> {
    // Validated *before* ever provisioning a real engine tenant - a bad
    // base currency should never leave an orphaned tenant behind that this
    // connection attempt then fails to record locally.
    let requested = req.base_currency.clone();
    let base_currency = state
        .db
        .read(move |db| crate::currencies::resolve_currency(db, &requested))
        .await
        .map_err(|_| CreateConnectionError::Internal)?
        .ok_or_else(|| {
            CreateConnectionError::BadRequest(format!(
                "{:?} is not a known currency",
                req.base_currency
            ))
        })?;

    // A site another store has is refused before anything is made.
    if let Some(site) = req.site.clone() {
        let user_id = user.id.clone();
        if let Some(taken) = state
            .db
            .read(move |db| SiteTaken::of(db, &user_id, &site))
            .await
            .map_err(|_| CreateConnectionError::Internal)?
        {
            return Err(CreateConnectionError::SiteTaken(taken));
        }
    }

    // The wallet the store takes payments into.
    let (user_id, wallet_id) = (user.id.clone(), req.wallet_id.clone());
    let wallet = state
        .db
        .read(move |db| db.get_wallet(&user_id, &wallet_id))
        .await
        .map_err(|_| CreateConnectionError::Internal)?
        .filter(|w| w.retired_at.is_none())
        .ok_or_else(|| {
            CreateConnectionError::BadRequest("Choose one of your wallets.".to_owned())
        })?;
    let created = state
        .engine
        .client
        .create_tenant_on_wallet(CreateTenantOnWalletRequest {
            wallet_id: wallet.engine_wallet_id.clone(),
            confirmations_required: req.confirmations_required,
            order_expiry_seconds: req.order_expiry_seconds,
        })
        .await
        .map_err(|e| match e {
            // The engine's own `ApiError::BadRequest` (an unconfigured
            // network, a wallet it no longer has) - a mistake worth showing
            // as it is. Anything else is this service's own problem.
            EngineClientError::EngineError { status, message }
                if status == reqwest::StatusCode::BAD_REQUEST =>
            {
                CreateConnectionError::BadRequest(message)
            }
            _ => CreateConnectionError::Internal,
        })?;

    let id = crate::db::ConnectionId::new(Uuid::new_v4().to_string());
    let encrypted_secret_token = crypto::encrypt(
        &state.encryption_key,
        crypto::Binding::StoreSecret(id.as_str()),
        created.secret_token.expose(),
    );
    let secret_key_hash = created.secret_token.hash();
    let (connection_id, user_id, public_key, wallet_id) = (
        id.clone(),
        user.id.clone(),
        created.public_key.clone(),
        wallet.id.clone(),
    );
    let saved = state
        .db
        .write(move |db| {
            db.create_store_connection_on_wallet(
                &connection_id,
                &user_id,
                &req.name,
                req.site.as_ref(),
                &public_key,
                &encrypted_secret_token,
                now_unix(),
                &base_currency,
                &wallet_id,
            )?;
            db.set_secret_key_hash(&connection_id, &secret_key_hash)?;
            // The site's domain joins the store's domains waiting for DNS
            // (`crate::embed_domains`).
            crate::embed_domains::suggest_site_domain(
                db,
                &connection_id,
                req.site.as_ref(),
                now_unix(),
            );
            let _ = db.mark_store_domains_imported(&connection_id);
            Ok::<_, crate::db::DbError>(())
        })
        .await;
    if let Err(e) = saved {
        let taken = e.is_unique_violation();
        // The tenant exists but nothing owns it: its secret would be lost
        // with this request, and the engine would scan for it forever. Give
        // it back; if that fails too, name it for an operator to clean up.
        tracing::error!(error = %e, public_key = %created.public_key, "could not save the store connection; removing its engine tenant");
        if let Err(e) = state
            .engine
            .client
            .delete_tenant(&created.secret_token)
            .await
        {
            tracing::error!(error = %e, public_key = %created.public_key, "the engine tenant could not be removed either; it is orphaned");
        }
        // Another store took the site while this one was being made.
        if taken {
            return Err(CreateConnectionError::SiteTaken(SiteTaken::Someone));
        }
        return Err(CreateConnectionError::Internal);
    }

    Ok(CreateConnectionOutcome { connection_id: id })
}
