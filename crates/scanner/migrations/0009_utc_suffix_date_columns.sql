-- Every date/datetime column in this database is, and always has been, a
-- plain unix-second integer - inherently unambiguous, timezone-wise, since
-- unix time has no timezone concept to begin with. But nothing in a column's
-- own name said so, which matters now that the advanced-mode rescan window
-- (`docs/order_rescan_wbs.md`) accepts merchant-supplied dates from a
-- browser's own local timezone (converted to UTC before it ever reaches
-- this database - see `monokulo::templates::date_string_to_unix_midnight`).
-- Suffixing every one of them `_utc` makes that explicit at the schema level
-- rather than leaving it as tribal knowledge - a future column, or a future
-- reader of a raw `sqlite3` prompt, should never have to guess. Block
-- heights (`from_height`/`to_height`/`current_height`/`first_scanned_height`/
-- `last_scanned_height`) are deliberately untouched - they're chain
-- positions, not points in time, and were never ambiguous to begin with.
ALTER TABLE tenants RENAME COLUMN created_at TO created_at_utc;
ALTER TABLE tenants RENAME COLUMN disabled_at TO disabled_at_utc;

ALTER TABLE orders RENAME COLUMN created_at TO created_at_utc;
ALTER TABLE orders RENAME COLUMN expires_at TO expires_at_utc;
ALTER TABLE orders RENAME COLUMN updated_at TO updated_at_utc;
ALTER TABLE orders RENAME COLUMN double_spend_detected_at TO double_spend_detected_at_utc;

ALTER TABLE order_payments RENAME COLUMN first_seen_at TO first_seen_at_utc;
ALTER TABLE order_payments RENAME COLUMN voided_at TO voided_at_utc;

ALTER TABLE webhooks RENAME COLUMN created_at TO created_at_utc;

-- SQLite's RENAME COLUMN rewrites every reference to the old name in other
-- objects that depend on it (indexes, triggers, views) automatically,
-- including the partial index below - `webhook_deliveries_due_idx ...
-- WHERE delivered_at IS NULL` continues to exist and function unchanged,
-- now reading `delivered_at_utc`. Verified by this migration's own
-- reopening/round-trip test in store.rs, not just assumed from the SQLite
-- docs.
ALTER TABLE webhook_deliveries RENAME COLUMN next_attempt_at TO next_attempt_at_utc;
ALTER TABLE webhook_deliveries RENAME COLUMN delivered_at TO delivered_at_utc;
ALTER TABLE webhook_deliveries RENAME COLUMN last_attempted_at TO last_attempted_at_utc;

ALTER TABLE order_rescans RENAME COLUMN started_at TO started_at_utc;
ALTER TABLE order_rescans RENAME COLUMN finished_at TO finished_at_utc;
ALTER TABLE order_rescans RENAME COLUMN updated_at TO updated_at_utc;
