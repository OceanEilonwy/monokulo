-- A store has no platform or kind: any store can take payments at the
-- till, show the checkout on its site and connect the WooCommerce plugin,
-- all at once. It is a name and, optionally, a site. Where an order came
-- from is the order's own (`order_currency_metadata.source`).
ALTER TABLE store_connections DROP COLUMN platform;
