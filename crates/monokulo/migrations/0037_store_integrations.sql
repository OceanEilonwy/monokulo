-- A plugin connected to a store (docs/wallets.md, "Store site and
-- integrations"): today the WooCommerce plugin. A row is written when the
-- plugin's connect flow finishes (`/connect/{plugin}/finish`), and it is
-- active until disconnected: while one is, the store's site is the
-- plugin's shop and can't be changed. Connecting again adds a new row.
--
-- `kind`: the plugin ('woocommerce'). `site`: the store's site (a host) when
-- it connected. `version`: the plugin's, from its `Monokulo-Client` header
-- (`woocommerce/<version>`), at connect time and on each order it makes.
-- `webhook_id`, `webhook_url`: the paid webhook it registered when it
-- connected, removed when it is disconnected. `last_seen_at_utc`: its last
-- order. `disconnected_at_utc`: NULL while it is active.
CREATE TABLE store_integrations (
    id                  TEXT PRIMARY KEY,
    store_id            TEXT NOT NULL REFERENCES store_connections(id) ON DELETE CASCADE,
    kind                TEXT NOT NULL,
    site                TEXT NOT NULL,
    version             TEXT NOT NULL DEFAULT '',
    webhook_id          TEXT,
    webhook_url         TEXT,
    connected_at_utc    INTEGER NOT NULL,
    last_seen_at_utc    INTEGER,
    disconnected_at_utc INTEGER
);
CREATE INDEX store_integrations_store ON store_integrations (store_id, connected_at_utc);
