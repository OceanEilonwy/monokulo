-- Indexes for the scanner's hot queries (docs/scanner_microtasks.md). Each
-- is checked by a query-plan test (store::work::tests).

-- The "recently closed" half of the scan window, per tenant. The "open"
-- half is served by orders_tenant_status_idx.
CREATE INDEX orders_tenant_closed_idx ON orders (tenant_id, closed_at_utc)
    WHERE closed_at_utc IS NOT NULL;

-- Tenant cursors by network: catch-up groups, their members, idle moves.
CREATE INDEX tenants_network_cursor_idx ON tenants (network, scanned_through_height)
    WHERE disabled_at_utc IS NULL;

-- Voided payments in id order: the recent-void recheck's pages.
CREATE INDEX order_payments_voided_idx ON order_payments (id)
    WHERE voided_at_utc IS NOT NULL;
