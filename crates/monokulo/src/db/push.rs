//! Push targets and this instance's VAPID key (migration 0052,
//! `crate::push`): where connected apps want their store's sales pushed,
//! and the key the pushes are signed with.

use rusqlite::{params, OptionalExtension as _};

use super::{AppId, ConnectionId, Db, Result};
use crate::api::runtime::scalars::ClientId;
use crate::apps::SaleKind;
use crate::push::PushTargetKind;
pub use engine_protocol::ids::PushTargetId;

/// The most push targets one app may have: a till has one or two
/// subscriptions for each push service its device uses, and a key that
/// could register any number would be a way to fill the database.
pub const MAX_TARGETS_PER_APP: usize = 8;

/// How a target's pushes are sent, with what that needs: a kind's keys
/// can't be given to the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetKind {
    /// Encrypted to the device (RFC 8291) with its subscription's keys.
    Encrypted {
        p256dh: ::push::P256dh,
        auth: ::push::AuthSecret,
    },
    /// Plain JSON to the push relay, which hands it to an iOS Live
    /// Activity. `include_payment_uri`: whether the pushes may carry the
    /// payment URI, which the app shows in the Dynamic Island when its
    /// merchant chose to.
    LiveActivity { include_payment_uri: bool },
}

impl TargetKind {
    /// Its name.
    pub fn name(&self) -> PushTargetKind {
        match self {
            TargetKind::Encrypted { .. } => PushTargetKind::Encrypted,
            TargetKind::LiveActivity { .. } => PushTargetKind::LiveActivity,
        }
    }
}

/// Where an app wants its store's sale events pushed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushTarget {
    pub id: PushTargetId,
    pub store_id: ConnectionId,
    pub app_id: AppId,
    /// The app's own name for it.
    pub client_id: ClientId,
    pub kind: TargetKind,
    pub endpoint: ::push::Endpoint,
    /// The sale events it wants, in [`SaleKind::ALL`]'s order.
    pub events: Vec<SaleKind>,
    pub created_at: i64,
    /// When a push last reached its endpoint; `None` until one has.
    pub last_ok_at: Option<i64>,
    /// Failed attempts in a row since the last success.
    pub failures: u32,
}

/// A push target an app registers, for [`Db::register_push_target`].
pub struct NewPushTarget<'a> {
    pub app_id: &'a AppId,
    pub client_id: &'a ClientId,
    pub kind: &'a TargetKind,
    pub endpoint: &'a ::push::Endpoint,
    pub events: &'a [SaleKind],
}

/// What [`Db::register_push_target`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    /// The target, as it is now: new, or the app's target with the same
    /// `client_id` with its subscription replaced (same id, same
    /// `created_at`).
    Registered(Box<PushTarget>),
    /// The app has [`MAX_TARGETS_PER_APP`] already, none of them under
    /// this `client_id`.
    TooMany,
    /// The app was disconnected meanwhile.
    AppGone,
}

/// This instance's VAPID key as `vapid_keys` keeps it: the private key
/// encrypted (`crypto::Binding::VapidKey`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredVapidKey {
    /// The public key, base64url.
    pub public_key: String,
    pub private_key_encrypted: String,
    pub created_at: i64,
}

/// `value` read as the column `column` holds it, or the error a column of
/// the wrong type is: a row that doesn't hold what the table promises is
/// unreadable, not silently something else.
fn parsed<T, E: std::error::Error + Send + Sync + 'static>(
    column: usize,
    parsed: std::result::Result<T, E>,
) -> rusqlite::Result<T> {
    parsed.map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// A message as an error, for [`parsed`].
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Unreadable(String);

fn target_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PushTarget> {
    let index = |name: &str| row.as_ref().column_index(name);
    let kind: String = row.get("kind")?;
    let p256dh: Option<String> = row.get("p256dh")?;
    let auth: Option<String> = row.get("auth")?;
    let kind = match (kind.as_str(), p256dh, auth) {
        ("encrypted", Some(p256dh), Some(auth)) => TargetKind::Encrypted {
            p256dh: parsed(index("p256dh")?, ::push::P256dh::from_base64url(&p256dh))?,
            auth: parsed(index("auth")?, ::push::AuthSecret::from_base64url(&auth))?,
        },
        ("live-activity", None, None) => TargetKind::LiveActivity {
            include_payment_uri: row.get("include_payment_uri")?,
        },
        (kind, ..) => {
            return parsed(
                index("kind")?,
                Err(Unreadable(format!(
                    "a push target of kind {kind:?} with keys it doesn't take"
                ))),
            )
        }
    };
    let client_id: String = row.get("client_id")?;
    let endpoint: String = row.get("endpoint")?;
    let events: String = row.get("events")?;
    Ok(PushTarget {
        id: row.get("id")?,
        store_id: row.get("store_id")?,
        app_id: row.get("app_id")?,
        client_id: parsed(
            index("client_id")?,
            ClientId::parse(&client_id).map_err(Unreadable),
        )?,
        kind,
        endpoint: parsed(index("endpoint")?, ::push::Endpoint::parse(&endpoint))?,
        events: parsed(
            index("events")?,
            serde_json::from_str::<Vec<SaleKind>>(&events),
        )?,
        created_at: row.get("created_at_utc")?,
        last_ok_at: row.get("last_ok_at_utc")?,
        failures: row.get("failures")?,
    })
}

/// `events` once each, in [`SaleKind::ALL`]'s order, as `push_targets.events`
/// keeps them.
fn events_json(events: &[SaleKind]) -> String {
    let kept: Vec<SaleKind> = SaleKind::ALL
        .into_iter()
        .filter(|kind| events.contains(kind))
        .collect();
    serde_json::Value::from(kept.iter().map(|kind| kind.as_str()).collect::<Vec<_>>()).to_string()
}

impl Db {
    /// Registers an app's push target, in one transaction: the app's
    /// target with the same `client_id` gets the new subscription in place
    /// (its id and `created_at` kept, its failures and last success
    /// forgotten: they were the old subscription's); otherwise a new one,
    /// while the app has fewer than [`MAX_TARGETS_PER_APP`]. Its store is
    /// the app's.
    pub fn register_push_target(&self, new: &NewPushTarget<'_>, now: i64) -> Result<Registration> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let (p256dh, auth, include_payment_uri) = match new.kind {
            TargetKind::Encrypted { p256dh, auth } => (
                Some(p256dh.to_base64url()),
                Some(auth.to_base64url()),
                false,
            ),
            TargetKind::LiveActivity {
                include_payment_uri,
            } => (None, None, *include_payment_uri),
        };
        let events = events_json(new.events);
        let existing: Option<PushTargetId> = tx
            .query_row(
                "SELECT id FROM push_targets WHERE app_id = ?1 AND client_id = ?2",
                params![new.app_id, new.client_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE push_targets SET kind = ?2, endpoint = ?3, p256dh = ?4, auth = ?5,
                         events = ?6, include_payment_uri = ?7, last_ok_at_utc = NULL, failures = 0
                     WHERE id = ?1",
                    params![
                        id,
                        new.kind.name().as_str(),
                        new.endpoint.as_str(),
                        p256dh,
                        auth,
                        events,
                        include_payment_uri
                    ],
                )?;
                id
            }
            None => {
                let held: u32 = tx.query_row(
                    "SELECT COUNT(*) FROM push_targets WHERE app_id = ?1",
                    params![new.app_id],
                    |row| row.get(0),
                )?;
                if usize::try_from(held).map_or(true, |held| held >= MAX_TARGETS_PER_APP) {
                    return Ok(Registration::TooMany);
                }
                let id = PushTargetId::new(format!("pt_{}", uuid::Uuid::new_v4().simple()));
                // The app's own store, read with it: a target is never
                // under a store its app isn't.
                let made = tx.execute(
                    "INSERT INTO push_targets
                         (id, store_id, app_id, client_id, kind, endpoint, p256dh, auth, events,
                          include_payment_uri, created_at_utc)
                     SELECT ?1, store_id, id, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10
                     FROM app_connections WHERE id = ?2",
                    params![
                        id,
                        new.app_id,
                        new.client_id.as_str(),
                        new.kind.name().as_str(),
                        new.endpoint.as_str(),
                        p256dh,
                        auth,
                        events,
                        include_payment_uri,
                        now
                    ],
                )?;
                if made == 0 {
                    return Ok(Registration::AppGone);
                }
                id
            }
        };
        let target = tx.query_row(
            "SELECT * FROM push_targets WHERE id = ?1",
            params![id],
            target_from_row,
        )?;
        tx.commit()?;
        Ok(Registration::Registered(Box::new(target)))
    }

    /// An app's push targets, oldest first.
    pub fn list_push_targets(&self, app_id: &AppId) -> Result<Vec<PushTarget>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT * FROM push_targets WHERE app_id = ?1 ORDER BY created_at_utc, rowid",
        )?;
        let rows = stmt
            .query_map(params![app_id], target_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The app's push target `id`; `None` for one it doesn't have (another
    /// app's included).
    pub fn get_push_target(&self, app_id: &AppId, id: &PushTargetId) -> Result<Option<PushTarget>> {
        self.conn
            .query_row(
                "SELECT * FROM push_targets WHERE app_id = ?1 AND id = ?2",
                params![app_id, id],
                target_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Deletes the app's push target `id`: whether it had one.
    pub fn delete_push_target(&self, app_id: &AppId, id: &PushTargetId) -> Result<bool> {
        let deleted = self.conn.execute(
            "DELETE FROM push_targets WHERE app_id = ?1 AND id = ?2",
            params![app_id, id],
        )?;
        Ok(deleted > 0)
    }

    /// This instance's VAPID key, once one is stored.
    pub fn vapid_key(&self) -> Result<Option<StoredVapidKey>> {
        self.conn
            .query_row(
                "SELECT public_key, private_key_encrypted, created_at_utc FROM vapid_keys WHERE id = 1",
                [],
                |row| {
                    Ok(StoredVapidKey {
                        public_key: row.get(0)?,
                        private_key_encrypted: row.get(1)?,
                        created_at: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Stores `key` as this instance's VAPID key unless one is stored
    /// already, and answers the one that is: the first one stored is kept
    /// for good, whoever stored it.
    pub fn store_vapid_key(&self, key: &StoredVapidKey) -> Result<StoredVapidKey> {
        self.conn.execute(
            "INSERT INTO vapid_keys (id, public_key, private_key_encrypted, created_at_utc)
             VALUES (1, ?1, ?2, ?3) ON CONFLICT (id) DO NOTHING",
            params![key.public_key, key.private_key_encrypted, key.created_at],
        )?;
        self.vapid_key()?
            .ok_or(super::DbError::Sqlite(rusqlite::Error::QueryReturnedNoRows))
    }
}
