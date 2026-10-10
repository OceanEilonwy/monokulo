//! A store's webhooks on its settings page (`views::webhooks`): adding and
//! deleting one, a delivery's detail, "Send again" and "Retry failed".
//! Every action is a plain form POST that comes back to the settings page
//! (`303`) with a toast; nothing here needs JavaScript.

use std::collections::BTreeMap;

use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use shared::ids::WebhookId;

use super::orders::{load_owned_connection, render_store_settings_page, saved};
use super::{AppState, AuthedUser};
use crate::db::{ConnectionId, DbError, StoreConnectionRow, UserRow};
use crate::views;
use crate::views::store_settings::StoreSection;
use crate::views::webhooks::{WebhookEntry, WebhooksCard};

const SECTION: StoreSection = StoreSection::Webhooks;

/// The Webhooks card's data for `store`: each webhook, how it's doing and
/// its latest deliveries.
pub(super) async fn card(
    state: &AppState,
    store: &StoreConnectionRow,
    clock: &views::time::Clock,
    created_secret: Option<String>,
) -> WebhooksCard {
    let (store_id, since) = (
        store.id.clone(),
        clock.now() - views::webhooks::RECENT_DAYS * 86_400,
    );
    let read = state
        .db
        .read(move |db| entries(db, &store_id, since, views::webhooks::RECENT))
        .await;
    let (webhooks, unavailable) = match read {
        Ok(webhooks) => (webhooks, false),
        Err(e) => {
            tracing::warn!(store.id = %store.id, error = %e, "could not read the store's webhooks");
            (Vec::new(), true)
        }
    };
    WebhooksCard {
        store_id: store.id.to_string(),
        store_name: store.name.clone(),
        clock: clock.clone(),
        max_attempts: state.settings.webhooks.load().max_attempts,
        webhooks,
        unavailable,
        created_secret,
    }
}

/// The store's webhooks with `recent` deliveries each.
fn entries(
    db: &crate::db::Db,
    store_id: &ConnectionId,
    since: i64,
    recent: usize,
) -> Result<Vec<WebhookEntry>, DbError> {
    let plugin = db.active_integration(store_id)?;
    db.list_webhooks(store_id)?
        .into_iter()
        .map(|webhook| {
            Ok(WebhookEntry {
                health: db.webhook_health(&webhook.id, since)?,
                recent: db.recent_deliveries(&webhook.id, recent)?,
                plugin: plugin
                    .as_ref()
                    .filter(|p| p.webhook_id.as_deref() == Some(webhook.id.as_str()))
                    .map(|p| super::store_site::integration_name(&p.kind)),
                webhook,
            })
        })
        .collect()
}

/// One of the store's webhooks with `recent` deliveries.
async fn entry(
    state: &AppState,
    store: &StoreConnectionRow,
    webhook: WebhookId,
    recent: usize,
) -> Result<Option<WebhookEntry>, DbError> {
    let store_id = store.id.clone();
    state
        .db
        .read(move |db| {
            Ok(entries(db, &store_id, 0, recent)?
                .into_iter()
                .find(|e| e.webhook.id == webhook))
        })
        .await
}

#[derive(Deserialize)]
pub struct AddWebhookForm {
    pub url: String,
    /// One `Header-Name: value` pair per line: the plainest way to take any
    /// number of them from a form without JavaScript. Blank lines are
    /// ignored; any other line without `:` refuses the whole form, naming
    /// the line (a header the merchant thinks they set but didn't would be
    /// worse than an error now).
    #[serde(default)]
    pub extra_headers: String,
}

/// [`AddWebhookForm::extra_headers`] as name/value pairs.
fn parse_extra_headers(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut headers = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let refused = || {
            format!(
                "Custom headers must be one \"Header-Name: value\" pair per line - could not read: {line:?}"
            )
        };
        let (name, value) = line.split_once(':').ok_or_else(refused)?;
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() {
            return Err(refused());
        }
        headers.insert(name.to_string(), value.to_string());
    }
    Ok(headers)
}

/// `POST /dashboard/stores/{id}/settings/webhooks`: adds a webhook and shows
/// the page with its signing secret, this once. Answers with the page
/// rather than a redirect, so the secret is never in a URL (history, a
/// `Referer`).
pub async fn add(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path(id): Path<ConnectionId>,
    Form(form): Form<AddWebhookForm>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let refused = |message: String| {
        render_store_settings_page(&state, row, &user, Some(message), None, Some(SECTION))
    };
    let headers = match parse_extra_headers(&form.extra_headers) {
        Ok(headers) => headers,
        Err(message) => return refused(message).await,
    };
    match crate::webhooks::create(&state.db, &state.encryption_key, &id, &form.url, &headers).await
    {
        Ok(created) => {
            let row = match load_owned_connection(&state.db, &user, &id).await {
                Ok(Some(row)) => row,
                _ => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
            render_store_settings_page(
                &state,
                row,
                &user,
                None,
                Some(created.signing_secret),
                Some(SECTION),
            )
            .await
        }
        Err(crate::webhooks::CreateError::Invalid(message)) => refused(message).await,
        Err(crate::webhooks::CreateError::Db(e)) => {
            tracing::error!(store.id = %id, error = %e, "a webhook could not be added");
            refused("Something went wrong. Please try again.".to_string()).await
        }
    }
}

/// The store, if it's `user`'s, and the webhook, if it's the store's.
async fn owned(
    state: &AppState,
    user: &UserRow,
    id: &ConnectionId,
    webhook: &WebhookId,
    recent: usize,
) -> Result<(super::OwnedStore, WebhookEntry), StatusCode> {
    let row = match load_owned_connection(&state.db, user, id).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(()) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    match entry(state, &row, webhook.clone(), recent).await {
        Ok(Some(entry)) => Ok((row, entry)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// `GET …/settings/webhooks/{webhook}/delete`: the question as a page,
/// for a browser without JavaScript.
pub async fn delete_page(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, webhook)): Path<(ConnectionId, WebhookId)>,
) -> Response {
    let (row, entry) = match owned(&state, &user, &id, &webhook, 0).await {
        Ok(found) => found,
        Err(status) => return status.into_response(),
    };
    let chrome = super::page_chrome(
        &state,
        Some(&user),
        views::webhooks::Paths::new(id.as_str()).delete(webhook.as_str()),
    )
    .await;
    views::webhooks::delete_page(&chrome, &row.name, &entry).into_response()
}

/// `POST …/settings/webhooks/{webhook}/delete`: deletes it and its
/// deliveries. Gone already (another tab): the same, nothing to do.
pub async fn delete(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, webhook)): Path<(ConnectionId, WebhookId)>,
) -> Response {
    let row = match load_owned_connection(&state.db, &user, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let (store_id, target) = (id.clone(), webhook.clone());
    match state
        .db
        .write(move |db| db.delete_webhook(&store_id, &target))
        .await
    {
        Ok(deleted) => {
            if deleted {
                tracing::info!(store.id = %id, webhook.id = %webhook, "webhook deleted");
            }
            saved(&id, SECTION)
        }
        Err(e) => {
            tracing::error!(store.id = %id, webhook.id = %webhook, error = %e, "a webhook could not be deleted");
            render_store_settings_page(
                &state,
                row,
                &user,
                Some("Could not delete that webhook. Please try again.".to_string()),
                None,
                Some(SECTION),
            )
            .await
        }
    }
}

/// `GET …/settings/webhooks/{webhook}/deliveries`: its latest deliveries.
pub async fn all(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, webhook)): Path<(ConnectionId, WebhookId)>,
) -> Response {
    let (row, entry) = match owned(&state, &user, &id, &webhook, views::webhooks::ALL).await {
        Ok(found) => found,
        Err(status) => return status.into_response(),
    };
    let chrome = super::page_chrome(
        &state,
        Some(&user),
        views::webhooks::Paths::new(id.as_str()).all(webhook.as_str()),
    )
    .await;
    views::webhooks::all_page(
        &chrome,
        &row.name,
        state.settings.webhooks.load().max_attempts,
        &entry,
    )
    .into_response()
}

/// `GET …/settings/webhooks/{webhook}/deliveries/{delivery}`: a delivery's
/// detail as a page (with JavaScript, the settings page opens it as a
/// dialog).
pub async fn detail(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, webhook, delivery)): Path<(ConnectionId, WebhookId, i64)>,
) -> Response {
    let (row, entry) = match owned(&state, &user, &id, &webhook, 0).await {
        Ok(found) => found,
        Err(status) => return status.into_response(),
    };
    let (store_id, target) = (id.clone(), webhook.clone());
    let found = state
        .db
        .read(move |db| db.get_delivery(&store_id, &target, delivery))
        .await;
    let delivery = match found {
        Ok(Some(delivery)) => delivery,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let chrome = super::page_chrome(
        &state,
        Some(&user),
        views::webhooks::Paths::new(id.as_str()).detail(webhook.as_str(), delivery.id),
    )
    .await;
    views::webhooks::detail_page(
        &chrome,
        &row.name,
        state.settings.webhooks.load().max_attempts,
        &entry.webhook,
        &delivery,
    )
    .into_response()
}

/// `POST …/deliveries/{delivery}/send-again`: one more attempt at once; a
/// delivery that was delivered or given up on starts its schedule over.
pub async fn send_again(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, webhook, delivery)): Path<(ConnectionId, WebhookId, i64)>,
) -> Response {
    if let Err(status) = owned(&state, &user, &id, &webhook, 0).await {
        return status.into_response();
    }
    let (store_id, target, now) = (id.clone(), webhook.clone(), crate::now_unix());
    match state
        .db
        .write(move |db| db.send_delivery_again(&store_id, &target, delivery, now))
        .await
    {
        Ok(true) => {
            tracing::info!(store.id = %id, webhook.id = %webhook, webhook.delivery = delivery, "webhook delivery sent again");
            state.webhooks.wake();
            saved(&id, SECTION)
        }
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(store.id = %id, error = %e, "a webhook delivery could not be sent again");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `POST …/settings/webhooks/{webhook}/retry-failed`: every delivery of the
/// webhook that gave up, queued again, each starting its schedule over.
pub async fn retry_failed(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, webhook)): Path<(ConnectionId, WebhookId)>,
) -> Response {
    if let Err(status) = owned(&state, &user, &id, &webhook, 0).await {
        return status.into_response();
    }
    let (store_id, target, now) = (id.clone(), webhook.clone(), crate::now_unix());
    match state
        .db
        .write(move |db| db.retry_failed_deliveries(&store_id, &target, now))
        .await
    {
        Ok(count) => {
            tracing::info!(store.id = %id, webhook.id = %webhook, webhook.deliveries = count, "failed webhook deliveries retried");
            state.webhooks.wake();
            saved(&id, SECTION)
        }
        Err(e) => {
            tracing::error!(store.id = %id, error = %e, "failed webhook deliveries could not be retried");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests;
