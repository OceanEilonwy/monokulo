-- What a connected plugin calls itself ("PrestaShop"), as it said when it
-- connected (`crate::plugins`): monokulo keeps no list of plugins, so the
-- name comes from the plugin. '' when it gave none, and the dashboard shows
-- its kind with a capital first letter instead.
--
-- Rows from before plugins named themselves were all the WooCommerce
-- plugin's, which keeps its name.
ALTER TABLE store_integrations ADD COLUMN name TEXT NOT NULL DEFAULT '';
UPDATE store_integrations SET name = 'WooCommerce' WHERE kind = 'woocommerce';
