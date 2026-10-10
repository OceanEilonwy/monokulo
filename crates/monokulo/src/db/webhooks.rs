//! Stores' webhooks and their deliveries (migration 0038,
//! `crate::webhooks`): what the store settings page lists, what the
//! subscriber queues from the engine's order-event log, and what the
//! delivery worker sends.

use rusqlite::{params, OptionalExtension as _};
use shared::ids::WebhookId;
use shared::sqlite::Unsigned;

use super::{ConnectionId, Db, OrderId, Result};

/// Most attempts a delivery keeps in `attempts_json`.
pub const KEPT_ATTEMPTS: usize = 20;

/// A store's webhook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookRow {
    pub id: WebhookId,
    pub store_id: ConnectionId,
    pub url: String,
    /// Its signing secret, encrypted at rest (`crypto::Binding::WebhookSecret`).
    pub signing_secret_encrypted: String,
    /// A JSON object of the headers sent with every delivery.
    pub extra_headers: String,
    pub created_at: i64,
}

/// A webhook just made, for [`Db::create_webhook`].
pub struct NewWebhook<'a> {
    pub id: &'a WebhookId,
    pub store_id: &'a ConnectionId,
    pub url: &'a str,
    pub signing_secret_encrypted: &'a str,
    pub extra_headers: &'a str,
    pub at: i64,
}

/// One attempt at a delivery.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Attempt {
    /// Which attempt of its schedule (1 to the most allowed).
    pub n: u32,
    /// When it was made.
    pub at: i64,
    /// The endpoint's status code; `None` when nothing answered.
    pub status: Option<u16>,
    pub error: Option<String>,
    /// How long it took, in milliseconds.
    pub ms: u64,
    /// The `X-Monokulo-Signature` it was sent with.
    pub signature: String,
}

/// Where a delivery is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    /// Not tried yet.
    Queued,
    /// Tried, failed, and tried again later.
    Retrying,
    Delivered,
    /// Its last attempt failed: never tried again by itself.
    GaveUp,
}

/// A delivery of one event to one webhook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRow {
    pub id: i64,
    pub webhook_id: WebhookId,
    pub event_seq: i64,
    pub event_id: String,
    pub event_type: String,
    pub order_id: OrderId,
    /// The exact body sent.
    pub body: String,
    pub created_at: i64,
    /// Attempts in its current schedule.
    pub attempt_count: u32,
    pub next_attempt_at: Option<i64>,
    pub last_attempt_at: Option<i64>,
    pub last_status_code: Option<u16>,
    pub last_error: Option<String>,
    pub last_duration_ms: Option<u64>,
    pub last_response: Option<String>,
    pub delivered_at: Option<i64>,
    pub gave_up_at: Option<i64>,
    /// The latest attempts, oldest first.
    pub attempts: Vec<Attempt>,
}

impl DeliveryRow {
    pub fn state(&self) -> DeliveryState {
        if self.delivered_at.is_some() {
            DeliveryState::Delivered
        } else if self.gave_up_at.is_some() {
            DeliveryState::GaveUp
        } else if self.attempt_count == 0 {
            DeliveryState::Queued
        } else {
            DeliveryState::Retrying
        }
    }
}

/// A delivery due now, with what sending it takes.
#[derive(Debug, Clone)]
pub struct DueDelivery {
    pub delivery_id: i64,
    pub webhook_id: WebhookId,
    pub store_id: ConnectionId,
    pub order_id: OrderId,
    pub event_id: String,
    pub event_type: String,
    pub body: String,
    pub attempt_count: u32,
    pub url: String,
    pub extra_headers: String,
    pub signing_secret_encrypted: String,
}

/// How one attempt went, for [`Db::record_delivery_attempt`].
#[derive(Debug, Clone)]
pub struct AttemptOutcome {
    pub attempt: Attempt,
    pub delivered: bool,
    /// The start of the answer, when there was one.
    pub response: Option<String>,
    /// When to try again; `None` gives up (unless delivered).
    pub next_attempt_at: Option<i64>,
}

/// One event from the engine's order-event log, as monokulo keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedEvent {
    /// Its position in the engine's log.
    pub seq: i64,
    pub event_id: String,
    /// `order.<status>`, `order.double_spend_detected` or
    /// `order.double_spend_reversed`.
    pub event_type: String,
    pub created_at: i64,
    /// The store's public key.
    pub tenant_public_key: String,
    pub order_id: OrderId,
    pub status: Option<String>,
    pub txid: Option<String>,
    pub merchant_order_id: Option<String>,
    pub xmr_amount_piconero: u64,
}

/// A webhook's deliveries in sum: what its health line says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebhookHealth {
    /// Waiting for a first or a later attempt.
    pub waiting: u64,
    /// Of those, tried at least once.
    pub retrying: u64,
    pub gave_up: u64,
    /// Delivered since the `since` asked for.
    pub delivered_recently: u64,
    /// The soonest next attempt.
    pub next_attempt_at: Option<i64>,
    /// The waiting delivery tried most, with its last failure.
    pub worst_retrying: Option<DeliveryRow>,
    /// The latest given-up delivery.
    pub latest_gave_up: Option<DeliveryRow>,
    /// The latest delivered one.
    pub latest_delivered: Option<DeliveryRow>,
}

const DELIVERY_COLUMNS: &str = "id, webhook_id, event_seq, event_id, event_type, order_id, body,
    created_at_utc, attempt_count, next_attempt_at_utc, last_attempt_at_utc, last_status_code,
    last_error, last_duration_ms, last_response, delivered_at_utc, gave_up_at_utc, attempts_json";

fn delivery_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeliveryRow> {
    let attempts: String = row.get(17)?;
    Ok(DeliveryRow {
        id: row.get(0)?,
        webhook_id: row.get(1)?,
        event_seq: row.get(2)?,
        event_id: row.get(3)?,
        event_type: row.get(4)?,
        order_id: row.get(5)?,
        body: row.get(6)?,
        created_at: row.get(7)?,
        attempt_count: row.get::<_, Unsigned<u32>>(8)?.0,
        next_attempt_at: row.get(9)?,
        last_attempt_at: row.get(10)?,
        last_status_code: row.get::<_, Option<Unsigned<u16>>>(11)?.map(|s| s.0),
        last_error: row.get(12)?,
        last_duration_ms: row.get::<_, Option<Unsigned<u64>>>(13)?.map(|s| s.0),
        last_response: row.get(14)?,
        delivered_at: row.get(15)?,
        gave_up_at: row.get(16)?,
        // Written only here; a row that can't be read shows no attempts
        // rather than failing the page.
        attempts: serde_json::from_str(&attempts).unwrap_or_default(),
    })
}

fn webhook_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WebhookRow> {
    Ok(WebhookRow {
        id: row.get(0)?,
        store_id: row.get(1)?,
        url: row.get(2)?,
        signing_secret_encrypted: row.get(3)?,
        extra_headers: row.get(4)?,
        created_at: row.get(5)?,
    })
}

const WEBHOOK_COLUMNS: &str =
    "id, store_id, url, signing_secret_encrypted, extra_headers, created_at_utc";

impl Db {
    pub fn create_webhook(&self, new: &NewWebhook<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO webhooks (id, store_id, url, signing_secret_encrypted, extra_headers, created_at_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                new.id,
                new.store_id,
                new.url,
                new.signing_secret_encrypted,
                new.extra_headers,
                new.at
            ],
        )?;
        Ok(())
    }

    /// A store's webhooks, oldest first.
    pub fn list_webhooks(&self, store_id: &ConnectionId) -> Result<Vec<WebhookRow>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {WEBHOOK_COLUMNS} FROM webhooks WHERE store_id = ?1 ORDER BY created_at_utc, id"
        ))?;
        let rows = stmt
            .query_map(params![store_id], webhook_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// One of the store's webhooks; `None` when it isn't the store's.
    pub fn get_webhook(
        &self,
        store_id: &ConnectionId,
        id: &WebhookId,
    ) -> Result<Option<WebhookRow>> {
        self.conn
            .query_row(
                &format!("SELECT {WEBHOOK_COLUMNS} FROM webhooks WHERE id = ?1 AND store_id = ?2"),
                params![id, store_id],
                webhook_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Deletes one of the store's webhooks and its deliveries. `false` when
    /// it isn't the store's (or is gone already).
    pub fn delete_webhook(&self, store_id: &ConnectionId, id: &WebhookId) -> Result<bool> {
        let changed = self.conn.execute(
            "DELETE FROM webhooks WHERE id = ?1 AND store_id = ?2",
            params![id, store_id],
        )?;
        Ok(changed == 1)
    }

    /// Deletes one delivery: for fixtures that queue an event for one
    /// webhook of a store with several (each event goes to all of them).
    #[cfg(any(test, feature = "test-support"))]
    pub fn delete_delivery_for_test(&self, delivery_id: i64) {
        self.conn
            .execute(
                "DELETE FROM webhook_deliveries WHERE id = ?1",
                params![delivery_id],
            )
            .expect("deleting a test delivery");
    }

    /// Where the order-event log has been read up to: 0 before the first
    /// event.
    pub fn order_event_position(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row(
                "SELECT after_seq FROM order_event_position WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    fn save_order_event_position(&self, after: i64, at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO order_event_position (id, after_seq, updated_at_utc) VALUES (1, ?1, ?2)
             ON CONFLICT (id) DO UPDATE SET after_seq = ?1, updated_at_utc = ?2",
            params![after, at],
        )?;
        Ok(())
    }

    /// Moves the position without queueing anything: the engine said the
    /// events before `after` are gone.
    pub fn skip_order_events_to(&self, after: i64, at: i64) -> Result<()> {
        self.save_order_event_position(after, at)
    }

    /// Queues `event` for each of its store's webhooks, with the
    /// body `body` makes for each, and moves the position past it, in one
    /// transaction: a restart never queues an event twice nor skips one.
    /// An event at or before the position was handled already and is left
    /// alone. Returns how many deliveries were queued.
    pub fn queue_order_event(
        &self,
        event: &LoggedEvent,
        at: i64,
        body: impl Fn(&super::StoreConnectionRow, Option<&super::OrderCurrencyMetadataRow>) -> String,
    ) -> Result<usize> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if event.seq <= self.order_event_position()? {
            return Ok(0);
        }
        let mut queued = 0;
        if let Some(store) = self.get_store_connection_by_public_key(&event.tenant_public_key)? {
            let webhooks = self.list_webhooks(&store.id)?;
            if !webhooks.is_empty() {
                let metadata = self.get_order_currency_metadata(&store.id, &event.order_id)?;
                let body = body(&store, metadata.as_ref());
                for webhook in &webhooks {
                    queued += self.conn.execute(
                        "INSERT INTO webhook_deliveries
                            (webhook_id, event_seq, event_id, event_type, order_id, body,
                             created_at_utc, next_attempt_at_utc)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                         ON CONFLICT (webhook_id, event_id) DO NOTHING",
                        params![
                            webhook.id,
                            event.seq,
                            event.event_id,
                            event.event_type,
                            event.order_id,
                            body,
                            at
                        ],
                    )?;
                }
            }
        }
        self.save_order_event_position(event.seq, at)?;
        tx.commit()?;
        Ok(queued)
    }

    /// Deliveries due by `now`, picked fairly:
    /// - at most one per (webhook, order), the oldest queued among every
    ///   waiting one (due or between attempts), so two events for one
    ///   order are never in flight at once and a later one never overtakes
    ///   an earlier one; a given-up delivery holds nothing back;
    /// - at most `per_store` per store, so one store's backlog or slow
    ///   endpoint can't fill the batch.
    pub fn due_deliveries(&self, now: i64, per_store: u32, limit: u32) -> Result<Vec<DueDelivery>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, webhook_id, store_id, order_id, event_id, event_type, body, attempt_count,
                    url, extra_headers, signing_secret_encrypted
             FROM (
                SELECT *, ROW_NUMBER() OVER (PARTITION BY store_id ORDER BY due_at, id) AS per_store
                FROM (
                    SELECT d.id, d.webhook_id, w.store_id, d.order_id, d.event_id, d.event_type, d.body,
                           d.attempt_count, w.url, w.extra_headers, w.signing_secret_encrypted,
                           d.next_attempt_at_utc AS due_at,
                           ROW_NUMBER() OVER (PARTITION BY d.webhook_id, d.order_id ORDER BY d.id) AS per_order
                    FROM webhook_deliveries d
                    JOIN webhooks w ON w.id = d.webhook_id
                    WHERE d.next_attempt_at_utc IS NOT NULL
                )
                WHERE per_order = 1 AND due_at <= ?1
             )
             WHERE per_store <= ?2
             ORDER BY due_at, id
             LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![now, per_store, limit], |row| {
                Ok(DueDelivery {
                    delivery_id: row.get(0)?,
                    webhook_id: row.get(1)?,
                    store_id: row.get(2)?,
                    order_id: row.get(3)?,
                    event_id: row.get(4)?,
                    event_type: row.get(5)?,
                    body: row.get(6)?,
                    attempt_count: row.get::<_, Unsigned<u32>>(7)?.0,
                    url: row.get(8)?,
                    extra_headers: row.get(9)?,
                    signing_secret_encrypted: row.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Records one attempt: delivered, scheduled again at
    /// `outcome.next_attempt_at`, or given up on. A delivery no longer
    /// waiting (delivered or given up meanwhile, or deleted) is left as it
    /// is.
    pub fn record_delivery_attempt(
        &self,
        delivery_id: i64,
        outcome: &AttemptOutcome,
    ) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let attempts: Option<String> = self
            .conn
            .query_row(
                "SELECT attempts_json FROM webhook_deliveries
                 WHERE id = ?1 AND next_attempt_at_utc IS NOT NULL",
                params![delivery_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(attempts) = attempts else {
            return Ok(());
        };
        let mut attempts: Vec<Attempt> = serde_json::from_str(&attempts).unwrap_or_default();
        attempts.push(outcome.attempt.clone());
        if attempts.len() > KEPT_ATTEMPTS {
            attempts.drain(..attempts.len() - KEPT_ATTEMPTS);
        }
        let at = outcome.attempt.at;
        let (next, delivered, gave_up) = if outcome.delivered {
            (None, Some(at), None)
        } else {
            match outcome.next_attempt_at {
                Some(next) => (Some(next), None, None),
                None => (None, None, Some(at)),
            }
        };
        self.conn.execute(
            "UPDATE webhook_deliveries
             SET attempt_count = MIN(attempt_count + 1, 4294967295),
                 next_attempt_at_utc = ?2, last_attempt_at_utc = ?3, last_status_code = ?4,
                 last_error = ?5, last_duration_ms = ?6, last_response = ?7,
                 delivered_at_utc = ?8, gave_up_at_utc = ?9, attempts_json = ?10
             WHERE id = ?1",
            params![
                delivery_id,
                next,
                at,
                outcome.attempt.status,
                outcome.attempt.error,
                Unsigned(outcome.attempt.ms),
                outcome.response,
                delivered,
                gave_up,
                serde_json::to_string(&attempts).unwrap_or_else(|_| "[]".to_string()),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Gives up on a delivery whose schedule ran out without another
    /// attempt (the most attempts allowed was lowered).
    pub fn give_up_delivery(&self, delivery_id: i64, at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE webhook_deliveries SET next_attempt_at_utc = NULL, gave_up_at_utc = ?2
             WHERE id = ?1 AND next_attempt_at_utc IS NOT NULL",
            params![delivery_id, at],
        )?;
        Ok(())
    }

    /// Sends one of the webhook's deliveries again at `now`. One that was
    /// delivered or given up on starts the schedule over; one still
    /// waiting is just tried now. `false` when it isn't the store's.
    pub fn send_delivery_again(
        &self,
        store_id: &ConnectionId,
        webhook_id: &WebhookId,
        delivery_id: i64,
        now: i64,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE webhook_deliveries
             SET attempt_count = CASE WHEN next_attempt_at_utc IS NULL THEN 0 ELSE attempt_count END,
                 next_attempt_at_utc = ?4, delivered_at_utc = NULL, gave_up_at_utc = NULL
             WHERE id = ?3 AND webhook_id = ?2
               AND EXISTS (SELECT 1 FROM webhooks WHERE id = ?2 AND store_id = ?1)",
            params![store_id, webhook_id, delivery_id, now],
        )?;
        Ok(changed == 1)
    }

    /// Queues every delivery of the webhook that was given up on again,
    /// each starting the schedule over at `now`. Returns how many.
    pub fn retry_failed_deliveries(
        &self,
        store_id: &ConnectionId,
        webhook_id: &WebhookId,
        now: i64,
    ) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE webhook_deliveries
             SET attempt_count = 0, next_attempt_at_utc = ?3, gave_up_at_utc = NULL
             WHERE webhook_id = ?2 AND gave_up_at_utc IS NOT NULL
               AND EXISTS (SELECT 1 FROM webhooks WHERE id = ?2 AND store_id = ?1)",
            params![store_id, webhook_id, now],
        )?)
    }

    /// The webhook's latest `limit` deliveries, newest first.
    pub fn recent_deliveries(
        &self,
        webhook_id: &WebhookId,
        limit: usize,
    ) -> Result<Vec<DeliveryRow>> {
        self.deliveries_page(webhook_id, 0, limit)
    }

    /// `limit` of the webhook's deliveries, newest first, after skipping
    /// the `offset` newest.
    pub fn deliveries_page(
        &self,
        webhook_id: &WebhookId,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<DeliveryRow>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {DELIVERY_COLUMNS} FROM webhook_deliveries WHERE webhook_id = ?1
             ORDER BY id DESC LIMIT ?2 OFFSET ?3"
        ))?;
        let rows = stmt
            .query_map(
                params![
                    webhook_id,
                    i64::try_from(limit).unwrap_or(i64::MAX),
                    i64::try_from(offset).unwrap_or(i64::MAX)
                ],
                delivery_from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Deletes deliveries delivered before `delivered_before` and ones
    /// given up on before `gave_up_before` (unix seconds). Returns how many
    /// of each.
    pub fn prune_deliveries(
        &self,
        delivered_before: i64,
        gave_up_before: i64,
    ) -> Result<(u64, u64)> {
        let delivered = self.conn.execute(
            "DELETE FROM webhook_deliveries WHERE delivered_at_utc < ?1",
            params![delivered_before],
        )?;
        let gave_up = self.conn.execute(
            "DELETE FROM webhook_deliveries WHERE gave_up_at_utc < ?1",
            params![gave_up_before],
        )?;
        Ok((
            u64::try_from(delivered).unwrap_or(u64::MAX),
            u64::try_from(gave_up).unwrap_or(u64::MAX),
        ))
    }

    /// One of the store's deliveries; `None` when it isn't the store's.
    pub fn get_delivery(
        &self,
        store_id: &ConnectionId,
        webhook_id: &WebhookId,
        delivery_id: i64,
    ) -> Result<Option<DeliveryRow>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {DELIVERY_COLUMNS} FROM webhook_deliveries d
                     WHERE d.id = ?3 AND d.webhook_id = ?2
                       AND EXISTS (SELECT 1 FROM webhooks WHERE id = ?2 AND store_id = ?1)"
                ),
                params![store_id, webhook_id, delivery_id],
                delivery_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// What the webhook's health line says; `since` bounds "delivered
    /// recently".
    pub fn webhook_health(&self, webhook_id: &WebhookId, since: i64) -> Result<WebhookHealth> {
        let (waiting, retrying, gave_up, delivered_recently, next_attempt_at) = self.conn.query_row(
            "SELECT COUNT(next_attempt_at_utc),
                    COUNT(CASE WHEN next_attempt_at_utc IS NOT NULL AND attempt_count > 0 THEN 1 END),
                    COUNT(gave_up_at_utc),
                    COUNT(CASE WHEN delivered_at_utc >= ?2 THEN 1 END),
                    MIN(next_attempt_at_utc)
             FROM webhook_deliveries WHERE webhook_id = ?1",
            params![webhook_id, since],
            |row| {
                Ok((
                    row.get::<_, Unsigned<u64>>(0)?.0,
                    row.get::<_, Unsigned<u64>>(1)?.0,
                    row.get::<_, Unsigned<u64>>(2)?.0,
                    row.get::<_, Unsigned<u64>>(3)?.0,
                    row.get::<_, Option<i64>>(4)?,
                ))
            },
        )?;
        let one = |condition: &str, order: &str| -> Result<Option<DeliveryRow>> {
            self.conn
                .query_row(
                    &format!(
                        "SELECT {DELIVERY_COLUMNS} FROM webhook_deliveries
                         WHERE webhook_id = ?1 AND {condition} ORDER BY {order} LIMIT 1"
                    ),
                    params![webhook_id],
                    delivery_from_row,
                )
                .optional()
                .map_err(Into::into)
        };
        Ok(WebhookHealth {
            waiting,
            retrying,
            gave_up,
            delivered_recently,
            next_attempt_at,
            worst_retrying: one(
                "next_attempt_at_utc IS NOT NULL AND attempt_count > 0",
                "attempt_count DESC, id",
            )?,
            latest_gave_up: one("gave_up_at_utc IS NOT NULL", "gave_up_at_utc DESC, id DESC")?,
            latest_delivered: one(
                "delivered_at_utc IS NOT NULL",
                "delivered_at_utc DESC, id DESC",
            )?,
        })
    }
}
