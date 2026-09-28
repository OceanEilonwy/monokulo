-- A payment write can outlive the tick that would recompute its order.
-- Keep that obligation even for closed orders outside the live scan window.
CREATE TABLE pending_payment_recomputes (
    order_id TEXT PRIMARY KEY REFERENCES orders(id) ON DELETE CASCADE
);

CREATE TRIGGER payment_insert_needs_recompute AFTER INSERT ON order_payments
BEGIN
    INSERT INTO pending_payment_recomputes(order_id)
    SELECT NEW.order_id WHERE NOT EXISTS (
        SELECT 1 FROM pending_payment_recomputes WHERE order_id = NEW.order_id
    );
END;

CREATE TRIGGER payment_update_needs_recompute AFTER UPDATE ON order_payments
WHEN OLD.block_height IS NOT NEW.block_height
  OR OLD.voided_at_utc IS NOT NEW.voided_at_utc
  OR OLD.amount_piconero IS NOT NEW.amount_piconero
BEGIN
    -- Avoid a conflict altogether: the outer payment UPSERT can override a
    -- trigger's INSERT OR IGNORE policy when the order is already pending.
    INSERT INTO pending_payment_recomputes(order_id)
    SELECT NEW.order_id WHERE NOT EXISTS (
        SELECT 1 FROM pending_payment_recomputes WHERE order_id = NEW.order_id
    );
END;

-- Repair any interrupted recomputes from before this migration as well.
INSERT OR IGNORE INTO pending_payment_recomputes(order_id)
SELECT DISTINCT order_id FROM order_payments;
