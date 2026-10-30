//! The Monokular Bell: turns a point of sale's payment webhooks into dings
//! for the tills registered at that location (`docs/bell.md`). It
//! serves Square, Clover, or both, in as many of their environments at
//! once as are on: each integration (a point of sale's environment) is an
//! adapter, on when its settings name an application
//! (`[bell.square.production]`, `[bell.clover.europe]`).
//!
//! A till registers its Web Push subscription for its integration and one
//! of its merchant's locations (for Clover, the merchant itself), with the
//! merchant's access token there, which the Bell uses once to ask the
//! point of sale whose the location is, then drops. When the integration's
//! webhook says a payment was taken there, the Bell sends each of that
//! location's tills a ding: an encrypted `{"wake":"square"}` or
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
pub mod status;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use abuse::{ClientIdentity, TrustedProxies};
use outbound::Reach;
use push::{Sender, Topic, VapidKey};
use rate_limit::RateLimiter;

use crate::clover::Clover;
use crate::dings::Dings;
pub use crate::pos::{
    ByIntegration, CloverEnvironment, Integration, PointOfSale, SquareEnvironment,
};
use crate::registrations::{RegistrationStore, StoreError};
use crate::settings::{BellConfig, PublicUri};
use crate::square::Square;

/// The `User-Agent` of every request the Bell makes.
pub(crate) const USER_AGENT: &str = concat!("monokulo-bell/", env!("CARGO_PKG_VERSION"));

/// Why a Bell can't start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// A setting the Bell can't run without, unset.
    #[error("{0} must be set: {1}")]
    Unset(&'static str, &'static str),
    /// No integration is on.
    #[error(
        "an integration's application_id must be set, such as bell.square.production.application_id or bell.clover.north_america.application_id (one or several): the Bell serves no point of sale without one"
    )]
    NoIntegration,
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the {0} client can't be built: {1}")]
    Client(String, reqwest::Error),
    #[error("a ding's topic: {0}")]
    Topic(#[from] push::message::TopicError),
}

/// One integration's adapter.
pub(crate) enum Adapter {
    Square(Arc<Square>),
    Clover(Arc<Clover>),
}

impl Adapter {
    pub(crate) fn integration(&self) -> Integration {
        match self {
            Adapter::Square(square) => square.integration(),
            Adapter::Clover(clover) => clover.integration(),
        }
    }

    pub(crate) fn application_id(&self) -> &str {
        match self {
            Adapter::Square(square) => square.application_id().as_str(),
            Adapter::Clover(clover) => clover.application_id().as_str(),
        }
    }
}

/// The integrations this Bell serves, each one's adapter, in
/// [`Integration::ALL`]'s order: at least one.
pub(crate) struct Adapters(Vec<Adapter>);

impl Adapters {
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Adapter> {
        self.0.iter()
    }

    /// `integration`'s adapter, when it is on.
    pub(crate) fn get(&self, integration: Integration) -> Option<&Adapter> {
        self.0
            .iter()
            .find(|adapter| adapter.integration() == integration)
    }

    /// The integrations that are on.
    pub(crate) fn on(&self) -> impl Iterator<Item = Integration> + '_ {
        self.0.iter().map(Adapter::integration)
    }
}

/// What every request reads.
pub(crate) struct Shared {
    adapters: Adapters,
    vapid_key: Arc<VapidKey>,
    /// `bell.public_uri`, for the status.
    public_uri: PublicUri,
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

/// A Bell's settings, checked as a start checks them, with the adapters
/// and the push client it would run on: [`Bell::check`], the half of a
/// start that opens no file and reaches nothing (`--check`), whose result
/// the other half ([`Bell::start`]) runs on.
pub struct Checked<'a> {
    config: &'a BellConfig,
    public_uri: PublicUri,
    adapters: Adapters,
    vapid_key: Arc<VapidKey>,
    reach: Reach,
    sender: Sender,
    topic: Topic,
}

impl Checked<'_> {
    /// What the Bell would run on, a line each, as `--check` says it: its
    /// addresses, each integration that is on with its application and
    /// webhook URL, the ones that are off in one line, and the VAPID public
    /// key the tills subscribe for. Never a secret.
    pub fn facts(&self) -> Vec<String> {
        let mut facts = vec![
            format!("listens on {}", self.config.listen),
            format!("public address {}", self.public_uri.as_str()),
        ];
        for adapter in self.adapters.iter() {
            let integration = adapter.integration();
            facts.push(format!(
                "{integration}: on, application {}, webhooks to {}",
                adapter.application_id(),
                self.public_uri.notification_url(integration)
            ));
        }
        let off: Vec<Integration> = Integration::ALL
            .into_iter()
            .filter(|integration| self.adapters.get(*integration).is_none())
            .collect();
        if !off.is_empty() {
            facts.push(format!("off: {}", pos::in_words(off)));
        }
        facts.push(format!("VAPID public key {}", self.vapid_key.public()));
        facts.push(format!(
            "registrations in {}",
            self.config.store_path.display()
        ));
        facts
    }
}

impl Bell {
    /// Everything a start checks that needs no file and no network: the
    /// address, the integrations that are on, the VAPID key, and the
    /// clients for the points of sale and the push services. `--check`
    /// stops here; [`Bell::start`] begins here, so the two can't disagree.
    pub fn check(config: &BellConfig) -> Result<Checked<'_>, StartError> {
        let public_uri = config.public_uri.clone().ok_or(StartError::Unset(
            settings::PUBLIC_URI.key,
            "the Bell's address, which the points of sale send their webhooks to",
        ))?;
        let client_error = |integration: Integration| {
            move |error| StartError::Client(integration.to_string(), error)
        };
        let mut adapters = Vec::new();
        for square in &config.square {
            let integration = Integration::Square(square.environment);
            adapters.push(Adapter::Square(Arc::new(
                Square::new(
                    square.api_url.clone(),
                    square.application_id.clone(),
                    square.environment,
                    public_uri.notification_url(integration),
                    square.webhook_key.clone(),
                    config.timeout,
                )
                .map_err(client_error(integration))?,
            )));
        }
        for clover in &config.clover {
            let integration = Integration::Clover(clover.environment);
            adapters.push(Adapter::Clover(Arc::new(
                Clover::new(
                    clover.api_url.clone(),
                    clover.application_id.clone(),
                    clover.environment,
                    clover.auth_code.clone(),
                    config.timeout,
                )
                .map_err(client_error(integration))?,
            )));
        }
        if adapters.is_empty() {
            return Err(StartError::NoIntegration);
        }
        let vapid_key = config
            .vapid_key
            .as_ref()
            .map(|key| Arc::clone(key.key()))
            .ok_or(StartError::Unset(
                settings::VAPID_KEY.env_var,
                "the key every ding is signed with",
            ))?;
        let reach = Reach::allowing_private(config.allow_private_endpoints);
        // Redirects off, no proxy and the checking resolver, as monokulo
        // sends its webhooks and pushes: a ding endpoint could name an
        // address inside the network the Bell runs in.
        let client = outbound::client_builder(reach)
            .user_agent(USER_AGENT)
            .timeout(config.timeout)
            .build()
            .map_err(|error| StartError::Client("push".to_owned(), error))?;
        let sender = Sender::new(client, Arc::clone(&vapid_key), public_uri.contact());
        Ok(Checked {
            config,
            public_uri,
            adapters: Adapters(adapters),
            vapid_key,
            reach,
            sender,
            topic: Dings::topic()?,
        })
    }

    /// [`Bell::check`]s the settings, then opens the registrations'
    /// database at `bell.store_path` (making it if need be).
    pub async fn start(config: &BellConfig) -> Result<Bell, StartError> {
        let Checked {
            public_uri,
            adapters,
            vapid_key,
            reach,
            sender,
            topic,
            ..
        } = Bell::check(config)?;
        for adapter in adapters.iter() {
            let integration = adapter.integration();
            tracing::info!(
                bell.webhook_url = public_uri.notification_url(integration),
                bell.point_of_sale = %integration.point_of_sale(),
                bell.environment = integration.environment(),
                bell.application = adapter.application_id(),
                "serving {integration}"
            );
        }
        let path = config.store_path.clone();
        let store = tokio::task::spawn_blocking(move || RegistrationStore::open(&path))
            .await
            .map_err(|error| {
                StartError::Store(StoreError::Open {
                    path: config.store_path.display().to_string(),
                    error: error.to_string(),
                })
            })??;
        let dings = Dings::new(
            sender,
            topic,
            store.clone(),
            reach,
            config.registration_failure_limit,
            config.dings_per_merchant_per_minute,
        );
        tracing::info!(vapid_public_key = %vapid_key.public(), "the Bell is ready");
        Ok(Bell {
            shared: Arc::new(Shared {
                adapters,
                vapid_key,
                public_uri,
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
