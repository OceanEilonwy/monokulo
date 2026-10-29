//! Pushes to connected apps (monokular docs/design.md §5): an app that
//! isn't running hears of its store's sales from a push, not its event
//! stream. It registers where the pushes go (`/api/v1/push-targets`,
//! `db::push`): a Web Push subscription, encrypted to the device, or a
//! Live Activity behind the push relay, sent plain. Every push is signed
//! with this instance's one VAPID key ([`Push`]), which the app hands its
//! push service so that only this Monokulo can push to it.
//!
//! The key is made at the first start and kept in `vapid_keys`, its private
//! half encrypted with MONOKULO_ENCRYPTION_KEY (`crypto::Binding::VapidKey`);
//! a key that can't be read back stops the process, as any other secret
//! does, rather than quietly making a new one that every registered
//! subscription would refuse.
//!
//! Pushes ride the webhooks' pipeline (monokular docs/design.md §5):
//!
//! ```text
//! engine order_events ──SSE──▶ webhooks::subscriber
//!                                │ same transaction (`Db::queue_order_event`)
//!                                ├─▶ webhook_deliveries
//!                                └─▶ push_deliveries, one per target that wants the event
//! orders API (an app's order) ──▶ push_deliveries (`sale.waiting`)
//! push::delivery worker ──Web Push──▶ the target's endpoint (a distributor, or the relay)
//! ```
//!
//! What each push asks of the push service is [`policy`]'s, what it says
//! [`payload`]'s; [`delivery`] sends them.

pub mod delivery;
pub mod payload;
pub mod policy;

use std::sync::Arc;

use zeroize::Zeroizing;

use crate::crypto::{AtRestKey, Binding, CryptoError};
use crate::db::{Database, DbError, StoredVapidKey};

/// What this instance pushes with, and what tells the delivery worker
/// there's something to send now.
pub struct Push {
    vapid: Arc<::push::VapidKey>,
    wake: tokio::sync::Notify,
    client: tokio::sync::OnceCell<delivery::PushClient>,
    /// How many test pushes each store has sent in its current minute
    /// ("Send a test push" on its app's page): as many as test webhooks.
    pub tests: rate_limit::RateLimiter<crate::db::ConnectionId>,
}

/// What queueing a sale's pushes reads from the settings, read as each
/// sale event is queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queueing {
    /// `push.default_ttl_secs`.
    pub default_ttl: ::push::Ttl,
    /// Where an `encrypted` push's links start: the path of this
    /// Monokulo's web address, as an app on it reads the event stream.
    pub link_base: String,
}

impl Queueing {
    /// As the settings are now.
    pub fn now(settings: &crate::settings::MonokuloSettings) -> Self {
        Queueing {
            default_ttl: settings.push.load().default_ttl,
            link_base: settings
                .public_uri()
                .and_then(|uri| url::Url::parse(&uri).ok())
                .map(|uri| uri.path().trim_end_matches('/').to_owned())
                .unwrap_or_default(),
        }
    }
}

/// Why this instance's VAPID key couldn't be read or made.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("the database failed: {0}")]
    Db(#[from] DbError),
    #[error(
        "the stored VAPID key can't be decrypted ({0}): MONOKULO_ENCRYPTION_KEY isn't the key it was stored with"
    )]
    Decrypt(#[from] CryptoError),
    #[error("the stored VAPID key isn't one: {0}")]
    Unreadable(String),
    #[error("the stored VAPID public key isn't the stored private key's")]
    Mismatched,
}

impl Push {
    /// This instance's VAPID key from `db`, decrypted with `key`; made and
    /// stored first if there's none yet.
    pub async fn load_or_make(db: &Database, key: &AtRestKey) -> Result<Push, LoadError> {
        let stored = match db.read(|db| db.vapid_key()).await? {
            Some(stored) => stored,
            None => {
                let made = ::push::VapidKey::generate();
                let new = StoredVapidKey {
                    public_key: made.public().to_string(),
                    private_key_encrypted: encrypt_private(key, &made),
                    created_at: crate::now_unix(),
                };
                // Whoever stored one first, that one is kept: read back
                // what is stored, below, never assume it's this one.
                let stored = db.write(move |db| db.store_vapid_key(&new)).await?;
                tracing::info!(push.vapid_public_key = %stored.public_key, "made this Monokulo's VAPID key");
                stored
            }
        };
        let vapid = decrypt_private(key, &stored)?;
        Ok(Push::with_key(vapid))
    }

    fn with_key(vapid: ::push::VapidKey) -> Push {
        Push {
            vapid: Arc::new(vapid),
            wake: tokio::sync::Notify::new(),
            client: tokio::sync::OnceCell::new(),
            tests: rate_limit::RateLimiter::new(crate::webhooks::test_delivery::TESTS_PER_MINUTE),
        }
    }

    /// A push with a key of its own, never stored: for a test's state.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests() -> Push {
        Push::with_key(::push::VapidKey::generate())
    }

    /// The public half of the key, which apps hand their push service.
    pub fn vapid_public_key(&self) -> ::push::VapidPublicKey {
        self.vapid.public()
    }

    /// The key pushes are signed with, for the delivery worker's senders.
    pub(crate) fn vapid(&self) -> Arc<::push::VapidKey> {
        Arc::clone(&self.vapid)
    }

    /// What pushes are sent with, built the first time it's asked for
    /// (with no `sub` claim: its user sets the one it signs with, from the
    /// settings, before sending). Fails only if TLS can't be set up; asked
    /// again, it tries again.
    pub async fn client(&self) -> reqwest::Result<&delivery::PushClient> {
        self.client
            .get_or_try_init(|| async { delivery::PushClient::build(self.vapid(), None) })
            .await
    }

    /// Wakes the delivery worker (it also looks every few seconds).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Until [`Self::wake`] is called (or was since the last time).
    pub async fn woken(&self) {
        self.wake.notified().await;
    }
}

/// `vapid`'s private scalar, as `vapid_keys.private_key_encrypted` keeps
/// it: hex, encrypted with `key`.
fn encrypt_private(key: &AtRestKey, vapid: &::push::VapidKey) -> String {
    let hex = Zeroizing::new(hex::encode(*vapid.to_bytes()));
    crate::crypto::encrypt(key, Binding::VapidKey, &hex)
}

/// The key `stored` holds, decrypted with `key`, checked against its
/// stored public half.
fn decrypt_private(
    key: &AtRestKey,
    stored: &StoredVapidKey,
) -> Result<::push::VapidKey, LoadError> {
    let hex = Zeroizing::new(crate::crypto::decrypt(
        key,
        Binding::VapidKey,
        &stored.private_key_encrypted,
    )?);
    let mut bytes = Zeroizing::new([0u8; 32]);
    hex::decode_to_slice(hex.as_bytes(), &mut *bytes).map_err(|e| {
        LoadError::Unreadable(format!("its private key isn't 32 bytes of hex: {e}"))
    })?;
    let vapid =
        ::push::VapidKey::from_bytes(&bytes).map_err(|e| LoadError::Unreadable(e.to_string()))?;
    if vapid.public().to_string() != stored.public_key {
        return Err(LoadError::Mismatched);
    }
    Ok(vapid)
}

/// How a push target's pushes are sent, by name (`db::push::TargetKind`
/// with what each needs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PushTargetKind {
    /// Web Push, encrypted to the device (RFC 8291).
    Encrypted,
    /// Plain JSON to the push relay, for an iOS Live Activity.
    LiveActivity,
}

impl PushTargetKind {
    pub const ALL: [PushTargetKind; 2] = [PushTargetKind::Encrypted, PushTargetKind::LiveActivity];

    /// The kind named `name`, as [`Self::as_str`] writes it.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == name)
    }

    /// Its name, as the API and `push_targets.kind` write it.
    pub fn as_str(self) -> &'static str {
        match self {
            PushTargetKind::Encrypted => "encrypted",
            PushTargetKind::LiveActivity => "live-activity",
        }
    }
}

impl schemars::JsonSchema for PushTargetKind {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PushTargetKind".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let names: Vec<&str> = PushTargetKind::ALL.iter().map(|k| k.as_str()).collect();
        schemars::json_schema!({
            "type": "string",
            "enum": names,
            "description": "How its pushes are sent: `encrypted`, Web Push encrypted to the device (RFC 8291) with the subscription's `keys`, to a UnifiedPush distributor or the push relay; or `live-activity`, plain JSON to the push relay for an iOS Live Activity, with no keys."
        })
    }
}

#[cfg(test)]
mod pipeline_tests;
#[cfg(test)]
mod tests;
