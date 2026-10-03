//! The engine as a library (`docs/engine_as_library.md`, phase 1): everything
//! a running engine is, started by [`Engine::start`] and stopped by
//! [`Engine::shutdown`], without owning the process.
//!
//! [`Engine::start`] opens storage, loads every setting, registers every
//! store's wallet with key custody, and starts the webhook delivery loop and
//! the network loop manager (which starts a scanner, proof and node event
//! loop per configured network). It returns the admin API as a router.
//! Serving that router, handling signals, logging and sampling the process's
//! resources belong to whoever hosts the engine: today the standalone
//! `monokulo-engine` binary (`main.rs`), which serves it on `server.bind`.
//!
//! Nothing here ends the process: what would stop the engine at start is
//! returned as a [`StartError`] for the host to report.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;

use crate::engine_settings::{
    Daemons, EngineSettings, SnpBootConfig, ALL, SERVER_TOKEN, STANDALONE_ONLY,
};
use crate::http::rate_limit::RateLimiter;
use crate::http::{build_router, AppState};
use crate::key_custody::{CustodyRouter, KeyCustody, SnpSlot, StoreWraps, WalletHandle};
use crate::store::{SharedStore, Store, StoreError, TenantId};

/// A fixed outer ceiling on request bodies; `server.max_body_bytes` (the
/// live limit, task 2.6) is what normally applies, and its own range tops
/// out here.
const MAX_BODY_CEILING: usize = 16 * 1024 * 1024;

/// How long start-up spends registering stores' wallets with key custody
/// before leaving the rest to the scanner, which registers them as it goes.
const REGISTRATION_ALLOWANCE: Duration = Duration::from_secs(20);

/// What an engine starts from, decided by whoever hosts it.
pub struct EngineConfig {
    /// The options file the engine's settings are read from, and the admin
    /// page saves them to: `engine.toml` for the standalone engine, the
    /// `[engine.*]` tables of monokulo's own file when embedded
    /// (`OptionsFile::scoped`).
    pub options: live_settings::OptionsFile,
    /// Command-line options and environment variables, which win over the
    /// file.
    pub env: live_settings::Env,
    /// The engine's database file.
    pub database_path: PathBuf,
    pub host: Host,
}

/// Who runs the engine.
pub enum Host {
    /// Its own process (`monokulo-engine`), serving its API over HTTP: the
    /// engine token (`ENGINE_TOKEN`) is required in the environment.
    Standalone,
    /// Inside monokulo, which calls its router in-process with `token`, one
    /// it made for this run. Settings in
    /// [`STANDALONE_ONLY`](crate::engine_settings::STANDALONE_ONLY) do
    /// nothing here, so giving one stops the engine.
    Embedded { token: shared::auth::RawToken },
}

/// Why an engine didn't start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// No engine token, or one too short: nothing could talk to the engine.
    #[error("{0}")]
    Token(String),
    #[error("failed to open the database at {path}: {source}")]
    Database {
        path: PathBuf,
        #[source]
        source: StoreError,
    },
    #[error("failed to load settings: {0}")]
    Settings(String),
    #[error("failed to start the database worker: {0}")]
    DatabaseWorker(StoreError),
    #[error("failed to open the database read pool: {0}")]
    ReadPool(StoreError),
    /// Settings that only mean something to the standalone engine, given
    /// to an embedded one: each named with where it was given.
    #[error(
        "{0}: these only apply to the engine running on its own (monokulo-engine), not inside monokulo. Remove them, or set engine.mode = \"remote\" and run monokulo-engine."
    )]
    NotWhenEmbedded(String),
}

/// How [`Engine::shutdown`] went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// Every loop the engine started has ended.
    Cleanly,
    /// The grace period ran out first; the loops still running were left to
    /// end with the runtime.
    TimedOut,
}

/// A running engine. Dropping it without [`Engine::shutdown`] leaves its
/// loops running until the runtime ends, as the standalone binary's did.
pub struct Engine {
    router: axum::Router,
    settings: Arc<EngineSettings>,
    stop: tokio::sync::watch::Sender<bool>,
    loops: Vec<tokio::task::JoinHandle<()>>,
}

impl Engine {
    /// Opens the database, loads the settings, registers every active
    /// store's wallet, and starts the engine's loops.
    pub async fn start(config: EngineConfig) -> Result<Self, StartError> {
        // Every request must carry it (`http::engine_token_middleware`):
        // without one, nothing could talk to this engine, so it doesn't start.
        let embedded = matches!(config.host, Host::Embedded { token: _ });
        let (engine_token, raw_token, env) = match &config.host {
            Host::Standalone => {
                let token = SERVER_TOKEN
                    .require(&config.env)
                    .map_err(StartError::Token)?;
                (
                    Arc::new(shared::auth::engine_token(token.expose()).hash()),
                    token.expose().to_owned(),
                    config.env.clone(),
                )
            }
            Host::Embedded { token } => {
                refuse_standalone_settings(&config.options, &config.env)?;
                // The token monokulo made is this engine's `server.token`.
                (
                    Arc::new(token.hash()),
                    token.expose().to_owned(),
                    config
                        .env
                        .clone()
                        .with_var(SERVER_TOKEN.env_var, token.expose()),
                )
            }
        };
        let env = &env;

        let db_path = config.database_path;
        let db_file = db_path.to_string_lossy().into_owned();
        let store = Store::open_file(&db_file)
            .map_err(|source| StartError::Database {
                path: db_path.clone(),
                source,
            })?
            .into_shared();
        // Beside the main database; lines logged since start-up go in too.
        let log_store = telemetry::global().and_then(|t| t.open_store_beside(&db_path));

        // Every setting, live (admin_settings_v2.md part 1). Node clients
        // are built into `daemons` from the saved node settings, and rebuilt
        // whenever they are saved; the rate limiter and the key custody
        // backends (part 5) follow their settings the same way.
        let daemons = Daemons::default();
        let admin_rate_limiter = Arc::new(RateLimiter::new(1));
        let custody_router = Arc::new(CustodyRouter::default());
        // The snp backend's settings apply at a restart: read once, here. It
        // starts when it is first enabled (`SnpSlot`).
        let file = config
            .options
            .read(ALL)
            .map_err(|e| StartError::Settings(e.to_string()))?;
        let snp_boot = live_settings::read_sync_with_env::<SnpBootConfig>(Ok(file), env);
        let snp = Arc::new(SnpSlot::new(
            snp_boot.snp_config(),
            // Fixed, not a setting: the host writes the settings.
            Arc::new(snp_attest::guest::SevGuest::new("/dev/sev-guest")),
            Arc::new(StoreWraps(Arc::clone(&store))),
        ));
        let settings = EngineSettings::load(
            Arc::clone(&store),
            daemons.clone(),
            crate::engine_settings::CustodyReloadable::new(
                Arc::clone(&custody_router),
                Arc::clone(&snp),
            ),
            Arc::clone(&admin_rate_limiter),
            env.clone(),
            config.options,
            embedded,
        )
        .await
        .map_err(StartError::Settings)?;
        if daemons.networks().is_empty() {
            // A warning, not an error: the settings API has to be reachable
            // to configure a node at all, and one saved there applies
            // straight away.
            tracing::warn!(
                "no Monero node is configured for any network (mainnet/stagenet/testnet) yet - nothing is \
                 scanned until one is saved on the admin settings page (or POST /api/v1/admin/settings, \
                 monero_node.<network>); it applies without a restart"
            );
        }
        warn_about_stranded_stores(&store, &custody_router);
        // One scan at a time per CPU the engine may use (`server.cpus`),
        // set before any scan runs.
        let scan_slots =
            crate::key_custody::size_scan_slots(settings.runtime.load().threads.scan_slots());
        tracing::info!(scan_slots, "scans run at most this many at a time");

        let key_custody: Arc<dyn KeyCustody> = Arc::<CustodyRouter>::clone(&custody_router);
        let default_backend = custody_router.default_backend();
        let scanner_status = crate::scanner_status::new_scanner_status_map();
        let wallet_handles = Arc::new(RwLock::new(
            register_all_tenants(&store, &key_custody).await,
        ));

        // The database worker: its own connection, on its own thread, for
        // the scanner, webhook delivery and API writes
        // (docs/scanner_microtasks.md).
        let db =
            crate::store::Db::open(&db_file, &store.lock()).map_err(StartError::DatabaseWorker)?;
        let read_pool =
            crate::store::ReadStorePool::open(&db_file, settings.runtime.load().read_connections)
                .map_err(StartError::ReadPool)?;
        let app_state = AppState {
            db: crate::store::Database::from_parts(db.clone(), read_pool, &store.lock()),
            admin_rate_limiter,
            log_store,
            engine_token,
            settings: Arc::clone(&settings),
            custody: crate::http::Custody {
                backends: Arc::clone(&key_custody),
                default_backend,
                wallet_handles: Arc::clone(&wallet_handles),
                snp: Some(Arc::clone(&snp)),
            },
            networks: crate::http::Networks {
                daemons: daemons.clone(),
                scanner_status: Arc::clone(&scanner_status),
            },
        };

        let (stop, stopped) = tokio::sync::watch::channel(false);
        let mut loops = Vec::new();

        let delivery_db = db.clone();
        let delivery_settings = Arc::clone(&settings);
        // Woken by the scanner as soon as it enqueues a webhook.
        let webhook_wake = Arc::new(tokio::sync::Notify::new());
        let delivery_wake = Arc::clone(&webhook_wake);
        loops.push(shared::supervise::supervise_until(
            "webhook delivery",
            stopped.clone(),
            move || {
                crate::loops::run_webhook_delivery_loop(
                    delivery_db.clone(),
                    Arc::clone(&delivery_settings),
                    Arc::clone(&delivery_wake),
                )
            },
        ));

        // AMD's certificates for the snp backend's report, and the handoff
        // of its master key when it runs a new image.
        let handoff_url = snp_boot.handoff_url.map(|url| url.url().to_string());
        loops.push(shared::supervise::supervise_until(
            "snp key custody upkeep",
            stopped.clone(),
            move || {
                crate::key_custody::run_snp_upkeep(
                    Arc::clone(&snp),
                    handoff_url.clone().map(|url| crate::key_custody::Handoff {
                        url,
                        token: raw_token.clone(),
                    }),
                )
            },
        ));

        // One scanner loop per configured network (task 7.4), started and
        // stopped as node settings are saved (task 2.1). Supervised like the
        // loops it starts: stopping or restarting it drops the network
        // loops' stop signals, which ends them, and a restart starts them
        // again.
        let loops_settings = Arc::clone(&settings);
        loops.push(shared::supervise::supervise_until(
            "network loop manager",
            stopped,
            move || {
                crate::loops::manage_network_loops(
                    db.clone(),
                    Arc::clone(&webhook_wake),
                    Arc::clone(&key_custody),
                    daemons.clone(),
                    Arc::clone(&wallet_handles),
                    Arc::clone(&scanner_status),
                    Arc::clone(&loops_settings),
                )
            },
        ));

        Ok(Self {
            router: build_router(app_state, MAX_BODY_CEILING),
            settings,
            stop,
            loops,
        })
    }

    /// The engine's admin API: what the standalone binary serves on
    /// `server.bind`, and what monokulo calls.
    pub fn router(&self) -> axum::Router {
        self.router.clone()
    }

    /// Where the standalone binary listens (`server.bind`). Read once: the
    /// listen address applies at restart (decision D8).
    pub fn bind_address(&self) -> std::net::SocketAddr {
        self.settings.runtime.load().bind
    }

    /// Stops the engine's loops and waits up to `grace` for them to end.
    /// Every step they take is safe to interrupt (payments are recorded
    /// idempotently, a block is only marked scanned after everything in it
    /// is recorded, webhooks are marked delivered only after they went
    /// out), so the next start carries on where they stopped.
    pub async fn shutdown(self, grace: Duration) -> Stopped {
        let _ = self.stop.send(true);
        let all = futures_util::future::join_all(self.loops);
        match tokio::time::timeout(grace, all).await {
            Ok(_) => Stopped::Cleanly,
            Err(_) => Stopped::TimedOut,
        }
    }
}

/// Refuses any [`STANDALONE_ONLY`] setting given to an embedded engine.
///
/// An embedded engine has no listener, is given its token, and logs through
/// its host's logger: such a setting (in the options file, on the command
/// line or in the environment) would do nothing, so it is refused rather
/// than ignored.
pub fn refuse_standalone_settings(
    options: &live_settings::OptionsFile,
    env: &live_settings::Env,
) -> Result<(), StartError> {
    let file = options
        .read(ALL)
        .map_err(|e| StartError::Settings(e.to_string()))?;
    let snapshot = live_settings::Snapshot::new(file, env.clone());
    let given: Vec<String> = STANDALONE_ONLY
        .iter()
        .filter_map(|setting| {
            let source = snapshot.source_of(*setting);
            let place = match source {
                live_settings::SettingSource::Default | live_settings::SettingSource::Database => {
                    return None
                }
                live_settings::SettingSource::Toml => {
                    format!("engine.{} in the options file", setting.key())
                }
                live_settings::SettingSource::Cli => {
                    format!("--engine-{}", live_settings::cli_flag(setting.key()))
                }
                live_settings::SettingSource::Env => setting.env_var().to_owned(),
            };
            Some(place)
        })
        .collect();
    if given.is_empty() {
        Ok(())
    } else {
        Err(StartError::NotWhenEmbedded(given.join(", ")))
    }
}

/// Warns about stores whose keys are in a key custody backend that isn't
/// enabled: their payments aren't detected until it is, or they move.
fn warn_about_stranded_stores(store: &SharedStore, custody_router: &CustodyRouter) {
    let enabled = custody_router.enabled_backends();
    let tenants = store.lock().tenant_custody_backends().unwrap_or_default();
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (_, _, backend) in tenants
        .into_iter()
        .filter(|(_, _, backend)| !enabled.contains(backend))
    {
        *counts.entry(backend).or_default() += 1;
    }
    for (backend, count) in counts {
        tracing::warn!(
            custody.backend = %backend,
            stores = count,
            enabled_backends = %enabled.join(","),
            "{count} store(s) keep their keys in the {backend:?} key custody backend, which is not enabled. Their \
             payments are NOT being detected until it is enabled again or they move their keys to an enabled backend."
        );
    }
}

/// Eagerly registers every non-disabled tenant's sealed key material with
/// `KeyCustody`, so `AppState::wallet_handles` starts populated rather than
/// relying solely on the lazy on-first-use path in
/// `http::resolve_wallet_handle`.
async fn register_all_tenants(
    store: &SharedStore,
    key_custody: &Arc<dyn KeyCustody>,
) -> HashMap<TenantId, WalletHandle> {
    let deadline = tokio::time::Instant::now() + REGISTRATION_ALLOWANCE;
    // A database error here must not stop the engine at start: retry with
    // backoff until the store answers, logging each failure.
    let mut delay = Duration::from_millis(500);
    let tenants = loop {
        // Bound first: the lock must not be held through the retry's sleep.
        let listed = {
            let store = Arc::clone(store);
            tokio::task::spawn_blocking(move || store.lock().list_active_tenants())
                .await
                .unwrap_or_else(|e| Err(StoreError::WorkerUnavailable(e.to_string())))
        };
        match listed {
            Ok(tenants) => break tenants,
            Err(e) => {
                tracing::warn!(error = %e, retry_in = ?delay, "failed to list tenants at boot, retrying");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(30));
            }
        }
    };
    let mut handles = HashMap::new();
    for tenant in tenants {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!("boot registration allowance exhausted; remaining tenants will be retried by the scanner");
            break;
        }
        match tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + Duration::from_secs(10)),
            key_custody.unseal_and_register_in_idempotent(
                &tenant.key_custody_backend,
                &tenant.sealed_key_material,
                tenant.id.as_str(),
            ),
        )
        .await
        {
            Ok(Ok(handle)) => {
                handles.insert(tenant.id, handle);
            }
            Ok(Err(e)) => {
                tracing::error!(store.id = %tenant.id, error = %e, "failed to register store with key custody");
            }
            Err(_) => {
                tracing::error!(store.id = %tenant.id, "registering a store with key custody exceeded its deadline");
            }
        }
    }
    handles
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    use super::*;

    const TOKEN: &str = "an-engine-token-of-at-least-32-characters";

    /// A directory of its own under the system's temporary directory,
    /// removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "engine-run-{tag}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// What the standalone binary would start from, in `dir`.
    fn config(dir: &TempDir, env: &[(&str, &str)]) -> EngineConfig {
        EngineConfig {
            options: live_settings::OptionsFile::at(dir.0.join("engine.toml")),
            env: live_settings::Env::fixed(env.iter().copied()),
            database_path: dir.0.join("engine.db"),
            host: Host::Standalone,
        }
    }

    /// What monokulo would start an embedded engine from, in `dir`: the
    /// `[engine.*]` tables of its own options file, and a token of its own.
    fn embedded_config(dir: &TempDir, env: &[(&str, &str)]) -> EngineConfig {
        EngineConfig {
            options: live_settings::OptionsFile::at(dir.0.join("monokulo.toml")).scoped("engine"),
            env: live_settings::Env::fixed(env.iter().copied()),
            database_path: dir.0.join("engine.db"),
            host: Host::Embedded {
                token: shared::auth::RawToken::presented(TOKEN),
            },
        }
    }

    async fn call(
        engine: &Engine,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(shared::auth::ENGINE_TOKEN_HEADER, TOKEN)
            .header("content-type", "application/json")
            .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                40_000,
            ))))
            .body(body.map_or_else(Body::empty, |json| Body::from(json.to_string())))
            .unwrap();
        let response = engine.router().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    /// Started from a database file and an options file that don't exist
    /// yet, as on a first run: the engine serves its API through the router
    /// it hands its host, and its loops stop when it is shut down.
    #[tokio::test]
    async fn an_engine_starts_serves_its_api_and_shuts_down_cleanly() {
        let dir = TempDir::new("start");
        let engine = Engine::start(config(&dir, &[("ENGINE_TOKEN", TOKEN)]))
            .await
            .unwrap();
        assert!(dir.0.join("engine.db").exists(), "the database is made");
        assert_eq!(
            engine.bind_address(),
            "127.0.0.1:8443".parse().unwrap(),
            "server.bind's default"
        );

        let (status, body) = call(&engine, "GET", "/status", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        assert_eq!(
            engine.shutdown(Duration::from_secs(5)).await,
            Stopped::Cleanly
        );
    }

    /// The API refuses a request without the token, through the library's
    /// router just as over HTTP.
    #[tokio::test]
    async fn the_router_still_requires_the_engine_token() {
        let dir = TempDir::new("token-check");
        let engine = Engine::start(config(&dir, &[("ENGINE_TOKEN", TOKEN)]))
            .await
            .unwrap();
        let request = Request::builder()
            .uri("/status")
            .body(Body::empty())
            .unwrap();
        let response = engine.router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        engine.shutdown(Duration::from_secs(5)).await;
    }

    /// Without an engine token, nothing could talk to the engine: it
    /// doesn't start, and says why, without ending the process that hosts
    /// it.
    #[tokio::test]
    async fn without_an_engine_token_it_does_not_start() {
        let dir = TempDir::new("no-token");
        let Err(error) = Engine::start(config(&dir, &[])).await else {
            panic!("started without a token");
        };
        assert!(matches!(error, StartError::Token(_)), "{error}");
        assert!(error.to_string().contains("ENGINE_TOKEN"), "{error}");
        assert!(
            !dir.0.join("engine.db").exists(),
            "nothing is opened before the token is checked"
        );
    }

    /// A database path that can't be a database (here, a directory) is an
    /// error naming the path.
    #[tokio::test]
    async fn a_database_that_cannot_be_opened_is_an_error() {
        let dir = TempDir::new("bad-db");
        let mut config = config(&dir, &[("ENGINE_TOKEN", TOKEN)]);
        config.database_path = dir.0.clone();
        let Err(error) = Engine::start(config).await else {
            panic!("started on a directory");
        };
        let StartError::Database { path, source: _ } = &error else {
            panic!("not a database error: {error}");
        };
        assert_eq!(path, &dir.0);
        assert!(
            error.to_string().contains(&*dir.0.to_string_lossy()),
            "{error}"
        );
    }

    /// A setting saved through one engine's API is in the options file and
    /// the database the next engine starts from: stopping and starting
    /// again in one process carries on.
    #[tokio::test]
    async fn a_second_start_on_the_same_files_carries_on() {
        let dir = TempDir::new("restart");
        let first = Engine::start(config(&dir, &[("ENGINE_TOKEN", TOKEN)]))
            .await
            .unwrap();
        let (status, body) = call(
            &first,
            "POST",
            "/api/v1/admin/settings",
            Some(serde_json::json!({
                "scalars": { "payment.confirmations_required": "7" }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            first.shutdown(Duration::from_secs(5)).await,
            Stopped::Cleanly
        );
        let saved = std::fs::read_to_string(dir.0.join("engine.toml")).unwrap();
        assert!(saved.contains("confirmations_required = 7"), "{saved}");

        let second = Engine::start(config(&dir, &[("ENGINE_TOKEN", TOKEN)]))
            .await
            .unwrap();
        let (status, body) = call(&second, "GET", "/api/v1/admin/settings", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let setting = &body["scalars"]["payment.confirmations_required"];
        assert_eq!(setting["value"], "7", "{setting}");
        assert_eq!(setting["source"], "toml", "{setting}");
        assert_eq!(
            second.shutdown(Duration::from_secs(5)).await,
            Stopped::Cleanly
        );
    }

    /// Inside monokulo: started with the token monokulo made (no
    /// `ENGINE_TOKEN` anywhere), it answers through its router, and keeps
    /// its settings in the `[engine.*]` tables of monokulo's file, leaving
    /// monokulo's own keys as they were.
    #[tokio::test]
    async fn an_embedded_engine_uses_its_hosts_token_and_its_table_of_the_file() {
        let dir = TempDir::new("embedded");
        std::fs::write(
            dir.0.join("monokulo.toml"),
            "[server]\nbind = \"0.0.0.0:8081\"\n\n[engine.payment]\nconfirmations_required = 6\n",
        )
        .unwrap();
        let engine = Engine::start(embedded_config(&dir, &[])).await.unwrap();

        let (status, body) = call(&engine, "GET", "/api/v1/admin/settings", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let confirmations = &body["scalars"]["payment.confirmations_required"];
        assert_eq!(confirmations["value"], "6", "{confirmations}");
        assert_eq!(
            confirmations["set_with"], "--engine-payment-confirmations-required",
            "monokulo's option names: {confirmations}"
        );
        for standalone in [
            "server.bind",
            "server.token",
            "logging.level",
            "logging.format",
        ] {
            assert!(
                body["scalars"].get(standalone).is_none(),
                "{standalone} is left out inside monokulo"
            );
        }

        let (status, body) = call(
            &engine,
            "POST",
            "/api/v1/admin/settings",
            Some(serde_json::json!({ "scalars": { "payment.confirmations_required": "8" } })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = call(
            &engine,
            "POST",
            "/api/v1/admin/settings",
            Some(serde_json::json!({ "scalars": { "server.bind": "0.0.0.0:9999" } })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["fields"][0]["key"], "server.bind", "{body}");

        let saved = std::fs::read_to_string(dir.0.join("monokulo.toml")).unwrap();
        assert_eq!(
            saved,
            "[server]\nbind = \"0.0.0.0:8081\"\n\n[engine.payment]\nconfirmations_required = 8\n"
        );
        assert_eq!(
            engine.shutdown(Duration::from_secs(5)).await,
            Stopped::Cleanly
        );
    }

    /// The standalone engine's own settings do nothing inside monokulo:
    /// given in the file, on the command line or in the environment, each
    /// stops the engine, named as it was given, before anything is opened.
    #[tokio::test]
    async fn an_embedded_engine_refuses_the_standalone_engines_settings() {
        let dir = TempDir::new("embedded-refused");
        std::fs::write(
            dir.0.join("monokulo.toml"),
            "[engine.server]\nbind = \"0.0.0.0:8443\"\n",
        )
        .unwrap();
        let mut config = embedded_config(&dir, &[("ENGINE_TOKEN", TOKEN)]);
        config.env = config.env.with_cli(HashMap::from([(
            "logging.level".to_owned(),
            "debug".to_owned(),
        )]));
        let Err(error) = Engine::start(config).await else {
            panic!("started with the standalone engine's settings");
        };
        let StartError::NotWhenEmbedded(given) = &error else {
            panic!("not refused for them: {error}");
        };
        assert_eq!(
            given,
            "engine.server.bind in the options file, ENGINE_TOKEN, --engine-logging-level"
        );
        assert!(
            !dir.0.join("engine.db").exists(),
            "nothing is opened before they are refused"
        );
    }
}
