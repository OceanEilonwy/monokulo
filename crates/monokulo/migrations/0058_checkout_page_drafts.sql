-- A store's custom checkout page being written (`http::checkout_pages`): one per
-- store, overwritten by the next, and unread after a week. Customers never see it.
-- `preview_token` is what a preview link carries, and changes with each save.
CREATE TABLE checkout_page_drafts (
    connection_id   TEXT PRIMARY KEY REFERENCES store_connections (id),
    template        TEXT NOT NULL,
    css             TEXT NOT NULL,
    written_css     TEXT NOT NULL,
    preview_token   TEXT NOT NULL UNIQUE,
    created_at_utc  INTEGER NOT NULL
);
