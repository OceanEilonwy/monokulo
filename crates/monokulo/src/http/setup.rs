//! Setting up a store (`/setup`): Store, then Wallet, then Done.
//!
//! Sign-up lands here, the dashboard's "add a store" comes here, and so
//! does a WooCommerce plugin connecting a shop that has no store yet
//! (`http::connect`). The store step's answers ([`StoreDraft`]) ride along
//! as hidden fields and query parameters through the wallet step's
//! screens, and are checked again at each one: nothing is made until the
//! wallet is added, and then the store is made on it in the same request.
//! Base currency (XMR), exchange rate provider and confirmations get their
//! defaults; they're changed in the store's settings. Every screen works
//! without JavaScript except making a new wallet.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Form;
use serde::Deserialize;

use crate::db::{ConnectionId, UserRow, WalletId, WalletRow};
use crate::stores::StoreKind;
use crate::views;
use crate::views::setup::{SetupContext, SiteError, StoreStepViewModel};
use crate::views::wallets::Flow;

use super::connections::{
    create_connection_for_user, CreateConnectionError, CreateConnectionFields, SiteTaken,
};
use super::dashboard::redirect_303;
use super::wallets::{CreateForm, ImportForm, NameQuery};
use super::{AppState, AuthedUser};

/// The store step's answers, and the plugin's request when a plugin sent
/// the merchant here: what every setup screen carries.
#[derive(Deserialize, Default, Clone, Debug)]
pub struct StoreDraft {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub store_name: String,
    #[serde(default)]
    pub store_site: String,
    /// The plugin's platform, its shop's address, where to send its key,
    /// and its nonce (`/connect/{platform}`).
    #[serde(default)]
    pub plugin: String,
    #[serde(default)]
    pub site_url: String,
    #[serde(default)]
    pub return_url: String,
    #[serde(default)]
    pub nonce: String,
    /// Not on any form: a plugin's test harness can set the store's
    /// confirmations and order expiry this way, as it could when the
    /// plugin's connect form took them.
    #[serde(default)]
    pub confirmations_required: String,
    #[serde(default)]
    pub order_expiry_seconds: String,
}

impl StoreDraft {
    /// The fields the plugin's request and the harness's settings travel
    /// in, when set.
    fn carried(&self) -> Vec<(&'static str, String)> {
        [
            ("plugin", &self.plugin),
            ("site_url", &self.site_url),
            ("return_url", &self.return_url),
            ("nonce", &self.nonce),
            ("confirmations_required", &self.confirmations_required),
            ("order_expiry_seconds", &self.order_expiry_seconds),
        ]
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(name, value)| (name, value.clone()))
        .collect()
    }

    fn has_plugin(&self) -> bool {
        !self.plugin.is_empty()
    }
}

/// A store step whose answers hold: what the store will be.
pub(super) struct ValidStore {
    kind: StoreKind,
    name: String,
    site: String,
    plugin: Option<views::setup::PluginReturn>,
    confirmations_required: Option<u64>,
    order_expiry_seconds: Option<i64>,
    context: SetupContext,
}

impl ValidStore {
    fn flow(&self) -> Flow<'_> {
        Flow::Setup(&self.context)
    }
}

/// The store step, shown with `draft`'s answers and what's wrong with them.
async fn store_page(
    state: &AppState,
    user: &UserRow,
    draft: &StoreDraft,
    name_error: Option<String>,
    site_error: Option<SiteError>,
) -> Response {
    let plugin_host = draft
        .has_plugin()
        .then(|| crate::stores::normalize_site(&draft.site_url).unwrap_or_default());
    let data = StoreStepViewModel {
        kind: StoreKind::parse(&draft.kind).unwrap_or(StoreKind::Website),
        name: draft.store_name.clone(),
        site: plugin_host
            .clone()
            .unwrap_or_else(|| draft.store_site.clone()),
        plugin_host,
        carried: draft.carried(),
        name_error,
        site_error,
    };
    let chrome = super::page_chrome(state, Some(user), "/setup").await;
    views::setup::store_page(&chrome, &data).into_response()
}

/// Checks the store step's answers: a kind, a name, a site no other store
/// has (none for a store that's in person only), and a plugin request that
/// can be answered. `Err` is the page to show instead: the store step with
/// what's wrong, or why the plugin can't connect.
pub(super) async fn check(
    state: &AppState,
    user: &UserRow,
    draft: &StoreDraft,
) -> Result<ValidStore, Box<Response>> {
    let plugin = if draft.has_plugin() {
        if let Some(why) =
            super::connect::plugin_problem(state, &draft.site_url, &draft.return_url).await
        {
            let chrome = super::page_chrome(state, Some(user), "/setup").await;
            let host = crate::stores::normalize_site(&draft.site_url).ok();
            return Err(Box::new(
                views::connect::cannot_connect_page(&chrome, host.as_deref(), &why).into_response(),
            ));
        }
        Some(views::setup::PluginReturn {
            platform: draft.plugin.clone(),
            site_url: draft.site_url.clone(),
            return_url: draft.return_url.clone(),
            nonce: draft.nonce.clone(),
        })
    } else {
        None
    };
    let kind = if plugin.is_some() {
        StoreKind::WooCommerce
    } else {
        StoreKind::parse(&draft.kind).unwrap_or(StoreKind::Website)
    };
    let name = crate::stores::clean_name(&draft.store_name);
    let site_input = if plugin.is_some() {
        draft.site_url.as_str()
    } else {
        draft.store_site.as_str()
    };
    let site = if kind.has_site() {
        crate::stores::normalize_site(site_input).map_err(|why| SiteError::Invalid(why.to_owned()))
    } else {
        Ok(String::new())
    };
    let taken = match &site {
        Ok(site) if !site.is_empty() => {
            let (user_id, site) = (user.id.clone(), site.clone());
            state
                .db
                .read(move |db| SiteTaken::of(db, &user_id, &site))
                .await
                .ok()
                .flatten()
        }
        _ => None,
    };
    let site = match (site, taken) {
        (Ok(site), None) => Ok(site),
        (Ok(site), Some(taken)) => Err(site_error(taken, site)),
        (Err(e), _) => Err(e),
    };
    match (name, site) {
        (Ok(name), Ok(site)) => {
            let mut fields = vec![
                ("kind", kind.key().to_owned()),
                ("store_name", name.clone()),
                ("store_site", site.clone()),
            ];
            fields.extend(draft.carried());
            Ok(ValidStore {
                kind,
                context: SetupContext {
                    store_name: name.clone(),
                    fields,
                },
                name,
                site,
                plugin,
                confirmations_required: draft.confirmations_required.parse().ok(),
                order_expiry_seconds: draft.order_expiry_seconds.parse().ok(),
            })
        }
        (name, site) => Err(Box::new(
            store_page(
                state,
                user,
                draft,
                name.err().map(str::to_owned),
                site.err(),
            )
            .await,
        )),
    }
}

fn site_error(taken: SiteTaken, site: String) -> SiteError {
    match taken {
        SiteTaken::Yours { id, name } => SiteError::Yours {
            id: id.to_string(),
            name,
            site,
        },
        SiteTaken::Someone => SiteError::Someone { site },
    }
}

/// `GET /setup`: the store step.
pub async fn store_form(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Query(draft): Query<StoreDraft>,
) -> Response {
    store_page(&state, &user, &draft, None, None).await
}

/// `POST /setup`: the store step's answers, on to the wallet step.
pub async fn store_submit(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Form(draft): Form<StoreDraft>,
) -> Response {
    match check(&state, &user, &draft).await {
        Ok(store) => redirect_303(&format!("/setup/wallet?{}", store.context.query())),
        Err(page) => *page,
    }
}

#[derive(Deserialize)]
pub struct WithName {
    #[serde(flatten)]
    draft: StoreDraft,
    #[serde(flatten)]
    name: NameQuery,
}

/// `GET /setup/wallet`: where the store's money goes.
pub async fn wallet_form(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Query(query): Query<WithName>,
) -> Response {
    let store = match check(&state, &user, &query.draft).await {
        Ok(store) => store,
        Err(page) => return *page,
    };
    let (name, problem) = match query.name.name {
        Some(name) => {
            let problem = super::wallets::name_problem(&state, &user, &name).await;
            (name, problem)
        }
        None => {
            let wanted = format!("{} takings", store.name);
            (
                super::wallets::suggested_name(&state, &user, Some(&wanted)).await,
                None,
            )
        }
    };
    super::wallets::render_choice(
        &state,
        &user,
        store.flow(),
        name,
        problem,
        super::wallets::network_or_mainnet(query.name.network.as_deref()),
        None,
    )
    .await
}

/// Makes the store on `wallet`, then Done; or the store step again when it
/// can't be made (another store took its site meanwhile).
async fn make_store(
    state: &AppState,
    user: &UserRow,
    store: &ValidStore,
    draft: &StoreDraft,
    wallet: &WalletRow,
    skipped_backup: bool,
) -> Response {
    let fields = CreateConnectionFields {
        platform: store
            .plugin
            .as_ref()
            .map(|p| p.platform.clone())
            .unwrap_or_else(|| store.kind.platform().to_owned()),
        name: store.name.clone(),
        site: store.site.clone(),
        view_key_hex: String::new(),
        spend_pubkey_hex: String::new(),
        encrypted_keys: None,
        network: None,
        domains: Vec::new(),
        confirmations_required: store.confirmations_required,
        order_expiry_seconds: store.order_expiry_seconds,
        base_currency: "XMR".to_owned(),
        key_custody_backend: None,
        wallet_id: Some(wallet.id.clone()),
    };
    match create_connection_for_user(state, user, fields).await {
        Ok(outcome) => {
            let mut query = url::form_urlencoded::Serializer::new(String::new());
            if let Some(plugin) = &store.plugin {
                query
                    .append_pair("plugin", &plugin.platform)
                    .append_pair("site_url", &plugin.site_url)
                    .append_pair("return_url", &plugin.return_url)
                    .append_pair("nonce", &plugin.nonce);
            }
            if skipped_backup {
                query.append_pair("skipped", "1");
            }
            let query = query.finish();
            let path = format!("/setup/done/{}", outcome.connection_id);
            redirect_303(&if query.is_empty() {
                path
            } else {
                format!("{path}?{query}")
            })
        }
        Err(CreateConnectionError::SiteTaken(taken)) => {
            store_page(
                state,
                user,
                draft,
                None,
                Some(site_error(taken, store.site.clone())),
            )
            .await
        }
        Err(CreateConnectionError::BadRequest(message)) => {
            super::wallets::render_choice(
                state,
                user,
                store.flow(),
                String::new(),
                None,
                wallet.network.clone(),
                Some(message),
            )
            .await
        }
        Err(CreateConnectionError::Internal) => {
            super::wallets::render_choice(
                state,
                user,
                store.flow(),
                String::new(),
                None,
                wallet.network.clone(),
                Some("The store couldn't be made right now. Try again in a minute.".to_owned()),
            )
            .await
        }
    }
}

#[derive(Deserialize)]
pub struct ExistingForm {
    #[serde(flatten)]
    draft: StoreDraft,
    #[serde(default)]
    wallet_id: String,
}

/// `POST /setup/wallet/existing`: the store takes payments into a wallet
/// already added.
pub async fn use_existing(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Form(form): Form<ExistingForm>,
) -> Response {
    let store = match check(&state, &user, &form.draft).await {
        Ok(store) => store,
        Err(page) => return *page,
    };
    let (user_id, wallet_id) = (user.id.clone(), WalletId::new(form.wallet_id));
    let wallet = state
        .db
        .read(move |db| db.get_wallet(&user_id, &wallet_id))
        .await
        .ok()
        .flatten()
        .filter(|w| w.retired_at.is_none());
    match wallet {
        Some(wallet) => make_store(&state, &user, &store, &form.draft, &wallet, false).await,
        None => {
            super::wallets::render_choice(
                &state,
                &user,
                store.flow(),
                String::new(),
                None,
                "mainnet".to_owned(),
                Some("Choose one of your wallets.".to_owned()),
            )
            .await
        }
    }
}

/// `GET /setup/wallet/keys`: bring your own wallet.
pub async fn keys_form(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Query(query): Query<WithName>,
) -> Response {
    match check(&state, &user, &query.draft).await {
        Ok(store) => super::wallets::keys_screen(&state, &user, store.flow(), query.name).await,
        Err(page) => *page,
    }
}

#[derive(Deserialize)]
pub struct KeysForm {
    #[serde(flatten)]
    draft: StoreDraft,
    #[serde(flatten)]
    wallet: ImportForm,
}

/// `POST /setup/wallet/keys`: the wallet's keys; the wallet is added and
/// the store made on it.
pub async fn keys_submit(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Form(form): Form<KeysForm>,
) -> Response {
    let store = match check(&state, &user, &form.draft).await {
        Ok(store) => store,
        Err(page) => return *page,
    };
    match super::wallets::add_imported(&state, &user, store.flow(), &form.wallet).await {
        Ok(wallet) => make_store(&state, &user, &store, &form.draft, &wallet, false).await,
        Err(page) => *page,
    }
}

/// `GET /setup/wallet/new`: make a new wallet in the browser.
pub async fn new_form(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Query(query): Query<WithName>,
) -> Response {
    match check(&state, &user, &query.draft).await {
        Ok(store) => super::wallets::create_screen(&state, &user, store.flow(), query.name).await,
        Err(page) => *page,
    }
}

#[derive(Deserialize)]
pub struct NewForm {
    #[serde(flatten)]
    draft: StoreDraft,
    #[serde(flatten)]
    wallet: CreateForm,
}

/// `POST /setup/wallet/new`: the new wallet's watch-only keys, once its
/// backup was checked or skipped; the wallet is added and the store made
/// on it.
pub async fn new_submit(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Form(form): Form<NewForm>,
) -> Response {
    let store = match check(&state, &user, &form.draft).await {
        Ok(store) => store,
        Err(page) => return *page,
    };
    match super::wallets::add_created(&state, &user, store.flow(), &form.wallet).await {
        Ok(wallet) => {
            let skipped = form.wallet.backup == "skipped";
            make_store(&state, &user, &store, &form.draft, &wallet, skipped).await
        }
        Err(page) => *page,
    }
}

#[derive(Deserialize)]
pub struct DoneQuery {
    #[serde(default)]
    plugin: String,
    #[serde(default)]
    site_url: String,
    #[serde(default)]
    return_url: String,
    #[serde(default)]
    nonce: String,
    #[serde(default)]
    skipped: Option<String>,
}

/// `GET /setup/done/{id}`: what's left before the store takes payments.
pub async fn done(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
    Query(query): Query<DoneQuery>,
) -> Response {
    let row = match super::orders::load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row.into_row(),
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let wallet = match row.wallet_id.clone() {
        Some(wallet_id) => {
            let user_id = user.id.clone();
            state
                .db
                .read(move |db| db.get_wallet(&user_id, &wallet_id))
                .await
                .ok()
                .flatten()
        }
        None => None,
    };
    let plugin = (!query.plugin.is_empty() && !query.return_url.is_empty()).then(|| {
        views::setup::PluginReturn {
            platform: query.plugin.clone(),
            site_url: query.site_url.clone(),
            return_url: query.return_url.clone(),
            nonce: query.nonce.clone(),
        }
    });
    let data = views::setup::DoneViewModel {
        store_id: row.id.to_string(),
        kind: StoreKind::of_platform(&row.platform),
        site: row.site.clone(),
        public_key: row.tenant_public_key.clone(),
        base_currency: row.base_currency.clone(),
        wallet_name: wallet.as_ref().map(|w| w.name.clone()).unwrap_or_default(),
        wallet_network: wallet
            .as_ref()
            .map(|w| w.network.clone())
            .unwrap_or_default(),
        skipped_backup: query.skipped.is_some()
            && wallet.as_ref().and_then(|w| w.backup.as_deref()) == Some("skipped"),
        plugin,
        name: row.name,
    };
    let chrome = super::page_chrome(&state, Some(&user), format!("/setup/done/{id}")).await;
    views::setup::done_page(&chrome, &data).into_response()
}
