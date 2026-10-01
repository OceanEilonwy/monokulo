-- When a delivery was given up on (its last allowed attempt failed), so the
-- picker can leave it out without a sentinel date: a given-up delivery used
-- to be scheduled a hundred years out, which also kept it in the way of every
-- later event for its order (one is sent per order at a time, oldest first).
-- The row stays for inspection; it is never deleted (docs/DESIGN.md §11).
ALTER TABLE webhook_deliveries ADD COLUMN gave_up_at_utc INTEGER;
