//! Handles: where a push to one of this relay's endpoints goes. A handle
//! is made by an app's registration, for one device token, app and sender
//! (the VAPID key it was registered with), and kept in the relay's own
//! SQLite file (`store.path`), never in monokulo's database.
//!
//! The same four registered again are the same handle with its expiry
//! pushed back, so an app re-registering on every launch makes no new
//! endpoint. One device registered for two senders (two stores' monokulos,
//! or a monokulo and a Bell) has two handles, each taking pushes signed by
//! its own sender only.
//!
//! The file can't link them. A handle's id is derived from the four it was
//! registered for, and a row keeps only a hash of the id, the platform
//! and app, and the token and sender key sealed under a key derived from
//! the id: a row is readable only with its endpoint in hand, and two rows
//! of one device share no value but those every device of that app and
//! platform shares. The relay itself sees the token and the sender
//! together at every registration and push, as it must to hand pushes on;
//! a copy of the file (a backup, or the one moved from inside monokulo to
//! a relay of its own) is what this keeps from linking them. Log lines
//! name a handle by a tag of its row, never its id.
//!
//! A handle no push has reached yet lives `handles.unused_ttl_days` from
//! its registration; once APNs or FCM has taken a push for it,
//! `handles.ttl_days` from its last registration or delivery. Registering
//! needs no credential, so a made-up token's row goes in days, not
//! months.

use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use aes_gcm::aead::{Aead as _, Key, KeyInit as _, Nonce, Payload};
use aes_gcm::Aes256Gcm;
use hkdf::Hkdf;
use push::VapidPublicKey;
use rand::Rng as _;
use rusqlite::{params, Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

/// The handles table and its index: the first shape kept tokens and keys
/// in the clear, so the second replaces it.
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_handles.sql")),
    (2, include_str!("../migrations/0002_sealed_handles.sql")),
];

/// What a handle's id is derived from, before the four it was registered
/// for.
const ID_CONTEXT: &[u8] = b"monokulo push-relay handle id v1";

/// HKDF's `info` for the key a row is sealed under.
const SEAL_CONTEXT: &[u8] = b"monokulo push-relay handle seal v1";

/// AES-GCM's nonce, stored before the ciphertext.
const NONCE_LEN: usize = 12;

/// Expiries are kept to the hour, so rows registered in the same hour
/// can't be told apart by theirs.
const EXPIRY_STEP: i64 = 60 * 60;

/// A handle's id: `ph_` and 32 hex digits, the last part of its endpoint:
/// the first 128 bits of a hash of the platform, app, token and sender
/// key it was registered for ([`HandleId::of`]). Unguessable without the
/// token, which nobody but the device, APNs or FCM and the relay sees; it
/// is still useless without the sender's signature. The file keeps only
/// its hash ([`HandleId::row_key`]), and it is the key to the row's
/// sealed token and sender, so it is never logged: [`HandleId::tag`] is.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct HandleId(String);

/// Its tag, as a log line would have it: never the id.
impl fmt::Debug for HandleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HandleId({})", self.tag())
    }
}

impl HandleId {
    const PREFIX: &'static str = "ph_";

    /// The id of the handle `new` asks for: the same four, the same id, so
    /// registering again finds the row.
    fn of(new: &NewHandle) -> Self {
        let mut hash = Sha256::new();
        for part in [
            ID_CONTEXT,
            new.platform.as_str().as_bytes(),
            new.app.as_str().as_bytes(),
            new.token.as_str().as_bytes(),
            new.sender.to_base64url().as_bytes(),
        ] {
            // None of them holds a NUL, so the parts can't run together.
            hash.update(part);
            hash.update([0]);
        }
        HandleId(format!(
            "{}{}",
            Self::PREFIX,
            hex::encode(&hash.finalize()[..16])
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The row's primary key: SHA-256 of the id.
    fn row_key(&self) -> [u8; 32] {
        Sha256::digest(self.0.as_bytes()).into()
    }

    /// How log lines name the handle: the first 12 hex digits of its row
    /// key, which find the row (`hex(key) LIKE '…%'`) but don't open it.
    pub fn tag(&self) -> HandleTag {
        HandleTag(hex::encode(&self.row_key()[..6]))
    }

    /// The key `token` and `sender` are sealed under in this handle's row.
    fn seal_key(&self) -> Zeroizing<[u8; 32]> {
        let mut key = Zeroizing::new([0u8; 32]);
        Hkdf::<Sha256>::new(None, self.0.as_bytes())
            .expand(SEAL_CONTEXT, key.as_mut())
            .unwrap_or_else(|_| unreachable!("32 bytes is a valid HKDF-SHA-256 output length"));
        key
    }
}

/// A handle as log lines name it: `h_` and 12 hex digits of its row key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleTag(String);

impl fmt::Display for HandleTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "h_{}", self.0)
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
/// and `:`). Bounded by what the platforms issue, with room to grow
/// ([`DeviceToken::max_len`]), so a made-up one makes no large row, and of
/// characters that go into a URL path as they are. Whoever holds it can
/// address the device through APNs or FCM with the app's credentials, so
/// `Debug` doesn't print it and no log line carries it.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceToken(String);

impl DeviceToken {
    /// The longest token taken for `platform`. APNs device tokens are 32
    /// bytes (64 hex digits) and activity tokens longer, so 128 bytes; FCM
    /// registration tokens are about 160 characters, so 512.
    pub const fn max_len(platform: Platform) -> usize {
        match platform {
            Platform::Apns | Platform::ApnsLiveActivity | Platform::ApnsPushToStart => 256,
            Platform::Fcm => 512,
        }
    }

    pub fn parse(platform: Platform, text: &str) -> Result<Self, String> {
        let max = Self::max_len(platform);
        if text.is_empty() || text.len() > max {
            return Err(format!("a {platform} token is 1 to {max} characters"));
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

/// How long a handle lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lifetimes {
    /// From its registration, while no push has reached it
    /// (`handles.unused_ttl_days`).
    pub unused: Duration,
    /// From its last registration or delivered push, once one has
    /// (`handles.ttl_days`).
    pub delivered: Duration,
}

/// What a row seals: the token and the sender's key.
#[derive(Serialize, Deserialize)]
struct Sealed {
    token: String,
    sender: String,
}

/// `new`'s token and sender, sealed under `id`'s key, the row's platform
/// and app bound to them: the nonce, then the ciphertext.
fn seal(id: &HandleId, new: &NewHandle) -> Vec<u8> {
    let plaintext = Zeroizing::new(
        serde_json::to_vec(&Sealed {
            token: new.token.as_str().to_owned(),
            sender: new.sender.to_base64url(),
        })
        .unwrap_or_else(|_| unreachable!("two strings serialize")),
    );
    let mut nonce = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce);
    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(*id.seal_key()));
    let aad = bound(new.platform, &new.app);
    // AES-GCM refuses only a plaintext of more than 64 GiB.
    let Ok(ciphertext) = cipher.encrypt(
        &Nonce::<Aes256Gcm>::from(nonce),
        Payload {
            msg: plaintext.as_slice(),
            aad: aad.as_bytes(),
        },
    ) else {
        unreachable!("AES-256-GCM refused {} bytes", plaintext.len())
    };
    let mut sealed = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    sealed
}

/// The token and sender `sealed` holds for `id`, or `None` if it wasn't
/// sealed for this id, platform and app.
fn open(
    id: &HandleId,
    platform: Platform,
    app: &AppName,
    sealed: &[u8],
) -> Option<(DeviceToken, VapidPublicKey)> {
    let (nonce, ciphertext) = sealed.split_at_checked(NONCE_LEN)?;
    let nonce: [u8; NONCE_LEN] = nonce.try_into().ok()?;
    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(*id.seal_key()));
    let aad = bound(platform, app);
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                &Nonce::<Aes256Gcm>::from(nonce),
                Payload {
                    msg: ciphertext,
                    aad: aad.as_bytes(),
                },
            )
            .ok()?,
    );
    let sealed: Sealed = serde_json::from_slice(&plaintext).ok()?;
    Some((
        DeviceToken::parse(platform, &sealed.token).ok()?,
        VapidPublicKey::from_base64url(&sealed.sender).ok()?,
    ))
}

/// What a row's seal is bound to besides its key: its platform and app, so
/// neither can be changed in the file without the row failing to open.
fn bound(platform: Platform, app: &AppName) -> String {
    format!("{platform}\0{app}")
}

/// The handles, on one connection: every push reads its handle and,
/// once APNs or FCM took it, writes its new expiry, so a separate reader
/// would buy nothing.
#[derive(Clone)]
pub struct HandleStore {
    pool: sqlite::Pool<Connection>,
    lifetimes: Lifetimes,
}

impl HandleStore {
    /// Opens the database at `path`, making it and its directory if they
    /// aren't there, and brings its schema up to date.
    pub fn open(path: &Path, lifetimes: Lifetimes) -> Result<Self, StoreError> {
        let open_error = |error: String| StoreError::Open {
            path: path.display().to_string(),
            error,
        };
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|error| open_error(error.to_string()))?;
        }
        let connection = Connection::open(path).map_err(|error| open_error(error.to_string()))?;
        sqlite::configure_writer(&connection)?;
        // A deleted row's bytes are overwritten, not left in a free page:
        // the first shape's plaintext tokens, dropped by migration 2,
        // above all.
        connection.pragma_update(None, "secure_delete", "ON")?;
        sqlite::migrations::apply(&connection, MIGRATIONS)?;
        let pool = sqlite::Pool::start("push-relay-db", vec![connection])
            .map_err(|error| open_error(error.to_string()))?;
        Ok(HandleStore { pool, lifetimes })
    }

    /// The handle for `new`, made if there is none. A new one lives the
    /// unused lifetime from `now`; one a push has reached, the delivered
    /// lifetime. Never shortened.
    pub async fn register(&self, new: NewHandle, now: i64) -> Result<Handle, StoreError> {
        let id = HandleId::of(&new);
        let sealed = seal(&id, &new);
        let unused = expiry(now, self.lifetimes.unused);
        let delivered = expiry(now, self.lifetimes.delivered);
        let key = id.row_key();
        self.pool
            .run(move |connection| {
                // An expired handle not yet swept is taken back too: the
                // device and sender are the same, so is the endpoint.
                let expires_at: i64 = connection.query_row(
                    "INSERT INTO handles (key, platform, app, sealed, delivered, expires_at)
                     VALUES (?1, ?2, ?3, ?4, 0, ?5)
                     ON CONFLICT (key) DO UPDATE SET expires_at = MAX(
                         handles.expires_at,
                         CASE WHEN handles.delivered = 1 THEN ?6 ELSE ?5 END)
                     RETURNING expires_at",
                    params![
                        key.as_slice(),
                        new.platform.as_str(),
                        new.app.as_str(),
                        sealed,
                        unused,
                        delivered,
                    ],
                    |row| row.get(0),
                )?;
                Ok(Handle {
                    id,
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
                        "SELECT platform, app, sealed, expires_at
                         FROM handles WHERE key = ?1 AND expires_at > ?2",
                        params![id.row_key().as_slice(), now],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, Vec<u8>>(2)?,
                                row.get::<_, i64>(3)?,
                            ))
                        },
                    )
                    .optional()?;
                row.map(|(platform, app, sealed, expires_at)| {
                    let corrupt =
                        |what: &str| StoreError::Corrupt(format!("{}: its {what}", id.tag()));
                    let platform = Platform::from_stored(&platform).ok_or(corrupt("platform"))?;
                    let app = AppName::try_from(app).map_err(|_| corrupt("app"))?;
                    let (token, sender) = open(&id, platform, &app, &sealed)
                        .ok_or(corrupt("sealed token and key"))?;
                    Ok(Handle {
                        id: id.clone(),
                        platform,
                        app,
                        token,
                        sender,
                        expires_at,
                    })
                })
                .transpose()
            })
            .await
    }

    /// Marks `id` as reached by a push at `now`: it lives the delivered
    /// lifetime from then.
    pub async fn touch(&self, id: &HandleId, now: i64) -> Result<(), StoreError> {
        let key = id.row_key();
        let expires_at = expiry(now, self.lifetimes.delivered);
        self.pool
            .run(move |connection| {
                connection.execute(
                    "UPDATE handles SET delivered = 1, expires_at = ?2 WHERE key = ?1",
                    params![key.as_slice(), expires_at],
                )?;
                Ok(())
            })
            .await
    }

    /// Forgets `id`: APNs or FCM said its token is gone.
    pub async fn delete(&self, id: &HandleId) -> Result<(), StoreError> {
        let key = id.row_key();
        self.pool
            .run(move |connection| {
                connection.execute(
                    "DELETE FROM handles WHERE key = ?1",
                    params![key.as_slice()],
                )?;
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

/// `ttl` from `now`, rounded up to the hour ([`EXPIRY_STEP`]).
fn expiry(now: i64, ttl: Duration) -> i64 {
    let at = now.saturating_add(i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX));
    match at.rem_euclid(EXPIRY_STEP) {
        0 => at,
        past => at.saturating_add(EXPIRY_STEP - past),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use push::VapidKey;

    /// On the hour, as every expiry is kept.
    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;
    const LIFETIMES: Lifetimes = Lifetimes {
        unused: Duration::from_secs(7 * 86_400),
        delivered: Duration::from_secs(90 * 86_400),
    };

    pub(crate) fn temp_path(tag: &str) -> std::path::PathBuf {
        let mut unique = [0u8; 8];
        rand::rng().fill_bytes(&mut unique);
        std::env::temp_dir()
            .join(format!(
                "push-relay-{tag}-{}-{}",
                std::process::id(),
                hex::encode(unique)
            ))
            .join("relay.db")
    }

    fn open_store(path: &Path) -> HandleStore {
        HandleStore::open(path, LIFETIMES).unwrap()
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
        open_store(&path);
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
        open_store(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The same device, app and sender registered again keep their handle
    /// and push its expiry back; a second sender gets a handle of its own.
    #[tokio::test]
    async fn registering_again_keeps_the_handle_and_refreshes_its_expiry() {
        let path = temp_path("again");
        let store = open_store(&path);
        let (monokulo, bell) = (VapidKey::generate(), VapidKey::generate());
        let token = "a1b2".repeat(16);
        let first = store
            .register(new(Platform::Apns, &token, &monokulo), NOW)
            .await
            .unwrap();
        assert!(
            first.id.as_str().starts_with("ph_"),
            "{}",
            first.id.as_str()
        );
        assert_eq!(first.id.as_str().parse::<HandleId>(), Ok(first.id.clone()));
        assert_eq!(first.expires_at, NOW + 7 * DAY);

        // Pushed back to the hour.
        let again = store
            .register(new(Platform::Apns, &token, &monokulo), NOW + 100)
            .await
            .unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(again.expires_at, NOW + 7 * DAY + 3_600);

        let other = store
            .register(new(Platform::Apns, &token, &bell), NOW)
            .await
            .unwrap();
        assert_ne!(other.id, first.id);
        assert_eq!(
            store.find(&other.id, NOW).await.unwrap().unwrap().sender,
            bell.public()
        );
        // Another platform for the same token is another handle too.
        let activity = store
            .register(new(Platform::ApnsLiveActivity, &token, &monokulo), NOW)
            .await
            .unwrap();
        assert_ne!(activity.id, first.id);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The file can't link one device's handles: two senders' rows for
    /// one token share no value but the platform and app (every such
    /// device's), and neither the token nor a sender's key is anywhere in
    /// it. The relay still finds both, by their endpoints.
    #[tokio::test]
    async fn two_handles_of_one_device_share_nothing_in_the_file() {
        let path = temp_path("unlinkable");
        let store = open_store(&path);
        let (monokulo, bell) = (VapidKey::generate(), VapidKey::generate());
        let token = "5e".repeat(32);
        let one = store
            .register(new(Platform::Apns, &token, &monokulo), NOW)
            .await
            .unwrap();
        // Later in the same minute, as an app registers both at launch.
        let two = store
            .register(new(Platform::Apns, &token, &bell), NOW + 40)
            .await
            .unwrap();
        // The second one's first push, a day later.
        store.touch(&two.id, NOW + DAY).await.unwrap();
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let columns: Vec<String> = connection
            .prepare("SELECT name FROM pragma_table_info('handles')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let rows: Vec<Vec<rusqlite::types::Value>> = connection
            .prepare("SELECT * FROM handles")
            .unwrap()
            .query_map([], |row| {
                (0..columns.len()).map(|column| row.get(column)).collect()
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows.len(), 2);
        for (column, name) in columns.iter().enumerate() {
            let shared = rows[0][column] == rows[1][column];
            let everyones = matches!(name.as_str(), "platform" | "app");
            assert_eq!(shared, everyones, "{name}: {:?}", rows[0][column]);
        }
        drop(connection);
        // Nor anywhere in the file's bytes, in any spelling.
        let mut file = std::fs::read(&path).unwrap();
        if let Ok(wal) = std::fs::read(path.with_extension("db-wal")) {
            file.extend(wal);
        }
        let token_bytes = hex::decode(&token).unwrap();
        for needle in [
            token.as_bytes(),
            token_bytes.as_slice(),
            monokulo.public().to_base64url().as_bytes(),
            bell.public().to_base64url().as_bytes(),
            one.id.as_str().as_bytes(),
            two.id.as_str().as_bytes(),
        ] {
            assert!(
                !file.windows(needle.len()).any(|window| window == needle),
                "{} is in the file",
                String::from_utf8_lossy(needle)
            );
        }

        let store = open_store(&path);
        assert_eq!(store.find(&one.id, NOW).await.unwrap(), Some(one.clone()));
        let found = store.find(&two.id, NOW).await.unwrap().unwrap();
        assert_eq!((found.token, found.sender), (two.token, bell.public()));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A row whose platform or app is changed in the file no longer opens:
    /// the seal is bound to them.
    #[tokio::test]
    async fn a_row_changed_in_the_file_does_not_open() {
        let path = temp_path("tampered");
        let store = open_store(&path);
        let sender = VapidKey::generate();
        let handle = store
            .register(new(Platform::Apns, &"7a".repeat(32), &sender), NOW)
            .await
            .unwrap();
        Connection::open(&path)
            .unwrap()
            .execute("UPDATE handles SET platform = 'apns-liveactivity'", [])
            .unwrap();
        let error = store.find(&handle.id, NOW).await.unwrap_err().to_string();
        assert!(error.contains("sealed"), "{error}");
        assert!(!error.contains(handle.id.as_str()), "{error}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A handle no push has reached lives the unused lifetime from its
    /// registration, then is unknown and swept; one a push has reached
    /// lives the delivered lifetime from the push, and registering it
    /// again keeps the long one.
    #[tokio::test]
    async fn a_handle_no_push_reached_goes_after_the_unused_lifetime() {
        let path = temp_path("unused");
        let store = open_store(&path);
        let sender = VapidKey::generate();
        let unused = store
            .register(new(Platform::Fcm, "fcm:never_pushed", &sender), NOW)
            .await
            .unwrap();
        let used = store
            .register(new(Platform::Fcm, "fcm:pushed", &sender), NOW)
            .await
            .unwrap();
        assert_eq!(unused.expires_at, NOW + 7 * DAY);
        store.touch(&used.id, NOW + DAY).await.unwrap();

        assert!(store
            .find(&unused.id, NOW + 7 * DAY - 1)
            .await
            .unwrap()
            .is_some());
        assert_eq!(store.find(&unused.id, NOW + 7 * DAY).await.unwrap(), None);
        assert_eq!(store.sweep(NOW + 7 * DAY).await.unwrap(), 1);
        let found = store.find(&used.id, NOW + 7 * DAY).await.unwrap().unwrap();
        assert_eq!(found.expires_at, NOW + DAY + 90 * DAY);

        // Registered again, the delivered one keeps its lifetime from now.
        let again = store
            .register(new(Platform::Fcm, "fcm:pushed", &sender), NOW + 2 * DAY)
            .await
            .unwrap();
        assert_eq!(again.expires_at, NOW + 2 * DAY + 90 * DAY);
        assert_eq!(store.sweep(NOW + 90 * DAY).await.unwrap(), 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A handle lives for its lifetime after its last use, then is unknown;
    /// a use pushes it back, and the sweep forgets only expired ones.
    #[tokio::test]
    async fn a_handle_expires_by_its_last_use() {
        let path = temp_path("expiry");
        let store = open_store(&path);
        let sender = VapidKey::generate();
        let handle = store
            .register(new(Platform::Fcm, "fcm:token_1-x", &sender), NOW)
            .await
            .unwrap();
        store.touch(&handle.id, NOW).await.unwrap();
        let ttl = 90 * DAY;
        let found = store
            .find(&handle.id, NOW + ttl - 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            found,
            Handle {
                expires_at: NOW + ttl,
                ..handle.clone()
            }
        );
        assert_eq!(store.find(&handle.id, NOW + ttl).await.unwrap(), None);

        store.touch(&handle.id, NOW + 1_000).await.unwrap();
        // To the hour.
        let until = NOW + 3_600 + ttl;
        assert!(store.find(&handle.id, until - 1).await.unwrap().is_some());
        assert_eq!(store.sweep(until - 1).await.unwrap(), 0);
        assert_eq!(store.sweep(until).await.unwrap(), 1);
        assert_eq!(store.find(&handle.id, NOW).await.unwrap(), None);

        // Registered again after the sweep: the same endpoint, new again.
        let back = store
            .register(new(Platform::Fcm, "fcm:token_1-x", &sender), NOW)
            .await
            .unwrap();
        assert_eq!(back.id, handle.id);
        assert_eq!(back.expires_at, NOW + 7 * DAY);
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
        let store = open_store(&path);
        let sender = VapidKey::generate();
        let token = "c3".repeat(32);
        let handle = store
            .register(new(Platform::Apns, &token, &sender), NOW)
            .await
            .unwrap();
        let later = NOW + 7 * DAY + 3_600;
        assert_eq!(store.find(&handle.id, later).await.unwrap(), None);
        let back = store
            .register(new(Platform::Apns, &token, &sender), later)
            .await
            .unwrap();
        assert_eq!(back.id, handle.id);
        assert_eq!(back.expires_at, later + 7 * DAY);
        assert!(store.find(&handle.id, later).await.unwrap().is_some());
        assert_eq!(store.sweep(later).await.unwrap(), 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The first shape's file, plaintext tokens and all, is replaced:
    /// its rows go, and nothing of them is left in the file.
    #[tokio::test]
    async fn the_first_shapes_rows_are_dropped_and_overwritten() {
        let path = temp_path("first-shape");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let token = "9d".repeat(32);
        {
            let connection = Connection::open(&path).unwrap();
            sqlite::migrations::apply(&connection, &MIGRATIONS[..1]).unwrap();
            for n in 0..200 {
                connection
                    .execute(
                        "INSERT INTO handles VALUES (?1, 'apns', 'monokular', ?2, 'key', 0, 0, 1)",
                        params![format!("ph_{n:032x}"), format!("{token}{n:04}")],
                    )
                    .unwrap();
            }
        }
        let store = open_store(&path);
        drop(store);
        let connection = Connection::open(&path).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM handles", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        drop(connection);
        let file = std::fs::read(&path).unwrap();
        assert!(
            !file
                .windows(token.len())
                .any(|window| window == token.as_bytes()),
            "a dropped token is still in the file"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn handle_ids_are_ph_and_32_hex_digits_derived_from_the_registration() {
        let sender = VapidKey::generate();
        let id = HandleId::of(&new(Platform::Apns, &"ab".repeat(32), &sender));
        assert_eq!(id.as_str().len(), 35);
        assert_eq!(id.as_str().parse::<HandleId>(), Ok(id.clone()));
        // Either spelling of the token's hex is the one handle.
        assert_eq!(
            HandleId::of(&new(Platform::Apns, &"AB".repeat(32), &sender)),
            id
        );
        let tag = id.tag().to_string();
        assert!(tag.starts_with("h_") && tag.len() == 14, "{tag}");
        assert!(!id.as_str().contains(&tag[2..]), "{tag}");
        assert_eq!(format!("{id:?}"), format!("HandleId({tag})"));
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
    fn expiries_are_kept_to_the_hour() {
        let hour = Duration::from_secs(3_600);
        assert_eq!(expiry(NOW, hour), NOW + 3_600);
        assert_eq!(expiry(NOW + 1, hour), NOW + 7_200);
        assert_eq!(expiry(NOW + 3_599, Duration::ZERO), NOW + 3_600);
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
        assert!(DeviceToken::parse(Platform::Apns, &"ab".repeat(128)).is_ok());
        for bad in ["", "abc", "zz", "ab/cd", &"ab".repeat(129)] {
            assert!(DeviceToken::parse(Platform::Apns, bad).is_err(), "{bad}");
        }
        assert!(DeviceToken::parse(Platform::Fcm, &"f".repeat(512)).is_ok());
        assert!(DeviceToken::parse(Platform::Fcm, &"f".repeat(513)).is_err());
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
