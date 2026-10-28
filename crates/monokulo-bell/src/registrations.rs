//! Registrations: which tills a payment at a Square location dings. A till
//! registers its Web Push subscription for one location, once the
//! merchant's token has shown Square that the location is theirs; the
//! registration keeps the merchant and location Square named, the
//! subscription, and how its dings have gone. It lives in the Bell's own
//! SQLite file (`bell.store_path`), and nothing else does.
//!
//! One subscription is one registration: registered again (a till moved
//! to another location, or its app started again), its row is updated,
//! keeping its id. An endpoint alone proves nothing (a push service, a
//! distributor's log or a shared server can show it), so only its own
//! merchant, or a caller holding its keys, may change its registration.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use push::{AuthSecret, Endpoint, P256dh, Subscription, SubscriptionKeys};
use rand::Rng as _;
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};
use subtle::ConstantTimeEq as _;

use crate::square::{LocationId, MerchantId};

/// The registrations table and its index.
const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../migrations/0001_registrations.sql"))];

/// The most registrations one location may have: a shop has a till or a
/// few, and each is a push per payment. A bound, so one merchant's token
/// can't fill the database.
pub const MAX_PER_LOCATION: u32 = 32;

/// A registration's id: `br_` and 32 random hex digits, what the till
/// unregisters with. Unguessable, so it is all the unregistering takes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RegistrationId(String);

impl RegistrationId {
    const PREFIX: &'static str = "br_";

    fn generate() -> Self {
        let mut bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        RegistrationId(format!("{}{}", Self::PREFIX, hex::encode(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RegistrationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Not a registration id of the Bell's making.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a registration id (br_ and 32 lowercase hex digits)")]
pub struct RegistrationIdError;

impl FromStr for RegistrationId {
    type Err = RegistrationIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let digits = text.strip_prefix(Self::PREFIX).ok_or(RegistrationIdError)?;
        if digits.len() == 32
            && digits
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(RegistrationId(text.to_owned()))
        } else {
            Err(RegistrationIdError)
        }
    }
}

/// What a registration asks for: Square has shown the location is the
/// merchant's.
#[derive(Debug, Clone)]
pub struct NewRegistration {
    pub merchant_id: MerchantId,
    pub location_id: LocationId,
    pub subscription: Subscription,
}

/// A registration, as a ding is sent to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub id: RegistrationId,
    pub subscription: Subscription,
}

/// What became of a ding its push service didn't take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failed {
    /// Counted: this many in a row now.
    Counted(u32),
    /// The limit was reached, and the registration deleted.
    Deleted,
    /// It had gone already (a ding of another webhook alongside deleted
    /// it, or the till unregistered).
    Gone,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("the registrations database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("the registrations database: {0}")]
    Pool(#[from] sqlite::PoolError),
    #[error("the registrations database can't be opened at {path}: {error}")]
    Open { path: String, error: String },
    #[error("a stored registration doesn't read: {0}")]
    Corrupt(String),
}

/// Why a registration wasn't made.
#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error("the location has {MAX_PER_LOCATION} registrations already")]
    Full,
    /// The endpoint is registered for another merchant, and the keys given
    /// aren't the ones it was registered with.
    #[error("the endpoint is another merchant's registration")]
    Taken,
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<sqlite::PoolError> for RegisterError {
    fn from(error: sqlite::PoolError) -> Self {
        RegisterError::Store(StoreError::Pool(error))
    }
}

/// The registrations, on one connection: each webhook reads a location's
/// and writes how its dings went, so a separate reader would buy nothing.
#[derive(Clone)]
pub struct RegistrationStore {
    pool: sqlite::Pool<Connection>,
}

impl RegistrationStore {
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
        let pool = sqlite::Pool::start("bell-db", vec![connection])
            .map_err(|error| open_error(error.to_string()))?;
        Ok(RegistrationStore { pool })
    }

    /// Registers `new`'s subscription for its location at `now`: a new
    /// registration, or its existing one moved there with its keys
    /// replaced and its failures forgotten. An existing one moves only for
    /// its own merchant, or for a caller giving the auth secret it was
    /// registered with (a till signed in to another account): otherwise
    /// [`RegisterError::Taken`], and it is left as it was. Refused when
    /// the location has [`MAX_PER_LOCATION`] others.
    pub async fn register(
        &self,
        new: NewRegistration,
        now: i64,
    ) -> Result<RegistrationId, RegisterError> {
        let id = RegistrationId::generate();
        self.pool
            .run(move |connection| {
                // Immediate: the count and the insert see the same rows.
                let transaction = rusqlite::Transaction::new_unchecked(
                    connection,
                    TransactionBehavior::Immediate,
                )
                .map_err(StoreError::from)?;
                let endpoint = new.subscription.endpoint.as_str();
                let auth = new.subscription.keys.auth.to_base64url();
                let existing: Option<(String, String)> = transaction
                    .query_row(
                        "SELECT merchant_id, auth FROM registrations WHERE endpoint = ?1",
                        params![endpoint],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(StoreError::from)?;
                if let Some((merchant_id, stored_auth)) = existing {
                    // Compared in constant time: the secret is the proof.
                    let holds_keys: bool = stored_auth.as_bytes().ct_eq(auth.as_bytes()).into();
                    if merchant_id != new.merchant_id.as_str() && !holds_keys {
                        return Err(RegisterError::Taken);
                    }
                }
                let others: u32 = transaction
                    .query_row(
                        "SELECT COUNT(*) FROM registrations
                         WHERE merchant_id = ?1 AND location_id = ?2 AND endpoint != ?3",
                        params![new.merchant_id.as_str(), new.location_id.as_str(), endpoint],
                        |row| row.get(0),
                    )
                    .map_err(StoreError::from)?;
                if others >= MAX_PER_LOCATION {
                    return Err(RegisterError::Full);
                }
                let id: String = transaction
                    .query_row(
                        "INSERT INTO registrations
                             (id, merchant_id, location_id, endpoint, p256dh, auth, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                         ON CONFLICT (endpoint)
                         DO UPDATE SET merchant_id = excluded.merchant_id,
                                       location_id = excluded.location_id,
                                       p256dh = excluded.p256dh,
                                       auth = excluded.auth,
                                       failures = 0
                         RETURNING id",
                        params![
                            id.as_str(),
                            new.merchant_id.as_str(),
                            new.location_id.as_str(),
                            endpoint,
                            new.subscription.keys.p256dh.to_base64url(),
                            auth,
                            now,
                        ],
                        |row| row.get(0),
                    )
                    .map_err(StoreError::from)?;
                transaction.commit().map_err(StoreError::from)?;
                id.parse()
                    .map_err(|_| RegisterError::Store(StoreError::Corrupt(id)))
            })
            .await
    }

    /// Deletes `id`, and says whether there was one.
    pub async fn delete(&self, id: &RegistrationId) -> Result<bool, StoreError> {
        let id = id.clone();
        self.pool
            .run(move |connection| {
                let deleted = connection.execute(
                    "DELETE FROM registrations WHERE id = ?1",
                    params![id.as_str()],
                )?;
                Ok(deleted > 0)
            })
            .await
    }

    /// Every registration of `merchant_id`'s location `location_id`.
    pub async fn at_location(
        &self,
        merchant_id: &MerchantId,
        location_id: &LocationId,
    ) -> Result<Vec<Registration>, StoreError> {
        let (merchant_id, location_id) = (merchant_id.clone(), location_id.clone());
        self.pool
            .run(move |connection| {
                let mut statement = connection.prepare_cached(
                    "SELECT id, endpoint, p256dh, auth FROM registrations
                     WHERE merchant_id = ?1 AND location_id = ?2 ORDER BY created_at, id",
                )?;
                let rows = statement.query_map(
                    params![merchant_id.as_str(), location_id.as_str()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )?;
                rows.map(|row| {
                    let (id, endpoint, p256dh, auth) = row?;
                    read_registration(id, &endpoint, &p256dh, &auth)
                })
                .collect()
            })
            .await
    }

    /// `id`'s push service took a ding at `now`: its failures start over.
    pub async fn delivered(&self, id: &RegistrationId, now: i64) -> Result<(), StoreError> {
        let id = id.clone();
        self.pool
            .run(move |connection| {
                connection.execute(
                    "UPDATE registrations SET last_ok_at = ?2, failures = 0 WHERE id = ?1",
                    params![id.as_str(), now],
                )?;
                Ok(())
            })
            .await
    }

    /// `id`'s push service didn't take a ding: one more failure, and at
    /// `limit` in a row the registration is deleted.
    pub async fn failed(&self, id: &RegistrationId, limit: u32) -> Result<Failed, StoreError> {
        let id = id.clone();
        self.pool
            .run(move |connection| {
                let failures: Option<u32> = connection
                    .query_row(
                        "UPDATE registrations SET failures = failures + 1 WHERE id = ?1
                         RETURNING failures",
                        params![id.as_str()],
                        |row| row.get(0),
                    )
                    .optional()?;
                match failures {
                    None => Ok(Failed::Gone),
                    Some(failures) if failures < limit => Ok(Failed::Counted(failures)),
                    Some(_) => {
                        connection.execute(
                            "DELETE FROM registrations WHERE id = ?1",
                            params![id.as_str()],
                        )?;
                        Ok(Failed::Deleted)
                    }
                }
            })
            .await
    }
}

/// A stored row, read back into the types it was written from.
fn read_registration(
    id: String,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
) -> Result<Registration, StoreError> {
    let corrupt = |what: &str| StoreError::Corrupt(format!("{id}: its {what}"));
    let subscription = Subscription {
        endpoint: Endpoint::parse(endpoint).map_err(|_| corrupt("endpoint"))?,
        keys: SubscriptionKeys {
            p256dh: P256dh::from_base64url(p256dh).map_err(|_| corrupt("p256dh"))?,
            auth: AuthSecret::from_base64url(auth).map_err(|_| corrupt("auth"))?,
        },
    };
    Ok(Registration {
        id: id.parse().map_err(|_| corrupt("id"))?,
        subscription,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use push::encrypt::ReceiverKeys;

    const NOW: i64 = 1_800_000_000;

    pub(crate) fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!(
                "monokular-bell-{tag}-{}-{}",
                std::process::id(),
                RegistrationId::generate()
            ))
            .join("bell.db")
    }

    fn merchant(id: &str) -> MerchantId {
        MerchantId::try_from(id.to_owned()).unwrap()
    }

    fn location(id: &str) -> LocationId {
        LocationId::try_from(id.to_owned()).unwrap()
    }

    fn subscription(endpoint: &str) -> Subscription {
        Subscription {
            endpoint: Endpoint::parse(endpoint).unwrap(),
            keys: ReceiverKeys::generate().subscription_keys(),
        }
    }

    fn new(merchant_id: &str, location_id: &str, endpoint: &str) -> NewRegistration {
        NewRegistration {
            merchant_id: merchant(merchant_id),
            location_id: location(location_id),
            subscription: subscription(endpoint),
        }
    }

    #[tokio::test]
    async fn the_database_is_made_at_its_path_with_only_the_registrations_table() {
        let path = temp_path("tables");
        RegistrationStore::open(&path).unwrap();
        let connection = Connection::open(&path).unwrap();
        let mut tables: Vec<String> = connection
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        tables.retain(|table| !table.ends_with("_migrations"));
        assert_eq!(tables, ["registrations"]);
        // Opened again, nothing runs twice.
        RegistrationStore::open(&path).unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A subscription registered again keeps its id and moves to the
    /// location it was registered for last, with its new keys.
    #[tokio::test]
    async fn registering_a_subscription_again_moves_it_and_keeps_its_id() {
        let path = temp_path("again");
        let store = RegistrationStore::open(&path).unwrap();
        let till = "https://push.example.org/till-1";
        let first = store.register(new("M1", "L1", till), NOW).await.unwrap();
        assert!(first.as_str().starts_with("br_"), "{first}");
        let other = store
            .register(new("M1", "L1", "https://push.example.org/till-2"), NOW)
            .await
            .unwrap();
        assert_ne!(other, first);

        store.failed(&first, 20).await.unwrap();
        let moved = new("M1", "L2", till);
        let keys = moved.subscription.keys.clone();
        let again = store.register(moved, NOW + 1).await.unwrap();
        assert_eq!(again, first);
        let at_l1 = store
            .at_location(&merchant("M1"), &location("L1"))
            .await
            .unwrap();
        assert_eq!(at_l1.iter().map(|r| &r.id).collect::<Vec<_>>(), [&other]);
        let at_l2 = store
            .at_location(&merchant("M1"), &location("L2"))
            .await
            .unwrap();
        assert_eq!(at_l2.len(), 1);
        assert_eq!(at_l2[0].subscription.keys, keys);
        // Its failures were forgotten: a limit of one deletes at once.
        assert_eq!(store.failed(&first, 2).await.unwrap(), Failed::Counted(1));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Another merchant can't take a registration by its endpoint: refused,
    /// it keeps its merchant, location, keys and failures. Given the keys
    /// it was registered with (the till signed in to another account), it
    /// moves.
    #[tokio::test]
    async fn another_merchant_moves_a_registration_only_with_its_keys() {
        let path = temp_path("taken");
        let store = RegistrationStore::open(&path).unwrap();
        let till = "https://push.example.org/till-1";
        let first = new("M1", "L1", till);
        let keys = first.subscription.keys.clone();
        let id = store.register(first, NOW).await.unwrap();
        store.failed(&id, 20).await.unwrap();

        let taken = store.register(new("M2", "L9", till), NOW + 1).await;
        assert!(matches!(taken, Err(RegisterError::Taken)), "{taken:?}");
        let at_l1 = store
            .at_location(&merchant("M1"), &location("L1"))
            .await
            .unwrap();
        assert_eq!(at_l1.len(), 1);
        assert_eq!((&at_l1[0].id, &at_l1[0].subscription.keys), (&id, &keys));
        assert_eq!(store.failed(&id, 20).await.unwrap(), Failed::Counted(2));

        let mut signed_in_elsewhere = new("M2", "L9", till);
        signed_in_elsewhere.subscription.keys = keys;
        assert_eq!(
            store.register(signed_in_elsewhere, NOW + 2).await.unwrap(),
            id
        );
        assert!(
            store
                .at_location(&merchant("M1"), &location("L1"))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .at_location(&merchant("M2"), &location("L9"))
                .await
                .unwrap()
                .len(),
            1
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The same location of another merchant is another location.
    #[tokio::test]
    async fn a_location_is_its_merchants() {
        let path = temp_path("merchant");
        let store = RegistrationStore::open(&path).unwrap();
        store
            .register(new("M1", "L1", "https://push.example.org/a"), NOW)
            .await
            .unwrap();
        assert!(
            store
                .at_location(&merchant("M2"), &location("L1"))
                .await
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn a_location_takes_a_bounded_number_of_registrations() {
        let path = temp_path("full");
        let store = RegistrationStore::open(&path).unwrap();
        for n in 0..MAX_PER_LOCATION {
            store
                .register(
                    new("M1", "L1", &format!("https://push.example.org/{n}")),
                    NOW,
                )
                .await
                .unwrap();
        }
        let refused = store
            .register(new("M1", "L1", "https://push.example.org/one-more"), NOW)
            .await;
        assert!(matches!(refused, Err(RegisterError::Full)), "{refused:?}");
        // One already there may register again, and another location has
        // room of its own.
        store
            .register(new("M1", "L1", "https://push.example.org/0"), NOW)
            .await
            .unwrap();
        store
            .register(new("M1", "L2", "https://push.example.org/one-more"), NOW)
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A till moving to a full location is refused, and stays where it
    /// was, with the keys and the failures it had: the refused move
    /// changes nothing.
    #[tokio::test]
    async fn a_move_to_a_full_location_leaves_the_registration_where_it_was() {
        let path = temp_path("full-move");
        let store = RegistrationStore::open(&path).unwrap();
        for n in 0..MAX_PER_LOCATION {
            store
                .register(
                    new("M1", "L1", &format!("https://push.example.org/{n}")),
                    NOW,
                )
                .await
                .unwrap();
        }
        let till = "https://push.example.org/moving";
        let staying = new("M1", "L2", till);
        let keys = staying.subscription.keys.clone();
        let id = store.register(staying, NOW).await.unwrap();
        store.failed(&id, 20).await.unwrap();
        let refused = store.register(new("M1", "L1", till), NOW + 1).await;
        assert!(matches!(refused, Err(RegisterError::Full)), "{refused:?}");
        let at_l2 = store
            .at_location(&merchant("M1"), &location("L2"))
            .await
            .unwrap();
        assert_eq!(at_l2.len(), 1);
        assert_eq!((&at_l2[0].id, &at_l2[0].subscription.keys), (&id, &keys));
        assert_eq!(store.failed(&id, 20).await.unwrap(), Failed::Counted(2));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A row that no longer reads (written by hand, or by a later Bell) is
    /// an error for the ding that reads it, never a ding to a wrong place.
    #[tokio::test]
    async fn a_stored_row_that_doesnt_read_is_corrupt() {
        let path = temp_path("corrupt");
        let store = RegistrationStore::open(&path).unwrap();
        store
            .register(new("M1", "L1", "https://push.example.org/a"), NOW)
            .await
            .unwrap();
        Connection::open(&path)
            .unwrap()
            .execute(
                "UPDATE registrations SET endpoint = 'http://push.example.org/a'",
                [],
            )
            .unwrap();
        let read = store.at_location(&merchant("M1"), &location("L1")).await;
        assert!(matches!(read, Err(StoreError::Corrupt(_))), "{read:?}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn failures_in_a_row_delete_at_the_limit_and_a_delivery_resets_them() {
        let path = temp_path("failures");
        let store = RegistrationStore::open(&path).unwrap();
        let id = store
            .register(new("M1", "L1", "https://push.example.org/a"), NOW)
            .await
            .unwrap();
        assert_eq!(store.failed(&id, 3).await.unwrap(), Failed::Counted(1));
        assert_eq!(store.failed(&id, 3).await.unwrap(), Failed::Counted(2));
        store.delivered(&id, NOW + 5).await.unwrap();
        assert_eq!(store.failed(&id, 3).await.unwrap(), Failed::Counted(1));
        assert_eq!(store.failed(&id, 3).await.unwrap(), Failed::Counted(2));
        assert_eq!(store.failed(&id, 3).await.unwrap(), Failed::Deleted);
        assert!(!store.delete(&id).await.unwrap());
        assert_eq!(store.failed(&id, 3).await.unwrap(), Failed::Gone);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ids_are_the_bells_own() {
        let id = RegistrationId::generate();
        assert_eq!(id.as_str().len(), 35);
        assert_eq!(id.as_str().parse(), Ok(id));
        for bad in [
            "br_",
            "ph_0123456789abcdef0123456789abcdef",
            "br_0123456789ABCDEF0123456789abcdef",
            "br_0123456789abcdef0123456789abcde",
        ] {
            assert!(bad.parse::<RegistrationId>().is_err(), "{bad}");
        }
    }
}
