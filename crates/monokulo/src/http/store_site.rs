//! A store's deletion and the plugins connected to it (`views::store_site`):
//! deleting the store and disconnecting a plugin, each behind its checklist
//! dialog, or the page holding it without JavaScript.
//!
//! - `GET`/`POST /stores/{id}/settings/delete`: refused while an order can
//!   still be paid. The engine forgets the store, then its rows here are
//!   deleted, history and webhooks with them.
//! - `GET`/`POST /stores/{id}/settings/plugins/{integration}/disconnect`:
//!   refused while an order the plugin made can still be paid. Gives the
//!   store a new secret key in the engine, so the plugin's stops working,
//!   then removes the plugin's webhook and marks it disconnected in one
//!   write.

use std::collections::HashSet;

use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::db::{ConnectionId, StoreIntegrationRow, UserRow};
use crate::views::store_settings::StoreSection;
use crate::views::store_site::{IntegrationView, OpenOrders, SiteView};

use super::orders::{decrypt_sk, load_owned_connection};
use super::{AppState, AuthedUser, OwnedStore};

/// What a connected plugin is called: the name it gave, else its slug.
pub(super) fn integration_name(row: &StoreIntegrationRow) -> String {
    row.plugin().to_string()
}

fn integration_view(
    row: &StoreIntegrationRow,
    clock: &crate::views::time::Clock,
) -> IntegrationView {
    IntegrationView {
        id: row.id.clone(),
        name: integration_name(row),
        site: row.site.clone(),
        version: row.version.clone(),
        connected: clock.text(row.connected_at),
        last_order: row.last_seen_at.map(|at| clock.text(at)),
        until: row.disconnected_at.map(|at| clock.text(at)),
        webhook_url: row.webhook_url.clone(),
    }
}

/// Of the store's orders that can still be paid (`listed`, the engine's
/// open orders, `None` when it couldn't say), those in `only` when given.
/// That is every order the engine holds open: waiting or part paid, and
/// also paid but still confirming, which goes on to paid whatever its
/// deadline. `until` is the latest deadline not yet passed.
fn open_orders(
    listed: Option<&[crate::engine_client::OrderView]>,
    only: Option<&HashSet<crate::db::OrderId>>,
    clock: &crate::views::time::Clock,
) -> Option<OpenOrders> {
    let orders = listed?;
    let now = clock.now();
    let open: Vec<_> = orders
        .iter()
        .filter(|o| only.is_none_or(|ids| ids.contains(&o.order_id)))
        .collect();
    Some(OpenOrders {
        count: open.len() as u64,
        until: open
            .iter()
            .map(|o| o.expires_at)
            .filter(|at| clock::UnixTime::from_secs(*at) > now)
            .max()
            .map(|at| clock.text(at)),
    })
}

/// The website and integrations parts of `row`'s settings page.
pub(super) async fn site_view(
    state: &AppState,
    row: &crate::db::StoreConnectionRow,
    sk: &credentials::auth::RawToken,
    clock: &crate::views::time::Clock,
) -> SiteView {
    let id = row.id.clone();
    let (integrations, from_plugin) = state
        .db
        .read(move |db| {
            let integrations = db.list_integrations(&id)?;
            // The active plugin's orders, which disconnecting it waits on.
            let from_plugin = match integrations.iter().find(|i| i.disconnected_at.is_none()) {
                Some(active) => db.order_ids_from_source(&id, &active.kind)?,
                None => Default::default(),
            };
            Ok::<_, crate::db::DbError>((integrations, from_plugin))
        })
        .await
        .unwrap_or_default();
    let active = integrations.iter().find(|i| i.disconnected_at.is_none());
    let listed = state.engine.client.list_open_orders(sk).await.ok();
    let open = open_orders(listed.as_deref(), None, clock);
    let plugin_open = match active {
        Some(_) => open_orders(listed.as_deref(), Some(&from_plugin), clock),
        None => None,
    };
    SiteView {
        store_id: row.id.to_string(),
        store_name: row.name.clone(),
        site: crate::stores::stored_site(row.site.as_ref()).to_owned(),
        active: active.map(|i| integration_view(i, clock)),
        past: integrations
            .iter()
            .filter(|i| i.disconnected_at.is_some())
            .map(|i| integration_view(i, clock))
            .collect(),
        open_orders: open,
        plugin_open_orders: plugin_open,
    }
}

/// The store, its key and what its settings page shows of its site.
async fn load(
    state: &AppState,
    user: &UserRow,
    id: &ConnectionId,
    path: String,
) -> Result<
    (
        OwnedStore,
        credentials::auth::RawToken,
        SiteView,
        crate::views::PageChrome,
    ),
    StatusCode,
> {
    let row = match load_owned_connection(&state.db, user, id).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(()) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let sk =
        decrypt_sk(&state.encryption_key, &row).map_err(|()| StatusCode::INTERNAL_SERVER_ERROR)?;
    let chrome = super::page_chrome(state, Some(user), path).await;
    let view = site_view(state, &row, &sk, &chrome.clock).await;
    Ok((row, sk, view, chrome))
}

fn something_went_wrong() -> String {
    "Something went wrong saving this. Nothing changed; try again in a minute.".to_owned()
}

// -- Deleting the store ----------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct DeleteForm {
    #[serde(default)]
    confirm: String,
}

fn delete_path(id: &ConnectionId) -> String {
    format!("/stores/{id}/settings/delete")
}

/// `GET /stores/{id}/settings/delete`: the delete dialog as a page.
pub async fn delete_form(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
) -> Response {
    let (_, _, view, chrome) = match load(&state, &user, &id, delete_path(&id)).await {
        Ok(loaded) => loaded,
        Err(status) => return status.into_response(),
    };
    crate::views::store_site::delete_page(&chrome, &view, None).into_response()
}

/// `POST /stores/{id}/settings/delete`: deletes the store, once no order
/// can still be paid and its name was typed. The engine forgets the store
/// first (its orders and keys), then the store's rows here go in one
/// write: its history, webhooks, domains, apps and plugins with them. The
/// wallet it paid into is the merchant's and stays.
pub async fn delete_submit(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
    Form(form): Form<DeleteForm>,
) -> Response {
    let (row, sk, view, chrome) = match load(&state, &user, &id, delete_path(&id)).await {
        Ok(loaded) => loaded,
        Err(status) => return status.into_response(),
    };
    let refuse = |message: String| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::views::store_site::delete_page(&chrome, &view, Some(&message)),
        )
            .into_response()
    };
    match &view.open_orders {
        None => {
            return refuse("Monokulo can't check the store's orders right now. Nothing changed; try again in a minute.".to_owned())
        }
        Some(open) if open.count > 0 => {
            return refuse(format!(
                "{} can still be paid. Delete the store once none can.",
                if open.count == 1 { "1 order".to_owned() } else { format!("{} orders", open.count) },
            ))
        }
        Some(_) => {}
    }
    if form.confirm.trim() != row.name {
        return refuse(format!(
            "Type \u{201c}{}\u{201d} exactly to delete it.",
            row.name
        ));
    }
    if let Err(e) = state.engine.client.delete_tenant(&sk).await {
        tracing::error!(store.id = %id, error = %e, "the engine could not delete the store");
        return refuse(something_went_wrong());
    }
    // The engine refuses the key now: what was opened with it ends.
    state.revocations.revoke(&sk.hash());
    let store_id = row.id.clone();
    if let Err(e) = state.db.write(move |db| db.delete_store(&store_id)).await {
        // The engine has already forgotten the store: nothing to fall back
        // on, and nothing a retry could do but this same delete.
        tracing::error!(store.id = %id, error = %e, "the store's rows could not be deleted");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    tracing::info!(store.id = %id, "store deleted");
    crate::base::see_other("/")
}

// -- Disconnecting a plugin ------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct DisconnectForm {
    #[serde(default)]
    confirm: String,
}

fn disconnect_path(id: &ConnectionId, integration: &str) -> String {
    format!("/stores/{id}/settings/plugins/{integration}/disconnect")
}

/// `GET …/settings/plugins/{integration}/disconnect`: C1 as a page. One
/// that isn't the store's active plugin: back to its settings.
pub async fn disconnect_form(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, integration)): Path<(ConnectionId, String)>,
) -> Response {
    let (_, _, view, chrome) =
        match load(&state, &user, &id, disconnect_path(&id, &integration)).await {
            Ok(loaded) => loaded,
            Err(status) => return status.into_response(),
        };
    let Some(active) = view.active.as_ref().filter(|a| a.id == integration) else {
        return crate::base::see_other(format!(
            "/stores/{id}/settings?tab=integrations#card-integrations"
        ));
    };
    crate::views::store_site::disconnect_page(&chrome, &view, active, None).into_response()
}

/// `POST …/settings/plugins/{integration}/disconnect`: once no order
/// the plugin made can still be paid and the store's name was typed,
/// gives the store a new secret key (the plugin's stops working), then
/// removes its webhook and marks it disconnected.
pub async fn disconnect_submit(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, integration)): Path<(ConnectionId, String)>,
    Form(form): Form<DisconnectForm>,
) -> Response {
    let (row, sk, view, chrome) =
        match load(&state, &user, &id, disconnect_path(&id, &integration)).await {
            Ok(loaded) => loaded,
            Err(status) => return status.into_response(),
        };
    let Some(active) = view.active.as_ref().filter(|a| a.id == integration) else {
        return crate::base::see_other(format!(
            "/stores/{id}/settings?tab=integrations#card-integrations"
        ));
    };
    let refuse = |message: String| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::views::store_site::disconnect_page(&chrome, &view, active, Some(&message)),
        )
            .into_response()
    };
    match &view.plugin_open_orders {
        None => return refuse("Monokulo can't check the shop's orders right now. Nothing changed; try again in a minute.".to_owned()),
        Some(open) if open.count > 0 => {
            return refuse(format!(
                "{} from {} can still be paid. Disconnect it once none can.",
                if open.count == 1 { "1 order".to_owned() } else { format!("{} orders", open.count) },
                active.site
            ))
        }
        Some(_) => {}
    }
    if form.confirm.trim() != row.name {
        return refuse(format!(
            "Type \u{201c}{}\u{201d} exactly to disconnect it.",
            row.name
        ));
    }
    // The engine's new key first: until it has one, nothing here changes,
    // so a refusal's "Nothing changed" is true.
    let new_sk = match state.engine.client.rotate_secret(&sk).await {
        Ok(new_sk) => new_sk,
        Err(e) => {
            tracing::error!(store.id = %id, error = %e, "the store's secret key could not be rotated");
            return refuse(something_went_wrong());
        }
    };
    let encrypted = crate::crypto::encrypt(
        &state.encryption_key,
        crate::crypto::Binding::StoreSecret(row.id.as_str()),
        new_sk.expose(),
    );
    let new_hash = new_sk.hash();
    let (store_id, integration_id) = (row.id.clone(), integration.clone());
    let now = state.clock.now().as_secs();
    let written = state
        .db
        .write(move |db| {
            db.disconnect_integration(&store_id, &integration_id, &encrypted, &new_hash, now)
        })
        .await;
    // The engine refuses the old key now, written or not: what was opened
    // with it ends.
    state.revocations.revoke(&sk.hash());
    if let Err(e) = written {
        // The engine has the new key and this store can't read it: nothing
        // to fall back on.
        tracing::error!(store.id = %id, error = %e, "the store's new secret key could not be recorded");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    tracing::info!(store.id = %id, integration = %integration, "plugin disconnected");
    super::orders::saved(&id, StoreSection::Integrations)
}
