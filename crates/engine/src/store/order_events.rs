//! The order-event log (migration 0030, `docs/DESIGN.md` §11): what a
//! store's webhooks announce, written in the same transaction as the change
//! itself, read in order by monokulo's subscription
//! (`GET /api/v1/admin/order-events`), and kept for
//! `order_events.retention_days`.
//!
//! The engine sends no webhook itself: monokulo holds the merchant's side of
//! an order (its price, its store) and delivers them.

use rusqlite::{params, OptionalExtension as _};

use super::{OrderId, Result, Store, StoreError, TenantId};

/// The default for `order_events.retention_days`.
pub const DEFAULT_ORDER_EVENT_RETENTION_DAYS: u64 = 7;

/// [`DEFAULT_ORDER_EVENT_RETENTION_DAYS`] in seconds, for callers without
/// settings (tests, tools).
pub const DEFAULT_ORDER_EVENT_RETENTION_SECS: i64 = 7 * 86_400;

/// One event in the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderEvent {
    /// Its position in the log: the SSE `id:`.
    pub seq: i64,
    /// `evt_` + a UUID: the id a webhook carries.
    pub event_id: String,
    pub tenant_id: TenantId,
    /// The store's public key, which monokulo knows the store by.
    pub tenant_public_key: String,
    pub order_id: OrderId,
    /// `order.<status>`, `order.double_spend_detected` or
    /// `order.double_spend_reversed`.
    pub event_type: String,
    /// The event's own fields, a JSON object.
    pub payload_json: String,
    pub created_at: i64,
}

impl OrderEvent {
    /// The event as the stream sends it: its own fields, plus `event_id`,
    /// `event`, `created_at` and `tenant` (the store's public key).
    pub fn wire_json(&self) -> String {
        let mut object = match serde_json::from_str::<serde_json::Value>(&self.payload_json) {
            Ok(serde_json::Value::Object(object)) => object,
            _ => serde_json::Map::new(),
        };
        object.insert("event_id".into(), self.event_id.clone().into());
        object.insert("event".into(), self.event_type.clone().into());
        object.insert("created_at".into(), self.created_at.into());
        object.insert("tenant".into(), self.tenant_public_key.clone().into());
        serde_json::Value::Object(object).to_string()
    }

    /// One of the event's own fields, as text.
    pub fn field(&self, name: &str) -> Option<String> {
        let value: serde_json::Value = serde_json::from_str(&self.payload_json).ok()?;
        match value.get(name)? {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Null => None,
            other @ (serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::Array(_)
            | serde_json::Value::Object(_)) => Some(other.to_string()),
        }
    }
}

/// Which events the log still holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderEventSpan {
    /// The oldest kept event's `seq`; `None` when none is kept.
    pub oldest: Option<i64>,
    /// The highest `seq` ever handed out, kept or pruned; 0 before the
    /// first event.
    pub latest: i64,
}

/// Where a reader asking for the events after `after` carries on from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumePoint {
    /// Every event after `after` is still in the log.
    Complete,
    /// Some events after `after` were pruned (it's older than the log), or
    /// `after` is past the newest event this log ever had (it isn't this
    /// log's position: the engine's database was replaced). The reader
    /// carries on after `resume_after` and has missed the rest.
    Lost { resume_after: i64 },
}

impl OrderEventSpan {
    /// Whether the events after `after` are all still here.
    pub fn resume(&self, after: i64) -> ResumePoint {
        if after > self.latest {
            return ResumePoint::Lost {
                resume_after: self.oldest.map_or(self.latest, |oldest| oldest - 1),
            };
        }
        // With nothing kept, the next event is the one after the newest.
        let oldest = self.oldest.unwrap_or(self.latest + 1);
        if after.saturating_add(1) < oldest {
            ResumePoint::Lost {
                resume_after: oldest - 1,
            }
        } else {
            ResumePoint::Complete
        }
    }
}

impl Store {
    /// Adds an event about `order_id` to the log, with `fields` and the
    /// order's `order_id`, `merchant_order_id` and `xmr_amount_piconero`.
    /// Called in the transaction that makes the change it announces, so the
    /// two land together or not at all. Returns its `seq`.
    pub fn append_order_event(
        &self,
        order_id: &OrderId,
        event_type: &str,
        fields: &[(&str, &str)],
        now: i64,
    ) -> Result<i64> {
        let (tenant_id, merchant_order_id, xmr_amount): (TenantId, Option<String>, i64) = self
            .conn
            .query_row(
                "SELECT tenant_id, merchant_order_id, xmr_amount_piconero FROM orders WHERE id = ?1",
                params![order_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        let mut payload: serde_json::Map<String, serde_json::Value> = fields
            .iter()
            .map(|(name, value)| ((*name).to_owned(), serde_json::Value::from(*value)))
            .collect();
        payload.insert("order_id".into(), order_id.as_str().into());
        payload.insert("merchant_order_id".into(), merchant_order_id.into());
        payload.insert("xmr_amount_piconero".into(), xmr_amount.into());
        let event_id = format!("evt_{}", uuid::Uuid::new_v4().simple());
        self.conn
            .prepare_cached(
                "INSERT INTO order_events
                     (event_id, tenant_id, order_id, event_type, payload_json, created_at_utc)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![
                event_id,
                tenant_id,
                order_id,
                event_type,
                serde_json::Value::Object(payload).to_string(),
                now
            ])?;
        self.note_order_event_appended();
        Ok(self.conn.last_insert_rowid())
    }

    /// Up to `limit` events after `after`, oldest first.
    pub fn order_events_after(&self, after: i64, limit: usize) -> Result<Vec<OrderEvent>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = self.conn.prepare_cached(
            "SELECT e.seq, e.event_id, e.tenant_id, t.public_key, e.order_id, e.event_type,
                    e.payload_json, e.created_at_utc
             FROM order_events e JOIN tenants t ON t.id = e.tenant_id
             WHERE e.seq > ?1 ORDER BY e.seq LIMIT ?2",
        )?;
        let rows = statement.query_map(params![after, limit], |row| {
            Ok(OrderEvent {
                seq: row.get(0)?,
                event_id: row.get(1)?,
                tenant_id: row.get(2)?,
                tenant_public_key: row.get(3)?,
                order_id: row.get(4)?,
                event_type: row.get(5)?,
                payload_json: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    /// Which events the log holds.
    pub fn order_event_span(&self) -> Result<OrderEventSpan> {
        let oldest = self
            .conn
            .query_row("SELECT MIN(seq) FROM order_events", [], |row| row.get(0))?;
        let latest = self
            .conn
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'order_events'",
                [],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(OrderEventSpan { oldest, latest })
    }

    /// Deletes events made before `cutoff` (unix seconds). Returns how many.
    pub fn prune_order_events_before(&self, cutoff: i64) -> Result<u64> {
        let deleted = self.conn.execute(
            "DELETE FROM order_events WHERE created_at_utc < ?1",
            params![cutoff],
        )?;
        Ok(u64::try_from(deleted).unwrap_or(u64::MAX))
    }

    /// Every event in the log, oldest first: for tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn order_events_for_test(&self) -> Result<Vec<OrderEvent>> {
        self.order_events_after(0, usize::MAX >> 1)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn span(oldest: Option<i64>, latest: i64) -> OrderEventSpan {
        OrderEventSpan { oldest, latest }
    }

    /// A reader is told it lost events exactly when some after its
    /// position are gone: pruned, or never in this log at all.
    #[test]
    fn a_resume_point_is_lost_only_when_events_after_it_are_gone() {
        // A new log, and a reader that has read everything.
        assert_eq!(span(None, 0).resume(0), ResumePoint::Complete);
        assert_eq!(span(Some(1), 10).resume(0), ResumePoint::Complete);
        assert_eq!(span(Some(1), 10).resume(10), ResumePoint::Complete);
        // Events 1-4 pruned: a reader at 4 or later has missed nothing.
        assert_eq!(span(Some(5), 10).resume(4), ResumePoint::Complete);
        assert_eq!(
            span(Some(5), 10).resume(2),
            ResumePoint::Lost { resume_after: 4 }
        );
        // Everything pruned: a reader at the newest missed nothing.
        assert_eq!(span(None, 10).resume(10), ResumePoint::Complete);
        assert_eq!(
            span(None, 10).resume(3),
            ResumePoint::Lost { resume_after: 10 }
        );
        // A position past anything this log handed out.
        assert_eq!(
            span(Some(1), 10).resume(50),
            ResumePoint::Lost { resume_after: 0 }
        );
        assert_eq!(
            span(None, 0).resume(50),
            ResumePoint::Lost { resume_after: 0 }
        );
    }
}
