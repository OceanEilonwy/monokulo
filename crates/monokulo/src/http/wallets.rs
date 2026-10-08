//! A merchant's wallets (docs/wallets.md): `/dashboard/wallets` and its
//! pages. Choosing how to set one up, bringing one's own (keys pasted in,
//! works without JavaScript), making a new one in the browser (the phrase
//! never leaves the page; only watch-only keys are posted), the list, and a
//! wallet's page where it is renamed or deleted.

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Form;
use maud::html;
use serde::Deserialize;

use crate::db::{DeleteWallet, UserRow, WalletId, WalletOrigin, WalletRow};
use crate::views;
use crate::views::wallets::{
    CreateViewModel, DetailViewModel, ImportViewModel, ReadyViewModel, WalletEvent, WalletListItem,
    WalletStore,
};
use crate::wallets::{clean_name, friendly_name};

use super::dashboard::{redirect_303, SafePath};
use super::wallet_service::{add_wallet, adopt_unlinked_stores, AddWallet, AddWalletError};
use super::{AppState, AuthedUser};

#[derive(Deserialize, Default)]
pub struct SetupQuery {
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    suggested: Option<String>,
    #[serde(default)]
    network: Option<String>,
}

/// `?next=`, kept only if it's a path on this site.
fn safe_next(next: Option<&str>) -> Option<String> {
    next.and_then(SafePath::parse)
        .map(|p| p.as_str().to_owned())
}

fn network_or_mainnet(network: Option<&str>) -> String {
    match network {
        Some(n @ ("stagenet" | "testnet")) => n.to_owned(),
        _ => "mainnet".to_owned(),
    }
}

/// The plugin's site, when `next` is the WooCommerce connect page (the
/// merchant is setting up a wallet on the way to connecting a shop).
pub(crate) fn connecting_site(next: Option<&str>) -> Option<String> {
    let next = next?;
    let query = next.strip_prefix("/connect/")?.split_once('?')?.1;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == "site_url")
        .map(|(_, v)| super::orders::display_name_for(&v))
}

async fn wallet_names(state: &AppState, user: &UserRow) -> Vec<String> {
    let user_id = user.id.clone();
    state
        .db
        .read(move |db| db.list_wallets(&user_id))
        .await
        .map(|wallets| wallets.into_iter().map(|w| w.wallet.name).collect())
        .unwrap_or_default()
}

/// The name a wallet will get: the one typed, else the one the form
/// already suggested if it's still free, else a fresh friendly one.
fn chosen_name(typed: Option<&str>, suggested: Option<&str>, taken: &[String]) -> String {
    if let Ok(Some(name)) = clean_name(typed.unwrap_or("")) {
        return name;
    }
    match suggested.and_then(|s| clean_name(s).ok().flatten()) {
        Some(s) if !taken.iter().any(|t| t.eq_ignore_ascii_case(&s)) => s,
        _ => friendly_name(taken),
    }
}

/// A first wallet: the setup steps (Account, Wallet, Ready) are shown.
async fn is_onboarding(state: &AppState, user: &UserRow) -> bool {
    let user_id = user.id.clone();
    state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>(
                db.list_wallets(&user_id)?.is_empty()
                    && db.list_store_connections_for_user(&user_id)?.is_empty(),
            )
        })
        .await
        .unwrap_or(false)
}

/// `GET /dashboard/wallets/setup`.
pub async fn setup(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Query(query): Query<SetupQuery>,
) -> Response {
    let next = safe_next(query.next.as_deref());
    let taken = wallet_names(&state, &user).await;
    let data = views::wallets::ChoiceViewModel {
        onboarding: is_onboarding(&state, &user).await,
        connecting_site: connecting_site(next.as_deref()),
        suggested_name: chosen_name(None, query.suggested.as_deref(), &taken),
        name: query.name.unwrap_or_default(),
        network: network_or_mainnet(query.network.as_deref()),
        next,
    };
    let chrome = super::page_chrome(&state, Some(&user), "/dashboard/wallets/setup").await;
    views::wallets::choice_page(&chrome, &data).into_response()
}

// -- Bring your own wallet ---------------------------------------------------

#[derive(Deserialize)]
pub struct ImportForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    suggested: Option<String>,
    #[serde(default)]
    view_key_hex: String,
    #[serde(default)]
    spend_pubkey_hex: String,
    #[serde(default)]
    encrypted_keys: Option<String>,
    #[serde(default)]
    network: String,
    #[serde(default)]
    key_custody_backend: Option<String>,
    #[serde(default)]
    next: Option<String>,
}

async fn render_import(
    state: &AppState,
    user: &UserRow,
    error: Option<String>,
    form: Option<&ImportForm>,
    query: &SetupQuery,
) -> Response {
    let taken = wallet_names(state, user).await;
    let custody_choices = super::status_page::custody_choice_views(
        state,
        form.and_then(|f| f.key_custody_backend.as_deref()),
    );
    let snp_entry = super::key_entry::prepare(
        state,
        &user.id,
        super::key_entry::Purpose::Create,
        &super::key_entry::offered_backends(state, &custody_choices),
    )
    .await;
    let data = ImportViewModel {
        onboarding: is_onboarding(state, user).await,
        error,
        name: form
            .map(|f| f.name.clone())
            .or_else(|| query.name.clone())
            .unwrap_or_default(),
        suggested_name: chosen_name(
            None,
            form.and_then(|f| f.suggested.as_deref())
                .or(query.suggested.as_deref()),
            &taken,
        ),
        spend_pubkey_hex: form.map(|f| f.spend_pubkey_hex.clone()).unwrap_or_default(),
        network: network_or_mainnet(
            form.map(|f| f.network.as_str())
                .or(query.network.as_deref()),
        ),
        next: safe_next(
            form.and_then(|f| f.next.as_deref())
                .or(query.next.as_deref()),
        ),
        custody_choices,
        snp_entry,
    };
    let chrome = super::page_chrome(state, Some(user), "/dashboard/wallets/import").await;
    views::wallets::import_page(&chrome, &data).into_response()
}

/// `GET /dashboard/wallets/import`.
pub async fn import_form(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Query(query): Query<SetupQuery>,
) -> Response {
    render_import(&state, &user, None, None, &query).await
}

/// `POST /dashboard/wallets/import`.
pub async fn import_submit(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Form(form): Form<ImportForm>,
) -> Response {
    let taken = wallet_names(&state, &user).await;
    let name = chosen_name(Some(&form.name), form.suggested.as_deref(), &taken);
    let added = add_wallet(
        &state,
        &user,
        AddWallet {
            name,
            view_key_hex: form.view_key_hex.clone(),
            spend_pubkey_hex: form.spend_pubkey_hex.clone(),
            encrypted_keys: form.encrypted_keys.clone(),
            network: network_or_mainnet(Some(&form.network)),
            key_custody_backend: form.key_custody_backend.clone(),
            origin: WalletOrigin::Imported,
            backup: None,
            expected_address: None,
        },
    )
    .await;
    let next = safe_next(form.next.as_deref());
    match added {
        Ok(wallet) => redirect_303(&ready_path(&wallet, next.as_deref(), false)),
        Err(AddWalletError::AlreadyAdded(existing)) => {
            let message = format!(
                "You've already added this wallet, as \u{201c}{}\u{201d}.",
                existing.name
            );
            render_import(
                &state,
                &user,
                Some(message),
                Some(&form),
                &SetupQuery::default(),
            )
            .await
        }
        Err(AddWalletError::Invalid(message)) => {
            render_import(
                &state,
                &user,
                Some(message),
                Some(&form),
                &SetupQuery::default(),
            )
            .await
        }
        Err(AddWalletError::Internal) => {
            render_import(
                &state,
                &user,
                Some("The wallet couldn't be added right now. Try again in a minute.".to_owned()),
                Some(&form),
                &SetupQuery::default(),
            )
            .await
        }
    }
}

fn ready_path(wallet: &WalletRow, next: Option<&str>, skipped: bool) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    if let Some(next) = next {
        query.append_pair("next", next);
    }
    if skipped {
        query.append_pair("skipped", "1");
    }
    let query = query.finish();
    if query.is_empty() {
        format!("/dashboard/wallets/{}/ready", wallet.id)
    } else {
        format!("/dashboard/wallets/{}/ready?{query}", wallet.id)
    }
}

// -- Create a new wallet -----------------------------------------------------

/// The chain's height now on `network`, from the engine's cached status:
/// a 25-word restore starts there (the wallet is new, so nothing before
/// can be its).
async fn current_height(state: &AppState, network: &str) -> Option<u64> {
    let status = tokio::time::timeout(
        std::time::Duration::from_millis(1500),
        super::status_page::get_status_cached(&state.engine),
    )
    .await
    .ok()?
    .ok()?;
    status
        .networks
        .iter()
        .find(|n| n.network == network)?
        .nodes
        .iter()
        .filter_map(|node| node.height)
        .max()
}

/// The encrypted key entry for a wallet made in the page: only when the
/// keys must go to SEV-SNP key storage (the engine's default backend), as
/// the page has no backend choice to offer.
async fn snp_for_new_wallet(
    state: &AppState,
    user: &UserRow,
) -> Option<views::key_entry::SnpKeyEntry> {
    let backends = super::status_page::known_enabled_custody_backends(&state.engine);
    if backends.first().map(String::as_str) != Some("snp") {
        return None;
    }
    super::key_entry::prepare(
        state,
        &user.id,
        super::key_entry::Purpose::Create,
        &["snp".to_owned()],
    )
    .await
}

async fn render_create(
    state: &AppState,
    user: &UserRow,
    name: String,
    network: String,
    next: Option<String>,
    error: Option<String>,
) -> Response {
    let data = CreateViewModel {
        onboarding: is_onboarding(state, user).await,
        restore_height: current_height(state, &network).await,
        snp_entry: snp_for_new_wallet(state, user).await,
        name,
        network,
        next,
        error,
    };
    let chrome = super::page_chrome(state, Some(user), "/dashboard/wallets/new").await;
    (
        // Never kept: a reload makes a different wallet.
        [(header::CACHE_CONTROL, "no-store")],
        views::wallets::create_page(&chrome, &data),
    )
        .into_response()
}

/// `GET /dashboard/wallets/new`.
pub async fn create_form(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Query(query): Query<SetupQuery>,
) -> Response {
    let taken = wallet_names(&state, &user).await;
    let name = chosen_name(query.name.as_deref(), query.suggested.as_deref(), &taken);
    render_create(
        &state,
        &user,
        name,
        network_or_mainnet(query.network.as_deref()),
        safe_next(query.next.as_deref()),
        None,
    )
    .await
}

#[derive(Deserialize)]
pub struct CreateForm {
    name: String,
    network: String,
    backup: String,
    primary_address: String,
    #[serde(default)]
    view_key_hex: String,
    #[serde(default)]
    spend_pubkey_hex: String,
    #[serde(default)]
    encrypted_keys: Option<String>,
    #[serde(default)]
    next: Option<String>,
}

/// `POST /dashboard/wallets/new`: the watch-only keys of the wallet the page
/// made, and how its phrase was backed up. The page's address is checked
/// against the one the engine works out from the keys.
pub async fn create_submit(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Form(form): Form<CreateForm>,
) -> Response {
    let next = safe_next(form.next.as_deref());
    let skipped = form.backup == "skipped";
    let added = add_wallet(
        &state,
        &user,
        AddWallet {
            name: form.name.clone(),
            view_key_hex: form.view_key_hex,
            spend_pubkey_hex: form.spend_pubkey_hex,
            encrypted_keys: form.encrypted_keys,
            network: network_or_mainnet(Some(&form.network)),
            key_custody_backend: None,
            origin: WalletOrigin::Created,
            backup: Some(form.backup),
            expected_address: Some(form.primary_address),
        },
    )
    .await;
    match added {
        Ok(wallet) => redirect_303(&ready_path(&wallet, next.as_deref(), skipped)),
        Err(error) => {
            let message = match error {
                AddWalletError::Invalid(message) => message,
                AddWalletError::AlreadyAdded(existing) => format!(
                    "You've already added this wallet, as \u{201c}{}\u{201d}.",
                    existing.name
                ),
                AddWalletError::Internal => "The wallet couldn't be added right now.".to_owned(),
            };
            // The phrase backed up on that page is not registered anywhere,
            // and this page makes a different one: say so plainly.
            let message = format!(
                "{message} Nothing was saved, so the recovery phrase you just backed up isn't connected to Monokulo: below is a new wallet to back up instead."
            );
            render_create(
                &state,
                &user,
                form.name,
                network_or_mainnet(Some(&form.network)),
                next,
                Some(message),
            )
            .await
        }
    }
}

// -- Added -------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ReadyQuery {
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    skipped: Option<String>,
}

async fn load_wallet(state: &AppState, user: &UserRow, id: &str) -> Option<WalletRow> {
    let (user_id, id) = (user.id.clone(), WalletId::new(id));
    state
        .db
        .read(move |db| db.get_wallet(&user_id, &id))
        .await
        .ok()
        .flatten()
}

/// `GET /dashboard/wallets/{id}/ready`.
pub async fn ready(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Query(query): Query<ReadyQuery>,
) -> Response {
    let Some(wallet) = load_wallet(&state, &user, &id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let user_id = user.id.clone();
    let (wallets, stores) = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                db.list_wallets(&user_id)?.len(),
                db.list_store_connections_for_user(&user_id)?.len(),
            ))
        })
        .await
        .unwrap_or((0, 0));
    let next = safe_next(query.next.as_deref()).map(|path| {
        let label = match connecting_site(Some(&path)) {
            Some(site) => format!("Continue connecting {site}"),
            None if path.starts_with("/dashboard/connect") => {
                "Back to adding your store".to_owned()
            }
            None => "Continue".to_owned(),
        };
        (path, label)
    });
    let data = ReadyViewModel {
        onboarding: wallets == 1 && stores == 0,
        skipped_backup: query.skipped.is_some() && wallet.backup.as_deref() == Some("skipped"),
        wallet,
        next,
    };
    let chrome = super::page_chrome(&state, Some(&user), "/dashboard/wallets").await;
    views::wallets::ready_page(&chrome, &data).into_response()
}

// -- The list and a wallet's page --------------------------------------------

/// `GET /dashboard/wallets`.
pub async fn index(State(state): State<AppState>, AuthedUser(user, _): AuthedUser) -> Response {
    adopt_unlinked_stores(&state, &user).await;
    let user_id = user.id.clone();
    let wallets = match state.db.read(move |db| db.list_wallets(&user_id)).await {
        Ok(wallets) => wallets,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let items: Vec<WalletListItem> = wallets
        .into_iter()
        .map(|w| WalletListItem {
            id: w.wallet.id.to_string(),
            kind: match w.wallet.origin {
                WalletOrigin::Created => "Made in Monokulo",
                WalletOrigin::Imported => "Brought in",
            },
            address: views::wallets::short_address(&w.wallet.primary_address),
            network: w.wallet.network,
            name: w.wallet.name,
            stores: w.store_count,
        })
        .collect();
    let chrome = super::page_chrome(&state, Some(&user), "/dashboard/wallets").await;
    views::wallets::list_page(&chrome, &items).into_response()
}

#[derive(Deserialize, Default)]
pub struct DetailQuery {
    #[serde(default)]
    renamed: Option<String>,
}

async fn render_detail(
    state: &AppState,
    user: &UserRow,
    wallet: WalletRow,
    error: Option<String>,
    notice: Option<String>,
    name_field: Option<String>,
) -> Response {
    let chrome = super::page_chrome(state, Some(user), "/dashboard/wallets").await;
    let (user_id, wallet_id) = (user.id.clone(), wallet.id.clone());
    let (stores, events) = state
        .db
        .read(move |db| {
            let stores: Vec<_> = db
                .list_store_connections_for_user(&user_id)?
                .into_iter()
                .filter(|s| s.wallet_id.as_ref() == Some(&wallet_id))
                .collect();
            Ok::<_, crate::db::DbError>((stores, db.wallet_events(&wallet_id)?))
        })
        .await
        .unwrap_or_default();
    let store_name = |id: &str| {
        stores.iter().find(|s| s.id == id).map(|s| {
            (
                s.id.to_string(),
                super::orders::display_name_for(&s.site_url),
            )
        })
    };

    let mut history: Vec<(i64, WalletEvent)> = events
        .iter()
        .map(|event| {
            let what = match event.kind.as_str() {
                "created" => html! {
                    "Made in this browser"
                    @if !event.detail.is_empty() { ", " (crate::wallets::backup_label(&event.detail).to_lowercase()) }
                },
                "imported" => html! { "Brought in" },
                "renamed" => html! { "Renamed from \u{201c}" (event.detail) "\u{201d}" },
                "store_connected" => match store_name(&event.detail) {
                    Some((id, name)) => html! { "Store " a href=(format!("/dashboard/stores/{id}")) { (name) } " connected" },
                    None => html! { "A store connected" },
                },
                other => html! { (other) },
            };
            (event.at, WalletEvent { when: chrome.clock.text(event.at), what })
        })
        .collect();
    // Payments into the wallet: its stores' orders that have received
    // something, newest first.
    for store in &stores {
        let Ok(sk) = super::orders::decrypt_sk(&state.encryption_key, store) else {
            continue;
        };
        let Ok(orders) = state.engine.client.list_orders(&sk).await else {
            continue;
        };
        let name = super::orders::display_name_for(&store.site_url);
        for order in orders
            .into_iter()
            .filter(|o| o.amount_received_piconero > 0)
        {
            let link = format!("/dashboard/stores/{}/orders/{}", store.id, order.order_id);
            history.push((
                order.updated_at,
                WalletEvent {
                    when: chrome.clock.text(order.updated_at),
                    what: html! {
                        "Payment for order " a href=(link) { code { (order.order_id) } } " on " (name) " "
                        (views::state_badge(order.status))
                    },
                },
            ));
        }
    }
    history.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    history.truncate(30);

    let data = DetailViewModel {
        name_field: name_field.unwrap_or_else(|| wallet.name.clone()),
        stores: stores
            .iter()
            .map(|s| WalletStore {
                id: s.id.to_string(),
                name: super::orders::display_name_for(&s.site_url),
            })
            .collect(),
        history: history.into_iter().map(|(_, e)| e).collect(),
        wallet,
        error,
        notice,
    };
    views::wallets::detail_page(&chrome, &data).into_response()
}

/// `GET /dashboard/wallets/{id}`.
pub async fn detail(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Query(query): Query<DetailQuery>,
) -> Response {
    let Some(wallet) = load_wallet(&state, &user, &id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let notice = query.renamed.map(|_| "Renamed.".to_owned());
    render_detail(&state, &user, wallet, None, notice, None).await
}

#[derive(Deserialize)]
pub struct RenameForm {
    #[serde(default)]
    name: String,
}

/// `POST /dashboard/wallets/{id}/rename`.
pub async fn rename(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(form): Form<RenameForm>,
) -> Response {
    let Some(wallet) = load_wallet(&state, &user, &id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let name = match clean_name(&form.name) {
        Ok(Some(name)) => name,
        Ok(None) => {
            return render_detail(
                &state,
                &user,
                wallet,
                Some("A wallet needs a name.".to_owned()),
                None,
                Some(form.name),
            )
            .await
        }
        Err(message) => {
            return render_detail(&state, &user, wallet, Some(message), None, Some(form.name)).await
        }
    };
    let (user_id, wallet_id, new_name) = (user.id.clone(), wallet.id.clone(), name.clone());
    match state
        .db
        .write(move |db| db.rename_wallet(&user_id, &wallet_id, &new_name, crate::now_unix()))
        .await
    {
        Ok(_) => redirect_303(&format!("/dashboard/wallets/{}?renamed=1", wallet.id)),
        Err(e) if e.is_unique_violation() => {
            render_detail(
                &state,
                &user,
                wallet,
                Some(format!(
                    "You already have a wallet called \u{201c}{name}\u{201d}."
                )),
                None,
                Some(form.name),
            )
            .await
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
pub struct DeleteForm {
    #[serde(default)]
    confirm: String,
}

/// `POST /dashboard/wallets/{id}/delete`: refused while a store uses the
/// wallet, and unless its name was typed to confirm.
pub async fn delete(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(form): Form<DeleteForm>,
) -> Response {
    let Some(wallet) = load_wallet(&state, &user, &id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if form.confirm.trim() != wallet.name {
        let message = format!("Type \u{201c}{}\u{201d} exactly to delete it.", wallet.name);
        return render_detail(&state, &user, wallet, Some(message), None, None).await;
    }
    // The engine first: it refuses while a store of any account uses the
    // keys, and a wallet it forgot can't be used again by mistake.
    match state
        .engine
        .client
        .delete_wallet(&wallet.engine_wallet_id)
        .await
    {
        Ok(()) => {}
        Err(crate::engine_client::EngineClientError::EngineError { status, .. })
            if status == StatusCode::NOT_FOUND => {}
        Err(crate::engine_client::EngineClientError::EngineError { status, message })
            if status == StatusCode::CONFLICT =>
        {
            return render_detail(
                &state,
                &user,
                wallet,
                Some(format!("It's still in use: {message}.")),
                None,
                None,
            )
            .await;
        }
        Err(e) => {
            tracing::error!(error = %e, wallet = %wallet.id, "the engine could not delete a wallet");
            return render_detail(
                &state,
                &user,
                wallet,
                Some("The wallet couldn't be deleted right now. Try again in a minute.".to_owned()),
                None,
                None,
            )
            .await;
        }
    }
    let (user_id, wallet_id) = (user.id.clone(), wallet.id.clone());
    match state
        .db
        .write(move |db| db.delete_wallet(&user_id, &wallet_id))
        .await
    {
        Ok(DeleteWallet::Deleted | DeleteWallet::NotFound) => redirect_303("/dashboard/wallets"),
        Ok(DeleteWallet::InUse(n)) => {
            render_detail(
                &state,
                &user,
                wallet,
                Some(format!(
                    "{n} store(s) still use this wallet. Delete or move them first."
                )),
                None,
                None,
            )
            .await
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

// -- The page's script, its module and the wallet app icons -----------------

pub async fn script(headers: axum::http::HeaderMap) -> Response {
    super::pay::static_asset(
        &headers,
        "text/javascript; charset=utf-8",
        super::pay::REVALIDATE,
        include_bytes!("../../static/wallet-setup.js"),
    )
}

pub async fn module(headers: axum::http::HeaderMap) -> Response {
    super::pay::static_asset(
        &headers,
        "application/wasm",
        super::pay::REVALIDATE,
        include_bytes!(concat!(env!("OUT_DIR"), "/wallet_setup.wasm")),
    )
}

pub async fn logo(headers: axum::http::HeaderMap, Path(file): Path<String>) -> Response {
    let bytes: &'static [u8] = match file.as_str() {
        "cake.png" => include_bytes!("../../static/wallet-logos/cake.png"),
        "monerocom.png" => include_bytes!("../../static/wallet-logos/monerocom.png"),
        "stack.png" => include_bytes!("../../static/wallet-logos/stack.png"),
        "feather.png" => include_bytes!("../../static/wallet-logos/feather.png"),
        "gui.png" => include_bytes!("../../static/wallet-logos/gui.png"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    super::pay::static_asset(&headers, "image/png", super::pay::REVALIDATE, bytes)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use tower::ServiceExt;

    use super::*;
    use crate::engine_client::EngineClient;
    use crate::http::build_router;
    use crate::http::test_support::{body_text, urlencoding_encode};

    const TEST_VIEW_KEY_HEX: &str =
        "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str =
        "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
    const PASSWORD: &str = "correct horse battery staple";

    async fn real_engine_state() -> (AppState, engine_test_support::TestEngineHandle) {
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let state = AppState {
            engine: crate::http::Engine::new(EngineClient::embedded_for_tests(engine.router())),
            ..AppState::for_tests()
        }
        .with_options("public_url = \"https://pay.example.test\"\n[signup]\nmode = \"public\"\n")
        .await;
        (state, engine)
    }

    fn post(uri: &str, cookie: Option<&str>, fields: &[(&str, &str)]) -> Request<Body> {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded");
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        builder.body(Body::from(body)).unwrap()
    }

    async fn get(router: &Router, uri: &str, cookie: &str) -> axum::response::Response {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn location(response: &axum::response::Response) -> String {
        response.headers()["location"].to_str().unwrap().to_owned()
    }

    /// Signs up through the form: the new account is logged in straight
    /// away; its session cookie, and where sign-up sent it.
    async fn sign_up(router: &Router, email: &str, next: Option<&str>) -> (String, String) {
        let mut fields = vec![("email", email), ("password", PASSWORD)];
        if let Some(next) = next {
            fields.push(("next", next));
        }
        let response = router
            .clone()
            .oneshot(post("/dashboard/signup", None, &fields))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        (cookie, location(&response))
    }

    async fn bring_in(router: &Router, cookie: &str, name: &str) -> axum::response::Response {
        router
            .clone()
            .oneshot(post(
                "/dashboard/wallets/import",
                Some(cookie),
                &[
                    ("name", name),
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                    ("network", "mainnet"),
                ],
            ))
            .await
            .unwrap()
    }

    fn wallets_of(state: &AppState, email: &str) -> Vec<crate::db::WalletSummary> {
        let db = state.db.lock();
        let user = db.get_user_by_email(email).unwrap().unwrap();
        db.list_wallets(&user.id).unwrap()
    }

    /// A new wallet as the "Create a new wallet" page makes one, with the
    /// same module the page runs.
    fn made_in_the_browser(seed: u8) -> wallet_setup::NewWallet {
        wallet_setup::generate([seed; 32], 1_791_400_000, wallet_setup::Network::Mainnet).unwrap()
    }

    #[tokio::test]
    async fn signing_up_leads_to_setting_up_a_wallet_where_creating_one_waits_for_javascript() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state);
        let (cookie, to) = sign_up(&router, "first@example.com", None).await;
        assert_eq!(to, "/dashboard/wallets/setup");

        let html = body_text(get(&router, &to, &cookie).await).await;
        assert!(html.contains("Set up your wallet"));
        assert!(
            html.contains(r#"aria-current="step""#),
            "the setup steps: {html}"
        );
        // Drawn unavailable, with the reason, until the script turns it on.
        assert!(html.contains("pick-card recommended unavailable"));
        assert!(
            html.contains(r#"formaction="/dashboard/wallets/new" disabled"#),
            "{html}"
        );
        assert!(html.contains("Creating a new wallet needs JavaScript"));
        assert!(
            html.contains("Coming soon"),
            "hardware wallets aren't there yet"
        );
        assert!(
            html.contains(r#"formaction="/dashboard/wallets/import""#),
            "bring your own works without it"
        );
    }

    #[tokio::test]
    async fn a_brought_in_wallet_is_registered_named_and_listed() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let (cookie, _) = sign_up(&router, "byo@example.com", None).await;

        let added = bring_in(&router, &cookie, "  Market   stall ").await;
        assert_eq!(added.status(), StatusCode::SEE_OTHER);
        let ready = location(&added);
        assert!(ready.ends_with("/ready"), "{ready}");

        let wallets = wallets_of(&state, "byo@example.com");
        assert_eq!(wallets.len(), 1);
        let wallet = &wallets[0].wallet;
        assert_eq!(wallet.name, "Market stall");
        assert_eq!(wallet.origin, crate::db::WalletOrigin::Imported);
        assert!(wallet.primary_address.starts_with('4'));

        let html = body_text(get(&router, &ready, &cookie).await).await;
        assert!(html.contains("You're ready to take payments"), "{html}");
        assert!(
            html.contains(r#"href="/dashboard/stores/new""#),
            "one button to add a store"
        );

        let list = body_text(get(&router, "/dashboard/wallets", &cookie).await).await;
        assert!(
            list.contains("Market stall") && list.contains("Brought in"),
            "{list}"
        );
    }

    #[tokio::test]
    async fn the_same_wallet_twice_is_refused_and_a_blank_name_is_picked_for_you() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let (cookie, _) = sign_up(&router, "twice@example.com", None).await;

        assert_eq!(
            bring_in(&router, &cookie, "").await.status(),
            StatusCode::SEE_OTHER
        );
        let name = wallets_of(&state, "twice@example.com")[0]
            .wallet
            .name
            .clone();
        assert_eq!(
            name.split(' ').count(),
            2,
            "a friendly two-word name: {name}"
        );

        let again = bring_in(&router, &cookie, "Another").await;
        assert_eq!(again.status(), StatusCode::OK);
        let html = body_text(again).await;
        assert!(html.contains("already added this wallet"), "{html}");
        assert!(html.contains(&name), "it says which one: {html}");
        assert_eq!(wallets_of(&state, "twice@example.com").len(), 1);
    }

    #[tokio::test]
    async fn a_wallet_made_in_the_browser_is_added_with_its_backup_and_the_page_never_sends_its_phrase(
    ) {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let (cookie, _) = sign_up(&router, "maker@example.com", None).await;

        let page = get(
            &router,
            "/dashboard/wallets/new?name=Copper%20Heron",
            &cookie,
        )
        .await;
        assert_eq!(page.headers()["cache-control"], "no-store");
        let html = body_text(page).await;
        assert!(
            html.contains(r#"data-wallet-setup"#) && html.contains(r#"data-name="Copper Heron""#)
        );
        assert!(html.contains("Back up Copper Heron"));
        assert!(html.contains("/static/wallet-logos/cake.png"));
        assert!(
            !html.contains(r#"name="phrase""#) && !html.contains(r#"name="seed""#),
            "the form posts no phrase field at all: {html}"
        );

        let wallet = made_in_the_browser(11);
        let added = router
            .clone()
            .oneshot(post(
                "/dashboard/wallets/new",
                Some(&cookie),
                &[
                    ("name", "Copper Heron"),
                    ("network", "mainnet"),
                    ("backup", "cake"),
                    ("primary_address", &wallet.address),
                    ("view_key_hex", &wallet.view_key_hex),
                    ("spend_pubkey_hex", &wallet.spend_pubkey_hex),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(added.status(), StatusCode::SEE_OTHER);
        let recorded = &wallets_of(&state, "maker@example.com")[0].wallet;
        assert_eq!(recorded.primary_address, wallet.address);
        assert_eq!(recorded.origin, crate::db::WalletOrigin::Created);
        assert_eq!(recorded.backup.as_deref(), Some("cake"));
    }

    #[tokio::test]
    async fn keys_that_dont_match_the_pages_address_are_refused_and_nothing_is_kept() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let (cookie, _) = sign_up(&router, "mismatch@example.com", None).await;
        let (made, other) = (made_in_the_browser(12), made_in_the_browser(13));
        let response = router
            .clone()
            .oneshot(post(
                "/dashboard/wallets/new",
                Some(&cookie),
                &[
                    ("name", "Mixed up"),
                    ("network", "mainnet"),
                    ("backup", "paper"),
                    ("primary_address", &other.address),
                    ("view_key_hex", &made.view_key_hex),
                    ("spend_pubkey_hex", &made.spend_pubkey_hex),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("don&#39;t match") || html.contains("don't match"),
            "{html}"
        );
        assert!(html.contains("Nothing was saved"), "{html}");
        assert!(wallets_of(&state, "mismatch@example.com").is_empty());
    }

    /// The point of named wallets: two stores on one wallet, each handing
    /// out its own addresses from the wallet's one counter.
    #[tokio::test]
    async fn two_stores_on_one_wallet_get_different_order_addresses() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let (cookie, _) = sign_up(&router, "two-shops@example.com", None).await;
        assert_eq!(
            bring_in(&router, &cookie, "Shared").await.status(),
            StatusCode::SEE_OTHER
        );
        let wallet_id = wallets_of(&state, "two-shops@example.com")[0]
            .wallet
            .id
            .to_string();

        for site in ["https://one.example.com", "https://two.example.com"] {
            let response = router
                .clone()
                .oneshot(post(
                    "/dashboard/connect",
                    Some(&cookie),
                    &[
                        ("site_url", site),
                        ("wallet_id", &wallet_id),
                        ("base_currency", "XMR"),
                    ],
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FOUND, "{site}");
        }
        let stores = {
            let db = state.db.lock();
            let user = db
                .get_user_by_email("two-shops@example.com")
                .unwrap()
                .unwrap();
            db.list_store_connections_for_user(&user.id).unwrap()
        };
        assert_eq!(stores.len(), 2);
        let mut addresses = Vec::new();
        for store in &stores {
            assert_eq!(
                store.wallet_id.as_ref().map(|w| w.to_string()),
                Some(wallet_id.clone())
            );
            let sk = super::super::orders::decrypt_sk(&state.encryption_key, store).unwrap();
            for _ in 0..2 {
                let order = state
                    .engine
                    .client
                    .create_order(
                        &sk,
                        shared::xmr_amount::Piconero(1_000_000),
                        None,
                        None,
                        None,
                    )
                    .await
                    .unwrap();
                addresses.push(order.address);
            }
        }
        let mut unique = addresses.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            4,
            "no address is handed to both stores: {addresses:?}"
        );

        let html =
            body_text(get(&router, &format!("/dashboard/wallets/{wallet_id}"), &cookie).await)
                .await;
        assert!(
            html.contains("one.example.com") && html.contains("two.example.com"),
            "{html}"
        );
        assert!(
            html.contains("store(s) still use this wallet"),
            "it can't be deleted yet: {html}"
        );
    }

    #[tokio::test]
    async fn the_store_form_picks_your_only_wallet_but_never_one_of_several() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let (cookie, _) = sign_up(&router, "picker@example.com", None).await;
        bring_in(&router, &cookie, "Only one").await;
        let id = wallets_of(&state, "picker@example.com")[0]
            .wallet
            .id
            .to_string();
        let html = body_text(get(&router, "/dashboard/connect", &cookie).await).await;
        assert!(
            html.contains(&format!(r#"value="{id}" selected"#)),
            "{html}"
        );

        let made = made_in_the_browser(21);
        router
            .clone()
            .oneshot(post(
                "/dashboard/wallets/new",
                Some(&cookie),
                &[
                    ("name", "Second"),
                    ("network", "mainnet"),
                    ("backup", "paper"),
                    ("primary_address", &made.address),
                    ("view_key_hex", &made.view_key_hex),
                    ("spend_pubkey_hex", &made.spend_pubkey_hex),
                ],
            ))
            .await
            .unwrap();
        let html = body_text(get(&router, "/dashboard/connect", &cookie).await).await;
        assert!(html.contains("Choose a wallet…"), "{html}");
        assert!(!html.contains(" selected>Only one") && !html.contains(" selected>Second"));
        assert!(
            !html.contains(&format!(r#"value="{id}" selected"#)),
            "{html}"
        );
    }

    #[tokio::test]
    async fn a_wallet_is_renamed_and_only_deleted_unused_with_its_name_typed() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let (cookie, _) = sign_up(&router, "detail@example.com", None).await;
        bring_in(&router, &cookie, "Old name").await;
        let id = wallets_of(&state, "detail@example.com")[0]
            .wallet
            .id
            .to_string();
        let page = format!("/dashboard/wallets/{id}");

        let renamed = router
            .clone()
            .oneshot(post(
                &format!("{page}/rename"),
                Some(&cookie),
                &[("name", "New name")],
            ))
            .await
            .unwrap();
        assert_eq!(renamed.status(), StatusCode::SEE_OTHER);
        let html = body_text(get(&router, &location(&renamed), &cookie).await).await;
        assert!(
            html.contains("New name") && html.contains("Renamed from"),
            "{html}"
        );
        assert!(
            html.contains("btn-danger"),
            "no store uses it, so delete is offered: {html}"
        );

        let wrong = router
            .clone()
            .oneshot(post(
                &format!("{page}/delete"),
                Some(&cookie),
                &[("confirm", "nope")],
            ))
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::OK);
        assert_eq!(wallets_of(&state, "detail@example.com").len(), 1);

        let deleted = router
            .clone()
            .oneshot(post(
                &format!("{page}/delete"),
                Some(&cookie),
                &[("confirm", "New name")],
            ))
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::SEE_OTHER);
        assert!(wallets_of(&state, "detail@example.com").is_empty());
        // The engine forgot it too: the same keys can be brought in again.
        assert_eq!(
            bring_in(&router, &cookie, "Back again").await.status(),
            StatusCode::SEE_OTHER
        );
    }

    #[tokio::test]
    async fn someone_elses_wallet_is_not_found() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let (owner, _) = sign_up(&router, "owner@example.com", None).await;
        bring_in(&router, &owner, "Mine").await;
        let id = wallets_of(&state, "owner@example.com")[0]
            .wallet
            .id
            .to_string();
        let (other, _) = sign_up(&router, "other@example.com", None).await;
        let page = format!("/dashboard/wallets/{id}");
        assert_eq!(
            get(&router, &page, &other).await.status(),
            StatusCode::NOT_FOUND
        );
        let delete = router
            .clone()
            .oneshot(post(
                &format!("{page}/delete"),
                Some(&other),
                &[("confirm", "Mine")],
            ))
            .await
            .unwrap();
        assert_eq!(delete.status(), StatusCode::NOT_FOUND);
        assert_eq!(wallets_of(&state, "owner@example.com").len(), 1);
    }

    /// From the WooCommerce plugin with no account: sign up, set up a wallet,
    /// and back to connecting the shop with that wallet picked.
    #[tokio::test]
    async fn a_new_merchant_from_woocommerce_comes_back_to_connect_the_shop_after_wallet_setup() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let connect = "/connect/woocommerce?site_url=https%3A%2F%2Fshop.example.com&return_url=https%3A%2F%2Fshop.example.com%2Fwp-admin&nonce=n1";

        let login = router
            .clone()
            .oneshot(Request::builder().uri(connect).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let login_page = location(&login);
        assert!(
            login_page.starts_with("/dashboard/login?next="),
            "{login_page}"
        );
        let html = body_text(
            router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(&login_page)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert!(html.contains("shop.example.com wants to connect"), "{html}");
        assert!(
            html.contains("/dashboard/signup?next="),
            "sign up keeps where they were going: {html}"
        );

        let (cookie, to) = sign_up(&router, "from-woo@example.com", Some(connect)).await;
        assert!(
            to.starts_with("/dashboard/wallets/setup?next=%2Fconnect%2Fwoocommerce"),
            "{to}"
        );
        let html = body_text(get(&router, &to, &cookie).await).await;
        assert!(
            html.contains("shop.example.com"),
            "it says what the wallet is for: {html}"
        );

        let added = router
            .clone()
            .oneshot(post(
                "/dashboard/wallets/import",
                Some(&cookie),
                &[
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                    ("network", "mainnet"),
                    ("next", connect),
                ],
            ))
            .await
            .unwrap();
        let ready = body_text(get(&router, &location(&added), &cookie).await).await;
        assert!(
            ready.contains("Continue connecting shop.example.com"),
            "{ready}"
        );

        let html = body_text(get(&router, connect, &cookie).await).await;
        let id = wallets_of(&state, "from-woo@example.com")[0]
            .wallet
            .id
            .to_string();
        assert!(
            html.contains(r#"name="mode" value="new""#),
            "no store yet, so no question: {html}"
        );
        assert!(
            html.contains(&format!(r#"value="{id}" selected"#)),
            "{html}"
        );
    }

    /// A store made before wallets is matched to the wallet the engine says
    /// it uses, the first time the wallets page is opened.
    #[tokio::test]
    async fn a_store_made_before_wallets_gets_its_wallet() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let (cookie, _) = sign_up(&router, "legacy@example.com", None).await;
        bring_in(&router, &cookie, "Temporary").await;
        let id = wallets_of(&state, "legacy@example.com")[0]
            .wallet
            .id
            .to_string();
        router
            .clone()
            .oneshot(post(
                "/dashboard/connect",
                Some(&cookie),
                &[
                    ("site_url", "https://old.example.com"),
                    ("wallet_id", &id),
                    ("base_currency", "XMR"),
                ],
            ))
            .await
            .unwrap();
        // As it was before wallets: no wallet on the store, none recorded.
        {
            let db = state.db.lock();
            db.conn_for_test()
                .execute_batch(
                    "UPDATE store_connections SET wallet_id = NULL; DELETE FROM wallets;",
                )
                .unwrap();
        }
        let html = body_text(get(&router, "/dashboard/wallets", &cookie).await).await;
        assert!(html.contains("Brought in"), "{html}");
        let wallets = wallets_of(&state, "legacy@example.com");
        assert_eq!(wallets.len(), 1);
        assert_eq!(wallets[0].store_count, 1);
    }

    #[test]
    fn the_connecting_site_is_read_from_a_woocommerce_connect_link_only() {
        let next =
            "/connect/woocommerce?site_url=https%3A%2F%2Fshop.example.com%2F&return_url=x&nonce=n";
        assert_eq!(
            connecting_site(Some(next)).as_deref(),
            Some("shop.example.com")
        );
        assert_eq!(connecting_site(Some("/dashboard/connect")), None);
        assert_eq!(connecting_site(None), None);
    }

    #[test]
    fn a_typed_name_wins_then_a_free_suggestion_then_a_fresh_one() {
        let taken = vec!["Copper Heron".to_owned()];
        assert_eq!(
            chosen_name(Some(" Till "), Some("Amber Finch"), &taken),
            "Till"
        );
        assert_eq!(
            chosen_name(Some(""), Some("Amber Finch"), &taken),
            "Amber Finch"
        );
        let fresh = chosen_name(None, Some("copper heron"), &taken);
        assert_ne!(fresh.to_lowercase(), "copper heron");
    }
}
