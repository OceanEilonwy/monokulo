-- foreign_keys: off
-- Changing a store's wallet (docs/wallets.md, "Changing a store's wallet").
--
-- A store's orders keep their store (`tenant_id`): the API, webhooks and
-- settings all go by it. What changes is which keys watch an order. Each
-- order now records the wallet its address came from (`wallet_id`) and the
-- tenant row whose keys the scanner watches it with (`scan_tenant_id`).
-- That row is the store itself until the store changes wallet; then the
-- orders it took on the old wallet move to a scan-only row (`watches_for`
-- set to the store) holding the old wallet's keys, which the scanner
-- treats like any other tenant: its own cursor, its own handle. So orders
-- still open, or closed within the grace period, keep being paid into the
-- old wallet and are seen.
--
-- `UNIQUE (tenant_id, minor_index)` can't stay: a store's orders on two
-- wallets can share a minor index, each wallet counting from its own
-- counter. An address is a wallet and an index; the scanner finds an order
-- by the row it scanned for and the index, which stay unique. SQLite can
-- only change a table's constraints by rebuilding it, hence foreign keys
-- off for this migration (`shared::migrations::FOREIGN_KEYS_OFF`).

ALTER TABLE tenants ADD COLUMN watches_for TEXT REFERENCES tenants(id);
-- When the store last changed wallet. A scan round that began before then
-- used the old keys for the store; what it found is not recorded against
-- the store (the scan-only row looks at those blocks again with the old
-- keys), so an old wallet's payment can't be credited to a new order that
-- happens to share its index.
ALTER TABLE tenants ADD COLUMN wallet_changed_at_utc INTEGER;

CREATE TABLE orders_new (
    id                       TEXT PRIMARY KEY,
    tenant_id                TEXT NOT NULL REFERENCES tenants(id),
    merchant_order_id        TEXT,
    minor_index              INTEGER NOT NULL,
    address                  TEXT NOT NULL,
    xmr_amount_piconero      INTEGER NOT NULL,
    amount_received_piconero INTEGER NOT NULL DEFAULT 0,
    status                   TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'unconfirmed', 'confirming', 'paid', 'partial', 'overpaid', 'expired')),
    confirmations            INTEGER NOT NULL DEFAULT 0,
    double_spend_detected_at_utc INTEGER,
    refund_address           TEXT,
    description              TEXT,
    created_at_utc           INTEGER NOT NULL,
    expires_at_utc           INTEGER NOT NULL,
    updated_at_utc           INTEGER NOT NULL,
    first_scanned_height     INTEGER,
    last_scanned_height      INTEGER,
    confirmations_required_override INTEGER,
    closed_at_utc            INTEGER,
    next_due_at_utc          INTEGER,
    next_due_height          INTEGER,
    idempotency_key          TEXT,
    wallet_id                TEXT REFERENCES wallets(id),
    scan_tenant_id           TEXT NOT NULL REFERENCES tenants(id),
    UNIQUE (scan_tenant_id, minor_index)
);

INSERT INTO orders_new
    (id, tenant_id, merchant_order_id, minor_index, address, xmr_amount_piconero,
     amount_received_piconero, status, confirmations, double_spend_detected_at_utc,
     refund_address, description, created_at_utc, expires_at_utc, updated_at_utc,
     first_scanned_height, last_scanned_height, confirmations_required_override,
     closed_at_utc, next_due_at_utc, next_due_height, idempotency_key,
     wallet_id, scan_tenant_id)
SELECT o.id, o.tenant_id, o.merchant_order_id, o.minor_index, o.address, o.xmr_amount_piconero,
       o.amount_received_piconero, o.status, o.confirmations, o.double_spend_detected_at_utc,
       o.refund_address, o.description, o.created_at_utc, o.expires_at_utc, o.updated_at_utc,
       o.first_scanned_height, o.last_scanned_height, o.confirmations_required_override,
       o.closed_at_utc, o.next_due_at_utc, o.next_due_height, o.idempotency_key,
       (SELECT t.wallet_id FROM tenants t WHERE t.id = o.tenant_id), o.tenant_id
FROM orders o;

DROP TABLE orders;
ALTER TABLE orders_new RENAME TO orders;

-- Dropping the old table dropped its indexes and triggers with it.
CREATE INDEX orders_tenant_status_idx ON orders (tenant_id, status);
CREATE INDEX orders_tenant_merchant_order_idx ON orders (tenant_id, merchant_order_id);
CREATE INDEX orders_status_tenant_idx ON orders (status, tenant_id);
CREATE INDEX orders_due_at_idx ON orders (next_due_at_utc, id) WHERE next_due_at_utc IS NOT NULL;
CREATE INDEX orders_due_height_idx ON orders (next_due_height, id) WHERE next_due_height IS NOT NULL;
CREATE INDEX orders_tenant_closed_idx ON orders (tenant_id, closed_at_utc)
    WHERE closed_at_utc IS NOT NULL;
CREATE UNIQUE INDEX orders_idempotency_key_idx
    ON orders (tenant_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
-- The scanner's: a scan row's orders in its window (`tenant_in_scope`,
-- `scan_window_orders`), each half from its own index.
CREATE INDEX orders_scan_tenant_status_idx ON orders (scan_tenant_id, status);
CREATE INDEX orders_scan_tenant_closed_idx ON orders (scan_tenant_id, closed_at_utc)
    WHERE closed_at_utc IS NOT NULL;
-- Deleting a wallet: refused while an order on it could still be paid.
CREATE INDEX orders_wallet_status_idx ON orders (wallet_id, status);
CREATE INDEX orders_wallet_closed_idx ON orders (wallet_id, closed_at_utc)
    WHERE closed_at_utc IS NOT NULL;

CREATE TRIGGER order_insert_schedules_expiry AFTER INSERT ON orders
WHEN NEW.next_due_at_utc IS NULL AND NEW.status IN ('pending', 'unconfirmed', 'confirming', 'partial')
BEGIN
    UPDATE orders SET next_due_at_utc = NEW.expires_at_utc WHERE id = NEW.id;
END;

CREATE TRIGGER order_deadline_change_reschedules AFTER UPDATE OF expires_at_utc ON orders
WHEN NEW.status IN ('pending', 'unconfirmed', 'confirming', 'partial')
BEGIN
    UPDATE orders SET next_due_at_utc = 0 WHERE id = NEW.id;
END;
