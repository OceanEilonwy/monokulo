//! The Bell's routes.
//!
//! `POST /v1/register`, from a till: its point of sale and environment,
//! the merchant's access token there, a location and the till's Web Push
//! subscription, answered `201` with the registration's id once the point
//! of sale says the location is the token's merchant's. `DELETE
//! /v1/register/{id}` takes it back.
//!
//! `GET /v1/info`, from a till choosing a Bell: the integrations it
//! serves, the application each one's sign-in must be through, and the key
//! its subscription is made for.
//!
//! `POST /v1/{pos}/{environment}/webhook`, from a point of sale, only for
//! the integrations this Bell serves: answered `200` once it is shown to be
//! that integration's, and a payment's tills dinged after, once per
//! payment.
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

use crate::clover::{self, AuthError, Clover, Incoming, VerificationCode};
use crate::pos::{
    self, AccessToken, ByIntegration, Integration, LocationId, MerchantId, PointOfSale, ProofError,
    Ring,
};
use crate::problem::{Problem, ProblemDetails};
use crate::registrations::{NewRegistration, RegisterError, RegistrationId, StoreError};
use crate::square::{self, Notification, Square};
use crate::{Adapter, Adapters, Shared};

/// The largest registration read: five short fields and a subscription.
const MAX_REGISTRATION: usize = 8 * 1024;

/// The largest webhook read. A payment's is a few kilobytes.
const MAX_WEBHOOK: usize = 256 * 1024;

/// The most of a Clover webhook without its auth header that is read: its
/// verification handshake is some 60 bytes, and nothing else comes
/// without the header.
const MAX_VERIFICATION: usize = 1024;

/// The routes, with a webhook route for each integration this Bell serves
/// and none for the others: a webhook to one that is off is `404`, its
/// body never read (decision 51).
pub fn router(shared: Arc<Shared>) -> Router {
    let mut router = Router::new()
        .route("/v1/register", post(register))
        .route("/v1/register/{registration_id}", delete(unregister))
        .route("/v1/info", get(info))
        .with_state(Arc::clone(&shared));
    for adapter in shared.adapters.iter() {
        let path = adapter.integration().webhook_path();
        router = router.merge(match adapter {
            Adapter::Square(square) => Router::new()
                .route(path, post(square_webhook))
                .with_state((Arc::clone(&shared), Arc::clone(square))),
            Adapter::Clover(clover) => Router::new()
                .route(path, post(clover_webhook))
                .with_state((Arc::clone(&shared), Arc::clone(clover))),
        });
    }
    router
}

/// A registration as a till sends it, one shape for every point of sale
/// (decision 44): for Clover, the location is the merchant. The
/// environment is its point of sale's wire name for it (decision 75). A
/// field the Bell doesn't know is refused, as one misspelt would otherwise
/// go unnoticed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    point_of_sale: PointOfSale,
    environment: String,
    access_token: AccessToken,
    location_id: LocationId,
    subscription: Subscription,
}

#[derive(Serialize)]
struct Registered<'a> {
    registration_id: &'a str,
    point_of_sale: PointOfSale,
    environment: &'static str,
    application_id: &'a str,
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
        point_of_sale,
        environment,
        access_token,
        location_id,
        subscription,
    } = registration;
    let integration = Integration::of(point_of_sale, &environment)
        .ok_or_else(|| unknown_environment(point_of_sale))?;
    outbound::check_host(subscription.endpoint.as_url(), shared.reach).map_err(|error| {
        Problem::InvalidRequest.detail(format!("subscription.endpoint: {error}"))
    })?;
    let adapter = shared
        .adapters
        .get(integration)
        .ok_or_else(|| not_served(&shared.adapters, integration))?;
    let merchant_id = match adapter {
        Adapter::Square(square) => square
            .merchant_of(access_token, &location_id)
            .await
            .map_err(|error| refused(integration, error))?,
        Adapter::Clover(clover) => {
            // A Clover merchant is its own location (decision 48).
            let merchant = MerchantId::from(location_id.clone());
            clover
                .prove(access_token, &merchant)
                .await
                .map_err(|error| refused(integration, error))?;
            merchant
        }
    };
    let id = shared
        .store
        .register(
            NewRegistration {
                integration,
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
                    "The subscription is registered for another merchant. Unregister it there, or register with the subscription's own keys.",
                )
            }
            RegisterError::Store(error) => internal(error),
        })?;
    tracing::info!(
        registration = %id,
        point_of_sale = %point_of_sale,
        environment = integration.environment(),
        "registered"
    );
    Ok((
        StatusCode::CREATED,
        Json(Registered {
            registration_id: id.as_str(),
            point_of_sale,
            environment: integration.environment(),
            application_id: adapter.application_id(),
        }),
    )
        .into_response())
}

/// An environment `pos` doesn't have: its token is never used.
fn unknown_environment(pos: PointOfSale) -> ProblemDetails {
    let environments: Vec<&str> = Integration::ALL
        .into_iter()
        .filter(|integration| integration.point_of_sale() == pos)
        .map(Integration::environment)
        .collect();
    Problem::InvalidRequest.detail(format!(
        "environment must be one of {}'s: {}.",
        pos.title(),
        environments.join(", ")
    ))
}

/// A registration for an integration this Bell doesn't serve: its token
/// is never used.
fn not_served(adapters: &Adapters, integration: Integration) -> ProblemDetails {
    Problem::InvalidRequest.detail(format!(
        "{integration} isn't an integration this Bell serves: this Bell runs {}.",
        pos::in_words(adapters.on())
    ))
}

/// What an integration's refusal to prove a registration answers.
fn refused(integration: Integration, error: ProofError) -> ProblemDetails {
    let pos = integration.point_of_sale();
    let name = pos.title();
    let environment = integration.environment();
    match error {
        ProofError::Refused(status) => {
            tracing::info!(%status, point_of_sale = %pos, environment, "{name} refused a registration's token or location");
            Problem::Refused(pos).detail(format!(
                "{integration} refused the access token, or the location isn't its merchant's."
            ))
        }
        ProofError::NotById => {
            tracing::info!(point_of_sale = %pos, environment, "a registration named its location by another id");
            Problem::InvalidRequest.detail(match pos {
                PointOfSale::Square => "location_id must be the location's own ID, as Square lists it, not an alias such as main.",
                PointOfSale::Clover => "location_id must be the merchant ID Clover answers the token for.",
            })
        }
        ProofError::Unavailable(reason) => {
            tracing::warn!(%reason, point_of_sale = %pos, environment, "{name} couldn't check a registration");
            Problem::Unavailable(pos)
                .detail(format!("{integration} couldn't be asked; try again later."))
        }
    }
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

/// `/v1/info`: the integrations this Bell serves, by point of sale and
/// environment (decision 77), each with its application.
#[derive(Serialize)]
struct Info<'a> {
    points_of_sale: ByIntegration<Application<'a>>,
    vapid_public_key: String,
    version: &'static str,
}

#[derive(Serialize)]
struct Application<'a> {
    application_id: &'a str,
}

async fn info(State(shared): State<Arc<Shared>>) -> Response {
    let points_of_sale = ByIntegration(
        shared
            .adapters
            .iter()
            .map(|adapter| {
                (
                    adapter.integration(),
                    Application {
                        application_id: adapter.application_id(),
                    },
                )
            })
            .collect(),
    );
    Json(Info {
        points_of_sale,
        vapid_public_key: shared.vapid_key.public().to_base64url(),
        version: env!("CARGO_PKG_VERSION"),
    })
    .into_response()
}

async fn square_webhook(
    State((shared, square)): State<(Arc<Shared>, Arc<Square>)>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, ProblemDetails> {
    let integration = square.integration();
    let body = read(body, MAX_WEBHOOK).await?;
    let signature = headers
        .get(square::SIGNATURE_HEADER)
        .map(|value| value.as_bytes());
    square.verify(&body, signature).map_err(|error| {
        tracing::warn!(error = %error, environment = integration.environment(), "a webhook not signed by Square was refused");
        Problem::InvalidSignature(PointOfSale::Square).detail(format!(
            "{error}. Square signs each webhook for the URL its subscription names, which for {integration} must be {} exactly, with that application's key.",
            square.notification_url()
        ))
    })?;
    match square::read_notification(&body) {
        Ok(Notification::Completed {
            payment_id,
            merchant_id,
            location_id,
        }) => shared.dings.ring(Ring {
            integration,
            payment_id,
            merchant_id,
            location_id,
        }),
        Ok(Notification::Ignored) => {}
        // Square's own, so answered as taken: refusing it would only have
        // Square send it again.
        Err(error) => tracing::warn!(error = %error, "a signed webhook was dropped"),
    }
    Ok(StatusCode::OK)
}

/// A webhook from Clover: authenticated by its `X-Clover-Auth` header
/// alone, as Clover signs nothing (decision 49), each environment's with
/// its own Auth Code. Without the header, it may be the verification
/// handshake and nothing else: read no further than that takes, answered
/// `200` with the code logged for the operator, or `401` with nothing
/// logged, so nobody writes into the Bell's log.
async fn clover_webhook(
    State((shared, clover)): State<(Arc<Shared>, Arc<Clover>)>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, ProblemDetails> {
    let integration = clover.integration();
    let Some(header) = headers.get(clover::AUTH_HEADER) else {
        let body = axum::body::to_bytes(body, MAX_VERIFICATION).await.ok();
        return match body.as_deref().and_then(clover::read_verification) {
            Some(code) => {
                verification(integration, &code);
                Ok(StatusCode::OK)
            }
            None => Err(not_from_clover(integration, AuthError::Missing)),
        };
    };
    clover.verify(Some(header.as_bytes())).map_err(|error| {
        tracing::warn!(error = %error, environment = integration.environment(), "a webhook not from Clover was refused");
        not_from_clover(integration, error)
    })?;
    let body = read(body, MAX_WEBHOOK).await?;
    match clover.read_webhook(&body) {
        Ok(Incoming::Updates(rings)) => {
            for ring in rings {
                shared.dings.ring(ring);
            }
        }
        Ok(Incoming::Verification(code)) => verification(integration, &code),
        Ok(Incoming::OtherApplication) => {
            tracing::warn!(
                environment = integration.environment(),
                "another application's Clover webhook was dropped"
            );
        }
        // Clover's own, so answered as taken, as Square's are.
        Err(error) => tracing::warn!(error = %error, "a Clover webhook was dropped"),
    }
    Ok(StatusCode::OK)
}

/// Clover's verification code, for the operator to enter in the developer
/// dashboard of the integration it came to: the one value of a body the
/// Bell ever logs.
fn verification(integration: Integration, code: &VerificationCode) {
    let environment = integration.environment();
    tracing::info!(
        environment,
        "{integration} webhook verification code: {code} (enter it on the Webhooks page of the {environment} app's developer dashboard, then Verify)"
    );
}

fn not_from_clover(integration: Integration, error: AuthError) -> ProblemDetails {
    Problem::InvalidSignature(PointOfSale::Clover).detail(match error {
        AuthError::Missing => {
            "There is no x-clover-auth header: Clover sends the app's Auth Code with every webhook."
                .to_owned()
        }
        AuthError::Mismatch => {
            format!("The x-clover-auth header isn't this Bell's {integration} Auth Code.")
        }
    })
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
