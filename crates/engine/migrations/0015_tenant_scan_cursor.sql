-- Per-tenant scan cursor (admin_settings_v2.md task 5.0).
--
-- Scan progress used to be tracked only per network (`scanned_blocks`), so
-- when one tenant couldn't be scanned (its key custody backend failing, or
-- its keys not registered) either the whole network stopped, or the network
-- moved on past blocks that tenant was never checked against. This column
-- records, per tenant, the highest block on its network that has been fully
-- scanned for it. A tenant whose cursor is below the network's highest
-- scanned block is "lagging" and is caught up block by block.
--
-- NULL means "not anchored yet": a tenant created before its network was
-- ever scanned. It is anchored to the network's height when the network is
-- first seeded.
--
-- Existing tenants were all scanned together up to now, so each starts at
-- its network's current height.
ALTER TABLE tenants ADD COLUMN scanned_through_height INTEGER;
UPDATE tenants SET scanned_through_height =
    (SELECT MAX(height) FROM scanned_blocks WHERE scanned_blocks.network = tenants.network);
