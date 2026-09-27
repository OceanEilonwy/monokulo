-- Where an order was created, for the store's orders list: 'pos' (the POS),
-- 'dashboard' (Create an order), 'api' (a shop's server with the secret
-- key, e.g. the WooCommerce plugin) or 'website' (a browser page through
-- monokulo-client.js without the key). NULL for orders from before this
-- column existed, shown as unknown.
ALTER TABLE order_currency_metadata ADD COLUMN source TEXT;
