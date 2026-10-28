//! The Monokular Bell: turns a Square application's payment webhooks into
//! dings for the tills registered at that location (`docs/design.md` §3).
//!
//! A till registers its Web Push subscription for one of its merchant's
//! locations, with the merchant's Square access token, which the Bell uses
//! once to ask Square whose the location is, then drops. When Square's
//! signed `payment.created` or `payment.updated` webhook says a payment
//! there completed, the Bell sends each of that location's tills a ding:
//! an encrypted `{"wake":"square"}` over Web Push (UnifiedPush directly, or
//! APNs and FCM through monokulo's push relay), which wakes the till to ask
//! Square for new payments itself.
//!
//! The Bell sees every webhook of its application, with each payment's
//! amount and card details. It reads a payment webhook's merchant,
//! location, payment id, status and whether it was refunded, and nothing
//! else; it keeps the ids of the payments it rang in memory for a few
//! minutes, so each rings once, and logs, stores or answers with none of
//! any body; it holds no Square token. Its own SQLite file holds the
//! registrations alone.
//!
//! [`settings`] declares its configuration, [`Bell::start`] builds it from
//! that, and [`Bell::router`] serves its four routes.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod cli;
mod dings;
mod problem;
mod registrations;
mod routes;
pub mod settings;
mod square;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use abuse::{ClientIdentity, TrustedProxies};
use outbound::Reach;
use push::{Sender, VapidKey};
use rate_limit::RateLimiter;

use crate::dings::Dings;
use crate::registrations::{RegistrationStore, StoreError};
use crate::settings::{ApplicationId, BellConfig, SquareEnvironment};
use crate::square::{Square, WebhookKey};

/// The `User-Agent` of every request the Bell makes.
pub(crate) const USER_AGENT: &str = concat!("monokular-bell/", env!("CARGO_PKG_VERSION"));

/// Why a Bell can't start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// A setting the Bell can't run without, unset.
    #[error("{0} must be set: {1}")]
    Unset(&'static str, &'static str),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the {0} client can't be built: {1}")]
    Client(&'static str, reqwest::Error),
    #[error("a ding's topic: {0}")]
    Topic(#[from] push::message::TopicError),
}

/// What every request reads.
pub(crate) struct Shared {
    /// `bell.public_uri` followed by the webhook path: what Square signs.
    notification_url: String,
    application_id: ApplicationId,
    environment: SquareEnvironment,
    webhook_key: WebhookKey,
    vapid_key: Arc<VapidKey>,
    trusted_proxies: TrustedProxies,
    reach: Reach,
    registrations: RateLimiter<ClientIdentity>,
    store: RegistrationStore,
    square: Square,
    dings: Arc<Dings>,
}

/// A Bell, ready to serve.
#[derive(Clone)]
pub struct Bell {
    shared: Arc<Shared>,
}

impl Bell {
    /// Opens the registrations' database at `bell.store_path` (making it if
    /// need be) and builds the clients for Square and the push services.
    pub async fn start(config: &BellConfig) -> Result<Bell, StartError> {
        let public_uri = config.public_uri.clone().ok_or(StartError::Unset(
            settings::PUBLIC_URI.key,
            "the Bell's address, which Square's webhooks are signed for",
        ))?;
        let application_id = config
            .square
            .application_id
            .clone()
            .ok_or(StartError::Unset(
                settings::SQUARE_APPLICATION_ID.key,
                "the Square application whose webhooks the Bell receives",
            ))?;
        // Both required: the settings don't load without them.
        let webhook_key = config.webhook_key.clone().ok_or(StartError::Unset(
            settings::SQUARE_WEBHOOK_KEY.env_var,
            "the webhook subscription's signature key",
        ))?;
        let vapid_key = config
            .vapid_key
            .as_ref()
            .map(|key| Arc::clone(key.key()))
            .ok_or(StartError::Unset(
                settings::VAPID_KEY.env_var,
                "the key every ding is signed with",
            ))?;
        let path = config.store_path.clone();
        let store = tokio::task::spawn_blocking(move || RegistrationStore::open(&path))
            .await
            .map_err(|error| {
                StartError::Store(StoreError::Open {
                    path: config.store_path.display().to_string(),
                    error: error.to_string(),
                })
            })??;
        let reach = Reach::allowing_private(config.allow_private_endpoints);
        // Redirects off, no proxy and the checking resolver, as monokulo
        // sends its webhooks and pushes: a ding endpoint could name an
        // address inside the network the Bell runs in.
        let client = outbound::client_builder(reach)
            .user_agent(USER_AGENT)
            .timeout(config.timeout)
            .build()
            .map_err(|error| StartError::Client("push", error))?;
        let sender = Sender::new(client, Arc::clone(&vapid_key), public_uri.contact());
        let dings = Dings::new(
            sender,
            store.clone(),
            reach,
            config.registration_failure_limit,
            config.dings_per_merchant_per_minute,
        )?;
        let square = Square::new(config.square.api_url.clone(), config.timeout)
            .map_err(|error| StartError::Client("Square", error))?;
        let notification_url = public_uri.notification_url();
        tracing::info!(
            bell.webhook_url = %notification_url,
            square.application = application_id.as_str(),
            square.environment = config.square.environment.as_str(),
            vapid_public_key = %vapid_key.public(),
            "the Bell is ready"
        );
        Ok(Bell {
            shared: Arc::new(Shared {
                notification_url,
                application_id,
                environment: config.square.environment,
                webhook_key,
                vapid_key,
                trusted_proxies: config.trusted_proxies.clone(),
                reach,
                registrations: RateLimiter::new(config.registrations_per_minute),
                store,
                square,
                dings: Arc::new(dings),
            }),
        })
    }

    /// The Bell's routes. Served with each connection's peer address
    /// ([`Bell::serve`] does), which the registration limit reads.
    pub fn router(&self) -> axum::Router {
        routes::router(Arc::clone(&self.shared))
    }

    /// Serves the Bell on `listener` until `shutdown` completes, then lets
    /// requests in flight finish.
    pub async fn serve(
        &self,
        listener: tokio::net::TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        axum::serve(
            listener,
            self.router()
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown)
        .await
    }

    /// Waits until every ding a webhook started has been sent, or has
    /// failed: what a stopping Bell waits for, after its last request.
    pub async fn dings_sent(&self) {
        self.shared.dings.idle().await;
    }
}
