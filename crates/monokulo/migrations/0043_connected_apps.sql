-- Connected apps (monokular docs/design.md §2): devices such as a till
-- running Monokular, each with a key of its own for one store. Unlike a
-- shop plugin (`store_integrations`), a store has any number of them, and
-- they connect from the dashboard rather than from a shop.
--
-- `slug` and `name`: what the app calls itself, with a plugin's rules
-- (`crate::plugins`); monokulo keeps no list of apps. `device_name`: what the
-- merchant (or, when they gave none, the app) calls the device. `key_hash`:
-- SHA-256 of the app's `ak_` key, never the key itself. `version`: from its
-- `Monokulo-Client` header, at connect time and on each order it makes.
-- Disconnecting deletes the row.
CREATE TABLE app_connections (
    id               TEXT PRIMARY KEY,
    store_id         TEXT NOT NULL REFERENCES store_connections(id) ON DELETE CASCADE,
    slug             TEXT NOT NULL,
    name             TEXT NOT NULL DEFAULT '',
    device_name      TEXT NOT NULL DEFAULT '',
    platform         TEXT NOT NULL DEFAULT '',
    version          TEXT NOT NULL DEFAULT '',
    key_hash         TEXT NOT NULL UNIQUE,
    connected_at_utc INTEGER NOT NULL,
    last_seen_at_utc INTEGER
);
CREATE INDEX app_connections_store ON app_connections (store_id, connected_at_utc);

-- Single-use codes the dashboard shows to connect an app, the same model as
-- `connect_tokens`: SHA-256 of the raw `ct_` token, spent exactly once by
-- an atomic UPDATE. `app_id` is the connection it made, so the page that
-- showed the code can say it worked.
CREATE TABLE app_connect_tokens (
    token_hash      TEXT PRIMARY KEY,
    store_id        TEXT NOT NULL REFERENCES store_connections(id) ON DELETE CASCADE,
    device_name     TEXT NOT NULL DEFAULT '',
    created_at_utc  INTEGER NOT NULL,
    consumed_at_utc INTEGER,
    app_id          TEXT
);

-- Orders a connected app made, and what its event stream tells the store's
-- apps about them: no items, only what the order API returned. The app that
-- made it may since have been disconnected, so `app_id` isn't a foreign key.
CREATE TABLE app_orders (
    order_id            TEXT PRIMARY KEY,
    store_id            TEXT NOT NULL REFERENCES store_connections(id) ON DELETE CASCADE,
    app_id              TEXT NOT NULL,
    device_name         TEXT NOT NULL DEFAULT '',
    amount              TEXT NOT NULL,
    currency            TEXT NOT NULL,
    address             TEXT NOT NULL,
    xmr_amount_piconero INTEGER NOT NULL,
    merchant_order_id   TEXT,
    expires_at          INTEGER NOT NULL,
    created_at_utc      INTEGER NOT NULL
);
CREATE INDEX app_orders_store ON app_orders (store_id, created_at_utc);
