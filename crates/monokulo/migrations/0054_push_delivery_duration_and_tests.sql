-- Test pushes and how long each push took (the store's app page,
-- `views::app_page`). Rebuilds `push_deliveries`, copying every row: it
-- changes the data of an existing table's rows (none are lost or altered,
-- but the table is replaced).
--
-- `order_id` becomes nullable: a test push, sent from the store's app page
-- ("Send a test push"), carries no order, and its row says so with NULL
-- rather than a made-up id (its `event_id` is `test_…`, its `event_seq` 0).
-- `last_duration_ms`: how long the last attempt took, from the request to
-- the push service's answer (NULL before any attempt, and for one made
-- before this migration), as `webhook_deliveries.last_duration_ms` is.
CREATE TABLE push_deliveries_with_tests (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    push_target_id      TEXT NOT NULL REFERENCES push_targets(id) ON DELETE CASCADE,
    event_seq           INTEGER NOT NULL,
    event_id            TEXT NOT NULL,
    event_type          TEXT NOT NULL,
    order_id            TEXT,
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
    last_duration_ms    INTEGER,
    delivered_at_utc    INTEGER,
    gave_up_at_utc      INTEGER,
    superseded_at_utc   INTEGER,
    UNIQUE (push_target_id, event_id)
);
INSERT INTO push_deliveries_with_tests
    (id, push_target_id, event_seq, event_id, event_type, order_id, topic, urgency, payload,
     expires_at_utc, created_at_utc, attempt_count, next_attempt_at_utc, last_attempt_at_utc,
     last_status_code, last_error, delivered_at_utc, gave_up_at_utc, superseded_at_utc)
    SELECT id, push_target_id, event_seq, event_id, event_type, order_id, topic, urgency, payload,
           expires_at_utc, created_at_utc, attempt_count, next_attempt_at_utc, last_attempt_at_utc,
           last_status_code, last_error, delivered_at_utc, gave_up_at_utc, superseded_at_utc
    FROM push_deliveries;
-- The ids go on from where the old table's were, pruned rows' included:
-- an id is never handed out twice (a page's `?tested=<id>` names one push).
DELETE FROM sqlite_sequence WHERE name = 'push_deliveries_with_tests';
INSERT INTO sqlite_sequence (name, seq)
    SELECT 'push_deliveries_with_tests', seq FROM sqlite_sequence WHERE name = 'push_deliveries';
DROP TABLE push_deliveries;
ALTER TABLE push_deliveries_with_tests RENAME TO push_deliveries;
CREATE INDEX push_deliveries_due ON push_deliveries (next_attempt_at_utc)
    WHERE next_attempt_at_utc IS NOT NULL;
CREATE INDEX push_deliveries_topic ON push_deliveries (push_target_id, topic)
    WHERE next_attempt_at_utc IS NOT NULL;
-- The app page's Recent pushes: a target's newest first.
CREATE INDEX push_deliveries_recent ON push_deliveries (push_target_id, created_at_utc);
