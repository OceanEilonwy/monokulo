-- `docs/order_rescan_wbs.md` Phase 5.1: tracks the actual block-height range that
-- has ever been examined for each order, so a merchant can tell "was the block
-- range around when my customer says they paid actually checked" without reading
-- logs. Both columns are `NULL` until an order is first examined by anything -
-- essentially immediately after creation (the very next scan tick), never
-- backfilled to `created_at`'s own height.
--
-- Updated by two independent writers sharing the same min/max-accumulate
-- discipline (`Store::bump_scanned_heights_for_tenant` for ordinary live
-- scanning, `Store::bump_scanned_range_for_order` for a manual rescan,
-- `scanner.rs`): `first_scanned_height` only ever moves earlier,
-- `last_scanned_height` only ever moves later. Never *replaced* by either writer -
-- a naive replace on a second rescan would silently narrow the displayed range
-- back down, hiding real prior coverage.
ALTER TABLE orders ADD COLUMN first_scanned_height INTEGER;
ALTER TABLE orders ADD COLUMN last_scanned_height INTEGER;
