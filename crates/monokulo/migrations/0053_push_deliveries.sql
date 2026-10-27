-- Pushes to connected apps' push targets (`crate::push::delivery`): one row
-- per sale event and target that wants it, queued in the same transaction
-- as the event's webhook deliveries (`Db::queue_order_event`), or as the
-- order an app made (`sale.waiting`). New tables and one new column; no
-- existing row changes.
--
-- `event_seq`: the event's place in the engine's order-event log, 0 for
-- `sale.waiting` (the order API makes it; the log never has it).
-- `event_type`: the sale event (`sale.paid`). `topic`: the RFC 8030 `Topic`
-- the push collapses under, one per order. `urgency`: its `Urgency` header.
-- `payload`: the plaintext JSON, encrypted afresh at each attempt (RFC 8291
-- wants a fresh salt and key for every message), or sent as it is to a
-- `live-activity` target. `expires_at_utc`: when its TTL ends; it is never
-- sent after that. A row is waiting while `next_attempt_at_utc` is set;
-- then delivered, given up on, or superseded: a newer push for the same
-- target and topic replaced it before it was sent. Deleting the target
-- deletes its rows.
CREATE TABLE push_deliveries (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    push_target_id      TEXT NOT NULL REFERENCES push_targets(id) ON DELETE CASCADE,
    event_seq           INTEGER NOT NULL,
    event_id            TEXT NOT NULL,
    event_type          TEXT NOT NULL,
    order_id            TEXT NOT NULL,
    topic               TEXT NOT NULL,
    urgency             TEXT NOT NULL CHECK (urgency IN ('very-low', 'low', 'normal', 'high')),
    payload             TEXT NOT NULL,
    expires_at_utc      INTEGER NOT NULL,
    created_at_utc      INTEGER NOT NULL,
    attempt_count       INTEGER NOT NULL DEFAULT 0,
    next_attempt_at_utc INTEGER,
    last_attempt_at_utc INTEGER,
    last_status_code    INTEGER,
    last_error          TEXT,
    delivered_at_utc    INTEGER,
    gave_up_at_utc      INTEGER,
    superseded_at_utc   INTEGER,
    UNIQUE (push_target_id, event_id)
);
CREATE INDEX push_deliveries_due ON push_deliveries (next_attempt_at_utc)
    WHERE next_attempt_at_utc IS NOT NULL;
CREATE INDEX push_deliveries_topic ON push_deliveries (push_target_id, topic)
    WHERE next_attempt_at_utc IS NOT NULL;

-- The latest sale event of an app's order, for the Live Activity's
-- `waitingCount` (the store's app orders still waiting for a payment).
-- NULL until one is queued: an order made before this migration has
-- expired long since, and one made after starts at `sale.waiting`.
ALTER TABLE app_orders ADD COLUMN last_sale_event TEXT
    CHECK (last_sale_event IN ('sale.waiting', 'sale.seen', 'sale.confirming', 'sale.paid',
                               'sale.partial', 'sale.expired', 'sale.double_spend'));
