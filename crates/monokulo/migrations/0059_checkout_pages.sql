-- A store's custom checkout page in use (`crate::checkout_pages`): the one version
-- its domain approved. Editing it makes a draft and leaves this showing.
--
-- `failing_since_utc`, like a verified domain's: when a check first found its
-- proof gone, after which it keeps showing for a grace period. `turned_off_at_utc`
-- is this Monokulo's operator taking it down, which nothing the merchant does
-- can undo.
CREATE TABLE checkout_pages (
    connection_id       TEXT PRIMARY KEY REFERENCES store_connections (id),
    template            TEXT NOT NULL,
    css                 TEXT NOT NULL,
    written_css         TEXT NOT NULL,
    token               TEXT NOT NULL,
    published_at_utc    INTEGER NOT NULL,
    last_checked_at_utc INTEGER,
    failing_since_utc   INTEGER,
    last_error          TEXT,
    turned_off_at_utc   INTEGER,
    turned_off_reason   TEXT
);
