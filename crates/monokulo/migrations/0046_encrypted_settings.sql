-- Secret settings the admin page may set (only the CoinMarketCap API key,
-- `exchange_rate.coinmarketcap_api_key`), each encrypted with
-- MONOKULO_ENCRYPTION_KEY and bound to its key (`settings::DbSettings`).
-- Kept apart from `settings` so the Query page can hide the column.
CREATE TABLE encrypted_settings (
    key TEXT PRIMARY KEY NOT NULL,
    value_encrypted TEXT NOT NULL
);
