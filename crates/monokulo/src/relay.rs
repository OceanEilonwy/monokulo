//! The push relay inside monokulo (`relay.enabled`): the Web Push service
//! that hands pushes on to APNs and FCM (crate `push-relay`), for an
//! operator shipping their own app with their own Apple and Google keys.
//!
//! It runs as `monokulo-push-relay` does, from the same settings, which
//! here are the `[relay.*]` tables of monokulo's options file and
//! `--relay-…` options (the embedded engine's pattern), on its own
//! listener (`server.listen`, 8444 by default), with its handles in its
//! own database file, beside monokulo's unless `store.path` says
//! otherwise. Monokulo itself never talks to it: its pushes reach a relay,
//! this one or another, over Web Push like any sender's.
//!
//! The tables are read and checked at every start, the relay on or off,
//! so a mistake in them stops monokulo then rather than the day the relay
//! is turned on. Without the `relay` feature, `relay.enabled` and the
//! tables stop monokulo, saying to run `monokulo-push-relay` instead.

use std::collections::HashMap;
use std::path::Path;

use live_settings::OptionsFile;
use tokio_util::sync::CancellationToken;

use crate::settings::RELAY_TABLE;

/// The relay's state and settings as the admin page's card shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayView {
    /// `relay.enabled`.
    pub enabled: bool,
    pub state: RelayState,
    /// `server.listen`, as set.
    pub listen: String,
    /// `server.public_uri`, when set.
    pub public_uri: Option<String>,
    /// The apps devices may register for (`handles.apps`), by name.
    pub apps: Vec<String>,
    /// Handles in the relay's database that haven't expired; `None` while
    /// the relay isn't running (its database is open only then), or when
    /// it couldn't be read.
    pub handles_stored: Option<u64>,
    /// Whether the APNs key, and its ids, are set.
    pub apns_ready: bool,
    /// Whether the FCM service account is set.
    pub fcm_ready: bool,
    /// What the relay can't start without, in its settings as saved.
    pub missing: Vec<Missing>,
}

/// Whether the embedded relay is serving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayState {
    /// `relay.enabled` is off: nothing listens.
    Off,
    /// Listening at `address` since `since` (Unix seconds).
    Running {
        address: std::net::SocketAddr,
        since: i64,
    },
    /// Enabled, but its last start (at `since`) failed: nothing listens
    /// until the next save or start fixes `error`, said with the relay as
    /// its subject and without it ("failed to listen on …: address
    /// already in use").
    Failed { error: String, since: i64 },
}

/// What the card's panel and the toast say when the relay is on but isn't
/// running, in bold.
pub const NOT_RUNNING: &str = "The push relay isn't running.";

/// What follows [`NOT_RUNNING`]: why ([`RelayState::Failed`]'s `error`),
/// and what to do.
pub fn failure_fix(error: &str) -> String {
    format!(
        "It {}. Fix it and save again, or restart monokulo.",
        error.trim_end_matches('.')
    )
}

/// Something the relay can't start without.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// `server.public_uri`.
    PublicUri,
    /// `MONOKULO_RELAY_HANDLE_KEY` (`handles.id_key`).
    HandleKey,
}

impl Missing {
    /// The relay's setting it is (its own key).
    pub fn key(self) -> &'static str {
        match self {
            Missing::PublicUri => "server.public_uri",
            Missing::HandleKey => "handles.id_key",
        }
    }

    /// What is missing and what to do, in a sentence.
    pub fn sentence(self) -> &'static str {
        match self {
            Missing::PublicUri => "relay.server.public_uri is not set: the relay can't start until it is. Set it under Edit settings.",
            Missing::HandleKey => "MONOKULO_RELAY_HANDLE_KEY is not set: the relay can't start until it is set in the environment and monokulo restarts. Generate one with openssl rand -hex 32.",
        }
    }
}

/// What a restart of the relay did ([`Handle::restart`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restarted {
    /// It was off, and now listens at the address.
    Started(std::net::SocketAddr),
    /// It was running, and listens again, at the address.
    Restarted(std::net::SocketAddr),
    /// It was running, and is off now.
    Stopped,
    /// It was off, and stays off.
    StillOff,
    /// It is on, but didn't start, for this reason.
    Failed(String),
}

impl Restarted {
    /// What the toast says about it, if anything.
    pub fn sentence(&self) -> Option<String> {
        match self {
            Restarted::Started(_) => Some("The push relay started its listener.".to_owned()),
            Restarted::Restarted(_) => Some("The push relay restarted its listener.".to_owned()),
            Restarted::Stopped => Some("The push relay stopped its listener.".to_owned()),
            Restarted::StillOff => None,
            Restarted::Failed(error) => Some(format!("{NOT_RUNNING} {}", failure_fix(error))),
        }
    }
}

#[cfg(feature = "relay")]
pub use embedded::*;

#[cfg(not(feature = "relay"))]
pub use without::*;

/// A port nothing listens on, for a relay a test is to start there.
///
/// Picked below every system's ephemeral range (Linux hands a `:0` bind
/// 32768 and up, macOS and Windows 49152 and up), so a `:0` bind in another
/// test's process never gets it between here and the relay's own bind, as
/// one did; from the test's name, so two tests running side by side don't
/// share one; and checked free, so nothing else on the machine is fought
/// over.
#[cfg(all(test, feature = "relay"))]
pub(crate) fn free_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static HANDED_OUT: AtomicU16 = AtomicU16::new(0);
    let thread = std::thread::current();
    let hash = thread
        .name()
        .unwrap_or("")
        .bytes()
        .fold(0u32, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(u32::from(byte))
        });
    // 10000 to 29999 in slots of 8 ports, one slot a test.
    let slot = 10_000 + (hash % 2_500) * 8;
    loop {
        let port = slot + u32::from(HANDED_OUT.fetch_add(1, Ordering::Relaxed));
        let port = u16::try_from(port).expect("a port below 30000, and a few more");
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

#[cfg(feature = "relay")]
mod embedded {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use live_settings::{Env, FileStore, Live, Registry};
    use push_relay::settings::{self, AppField, AppRow, Apps, Host, RelayConfig, STORE_PATH};
    use push_relay::Relay;

    use super::*;
    use crate::settings::RelaySwitch;
    use crate::views::admin::{RelayAppField, RelayAppRow};

    /// The relay's database file beside monokulo's, unless `store.path`
    /// is set.
    pub const STORE_FILE: &str = "push-relay.db";

    /// monokulo's own handle on its options file, leaving the `[relay.*]`
    /// tables to the relay.
    pub fn own_options(file: OptionsFile) -> OptionsFile {
        file.leaving(RELAY_TABLE)
    }

    /// The relay's settings as read before monokulo's runtime starts.
    pub struct Prepared {
        file: OptionsFile,
        env: Env,
    }

    /// Reads the relay's tables of `file` (a mistake in them stops
    /// monokulo, line by line) and its `--relay-…` options, `cli`.
    pub fn prepare(
        file: &OptionsFile,
        cli: HashMap<String, String>,
        enabled: bool,
    ) -> Result<Prepared, String> {
        prepare_in(
            file,
            Env::process().with_nested_cli(RELAY_TABLE, cli),
            enabled,
        )
    }

    /// [`prepare`] with the relay's secrets and `--relay-…` options read
    /// from `env`.
    pub(crate) fn prepare_in(
        file: &OptionsFile,
        env: Env,
        enabled: bool,
    ) -> Result<Prepared, String> {
        let file = file.scoped(RELAY_TABLE);
        file.read(&settings::embedded_settings())
            .map_err(|error| error.to_string())?;
        if enabled {
            if let Some(telemetry) = telemetry::global() {
                telemetry.host("push-relay", &["push_relay"], "push-relay-");
            }
        }
        Ok(Prepared { file, env })
    }

    /// Checks every relay setting, together and with its secrets, and, when
    /// the relay is enabled (`switch`, monokulo's own `relay.enabled`),
    /// starts it on its own listener until `stop`, the process's
    /// (`service_runtime::shutdown::on_signal`). Any problem is the error
    /// monokulo stops on. The handle it gives back is the admin page's:
    /// what its card shows, and what a save restarts.
    pub async fn start(
        prepared: Prepared,
        monokulo_db: &Path,
        stop: CancellationToken,
        switch: Live<RelaySwitch>,
    ) -> Result<Handle, String> {
        let Prepared { file, env } = prepared;
        // Beside monokulo's database unless `store.path` says otherwise:
        // the default the page shows, too.
        let store_default = monokulo_db.with_file_name(STORE_FILE);
        let env = env.with_defaults(&[(STORE_PATH.key, &store_default.to_string_lossy())]);
        let declared = settings::embedded_settings();
        let registry = settings::registry(
            Arc::new(FileStore::new(file, &declared)),
            env,
            Host::Embedded,
        )
        .await
        .map_err(|error| error.to_string())?;
        let handle = Handle(Some(Arc::new(Embedded {
            registry,
            switch,
            stop,
            run: tokio::sync::Mutex::new(Run {
                running: None,
                state: RelayState::Off,
            }),
        })));
        if let Restarted::Failed(error) = handle.restart().await {
            return Err(format!("the push relay {error}"));
        }
        Ok(handle)
    }

    /// The embedded relay as the dashboard reaches it
    /// (`http::AppState::relay`): what the admin page's Push relay card
    /// shows, and what a save of the relay's settings restarts.
    /// `Handle::default()` is a monokulo with no relay to show (a test's
    /// state).
    ///
    /// The card's state comes from here and nowhere else, so the page
    /// can't say the relay listens where it doesn't.
    #[derive(Clone, Default)]
    pub struct Handle(Option<Arc<Embedded>>);

    struct Embedded {
        /// The relay's settings, the `[relay.*]` tables of monokulo's
        /// options file: what the admin page describes, checks and saves.
        registry: Registry,
        /// monokulo's own `relay.enabled`.
        switch: Live<RelaySwitch>,
        /// The process's stop: each run's own stop is a child of it.
        stop: CancellationToken,
        run: tokio::sync::Mutex<Run>,
    }

    /// The relay running now, if it is, and how it stands.
    struct Run {
        running: Option<Running>,
        state: RelayState,
    }

    /// The relay, serving.
    struct Running {
        relay: Relay,
        /// This run's stop, a child of the process's.
        stop: CancellationToken,
        served: tokio::task::JoinHandle<std::io::Result<()>>,
    }

    /// `handles.apps` as the card's rows give it, or each row's problem,
    /// by its place.
    pub fn apps_setting(
        rows: &[RelayAppRow],
    ) -> Result<String, Vec<(usize, RelayAppField, String)>> {
        let rows: Vec<AppRow> = rows
            .iter()
            .map(|row| AppRow {
                name: row.name.clone(),
                apns_topic: row.apns_topic.clone(),
                fcm_project_id: row.fcm_project_id.clone(),
            })
            .collect();
        Apps::from_rows(&rows)
            .map(|apps| apps.to_setting())
            .map_err(|errors| {
                errors
                    .into_iter()
                    .map(|error| {
                        let field = match error.field {
                            AppField::Name => RelayAppField::Name,
                            AppField::ApnsTopic => RelayAppField::ApnsTopic,
                            AppField::FcmProjectId => RelayAppField::FcmProjectId,
                        };
                        (error.row, field, error.message)
                    })
                    .collect()
            })
    }

    /// An I/O error as words in a sentence: the system's message without
    /// its "(os error 98)" and capital ("address already in use").
    fn io_words(error: &std::io::Error) -> String {
        let text = error.to_string();
        let text = match (error.raw_os_error(), text.rfind(" (os error ")) {
            (Some(_), Some(at)) => &text[..at],
            _ => text.as_str(),
        };
        let mut chars = text.chars();
        match chars.next() {
            Some(first) => first.to_lowercase().chain(chars).collect(),
            None => String::new(),
        }
    }

    /// Starts a relay on `config` with its own stop under `parent`, and
    /// its listener: where it listens, or why it didn't start.
    async fn start_one(
        config: &RelayConfig,
        parent: &CancellationToken,
    ) -> Result<(Running, SocketAddr), String> {
        let stop = parent.child_token();
        let relay = Relay::start(config, stop.clone())
            .await
            .map_err(|error| format!("didn't start: {error}"))?;
        let listener = match tokio::net::TcpListener::bind(config.listen).await {
            Ok(listener) => listener,
            Err(error) => {
                stop.cancel();
                relay.stopped().await;
                return Err(format!(
                    "failed to listen on {}: {}",
                    config.listen,
                    io_words(&error)
                ));
            }
        };
        let address = listener.local_addr().unwrap_or(config.listen);
        tracing::info!(relay.address = %address, "push relay listening");
        let served = {
            let (relay, stop) = (relay.clone(), stop.clone());
            tokio::spawn(async move { relay.serve(listener, stop.cancelled_owned()).await })
        };
        Ok((
            Running {
                relay,
                stop,
                served,
            },
            address,
        ))
    }

    impl Running {
        /// Stops it, waiting up to the grace period for requests in flight
        /// to finish, and for its sweep to end.
        async fn stop(self) {
            self.stop.cancel();
            let grace = service_runtime::shutdown::GRACE;
            match tokio::time::timeout(grace, self.served).await {
                Ok(Ok(Ok(()))) => tracing::info!("the push relay stopped"),
                Ok(Ok(Err(error))) => {
                    tracing::error!(error = %error, "the push relay's server failed while stopping")
                }
                Ok(Err(error)) => {
                    tracing::error!(error = %error, "the push relay's server task failed")
                }
                Err(_) => {
                    tracing::warn!(grace = ?grace, "the push relay still had requests running after the grace period")
                }
            }
            self.relay.stopped().await;
        }
    }

    impl Handle {
        /// The relay's settings, for the admin page; `None` when this
        /// monokulo has no relay to show.
        pub fn registry(&self) -> Option<&Registry> {
            self.0.as_ref().map(|embedded| &embedded.registry)
        }

        /// The relay as it is now, or `None` when this monokulo has none.
        pub async fn view(&self) -> Option<RelayView> {
            let embedded = self.0.as_ref()?;
            let run = embedded.run.lock().await;
            let config = settings::config(&embedded.registry).ok();
            let handles_stored = match &run.running {
                Some(running) => running
                    .relay
                    .handles_stored(crate::now_unix())
                    .await
                    .map_err(|error| {
                        tracing::warn!(error = %error, "the push relay's handles couldn't be counted")
                    })
                    .ok(),
                None => None,
            };
            Some(RelayView {
                enabled: embedded.switch.load().enabled,
                state: run.state.clone(),
                listen: config
                    .as_ref()
                    .map(|config| config.listen.to_string())
                    .unwrap_or_default(),
                public_uri: config
                    .as_ref()
                    .and_then(|config| config.public_uri.as_ref())
                    .map(|uri| uri.origin().as_str().to_owned()),
                apps: config
                    .as_ref()
                    .map(|config| {
                        config
                            .apps
                            .iter()
                            .map(|(name, _)| name.as_str().to_owned())
                            .collect()
                    })
                    .unwrap_or_default(),
                handles_stored,
                apns_ready: config.as_ref().is_some_and(|config| config.apns.is_some()),
                fcm_ready: config.as_ref().is_some_and(|config| config.fcm.is_some()),
                missing: self.missing(None),
            })
        }

        /// What the relay can't start without, with its saved settings, or
        /// with `public_uri` in place of the saved one when a save would
        /// set it (`Some("")`: take it away).
        pub fn missing(&self, public_uri: Option<&str>) -> Vec<Missing> {
            let Some(embedded) = self.0.as_ref() else {
                return Vec::new();
            };
            let Ok(config) = settings::config(&embedded.registry) else {
                return Vec::new();
            };
            let mut missing = Vec::new();
            let no_uri = match public_uri {
                Some(uri) => uri.trim().is_empty(),
                None => config.public_uri.is_none(),
            };
            if no_uri {
                missing.push(Missing::PublicUri);
            }
            if config.handle_key.is_none() {
                missing.push(Missing::HandleKey);
            }
            missing
        }

        /// Starts the relay again from its settings as saved now
        /// (`relay.enabled` and `[relay.*]`): the running one, if any, is
        /// stopped first (requests in flight finish, as at shutdown), and a
        /// new one started when it is on. A start that fails leaves it
        /// stopped and says why, in the card too, until the next restart;
        /// monokulo carries on.
        pub async fn restart(&self) -> Restarted {
            let Some(embedded) = self.0.as_ref() else {
                return Restarted::StillOff;
            };
            let mut run = embedded.run.lock().await;
            let was_running = match run.running.take() {
                Some(running) => {
                    running.stop().await;
                    true
                }
                None => false,
            };
            let now = crate::now_unix();
            let outcome = if !embedded.switch.load().enabled {
                run.state = RelayState::Off;
                if was_running {
                    Restarted::Stopped
                } else {
                    Restarted::StillOff
                }
            } else {
                let started = match settings::config(&embedded.registry) {
                    Ok(config) => start_one(&config, &embedded.stop).await,
                    Err(error) => Err(format!("can't read its settings: {error}")),
                };
                match started {
                    Ok((running, address)) => {
                        run.running = Some(running);
                        run.state = RelayState::Running {
                            address,
                            since: now,
                        };
                        if was_running {
                            Restarted::Restarted(address)
                        } else {
                            Restarted::Started(address)
                        }
                    }
                    Err(error) => {
                        tracing::error!(error = %error, "the push relay didn't start");
                        run.state = RelayState::Failed {
                            error: error.clone(),
                            since: now,
                        };
                        Restarted::Failed(error)
                    }
                }
            };
            outcome
        }

        /// The apps devices may register for, as saved, as the card's rows.
        pub fn saved_apps(&self) -> Vec<RelayAppRow> {
            let Some(embedded) = self.0.as_ref() else {
                return Vec::new();
            };
            settings::config(&embedded.registry)
                .map(|config| config.apps.rows())
                .unwrap_or_default()
                .into_iter()
                .map(|row| RelayAppRow {
                    name: row.name,
                    apns_topic: row.apns_topic,
                    fcm_project_id: row.fcm_project_id,
                    is_saved: true,
                    ..RelayAppRow::default()
                })
                .collect()
        }

        /// Stops the relay for good, as monokulo shuts down.
        pub async fn stop(&self) {
            if let Some(embedded) = self.0.as_ref() {
                let running = embedded.run.lock().await.running.take();
                if let Some(running) = running {
                    running.stop().await;
                }
            }
        }
    }
}

#[cfg(not(feature = "relay"))]
mod without {
    use super::*;

    const NO_RELAY: &str = "this monokulo was built without the push relay (its relay feature): set relay.enabled = false, or run monokulo-push-relay";

    /// Why `[relay.*]` tables are refused.
    const NO_RELAY_HINT: &str = "this monokulo was built without the push relay (its relay feature); a standalone monokulo-push-relay keeps these in its own relay.toml";

    /// monokulo's own handle on its options file, refusing `[relay.*]`
    /// tables with why: nothing here would read them.
    pub fn own_options(file: OptionsFile) -> OptionsFile {
        file.with_hint(RELAY_TABLE, NO_RELAY_HINT)
    }

    /// Nothing to prepare.
    pub struct Prepared(());

    /// Stops monokulo when the relay is asked for.
    pub fn prepare(
        _file: &OptionsFile,
        _cli: HashMap<String, String>,
        enabled: bool,
    ) -> Result<Prepared, String> {
        if enabled {
            Err(NO_RELAY.to_owned())
        } else {
            Ok(Prepared(()))
        }
    }

    /// A monokulo without a relay has nothing to show of one.
    pub async fn start(
        _prepared: Prepared,
        _monokulo_db: &Path,
        _stop: CancellationToken,
        _switch: live_settings::Live<crate::settings::RelaySwitch>,
    ) -> Result<Handle, String> {
        Ok(Handle(()))
    }

    /// No relay: no card on the admin page, nothing to restart.
    #[derive(Clone, Default)]
    pub struct Handle(());

    impl Handle {
        pub fn registry(&self) -> Option<&live_settings::Registry> {
            None
        }

        pub async fn view(&self) -> Option<RelayView> {
            None
        }

        pub fn missing(&self, _public_uri: Option<&str>) -> Vec<Missing> {
            Vec::new()
        }

        pub async fn restart(&self) -> Restarted {
            Restarted::StillOff
        }

        pub fn saved_apps(&self) -> Vec<crate::views::admin::RelayAppRow> {
            Vec::new()
        }

        pub async fn stop(&self) {}
    }

    /// Never asked: without a relay there are no app rows.
    pub fn apps_setting(
        _rows: &[crate::views::admin::RelayAppRow],
    ) -> Result<String, Vec<(usize, crate::views::admin::RelayAppField, String)>> {
        Ok(String::new())
    }
}

#[cfg(all(test, feature = "relay"))]
mod tests {
    use std::sync::Arc;

    use live_settings::{Env, Live, MemoryStore, Registry};

    use super::*;
    use crate::settings::{RelaySwitch, RELAY_ENABLED};

    /// A directory of the test's own, with monokulo's database path in it.
    fn monokulo_db(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!(
                "monokulo-relay-{tag}-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ))
            .join("monokulo.db")
    }

    fn file(text: &str) -> OptionsFile {
        OptionsFile::in_memory(text)
    }

    /// An environment holding only the relay's handle key, with `cli` as
    /// its `--relay-…` options.
    fn with_key(cli: HashMap<String, String>) -> Env {
        Env::fixed([("MONOKULO_RELAY_HANDLE_KEY", "0123456789abcdef".repeat(4))])
            .with_nested_cli(RELAY_TABLE, cli)
    }

    /// monokulo's `relay.enabled` as its registry keeps it, `enabled` to
    /// start with: saving through the registry turns it on or off.
    async fn switch(enabled: bool) -> (Registry, Live<RelaySwitch>) {
        let store = Arc::new(MemoryStore::with([(
            RELAY_ENABLED.key,
            enabled.to_string(),
        )]));
        let mut builder =
            Registry::builder_with_env(store, &[&RELAY_ENABLED], Env::fixed::<&str, &str>([]))
                .await;
        let live = builder.section::<RelaySwitch>();
        let registry = builder.build().unwrap();
        registry.boot().await.unwrap();
        (registry, live)
    }

    /// The relay on or off, started over `text` with `env`.
    async fn started(text: &str, env: Env, on: bool, tag: &str) -> Result<Handle, String> {
        let (_, live) = switch(on).await;
        let prepared = prepare_in(&file(text), env, on).unwrap();
        start(prepared, &monokulo_db(tag), CancellationToken::new(), live).await
    }

    fn listening(text: u16) -> String {
        format!(
            "[relay.server]\nlisten = \"127.0.0.1:{text}\"\npublic_uri = \"https://relay.example.org\"\n"
        )
    }

    /// A registration of an app the relay doesn't know, at `port`: a
    /// relay answers 400 with its problem.
    async fn register_at(port: u16) -> Result<serde_json::Value, reqwest::Error> {
        let answer = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/register"))
            .body(
                serde_json::json!({
                    "platform": "apns", "token": "00", "app": "monokular",
                    "vapid_public_key": ::push::VapidKey::generate().public().to_base64url(),
                })
                .to_string(),
            )
            .send()
            .await?;
        assert_eq!(answer.status(), reqwest::StatusCode::BAD_REQUEST);
        answer.json().await
    }

    fn nothing_listens(port: u16) {
        std::net::TcpListener::bind(("127.0.0.1", port)).expect("nothing listens there");
    }

    #[test]
    fn a_mistake_in_the_relays_tables_stops_monokulo_with_the_relay_off() {
        let error = prepare(
            &file("[relay.server]\npublic_uri = \"http://relay.example.org\"\n"),
            HashMap::new(),
            false,
        )
        .err()
        .unwrap();
        assert!(error.contains("relay.server.public_uri"), "{error}");
        let error = prepare(
            &file("[relay.logging]\nlevel = \"debug\"\n"),
            HashMap::new(),
            false,
        )
        .err()
        .unwrap();
        assert!(
            error.contains("there is no setting called relay.logging.level"),
            "{error}"
        );
    }

    /// Off, the settings are still checked together, and nothing listens.
    #[tokio::test]
    async fn a_relay_that_is_off_is_checked_and_starts_nothing() {
        let port = free_port();
        let text = format!("[relay.server]\nlisten = \"127.0.0.1:{port}\"\n");
        let (_, live) = switch(false).await;
        let prepared = prepare(&file(&text), HashMap::new(), false).unwrap();
        let db = monokulo_db("off");
        let handle = start(prepared, &db, CancellationToken::new(), live)
            .await
            .unwrap();
        let view = handle.view().await.unwrap();
        assert_eq!(view.state, RelayState::Off);
        assert!(!view.enabled);
        assert_eq!(view.handles_stored, None);
        assert_eq!(view.missing, [Missing::PublicUri, Missing::HandleKey]);
        assert!(!db.with_file_name(STORE_FILE).exists());
        nothing_listens(port);

        let ids_alone = "[relay.apns]\nteam_id = \"TEAM123456\"\n";
        let error = started(ids_alone, Env::fixed::<&str, &str>([]), false, "ids")
            .await
            .err()
            .unwrap();
        assert!(error.contains("apns.team_id"), "{error}");
    }

    #[tokio::test]
    async fn an_enabled_relay_without_its_public_address_stops_monokulo() {
        let error = started("", with_key(HashMap::new()), true, "no-uri")
            .await
            .err()
            .unwrap();
        assert!(error.contains("server.public_uri must be set"), "{error}");
    }

    #[tokio::test]
    async fn an_enabled_relay_without_its_handle_key_stops_monokulo() {
        let text = "[relay.server]\npublic_uri = \"https://relay.example.org\"\n";
        let (_, live) = switch(true).await;
        let prepared = prepare_in(&file(text), Env::fixed::<&str, &str>([]), true).unwrap();
        let db = monokulo_db("no-key");
        let error = start(prepared, &db, CancellationToken::new(), live)
            .await
            .err()
            .unwrap();
        assert!(
            error.contains("MONOKULO_RELAY_HANDLE_KEY must be set"),
            "{error}"
        );
        assert!(!db.with_file_name(STORE_FILE).exists());
    }

    /// On, it listens where its settings say, its database beside
    /// monokulo's, and answers as a relay; the card's view says so.
    #[tokio::test]
    async fn an_enabled_relay_serves_on_its_own_listener_over_http() {
        let port = free_port();
        let cli = HashMap::from([("server.listen".to_owned(), format!("127.0.0.1:{port}"))]);
        let text = "[relay.server]\npublic_uri = \"https://relay.example.org\"\n\n[relay.handles.apps.monokular]\nfcm_project_id = \"monokular-test\"\n";
        let env =
            with_key(cli).with_var("MONOKULO_RELAY_FCM_CREDENTIALS", &push_relay_fcm_account());
        let (_, live) = switch(true).await;
        let prepared = prepare_in(&file(text), env, true).unwrap();
        let db = monokulo_db("on");
        let handle = start(prepared, &db, CancellationToken::new(), live)
            .await
            .unwrap();
        assert!(db.with_file_name(STORE_FILE).exists());
        // Its app is known, for Android alone.
        let problem = register_at(port).await.unwrap();
        assert_eq!(
            problem["type"],
            "https://docs.monokulo.com/push-relay/#unsupported-platform"
        );
        let view = handle.view().await.unwrap();
        assert!(view.enabled);
        assert!(
            matches!(view.state, RelayState::Running { address, .. } if address.port() == port),
            "{view:?}"
        );
        assert_eq!(view.listen, format!("127.0.0.1:{port}"));
        assert_eq!(
            view.public_uri.as_deref(),
            Some("https://relay.example.org")
        );
        assert_eq!(view.apps, ["monokular"]);
        assert_eq!(view.handles_stored, Some(0));
        assert!(view.fcm_ready && !view.apns_ready);
        assert!(view.missing.is_empty());
        // The store's default, beside monokulo's database, is what the
        // page shows.
        let store = handle
            .registry()
            .unwrap()
            .describe()
            .into_iter()
            .find(|view| view.key == "store.path")
            .unwrap();
        assert_eq!(store.value, db.with_file_name(STORE_FILE).to_string_lossy());
        // `--relay-server-listen` locks it, named as it was typed.
        let listen = handle
            .registry()
            .unwrap()
            .describe()
            .into_iter()
            .find(|view| view.key == "server.listen")
            .unwrap();
        assert!(listen.locked.unwrap().contains("--relay-server-listen"),);
        handle.stop().await;
        nothing_listens(port);
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// The FCM account the relay's own tests use, as text.
    fn push_relay_fcm_account() -> String {
        serde_json::json!({
            "type": "service_account",
            "project_id": "monokular-test",
            "private_key_id": "0123456789abcdef0123456789abcdef01234567",
            "private_key": include_str!("../../push-relay/tests/fixtures/fcm-test-key.pem"),
            "client_email": "relay@monokular-test.iam.gserviceaccount.com",
            "client_id": "100000000000000000000",
            "token_uri": "https://oauth2.googleapis.com/token",
        })
        .to_string()
    }

    /// A stop that came while monokulo was still starting stops the relay
    /// too, with nothing left listening.
    #[tokio::test]
    async fn an_enabled_relay_given_a_stop_that_already_came_stops() {
        let port = free_port();
        let (_, live) = switch(true).await;
        let prepared = prepare_in(&file(&listening(port)), with_key(HashMap::new()), true).unwrap();
        let db = monokulo_db("stopped");
        let stop = CancellationToken::new();
        stop.cancel();
        let handle = start(prepared, &db, stop, live).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), handle.stop())
            .await
            .expect("the relay stopped without waiting out its grace period");
        nothing_listens(port);
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// A save of `server.listen` and a restart move the listener: the old
    /// address no longer answers, the new one does, and the card's state
    /// says where.
    #[tokio::test]
    async fn a_restart_moves_the_listener_to_its_new_address_over_http() {
        let (old, new) = (free_port(), free_port());
        let (_, live) = switch(true).await;
        let prepared = prepare_in(&file(&listening(old)), with_key(HashMap::new()), true).unwrap();
        let db = monokulo_db("moved");
        let handle = start(prepared, &db, CancellationToken::new(), live)
            .await
            .unwrap();
        register_at(old).await.unwrap();
        handle
            .registry()
            .unwrap()
            .save(vec![(
                "server.listen".into(),
                Some(format!("127.0.0.1:{new}")),
            )])
            .await
            .unwrap();
        // Saved, not applied yet: it still listens where it did.
        register_at(old).await.unwrap();
        let restarted = handle.restart().await;
        assert!(
            matches!(restarted, Restarted::Restarted(address) if address.port() == new),
            "{restarted:?}"
        );
        nothing_listens(old);
        register_at(new).await.unwrap();
        assert!(matches!(
            handle.view().await.unwrap().state,
            RelayState::Running { address, .. } if address.port() == new
        ));
        handle.stop().await;
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// Turned off and restarted, nothing listens; on again, it starts.
    #[tokio::test]
    async fn a_restart_with_the_relay_turned_off_stops_it() {
        let port = free_port();
        let (switches, live) = switch(true).await;
        let prepared = prepare_in(&file(&listening(port)), with_key(HashMap::new()), true).unwrap();
        let db = monokulo_db("turned-off");
        let handle = start(prepared, &db, CancellationToken::new(), live)
            .await
            .unwrap();
        switches
            .save(vec![(RELAY_ENABLED.key.into(), Some("false".into()))])
            .await
            .unwrap();
        assert_eq!(handle.restart().await, Restarted::Stopped);
        nothing_listens(port);
        assert_eq!(handle.view().await.unwrap().state, RelayState::Off);
        assert_eq!(handle.restart().await, Restarted::StillOff);
        switches
            .save(vec![(RELAY_ENABLED.key.into(), Some("true".into()))])
            .await
            .unwrap();
        assert!(matches!(handle.restart().await, Restarted::Started(_)));
        register_at(port).await.unwrap();
        handle.stop().await;
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// A restart that can't listen where it is told leaves the relay
    /// stopped, says why, and the card shows it failed; the next restart
    /// that can, starts it.
    #[tokio::test]
    async fn a_restart_that_cannot_listen_fails_and_the_next_one_starts_it() {
        let (first, taken) = (free_port(), free_port());
        let (_, live) = switch(true).await;
        let prepared =
            prepare_in(&file(&listening(first)), with_key(HashMap::new()), true).unwrap();
        let db = monokulo_db("taken");
        let handle = start(prepared, &db, CancellationToken::new(), live)
            .await
            .unwrap();
        let occupied = std::net::TcpListener::bind(("127.0.0.1", taken)).unwrap();
        let registry = handle.registry().unwrap();
        registry
            .save(vec![(
                "server.listen".into(),
                Some(format!("127.0.0.1:{taken}")),
            )])
            .await
            .unwrap();
        let Restarted::Failed(error) = handle.restart().await else {
            panic!("the port is taken");
        };
        // Plain words: no "(os error 98)", no capital mid-sentence. The
        // words are the system's ("address already in use" here, a sentence
        // on Windows), so not spelt out.
        let prefix = format!("failed to listen on 127.0.0.1:{taken}: ");
        let words = error
            .strip_prefix(&prefix)
            .unwrap_or_else(|| panic!("{error}"));
        assert!(!words.contains("(os error"), "{error}");
        assert!(words.starts_with(|c: char| c.is_lowercase()), "{error}");
        nothing_listens(first);
        let view = handle.view().await.unwrap();
        assert!(matches!(&view.state, RelayState::Failed { error: shown, .. } if *shown == error));
        assert_eq!(view.handles_stored, None);
        drop(occupied);
        assert!(matches!(handle.restart().await, Restarted::Started(_)));
        register_at(taken).await.unwrap();
        handle.stop().await;
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// monokulo's own handle reads `relay.enabled` and leaves the tables
    /// under `[relay]` to the relay.
    #[test]
    fn monokulos_own_handle_leaves_the_relays_tables() {
        let text = "[relay]\nenabled = true\n\n[relay.server]\npublic_uri = \"https://relay.example.org\"\n";
        let values = own_options(file(text)).read(crate::settings::ALL).unwrap();
        assert_eq!(
            values.get("relay.enabled").map(String::as_str),
            Some("true")
        );
        assert_eq!(values.len(), 1);
    }
}
