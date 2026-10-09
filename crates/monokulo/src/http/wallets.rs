//! A merchant's wallets (docs/wallets.md): `/account?tab=wallets` and its
//! pages. Adding one from the Account page (`/account/wallets/add`):
//! bringing one's own (keys pasted in, works without JavaScript) or making
//! a new one in the browser (the phrase never leaves the page; only
//! watch-only keys are posted). The list, and a wallet's page where it is
//! renamed or retired.
//!
//! The screens for adding a wallet are shared with setup's Wallet step
//! (`http::setup`): the helpers here take the [`Flow`] they're in.

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Form;
use maud::html;
use serde::Deserialize;

use crate::db::{UserRow, WalletId, WalletOrigin, WalletRow};
use crate::views;
use crate::views::wallets::{
    ChoiceViewModel, CreateViewModel, DetailViewModel, Flow, ImportViewModel, RenameOutcome,
    WalletEvent, WalletListItem, WalletStore,
};
use crate::wallets::{clean_name, friendly_name};

use super::dashboard::redirect_303;
use super::wallet_service::{add_wallet, adopt_unlinked_stores, AddWallet, AddWalletError};
use super::{AppState, AuthedUser};

/// The name and network chosen on the choice screen, as the next screen
/// gets them.
#[derive(Deserialize, Default)]
pub struct NameQuery {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub network: Option<String>,
}

pub(super) fn network_or_mainnet(network: Option<&str>) -> String {
    match network {
        Some(n @ ("stagenet" | "testnet")) => n.to_owned(),
        _ => "mainnet".to_owned(),
    }
}

async fn wallet_names(state: &AppState, user: &UserRow) -> Vec<String> {
    let user_id = user.id.clone();
    state
        .db
        .read(move |db| db.wallet_names(&user_id))
        .await
        .unwrap_or_default()
}

pub(super) fn already_added(existing: &WalletRow) -> String {
    if existing.retired_at.is_some() {
        format!(
            "You retired this wallet, \u{201c}{}\u{201d}. Restore it on its page to use it again.",
            existing.name
        )
    } else {
        format!(
            "You've already added this wallet, as \u{201c}{}\u{201d}.",
            existing.name
        )
    }
}

/// Why `name` can't be a new wallet's name for `user`, if it can't: too
/// long, or one of their wallets has it. Blank is fine: one is picked.
pub(super) async fn name_problem(state: &AppState, user: &UserRow, name: &str) -> Option<String> {
    let name = match clean_name(name) {
        Ok(Some(name)) => name,
        Ok(None) => return None,
        Err(why) => return Some(why),
    };
    let taken = wallet_names(state, user).await;
    taken
        .iter()
        .any(|t| t.eq_ignore_ascii_case(&name))
        .then(|| format!("You already have a wallet called {name}. Pick another name."))
}

/// The name a new wallet gets: the one typed, else a friendly one. One
/// taken in the meantime gets a number, so a phrase already backed up is
/// never refused for its name.
async fn final_name(state: &AppState, user: &UserRow, typed: &str) -> String {
    let taken = wallet_names(state, user).await;
    let is_free = |name: &str| !taken.iter().any(|t| t.eq_ignore_ascii_case(name));
    match clean_name(typed) {
        Ok(Some(name)) if is_free(&name) => name,
        Ok(Some(name)) => (2..)
            .map(|n| format!("{name} ({n})"))
            .find(|candidate| is_free(candidate))
            .unwrap_or(name),
        _ => friendly_name(&taken),
    }
}

/// The name offered on the choice screen: `wanted` (the store's own idea,
/// "Bakery takings") if it's free, else a friendly one.
pub(super) async fn suggested_name(
    state: &AppState,
    user: &UserRow,
    wanted: Option<&str>,
) -> String {
    let taken = wallet_names(state, user).await;
    match wanted.and_then(|w| clean_name(w).ok().flatten()) {
        Some(name) if !taken.iter().any(|t| t.eq_ignore_ascii_case(&name)) => name,
        _ => friendly_name(&taken),
    }
}

/// The choice screen: use a wallet already added (in setup), or name a new
/// one and pick where it comes from.
pub(super) async fn render_choice(
    state: &AppState,
    user: &UserRow,
    flow: Flow<'_>,
    name: String,
    name_problem: Option<String>,
    network: String,
    error: Option<String>,
) -> Response {
    let wallets = match flow {
        Flow::Setup(_) => {
            let user_id = user.id.clone();
            state
                .db
                .read(move |db| db.list_wallets(&user_id))
                .await
                .unwrap_or_default()
        }
        Flow::Account => Vec::new(),
    };
    let data = ChoiceViewModel {
        wallets,
        name,
        name_problem,
        network,
        error,
    };
    let path = match flow {
        Flow::Setup(_) => "/setup/wallet",
        Flow::Account => "/account/wallets/add",
    };
    let chrome = super::page_chrome(state, Some(user), path).await;
    views::wallets::choice_page(&chrome, flow, &data).into_response()
}

/// `GET /account/wallets/add`.
pub async fn add(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Query(query): Query<NameQuery>,
) -> Response {
    let name = match query.name {
        Some(name) => name,
        None => suggested_name(&state, &user, None).await,
    };
    let problem = name_problem(&state, &user, &name).await;
    render_choice(
        &state,
        &user,
        Flow::Account,
        name,
        problem,
        network_or_mainnet(query.network.as_deref()),
        None,
    )
    .await
}

#[derive(Deserialize)]
pub struct NameCheckQuery {
    #[serde(default)]
    name: String,
}

/// `GET /account/wallets/name-check?name=`: whether the name is free, as
/// the line under the field (fixi swaps it in as the name changes).
pub async fn name_check(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Query(query): Query<NameCheckQuery>,
) -> Response {
    let problem = name_problem(&state, &user, &query.name).await;
    views::wallets::name_check(&query.name, problem.as_deref()).into_response()
}

// -- Bring your own wallet ---------------------------------------------------

#[derive(Deserialize)]
pub struct ImportForm {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub view_key_hex: String,
    #[serde(default)]
    pub spend_pubkey_hex: String,
    #[serde(default)]
    pub encrypted_keys: Option<String>,
    #[serde(default)]
    pub network: String,
    /// "Which app is it in?": a `WALLET_APPS` key, `other`, or blank.
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub key_custody_backend: Option<String>,
}

pub(super) async fn render_import(
    state: &AppState,
    user: &UserRow,
    flow: Flow<'_>,
    name: String,
    network: String,
    error: Option<String>,
    form: Option<&ImportForm>,
) -> Response {
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
        error,
        name,
        network,
        spend_pubkey_hex: form.map(|f| f.spend_pubkey_hex.clone()).unwrap_or_default(),
        app: form.and_then(|f| f.app.clone()).unwrap_or_default(),
        custody_choices,
        snp_entry,
    };
    let path = match flow {
        Flow::Setup(_) => "/setup/wallet/keys",
        Flow::Account => "/account/wallets/import",
    };
    let chrome = super::page_chrome(state, Some(user), path).await;
    views::wallets::import_page(&chrome, flow, &data).into_response()
}

/// The keys screen, unless the name chosen can't be used: then the choice
/// screen again, saying why. Nothing is made before the name is checked.
pub(super) async fn keys_screen(
    state: &AppState,
    user: &UserRow,
    flow: Flow<'_>,
    query: NameQuery,
) -> Response {
    let name = query.name.unwrap_or_default();
    let network = network_or_mainnet(query.network.as_deref());
    if let Some(problem) = name_problem(state, user, &name).await {
        return render_choice(state, user, flow, name, Some(problem), network, None).await;
    }
    render_import(state, user, flow, name, network, None, None).await
}

/// `GET /account/wallets/import`.
pub async fn import_form(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Query(query): Query<NameQuery>,
) -> Response {
    keys_screen(&state, &user, Flow::Account, query).await
}

/// Adds the wallet whose keys were pasted in, or the keys screen again
/// saying why it couldn't.
pub(super) async fn add_imported(
    state: &AppState,
    user: &UserRow,
    flow: Flow<'_>,
    form: &ImportForm,
) -> Result<WalletRow, Box<Response>> {
    let name = final_name(state, user, &form.name).await;
    let network = network_or_mainnet(Some(&form.network));
    let added = add_wallet(
        state,
        user,
        AddWallet {
            name: name.clone(),
            view_key_hex: form.view_key_hex.clone(),
            spend_pubkey_hex: form.spend_pubkey_hex.clone(),
            encrypted_keys: form.encrypted_keys.clone(),
            network: network.clone(),
            key_custody_backend: form.key_custody_backend.clone(),
            origin: WalletOrigin::Imported,
            backup: None,
            app: form.app.clone().filter(|app| !app.is_empty()),
            expected_address: None,
        },
    )
    .await;
    let message = match added {
        Ok(wallet) => return Ok(wallet),
        Err(AddWalletError::AlreadyAdded(existing)) => already_added(&existing),
        Err(AddWalletError::Invalid(message)) => message,
        Err(AddWalletError::Internal) => {
            "The wallet couldn't be added right now. Try again in a minute.".to_owned()
        }
    };
    Err(Box::new(
        render_import(
            state,
            user,
            flow,
            form.name.clone(),
            network,
            Some(message),
            Some(form),
        )
        .await,
    ))
}

/// `POST /account/wallets/import`.
pub async fn import_submit(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Form(form): Form<ImportForm>,
) -> Response {
    match add_imported(&state, &user, Flow::Account, &form).await {
        Ok(wallet) => redirect_303(&format!("/account/wallets/{}?added=1", wallet.id)),
        Err(page) => *page,
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
    flow: Flow<'_>,
    name: String,
    network: String,
) -> Response {
    let data = CreateViewModel {
        restore_height: current_height(state, &network).await,
        snp_entry: snp_for_new_wallet(state, user).await,
        name,
        network,
        error: None,
    };
    let path = match flow {
        Flow::Setup(_) => "/setup/wallet/new",
        Flow::Account => "/account/wallets/new",
    };
    let chrome = super::page_chrome(state, Some(user), path).await;
    (
        // Never kept: a reload makes a different wallet.
        [(header::CACHE_CONTROL, "no-store")],
        views::wallets::create_page(&chrome, flow, &data),
    )
        .into_response()
}

/// The create screens, unless the name chosen can't be used: then the
/// choice screen again, saying why, before any phrase is made.
pub(super) async fn create_screen(
    state: &AppState,
    user: &UserRow,
    flow: Flow<'_>,
    query: NameQuery,
) -> Response {
    let typed = query.name.unwrap_or_default();
    let network = network_or_mainnet(query.network.as_deref());
    if let Some(problem) = name_problem(state, user, &typed).await {
        return render_choice(state, user, flow, typed, Some(problem), network, None).await;
    }
    // A blank name is picked now, so the restore link carries it.
    let name = final_name(state, user, &typed).await;
    render_create(state, user, flow, name, network).await
}

/// `GET /account/wallets/new`.
pub async fn create_form(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Query(query): Query<NameQuery>,
) -> Response {
    create_screen(&state, &user, Flow::Account, query).await
}

#[derive(Deserialize)]
pub struct CreateForm {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub network: String,
    #[serde(default)]
    pub backup: String,
    #[serde(default)]
    pub primary_address: String,
    #[serde(default)]
    pub view_key_hex: String,
    #[serde(default)]
    pub spend_pubkey_hex: String,
    #[serde(default)]
    pub encrypted_keys: Option<String>,
}

/// Adds the wallet the page made, from its watch-only keys and how its
/// phrase was backed up; the page's address is checked against the one
/// the engine works out from the keys. `Err` is the choice screen again,
/// saying what went wrong.
pub(super) async fn add_created(
    state: &AppState,
    user: &UserRow,
    flow: Flow<'_>,
    form: &CreateForm,
) -> Result<WalletRow, Box<Response>> {
    let network = network_or_mainnet(Some(&form.network));
    let added = add_wallet(
        state,
        user,
        AddWallet {
            name: final_name(state, user, &form.name).await,
            view_key_hex: form.view_key_hex.clone(),
            spend_pubkey_hex: form.spend_pubkey_hex.clone(),
            encrypted_keys: form.encrypted_keys.clone(),
            network: network.clone(),
            key_custody_backend: None,
            origin: WalletOrigin::Created,
            backup: Some(form.backup.clone()),
            app: None,
            expected_address: Some(form.primary_address.clone()),
        },
    )
    .await;
    let message = match added {
        Ok(wallet) => return Ok(wallet),
        Err(AddWalletError::Invalid(message)) => message,
        Err(AddWalletError::AlreadyAdded(existing)) => already_added(&existing),
        Err(AddWalletError::Internal) => "The wallet couldn't be added right now.".to_owned(),
    };
    // The phrase backed up on that page is registered nowhere: say so
    // plainly, rather than showing a new phrase as if nothing happened.
    let message = format!(
        "{message} Nothing was saved, so the recovery phrase you backed up isn't connected to Monokulo. Start again below."
    );
    Err(Box::new(
        render_choice(
            state,
            user,
            flow,
            form.name.clone(),
            None,
            network,
            Some(message),
        )
        .await,
    ))
}

/// `POST /account/wallets/new`.
pub async fn create_submit(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Form(form): Form<CreateForm>,
) -> Response {
    match add_created(&state, &user, Flow::Account, &form).await {
        Ok(wallet) => {
            let skipped = if form.backup == "skipped" {
                "&skipped=1"
            } else {
                ""
            };
            redirect_303(&format!("/account/wallets/{}?added=1{skipped}", wallet.id))
        }
        Err(page) => *page,
    }
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

// -- The list and a wallet's page --------------------------------------------

/// `user`'s wallets and retired wallets for the Account page's Wallets tab
/// (`http::account`), retirement times in `clock`. `None` when they can't
/// be read.
pub(super) async fn list(
    state: &AppState,
    user: &UserRow,
    clock: &views::time::Clock,
) -> Option<(Vec<WalletListItem>, Vec<views::wallets::RetiredListItem>)> {
    adopt_unlinked_stores(state, user).await;
    let user_id = user.id.clone();
    let (wallets, retired) = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                db.list_wallets(&user_id)?,
                db.list_retired_wallets(&user_id)?,
            ))
        })
        .await
        .ok()?;
    let items = wallets
        .into_iter()
        .map(|w| WalletListItem {
            id: w.wallet.id.to_string(),
            kind: match w.wallet.origin {
                WalletOrigin::Created => "Made in Monokulo",
                WalletOrigin::Imported => "Brought in",
            },
            address: w.wallet.primary_address,
            network: w.wallet.network,
            name: w.wallet.name,
            stores: w.store_count,
        })
        .collect();
    let retired = retired
        .into_iter()
        .map(|w| views::wallets::RetiredListItem {
            id: w.id.to_string(),
            retired: w.retired_at.map(|at| clock.text(at)).unwrap_or_default(),
            name: w.name,
        })
        .collect();
    Some((items, retired))
}

#[derive(Deserialize, Default)]
pub struct DetailQuery {
    #[serde(default)]
    renamed: Option<String>,
    #[serde(default)]
    restored: Option<String>,
    /// Just added (`POST /account/wallets/import` or `/new`).
    #[serde(default)]
    added: Option<String>,
    /// Added without backing up its phrase.
    #[serde(default)]
    skipped: Option<String>,
}

async fn render_detail(
    state: &AppState,
    user: &UserRow,
    wallet: WalletRow,
    error: Option<String>,
    notice: Option<String>,
    rename: Option<RenameOutcome>,
) -> Response {
    let chrome =
        super::page_chrome(state, Some(user), format!("/account/wallets/{}", wallet.id)).await;
    let (user_id, wallet_id) = (user.id.clone(), wallet.id.clone());
    // Every store that has used the wallet: those on it now, and those
    // that changed to another (`store_wallet_periods`).
    let (stores, periods, events) = state
        .db
        .read(move |db| {
            let periods = db.wallet_store_periods(&wallet_id)?;
            let stores: Vec<_> = db
                .list_store_connections_for_user(&user_id)?
                .into_iter()
                .filter(|s| {
                    s.wallet_id.as_ref() == Some(&wallet_id)
                        || periods.iter().any(|p| p.connection_id == s.id)
                })
                .collect();
            Ok::<_, crate::db::DbError>((stores, periods, db.wallet_events(&wallet_id)?))
        })
        .await
        .unwrap_or_default();
    let store_name = |id: &str| {
        stores
            .iter()
            .find(|s| s.id == id)
            .map(|s| (s.id.to_string(), s.name.clone()))
    };

    let mut history: Vec<(i64, WalletEvent)> = events
        .iter()
        .map(|event| {
            let what = match event.kind.as_str() {
                "created" => html! {
                    "Made in this browser"
                    @if !event.detail.is_empty() { ", " (crate::wallets::backup_label(&event.detail).to_lowercase()) }
                },
                "imported" => match crate::wallets::wallet_app(&event.detail) {
                    Some(app) => html! { "Brought in from " (app.name) },
                    None => html! { "Brought in" },
                },
                "renamed" => html! { "Renamed from \u{201c}" (event.detail) "\u{201d}" },
                "store_connected" => match store_name(&event.detail) {
                    Some((id, name)) => html! { "Store " a href=(format!("/dashboard/stores/{id}")) { (name) } " connected" },
                    None => html! { "A store connected" },
                },
                "store_changed_to" => match store_name(&event.detail) {
                    Some((id, name)) => html! { "Store " a href=(format!("/dashboard/stores/{id}")) { (name) } " changed to this wallet" },
                    None => html! { "A store changed to this wallet" },
                },
                "retired" => html! { "Retired: its keys were deleted from key storage" },
                "restored" => html! { "Brought back with its keys" },
                "store_changed_away" => match store_name(&event.detail) {
                    Some((id, name)) => html! { "Store " a href=(format!("/dashboard/stores/{id}")) { (name) } " changed to another wallet" },
                    None => html! { "A store changed to another wallet" },
                },
                other => html! { (other) },
            };
            (event.at, WalletEvent { when: chrome.clock.text(event.at), what })
        })
        .collect();
    // Payments into the wallet: its stores' orders that have received
    // something, made while the store used this wallet, newest first.
    for store in &stores {
        let on_this_wallet = |at: i64| {
            periods.iter().any(|p| {
                p.connection_id == store.id
                    && at >= p.from
                    && p.until.is_none_or(|until| at < until)
            })
        };
        let Ok(sk) = super::orders::decrypt_sk(&state.encryption_key, store) else {
            continue;
        };
        let Ok(orders) = state.engine.client.list_orders(&sk).await else {
            continue;
        };
        let name = store.name.clone();
        for order in orders
            .into_iter()
            .filter(|o| o.amount_received_piconero > 0 && on_this_wallet(o.created_at))
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

    let retire = retire_state(state, &wallet, &chrome.clock).await;
    let restore = match wallet.retired_at {
        Some(_) => Some(restore_form(state, user).await),
        None => None,
    };
    let data = DetailViewModel {
        retire,
        restore,
        rename,
        stores: stores
            .iter()
            .filter(|s| s.wallet_id.as_ref() == Some(&wallet.id))
            .map(|s| wallet_store(s, None))
            .collect(),
        past_stores: periods
            .iter()
            .filter_map(|p| {
                let until = p.until?;
                let store = stores.iter().find(|s| {
                    s.id == p.connection_id && s.wallet_id.as_ref() != Some(&wallet.id)
                })?;
                Some(wallet_store(store, Some(chrome.clock.text(until))))
            })
            .fold(Vec::new(), |mut seen: Vec<WalletStore>, store| {
                // Newest first: a store that left twice is listed once.
                if !seen.iter().any(|s| s.id == store.id) {
                    seen.push(store);
                }
                seen
            }),
        history: history.into_iter().map(|(_, e)| e).collect(),
        wallet,
        error,
        notice,
    };
    let page = views::wallets::detail_page(&chrome, &data);
    if matches!(data.rename, Some(RenameOutcome::Refused { .. })) {
        (StatusCode::UNPROCESSABLE_ENTITY, page).into_response()
    } else {
        page.into_response()
    }
}

/// `GET /account/wallets/{id}`.
pub async fn detail(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Query(query): Query<DetailQuery>,
) -> Response {
    let Some(wallet) = load_wallet(&state, &user, &id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let notice = match (query.restored, query.added) {
        (Some(_), _) => Some(format!("{} is back. Stores can use it again.", wallet.name)),
        (None, Some(_)) => Some(format!(
            "{} is added. Pick it for a store in the store's settings, or when you set one up.",
            wallet.name
        )),
        (None, None) => None,
    };
    let error = (query.skipped.is_some() && wallet.backup.as_deref() == Some("skipped")).then(|| {
        "This wallet's recovery phrase was not backed up. Payments to it can't be spent unless you have it."
            .to_owned()
    });
    let rename = query.renamed.map(|_| RenameOutcome::Saved);
    render_detail(&state, &user, wallet, error, notice, rename).await
}

#[derive(Deserialize)]
pub struct RenameForm {
    #[serde(default)]
    name: String,
}

/// `POST /account/wallets/{id}/rename`.
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
            let refused = RenameOutcome::Refused {
                name: form.name,
                message: "A wallet needs a name.".to_owned(),
            };
            return render_detail(&state, &user, wallet, None, None, Some(refused)).await;
        }
        Err(message) => {
            let refused = RenameOutcome::Refused {
                name: form.name,
                message,
            };
            return render_detail(&state, &user, wallet, None, None, Some(refused)).await;
        }
    };
    let (user_id, wallet_id, new_name) = (user.id.clone(), wallet.id.clone(), name.clone());
    match state
        .db
        .write(move |db| db.rename_wallet(&user_id, &wallet_id, &new_name, crate::now_unix()))
        .await
    {
        Ok(_) => redirect_303(&format!("/account/wallets/{}?renamed=1", wallet.id)),
        Err(e) if e.is_unique_violation() => {
            let refused = RenameOutcome::Refused {
                name: form.name,
                message: format!("You already have a wallet called \u{201c}{name}\u{201d}."),
            };
            render_detail(&state, &user, wallet, None, None, Some(refused)).await
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// A store as a wallet's page lists it: its name, and under it its site
/// when it has one.
fn wallet_store(store: &crate::db::StoreConnectionRow, until: Option<String>) -> WalletStore {
    WalletStore {
        id: store.id.to_string(),
        name: store.name.clone(),
        site: (!store.site.is_empty()).then(|| store.site.clone()),
        until,
    }
}

/// The stores of `user`'s taking payments into `wallet` now.
async fn current_stores(state: &AppState, user: &UserRow, wallet: &WalletRow) -> Vec<WalletStore> {
    let (user_id, wallet_id) = (user.id.clone(), wallet.id.clone());
    state
        .db
        .read(move |db| db.list_store_connections_for_user(&user_id))
        .await
        .unwrap_or_default()
        .iter()
        .filter(|s| s.wallet_id.as_ref() == Some(&wallet_id))
        .map(|s| wallet_store(s, None))
        .collect()
}

/// Whether the wallet can be retired now, and if not why: the engine knows
/// its stores (of any account) and its orders.
async fn retire_state(
    state: &AppState,
    wallet: &WalletRow,
    clock: &views::time::Clock,
) -> views::wallets::RetireState {
    use views::wallets::RetireState;
    if wallet.retired_at.is_some() {
        return RetireState::Retired;
    }
    match state
        .engine
        .client
        .wallet_status(&wallet.engine_wallet_id)
        .await
    {
        Ok(status) => RetireState::Checked {
            stores: status.stores,
            orders: status.payable_orders,
            until: status
                .payable_until
                .filter(|_| status.payable_orders > 0)
                .map(|at| clock.text(at)),
        },
        Err(e) => {
            tracing::warn!(error = %e, wallet = %wallet.id, "could not ask the engine whether a wallet is in use");
            RetireState::Unknown
        }
    }
}

/// The key fields for bringing a retired wallet back, as on "Bring your
/// own wallet".
async fn restore_form(state: &AppState, user: &UserRow) -> views::wallets::RestoreForm {
    let custody_choices = super::status_page::custody_choice_views(state, None);
    let snp_entry = super::key_entry::prepare(
        state,
        &user.id,
        super::key_entry::Purpose::Create,
        &super::key_entry::offered_backends(state, &custody_choices),
    )
    .await;
    views::wallets::RestoreForm {
        custody_choices,
        snp_entry,
    }
}

/// The retire checklist as a page, with `error` from a refused retire.
async fn render_retire(
    state: &AppState,
    user: &UserRow,
    wallet: WalletRow,
    error: Option<String>,
) -> Response {
    let chrome = super::page_chrome(
        state,
        Some(user),
        format!("/account/wallets/{}/retire", wallet.id),
    )
    .await;
    let retire = retire_state(state, &wallet, &chrome.clock).await;
    let stores = current_stores(state, user, &wallet).await;
    views::wallets::retire_page(&chrome, &wallet, &retire, &stores, error.as_deref())
        .into_response()
}

/// `GET /account/wallets/{id}/retire`: the retire dialog as a page, for a
/// browser without JavaScript.
pub async fn retire_form(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
) -> Response {
    let Some(wallet) = load_wallet(&state, &user, &id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if wallet.retired_at.is_some() {
        return redirect_303(&format!("/account/wallets/{}", wallet.id));
    }
    render_retire(&state, &user, wallet, None).await
}

/// The restore form as a page, with `error` from a refused restore.
async fn render_restore(
    state: &AppState,
    user: &UserRow,
    wallet: WalletRow,
    error: Option<String>,
) -> Response {
    let chrome = super::page_chrome(
        state,
        Some(user),
        format!("/account/wallets/{}/restore", wallet.id),
    )
    .await;
    let restore = restore_form(state, user).await;
    views::wallets::restore_page(&chrome, &wallet, &restore, error.as_deref()).into_response()
}

/// `GET /account/wallets/{id}/restore`: the restore dialog as a page, for
/// a browser without JavaScript.
pub async fn restore_form_page(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
) -> Response {
    let Some(wallet) = load_wallet(&state, &user, &id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if wallet.retired_at.is_none() {
        return redirect_303(&format!("/account/wallets/{}", wallet.id));
    }
    render_restore(&state, &user, wallet, None).await
}

#[derive(Deserialize)]
pub struct RetireForm {
    #[serde(default)]
    confirm: String,
}

/// `POST /account/wallets/{id}/retire`: the wallet is offered nowhere
/// again and its keys are deleted (docs/wallets.md, "Retiring a wallet").
/// Refused unless its name was typed, while a store uses it, or while an
/// order on it can still be paid.
pub async fn retire(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(form): Form<RetireForm>,
) -> Response {
    let Some(wallet) = load_wallet(&state, &user, &id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if wallet.retired_at.is_some() {
        return redirect_303(&format!("/account/wallets/{}", wallet.id));
    }
    if form.confirm.trim() != wallet.name {
        let message = format!("Type \u{201c}{}\u{201d} exactly to retire it.", wallet.name);
        return render_retire(&state, &user, wallet, Some(message)).await;
    }
    // The engine first: it deletes the keys, and refuses while a store of
    // any account uses them or an order on them can still be paid.
    let retired_at = match state
        .engine
        .client
        .retire_wallet(&wallet.engine_wallet_id)
        .await
    {
        Ok(at) => at,
        Err(crate::engine_client::EngineClientError::EngineError { status, message })
            if status == StatusCode::CONFLICT =>
        {
            let message = format!("It can't be retired yet: {message}.");
            return render_retire(&state, &user, wallet, Some(message)).await;
        }
        Err(e) => {
            tracing::error!(error = %e, wallet = %wallet.id, "the engine could not retire a wallet");
            let message =
                "The wallet couldn't be retired right now. Nothing changed; try again in a minute."
                    .to_owned();
            return render_retire(&state, &user, wallet, Some(message)).await;
        }
    };
    let (user_id, wallet_id) = (user.id.clone(), wallet.id.clone());
    if let Err(e) = state
        .db
        .write(move |db| db.retire_wallet(&user_id, &wallet_id, retired_at))
        .await
    {
        tracing::error!(error = %e, wallet = %wallet.id, "the engine retired a wallet but it couldn't be recorded");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    redirect_303(&format!("/account/wallets/{}", wallet.id))
}

#[derive(Deserialize)]
pub struct RestoreForm {
    #[serde(default)]
    view_key_hex: String,
    #[serde(default)]
    spend_pubkey_hex: String,
    #[serde(default)]
    encrypted_keys: Option<String>,
    #[serde(default)]
    key_custody_backend: Option<String>,
}

/// `POST /account/wallets/{id}/restore`: a retired wallet back, with its
/// keys entered again; the engine checks they are this wallet's.
pub async fn restore(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<String>,
    Form(form): Form<RestoreForm>,
) -> Response {
    let Some(wallet) = load_wallet(&state, &user, &id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if wallet.retired_at.is_none() {
        return redirect_303(&format!("/account/wallets/{}", wallet.id));
    }
    let backend = form
        .key_custody_backend
        .as_deref()
        .filter(|b| !b.is_empty());
    let keys = match super::key_entry::store_keys(
        &state,
        backend,
        &form.view_key_hex,
        &form.spend_pubkey_hex,
        form.encrypted_keys.as_deref(),
    ) {
        Ok((_, keys)) => keys,
        Err(message) => return render_restore(&state, &user, wallet, Some(message)).await,
    };
    match state
        .engine
        .client
        .restore_wallet(&wallet.engine_wallet_id, &keys, backend)
        .await
    {
        Ok(_) => {}
        Err(crate::engine_client::EngineClientError::EngineError { status, message })
            if status == StatusCode::BAD_REQUEST =>
        {
            return render_restore(&state, &user, wallet, Some(message)).await;
        }
        Err(e) => {
            tracing::error!(error = %e, wallet = %wallet.id, "the engine could not bring a wallet back");
            let message =
                "The wallet couldn't be brought back right now. Try again in a minute.".to_owned();
            return render_restore(&state, &user, wallet, Some(message)).await;
        }
    }
    let (user_id, wallet_id) = (user.id.clone(), wallet.id.clone());
    if let Err(e) = state
        .db
        .write(move |db| db.restore_wallet(&user_id, &wallet_id, crate::now_unix()))
        .await
    {
        tracing::error!(error = %e, wallet = %wallet.id, "the engine brought a wallet back but it couldn't be recorded");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    redirect_303(&format!("/account/wallets/{}?restored=1", wallet.id))
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
                "/account/wallets/import",
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

    /// "Which app is it in?" is saved and its page says so; an app the form
    /// doesn't offer is refused, the keys kept for another go.
    #[tokio::test]
    async fn a_brought_in_wallet_records_which_app_it_is_in() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let (cookie, _) = sign_up(&router, "app@example.com", None).await;
        let form = body_text(get(&router, "/account/wallets/import", &cookie).await).await;
        assert!(form.contains("Which app is it in? "), "{form}");
        assert!(
            form.contains(r#"<option value="" selected>Not saying</option>"#),
            "{form}"
        );
        assert!(
            form.contains(r#"<option value="feather" data-label="Feather" data-logo="/static/wallet-logos/feather."#),
            "{form}"
        );
        assert!(
            form.contains(r#"<option value="other">Other</option>"#),
            "{form}"
        );

        let fields = |app: &'static str| {
            vec![
                ("name", "Feather till"),
                ("view_key_hex", TEST_VIEW_KEY_HEX),
                ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                ("network", "mainnet"),
                ("app", app),
            ]
        };
        let refused = router
            .clone()
            .oneshot(post(
                "/account/wallets/import",
                Some(&cookie),
                &fields("dropbox"),
            ))
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::OK);
        let html = body_text(refused).await;
        assert!(
            html.contains("That wallet app isn't one this page offers."),
            "{html}"
        );
        assert!(wallets_of(&state, "app@example.com").is_empty());

        let added = router
            .clone()
            .oneshot(post(
                "/account/wallets/import",
                Some(&cookie),
                &fields("feather"),
            ))
            .await
            .unwrap();
        assert_eq!(added.status(), StatusCode::SEE_OTHER);
        let wallet = wallets_of(&state, "app@example.com").remove(0).wallet;
        assert_eq!(wallet.app.as_deref(), Some("feather"));
        assert_eq!(wallet.backup, None);
        let ready = body_text(get(&router, &location(&added), &cookie).await).await;
        assert!(ready.contains("Brought in from Feather"), "{ready}");
        let page =
            body_text(get(&router, &format!("/account/wallets/{}", wallet.id), &cookie).await)
                .await;
        assert!(
            page.contains("<strong>Brought in from Feather</strong><p class=\"hint\">Its keys and recovery phrase live in Feather."),
            "{page}"
        );
        assert!(
            page.contains("<span>Brought in from Feather</span>"),
            "the history too: {page}"
        );
    }

    #[tokio::test]
    async fn adding_a_wallet_names_it_first_and_creating_one_waits_for_javascript() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state);
        let (cookie, _) = sign_up(&router, "first@example.com", None).await;

        let html = body_text(get(&router, "/account/wallets/add", &cookie).await).await;
        assert!(html.contains("<h1>Add a wallet</h1>"), "{html}");
        // The wallet's own small steps, without setup's.
        assert!(
            html.contains(r#"<li aria-current="step"><span class="pill">Kind</span></li>"#),
            "{html}"
        );
        assert!(!html.contains("Setup progress"));
        // The name comes first, already a free one, checked.
        let name_at = html.find(r#"name="name""#).unwrap();
        assert!(name_at < html.find("Create a new wallet").unwrap());
        assert!(html.contains("Free to use"), "{html}");
        // Drawn unavailable, with the reason, until the script turns it on.
        assert!(html.contains("pick-card recommended unavailable"));
        assert!(
            html.contains(r#"formaction="/account/wallets/new" disabled"#),
            "{html}"
        );
        assert!(html.contains("Creating a new wallet needs JavaScript"));
        assert!(
            html.contains("Coming soon"),
            "hardware wallets aren't there yet"
        );
        assert!(
            html.contains(r#"formaction="/account/wallets/import""#),
            "bring your own works without it"
        );
        assert!(
            !html.contains("Use a wallet you already added"),
            "only setup offers one already added"
        );
    }

    /// The name is checked before anything is made: a name in use sends the
    /// merchant back to the choice screen, before any phrase exists.
    #[tokio::test]
    async fn a_name_in_use_is_refused_before_any_phrase_is_made() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let (cookie, _) = sign_up(&router, "names@example.com", None).await;
        bring_in(&router, &cookie, "Till").await;

        let check =
            body_text(get(&router, "/account/wallets/name-check?name=till", &cookie).await).await;
        assert!(check.contains(r#"class="field-check bad""#), "{check}");
        assert!(
            check.contains("You already have a wallet called till"),
            "{check}"
        );
        let check =
            body_text(get(&router, "/account/wallets/name-check?name=Market", &cookie).await).await;
        assert!(check.contains("Free to use"), "{check}");

        for screen in ["/account/wallets/new", "/account/wallets/import"] {
            let html = body_text(
                get(
                    &router,
                    &format!("{screen}?name=Till&network=mainnet"),
                    &cookie,
                )
                .await,
            )
            .await;
            assert!(html.contains("<h1>Add a wallet</h1>"), "{screen}: {html}");
            assert!(
                html.contains("You already have a wallet called Till"),
                "{html}"
            );
            assert!(
                !html.contains("data-wallet-setup"),
                "no phrase is made: {html}"
            );
        }
        // A free name goes on, and isn't asked again.
        let html = body_text(
            get(
                &router,
                "/account/wallets/import?name=Market&network=stagenet",
                &cookie,
            )
            .await,
        )
        .await;
        assert!(html.contains("<h1>Bring your own wallet</h1>"));
        assert!(
            html.contains(r#"<input type="hidden" name="name" value="Market">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="network" value="stagenet">"#),
            "{html}"
        );
        assert!(
            !html.contains(r#"<select name="network""#),
            "the network isn't asked twice"
        );
        // The CLI help names the secret view key line and the public spend key line.
        assert!(
            html.contains("Copy the <strong>secret</strong> one"),
            "{html}"
        );
        assert!(
            html.contains("Copy the <strong>public</strong> one only"),
            "{html}"
        );
        assert!(html.contains("restore_height"));
    }

    /// A name taken between checking it and adding the wallet gets a number,
    /// so a phrase already backed up is never refused for its name.
    #[tokio::test]
    async fn a_name_taken_meanwhile_gets_a_number_instead_of_losing_the_backup() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let (cookie, _) = sign_up(&router, "race@example.com", None).await;
        bring_in(&router, &cookie, "Till").await;
        let wallet = made_in_the_browser(40);
        let added = router
            .clone()
            .oneshot(post(
                "/account/wallets/new",
                Some(&cookie),
                &[
                    ("name", "Till"),
                    ("network", "mainnet"),
                    ("backup", "feather"),
                    ("primary_address", &wallet.address),
                    ("view_key_hex", &wallet.view_key_hex),
                    ("spend_pubkey_hex", &wallet.spend_pubkey_hex),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(added.status(), StatusCode::SEE_OTHER);
        let names: Vec<String> = wallets_of(&state, "race@example.com")
            .into_iter()
            .map(|w| w.wallet.name)
            .collect();
        assert!(names.contains(&"Till (2)".to_owned()), "{names:?}");
        let made = wallets_of(&state, "race@example.com")
            .into_iter()
            .find(|w| w.wallet.name == "Till (2)")
            .unwrap()
            .wallet;
        assert_eq!(
            made.backup.as_deref(),
            Some("feather"),
            "Feather is recorded as Feather"
        );
    }

    #[tokio::test]
    async fn a_brought_in_wallet_is_registered_named_and_listed() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let (cookie, _) = sign_up(&router, "byo@example.com", None).await;

        let added = bring_in(&router, &cookie, "  Market   stall ").await;
        assert_eq!(added.status(), StatusCode::SEE_OTHER);
        let page = location(&added);
        assert!(page.ends_with("?added=1"), "{page}");

        let wallets = wallets_of(&state, "byo@example.com");
        assert_eq!(wallets.len(), 1);
        let wallet = &wallets[0].wallet;
        assert_eq!(wallet.name, "Market stall");
        assert_eq!(wallet.origin, crate::db::WalletOrigin::Imported);
        assert!(wallet.primary_address.starts_with('4'));

        let html = body_text(get(&router, &page, &cookie).await).await;
        assert!(html.contains("Market stall is added"), "{html}");

        let list = body_text(get(&router, "/account?tab=wallets", &cookie).await).await;
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

        let page = get(&router, "/account/wallets/new?name=Copper%20Heron", &cookie).await;
        assert_eq!(page.headers()["cache-control"], "no-store");
        let html = body_text(page).await;
        assert!(
            html.contains(r#"data-wallet-setup"#) && html.contains(r#"data-name="Copper Heron""#)
        );
        assert!(html.contains("Back up Copper Heron"));
        assert!(html.contains(&crate::assets::url("wallet-logos/cake.png")));
        assert!(
            !html.contains(r#"name="phrase""#) && !html.contains(r#"name="seed""#),
            "the form posts no phrase field at all: {html}"
        );

        let wallet = made_in_the_browser(11);
        let added = router
            .clone()
            .oneshot(post(
                "/account/wallets/new",
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
                "/account/wallets/new",
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

        for (name, site) in [("One", "one.example.com"), ("Two", "two.example.com")] {
            let response = crate::http::test_support::set_up_store_on_wallet(
                &router, &cookie, name, site, &wallet_id,
            )
            .await;
            crate::http::test_support::store_made(&response);
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
            body_text(get(&router, &format!("/account/wallets/{wallet_id}"), &cookie).await).await;
        // Each store by its name, its site under it.
        assert!(
            html.contains(r#">One</a><span class="sr-host">one.example.com</span>"#)
                && html.contains(r#">Two</a><span class="sr-host">two.example.com</span>"#),
            "{html}"
        );
        assert!(html.contains("2 take payments into this wallet"), "{html}");
        // It can't be retired yet: the checklist names both stores.
        let retire = body_text(
            get(
                &router,
                &format!("/account/wallets/{wallet_id}/retire"),
                &cookie,
            )
            .await,
        )
        .await;
        assert!(
            retire.contains("One does · ") && retire.contains("Two does · "),
            "{retire}"
        );
        assert!(
            retire.contains("Retire becomes available when both are ticked."),
            "{retire}"
        );
        assert!(
            retire.contains(r#"<button type="button" class="btn-danger" disabled"#),
            "{retire}"
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
        let page = format!("/account/wallets/{id}");

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
        // The Details card, a settings form of its own, says it was saved.
        assert!(
            html.contains(&format!(r#"<mk-settings-form label="Details"><form method="post" action="/account/wallets/{id}/rename" id="settings-form">"#)),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<mk-settings-card id="card-details" class="settings-card" name="details""#
            ),
            "{html}"
        );
        assert!(html.contains("data-card-saved>Saved<"), "{html}");
        assert!(html.contains("<strong>Renamed</strong>"), "{html}");

        // A refused name: the card says why and keeps what was typed.
        let refused = router
            .clone()
            .oneshot(post(
                &format!("{page}/rename"),
                Some(&cookie),
                &[("name", "  ")],
            ))
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let html = body_text(refused).await;
        assert!(
            html.contains(r#"<mk-settings-card id="card-details" class="settings-card is-failed""#),
            "{html}"
        );
        assert!(html.contains("A wallet needs a name."), "{html}");
        assert!(
            html.contains(r#"name="name" value="  " maxlength"#),
            "{html}"
        );
        assert!(html.contains(r#"data-saved="New name""#), "{html}");
        assert!(
            html.contains(&format!(r#"<a class="btn btn-danger" href="{page}/retire" data-opens-dialog="retire-dialog">Retire wallet…</a>"#)),
            "{html}"
        );
        // No store uses it and no order is open: the name is asked for,
        // in the dialog and on the page without JavaScript alike.
        let form = format!(r#"<form method="post" action="{page}/retire">"#);
        assert!(html.contains(&form), "{html}");
        let retire_page = body_text(get(&router, &format!("{page}/retire"), &cookie).await).await;
        assert!(retire_page.contains(&form), "{retire_page}");
        assert!(
            retire_page.contains("<h1 id=\"retire-title\" class=\"dialog-title\">Retire \u{201c}New name\u{201d}?</h1>"),
            "{retire_page}"
        );

        let wrong = router
            .clone()
            .oneshot(post(
                &format!("{page}/retire"),
                Some(&cookie),
                &[("confirm", "nope")],
            ))
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::OK);
        let html = body_text(wrong).await;
        assert!(
            html.contains("Type \u{201c}New name\u{201d} exactly to retire it.")
                && html.contains("id=\"retire-confirm\""),
            "the retire page again, saying why: {html}"
        );
        assert_eq!(wallets_of(&state, "detail@example.com").len(), 1);

        let retired = router
            .clone()
            .oneshot(post(
                &format!("{page}/retire"),
                Some(&cookie),
                &[("confirm", "New name")],
            ))
            .await
            .unwrap();
        assert_eq!(retired.status(), StatusCode::SEE_OTHER);
        assert!(
            wallets_of(&state, "detail@example.com").is_empty(),
            "offered nowhere"
        );
        let html = body_text(get(&router, &page, &cookie).await).await;
        assert!(
            html.contains(r#"<p class="wallet-meta"><span class="tag tag-unknown">retired</span><span>Keys deleted "#)
                && html.contains(". History kept.</span></p>"),
            "{html}"
        );
        assert!(
            html.contains("Retired: its keys were deleted from key storage"),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"<a class="btn" href="{page}/restore" data-opens-dialog="restore-dialog">Restore wallet…</a>"#)),
            "{html}"
        );
        assert!(html.contains("Restore \u{201c}New name\u{201d}"), "{html}");
        assert!(!html.contains("Retire wallet"), "{html}");
        // Without JavaScript: the same form on a page of its own.
        let restore_page = body_text(get(&router, &format!("{page}/restore"), &cookie).await).await;
        assert!(
            restore_page.contains(&format!(r#"<form method="post" action="{page}/restore">"#))
                && restore_page.contains("name=\"view_key_hex\""),
            "{restore_page}"
        );
        // Retired, there is no retire page; not retired, no restore page.
        assert_eq!(
            location(&get(&router, &format!("{page}/retire"), &cookie).await),
            page
        );
        let list = body_text(get(&router, "/account?tab=wallets", &cookie).await).await;
        assert!(list.contains("Retired wallets (1)"), "{list}");
        // Its keys again: it's this one, retired, to be brought back.
        let again = body_text(bring_in(&router, &cookie, "Back again").await).await;
        assert!(again.contains("You retired this wallet"), "{again}");

        let restored = router
            .clone()
            .oneshot(post(
                &format!("{page}/restore"),
                Some(&cookie),
                &[
                    ("view_key_hex", TEST_VIEW_KEY_HEX),
                    ("spend_pubkey_hex", TEST_SPEND_PUBKEY_HEX),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(restored.status(), StatusCode::SEE_OTHER);
        let html = body_text(get(&router, &location(&restored), &cookie).await).await;
        assert!(html.contains("New name is back"), "{html}");
        assert!(html.contains("Brought back with its keys"), "{html}");
        assert_eq!(
            location(&get(&router, &format!("{page}/restore"), &cookie).await),
            page
        );
        assert_eq!(wallets_of(&state, "detail@example.com").len(), 1);
    }

    /// Another wallet's keys don't bring a retired one back.
    #[tokio::test]
    async fn a_retired_wallet_comes_back_only_with_its_own_keys() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let (cookie, _) = sign_up(&router, "restore@example.com", None).await;
        bring_in(&router, &cookie, "Till").await;
        let id = wallets_of(&state, "restore@example.com")[0]
            .wallet
            .id
            .to_string();
        let page = format!("/account/wallets/{id}");
        let retired = router
            .clone()
            .oneshot(post(
                &format!("{page}/retire"),
                Some(&cookie),
                &[("confirm", "Till")],
            ))
            .await
            .unwrap();
        assert_eq!(retired.status(), StatusCode::SEE_OTHER);
        let other = made_in_the_browser(40);
        let refused = router
            .clone()
            .oneshot(post(
                &format!("{page}/restore"),
                Some(&cookie),
                &[
                    ("view_key_hex", &other.view_key_hex),
                    ("spend_pubkey_hex", &other.spend_pubkey_hex),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::OK);
        let html = body_text(refused).await;
        assert!(html.contains("belong to a different wallet"), "{html}");
        assert!(
            html.contains(r#"<h1 id="restore-title" class="dialog-title">"#),
            "the restore page again: {html}"
        );
        assert!(wallets_of(&state, "restore@example.com").is_empty());
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
        let page = format!("/account/wallets/{id}");
        assert_eq!(
            get(&router, &page, &other).await.status(),
            StatusCode::NOT_FOUND
        );
        let retire = router
            .clone()
            .oneshot(post(
                &format!("{page}/retire"),
                Some(&other),
                &[("confirm", "Mine")],
            ))
            .await
            .unwrap();
        assert_eq!(retire.status(), StatusCode::NOT_FOUND);
        assert_eq!(wallets_of(&state, "owner@example.com").len(), 1);
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
        crate::http::test_support::store_made(
            &crate::http::test_support::set_up_store_on_wallet(
                &router,
                &cookie,
                "Old shop",
                "old.example.com",
                &id,
            )
            .await,
        );
        // As it was before wallets: no wallet on the store, none recorded.
        {
            let db = state.db.lock();
            db.conn_for_test()
                .execute_batch(
                    "UPDATE store_connections SET wallet_id = NULL; DELETE FROM wallets;",
                )
                .unwrap();
        }
        let html = body_text(get(&router, "/account?tab=wallets", &cookie).await).await;
        assert!(html.contains("Brought in"), "{html}");
        let wallets = wallets_of(&state, "legacy@example.com");
        assert_eq!(wallets.len(), 1);
        assert_eq!(wallets[0].store_count, 1);
    }

    // -- Changing a store's wallet ------------------------------------------

    /// Brings in a second wallet, with keys of its own.
    async fn bring_in_another(router: &Router, cookie: &str, name: &str, seed: u8) {
        let made = made_in_the_browser(seed);
        let response = router
            .clone()
            .oneshot(post(
                "/account/wallets/import",
                Some(cookie),
                &[
                    ("name", name),
                    ("view_key_hex", &made.view_key_hex),
                    ("spend_pubkey_hex", &made.spend_pubkey_hex),
                    ("network", "mainnet"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER, "{name}");
    }

    /// A store on `wallet`, its id.
    async fn connect_store(router: &Router, cookie: &str, site: &str, wallet: &str) -> String {
        let response =
            crate::http::test_support::set_up_store_on_wallet(router, cookie, site, site, wallet)
                .await;
        crate::http::test_support::store_made(&response)
    }

    fn wallet_named(state: &AppState, email: &str, name: &str) -> crate::db::WalletRow {
        wallets_of(state, email)
            .into_iter()
            .find(|w| w.wallet.name == name)
            .unwrap()
            .wallet
    }

    fn store_row(state: &AppState, id: &str) -> crate::db::StoreConnectionRow {
        state
            .db
            .lock()
            .get_store_connection_by_id(&crate::db::ConnectionId::new(id.to_owned()))
            .unwrap()
            .unwrap()
    }

    /// The whole change, without JavaScript: the store's settings show its
    /// wallet as Current; picking another asks first, counting the open
    /// order that stays behind; confirming changes it in the engine and
    /// here, and both wallets and the store's history say so.
    #[tokio::test]
    async fn changing_a_stores_wallet_asks_first_then_records_it_everywhere() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let email = "changer@example.com";
        let (cookie, _) = sign_up(&router, email, None).await;
        bring_in(&router, &cookie, "Copper Heron").await;
        bring_in_another(&router, &cookie, "Cafe till", 30).await;
        let copper = wallet_named(&state, email, "Copper Heron");
        let cafe = wallet_named(&state, email, "Cafe till");
        let store = connect_store(
            &router,
            &cookie,
            "https://shop.example.com",
            copper.id.as_str(),
        )
        .await;
        let sk =
            super::super::orders::decrypt_sk(&state.encryption_key, &store_row(&state, &store))
                .unwrap();
        let open = state
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

        let settings = format!("/dashboard/stores/{store}/settings");
        let html = body_text(get(&router, &settings, &cookie).await).await;
        assert!(
            html.contains(r#"<section id="card-wallet" class="settings-card""#),
            "{html}"
        );
        assert!(html.contains("Payments go to"), "{html}");
        assert!(
            html.contains(&format!(
                r#"value="{}" selected data-label="Copper Heron""#,
                copper.id
            )),
            "{html}"
        );
        assert!(
            html.contains(r#"data-chip="Current" data-chip-tone="current""#),
            "{html}"
        );

        let action = format!("/dashboard/stores/{store}/settings/wallet");
        let asked = router
            .clone()
            .oneshot(post(
                &action,
                Some(&cookie),
                &[("wallet_id", cafe.id.as_str())],
            ))
            .await
            .unwrap();
        assert_eq!(asked.status(), StatusCode::OK);
        let html = body_text(asked).await;
        assert!(html.contains("Change to Cafe till?"), "{html}");
        assert!(
            html.contains("The 1 order still open on Copper Heron keeps being paid into it"),
            "{html}"
        );
        assert_eq!(
            store_row(&state, &store).wallet_id.as_ref(),
            Some(&copper.id),
            "only asked"
        );

        let changed = router
            .clone()
            .oneshot(post(
                &action,
                Some(&cookie),
                &[("wallet_id", cafe.id.as_str()), ("confirm", "yes")],
            ))
            .await
            .unwrap();
        assert_eq!(changed.status(), StatusCode::OK);
        let html = body_text(changed).await;
        assert!(
            html.contains(
                "Changed to Cafe till. The 1 order it took on Copper Heron is still watched there."
            ),
            "{html}"
        );
        assert!(html.contains("<strong>Wallet changed</strong>"), "{html}");
        assert_eq!(store_row(&state, &store).wallet_id.as_ref(), Some(&cafe.id));
        let tenant = state.engine.client.get_tenant(&sk).await.unwrap();
        assert_eq!(tenant.wallet_id.as_ref(), Some(&cafe.engine_wallet_id));
        assert_eq!(tenant.primary_address, cafe.primary_address);

        let periods = state
            .db
            .lock()
            .store_wallet_periods(&crate::db::ConnectionId::new(store.clone()))
            .unwrap();
        assert_eq!(periods.len(), 2);
        assert_eq!(periods[0].wallet_id.as_ref(), Some(&cafe.id));
        assert_eq!(periods[0].until, None);
        assert_eq!(periods[1].wallet_id.as_ref(), Some(&copper.id));
        assert_eq!(periods[1].until, Some(periods[0].from));

        let html = body_text(get(&router, &settings, &cookie).await).await;
        assert!(html.contains("Wallet history (2 wallets)"), "{html}");
        assert!(
            html.contains(r#"<tr class="current" aria-current="true">"#),
            "{html}"
        );
        // The open order stays on Copper Heron, which can't go yet.
        let copper_page =
            body_text(get(&router, &format!("/account/wallets/{}", copper.id), &cookie).await)
                .await;
        assert!(
            copper_page.contains("changed to another wallet"),
            "{copper_page}"
        );
        assert!(copper_page.contains("Before"), "{copper_page}");
        let cafe_page =
            body_text(get(&router, &format!("/account/wallets/{}", cafe.id), &cookie).await).await;
        assert!(cafe_page.contains("changed to this wallet"), "{cafe_page}");
        let refused = router
            .clone()
            .oneshot(post(
                &format!("/account/wallets/{}/retire", copper.id),
                Some(&cookie),
                &[("confirm", "Copper Heron")],
            ))
            .await
            .unwrap();
        let html = body_text(refused).await;
        assert!(html.contains("can still be paid into it"), "{html}");
        // The checklist says so before anyone tries: Retire is off.
        assert!(
            html.contains(r#"No order on it can still be paid<span class="fix">until about "#),
            "{html}"
        );
        assert!(html.contains(r#"<li class="ok"><span class="mark" aria-hidden="true">✓</span><span><span class="visually-hidden">Done: </span>No store takes payments into it"#), "{html}");
        assert!(wallets_of(&state, email)
            .iter()
            .any(|w| w.wallet.id == copper.id));

        // New orders get the new wallet's addresses.
        let after = state
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
        assert_ne!(after.address, open.address);
    }

    /// Picking a wallet asks first, in the Wallet card; refusals show in
    /// the card.
    #[tokio::test]
    async fn the_wallet_card_asks_first_and_refuses_another_network() {
        let (state, _engine) = real_engine_state().await;
        let router = build_router(state.clone());
        let email = "fixi-changer@example.com";
        let (cookie, _) = sign_up(&router, email, None).await;
        bring_in(&router, &cookie, "Copper Heron").await;
        bring_in_another(&router, &cookie, "Cafe till", 31).await;
        let copper = wallet_named(&state, email, "Copper Heron");
        let cafe = wallet_named(&state, email, "Cafe till");
        let store = connect_store(
            &router,
            &cookie,
            "https://fixi.example.com",
            copper.id.as_str(),
        )
        .await;
        let action = format!("/dashboard/stores/{store}/settings/wallet");
        let card_post = |fields: &[(&str, &str)]| post(&action, Some(&cookie), fields);

        let response = router
            .clone()
            .oneshot(card_post(&[("wallet_id", cafe.id.as_str())]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains(r#"<section id="card-wallet""#), "{html}");
        assert!(html.contains("Change to Cafe till?"), "{html}");
        assert!(
            html.contains("No orders are open on Copper Heron"),
            "{html}"
        );

        // Picking the current one again asks nothing.
        let html = body_text(
            router
                .clone()
                .oneshot(card_post(&[("wallet_id", copper.id.as_str())]))
                .await
                .unwrap(),
        )
        .await;
        assert!(!html.contains("change-confirm"), "{html}");

        // A stagenet wallet: not offered, and refused if sent.
        let stagenet = crate::db::WalletId::new("w_stagenet".to_owned());
        {
            let db = state.db.lock();
            let user = db.get_user_by_email(email).unwrap().unwrap();
            db.create_wallet(&crate::db::NewWalletRow {
                id: &stagenet,
                user_id: &user.id,
                name: "Quiet Lantern",
                network: "stagenet",
                primary_address: "5stagenetaddress",
                engine_wallet_id: &crate::db::EngineWalletId::new("wl_x".to_owned()),
                origin: crate::db::WalletOrigin::Imported,
                backup: None,
                app: None,
                created_at: 1,
            })
            .unwrap();
        }
        let page = body_text(
            get(
                &router,
                &format!("/dashboard/stores/{store}/settings"),
                &cookie,
            )
            .await,
        )
        .await;
        assert!(!page.contains("w_stagenet"), "{page}");
        assert!(!page.contains("Quiet Lantern"), "{page}");
        assert!(
            page.contains("A store can only change to a wallet on its own network."),
            "{page}"
        );
        assert!(
            page.contains(r#"data-network="mainnet""#),
            "the store's own network's wallets carry its badge: {page}"
        );
        let response = router
            .clone()
            .oneshot(card_post(&[
                ("wallet_id", "w_stagenet"),
                ("confirm", "yes"),
            ]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let html = body_text(response).await;
        assert!(html.contains("network can"), "{html}");
        assert_eq!(
            store_row(&state, &store).wallet_id.as_ref(),
            Some(&copper.id)
        );

        // Someone else's wallet, or none: refused the same way.
        let response = router
            .clone()
            .oneshot(card_post(&[("wallet_id", "w_nope"), ("confirm", "yes")]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_text(response)
            .await
            .contains("Choose one of your wallets"));
    }
}
