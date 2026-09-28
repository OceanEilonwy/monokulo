-- Per-user time zone for every date and time the dashboard shows: an IANA
-- name ('Australia/Perth'), or NULL (the default, and existing rows') for
-- automatic - the browser's own zone, as fx-glue.js reports it in the `tz`
-- cookie, and UTC until it has.
ALTER TABLE users ADD COLUMN timezone TEXT;
