-- One row per idempotency key a store's orders were asked with, whichever
-- way they were made (`crate::orders`): the API with the public key or a
-- store key, the dashboard's form, the till. `idempotency_key` is the key
-- as the engine has it, with its prefix (`pay-pk:`, `pay:`, `dash:`,
-- `pos:`). `fingerprint` is what was asked for (the amount as sent, the
-- currency, the reference, the description, whether the reference shows):
-- the same key for anything else is refused. The price is the one the
-- first attempt was made at, so a retry after a lost answer asks the
-- engine for exactly the same order however the rate moved since.
-- `order_id` is set together with the order's own record, so a key that
-- names an order always names a recorded one.
CREATE TABLE order_requests (
    connection_id TEXT NOT NULL REFERENCES store_connections(id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    xmr_amount_piconero INTEGER NOT NULL,
    piconero_per_unit INTEGER NOT NULL,
    provider TEXT NOT NULL,
    store_base_currency TEXT NOT NULL,
    base_currency_piconero_per_unit INTEGER,
    confirmations_required INTEGER NOT NULL,
    reserved_at_utc INTEGER NOT NULL,
    order_id TEXT,
    PRIMARY KEY (connection_id, idempotency_key)
);

-- The dashboard's and the till's own key records go: `order_requests`
-- holds every surface's. A retry of a request made before this migration
-- still gets its order from the engine, which keeps every key, while the
-- rate is unchanged.
DROP TABLE dashboard_order_requests;

-- `pos_orders` without its `request_key`. SQLite can't drop a column that
-- is part of a UNIQUE constraint, so the table is copied.
CREATE TABLE pos_orders_without_keys (
    connection_id TEXT NOT NULL REFERENCES store_connections(id) ON DELETE CASCADE,
    order_id TEXT NOT NULL,
    reference TEXT,
    backgrounded INTEGER NOT NULL DEFAULT 0,
    created_at_utc INTEGER NOT NULL,
    PRIMARY KEY (connection_id, order_id)
);
INSERT INTO pos_orders_without_keys (connection_id, order_id, reference, backgrounded, created_at_utc)
    SELECT connection_id, order_id, reference, backgrounded, created_at_utc FROM pos_orders;
DROP TABLE pos_orders;
ALTER TABLE pos_orders_without_keys RENAME TO pos_orders;
CREATE INDEX pos_orders_recent ON pos_orders(connection_id, created_at_utc DESC, order_id DESC);
-- As migration 0045 left it.
CREATE TRIGGER pos_order_changed AFTER UPDATE OF backgrounded ON pos_orders
WHEN OLD.backgrounded IS NOT NEW.backgrounded
BEGIN
    SELECT pos_order_changed(NEW.connection_id, NEW.order_id);
END;
