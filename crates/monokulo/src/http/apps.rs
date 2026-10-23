//! Connected apps (`crate::apps`): the dashboard's Apps card actions and
//! connect pages, and the handshake an app connects with. Once connected,
//! an app uses the API (`crate::api`) with its own key.
//!
//! ```text
//! dashboard: Connect an app ──▶ code: https://{public_uri}/apps/connect#token=ct_…
//! app:       POST /apps/connect {token, slug, name, …} ──▶ {app_key, api: "/api/v1"}
//!            GET  /api/v1                     (Authorization: Bearer ak_…): links
//!            GET  /api/v1/store, /api/v1/app, POST /api/v1/orders, GET /api/v1/events, …
//!            DELETE /api/v1/app               (disconnects itself)
//! ```
//!
//! An app connects at `/apps/connect`, outside the plugins' `/connect/{slug}`,
//! so no plugin's slug can collide with it, and a store has any number of
//! apps where it has one plugin.

use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};

use crate::apps::{clean_device_name, CONNECT_TOKEN_TTL_SECONDS};
use crate::db::{AppConnection, ConnectionId, UserRow};
use crate::now_unix;
use crate::plugins::{Plugin, PluginName, PluginSlug};
use crate::views::connected_apps::{AppRow, AppsView};

use super::orders::load_owned_connection;
use super::{AppState, AuthedUser, OwnedStore};

/// What the Apps card shows for an app.
pub(super) fn app_row(app: &AppConnection, clock: &crate::views::time::Clock) -> AppRow {
    AppRow {
        id: app.id.clone(),
        app: Plugin::stored(&app.slug, &app.name).to_string(),
        device_name: app.device_name.clone(),
        platform: app.platform.clone(),
        version: app.version.clone(),
        connected: clock.text(app.connected_at),
        last_seen: app.last_seen_at.map(|at| clock.text(at)),
    }
}

/// The store's Apps card, for its settings page.
pub(super) async fn apps_view(
    state: &AppState,
    row: &crate::db::StoreConnectionRow,
    clock: &crate::views::time::Clock,
) -> AppsView {
    let id = row.id.clone();
    let apps = state
        .db
        .read(move |db| db.list_apps(&id))
        .await
        .inspect_err(|e| tracing::warn!(store.id = %row.id, error = %e, "the store's apps could not be read"))
        .unwrap_or_default();
    AppsView {
        store_id: row.id.to_string(),
        store_name: row.name.clone(),
        apps: apps.iter().map(|app| app_row(app, clock)).collect(),
    }
}

async fn load(
    state: &AppState,
    user: &UserRow,
    id: &ConnectionId,
    path: String,
) -> Result<(OwnedStore, AppsView, crate::views::PageChrome), StatusCode> {
    let row = match load_owned_connection(&state.db, user, id).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(()) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let chrome = super::page_chrome(state, Some(user), path).await;
    let view = apps_view(state, &row, &chrome.clock).await;
    Ok((row, view, chrome))
}

// -- The dashboard ---------------------------------------------------------------

/// `GET /dashboard/stores/{id}/apps/connect`: what to call the device.
pub async fn connect_form(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
) -> Response {
    let path = format!("/dashboard/stores/{id}/apps/connect");
    match load(&state, &user, &id, path).await {
        Ok((_, view, chrome)) => {
            crate::views::connected_apps::connect_form_page(&chrome, &view, None).into_response()
        }
        Err(status) => status.into_response(),
    }
}

#[derive(Deserialize, Default)]
pub struct ConnectForm {
    #[serde(default)]
    device_name: String,
}

/// `POST /dashboard/stores/{id}/apps/connect`: makes a single-use code and
/// shows it, once.
pub async fn connect_submit(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
    Form(form): Form<ConnectForm>,
) -> Response {
    let path = format!("/dashboard/stores/{id}/apps/connect");
    let (row, view, chrome) = match load(&state, &user, &id, path).await {
        Ok(loaded) => loaded,
        Err(status) => return status.into_response(),
    };
    let refuse = |message: &str| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::views::connected_apps::connect_form_page(&chrome, &view, Some(message)),
        )
            .into_response()
    };
    // The code is a link on this instance's public address, so an app can
    // reach the store from it.
    let Some(public_uri) = state.settings.public_uri() else {
        return refuse(
            "This Monokulo has no public address yet, so an app couldn't reach it. \
             An admin can set one under Settings.",
        );
    };
    let raw = shared::auth::generate_app_connect_token();
    let hash = raw.hash();
    let (store_id, device_name, stored_hash) = (
        row.id.clone(),
        clean_device_name(&form.device_name),
        hash.clone(),
    );
    if let Err(e) = state
        .db
        .write(move |db| {
            db.create_app_connect_token(&stored_hash, &store_id, &device_name, now_unix())
        })
        .await
    {
        tracing::error!(store.id = %row.id, error = %e, "a connect code could not be stored");
        return refuse("Something went wrong making the code. Try again in a minute.");
    }
    let link = connect_link(&public_uri, raw.expose());
    let qr = match crate::qr::encode(&link) {
        Ok(qr) => qr,
        Err(e) => {
            tracing::error!(error = %e, "a connect code could not be drawn");
            return refuse("Something went wrong making the code. Try again in a minute.");
        }
    };
    let status_path = status_path(&row.id, &hash);
    crate::views::connected_apps::code_page(&chrome, &view, &link, &qr, &status_path)
        .into_response()
}

/// The link a connect code is: on this instance's public address, with the
/// token in the `#` part, which a browser never sends to a server.
pub fn connect_link(public_uri: &str, token: &str) -> String {
    format!(
        "{}/apps/connect#token={token}",
        public_uri.trim_end_matches('/')
    )
}

/// Where the dashboard checks whether a code was used: by its hash, which
/// can't connect anything.
fn status_path(store_id: &ConnectionId, hash: &shared::auth::TokenHash) -> String {
    format!(
        "/dashboard/stores/{store_id}/apps/connect/{}",
        hash.as_str()
    )
}

/// `GET /dashboard/stores/{id}/apps/connect/{code_hash}`: whether the code
/// was used, and by which app.
pub async fn connect_status(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, code_hash)): Path<(ConnectionId, String)>,
) -> Response {
    let path = format!("/dashboard/stores/{id}/apps/connect/{code_hash}");
    let (row, view, chrome) = match load(&state, &user, &id, path.clone()).await {
        Ok(loaded) => loaded,
        Err(status) => return status.into_response(),
    };
    let app = state
        .db
        .read(move |db| db.app_connected_by_token(&code_hash))
        .await
        .ok()
        .flatten()
        // Only one of this store's apps: a hash from another store says nothing.
        .filter(|app| app.store_id == row.id);
    let app = app.map(|app| app_row(&app, &chrome.clock));
    crate::views::connected_apps::status_page(&chrome, &view, app.as_ref(), &path).into_response()
}

/// `POST /dashboard/stores/{id}/apps/{app_id}/disconnect`: the app's key
/// stops working at once. Its orders stay, and can still be paid.
pub async fn disconnect_submit(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, app_id)): Path<(ConnectionId, String)>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let (store_id, gone) = (row.id.clone(), app_id.clone());
    if let Err(e) = state
        .db
        .write(move |db| db.delete_app(&store_id, &gone))
        .await
    {
        tracing::error!(store.id = %id, error = %e, "an app could not be disconnected");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    state.apps.disconnected(&app_id);
    super::dashboard::redirect_303(&format!(
        "/dashboard/stores/{id}/settings?tab=connections#card-apps"
    ))
}

// -- The app's API -----------------------------------------------------------------

/// `GET /apps/connect`: a connect link opened in a browser.
pub async fn landing(State(state): State<AppState>) -> Response {
    let chrome = super::page_chrome(&state, None, "/apps/connect").await;
    crate::views::connected_apps::landing_page(&chrome).into_response()
}

#[derive(Deserialize)]
pub struct RedeemRequest {
    pub token: String,
    /// What the app calls itself in URLs and headers: a plugin's slug rules.
    pub slug: String,
    /// What it calls itself on pages; optional.
    #[serde(default)]
    pub name: Option<String>,
    /// Its own name for the device, used when the merchant gave none.
    #[serde(default)]
    pub device_name: Option<String>,
    /// What it runs on (`android`, `ios`); optional.
    #[serde(default)]
    pub platform: Option<String>,
}

#[derive(Serialize)]
pub struct RedeemResponse {
    /// The app's own key, given once: `Authorization: Bearer ak_…` on the
    /// API. It names the store, so the app needs nothing else.
    pub app_key: String,
    /// The API's entry, `/api/v1`: a path, to resolve against the URL the
    /// app connected at, so an app that connected over the onion address
    /// stays on it. Everything else (the store, its wallet, the app's own
    /// connection) is a link from there.
    pub api: String,
}

fn api_error(status: StatusCode, error: &str) -> Response {
    (status, Json(serde_json::json!({ "error": error }))).into_response()
}

/// `POST /apps/connect`: an app redeems a code, once, for a key of its own.
/// An unknown, spent or expired code is a bare `401`, all alike. While the
/// instance has no public address it's a `503` before the code is spent,
/// so the same code still works once an admin sets one.
pub async fn redeem(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RedeemRequest>,
) -> Response {
    let Some(slug) = PluginSlug::parse(&req.slug) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "slug must be a lowercase letter, then 1 to 31 lowercase letters, digits or hyphens",
        );
    };
    if state.settings.public_uri().is_none() {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "This Monokulo has no public address yet. Ask its admin to set one.",
        );
    }
    let name = req
        .name
        .as_deref()
        .and_then(PluginName::clean)
        .map(|n| n.as_str().to_owned())
        .unwrap_or_default();
    let device_name = clean_device_name(req.device_name.as_deref().unwrap_or_default());
    let platform: String = req
        .platform
        .as_deref()
        .and_then(PluginSlug::parse)
        .map(|p| p.as_str().to_owned())
        .unwrap_or_default();
    let version = super::pay::client_version(&headers, slug.as_str()).unwrap_or_default();
    let key = shared::auth::generate_app_key();
    let (token_hash, key_hash) = (
        shared::auth::RawToken::presented(&req.token).hash(),
        key.hash(),
    );
    let redeemed = state
        .db
        .write(move |db| {
            let Some(app) = db.redeem_app_connect_token(
                &token_hash,
                now_unix(),
                CONNECT_TOKEN_TTL_SECONDS,
                &crate::db::NewApp {
                    slug: slug.as_str(),
                    name: &name,
                    device_name: &device_name,
                    platform: &platform,
                    version: &version,
                    key_hash: &key_hash,
                },
            )?
            else {
                return Ok(None);
            };
            Ok::<_, crate::db::DbError>(Some(app))
        })
        .await;
    let app = match redeemed {
        Ok(Some(app)) => app,
        Ok(None) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "an app's connect code could not be redeemed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    tracing::info!(store.id = %app.store_id, app.id = %app.id, app.slug = %app.slug, "an app connected");
    Json(RedeemResponse {
        app_key: key.expose().to_owned(),
        api: crate::api::v1::root::Path {}.to_string(),
    })
    .into_response()
}

#[cfg(test)]
mod tests;
