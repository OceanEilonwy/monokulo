//! What `monokulo-bell` says of itself on its status listener
//! (`status.listen`, `GET /status`, [`service_runtime::status`]): its
//! addresses, how many registrations it holds, which integrations are
//! on and how the last ding went. Counts, times and readiness only: no
//! token, endpoint, payment, merchant or location.

use std::net::SocketAddr;

use serde::Serialize;
use service_runtime::status::Event;

use crate::Bell;
use crate::pos::{ByIntegration, Integration};

/// What became of a ding, as the status names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Dinged {
    /// The till's push service took it.
    Sent,
    /// The push service says the subscription is gone: its registration
    /// was deleted.
    Gone,
    /// It couldn't be sent, or the push service didn't take it (limiting
    /// the Bell included).
    Failed,
}

/// Whether an integration is served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Served {
    /// Its application is set, with its secret.
    Ready,
    /// Its application isn't set.
    Off,
}

impl From<bool> for Served {
    fn from(on: bool) -> Self {
        if on { Served::Ready } else { Served::Off }
    }
}

/// The status, as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// `monokulo-bell`.
    pub service: &'static str,
    pub version: &'static str,
    /// The commit it was built from, when the build said.
    pub commit: Option<&'static str>,
    /// Unix seconds.
    pub started_at: i64,
    /// The address its listener is bound to.
    pub listen: SocketAddr,
    /// `bell.public_uri`.
    pub public_uri: String,
    pub registrations: Registrations,
    /// Every integration, by point of sale and environment.
    pub points_of_sale: ByIntegration<Served>,
    /// The last ding sent since the Bell started.
    pub last_ding: Option<Event<Dinged>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Registrations {
    /// Every integration's; `None` when the database couldn't be read
    /// (said in the log).
    pub stored: Option<u64>,
}

impl Bell {
    /// The Bell's status now, listening on `listen` since `started_at`.
    pub async fn report(&self, listen: SocketAddr, started_at: i64) -> Report {
        let shared = &self.shared;
        let stored = match shared.store.count().await {
            Ok(stored) => Some(stored),
            Err(error) => {
                tracing::warn!(error = %error, "the status couldn't count the registrations");
                None
            }
        };
        Report {
            service: "monokulo-bell",
            version: env!("CARGO_PKG_VERSION"),
            commit: service_runtime::status::commit(),
            started_at,
            listen,
            public_uri: shared.public_uri.as_str().to_owned(),
            registrations: Registrations { stored },
            points_of_sale: ByIntegration(
                Integration::ALL
                    .into_iter()
                    .map(|integration| {
                        (
                            integration,
                            shared.adapters.get(integration).is_some().into(),
                        )
                    })
                    .collect(),
            ),
            last_ding: shared.dings.last(),
        }
    }
}
