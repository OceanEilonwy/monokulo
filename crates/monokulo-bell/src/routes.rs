//! The Bell's four routes.
//!
//! `POST /v1/register`, from a till: the merchant's Square access token, a
//! location and the till's Web Push subscription, answered `201` with the
//! registration's id once Square says the location is the token's
//! merchant's. `DELETE /v1/register/{id}` takes it back.
//!
//! `GET /v1/info`, from a till choosing a Bell: the Square application its
//! sign-in must be through, and the key its subscription is made for.
//!
//! `POST /v1/square/webhook`, from Square: answered `200` once its
//! signature checks, and a completed payment's tills dinged after, once
//! per payment.
//!
//! No route logs a body, a token or an endpoint: a line names a
//! registration and an endpoint's host at most.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use push::Subscription;
use serde::{Deserialize, Serialize};

use crate::Shared;
use crate::problem::{Problem, ProblemDetails};
use crate::registrations::{NewRegistration, RegisterError, RegistrationId, StoreError};
use crate::square::{self, AccessToken, LocationError, LocationId, Notification};

/// The largest registration read: three short fields and a subscription.
const MAX_REGISTRATION: usize = 8 * 1024;

/// The largest webhook read. A payment's is a few kilobytes.
const MAX_WEBHOOK: usize = 256 * 1024;

pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/v1/register", post(register))
        .route("/v1/register/{registration_id}", delete(unregister))
        .route("/v1/info", get(info))
        .route(square::WEBHOOK_PATH, post(webhook))
        .with_state(shared)
}

/// A registration as a till sends it. A field the Bell doesn't know is
/// refused, as one misspelt would otherwise go unnoticed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    square_access_token: AccessToken,
    location_id: LocationId,
    subscription: Subscription,
}

#[derive(Serialize)]
struct Registered<'a> {
    registration_id: &'a str,
    square_application_id: &'a str,
}

async fn register(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ProblemDetails> {
    let now = service_runtime::time::now_unix();
    let client = abuse::identity::client_address(peer.ip(), &headers, &shared.trusted_proxies);
    shared
        .registrations
        .check_with_retry(abuse::ClientIdentity::from_address(client), now)
        .map_err(|retry_after| {
            Problem::RateLimited
                .detail("Too many registrations from this address this minute.")
                .retry_after(retry_after)
        })?;

    let body = read(body, MAX_REGISTRATION).await?;
    let registration: Registration = serde_json::from_slice(&body)
        .map_err(|error| Problem::InvalidRequest.detail(error.to_string()))?;
    let Registration {
        square_access_token,
        location_id,
        subscription,
    } = registration;
    outbound::check_host(subscription.endpoint.as_url(), shared.reach).map_err(|error| {
        Problem::InvalidRequest.detail(format!("subscription.endpoint: {error}"))
    })?;
    let merchant_id = shared
        .square
        .merchant_of(square_access_token, &location_id)
        .await
        .map_err(|error| match error {
            LocationError::Refused(status) => {
                tracing::info!(%status, "Square refused a registration's token or location");
                Problem::SquareRefused.detail(
                    "Square refused the access token, or the location isn't its merchant's.",
                )
            }
            LocationError::NotById => {
                tracing::info!("a registration named its location by an alias");
                Problem::InvalidRequest.detail(
                    "location_id must be the location's own ID, as Square lists it, not an alias such as main.",
                )
            }
            LocationError::Unavailable(reason) => {
                tracing::warn!(%reason, "Square couldn't check a registration");
                Problem::SquareUnavailable.detail("Square couldn't be asked; try again later.")
            }
        })?;
    let id = shared
        .store
        .register(
            NewRegistration {
                merchant_id,
                location_id,
                subscription,
            },
            now,
        )
        .await
        .map_err(|error| match error {
            RegisterError::Full => Problem::TooManyRegistrations.detail(format!(
                "The location has {} registrations already: unregister a till that no longer uses this Bell.",
                crate::registrations::MAX_PER_LOCATION
            )),
            // No id: the registration isn't the caller's.
            RegisterError::Taken => {
                tracing::info!("a registration of another merchant's endpoint was refused");
                Problem::EndpointRegistered.detail(
                    "The subscription is registered for another Square merchant. Unregister it there, or register with the subscription's own keys.",
                )
            }
            RegisterError::Store(error) => internal(error),
        })?;
    tracing::info!(registration = %id, "registered");
    Ok((
        StatusCode::CREATED,
        Json(Registered {
            registration_id: id.as_str(),
            square_application_id: shared.application_id.as_str(),
        }),
    )
        .into_response())
}

async fn unregister(
    State(shared): State<Arc<Shared>>,
    Path(registration_id): Path<String>,
) -> Result<StatusCode, ProblemDetails> {
    // Not an id of the Bell's making: as unknown as a deleted one.
    let id: RegistrationId = registration_id
        .parse()
        .map_err(|_| unknown_registration())?;
    if shared.store.delete(&id).await.map_err(internal)? {
        tracing::info!(registration = %id, "unregistered");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(unknown_registration())
    }
}

#[derive(Serialize)]
struct Info {
    square_application_id: String,
    environment: &'static str,
    vapid_public_key: String,
    version: &'static str,
}

async fn info(State(shared): State<Arc<Shared>>) -> Json<Info> {
    Json(Info {
        square_application_id: shared.application_id.as_str().to_owned(),
        environment: shared.environment.as_str(),
        vapid_public_key: shared.vapid_key.public().to_base64url(),
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn webhook(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, ProblemDetails> {
    let body = read(body, MAX_WEBHOOK).await?;
    let signature = headers
        .get(square::SIGNATURE_HEADER)
        .map(|value| value.as_bytes());
    shared
        .webhook_key
        .verify(&shared.notification_url, &body, signature)
        .map_err(|error| {
            tracing::warn!(error = %error, "a webhook not signed by Square was refused");
            Problem::InvalidSignature.detail(format!(
                "{error}. Square signs each webhook for the URL its subscription names, which must be {} exactly.",
                shared.notification_url
            ))
        })?;
    match square::read_notification(&body) {
        Ok(Notification::Completed {
            payment_id,
            merchant_id,
            location_id,
        }) => shared.dings.ring(payment_id, merchant_id, location_id),
        Ok(Notification::Ignored) => {}
        // Square's own, so answered as taken: refusing it would only have
        // Square send it again.
        Err(error) => tracing::warn!(error = %error, "a signed webhook was dropped"),
    }
    Ok(StatusCode::OK)
}

/// The body, refused as too large past `limit` bytes without reading
/// further.
async fn read(body: Body, limit: usize) -> Result<Bytes, ProblemDetails> {
    axum::body::to_bytes(body, limit).await.map_err(|_| {
        Problem::PayloadTooLarge.detail(format!("The body is more than the {limit} bytes taken."))
    })
}

fn unknown_registration() -> ProblemDetails {
    Problem::UnknownRegistration.detail("There is no such registration here.")
}

fn internal(error: StoreError) -> ProblemDetails {
    tracing::error!(error = %error, "the registrations database failed");
    Problem::Internal.detail("Something went wrong on the Bell. Try again in a moment.")
}
