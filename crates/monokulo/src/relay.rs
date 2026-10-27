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

use crate::settings::RELAY_TABLE;

#[cfg(feature = "relay")]
pub use embedded::*;

#[cfg(not(feature = "relay"))]
pub use without::*;

#[cfg(feature = "relay")]
mod embedded {
    use live_settings::{Env, SettingSource, Snapshot};
    use push_relay::settings::{self, Host, STORE_PATH};
    use push_relay::Relay;

    use super::*;

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
        values: HashMap<String, String>,
        env: Env,
        enabled: bool,
    }

    /// Reads the relay's tables of `file` (a mistake in them stops
    /// monokulo, line by line) and its `--relay-…` options, `cli`.
    pub fn prepare(
        file: &OptionsFile,
        cli: HashMap<String, String>,
        enabled: bool,
    ) -> Result<Prepared, String> {
        let values = file
            .scoped(RELAY_TABLE)
            .read(&settings::embedded_settings())
            .map_err(|error| error.to_string())?;
        if enabled {
            if let Some(telemetry) = telemetry::global() {
                telemetry.host("push-relay", &["push_relay"], "push-relay-");
            }
        }
        Ok(Prepared {
            values,
            env: Env::process().with_cli(cli),
            enabled,
        })
    }

    /// The relay, serving.
    pub struct Running {
        served: tokio::task::JoinHandle<std::io::Result<()>>,
    }

    /// Checks every relay setting, together and with its secrets, and, when
    /// the relay is enabled, starts it on its own listener until the stop
    /// signal. Any problem is the error monokulo stops on.
    pub async fn start(prepared: Prepared, monokulo_db: &Path) -> Result<Option<Running>, String> {
        let Prepared {
            values,
            env,
            enabled,
        } = prepared;
        let store_default = Snapshot::new(values.clone(), env.clone()).source(&STORE_PATH)
            == SettingSource::Default;
        let mut config = settings::load(values, env, Host::Embedded)
            .await
            .map_err(|error| error.to_string())?;
        if !enabled {
            return Ok(None);
        }
        if store_default {
            config.store_path = monokulo_db.with_file_name(STORE_FILE);
        }
        let relay = Relay::start(&config)
            .await
            .map_err(|error| format!("the push relay didn't start: {error}"))?;
        let listener = tokio::net::TcpListener::bind(config.listen)
            .await
            .map_err(|error| {
                format!(
                    "the push relay failed to listen on {}: {error}",
                    config.listen
                )
            })?;
        let address = listener.local_addr().unwrap_or(config.listen);
        tracing::info!(relay.address = %address, "push relay listening");
        let served = tokio::spawn(async move {
            relay
                .serve(listener, service_runtime::shutdown::signal())
                .await
        });
        Ok(Some(Running { served }))
    }

    impl Running {
        /// Waits, up to the grace period, for requests in flight to finish
        /// after the stop signal.
        pub async fn stop(self) {
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

    /// Never made.
    pub enum Running {}

    pub async fn start(
        _prepared: Prepared,
        _monokulo_db: &Path,
    ) -> Result<Option<Running>, String> {
        Ok(None)
    }

    impl Running {
        pub async fn stop(self) {
            match self {}
        }
    }
}

#[cfg(all(test, feature = "relay"))]
mod tests {
    use super::*;

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

    /// A port nothing listens on, for a relay that is to listen there.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
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
        let prepared = prepare(&file(&text), HashMap::new(), false).unwrap();
        let db = monokulo_db("off");
        assert!(start(prepared, &db).await.unwrap().is_none());
        assert!(!db.with_file_name(STORE_FILE).exists());
        std::net::TcpListener::bind(("127.0.0.1", port)).expect("nothing listens there");

        let ids_alone = "[relay.apns]\nteam_id = \"TEAM123456\"\n";
        let prepared = prepare(&file(ids_alone), HashMap::new(), false).unwrap();
        let error = start(prepared, &db).await.err().unwrap();
        assert!(error.contains("apns.team_id"), "{error}");
    }

    #[tokio::test]
    async fn an_enabled_relay_without_its_public_address_stops_monokulo() {
        let prepared = prepare(&file(""), HashMap::new(), true).unwrap();
        let error = start(prepared, &monokulo_db("no-uri")).await.err().unwrap();
        assert!(error.contains("server.public_uri must be set"), "{error}");
    }

    /// On, it listens where its settings say, its database beside
    /// monokulo's, and answers as a relay.
    #[tokio::test]
    async fn an_enabled_relay_serves_on_its_own_listener_over_http() {
        let port = free_port();
        let cli = HashMap::from([("server.listen".to_owned(), format!("127.0.0.1:{port}"))]);
        let text = "[relay.server]\npublic_uri = \"https://relay.example.org\"\n";
        let prepared = prepare(&file(text), cli, true).unwrap();
        let db = monokulo_db("on");
        let running = start(prepared, &db).await.unwrap();
        assert!(running.is_some());
        assert!(db.with_file_name(STORE_FILE).exists());
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
            .await
            .unwrap();
        assert_eq!(answer.status(), reqwest::StatusCode::BAD_REQUEST);
        let problem: serde_json::Value = answer.json().await.unwrap();
        assert_eq!(
            problem["type"],
            "https://docs.monokulo.com/push-relay/#unknown-app"
        );
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
