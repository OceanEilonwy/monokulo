//! Handles: where a push to one of this relay's endpoints goes. A handle
//! is made by an app's registration, for one device token, app and sender
//! (the VAPID key it was registered with), and kept in the relay's own
//! SQLite file (`store.path`), never in monokulo's database.
//!
//! The same four registered again are the same handle with its expiry
//! pushed back, so an app re-registering on every launch makes no new
//! endpoint. One device registered for two senders (two stores' monokulos,
//! or a monokulo and a Bell) has two handles, which nothing links, and
//! each takes pushes signed by its own sender only.

use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use push::VapidPublicKey;
use rand::Rng as _;
use rusqlite::{params, Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};

/// The handles table and its index.
const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../migrations/0001_handles.sql"))];

/// A handle's id: `ph_` and 32 random hex digits, the last part of its
/// endpoint. Unguessable, so an endpoint can't be found by trying; it is
/// still useless without the sender's signature.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HandleId(String);

impl HandleId {
    const PREFIX: &'static str = "ph_";

    fn generate() -> Self {
        let mut bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        HandleId(format!("{}{}", Self::PREFIX, hex::encode(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HandleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Not a handle id of this relay's making.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a handle id (ph_ and 32 lowercase hex digits)")]
pub struct HandleIdError;

impl FromStr for HandleId {
    type Err = HandleIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let digits = text.strip_prefix(Self::PREFIX).ok_or(HandleIdError)?;
        if digits.len() == 32
            && digits
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(HandleId(text.to_owned()))
        } else {
            Err(HandleIdError)
        }
    }
}

/// What a handle delivers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Platform {
    /// An iOS notification (`apns-push-type: alert`), the ciphertext
    /// carried for the app's Notification Service Extension to decrypt.
    #[serde(rename = "apns")]
    Apns,
    /// An iOS Live Activity's updates, by its push token: plain JSON, as
    /// iOS applies them with no code of the app's to decrypt.
    #[serde(rename = "apns-liveactivity")]
    ApnsLiveActivity,
    /// Starting a Live Activity remotely (iOS 17.2), by the app's
    /// push-to-start token: plain JSON.
    #[serde(rename = "apns-pushtostart")]
    ApnsPushToStart,
    /// An Android data message through Firebase, the ciphertext carried for
    /// the app to decrypt.
    #[serde(rename = "fcm")]
    Fcm,
}

impl Platform {
    pub const fn as_str(self) -> &'static str {
        match self {
            Platform::Apns => "apns",
            Platform::ApnsLiveActivity => "apns-liveactivity",
            Platform::ApnsPushToStart => "apns-pushtostart",
            Platform::Fcm => "fcm",
        }
    }

    fn from_stored(text: &str) -> Option<Self> {
        [
            Platform::Apns,
            Platform::ApnsLiveActivity,
            Platform::ApnsPushToStart,
            Platform::Fcm,
        ]
        .into_iter()
        .find(|platform| platform.as_str() == text)
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An app's name as it registers (`"monokular"`) and as `handles.apps`
/// configures it: 1 to 32 lowercase letters, digits and hyphens, starting
/// with a letter.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AppName(String);

impl AppName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AppName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for AppName {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let bytes = text.as_bytes();
        let fits = (1..=32).contains(&bytes.len())
            && bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');
        if fits {
            Ok(AppName(text))
        } else {
            Err(format!(
                "{text:?} isn't an app name (1 to 32 lowercase letters, digits and hyphens, starting with a letter)"
            ))
        }
    }
}

impl From<AppName> for String {
    fn from(name: AppName) -> Self {
        name.0
    }
}

/// A device's token for one platform: an APNs device, activity or
/// push-to-start token (hex, as iOS hands it over as bytes; kept in
/// lowercase), or an FCM registration token (letters, digits, `-`, `_`
/// and `:`). Bounded, so a
/// row stays small, and of characters that go into a URL path as they
/// are. Whoever holds it can address the device through APNs or FCM with
/// the app's credentials, so `Debug` doesn't print it and no log line
/// carries it.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceToken(String);

impl DeviceToken {
    /// The longest token taken. APNs device tokens are 32 bytes (64 hex
    /// digits) today, FCM's about 160 characters; both have grown before.
    pub const MAX_LEN: usize = 1024;

    pub fn parse(platform: Platform, text: &str) -> Result<Self, String> {
        if text.is_empty() || text.len() > Self::MAX_LEN {
            return Err(format!("a token is 1 to {} characters", Self::MAX_LEN));
        }
        let fits = match platform {
            Platform::Apns | Platform::ApnsLiveActivity | Platform::ApnsPushToStart => {
                text.len() % 2 == 0 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
            }
            Platform::Fcm => text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':')),
        };
        if fits {
            Ok(DeviceToken(match platform {
                // Hex is the app's spelling of the token's bytes, in
                // either case: one spelling, so one device and sender
                // are one handle however the app writes them.
                Platform::Apns | Platform::ApnsLiveActivity | Platform::ApnsPushToStart => {
                    text.to_ascii_lowercase()
                }
                // FCM's tokens are case-sensitive.
                Platform::Fcm => text.to_owned(),
            }))
        } else {
            Err(match platform {
                Platform::Fcm => {
                    "an FCM token is letters, digits, hyphens, underscores and colons".to_owned()
                }
                _ => "an APNs token is the token's bytes in hex".to_owned(),
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DeviceToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceToken(..)")
    }
}

/// What a registration asks for.
#[derive(Debug, Clone)]
pub struct NewHandle {
    pub platform: Platform,
    pub app: AppName,
    pub token: DeviceToken,
    pub sender: VapidPublicKey,
}

/// A handle, as a push to it is handed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handle {
    pub id: HandleId,
    pub platform: Platform,
    pub app: AppName,
    pub token: DeviceToken,
    /// The only key whose pushes it takes.
    pub sender: VapidPublicKey,
    /// Unix seconds; it is unknown from then on.
    pub expires_at: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("the handles database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("the handles database: {0}")]
    Pool(#[from] sqlite::PoolError),
    #[error("the handles database can't be opened at {path}: {error}")]
    Open { path: String, error: String },
    #[error("a stored handle doesn't read: {0}")]
    Corrupt(String),
}

/// The handles, on one connection: every push reads its handle and,
/// once APNs or FCM took it, writes its new expiry, so a separate reader
/// would buy nothing.
#[derive(Clone)]
pub struct HandleStore {
    pool: sqlite::Pool<Connection>,
}

impl HandleStore {
    /// Opens the database at `path`, making it and its directory if they
    /// aren't there, and brings its schema up to date.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let open_error = |error: String| StoreError::Open {
            path: path.display().to_string(),
            error,
        };
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|error| open_error(error.to_string()))?;
        }
        let connection = Connection::open(path).map_err(|error| open_error(error.to_string()))?;
        sqlite::configure_writer(&connection)?;
        sqlite::migrations::apply(&connection, MIGRATIONS)?;
        let pool = sqlite::Pool::start("push-relay-db", vec![connection])
            .map_err(|error| open_error(error.to_string()))?;
        Ok(HandleStore { pool })
    }

    /// The handle for `new`, made if there is none, and live for `ttl`
    /// from `now` either way.
    pub async fn register(
        &self,
        new: NewHandle,
        now: i64,
        ttl: Duration,
    ) -> Result<Handle, StoreError> {
        let expires_at = expiry(now, ttl);
        let id = HandleId::generate();
        self.pool
            .run(move |connection| {
                // An expired handle not yet swept is taken back too: the
                // device and sender are the same, so is the endpoint.
                let id: String = connection.query_row(
                    "INSERT INTO handles
                         (id, platform, app, token, vapid_public_key, created_at, last_used_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)
                     ON CONFLICT (platform, app, token, vapid_public_key)
                     DO UPDATE SET last_used_at = excluded.last_used_at,
                                   expires_at = excluded.expires_at
                     RETURNING id",
                    params![
                        id.as_str(),
                        new.platform.as_str(),
                        new.app.as_str(),
                        new.token.as_str(),
                        new.sender.to_base64url(),
                        now,
                        expires_at,
                    ],
                    |row| row.get(0),
                )?;
                Ok(Handle {
                    id: id.parse().map_err(|_| StoreError::Corrupt(id.clone()))?,
                    platform: new.platform,
                    app: new.app,
                    token: new.token,
                    sender: new.sender,
                    expires_at,
                })
            })
            .await
    }

    /// The handle `id`, unless it has expired by `now` (or never was).
    pub async fn find(&self, id: &HandleId, now: i64) -> Result<Option<Handle>, StoreError> {
        let id = id.clone();
        self.pool
            .run(move |connection| {
                let row = connection
                    .query_row(
                        "SELECT platform, app, token, vapid_public_key, expires_at
                         FROM handles WHERE id = ?1 AND expires_at > ?2",
                        params![id.as_str(), now],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, i64>(4)?,
                            ))
                        },
                    )
                    .optional()?;
                row.map(|(platform, app, token, sender, expires_at)| {
                    let corrupt = |what: &str| StoreError::Corrupt(format!("{id}: its {what}"));
                    let platform = Platform::from_stored(&platform).ok_or(corrupt("platform"))?;
                    Ok(Handle {
                        id: id.clone(),
                        platform,
                        app: AppName::try_from(app).map_err(|_| corrupt("app"))?,
                        token: DeviceToken::parse(platform, &token)
                            .map_err(|_| corrupt("token"))?,
                        sender: VapidPublicKey::from_base64url(&sender)
                            .map_err(|_| corrupt("key"))?,
                        expires_at,
                    })
                })
                .transpose()
            })
            .await
    }

    /// Marks `id` used at `now`: it lives for `ttl` from then.
    pub async fn touch(&self, id: &HandleId, now: i64, ttl: Duration) -> Result<(), StoreError> {
        let id = id.clone();
        let expires_at = expiry(now, ttl);
        self.pool
            .run(move |connection| {
                connection.execute(
                    "UPDATE handles SET last_used_at = ?2, expires_at = ?3 WHERE id = ?1",
                    params![id.as_str(), now, expires_at],
                )?;
                Ok(())
            })
            .await
    }

    /// Forgets `id`: APNs or FCM said its token is gone.
    pub async fn delete(&self, id: &HandleId) -> Result<(), StoreError> {
        let id = id.clone();
        self.pool
            .run(move |connection| {
                connection.execute("DELETE FROM handles WHERE id = ?1", params![id.as_str()])?;
                Ok(())
            })
            .await
    }

    /// Forgets every handle expired by `now`, and says how many there were.
    pub async fn sweep(&self, now: i64) -> Result<usize, StoreError> {
        self.pool
            .run(move |connection| {
                Ok(connection
                    .execute("DELETE FROM handles WHERE expires_at <= ?1", params![now])?)
            })
            .await
    }
}

fn expiry(now: i64, ttl: Duration) -> i64 {
    now.saturating_add(i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use push::VapidKey;

    const NOW: i64 = 1_800_000_000;
    const TTL: Duration = Duration::from_secs(90 * 86_400);

    pub(crate) fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!(
                "push-relay-{tag}-{}-{}",
                std::process::id(),
                HandleId::generate()
            ))
            .join("relay.db")
    }

    fn new(platform: Platform, token: &str, sender: &VapidKey) -> NewHandle {
        NewHandle {
            platform,
            app: AppName::try_from("monokular".to_owned()).unwrap(),
            token: DeviceToken::parse(platform, token).unwrap(),
            sender: sender.public(),
        }
    }

    #[tokio::test]
    async fn the_database_is_made_at_its_path_with_only_the_handles_table() {
        let path = temp_path("tables");
        HandleStore::open(&path).unwrap();
        let connection = Connection::open(&path).unwrap();
        let mut tables: Vec<String> = connection
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        // The migration runner's own bookkeeping, and the handles.
        tables.retain(|table| !table.ends_with("_migrations"));
        assert_eq!(tables, ["handles"]);
        // Opened again, nothing runs twice.
        HandleStore::open(&path).unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The same device, app and sender registered again keep their handle
    /// and push its expiry back; a second sender gets a handle of its own.
    #[tokio::test]
    async fn registering_again_keeps_the_handle_and_refreshes_its_expiry() {
        let path = temp_path("again");
        let store = HandleStore::open(&path).unwrap();
        let (monokulo, bell) = (VapidKey::generate(), VapidKey::generate());
        let token = "a1b2".repeat(16);
        let first = store
            .register(new(Platform::Apns, &token, &monokulo), NOW, TTL)
            .await
            .unwrap();
        assert!(first.id.as_str().starts_with("ph_"), "{}", first.id);
        assert_eq!(first.expires_at, NOW + 90 * 86_400);

        let again = store
            .register(new(Platform::Apns, &token, &monokulo), NOW + 100, TTL)
            .await
            .unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(again.expires_at, first.expires_at + 100);

        let other = store
            .register(new(Platform::Apns, &token, &bell), NOW, TTL)
            .await
            .unwrap();
        assert_ne!(other.id, first.id);
        assert_eq!(
            store.find(&other.id, NOW).await.unwrap().unwrap().sender,
            bell.public()
        );
        // Another platform for the same token is another handle too.
        let activity = store
            .register(new(Platform::ApnsLiveActivity, &token, &monokulo), NOW, TTL)
            .await
            .unwrap();
        assert_ne!(activity.id, first.id);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A handle lives for its TTL after its last use, then is unknown; a
    /// use pushes it back, and the sweep forgets only expired ones.
    #[tokio::test]
    async fn a_handle_expires_by_its_last_use() {
        let path = temp_path("expiry");
        let store = HandleStore::open(&path).unwrap();
        let sender = VapidKey::generate();
        let handle = store
            .register(new(Platform::Fcm, "fcm:token_1-x", &sender), NOW, TTL)
            .await
            .unwrap();
        let ttl = 90 * 86_400;
        let found = store
            .find(&handle.id, NOW + ttl - 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found, handle);
        assert_eq!(store.find(&handle.id, NOW + ttl).await.unwrap(), None);

        store.touch(&handle.id, NOW + 1_000, TTL).await.unwrap();
        assert!(store.find(&handle.id, NOW + ttl).await.unwrap().is_some());
        assert_eq!(store.sweep(NOW + ttl).await.unwrap(), 0);
        assert_eq!(store.sweep(NOW + 1_000 + ttl).await.unwrap(), 1);
        assert_eq!(store.find(&handle.id, NOW).await.unwrap(), None);

        // Registered again after the sweep: a new handle.
        let back = store
            .register(new(Platform::Fcm, "fcm:token_1-x", &sender), NOW, TTL)
            .await
            .unwrap();
        assert_ne!(back.id, handle.id);
        store.delete(&back.id).await.unwrap();
        assert_eq!(store.find(&back.id, NOW).await.unwrap(), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// An expired handle the hourly sweep hasn't reached yet is unknown to
    /// pushes, and the same registration takes it back, endpoint and all:
    /// the app needn't hand its sender a new one.
    #[tokio::test]
    async fn registering_again_takes_back_an_expired_handle_not_yet_swept() {
        let path = temp_path("revive");
        let store = HandleStore::open(&path).unwrap();
        let sender = VapidKey::generate();
        let token = "c3".repeat(32);
        let handle = store
            .register(new(Platform::Apns, &token, &sender), NOW, TTL)
            .await
            .unwrap();
        let later = NOW + 90 * 86_400 + 10;
        assert_eq!(store.find(&handle.id, later).await.unwrap(), None);
        let back = store
            .register(new(Platform::Apns, &token, &sender), later, TTL)
            .await
            .unwrap();
        assert_eq!(back.id, handle.id);
        assert_eq!(back.expires_at, later + 90 * 86_400);
        assert!(store.find(&handle.id, later).await.unwrap().is_some());
        assert_eq!(store.sweep(later).await.unwrap(), 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn handle_ids_are_ph_and_32_hex_digits() {
        let id = HandleId::generate();
        assert_eq!(id.as_str().len(), 35);
        assert_eq!(id.as_str().parse::<HandleId>(), Ok(id));
        for bad in [
            "",
            "ph_",
            "pt_0123456789abcdef0123456789abcdef",
            "ph_0123456789ABCDEF0123456789abcdef",
            "ph_0123456789abcdef0123456789abcde",
            "ph_0123456789abcdef0123456789abcdef0",
        ] {
            assert_eq!(bad.parse::<HandleId>(), Err(HandleIdError), "{bad}");
        }
    }

    #[test]
    fn tokens_are_checked_for_their_platform() {
        assert!(DeviceToken::parse(Platform::Apns, &"ab".repeat(32)).is_ok());
        assert_eq!(
            DeviceToken::parse(Platform::ApnsLiveActivity, "ABCDEF01")
                .unwrap()
                .as_str(),
            "abcdef01"
        );
        // FCM's are taken as they are.
        assert_eq!(
            DeviceToken::parse(Platform::Fcm, "AbC:d").unwrap().as_str(),
            "AbC:d"
        );
        for bad in ["", "abc", "zz", "ab/cd", &"ab".repeat(513)] {
            assert!(DeviceToken::parse(Platform::Apns, bad).is_err(), "{bad}");
        }
        assert!(DeviceToken::parse(Platform::Fcm, "cX9:APA91b-Hx_y").is_ok());
        for bad in ["", "a/b", "a b", "a?b", "é"] {
            assert!(DeviceToken::parse(Platform::Fcm, bad).is_err(), "{bad}");
        }
        let token = DeviceToken::parse(Platform::Fcm, "secret-token").unwrap();
        assert_eq!(format!("{token:?}"), "DeviceToken(..)");
    }

    #[test]
    fn app_names_are_lowercase_and_short() {
        for good in ["monokular", "a", "my-app-2"] {
            assert!(AppName::try_from(good.to_owned()).is_ok(), "{good}");
        }
        for bad in ["", "Monokular", "2app", "my_app", &"a".repeat(33)] {
            assert!(AppName::try_from(bad.to_owned()).is_err(), "{bad}");
        }
    }
}
