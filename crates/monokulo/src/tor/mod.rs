//! Tor inside monokulo (`tor.mode = inside`): Arti, the Rust Tor, runs in
//! this process and serves an onion service in front of the same router
//! as the web listener, so visitors on Tor reach the same stores and
//! checkouts without anything else to install.
//!
//! [`TorReloadable`] starts, restarts and stops it when the Tor settings
//! are saved ([`crate::settings::TorConfig`]); [`Tor`] is the handle the
//! rest of monokulo reads: what Tor is doing ([`TorStatus`]) and the onion
//! address, once there is one.
//!
//! Connections arrive through a [`TorListener`]: Arti's side pushes each
//! accepted stream into it with the [`OnionPeer`] naming its rendezvous
//! circuit, the same identity an external tor's PROXY header gives
//! (`abuse::proxy_protocol`), so abuse protection counts each circuit as
//! one visitor either way. Tests push in-memory streams instead, with no
//! Tor at all.
//!
//! With an external tor (`tor.mode = external`) none of this runs: that
//! tor connects to the onion listener, and its address is `onion_uri`.

#[cfg(feature = "tor")]
mod arti;
mod listener;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use live_settings::{FieldError, Warning};

pub use listener::{circuit_peer, TorConnections, TorListener, TorStream};

use crate::abuse::proxy_protocol::OnionPeer;
use crate::settings::{TorConfig, TorMode, TOR_HIDDEN_SERVICE_DIR, TOR_STATE_DIR};

/// Whether this build can run Tor inside monokulo (its `tor` feature).
pub const BUILT_IN: bool = cfg!(feature = "tor");

/// The one onion service monokulo runs: its name in Arti's key store.
pub const NICKNAME: &str = "monokulo";

/// The file in `tor.state_dir` the onion address is written to once known,
/// as a C tor writes its HiddenServiceDir's `hostname`.
pub const HOSTNAME_FILE: &str = "hostname";

/// The file in a C tor HiddenServiceDir that holds the service's key.
pub const CTOR_SECRET_KEY_FILE: &str = "hs_ed25519_secret_key";

/// What Tor inside monokulo is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TorStatus {
    /// Not running: Tor is off or external.
    Off,
    /// Joining the Tor network: how far, out of 100, and what it is on.
    Starting { percent: u8, doing: String },
    /// The service runs at `address` and Tor is publishing it: usually
    /// reachable within a minute, though Arti only calls it running once
    /// every directory has it, which can take minutes. The address is fixed
    /// by the service's key, so links can use it already.
    Publishing { address: String },
    /// Reachable on Tor at `address` (`….onion`) since `since`.
    Running { address: String, since: i64 },
    /// Its onion address is known, but Tor says visitors may not reach it
    /// right now; it keeps trying.
    Degraded { address: String, problem: String },
    /// It stopped on an error, and starts again by itself.
    Failed { error: String, since: i64 },
}

impl TorStatus {
    /// The onion address (`….onion`), whenever one is known.
    pub fn address(&self) -> Option<&str> {
        match self {
            TorStatus::Running { address, .. }
            | TorStatus::Publishing { address }
            | TorStatus::Degraded { address, .. } => Some(address),
            _ => None,
        }
    }
}

/// The handle on Tor inside monokulo. Cheap to clone; clones share it.
#[derive(Clone, Default)]
pub struct Tor {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    status: parking_lot::RwLock<Option<TorStatus>>,
    /// The router to serve, once `main` has built it.
    router: OnceLock<axum::Router>,
    /// The running service: dropping it stops it.
    running: parking_lot::Mutex<Option<Running>>,
    /// A start asked for before the router existed.
    waiting: parking_lot::Mutex<Option<Wanted>>,
    /// The running Tor client, for fetching onion shops' verification
    /// files (`WellKnownOverTor`).
    #[cfg(feature = "tor")]
    client: parking_lot::Mutex<Option<arti::Client>>,
}

/// What a running service was started with, and how to stop it.
struct Running {
    wanted: Wanted,
    _stop: tokio::sync::watch::Sender<bool>,
}

/// The settings a service runs with: a change to any restarts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    pub state_dir: PathBuf,
    pub hidden_service_dir: Option<PathBuf>,
}

impl Tor {
    /// What Tor inside monokulo is doing now.
    pub fn status(&self) -> TorStatus {
        self.inner.status.read().clone().unwrap_or(TorStatus::Off)
    }

    /// The onion address (`….onion`) Tor inside monokulo serves, once known.
    pub fn onion_address(&self) -> Option<String> {
        self.status().address().map(str::to_string)
    }

    pub(crate) fn set_status(&self, status: TorStatus) {
        *self.inner.status.write() = Some(status);
    }

    /// Gives Tor the router to serve, starting a service that was asked
    /// for before the router existed.
    pub fn router_ready(&self, router: axum::Router) {
        let _ = self.inner.router.set(router);
        if let Some(wanted) = self.inner.waiting.lock().take() {
            self.start(wanted);
        }
    }

    /// A handle that says Tor is reachable at `address`, with nothing
    /// running: for tests of what depends on the onion address.
    #[cfg(any(test, feature = "test-support"))]
    pub fn running_at(address: &str) -> Self {
        let tor = Tor::default();
        tor.set_status(TorStatus::Running {
            address: address.to_string(),
            since: crate::now_unix(),
        });
        tor
    }

    fn start(&self, wanted: Wanted) {
        let Some(router) = self.inner.router.get().cloned() else {
            *self.inner.waiting.lock() = Some(wanted);
            return;
        };
        let (stop, stopped) = tokio::sync::watch::channel(false);
        self.set_status(TorStatus::Starting {
            percent: 0,
            doing: "Starting".to_string(),
        });
        self.spawn(wanted.clone(), router, stopped);
        *self.inner.running.lock() = Some(Running {
            wanted,
            _stop: stop,
        });
    }

    #[cfg(feature = "tor")]
    fn spawn(
        &self,
        wanted: Wanted,
        router: axum::Router,
        stopped: tokio::sync::watch::Receiver<bool>,
    ) {
        tokio::spawn(arti::run(self.clone(), wanted, router, stopped));
    }

    #[cfg(not(feature = "tor"))]
    fn spawn(
        &self,
        _wanted: Wanted,
        _router: axum::Router,
        _stopped: tokio::sync::watch::Receiver<bool>,
    ) {
        // `TorConfig` refuses `tor.mode = inside` in a build without Tor.
        self.set_status(TorStatus::Failed {
            error: "This monokulo was built without Tor inside it.".to_string(),
            since: crate::now_unix(),
        });
    }

    #[cfg(feature = "tor")]
    pub(crate) fn set_client(&self, client: Option<arti::Client>) {
        *self.inner.client.lock() = client;
    }

    fn stop(&self) {
        // Dropping the sender tells the service to stop.
        self.inner.running.lock().take();
        self.inner.waiting.lock().take();
        self.set_status(TorStatus::Off);
    }

    fn running_with(&self) -> Option<Wanted> {
        let running = self.inner.running.lock();
        running
            .as_ref()
            .map(|running| running.wanted.clone())
            .or_else(|| self.inner.waiting.lock().clone())
    }
}

/// Fetches onion shops' verification files (`embed_domains::Proofs`) over
/// Tor inside monokulo, never over the web: only while it runs.
pub struct WellKnownOverTor(pub Tor);

/// Why an onion shop can't be checked without Tor inside monokulo.
const NO_TOR: &str = "onion shops can be checked only while Tor runs inside monokulo (Admin settings, Tor: tor.mode = inside)";

impl crate::embed_domains::WellKnownFetch for WellKnownOverTor {
    fn fetch<'a>(
        &'a self,
        domain: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<String>, String>> + Send + 'a>,
    > {
        #[cfg(feature = "tor")]
        {
            let client = self.0.inner.client.lock().clone();
            Box::pin(async move {
                match client {
                    Some(client) => arti::fetch_well_known(client, domain).await,
                    None => Err(NO_TOR.to_string()),
                }
            })
        }
        #[cfg(not(feature = "tor"))]
        {
            let _ = domain;
            Box::pin(async { Err(NO_TOR.to_string()) })
        }
    }
}

/// Serves `router` to every connection `listener` accepts, each with its
/// [`OnionPeer`], until `stopped` turns true: what both the PROXY listener
/// for an external tor and Tor inside monokulo run.
pub async fn serve<L>(
    listener: L,
    router: axum::Router,
    mut stopped: tokio::sync::watch::Receiver<bool>,
) -> std::io::Result<()>
where
    L: axum::serve::Listener<Addr = OnionPeer>,
    OnionPeer: for<'a> axum::extract::connect_info::Connected<axum::serve::IncomingStream<'a, L>>,
{
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<OnionPeer>(),
    )
    .with_graceful_shutdown(async move {
        let _ = stopped.wait_for(|s| *s).await;
    })
    .await
}

/// Starts, restarts and stops Tor inside monokulo when the Tor settings are
/// saved. Only a change to what it runs with (the mode, `tor.state_dir`,
/// `tor.hidden_service_dir`) touches it; the other Tor settings are read
/// per request.
pub struct TorReloadable {
    tor: Tor,
}

impl TorReloadable {
    pub fn new(tor: Tor) -> Self {
        TorReloadable { tor }
    }
}

/// What saving the Tor settings does to Tor inside monokulo.
pub enum TorChange {
    Keep,
    Stop,
    Start(Wanted),
}

#[live_settings::async_trait]
impl live_settings::Reloadable for TorReloadable {
    type Config = TorConfig;
    type Prepared = TorChange;

    async fn prepare(
        &self,
        new: &TorConfig,
        _old: &TorConfig,
    ) -> Result<(TorChange, Vec<Warning>), FieldError> {
        if new.mode != TorMode::Inside {
            return Ok((TorChange::Stop, Vec::new()));
        }
        let wanted = Wanted {
            state_dir: new.state_dir.clone(),
            hidden_service_dir: new.hidden_service_dir.clone(),
        };
        if self.tor.running_with().as_ref() == Some(&wanted) {
            return Ok((TorChange::Keep, Vec::new()));
        }
        prepare_state_dir(&wanted.state_dir).map_err(|e| FieldError::new(TOR_STATE_DIR.key, e))?;
        if let Some(dir) = &wanted.hidden_service_dir {
            check_hidden_service_dir(dir)
                .map_err(|e| FieldError::new(TOR_HIDDEN_SERVICE_DIR.key, e))?;
        }
        Ok((TorChange::Start(wanted), Vec::new()))
    }

    async fn install(&self, change: TorChange) {
        match change {
            TorChange::Keep => {}
            TorChange::Stop => {
                if self.tor.running_with().is_some() {
                    tracing::info!("tor: stopping the onion service");
                }
                self.tor.stop();
            }
            TorChange::Start(wanted) => {
                self.tor.stop();
                self.tor.start(wanted);
            }
        }
    }

    fn boot_policy(&self) -> live_settings::BootPolicy {
        live_settings::BootPolicy::Exit
    }
}

/// Makes `dir` if it doesn't exist, readable by this user alone: it holds
/// the onion address's private key.
fn prepare_state_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("Can't make the folder {}: {e}.", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
            format!(
                "Can't make {} private to monokulo's user: {e}.",
                dir.display()
            )
        })?;
    }
    Ok(())
}

/// A HiddenServiceDir monokulo can take the key from.
fn check_hidden_service_dir(dir: &Path) -> Result<(), String> {
    let key = dir.join(CTOR_SECRET_KEY_FILE);
    match std::fs::metadata(&key) {
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(_) | Err(_) => Err(format!(
            "{} has no {CTOR_SECRET_KEY_FILE}: give the folder tor's HiddenServiceDir points at (a copy monokulo's user owns).",
            dir.display()
        )),
    }
}

#[cfg(test)]
mod tests;
