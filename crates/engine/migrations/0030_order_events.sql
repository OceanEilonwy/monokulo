-- The order-event log (docs/DESIGN.md §11): every event a store's
-- webhooks announce, written in the same transaction as the change it
-- announces, and read by monokulo over `GET /api/v1/admin/order-events`
-- (server-sent events), which delivers the store's webhooks itself.
--
-- `seq`: the event's position in the log, and its SSE `id:`. AUTOINCREMENT
-- so a number is never handed out twice, even after the newest rows are
-- pruned: `sqlite_sequence` keeps the highest ever used, which is how a
-- reader asking from before the oldest kept row is told it missed some.
-- `event_id`: `evt_` + a UUID, the id a webhook carries, the same on every
-- retry of its delivery. `tenant_id`: the store. `event_type`:
-- `order.<status>`, `order.double_spend_detected` or
-- `order.double_spend_reversed`. `payload_json`: the event's own fields
-- (`order_id`, `merchant_order_id`, `xmr_amount_piconero`, `status` on a
-- status change, `txid` on a reversal). Kept for
-- `order_events.retention_days` (seven by default), then pruned by the
-- scanner's upkeep tier.
CREATE TABLE order_events (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id       TEXT NOT NULL UNIQUE,
    tenant_id      TEXT NOT NULL REFERENCES tenants(id),
    order_id       TEXT NOT NULL REFERENCES orders(id),
    event_type     TEXT NOT NULL,
    payload_json   TEXT NOT NULL,
    created_at_utc INTEGER NOT NULL
);
CREATE INDEX order_events_created_idx ON order_events (created_at_utc);
