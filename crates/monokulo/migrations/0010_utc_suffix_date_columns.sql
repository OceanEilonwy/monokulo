-- Same reasoning as the engine's own `crates/scanner/migrations/
-- 0009_utc_suffix_date_columns.sql`: every date/datetime column here is,
-- and always has been, a plain unix-second integer, so it was never
-- actually ambiguous - but nothing in a column's own name said so, and that
-- matters now that the advanced-mode rescan window accepts a merchant's
-- own local-timezone date input (converted to UTC before it ever reaches
-- either database - see `templates::date_string_to_unix_midnight`).
-- Suffixing every one of them `_utc` makes that explicit at the schema
-- level instead of leaving it as tribal knowledge.
ALTER TABLE users RENAME COLUMN created_at TO created_at_utc;
ALTER TABLE sessions RENAME COLUMN created_at TO created_at_utc;
ALTER TABLE store_connections RENAME COLUMN created_at TO created_at_utc;
ALTER TABLE connect_tokens RENAME COLUMN created_at TO created_at_utc;
ALTER TABLE connect_tokens RENAME COLUMN consumed_at TO consumed_at_utc;
ALTER TABLE order_currency_metadata RENAME COLUMN created_at TO created_at_utc;
