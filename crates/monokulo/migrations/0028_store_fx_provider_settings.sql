-- Per-store settings for exchange-rate providers that have their own knobs
-- (today: how wide a spread and how thin an order book a store will accept a
-- Haveno quote from, and which currencies Haveno may quote for it). A JSON
-- object keyed by provider; '{}' means every setting at its default, so
-- existing stores need no backfill. See `crate::fx_provider_settings`.
ALTER TABLE store_connections ADD COLUMN fx_provider_settings TEXT NOT NULL DEFAULT '{}';
