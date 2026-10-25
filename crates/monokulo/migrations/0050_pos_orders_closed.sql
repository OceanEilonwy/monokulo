-- When the engine stopped watching a POS order (`closed_at_utc`): it can't
-- change any more, so the terminal's Active tab no longer asks the engine
-- about it. Every existing row starts open (NULL); the first Active-tab read
-- of each store asks the engine about them once and closes those it has
-- finished with.
ALTER TABLE pos_orders ADD COLUMN closed_at_utc INTEGER;
CREATE INDEX pos_orders_unclosed ON pos_orders(connection_id, created_at_utc DESC, order_id DESC)
    WHERE closed_at_utc IS NULL;
