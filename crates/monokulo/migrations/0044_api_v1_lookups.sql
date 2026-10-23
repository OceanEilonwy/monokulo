-- What the machine API (`/api/v1`, `http::api`) and the payer's pages
-- (`/checkout/{order_id}`) look stores up by, now that no `pk_` is in
-- their URLs.
--
-- `secret_key_hash`: SHA-256 of the store's `sk_` key
-- (`shared::auth::RawToken::hash`), the same lookup an app's `ak_` key
-- has (`app_connections.key_hash`). The key itself stays encrypted in
-- `tenant_secret_token_encrypted`. NULL until monokulo fills it at start
-- (`crate::stores::fill_secret_key_hashes`): the hash needs the key, and
-- the key needs the encryption key, which SQL doesn't have.
ALTER TABLE store_connections ADD COLUMN secret_key_hash TEXT;
CREATE UNIQUE INDEX store_connections_secret_key_hash ON store_connections (secret_key_hash);

-- An order id is unique on its own (the engine's primary key), so a
-- checkout finds its store from the order alone.
CREATE INDEX order_currency_metadata_order ON order_currency_metadata (order_id);
