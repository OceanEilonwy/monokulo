-- What a connected plugin calls itself ("PrestaShop"), as it said when it
-- connected (`crate::plugins`): monokulo keeps no list of plugins, so the
-- name comes from the plugin. '' when it gave none, and the dashboard shows
-- its kind with a capital first letter instead.
--
-- Rows from before plugins named themselves were all the WooCommerce
-- plugin's, which keeps its name.
ALTER TABLE store_integrations ADD COLUMN name TEXT NOT NULL DEFAULT '';
UPDATE store_integrations SET name = 'WooCommerce' WHERE kind = 'woocommerce';

-- A store whose WooCommerce plugin connected before integrations were
-- recorded (0037 added none for them) but has made orders since: its
-- plugin, connected from its first such order. A plugin's orders are now
-- its only while it is the store's integration, so without this row they
-- would read as the Store API's. Its webhook, if any, is the store's own
-- to manage.
INSERT INTO store_integrations (id, store_id, kind, name, site, connected_at_utc, last_seen_at_utc)
SELECT 'int_' || lower(hex(randomblob(16))), s.id, 'woocommerce', 'WooCommerce', s.site,
       MIN(m.created_at_utc), MAX(m.created_at_utc)
FROM store_connections s
JOIN order_currency_metadata m ON m.connection_id = s.id AND m.source = 'woocommerce'
WHERE NOT EXISTS (SELECT 1 FROM store_integrations i WHERE i.store_id = s.id)
GROUP BY s.id;
