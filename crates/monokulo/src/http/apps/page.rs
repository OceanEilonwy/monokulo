//! A connected app's page (`views::app_page`) and its two actions: Remove
//! a push target, and Send a test push. Both are plain form POSTs that
//! come back to the page (`303`) with what happened in the query, which
//! the page reads back from the database for its toast: nothing here
//! needs JavaScript.
//!
//! A test push is a push: queued as a `push_deliveries` row for the
//! target, then sent at once, while the merchant waits, by the delivery
//! worker's own code (`push::delivery::send_now`) through the one
//! `PushClient` the worker sends with: the same signing, encryption,
//! outbound rules (`check_host`: no private address unless
//! `webhooks.allow_private_urls`, never an onion address), store budget
//! and recording, its target's run of failures included. One that fails
//! is tried again by the worker like any push.

use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::super::fx::FxRequest;
use super::super::orders::load_owned_connection;
use super::super::{AppState, AuthedUser};
use crate::apps::SaleKind;
use crate::db::{ConnectionId, DbError, PushTargetId, StoreConnectionRow, TestPush};
use crate::views::app_page::{self, AppPageView, Limits, TargetView, TestForm};
use crate::views::time::Clock;

/// What a page after an action says, from its query.
#[derive(Deserialize, Default)]
pub struct PageQuery {
    /// A test push was just sent: its id.
    #[serde(default)]
    tested: Option<i64>,
    /// A push target was just removed.
    #[serde(default)]
    removed: Option<u8>,
}

/// The page's data for the store's app `app_id`; `None` when the store
/// has no such app.
async fn read(
    state: &AppState,
    store: &StoreConnectionRow,
    app_id: &crate::db::AppId,
    clock: &Clock,
    tested: Option<i64>,
) -> Result<Option<(AppPageView, Option<crate::db::PushDeliveryRow>)>, DbError> {
    let (store_id, app_id) = (store.id.clone(), app_id.clone());
    let since = clock.now().saturating_sub(app_page::RECENT_DAYS * 86_400);
    let read = state
        .db
        .read(move |db| {
            let Some(app) = db.app(&store_id, &app_id)? else {
                return Ok::<_, DbError>(None);
            };
            let orders = db.app_order_stats(&app_id, since)?;
            let targets = db.push_target_summaries(&app_id)?;
            let mut stats = db.app_push_stats(&app_id, since)?;
            let recent = db.push_deliveries_of_app(&app_id, app_page::RECENT_PUSHES)?;
            let kept = db.push_delivery_count_of_app(&app_id)?;
            let tested = match tested {
                Some(id) => db.push_delivery_of_app(&store_id, &app_id, id)?,
                None => None,
            };
            let targets: Vec<_> = targets
                .into_iter()
                .map(|target| {
                    let stats = stats.remove(&target.id).unwrap_or_default();
                    (target, stats)
                })
                .collect();
            Ok(Some((app, orders, targets, recent, kept, tested)))
        })
        .await?;
    let Some((app, orders, targets, recent, kept, tested)) = read else {
        return Ok(None);
    };
    let relay_host = relay_host(state).await;
    let config = state.settings.push.load();
    let view = AppPageView {
        store_id: store.id.to_string(),
        store_name: store.name.clone(),
        app: super::app_row(&app, clock, None),
        clock: clock.clone(),
        orders,
        connected_at: app.connected_at,
        targets: targets
            .into_iter()
            .map(|(target, stats)| TargetView {
                relay: target.host == app_page::PUBLIC_RELAY_HOST
                    || relay_host.as_deref() == Some(target.host.as_str()),
                target,
                stats,
            })
            .collect(),
        recent,
        kept,
        limits: Limits {
            failure_limit: config.target_failure_limit,
            max_attempts: config.max_attempts,
            keep_days: config.keep_secs / 86_400,
        },
        test_form: None,
        toast: None,
    };
    Ok(Some((view, tested)))
}

/// The host of this Monokulo's own push relay, while it runs one with a
/// public address.
async fn relay_host(state: &AppState) -> Option<String> {
    let uri = state.relay.view().await?.public_uri?;
    url::Url::parse(&uri).ok()?.host_str().map(str::to_owned)
}

/// The store, if it's the user's, and the chrome of the app's page.
async fn owned(
    state: &AppState,
    user: &crate::db::UserRow,
    id: &ConnectionId,
    app_id: &crate::db::AppId,
) -> Result<(super::super::OwnedStore, crate::views::PageChrome), StatusCode> {
    let row = match load_owned_connection(&state.db, user, id).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(()) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let path = crate::views::connected_apps::app_path(id.as_str(), app_id.as_str());
    let chrome = super::super::page_chrome(state, Some(user), path).await;
    Ok((row, chrome))
}

/// `GET /stores/{id}/settings/apps/{app_id}`: the app's page, for the
/// store's members; another store's app is `404`. Asked by fixi (Recent
/// pushes' poll while its newest push is still to be sent), that card
/// alone.
pub async fn app_page(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    FxRequest(fixi): FxRequest,
    Path((id, app_id)): Path<(ConnectionId, crate::db::AppId)>,
    Query(query): Query<PageQuery>,
) -> Response {
    let (row, chrome) = match owned(&state, &user, &id, &app_id).await {
        Ok(found) => found,
        Err(status) => return status.into_response(),
    };
    let (mut view, tested) = match read(&state, &row, &app_id, &chrome.clock, query.tested).await {
        Ok(Some(found)) => found,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(store.id = %id, app.id = %app_id, error = %e, "an app's page could not be read");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if fixi {
        return app_page::recent_card(&view).into_response();
    }
    view.toast = match (tested, query.removed) {
        (Some(push), _) => {
            let host = view
                .targets
                .iter()
                .find(|t| t.target.id == push.push_target_id)
                .map_or_else(|| "The push service".to_owned(), |t| t.target.host.clone());
            Some(app_page::tested_toast(&push, &host, &view.clock))
        }
        (None, Some(_)) => Some(app_page::removed_toast()),
        (None, None) => None,
    };
    app_page::page(&chrome, &view).into_response()
}

/// `POST /stores/{id}/settings/apps/{app_id}/push-targets/{target_id}/remove`:
/// deletes the push target, and its pushes, from the store's app; back to
/// the app's page with `?removed=1`. Not one of that app's (another app's,
/// another store's, gone already): `404`.
pub async fn remove_push_target(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, app_id, target_id)): Path<(ConnectionId, crate::db::AppId, PushTargetId)>,
) -> Response {
    if let Err(status) = owned(&state, &user, &id, &app_id).await {
        return status.into_response();
    }
    let (store_id, app, target) = (id.clone(), app_id.clone(), target_id.clone());
    match state
        .db
        .write(move |db| db.delete_push_target_of_store(&store_id, &app, &target))
        .await
    {
        Ok(true) => {
            tracing::info!(
                store.id = %id,
                app.id = %app_id,
                push_target.id = %target_id,
                "push target removed from the dashboard"
            );
            crate::base::see_other(format!(
                "{}?removed=1#card-push-targets",
                crate::views::connected_apps::app_path(id.as_str(), app_id.as_str())
            ))
        }
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(store.id = %id, push_target.id = %target_id, error = %e, "a push target could not be removed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct TestPushForm {
    #[serde(default)]
    target: String,
    #[serde(default)]
    event: String,
}

/// How long the worker leaves a test push to the request that queued it,
/// beyond the attempt's own timeout, before it may send it itself: only
/// if that attempt never got recorded.
const LEFT_TO_THE_REQUEST_SECS: i64 = 5;

/// `POST /stores/{id}/settings/apps/{app_id}/test-push`: sends one of the
/// store's app's push targets a test push of the sale event chosen, now,
/// as the delivery worker sends any push; back to the app's page with
/// `?tested=<push id>`, whose toast says how it went. Past the store's
/// test pushes for the minute, nothing is queued and the page says so
/// (`429`).
pub async fn test_push(
    State(state): State<AppState>,
    AuthedUser(user, _): AuthedUser,
    Path((id, app_id)): Path<(ConnectionId, crate::db::AppId)>,
    Form(form): Form<TestPushForm>,
) -> Response {
    let (row, chrome) = match owned(&state, &user, &id, &app_id).await {
        Ok(found) => found,
        Err(status) => return status.into_response(),
    };
    let refuse = |status: StatusCode, message: String| {
        let (state, row, chrome, app_id) = (&state, &row, &chrome, &app_id);
        let (target, event) = (form.target.clone(), form.event.clone());
        async move {
            match read(state, row, app_id, &chrome.clock, None).await {
                Ok(Some((mut view, _))) => {
                    view.toast = Some(
                        crate::views::settings::Toast::new(
                            crate::views::settings::ToastKind::Error,
                            "No test push sent.",
                        )
                        .line(message.clone()),
                    );
                    view.test_form = Some(TestForm {
                        target,
                        event,
                        refused: message,
                    });
                    (status, app_page::page(chrome, &view)).into_response()
                }
                Ok(None) => StatusCode::NOT_FOUND.into_response(),
                Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
    };
    let Ok(event) = form.event.parse::<SaleKind>() else {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Choose one of the sale events to test.".into(),
        )
        .await;
    };
    let target_id = PushTargetId::new(form.target.clone());
    // The abuse protection's clock, as test webhooks are counted.
    if let Err(wait) = state
        .push
        .tests
        .check_with_retry(row.id.clone(), state.abuse.now())
    {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "This store has sent {} test pushes in the last minute. Try again in {wait} s.",
                state.push.tests.limit()
            ),
        )
        .await;
    }
    let client = match state.push.client().await {
        Ok(client) => client,
        Err(e) => {
            tracing::error!(error = %e, "could not build the push HTTP client");
            return refuse(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Couldn't send a test push. Please try again.".into(),
            )
            .await;
        }
    };
    client.set_contact(crate::push::delivery::contact(&state.settings));
    let config = state.settings.push.load();
    let reach =
        outbound::Reach::allowing_private(state.settings.webhooks.load().allow_private_urls);
    let at = crate::now_unix();
    let left = i64::try_from(config.delivery_timeout.as_secs())
        .unwrap_or(i64::MAX)
        .saturating_add(1 + LEFT_TO_THE_REQUEST_SECS);
    let (store_id, app, target, ttl) = (
        row.id.clone(),
        app_id.clone(),
        target_id.clone(),
        config.default_ttl,
    );
    let queued = state
        .db
        .write(move |db| {
            let Some(push_id) = db.queue_test_push(&TestPush {
                store_id: &store_id,
                app_id: &app,
                target_id: &target,
                event,
                at,
                ttl,
                due_at: at.saturating_add(left),
            })?
            else {
                return Ok(None);
            };
            Ok::<_, DbError>(db.waiting_push(push_id)?.map(|due| (push_id, due)))
        })
        .await;
    let (push_id, due) = match queued {
        Ok(Some(queued)) => queued,
        // Not one of the app's targets (removed meanwhile, another app's).
        Ok(None) => {
            return refuse(
                StatusCode::NOT_FOUND,
                "This app has no such push target.".into(),
            )
            .await
        }
        Err(e) => {
            tracing::error!(store.id = %id, push_target.id = %target_id, error = %e, "a test push could not be queued");
            return refuse(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Couldn't send a test push. Please try again.".into(),
            )
            .await;
        }
    };
    match due {
        Ok(push) => {
            let sent = crate::push::delivery::send_now(
                &state.db,
                client,
                &push,
                &config,
                reach,
                &crate::webhooks::delivery::SystemClock,
            )
            .await;
            tracing::info!(
                store.id = %id,
                push_target.id = %target_id,
                push.host = push.target.endpoint.host(),
                push.event = event.as_str(),
                push.id = push_id,
                recorded = sent.is_ok(),
                "test push sent from the dashboard"
            );
        }
        // Its target can't be read: the worker gives it up when it comes due.
        Err(unreadable) => {
            tracing::error!(push_target.id = %unreadable.target_id, error = %unreadable.error, "a test push's target can't be read");
        }
    }
    crate::base::see_other(format!(
        "{}?tested={push_id}#card-recent-pushes",
        crate::views::connected_apps::app_path(id.as_str(), app_id.as_str())
    ))
}
