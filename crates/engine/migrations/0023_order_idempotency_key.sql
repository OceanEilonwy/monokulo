-- The caller's key for one purchase: a request that repeats it (a retry
-- after a timeout or a lost answer) gets the order the first one made,
-- never a second order with a second address.
ALTER TABLE orders ADD COLUMN idempotency_key TEXT;
CREATE UNIQUE INDEX orders_idempotency_key_idx
    ON orders (tenant_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
