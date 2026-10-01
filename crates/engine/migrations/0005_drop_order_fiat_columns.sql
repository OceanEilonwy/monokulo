-- `docs/fx_refactor.md` Phase 3: the engine's public order-creation API becomes
-- XMR-only. Fiat/FX is no longer this process's concern at all - the control-plane
-- now owns the sole copy of a fiat order record (its own `order_fiat_metadata`
-- table), keyed by the same `order_id` this table's `id` still is.
-- `xmr_amount_piconero` remains here as it always was: the actual source of truth
-- for what an order is worth, which the engine watches the chain against.
--
-- SQLite's `ALTER TABLE ... DROP COLUMN` (supported since SQLite 3.35.0, well
-- within this crate's bundled `rusqlite` version) refuses to drop a column that's
-- part of an index or a `CHECK`/`UNIQUE`/foreign-key constraint - none of these
-- three ever were, so no table rebuild is needed here (unlike migration 0004's
-- constraint change).
ALTER TABLE orders DROP COLUMN fiat_currency;
ALTER TABLE orders DROP COLUMN fiat_amount;
ALTER TABLE orders DROP COLUMN exchange_rate;
