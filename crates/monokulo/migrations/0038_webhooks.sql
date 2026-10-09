-- foreign_keys: off
-- Monokulo delivers stores' webhooks itself (docs/DESIGN.md §11): the
-- engine only keeps a log of order events, which monokulo reads
-- (`crate::webhooks::subscriber`) and turns into deliveries here.
--
-- `webhooks`: a store's endpoints. `signing_secret_encrypted`: the secret
-- each delivery's `X-Monokulo-Signature` is made with, encrypted at rest
-- like the store's own secret key (`crate::crypto`, bound to the webhook's
-- id); shown to the merchant once, when the webhook is made.
-- `extra_headers`: a JSON object of headers sent with every delivery.
CREATE TABLE webhooks (
    id                       TEXT PRIMARY KEY,
    store_id                 TEXT NOT NULL REFERENCES store_connections(id) ON DELETE CASCADE,
    url                      TEXT NOT NULL,
    signing_secret_encrypted TEXT NOT NULL,
    extra_headers            TEXT NOT NULL DEFAULT '{}',
    enabled                  INTEGER NOT NULL DEFAULT 1,
    created_at_utc           INTEGER NOT NULL
);
CREATE INDEX webhooks_store ON webhooks (store_id, created_at_utc);

-- One event for one webhook: its body (made once, so every attempt sends
-- the same bytes under a fresh signature) and how its attempts went.
-- Waiting while `next_attempt_at_utc` is set; delivered when
-- `delivered_at_utc` is, given up on (after the last attempt, never retried
-- by itself) when `gave_up_at_utc` is. `event_seq`: the event's position in
-- the engine's log. `attempts_json`: the latest attempts, newest last, at
-- most 20, each `{"n", "at", "status", "error", "ms", "signature"}`; kept
-- here rather than in a table of their own because they're only ever read
-- with their delivery, are written in the same statement as its outcome,
-- and go with it. `last_response`: the start of the last answer (its
-- status line, content type and first 512 bytes), shown as text.
CREATE TABLE webhook_deliveries (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    webhook_id          TEXT NOT NULL REFERENCES webhooks(id) ON DELETE CASCADE,
    event_seq           INTEGER NOT NULL,
    event_id            TEXT NOT NULL,
    event_type          TEXT NOT NULL,
    order_id            TEXT NOT NULL,
    body                TEXT NOT NULL,
    created_at_utc      INTEGER NOT NULL,
    attempt_count       INTEGER NOT NULL DEFAULT 0,
    next_attempt_at_utc INTEGER,
    last_attempt_at_utc INTEGER,
    last_status_code    INTEGER,
    last_error          TEXT,
    last_duration_ms    INTEGER,
    last_response       TEXT,
    delivered_at_utc    INTEGER,
    gave_up_at_utc      INTEGER,
    attempts_json       TEXT NOT NULL DEFAULT '[]',
    UNIQUE (webhook_id, event_id)
);
CREATE INDEX webhook_deliveries_due ON webhook_deliveries (next_attempt_at_utc)
    WHERE next_attempt_at_utc IS NOT NULL;
CREATE INDEX webhook_deliveries_by_webhook ON webhook_deliveries (webhook_id, id);

-- Where monokulo has read the engine's order-event log up to: the last
-- event whose deliveries are queued, saved in the same transaction as them.
-- One row.
CREATE TABLE order_event_position (
    id             INTEGER PRIMARY KEY CHECK (id = 1),
    after_seq      INTEGER NOT NULL,
    updated_at_utc INTEGER NOT NULL
);

-- A plugin's webhook is now one of these: `store_integrations.webhook_id`
-- names it (cleared when it's deleted). The engine's webhooks it used to
-- name are gone.
CREATE TABLE store_integrations_new (
    id                  TEXT PRIMARY KEY,
    store_id            TEXT NOT NULL REFERENCES store_connections(id) ON DELETE CASCADE,
    kind                TEXT NOT NULL,
    site                TEXT NOT NULL,
    version             TEXT NOT NULL DEFAULT '',
    webhook_id          TEXT REFERENCES webhooks(id) ON DELETE SET NULL,
    webhook_url         TEXT,
    connected_at_utc    INTEGER NOT NULL,
    last_seen_at_utc    INTEGER,
    disconnected_at_utc INTEGER
);
INSERT INTO store_integrations_new
    (id, store_id, kind, site, version, webhook_id, webhook_url, connected_at_utc,
     last_seen_at_utc, disconnected_at_utc)
SELECT id, store_id, kind, site, version, NULL, webhook_url, connected_at_utc,
       last_seen_at_utc, disconnected_at_utc
FROM store_integrations;
DROP TABLE store_integrations;
ALTER TABLE store_integrations_new RENAME TO store_integrations;
CREATE INDEX store_integrations_store ON store_integrations (store_id, connected_at_utc);
