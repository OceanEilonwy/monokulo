-- Searches an admin saved on the Logs page (structured_logging.md 5.1 row
-- 12): a name and the page's query string (q, level, service, range...),
-- so opening one is just following a link.
CREATE TABLE saved_log_searches (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    query_string TEXT NOT NULL,
    created_at_utc INTEGER NOT NULL
);
CREATE INDEX saved_log_searches_user ON saved_log_searches (user_id, name);
