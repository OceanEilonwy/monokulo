//! The Monokular Bell: turns a point of sale's payment webhooks into dings
//! for the tills registered at that location (`docs/design.md` §3). It
//! serves Square, Clover, or both: each point of sale is an adapter, on
//! when its settings name an application (`[bell.square]`,
//! `[bell.clover]`).
//!
//! A till registers its Web Push subscription for its point of sale and
//! one of its merchant's locations (for Clover, the merchant itself), with
//! the merchant's access token there, which the Bell uses once to ask the
//! point of sale whose the location is, then drops. When the point of
//! sale's webhook says a payment was taken there, the Bell sends each of
//! that location's tills a ding: an encrypted `{"wake":"square"}` or
//! `{"wake":"clover"}` over Web Push (UnifiedPush directly, or APNs and FCM
//! through monokulo's push relay), which wakes the till to ask its point of
//! sale for new payments itself.
//!
//! The Bell sees every webhook of its applications: Square's with each
//! payment's amount and card details, Clover's with ids alone. It reads a
//! payment webhook's merchant, location, payment id and (Square's) status
//! and whether it was refunded, and nothing else; it keeps the ids of the
//! payments it rang in memory for a few minutes, so each rings once, and
//! logs, stores or answers with none of any body (Clover's one-off
//! verification code aside, logged for the operator); it holds no token.
//! Its own SQLite file holds the registrations alone.
//!
//! [`settings`] declares its configuration, [`Bell::start`] builds it from
//! that, and [`Bell::router`] serves its routes.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod cli;
mod clover;
mod dings;
mod pos;
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

use crate::clover::Clover;
use crate::dings::Dings;
use crate::pos::PointOfSale;
use crate::registrations::{RegistrationStore, StoreError};
use crate::settings::BellConfig;
use crate::square::Square;

/// The `User-Agent` of every request the Bell makes.
pub(crate) const USER_AGENT: &str = concat!("monokular-bell/", env!("CARGO_PKG_VERSION"));

/// Why a Bell can't start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// A setting the Bell can't run without, unset.
    #[error("{0} must be set: {1}")]
    Unset(&'static str, &'static str),
    /// No point of sale is on.
    #[error(
        "bell.square.application_id or bell.clover.application_id (or both) must be set: the Bell serves no point of sale without one"
    )]
    NoPointOfSale,
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the {0} client can't be built: {1}")]
    Client(&'static str, reqwest::Error),
    #[error("a ding's topic: {0}")]
    Topic(#[from] push::message::TopicError),
}

/// The points of sale this Bell serves, each one's adapter: at least one.
pub(crate) struct Adapters {
    square: Option<Arc<Square>>,
    clover: Option<Arc<Clover>>,
}

impl Adapters {
    /// The points of sale that are on, in [`PointOfSale::ALL`]'s order.
    fn on(&self) -> Vec<PointOfSale> {
        PointOfSale::ALL
            .into_iter()
            .filter(|pos| match pos {
                PointOfSale::Square => self.square.is_some(),
                PointOfSale::Clover => self.clover.is_some(),
            })
            .collect()
    }
}

/// What every request reads.
pub(crate) struct Shared {
    adapters: Adapters,
    vapid_key: Arc<VapidKey>,
    trusted_proxies: TrustedProxies,
    reach: Reach,
    registrations: RateLimiter<ClientIdentity>,
    store: RegistrationStore,
    dings: Arc<Dings>,
}

/// A Bell, ready to serve.
#[derive(Clone)]
pub struct Bell {
    shared: Arc<Shared>,
}

impl Bell {
    /// Builds the adapters of the points of sale that are on, opens the
    /// registrations' database at `bell.store_path` (making it if need be)
    /// and builds the client for the push services.
    pub async fn start(config: &BellConfig) -> Result<Bell, StartError> {
        let public_uri = config.public_uri.clone().ok_or(StartError::Unset(
            settings::PUBLIC_URI.key,
            "the Bell's address, which the points of sale send their webhooks to",
        ))?;
        // A secret is set with its application, or the settings don't
        // load: the `Unset`s are for a config made some other way.
        let square = match (&config.square.application_id, &config.square.webhook_key) {
            (None, _) => None,
            (Some(_), None) => {
                return Err(StartError::Unset(
                    settings::SQUARE_WEBHOOK_KEY.env_var,
                    "the Square webhook subscription's signature key",
                ));
            }
            (Some(application_id), Some(webhook_key)) => {
                let square = Square::new(
                    config.square.api_url.clone(),
                    application_id.clone(),
                    config.square.environment,
                    public_uri.notification_url(PointOfSale::Square),
                    webhook_key.clone(),
                    config.timeout,
                )
                .map_err(|error| StartError::Client("Square", error))?;
                tracing::info!(
                    bell.webhook_url = square.notification_url(),
                    square.application = square.application_id().as_str(),
                    square.environment = square.environment().as_str(),
                    "serving Square"
                );
                Some(Arc::new(square))
            }
        };
        let clover = match (&config.clover.application_id, &config.clover.auth_code) {
            (None, _) => None,
            (Some(_), None) => {
                return Err(StartError::Unset(
                    settings::CLOVER_AUTH_CODE.env_var,
                    "the Clover app's Auth Code",
                ));
            }
            (Some(application_id), Some(auth_code)) => {
                let clover = Clover::new(
                    config.clover.api_url.clone(),
                    application_id.clone(),
                    config.clover.environment,
                    auth_code.clone(),
                    config.timeout,
                )
                .map_err(|error| StartError::Client("Clover", error))?;
                tracing::info!(
                    bell.webhook_url = public_uri.notification_url(PointOfSale::Clover),
                    clover.application = clover.application_id().as_str(),
                    clover.environment = clover.environment().as_str(),
                    "serving Clover"
                );
                Some(Arc::new(clover))
            }
        };
        if square.is_none() && clover.is_none() {
            return Err(StartError::NoPointOfSale);
        }
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
        tracing::info!(vapid_public_key = %vapid_key.public(), "the Bell is ready");
        Ok(Bell {
            shared: Arc::new(Shared {
                adapters: Adapters { square, clover },
                vapid_key,
                trusted_proxies: config.trusted_proxies.clone(),
                reach,
                registrations: RateLimiter::new(config.registrations_per_minute),
                store,
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
