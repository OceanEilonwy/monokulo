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
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

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
    #[error("MONOKULO_RELAY_HANDLE_KEY must be set (handles.id_key, from the environment only): every handle's id is made under it. Generate one with `openssl rand -hex 32`")]
    NoHandleKey,
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
    /// The hourly sweep, which ends with the stop the relay was started
    /// with ([`Relay::stopped`]).
    sweep: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl Relay {
    /// Opens the handles' database at `store.path` (making it if need
    /// be), builds the APNs and FCM clients the settings ask for, and
    /// starts the hourly sweep of expired handles on this runtime, until
    /// `stop`: a relay that is started again (monokulo's admin page saving
    /// its settings) leaves no sweep of the old one behind.
    pub async fn start(config: &RelayConfig, stop: CancellationToken) -> Result<Relay, StartError> {
        let public_uri = config.public_uri.clone().ok_or(StartError::NoPublicUri)?;
        let path = config.store_path.clone();
        let key = config.handle_key.clone().ok_or(StartError::NoHandleKey)?;
        let lifetimes = config.handle_lifetimes;
        let store = tokio::task::spawn_blocking(move || HandleStore::open(&path, lifetimes, key))
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
        // The sweep's supervisor stops once the sender goes, which it does
        // with the stop.
        let (stopping, stopped) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            stop.cancelled().await;
            drop(stopping);
        });
        let sweep =
            service_runtime::supervise::supervise_until("push relay sweep", stopped, move || {
                sweep(sweeping.clone())
            });
        Ok(Relay {
            sweep: Arc::new(Mutex::new(Some(sweep))),
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

    /// Handles stored that haven't expired by `now`: what the admin page
    /// shows of a relay inside monokulo.
    pub async fn handles_stored(&self, now: i64) -> Result<u64, StoreError> {
        self.shared.store.count(now).await
    }

    /// Waits for the sweep to end, once the stop it was started with has
    /// come.
    pub async fn stopped(&self) {
        let sweep = self.sweep.lock().take();
        if let Some(sweep) = sweep {
            if let Err(error) = sweep.await {
                tracing::warn!(error = %error, "the push relay's sweep ended badly");
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{AppField, AppRow, Apps, Host};

    /// A relay's settings with its database in a directory of the test's
    /// own.
    async fn config(tag: &str) -> RelayConfig {
        let path = std::env::temp_dir()
            .join(format!(
                "push-relay-lib-{tag}-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ))
            .join("relay.db");
        settings::tests::load_with(
            &[
                ("server.public_uri", "https://relay.example.org"),
                ("store.path", path.to_str().unwrap()),
            ],
            &[("MONOKULO_RELAY_HANDLE_KEY", &"5a".repeat(16))],
            Host::Embedded,
        )
        .await
        .unwrap()
    }

    fn sweep_restarts() -> u64 {
        service_runtime::supervise::restart_counts()
            .into_iter()
            .find(|(name, _)| *name == "push relay sweep")
            .map_or(0, |(_, count)| count)
    }

    /// The sweep lives as long as the stop the relay was started with,
    /// and its supervisor doesn't start it again after the stop: a relay
    /// started again and again (each save on monokulo's admin page)
    /// leaves no sweep of the old one running.
    #[tokio::test]
    async fn the_sweep_stops_with_the_relay() {
        let restarts = sweep_restarts();
        let stop = CancellationToken::new();
        let relay = Relay::start(&config("sweep").await, stop.clone())
            .await
            .unwrap();
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(2), relay.stopped())
            .await
            .expect("the sweep ended with the stop");
        assert!(relay.sweep.lock().is_none());
        // A second wait has nothing left to wait for.
        relay.stopped().await;
        assert_eq!(sweep_restarts(), restarts);
    }

    /// The handles stored are counted until each expires.
    #[tokio::test]
    async fn the_handles_stored_are_counted_until_they_expire() {
        let relay = Relay::start(&config("count").await, CancellationToken::new())
            .await
            .unwrap();
        let now = service_runtime::time::now_unix();
        assert_eq!(relay.handles_stored(now).await.unwrap(), 0);
        let sender = push::VapidKey::generate();
        for token in ["00ab", "00cd"] {
            relay
                .shared
                .store
                .register(
                    handles::NewHandle {
                        platform: handles::Platform::Apns,
                        app: settings::AppName::try_from("monokular".to_owned()).unwrap(),
                        token: handles::DeviceToken::parse(handles::Platform::Apns, token).unwrap(),
                        sender: sender.public(),
                    },
                    now,
                )
                .await
                .unwrap();
        }
        assert_eq!(relay.handles_stored(now).await.unwrap(), 2);
        // Unused handles last a week (`handles.unused_ttl_days`).
        assert_eq!(relay.handles_stored(now + 8 * 86_400).await.unwrap(), 0);
    }

    fn row(name: &str, apns_topic: &str, fcm_project_id: &str) -> AppRow {
        AppRow {
            name: name.into(),
            apns_topic: apns_topic.into(),
            fcm_project_id: fcm_project_id.into(),
        }
    }

    /// The apps from a form's rows: blank rows are none, each other row
    /// a whole app, and each problem is the row's and the field's.
    #[test]
    fn apps_come_from_rows_and_each_rows_problem_is_its_own() {
        let apps = Apps::from_rows(&[
            row("monokular", "com.example.monokular", "monokular-test"),
            row("", " ", ""),
            row("till", "", "till-project"),
        ])
        .unwrap();
        assert_eq!(
            apps.rows(),
            [
                row("monokular", "com.example.monokular", "monokular-test"),
                row("till", "", "till-project")
            ]
        );
        assert_eq!(
            Apps::from_rows(&apps.rows()).unwrap().to_setting(),
            apps.to_setting()
        );

        let errors = Apps::from_rows(&[
            row("Monokular", "com.example.monokular", ""),
            row("till", "", ""),
            row("till", "not a bundle", "UPPER"),
            row("", "com.example.x", ""),
        ])
        .unwrap_err();
        let found: Vec<(usize, AppField)> = errors.iter().map(|e| (e.row, e.field)).collect();
        assert_eq!(
            found,
            [
                (0, AppField::Name),
                (1, AppField::ApnsTopic),
                (2, AppField::Name),
                (2, AppField::ApnsTopic),
                (2, AppField::FcmProjectId),
                (3, AppField::Name),
            ]
        );
        assert_eq!(
            errors[1].message,
            "needs an iOS bundle ID or a Firebase project ID, or both."
        );
        assert_eq!(errors[2].message, "till is listed twice.");
    }
}
