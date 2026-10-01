-- Each payment's one-time output key (the output's stealth address, hex), so
-- the same key is never credited twice. Two outputs can carry the same key
-- (a sender who reuses a transaction key, by mistake or on purpose - the
-- "burning bug"), but only one of them can ever be spent: crediting both
-- would pay an order with money the merchant cannot have. The first credited
-- output keeps the key; a later one with the same key is refused
-- (`Store::record_payment_match`). Staged matches carry it through to the
-- payment row they become.
ALTER TABLE order_payments ADD COLUMN output_key TEXT;
CREATE INDEX order_payments_output_key_idx ON order_payments(output_key) WHERE output_key IS NOT NULL;
ALTER TABLE partial_block_matches ADD COLUMN output_key TEXT;
