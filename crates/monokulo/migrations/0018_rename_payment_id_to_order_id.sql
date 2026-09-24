-- The engine's own wire field/column for an order's own identifier is now
-- `order_id`, not `payment_id` (this session's full cross-service rename -
-- an order's own id was never actually "a payment", that word already means
-- something else in this system: a real on-chain monero transaction/receipt
-- toward an order, see `Store::get_order`'s own `payments` table). This
-- column follows suit, matching `order_currency_metadata`'s own name.
ALTER TABLE order_currency_metadata RENAME COLUMN payment_id TO order_id;
