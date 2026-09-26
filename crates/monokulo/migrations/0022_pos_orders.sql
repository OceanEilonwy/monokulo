-- POS-specific membership and merchant workflow state. The engine remains the
-- source of truth for payment status and financial amounts.
CREATE TABLE pos_orders (
    connection_id TEXT NOT NULL REFERENCES store_connections(id) ON DELETE CASCADE,
    order_id TEXT NOT NULL,
    request_key TEXT,
    reference TEXT,
    backgrounded INTEGER NOT NULL DEFAULT 0,
    cancelled_at_utc INTEGER,
    created_at_utc INTEGER NOT NULL,
    PRIMARY KEY (connection_id, order_id),
    UNIQUE (connection_id, request_key)
);
CREATE INDEX pos_orders_recent ON pos_orders(connection_id, created_at_utc DESC, order_id DESC);
