-- Durable state for the scanner's work units (docs/scanner_microtasks.md).

-- A detected reorg, reconciled across as many rounds as it takes. One per
-- network. While it exists, block scanning on the network is suspended and
-- no order may newly settle (paid/overpaid) on that network.
--
-- The candidate payments are collected before any is processed, in two
-- keyset streams: confirmed payments at or above the fork by
-- (block_height, id), then unconfirmed ones by id. Only payments with
-- id <= candidate_max_id count: anything recorded later came from the
-- mempool (block scans are suspended), and the forward scan after the
-- rewind re-covers the replacement blocks.
CREATE TABLE reorg_jobs (
    network TEXT PRIMARY KEY,
    fork_height INTEGER NOT NULL,
    phase TEXT NOT NULL CHECK (phase IN ('collect_confirmed', 'collect_unconfirmed', 'process')),
    candidate_max_id INTEGER NOT NULL,
    collect_after_height INTEGER NOT NULL,
    collect_after_id INTEGER NOT NULL,
    created_at_utc INTEGER NOT NULL,
    updated_at_utc INTEGER NOT NULL
);

-- One row per payment still to re-examine. A row is deleted in the same
-- transaction that applies its outcome, so a crash replays at most the
-- payment that was in flight, and that replay is idempotent. A failed
-- lookup backs off without holding up the rows behind it.
CREATE TABLE reorg_work (
    network TEXT NOT NULL REFERENCES reorg_jobs(network) ON DELETE CASCADE,
    payment_id INTEGER NOT NULL REFERENCES order_payments(id) ON DELETE CASCADE,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at_utc INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (network, payment_id)
);
CREATE INDEX reorg_work_due_idx ON reorg_work (network, next_attempt_at_utc, payment_id);

-- The two collection streams above.
CREATE INDEX order_payments_confirmed_height_idx ON order_payments (block_height, id)
    WHERE block_height IS NOT NULL;
CREATE INDEX order_payments_unconfirmed_idx ON order_payments (id)
    WHERE block_height IS NULL;

-- When an open order's status can next change without any payment changing:
-- the time it can expire, and the height at which its confirmation count
-- next moves. NULL when nothing but a payment change (which leaves a
-- pending_payment_recomputes row) can change it. Written with every status
-- recompute; the scheduler recomputes whatever is due, earliest first.
ALTER TABLE orders ADD COLUMN next_due_at_utc INTEGER;
ALTER TABLE orders ADD COLUMN next_due_height INTEGER;
CREATE INDEX orders_due_at_idx ON orders (next_due_at_utc, id) WHERE next_due_at_utc IS NOT NULL;
CREATE INDEX orders_due_height_idx ON orders (next_due_height, id) WHERE next_due_height IS NOT NULL;

-- A new order is next due at its deadline; every way of creating one gets it.
CREATE TRIGGER order_insert_schedules_expiry AFTER INSERT ON orders
WHEN NEW.next_due_at_utc IS NULL AND NEW.status IN ('pending', 'unconfirmed', 'confirming', 'partial')
BEGIN
    UPDATE orders SET next_due_at_utc = NEW.expires_at_utc WHERE id = NEW.id;
END;

-- Existing open orders are due now: the first recompute schedules them
-- exactly. Terminal ones stay unscheduled.
UPDATE orders SET next_due_at_utc = 0
WHERE status IN ('pending', 'unconfirmed', 'confirming', 'partial');

-- Rotation positions the scheduler keeps across restarts, so a restart
-- doesn't send every rotation back to its start. A fixed set of keys per
-- network (see `store::work::Position`), never one per tenant.
CREATE TABLE scheduler_positions (
    network TEXT NOT NULL,
    position TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (network, position)
);
