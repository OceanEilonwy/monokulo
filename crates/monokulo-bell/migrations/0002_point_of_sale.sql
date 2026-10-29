-- Which point of sale a registration is for: its wire name, `square` or
-- `clover`. Rows from before this migration are Square's: the Bell served
-- Square alone until then. A value the Bell doesn't know reads as a
-- corrupt row, never as another point of sale.
ALTER TABLE registrations ADD COLUMN point_of_sale TEXT NOT NULL DEFAULT 'square'
    CHECK (length(point_of_sale) BETWEEN 1 AND 32);

-- A webhook's dings: every registration of its point of sale's merchant's
-- location. Two points of sale may give out the same id text, so the
-- point of sale leads.
DROP INDEX registrations_location;
CREATE INDEX registrations_location ON registrations (point_of_sale, merchant_id, location_id);
