//! Tor inside monokulo (`tor.mode = inside`): Arti, the Rust Tor, runs in
//! this process and serves an onion service in front of the same router
//! as the web listener, so visitors on Tor reach the same stores and
//! checkouts without anything else to install.
//!
//! [`Tor::prepare`] and [`Tor::install`] start, restart and stop it when
//! the Tor settings are saved (monokulo's `settings::TorReloadable`, which
//! reads `settings::TorConfig` into a [`Wanted`]); [`Tor`] is the handle
//! the rest of monokulo reads: what Tor is doing ([`TorStatus`]) and the
//! onion address, once there is one.
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
//! [`serve`] serves both. [`onion`] checks onion addresses.
//!
//! Without the `arti` feature (monokulo's `tor`) none of Arti is built:
//! [`BUILT_IN`] is false, and a service asked for fails saying so.

#[cfg(feature = "arti")]
mod arti;
mod listener;
pub mod onion;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

pub use listener::{circuit_peer, TorConnections, TorListener, TorStream};

use abuse::proxy_protocol::OnionPeer;

/// Whether this build can run Tor inside monokulo (monokulo's `tor`
/// feature, this crate's `arti`).
pub const BUILT_IN: bool = cfg!(feature = "arti");

/// The one onion service monokulo runs: its name in Arti's key store.
pub const NICKNAME: &str = "monokulo";

/// Its name when it serves a HiddenServiceDir's key (`tor.hidden_service_dir`):
/// one Arti never makes a key under. Arti looks in its own key store before
/// the HiddenServiceDir, so a key it made under [`NICKNAME`] on an earlier
/// run would otherwise be served instead, at another address.
pub const CTOR_NICKNAME: &str = "monokulo-ctor";

/// The file in `tor.state_dir` the onion address is written to once known,
/// as a C tor writes its HiddenServiceDir's `hostname`.
pub const HOSTNAME_FILE: &str = "hostname";

/// The file in `tor.state_dir` that says what Tor inside monokulo is doing
/// ([`TorStatus::report`]), rewritten whenever that changes, for what
/// watches monokulo from outside (the OpenWrt app). Unlike the hostname
/// file it also says when Tor is off.
pub const STATUS_FILE: &str = "status";

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

    /// The [`STATUS_FILE`]'s contents: `state=` (off, starting,
    /// publishing, running, degraded or failed), with `address=` whenever
    /// the onion address is known and `percent=` while starting.
    pub fn report(&self) -> String {
        let state = match self {
            TorStatus::Off => "off",
            TorStatus::Starting { .. } => "starting",
            TorStatus::Publishing { .. } => "publishing",
            TorStatus::Running { .. } => "running",
            TorStatus::Degraded { .. } => "degraded",
            TorStatus::Failed { .. } => "failed",
        };
        let mut report = format!("state={state}\n");
        if let Some(address) = self.address() {
            report.push_str(&format!("address={address}\n"));
        }
        if let TorStatus::Starting { percent, .. } = self {
            report.push_str(&format!("percent={percent}\n"));
        }
        report
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
    /// The `tor.state_dir` whose [`STATUS_FILE`] reports the status, once
    /// the settings named it ([`Tor::report_in`]), and what was last
    /// written there.
    reported: parking_lot::Mutex<Option<(PathBuf, String)>>,
    /// The router to serve, once `main` has built it.
    router: OnceLock<axum::Router>,
    /// The running service: dropping it stops it.
    running: parking_lot::Mutex<Option<Running>>,
    /// A start asked for before the router existed.
    waiting: parking_lot::Mutex<Option<Wanted>>,
    /// The running Tor client, for fetching onion shops' verification
    /// files ([`Tor::fetch_over_tor`]).
    #[cfg(feature = "arti")]
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
        let report = status.report();
        *self.inner.status.write() = Some(status);
        let mut reported = self.inner.reported.lock();
        if let Some((dir, last)) = reported.as_mut() {
            if *last != report {
                write_status_file(dir, &report);
                *last = report;
            }
        }
    }

    /// Reports the status in `state_dir`'s [`STATUS_FILE`] from now on:
    /// the `tor.state_dir` setting, whatever the mode, so the file says
    /// "off" too. A folder it reported in before is told "off", since
    /// nothing reports there any more.
    pub fn report_in(&self, state_dir: &Path) {
        // Locked before the status is read: a change made meanwhile waits,
        // then finds this report and writes over it.
        let mut reported = self.inner.reported.lock();
        let report = self.status().report();
        match reported.as_ref() {
            Some((dir, last)) if dir == state_dir && *last == report => return,
            Some((dir, _)) if dir != state_dir => {
                write_status_file(dir, &TorStatus::Off.report());
            }
            _ => {}
        }
        write_status_file(state_dir, &report);
        *reported = Some((state_dir.to_path_buf(), report));
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
        Tor::with_status(TorStatus::Running {
            address: address.to_string(),
            since: service_runtime::time::now_unix(),
        })
    }

    /// A handle that says Tor is doing `status`, with nothing running: for
    /// tests of what depends on it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_status(status: TorStatus) -> Self {
        let tor = Tor::default();
        tor.set_status(status);
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

    #[cfg(feature = "arti")]
    fn spawn(
        &self,
        wanted: Wanted,
        router: axum::Router,
        stopped: tokio::sync::watch::Receiver<bool>,
    ) {
        tokio::spawn(arti::run(self.clone(), wanted, router, stopped));
    }

    #[cfg(not(feature = "arti"))]
    fn spawn(
        &self,
        _wanted: Wanted,
        _router: axum::Router,
        _stopped: tokio::sync::watch::Receiver<bool>,
    ) {
        // monokulo's `TorConfig` refuses `tor.mode = inside` in a build
        // without Tor ([`BUILT_IN`]).
        self.set_status(TorStatus::Failed {
            error: "This monokulo was built without Tor inside it.".to_string(),
            since: service_runtime::time::now_unix(),
        });
    }

    #[cfg(feature = "arti")]
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

/// Why an onion shop can't be checked without Tor inside monokulo.
const NO_TOR: &str = "onion shops can be checked only while Tor runs inside monokulo (Admin settings, Tor: tor.mode = inside)";

impl Tor {
    /// `path` on the onion service `domain`, fetched over Tor inside
    /// monokulo on a circuit of its own, never over the web: only while it
    /// runs. `Ok(None)` for any answer but a 200 (redirects aren't
    /// followed) or one bigger than `max_bytes`. What checking an onion
    /// shop's verification file does (monokulo's
    /// `embed_domains::WellKnownOverTor`).
    pub fn fetch_over_tor<'a>(
        &self,
        domain: &'a str,
        path: &'static str,
        max_bytes: usize,
    ) -> impl std::future::Future<Output = Result<Option<String>, String>> + Send + 'a {
        #[cfg(feature = "arti")]
        {
            let client = self.inner.client.lock().clone();
            async move {
                match client {
                    Some(client) => arti::fetch(client, domain, path, max_bytes).await,
                    None => Err(NO_TOR.to_string()),
                }
            }
        }
        #[cfg(not(feature = "arti"))]
        {
            let _ = (domain, path, max_bytes);
            async { Err(NO_TOR.to_string()) }
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

/// What saving the Tor settings does to Tor inside monokulo.
pub enum TorChange {
    Keep,
    Stop,
    Start(Wanted),
}

/// The setting a refused [`Tor::prepare`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WantedField {
    /// [`Wanted::state_dir`] (`tor.state_dir`).
    StateDir,
    /// [`Wanted::hidden_service_dir`] (`tor.hidden_service_dir`).
    HiddenServiceDir,
}

/// Why Tor inside monokulo can't run with what was asked for, and which
/// setting to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub field: WantedField,
    pub message: String,
}

impl Tor {
    /// What saving the Tor settings does to Tor inside monokulo: `wanted`
    /// is what they ask it to run with, `None` when they don't ask for it
    /// (off, or an external tor). Only a change to what it runs with
    /// touches it; the other Tor settings are read per request. Makes the
    /// state folder and checks the HiddenServiceDir's key, leaving what
    /// runs as it is until [`Tor::install`].
    pub fn prepare(&self, wanted: Option<Wanted>) -> Result<TorChange, Refused> {
        let refused = |field| move |message| Refused { field, message };
        let Some(wanted) = wanted else {
            return Ok(TorChange::Stop);
        };
        if self.running_with().as_ref() == Some(&wanted) {
            return Ok(TorChange::Keep);
        }
        prepare_state_dir(&wanted.state_dir).map_err(refused(WantedField::StateDir))?;
        if let Some(dir) = &wanted.hidden_service_dir {
            check_hidden_service_dir(dir).map_err(refused(WantedField::HiddenServiceDir))?;
            #[cfg(feature = "arti")]
            arti::address_to_serve(&wanted).map_err(refused(WantedField::HiddenServiceDir))?;
        }
        Ok(TorChange::Start(wanted))
    }

    /// Puts a [`Tor::prepare`]d change in place.
    pub fn install(&self, change: TorChange) {
        match change {
            TorChange::Keep => {}
            TorChange::Stop => {
                if self.running_with().is_some() {
                    tracing::info!("tor: stopping the onion service");
                }
                self.stop();
            }
            TorChange::Start(wanted) => {
                self.stop();
                self.start(wanted);
            }
        }
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

/// Replaces `dir`'s [`STATUS_FILE`] with `report` in one step, so a reader
/// never sees half of it. A folder that doesn't exist is left alone: Tor
/// never ran there, so there is nothing stale to correct, and only
/// [`prepare_state_dir`] makes it.
fn write_status_file(dir: &Path, report: &str) {
    if !dir.is_dir() {
        return;
    }
    let file = dir.join(STATUS_FILE);
    let partial = dir.join(format!("{STATUS_FILE}.partial"));
    let written = std::fs::write(&partial, report).and_then(|()| std::fs::rename(&partial, &file));
    if let Err(e) = written {
        tracing::warn!(path = %file.display(), error = %e, "tor: couldn't write Tor's status to a file");
    }
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
