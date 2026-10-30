-- Which environment of its point of sale a registration is for: its wire
-- name, such as `production` or `sandbox` (Square), `north_america` or
-- `europe` (Clover). A Bell now serves several at once, and a sandbox
-- merchant's ids must never meet a production one's.
--
-- Rows from before this migration are in the environment their point of
-- sale defaulted to then, the one a Bell without `environment` set ran:
-- Square's are `production`, Clover's `north_america`. A Bell that ran
-- Square's sandbox or another Clover region before this has to have its
-- tills register again, or its rows moved by hand. The default only fills
-- those rows: every registration names its environment.
ALTER TABLE registrations ADD COLUMN environment TEXT NOT NULL DEFAULT 'production'
    CHECK (length(environment) BETWEEN 1 AND 32);
UPDATE registrations SET environment = 'north_america' WHERE point_of_sale = 'clover';

-- A webhook's dings: every registration of its integration's merchant's
-- location. The point of sale and its environment lead: two integrations
-- may give out the same id text.
DROP INDEX registrations_location;
CREATE INDEX registrations_location
    ON registrations (point_of_sale, environment, merchant_id, location_id);
