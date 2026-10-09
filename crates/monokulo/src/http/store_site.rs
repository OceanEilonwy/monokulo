//! A store's website and the plugins connected to it (`views::store_site`):
//! changing or removing the website, and disconnecting a plugin, each
//! behind its checklist dialog, or the page holding it without JavaScript.
//!
//! - `GET`/`POST /dashboard/stores/{id}/settings/website` (`?site=` the
//!   new address, or `?remove=1`): refused while a plugin is connected.
//!   The old site's domain is dropped and the new one's waits to be
//!   verified.
//! - `GET`/`POST /dashboard/stores/{id}/settings/connections/{integration}/disconnect`:
//!   refused while an order the plugin made can still be paid. Removes the
//!   plugin's webhook and gives the store a new secret key in the engine,
//!   so the plugin's stops working, then marks it disconnected; the
//!   website unlocks.

use std::collections::HashSet;

use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::db::{ConnectionId, StoreIntegrationRow, UserRow};
use crate::views::store_settings::StoreSection;
use crate::views::store_site::{IntegrationView, OpenOrders, SiteView, WebsiteChange};

use super::orders::{decrypt_sk, load_owned_connection};
use super::{AppState, AuthedUser, OwnedStore};

/// What a plugin is called.
fn integration_name(kind: &str) -> String {
    match kind {
        "woocommerce" => "WooCommerce".to_owned(),
        other => {
            let mut chars = other.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        }
    }
}

fn integration_view(
    row: &StoreIntegrationRow,
    clock: &crate::views::time::Clock,
) -> IntegrationView {
    IntegrationView {
        id: row.id.clone(),
        name: integration_name(&row.kind),
        site: row.site.clone(),
        version: row.version.clone(),
        connected: clock.text(row.connected_at),
        last_order: row.last_seen_at.map(|at| clock.text(at)),
        until: row.disconnected_at.map(|at| clock.text(at)),
        webhook_url: row.webhook_url.clone(),
    }
}

/// The store's orders that can still be paid (waiting, or part paid, and
/// not yet expired), of those in `only` when given; `None` when the
/// engine can't say.
async fn open_orders(
    state: &AppState,
    sk: &shared::auth::RawToken,
    only: Option<&HashSet<crate::db::OrderId>>,
    clock: &crate::views::time::Clock,
) -> Option<OpenOrders> {
    use shared::order_status::OrderStatus;
    let orders = state.engine.client.list_orders(sk).await.ok()?;
    let now = crate::now_unix();
    let open: Vec<_> = orders
        .iter()
        .filter(|o| matches!(o.status, OrderStatus::Pending | OrderStatus::Partial))
        .filter(|o| o.expires_at > now)
        .filter(|o| only.is_none_or(|ids| ids.contains(&o.order_id)))
        .collect();
    Some(OpenOrders {
        count: open.len() as u64,
        until: open
            .iter()
            .map(|o| o.expires_at)
            .max()
            .map(|at| clock.text(at)),
    })
}

/// The website and connections parts of `row`'s settings page.
pub(super) async fn site_view(
    state: &AppState,
    row: &crate::db::StoreConnectionRow,
    sk: &shared::auth::RawToken,
    clock: &crate::views::time::Clock,
) -> SiteView {
    let id = row.id.clone();
    let (integrations, domains, from_plugin) = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                db.list_integrations(&id)?,
                db.list_store_domains(&id)?,
                db.order_ids_from_source(&id, "woocommerce")?,
            ))
        })
        .await
        .unwrap_or_default();
    let active = integrations.iter().find(|i| i.disconnected_at.is_none());
    let site_domain = crate::embed_domains::domain_of_site(&row.site)
        .is_some_and(|domain| domains.iter().any(|d| d.domain == domain));
    let open = open_orders(state, sk, None, clock).await;
    let plugin_open = match active {
        Some(_) => open_orders(state, sk, Some(&from_plugin), clock).await,
        None => None,
    };
    SiteView {
        store_id: row.id.to_string(),
        store_name: row.name.clone(),
        site: row.site.clone(),
        active: active.map(|i| integration_view(i, clock)),
        past: integrations
            .iter()
            .filter(|i| i.disconnected_at.is_some())
            .map(|i| integration_view(i, clock))
            .collect(),
        open_orders: open,
        plugin_open_orders: plugin_open,
        site_domain,
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
        shared::auth::RawToken,
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

// -- The website ---------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct WebsiteQuery {
    #[serde(default)]
    site: Option<String>,
    #[serde(default)]
    remove: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct WebsiteForm {
    #[serde(default)]
    site: String,
    #[serde(default)]
    remove: Option<String>,
    #[serde(default)]
    confirm: String,
}

fn website_path(id: &ConnectionId) -> String {
    format!("/dashboard/stores/{id}/settings/website")
}

/// `GET /dashboard/stores/{id}/settings/website`: B1 as a page. A store
/// with no website has nothing to change: back to its settings.
pub async fn website_form(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
    Query(query): Query<WebsiteQuery>,
) -> Response {
    let (row, _, view, chrome) = match load(&state, &user, &id, website_path(&id)).await {
        Ok(loaded) => loaded,
        Err(status) => return status.into_response(),
    };
    if row.site.is_empty() {
        return super::dashboard::redirect_303(&format!(
            "/dashboard/stores/{id}/settings#card-store"
        ));
    }
    let change = match (query.remove, query.site) {
        (Some(_), _) => WebsiteChange::Remove,
        (None, site) => WebsiteChange::To(site.unwrap_or_default()),
    };
    crate::views::store_site::website_page(&chrome, &view, &change, None).into_response()
}

/// `POST /dashboard/stores/{id}/settings/website`: the website changed or
/// removed, once no plugin is connected and the store's name was typed.
pub async fn website_submit(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
    Form(form): Form<WebsiteForm>,
) -> Response {
    let (row, _, view, chrome) = match load(&state, &user, &id, website_path(&id)).await {
        Ok(loaded) => loaded,
        Err(status) => return status.into_response(),
    };
    let change = if form.remove.is_some() {
        WebsiteChange::Remove
    } else {
        WebsiteChange::To(form.site.clone())
    };
    let refuse = |message: String| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::views::store_site::website_page(&chrome, &view, &change, Some(&message)),
        )
            .into_response()
    };
    if row.site.is_empty() {
        return super::dashboard::redirect_303(&format!(
            "/dashboard/stores/{id}/settings#card-store"
        ));
    }
    if let Some(active) = &view.active {
        return refuse(format!(
            "{} is connected on {}: disconnect it first.",
            active.name, active.site
        ));
    }
    let new_site = match &change {
        WebsiteChange::Remove => String::new(),
        WebsiteChange::To(typed) => match crate::stores::normalize_site(typed) {
            Ok(site) => site,
            Err(why) => return refuse(why.to_owned()),
        },
    };
    if form.confirm.trim() != row.name {
        return refuse(format!(
            "Type \u{201c}{}\u{201d} exactly to {} its website.",
            row.name,
            if new_site.is_empty() {
                "remove"
            } else {
                "change"
            }
        ));
    }
    if new_site == row.site {
        return super::orders::saved(&id, StoreSection::Store);
    }
    if !new_site.is_empty() {
        let (user_id, wanted) = (user.id.clone(), new_site.clone());
        match state
            .db
            .read(move |db| super::connections::SiteTaken::of(db, &user_id, &wanted))
            .await
        {
            Ok(None) => {}
            Ok(Some(taken)) => return refuse(taken.message(&new_site)),
            Err(_) => return refuse(something_went_wrong()),
        }
    }
    let (store_id, name, old_site, site) = (
        row.id.clone(),
        row.name.clone(),
        row.site.clone(),
        new_site.clone(),
    );
    let written = state
        .db
        .write(move |db| {
            db.set_store_name_and_site(&store_id, &name, &site)?;
            // The old site's domain goes; the new one's waits to be verified.
            if let Some(old) = crate::embed_domains::domain_of_site(&old_site) {
                for domain in db
                    .list_store_domains(&store_id)?
                    .into_iter()
                    .filter(|d| d.domain == old)
                {
                    db.delete_store_domain(&store_id, &domain.id)?;
                }
            }
            if !site.is_empty() {
                crate::embed_domains::suggest_site_domain(db, &store_id, &site, crate::now_unix());
            }
            Ok::<_, crate::db::DbError>(())
        })
        .await;
    match written {
        Ok(()) => {
            tracing::info!(store.id = %id, from = %row.site, to = %new_site, "store website changed");
            super::orders::saved(&id, StoreSection::Store)
        }
        Err(e) if e.is_unique_violation() => {
            refuse(super::connections::SiteTaken::Someone.message(&new_site))
        }
        Err(_) => refuse(something_went_wrong()),
    }
}

// -- Disconnecting a plugin ------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct DisconnectForm {
    #[serde(default)]
    confirm: String,
}

fn disconnect_path(id: &ConnectionId, integration: &str) -> String {
    format!("/dashboard/stores/{id}/settings/connections/{integration}/disconnect")
}

/// `GET …/settings/connections/{integration}/disconnect`: C1 as a page. One
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
    if view.active.as_ref().is_none_or(|a| a.id != integration) {
        return super::dashboard::redirect_303(&format!(
            "/dashboard/stores/{id}/settings#card-connections"
        ));
    }
    crate::views::store_site::disconnect_page(&chrome, &view, None).into_response()
}

/// `POST …/settings/connections/{integration}/disconnect`: once no order
/// the plugin made can still be paid and the store's name was typed,
/// removes its webhook, gives the store a new secret key (the plugin's
/// stops working) and marks it disconnected.
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
        return super::dashboard::redirect_303(&format!(
            "/dashboard/stores/{id}/settings#card-connections"
        ));
    };
    let refuse = |message: String| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::views::store_site::disconnect_page(&chrome, &view, Some(&message)),
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
    // Its webhook first, with the key that still works.
    let (store_id, integration_id) = (row.id.clone(), integration.clone());
    let webhook_id = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>(
                db.list_integrations(&store_id)?
                    .into_iter()
                    .find(|i| i.id == integration_id)
                    .and_then(|i| i.webhook_id),
            )
        })
        .await
        .ok()
        .flatten();
    if let Some(webhook_id) = &webhook_id {
        match state.engine.client.delete_webhook(&sk, webhook_id).await {
            Ok(()) => {}
            // Already gone: removed on the Webhooks card, say.
            Err(crate::engine_client::EngineClientError::EngineError { status, .. })
                if status == StatusCode::NOT_FOUND => {}
            Err(e) => {
                tracing::error!(store.id = %id, error = %e, "the plugin's webhook could not be removed");
                return refuse(something_went_wrong());
            }
        }
    }
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
    let (store_id, integration_id) = (row.id.clone(), integration.clone());
    let written = state
        .db
        .write(move |db| {
            db.set_store_secret(&store_id, &encrypted)?;
            db.disconnect_integration(&store_id, &integration_id, crate::now_unix())
        })
        .await;
    if let Err(e) = written {
        // The engine has the new key and this store can't read it: nothing
        // to fall back on.
        tracing::error!(store.id = %id, error = %e, "the store's new secret key could not be recorded");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    tracing::info!(store.id = %id, integration = %integration, "plugin disconnected");
    super::orders::saved(&id, StoreSection::Connections)
}
