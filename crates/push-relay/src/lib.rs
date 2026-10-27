//! The push relay: a Web Push service (RFC 8030) whose endpoints lead to
//! APNs and FCM, so a sender that only speaks Web Push (a monokulo, a
//! Monokular Bell) can reach an iPhone or an Android phone with Google's
//! services (monokular `docs/design.md` §5).
//!
//! An app registers its device token with the relay, naming the sender
//! whose pushes it wants by that sender's VAPID public key, and is given an
//! endpoint under the relay's address (`{public_uri}/v1/push/ph_…`). It
//! hands that endpoint to the sender as its push subscription's. A push to
//! it is accepted only when signed (VAPID, RFC 8292) by that same key and
//! for this relay, and is handed on to APNs or FCM as the handle's
//! platform needs. Ordinary pushes are ciphertext the
//! relay can't read (RFC 8291); only a Live Activity's state is plain
//! JSON, as iOS applies it with no code of the app's in between.
//!
//! The relay keeps its handles in its own SQLite file and nothing else: no
//! push is stored, and no line it logs carries a body, a token or an
//! endpoint.
//!
//! It runs inside monokulo (cargo feature `relay`, on when `relay.enabled`)
//! or on its own (`monokulo-push-relay`); both build a [`Relay`] from the
//! same settings ([`settings`]) and serve it on its own listener.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod apns;
pub mod cli;
mod fcm;
mod handles;
mod limits;
mod problem;
mod routes;
pub mod settings;
mod upstream;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use abuse::TrustedProxies;

use crate::apns::Apns;
use crate::fcm::Fcm;
use crate::handles::{HandleStore, StoreError};
use crate::limits::Limits;
use crate::settings::{Apps, PublicUri, RelayConfig};

/// How often expired handles are deleted. Until then they are already
/// unknown to every route; the sweep only stops their tokens lingering in
/// the file.
const SWEEP_EVERY: Duration = Duration::from_secs(60 * 60);

/// Why a relay can't start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("server.public_uri must be set: it is the address every endpoint the relay hands out is under, and the one pushes are signed for")]
    NoPublicUri,
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the {0} client can't be built: {1}")]
    Client(&'static str, reqwest::Error),
}

/// What every request reads.
pub(crate) struct Shared {
    public_uri: PublicUri,
    trusted_proxies: TrustedProxies,
    apps: Apps,
    store: HandleStore,
    limits: Limits,
    apns: Option<Apns>,
    fcm: Option<Fcm>,
}

/// A relay, ready to serve.
#[derive(Clone)]
pub struct Relay {
    shared: Arc<Shared>,
}

impl Relay {
    /// Opens the handles' database at `store.path` (making it if need
    /// be), builds the APNs and FCM clients the settings ask for, and
    /// starts the hourly sweep of expired handles on this runtime.
    pub async fn start(config: &RelayConfig) -> Result<Relay, StartError> {
        let public_uri = config.public_uri.clone().ok_or(StartError::NoPublicUri)?;
        let path = config.store_path.clone();
        let lifetimes = config.handle_lifetimes;
        let store = tokio::task::spawn_blocking(move || HandleStore::open(&path, lifetimes))
            .await
            .map_err(|error| {
                StartError::Store(StoreError::Open {
                    path: config.store_path.display().to_string(),
                    error: error.to_string(),
                })
            })??;
        let apns = config
            .apns
            .as_ref()
            .map(|apns| Apns::new(apns, config.upstream_timeout))
            .transpose()
            .map_err(|error| StartError::Client("APNs", error))?;
        let fcm = config
            .fcm
            .as_ref()
            .map(|fcm| Fcm::new(fcm, config.upstream_timeout))
            .transpose()
            .map_err(|error| StartError::Client("FCM", error))?;
        tracing::info!(
            relay.origin = %public_uri.origin(),
            relay.apns = apns.is_some(),
            relay.fcm = fcm.is_some(),
            "push relay ready"
        );
        let sweeping = store.clone();
        service_runtime::supervise::supervise("push relay sweep", move || sweep(sweeping.clone()));
        Ok(Relay {
            shared: Arc::new(Shared {
                public_uri,
                trusted_proxies: config.trusted_proxies.clone(),
                apps: config.apps.clone(),
                store,
                limits: Limits::new(config.limits),
                apns,
                fcm,
            }),
        })
    }

    /// The relay's routes. Served with each connection's peer address
    /// ([`Relay::serve`] does), which the registration limit reads.
    pub fn router(&self) -> axum::Router {
        routes::router(Arc::clone(&self.shared))
    }

    /// Serves the relay on `listener` until `shutdown` completes, then
    /// lets requests in flight finish.
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
}

/// Deletes expired handles, every [`SWEEP_EVERY`].
async fn sweep(store: HandleStore) {
    let mut every = tokio::time::interval(SWEEP_EVERY);
    loop {
        every.tick().await;
        match store.sweep(service_runtime::time::now_unix()).await {
            Ok(0) => {}
            Ok(swept) => tracing::info!(swept, "expired handles deleted"),
            Err(error) => tracing::warn!(error = %error, "expired handles couldn't be deleted"),
        }
    }
}
