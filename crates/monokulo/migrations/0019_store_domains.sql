-- Verified embed domains (`crate::embed_domains`): the domains a merchant
-- has proved they own, via a TXT record at `_monokulo.<domain>` holding
-- `token`. One domain covers all of its subdomains.
--
-- `verified_at_utc` is set by the first check that finds the record (and
-- refreshed by every later one); NULL means still waiting for it.
-- `failing_since_utc` is set by the first failed check after verification
-- and cleared by the next successful one - a failing domain keeps counting
-- as verified for a grace period (`embed_domains::GRACE_SECS`), then lapses.
-- `last_error` is the most recent failed check's reason, for the merchant.
CREATE TABLE store_domains (
    id                   TEXT PRIMARY KEY,
    connection_id        TEXT NOT NULL REFERENCES store_connections (id),
    domain               TEXT NOT NULL,
    token                TEXT NOT NULL,
    created_at_utc       INTEGER NOT NULL,
    verified_at_utc      INTEGER,
    failing_since_utc    INTEGER,
    last_checked_at_utc  INTEGER,
    last_error           TEXT,
    UNIQUE (connection_id, domain)
);

-- The store page's "any website can show this checkout" warning, shrunk to
-- one line once the merchant has dismissed it.
ALTER TABLE store_connections ADD COLUMN embed_warning_dismissed INTEGER NOT NULL DEFAULT 0;
