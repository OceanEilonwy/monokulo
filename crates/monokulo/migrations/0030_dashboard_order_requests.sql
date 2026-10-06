CREATE TABLE dashboard_order_requests (
    connection_id TEXT NOT NULL REFERENCES store_connections(id) ON DELETE CASCADE,
    request_key TEXT NOT NULL,
    order_id TEXT NOT NULL,
    PRIMARY KEY (connection_id, request_key)
);
