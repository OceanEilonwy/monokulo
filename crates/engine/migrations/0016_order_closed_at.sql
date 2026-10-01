-- When an order reached a terminal status (paid, overpaid or expired), for
-- the per-store scan window (admin_settings_v2.md task 7.3, decision D10): a
-- store is scanned only for orders that are open or closed within the grace
-- period, and "closed within" needs a close time. `updated_at` isn't one: it
-- moves on every recompute.
--
-- Set when an order first becomes terminal, cleared if it stops being
-- terminal (a reorg can move a paid order back to confirming). Existing
-- terminal orders are backfilled with their last update time, the closest
-- thing on record.
ALTER TABLE orders ADD COLUMN closed_at_utc INTEGER;
UPDATE orders SET closed_at_utc = updated_at_utc WHERE status IN ('paid', 'overpaid', 'expired');
