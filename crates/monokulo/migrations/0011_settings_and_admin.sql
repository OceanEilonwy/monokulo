-- Runtime-configurable settings, the same key/value shape (and the same
-- env > database > default resolution, shared/src/settings.rs) the engine's
-- own equivalent table already uses (crates/scanner/migrations/
-- 0010_settings.sql) - see that migration's own comment for why a plain
-- key/value table, not one column per setting, is the right shape here too.
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Distinguishes the one instance-admin account (created once, by the
-- first-run setup wizard - see `http::setup`) from every ordinary merchant
-- user `POST /dashboard/signup` creates. `0` for every existing row (an
-- already-deployed instance's own users are all ordinary merchants; the
-- setup wizard only ever creates a NEW row with this set) and for every new
-- signup going forward - only the setup wizard's own `create_user` call ever
-- passes `true`.
ALTER TABLE users ADD COLUMN is_admin INTEGER NOT NULL DEFAULT 0;
