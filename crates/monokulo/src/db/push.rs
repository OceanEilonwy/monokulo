//! Push targets and this instance's VAPID key (migration 0052,
//! `crate::push`): where connected apps want their store's sales pushed,
//! and the key the pushes are signed with. And the pushes themselves
//! (migration 0053): queued with each sale event, in the event's own
//! transaction, and sent by the delivery worker (`crate::push::delivery`).

use rusqlite::{params, OptionalExtension as _};
use sqlite::Unsigned;

use super::{AppId, AppOrder, ConnectionId, Db, LoggedEvent, Result};
use crate::api::runtime::scalars::ClientId;
use crate::apps::{SaleEvent, SaleKind};
use crate::push::payload::{notification, LiveActivityState};
use crate::push::{policy, PushTargetKind, Queueing};
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

impl PushTarget {
    /// Whether it is pushed the sale events of `order`, one of its store's
    /// apps': an `encrypted` target hears of every app's sales (the tills
    /// of one shop all want to know of a sale), a `live-activity` target
    /// only of its own app's. A Live Activity is one island per till, and
    /// iOS shows whatever state lands last: another till's sale there, its
    /// alert or its payment code, would have this till's customer pay, or
    /// its cashier hand goods over, for the wrong sale.
    pub fn hears_of(&self, order: &AppOrder) -> bool {
        match self.kind {
            TargetKind::Encrypted { .. } => true,
            TargetKind::LiveActivity { .. } => self.app_id == order.app_id,
        }
    }
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

/// Why a push was given up on without being sent: its TTL ran out first.
pub const EXPIRED_UNSENT: &str = "expired before it could be sent";

/// Why a push was given up on without being sent: its target was
/// registered again for pushes it was not made for (another kind, the
/// payment code no longer shown, or not its event).
pub const REREGISTERED: &str = "its target was registered again for other pushes";

/// A push due now, with the target it goes to.
#[derive(Debug, Clone)]
pub struct DuePush {
    pub id: i64,
    pub target: PushTarget,
    pub event: SaleKind,
    pub urgency: ::push::Urgency,
    pub topic: ::push::Topic,
    /// The plaintext: JSON, encrypted afresh at each attempt for an
    /// `encrypted` target.
    pub payload: String,
    /// When its TTL ends.
    pub expires_at: i64,
    /// Attempts made so far.
    pub attempt_count: u32,
}

/// A due push whose row, or whose target's, doesn't hold what its table
/// promises: given up on, never sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadablePush {
    pub id: i64,
    pub target_id: PushTargetId,
    pub error: String,
}

/// One attempt at a push, for [`Db::record_push_attempt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushAttempt {
    pub at: i64,
    /// The push service's status code; `None` when nothing answered.
    pub status: Option<u16>,
    pub result: PushResult,
}

/// How an attempt went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushResult {
    /// The push service took it.
    Delivered,
    Failed {
        error: String,
        /// When to try again; `None` gives up on it.
        retry_at: Option<i64>,
        /// How many failures in a row delete the target, when this one
        /// counts towards them; `None` when it doesn't (the push service
        /// limiting this sender says nothing about the target).
        failure_limit: Option<u32>,
    },
}

/// What became of a push's target after an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetAfter {
    Kept,
    /// Its run of failures reached the limit: it and its pushes are gone.
    Deleted,
}

/// Where a push is, as a test reads it.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushDeliveryRow {
    pub id: i64,
    pub event_seq: i64,
    pub event_id: String,
    pub event_type: String,
    pub order_id: super::OrderId,
    pub topic: String,
    pub urgency: String,
    pub payload: String,
    pub expires_at: i64,
    pub created_at: i64,
    pub attempt_count: u32,
    pub next_attempt_at: Option<i64>,
    pub last_attempt_at: Option<i64>,
    pub last_status_code: Option<u16>,
    pub last_error: Option<String>,
    pub delivered_at: Option<i64>,
    pub gave_up_at: Option<i64>,
    pub superseded_at: Option<i64>,
}

/// Where a sale event is in the engine's order-event log: its place (0 for
/// `sale.waiting` from the order API, which the log never has) and its id.
#[derive(Clone, Copy)]
struct Logged<'a> {
    seq: i64,
    id: &'a str,
}

/// The sale events after which an order still waits for a payment, as
/// `app_orders.last_sale_event` keeps them (with NULL, none yet).
const STILL_WAITING: &str = "('sale.waiting', 'sale.partial')";

fn due_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<std::result::Result<DuePush, UnreadablePush>> {
    let id: i64 = row.get("delivery_id")?;
    let target_id: PushTargetId = row.get("id")?;
    let unreadable = |error: String| UnreadablePush {
        id,
        target_id: target_id.clone(),
        error,
    };
    let target = match target_from_row(row) {
        Ok(target) => target,
        Err(e) => {
            return Ok(Err(unreadable(format!(
                "its push target can't be read: {e}"
            ))))
        }
    };
    let event: String = row.get("d_event_type")?;
    let urgency: String = row.get("d_urgency")?;
    let topic: String = row.get("d_topic")?;
    let parsed = (|| {
        Ok::<_, String>((
            event.parse::<SaleKind>().map_err(|e| e.to_string())?,
            urgency
                .parse::<::push::Urgency>()
                .map_err(|_| format!("{urgency:?} isn't an urgency"))?,
            ::push::Topic::parse(&topic).map_err(|e| format!("{topic:?} isn't a topic: {e}"))?,
        ))
    })();
    let (event, urgency, topic) = match parsed {
        Ok(parsed) => parsed,
        Err(e) => return Ok(Err(unreadable(format!("it can't be read: {e}")))),
    };
    Ok(Ok(DuePush {
        id,
        target,
        event,
        urgency,
        topic,
        payload: row.get("d_payload")?,
        expires_at: row.get("d_expires_at_utc")?,
        attempt_count: row.get::<_, Unsigned<u32>>("d_attempt_count")?.0,
    }))
}

impl Db {
    /// Registers an app's push target, in one transaction: the app's
    /// target with the same `client_id` gets the new subscription in place
    /// (its id and `created_at` kept, its failures and last success
    /// forgotten: they were the old subscription's; its unsent pushes it no
    /// longer takes given up on, [`REREGISTERED`]); otherwise a new one,
    /// while the app has fewer than [`MAX_TARGETS_PER_APP`]. Its store is
    /// the app's. Either way it is now the last registered, the one pushed
    /// of the store's targets on its endpoint.
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
        let existing: Option<(PushTargetId, String, bool)> = tx
            .query_row(
                "SELECT id, kind, include_payment_uri FROM push_targets
                 WHERE app_id = ?1 AND client_id = ?2",
                params![new.app_id, new.client_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let id = match existing {
            Some((id, old_kind, old_include_payment_uri)) => {
                // Its unsent pushes were made for the target as it was: one
                // made for another kind (an `encrypted` payload, the whole
                // sale, would go out in the clear to a Live Activity), with
                // a payment code it no longer shows, or for an event it no
                // longer wants, is given up on. A new endpoint or keys alone
                // keep them: each attempt encrypts afresh to the keys then.
                let reshaped = old_kind != new.kind.name().as_str()
                    || old_include_payment_uri != include_payment_uri;
                tx.execute(
                    "UPDATE push_deliveries
                     SET next_attempt_at_utc = NULL, gave_up_at_utc = ?2, last_error = ?3
                     WHERE push_target_id = ?1 AND next_attempt_at_utc IS NOT NULL
                       AND (?4 OR event_type NOT IN (SELECT value FROM json_each(?5)))",
                    params![id, now, REREGISTERED, reshaped, events],
                )?;
                tx.execute(
                    "UPDATE push_targets SET kind = ?2, endpoint = ?3, p256dh = ?4, auth = ?5,
                         events = ?6, include_payment_uri = ?7, last_ok_at_utc = NULL, failures = 0,
                         registered_seq = (SELECT MAX(registered_seq) + 1 FROM push_targets)
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
                          include_payment_uri, created_at_utc, registered_seq)
                     SELECT ?1, store_id, id, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                            (SELECT COALESCE(MAX(registered_seq), 0) + 1 FROM push_targets)
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

    /// Queues the pushes of the sale event `event` is, inside the caller's
    /// transaction ([`Db::queue_order_event`]): nothing unless an app made
    /// its order and apps hear of that kind of event. Records it as the
    /// order's latest; then one push per target of the order's store that
    /// wants it, each superseding that target's unsent push about the same
    /// order. Returns how many were queued.
    pub(super) fn queue_pushes_for_event(
        &self,
        event: &LoggedEvent,
        at: i64,
        queueing: &Queueing,
    ) -> Result<usize> {
        let Some(order) = self.app_order(&event.order_id)? else {
            return Ok(0);
        };
        let Some(sale) = SaleEvent::from_logged(&order, event) else {
            return Ok(0);
        };
        let event = Logged {
            seq: event.seq,
            id: &event.event_id,
        };
        self.queue_sale_pushes(&order, &sale, event, None, at, queueing)
    }

    /// Queues the `sale.waiting` pushes of `order`, which an app just made,
    /// inside the caller's transaction (`orders::record`): the engine's log
    /// never has it, so its place there is 0 and its id is made here. The
    /// app that made the order isn't pushed it: it shows the sale already.
    /// Returns how many were queued.
    pub fn queue_waiting_push(
        &self,
        order: &AppOrder,
        at: i64,
        queueing: &Queueing,
    ) -> Result<usize> {
        let event_id = format!("evt_{}", uuid::Uuid::new_v4().simple());
        let event = Logged {
            seq: 0,
            id: &event_id,
        };
        let sale = SaleEvent::waiting(order);
        self.queue_sale_pushes(order, &sale, event, Some(&order.app_id), at, queueing)
    }

    /// Queues `sale`'s pushes for every target of `order`'s store that
    /// wants it and hears of the order ([`PushTarget::hears_of`]) but
    /// `except`'s, once per endpoint: records it as the order's latest,
    /// then one push per target, each superseding that target's unsent
    /// push about the same order.
    fn queue_sale_pushes(
        &self,
        order: &AppOrder,
        sale: &SaleEvent,
        event: Logged<'_>,
        except: Option<&AppId>,
        at: i64,
        queueing: &Queueing,
    ) -> Result<usize> {
        let (event_seq, event_id) = (event.seq, event.id);
        self.conn.execute(
            "UPDATE app_orders SET last_sale_event = ?2 WHERE order_id = ?1",
            params![order.order_id, sale.event.as_str()],
        )?;
        // Targets that share an endpoint are one subscription: it is
        // pushed once, as the last registered of them that wants it, which
        // holds the keys the device has now.
        let mut endpoints = std::collections::HashSet::new();
        let targets: Vec<PushTarget> = self
            .store_push_targets(&order.store_id)?
            .into_iter()
            .filter(|target| target.events.contains(&sale.event) && target.hears_of(order))
            .filter(|target| except != Some(&target.app_id))
            .filter(|target| endpoints.insert(target.endpoint.as_url().as_str().to_owned()))
            .collect();
        if targets.is_empty() {
            return Ok(0);
        }
        let urgency = policy::urgency(sale.event);
        let ttl = policy::ttl(sale.event, order.expires_at, at, queueing.default_ttl);
        let expires_at = at.saturating_add(i64::from(ttl.as_secs()));
        // Each made once, for the targets that take it.
        let mut encrypted: Option<String> = None;
        let mut waiting_count: Option<u64> = None;
        let mut queued = 0;
        for target in &targets {
            let topic = policy::topic(&target.id, &order.order_id);
            let payload = match target.kind {
                TargetKind::Encrypted { .. } => encrypted
                    .get_or_insert_with(|| notification(sale, &queueing.link_base))
                    .clone(),
                TargetKind::LiveActivity {
                    include_payment_uri,
                } => {
                    let waiting = match waiting_count {
                        Some(count) => count,
                        None => *waiting_count.insert(self.waiting_app_orders(order, at)?),
                    };
                    LiveActivityState::of(sale, waiting, include_payment_uri).to_json()
                }
            };
            self.conn.execute(
                "UPDATE push_deliveries SET superseded_at_utc = ?3, next_attempt_at_utc = NULL
                 WHERE push_target_id = ?1 AND topic = ?2 AND next_attempt_at_utc IS NOT NULL
                   AND event_id <> ?4",
                params![target.id, topic.as_str(), at, event_id],
            )?;
            queued += self.conn.execute(
                "INSERT INTO push_deliveries
                     (push_target_id, event_seq, event_id, event_type, order_id, topic, urgency,
                      payload, expires_at_utc, created_at_utc, next_attempt_at_utc)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
                 ON CONFLICT (push_target_id, event_id) DO NOTHING",
                params![
                    target.id,
                    event_seq,
                    event_id,
                    sale.event.as_str(),
                    order.order_id,
                    topic.as_str(),
                    urgency.header_value(),
                    payload,
                    expires_at,
                    at
                ],
            )?;
        }
        Ok(queued)
    }

    /// Every push target of the store's apps, the last registered first.
    fn store_push_targets(&self, store_id: &ConnectionId) -> Result<Vec<PushTarget>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT * FROM push_targets WHERE store_id = ?1
             ORDER BY registered_seq DESC",
        )?;
        let rows = stmt
            .query_map(params![store_id], target_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The orders of `order`'s app still waiting for a payment at `now`:
    /// none seen yet, or only part, and not expired. A Live Activity's
    /// `waitingCount`: its island shows only its own app's sales.
    fn waiting_app_orders(&self, order: &AppOrder, now: i64) -> Result<u64> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM app_orders
                     WHERE store_id = ?1 AND app_id = ?2 AND expires_at > ?3
                       AND (last_sale_event IS NULL OR last_sale_event IN {STILL_WAITING})"
                ),
                params![order.store_id, order.app_id, now],
                |row| row.get::<_, Unsigned<u64>>(0),
            )?
            .0)
    }

    /// At most `limit` pushes due by `now`, soonest first, each with its
    /// target; one that can't be read comes back as such, to be given up
    /// on, rather than stopping every other. A push is due only once every
    /// older unsent push to its target is (one waiting for a retry holds
    /// the newer back), and comes after them: a target's pushes go out in
    /// the order they were queued, so a Live Activity's last state to land
    /// is its newest.
    pub fn due_push_deliveries(
        &self,
        now: i64,
        limit: u32,
    ) -> Result<Vec<std::result::Result<DuePush, UnreadablePush>>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT d.id AS delivery_id, d.event_type AS d_event_type, d.urgency AS d_urgency,
                    d.topic AS d_topic, d.payload AS d_payload,
                    d.expires_at_utc AS d_expires_at_utc, d.attempt_count AS d_attempt_count,
                    t.*
             FROM (SELECT id, MAX(next_attempt_at_utc) OVER (
                              PARTITION BY push_target_id ORDER BY id ROWS UNBOUNDED PRECEDING
                          ) AS due_at
                   FROM push_deliveries WHERE next_attempt_at_utc IS NOT NULL) u
             JOIN push_deliveries d ON d.id = u.id
             JOIN push_targets t ON t.id = d.push_target_id
             WHERE u.due_at <= ?1
             ORDER BY u.due_at, d.id
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![now, limit], due_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Records one attempt at the push `id` to `target_id`, and what it
    /// says about the target, in one transaction. Delivered: the target's
    /// run of failures ends. Failed: it is tried again at `retry_at` or
    /// given up on, and a failure that counts lengthens the target's run,
    /// which at its limit deletes the target. A push superseded while it
    /// was being sent keeps its attempt but isn't tried again.
    pub fn record_push_attempt(
        &self,
        id: i64,
        target_id: &PushTargetId,
        attempt: &PushAttempt,
    ) -> Result<TargetAfter> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let (error, retry_at, delivered_at, gave_up_at) = match &attempt.result {
            PushResult::Delivered => (None, None, Some(attempt.at), None),
            PushResult::Failed {
                error, retry_at, ..
            } => (
                Some(error.as_str()),
                *retry_at,
                None,
                retry_at.is_none().then_some(attempt.at),
            ),
        };
        self.conn.execute(
            "UPDATE push_deliveries
             SET attempt_count = MIN(attempt_count + 1, 4294967295),
                 next_attempt_at_utc = CASE WHEN superseded_at_utc IS NULL THEN ?2 END,
                 last_attempt_at_utc = ?3, last_status_code = ?4, last_error = ?5,
                 delivered_at_utc = ?6,
                 gave_up_at_utc = CASE WHEN superseded_at_utc IS NULL THEN ?7 END
             WHERE id = ?1 AND delivered_at_utc IS NULL AND gave_up_at_utc IS NULL",
            params![
                id,
                retry_at,
                attempt.at,
                attempt.status,
                error,
                delivered_at,
                gave_up_at
            ],
        )?;
        let after = match &attempt.result {
            PushResult::Delivered => {
                self.conn.execute(
                    "UPDATE push_targets SET failures = 0, last_ok_at_utc = ?2 WHERE id = ?1",
                    params![target_id, attempt.at],
                )?;
                TargetAfter::Kept
            }
            PushResult::Failed {
                failure_limit: None,
                ..
            } => TargetAfter::Kept,
            PushResult::Failed {
                failure_limit: Some(limit),
                ..
            } => {
                let deleted = self.conn.execute(
                    "DELETE FROM push_targets WHERE id = ?1 AND failures + 1 >= ?2",
                    params![target_id, limit],
                )?;
                if deleted == 0 {
                    self.conn.execute(
                        "UPDATE push_targets SET failures = MIN(failures + 1, 4294967295)
                         WHERE id = ?1",
                        params![target_id],
                    )?;
                    TargetAfter::Kept
                } else {
                    TargetAfter::Deleted
                }
            }
        };
        tx.commit()?;
        Ok(after)
    }

    /// Puts the push `id` off until `until` without an attempt: its
    /// store's pushes for this minute are spent (`push.per_store_per_minute`).
    pub fn defer_push(&self, id: i64, until: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE push_deliveries SET next_attempt_at_utc = ?2
             WHERE id = ?1 AND next_attempt_at_utc IS NOT NULL",
            params![id, until],
        )?;
        Ok(())
    }

    /// Gives up on the push `id` without an attempt, saying why: it expired
    /// unsent ([`EXPIRED_UNSENT`]), has no attempts left, would expire
    /// before its store's next minute of pushes, or can't be sent.
    pub fn give_up_push(&self, id: i64, at: i64, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE push_deliveries SET next_attempt_at_utc = NULL, gave_up_at_utc = ?2,
                 last_error = ?3
             WHERE id = ?1 AND next_attempt_at_utc IS NOT NULL",
            params![id, at, error],
        )?;
        Ok(())
    }

    /// Deletes a push target its push service said is gone (`404`, `410`),
    /// with its pushes: whether it was still there.
    pub fn delete_dead_push_target(&self, id: &PushTargetId) -> Result<bool> {
        let deleted = self
            .conn
            .execute("DELETE FROM push_targets WHERE id = ?1", params![id])?;
        Ok(deleted > 0)
    }

    /// Deletes pushes delivered, given up on or superseded before `before`
    /// (unix seconds). Returns how many.
    pub fn prune_push_deliveries(&self, before: i64) -> Result<u64> {
        let deleted = self.conn.execute(
            "DELETE FROM push_deliveries
             WHERE delivered_at_utc < ?1 OR gave_up_at_utc < ?1 OR superseded_at_utc < ?1",
            params![before],
        )?;
        Ok(u64::try_from(deleted).unwrap_or(u64::MAX))
    }

    /// Every push queued for `target_id`, oldest first.
    #[cfg(test)]
    pub fn push_deliveries(&self, target_id: &PushTargetId) -> Result<Vec<PushDeliveryRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT * FROM push_deliveries WHERE push_target_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![target_id], |row| {
                Ok(PushDeliveryRow {
                    id: row.get("id")?,
                    event_seq: row.get("event_seq")?,
                    event_id: row.get("event_id")?,
                    event_type: row.get("event_type")?,
                    order_id: row.get("order_id")?,
                    topic: row.get("topic")?,
                    urgency: row.get("urgency")?,
                    payload: row.get("payload")?,
                    expires_at: row.get("expires_at_utc")?,
                    created_at: row.get("created_at_utc")?,
                    attempt_count: row.get::<_, Unsigned<u32>>("attempt_count")?.0,
                    next_attempt_at: row.get("next_attempt_at_utc")?,
                    last_attempt_at: row.get("last_attempt_at_utc")?,
                    last_status_code: row
                        .get::<_, Option<Unsigned<u16>>>("last_status_code")?
                        .map(|s| s.0),
                    last_error: row.get("last_error")?,
                    delivered_at: row.get("delivered_at_utc")?,
                    gave_up_at: row.get("gave_up_at_utc")?,
                    superseded_at: row.get("superseded_at_utc")?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
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
