-- Whether this store has opted in to client logs (structured_logging.md,
-- follow-up): browser problem reports from its dashboard pages and its
-- checkout, the POS session timeline, and the WooCommerce plugin's
-- forwarded errors. Off by default; while off, all of them are dropped on
-- arrival whatever the client or the plugin is set to.
ALTER TABLE store_connections ADD COLUMN client_logging INTEGER NOT NULL DEFAULT 0;
