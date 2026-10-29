//! What `monokulo-push-relay` says of itself on its status listener
//! (`status.listen`, `GET /status`, [`service_runtime::status`]): its
//! addresses, its apps, how many handles it holds, whether APNs and FCM
//! are ready and how the last push it handed on went. Counts, times and
//! readiness only: no token, endpoint, body, key or handle id.
//!
//! Inside monokulo the admin page's relay card says this instead.

use std::net::SocketAddr;

use serde::Serialize;
use service_runtime::status::Event;

use crate::upstream::Delivery;
use crate::Relay;

/// What became of the last push handed to APNs or FCM, as the status
/// names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Delivered {
    /// Taken.
    Sent,
    /// The device's token is dead: its handle was forgotten.
    Gone,
    /// Refused for a reason of this relay's (its credentials, its settings).
    Refused,
    /// The service was down, busy or limiting this relay.
    Unavailable,
    /// Larger than the service carries for the device.
    TooLarge,
}

impl From<Delivery> for Delivered {
    fn from(delivery: Delivery) -> Self {
        match delivery {
            Delivery::Delivered => Delivered::Sent,
            Delivery::Gone => Delivered::Gone,
            Delivery::Refused => Delivered::Refused,
            Delivery::Unavailable { .. } => Delivered::Unavailable,
            Delivery::TooLarge => Delivered::TooLarge,
        }
    }
}

/// Whether a platform's credentials are set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Readiness {
    Ready,
    NotConfigured,
}

impl From<bool> for Readiness {
    fn from(ready: bool) -> Self {
        if ready {
            Readiness::Ready
        } else {
            Readiness::NotConfigured
        }
    }
}

/// The status, as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// `monokulo-push-relay`.
    pub service: &'static str,
    pub version: &'static str,
    /// The commit it was built from, when the build said.
    pub commit: Option<&'static str>,
    /// Unix seconds.
    pub started_at: i64,
    /// The address its listener is bound to.
    pub listen: SocketAddr,
    /// `server.public_uri`.
    pub public_uri: String,
    /// The apps devices may register for, by name.
    pub apps: Vec<String>,
    pub handles: Handles,
    pub platforms: Platforms,
    /// The last push handed to APNs or FCM since the relay started.
    pub last_delivery: Option<Event<Delivered>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Handles {
    /// Handles that haven't expired; `None` when the database couldn't be
    /// read (said in the log).
    pub stored: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Platforms {
    pub apns: Readiness,
    pub fcm: Readiness,
}

impl Relay {
    /// The relay's status now, listening on `listen` since `started_at`.
    pub async fn report(&self, listen: SocketAddr, started_at: i64) -> Report {
        let shared = &self.shared;
        let stored = match self.handles_stored(service_runtime::time::now_unix()).await {
            Ok(stored) => Some(stored),
            Err(error) => {
                tracing::warn!(error = %error, "the status couldn't count the handles");
                None
            }
        };
        Report {
            service: "monokulo-push-relay",
            version: env!("CARGO_PKG_VERSION"),
            commit: service_runtime::status::commit(),
            started_at,
            listen,
            public_uri: shared.public_uri.origin().to_string(),
            apps: shared
                .apps
                .iter()
                .map(|(name, _)| name.as_str().to_owned())
                .collect(),
            handles: Handles { stored },
            platforms: Platforms {
                apns: shared.apns.is_some().into(),
                fcm: shared.fcm.is_some().into(),
            },
            last_delivery: shared.last_delivery.get(),
        }
    }
}
