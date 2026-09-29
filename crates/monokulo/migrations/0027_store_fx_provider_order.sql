-- A store now keeps an ORDERED list of exchange-rate providers instead of a
-- single one: the first that is available and has a rate for the order's
-- currency prices the order (`exchange_rate_config::ExchangeRateProviders::
-- piconero_per_unit_for`). A provider absent from the list is off for that
-- store. The column is a JSON array of provider names, most preferred first.
--
-- Renamed in place (existing single value -> one-element list) rather than
-- adding a column beside it, so there is no second source of truth. The old
-- column's `DEFAULT 'fixed'` survives the rename; nothing relies on it
-- (`Db::create_store_connection` writes the list explicitly, and the reader
-- also tolerates a bare name).
ALTER TABLE store_connections RENAME COLUMN fx_provider TO fx_providers;
UPDATE store_connections SET fx_providers = json_array(fx_providers);
