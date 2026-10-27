//! The relay's two routes.
//!
//! `POST /v1/register`, from an app: a device token, the app's name and
//! the sender's VAPID key, answered with the endpoint (`201`) the app hands
//! that sender as its push subscription's.
//!
//! `POST /v1/push/{handle}`, from a sender: plain Web Push (RFC 8030),
//! signed with VAPID (RFC 8292) by the key the handle was registered with
//! and for this relay's origin, handed on to APNs or FCM and answered with
//! what became of it.
//!
//! Neither logs a body, a token or an endpoint: a line names the handle,
//! its platform and app, and the outcome.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use push::vapid::{self, VapidError};
use push::wire::{PushRequest, WireError};
use push::VapidPublicKey;
use serde::{Deserialize, Serialize};

use crate::handles::{AppName, DeviceToken, Handle, HandleId, NewHandle, Platform};
use crate::limits::Refused;
use crate::problem::{Problem, ProblemDetails};
use crate::upstream::Delivery;
use crate::{apns, fcm, Shared};

/// The largest registration read: four short fields.
const MAX_REGISTRATION: usize = 8 * 1024;

pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/v1/register", post(register))
        .route("/v1/push/{handle}", post(push))
        .with_state(shared)
}

/// A registration as an app sends it. A field the relay doesn't know is
/// refused, as one misspelt would otherwise go unnoticed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    platform: Platform,
    /// Checked against the platform once it is known.
    token: String,
    app: AppName,
    vapid_public_key: VapidPublicKey,
}

#[derive(Serialize)]
struct Registered {
    endpoint: String,
    /// Unix seconds: re-register before then (any registration pushes it
    /// back, as does every push APNs or FCM takes).
    expires_at: i64,
}

async fn register(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ProblemDetails> {
    let now = service_runtime::time::now_unix();
    let forwarded_for = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok());
    let client = abuse::identity::client_address(peer.ip(), forwarded_for, &shared.trusted_proxies);
    shared.limits.register(client, now).map_err(rate_limited)?;

    let body = read(body, MAX_REGISTRATION).await?;
    let registration: Registration = serde_json::from_slice(&body)
        .map_err(|error| Problem::InvalidRequest.detail(error.to_string()))?;
    let app = shared.apps.get(&registration.app).ok_or_else(|| {
        Problem::UnknownApp.detail(format!(
            "This relay doesn't serve an app called {}.",
            registration.app
        ))
    })?;
    let served = match registration.platform {
        Platform::Apns | Platform::ApnsLiveActivity | Platform::ApnsPushToStart => {
            app.apns_topic.is_some()
        }
        Platform::Fcm => app.fcm_project_id.is_some(),
    };
    if !served {
        return Err(Problem::UnsupportedPlatform.detail(format!(
            "This relay doesn't hand {} pushes on for {}.",
            registration.platform, registration.app
        )));
    }
    let token = DeviceToken::parse(registration.platform, &registration.token)
        .map_err(|error| Problem::InvalidRequest.detail(format!("token: {error}")))?;
    let handle = shared
        .store
        .register(
            NewHandle {
                platform: registration.platform,
                app: registration.app,
                token,
                sender: registration.vapid_public_key,
            },
            now,
            shared.handle_ttl,
        )
        .await
        .map_err(internal)?;
    tracing::info!(handle = %handle.id, platform = %handle.platform, app = %handle.app, "registered");
    Ok((
        StatusCode::CREATED,
        Json(Registered {
            endpoint: format!("{}/v1/push/{}", shared.public_uri.origin(), handle.id),
            expires_at: handle.expires_at,
        }),
    )
        .into_response())
}

async fn push(
    State(shared): State<Arc<Shared>>,
    Path(handle): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ProblemDetails> {
    let now = service_runtime::time::now_unix();
    // Not a handle of this relay's making: as unknown as an expired one.
    let id: HandleId = handle.parse().map_err(|_| unknown_handle())?;
    let body = read(body, push::MAX_BODY).await?;
    let request = PushRequest::parse(&headers, &body).map_err(wire_problem)?;
    let handle = shared
        .store
        .find(&id, now)
        .await
        .map_err(internal)?
        .ok_or_else(unknown_handle)?;
    vapid::verify(
        &request.authorization,
        &handle.sender,
        shared.public_uri.origin(),
        now,
    )
    .map_err(unauthorized)?;
    shared
        .limits
        .push(&handle.sender, &handle.id, now)
        .map_err(rate_limited)?;

    let delivery = hand_on(&shared, &handle, &request, now).await?;
    tracing::debug!(handle = %handle.id, platform = %handle.platform, app = %handle.app, outcome = ?delivery, "push handed on");
    // Past here the push is handed on or the token is gone: the answer
    // says so even when the bookkeeping fails, since a 500 would have the
    // sender send the same push again.
    match delivery {
        Delivery::Delivered => {
            if let Err(error) = shared.store.touch(&handle.id, now, shared.handle_ttl).await {
                tracing::error!(handle = %handle.id, error = %error, "a delivered push's handle can't be kept alive");
            }
            Ok(StatusCode::CREATED.into_response())
        }
        Delivery::Gone => {
            if let Err(error) = shared.store.delete(&handle.id).await {
                tracing::error!(handle = %handle.id, error = %error, "a gone handle can't be forgotten");
            }
            Err(unknown_handle())
        }
        Delivery::TooLarge => Err(Problem::PayloadTooLarge
            .detail("The push is larger than APNs or FCM carries for this device.")),
        Delivery::Unavailable { retry_after } => Err(Problem::UpstreamUnavailable
            .detail("APNs or FCM didn't take the push; try again later.")
            .retry_after(retry_after)),
        Delivery::Refused => {
            Err(Problem::UpstreamRefused.detail("APNs or FCM refused the push for this relay."))
        }
    }
}

/// `request` translated for `handle`'s platform and handed to APNs or
/// FCM.
async fn hand_on(
    shared: &Shared,
    handle: &Handle,
    request: &PushRequest,
    now: i64,
) -> Result<Delivery, ProblemDetails> {
    // The app's settings, as they are now: an app taken out of
    // `handles.apps`, or out of a platform, takes no more pushes.
    let app = shared.apps.get(&handle.app);
    match handle.platform {
        Platform::Apns | Platform::ApnsLiveActivity | Platform::ApnsPushToStart => {
            let (Some(apns), Some(bundle)) = (
                shared.apns.as_ref(),
                app.and_then(|app| app.apns_topic.as_ref()),
            ) else {
                return Ok(not_served(handle));
            };
            let push =
                apns::translate(handle, bundle, request, now).map_err(|error| match error {
                    apns::TranslateError::TooLarge(_) => {
                        Problem::PayloadTooLarge.detail(error.to_string())
                    }
                    apns::TranslateError::WrongBody { .. } | apns::TranslateError::State(_) => {
                        Problem::InvalidRequest.detail(error.to_string())
                    }
                })?;
            Ok(apns.send(handle, &push, now).await)
        }
        Platform::Fcm => {
            let (Some(fcm), Some(project)) = (
                shared.fcm.as_ref(),
                app.and_then(|app| app.fcm_project_id.as_ref()),
            ) else {
                return Ok(not_served(handle));
            };
            let push = fcm::translate(handle, request).map_err(|error| match error {
                fcm::TranslateError::TooLarge(_) => {
                    Problem::PayloadTooLarge.detail(error.to_string())
                }
                fcm::TranslateError::WrongBody => Problem::InvalidRequest.detail(error.to_string()),
            })?;
            Ok(fcm.send(handle, project, &push, now).await)
        }
    }
}

/// The body, refused as too large past `limit` bytes without reading
/// further.
async fn read(body: Body, limit: usize) -> Result<Bytes, ProblemDetails> {
    axum::body::to_bytes(body, limit).await.map_err(|_| {
        Problem::PayloadTooLarge.detail(format!("The body is more than the {limit} bytes taken."))
    })
}

/// What a request that isn't a push this relay takes is answered with.
fn wire_problem(error: WireError) -> ProblemDetails {
    match error {
        WireError::TooLarge(_) => Problem::PayloadTooLarge.detail(error.to_string()),
        WireError::NoAuthorization | WireError::Authorization(_) => {
            Problem::Unauthorized.detail(error.to_string())
        }
        WireError::NoTtl
        | WireError::Ttl
        | WireError::Urgency
        | WireError::Topic
        | WireError::Empty
        | WireError::ContentType
        | WireError::Encrypted(_)
        | WireError::Json(_) => Problem::InvalidRequest.detail(error.to_string()),
    }
}

fn unauthorized(error: VapidError) -> ProblemDetails {
    Problem::Unauthorized.detail(format!(
        "The push's VAPID signature isn't the handle's sender's for this relay: {error}."
    ))
}

fn unknown_handle() -> ProblemDetails {
    Problem::UnknownHandle.detail("There is no such handle here: drop the subscription.")
}

/// A handle whose app or platform has since been taken out of the
/// relay's settings: gone, as one whose token APNs or FCM refuses. The
/// app registers again if it comes back.
fn not_served(handle: &Handle) -> Delivery {
    tracing::warn!(handle = %handle.id, platform = %handle.platform, app = %handle.app, "a push to a handle whose app or platform this relay no longer serves: forgetting it");
    Delivery::Gone
}

fn rate_limited(refused: Refused) -> ProblemDetails {
    let detail = match refused {
        Refused::Sender(_) => "This sender has pushed too often this minute.",
        Refused::Handle(_) => "This handle has had too many pushes this minute.",
        Refused::Registrations(_) => "Too many registrations from this address this minute.",
    };
    Problem::RateLimited
        .detail(detail)
        .retry_after(refused.retry_after())
}

fn internal(error: crate::handles::StoreError) -> ProblemDetails {
    tracing::error!(error = %error, "the handles database failed");
    Problem::Internal.detail("Something went wrong on the relay. Try again in a moment.")
}
