-- Column named `token` for parity with a bearer-token session model, but it
-- holds the SHA-256 hash of the raw session token (see
-- shared::auth::hash_secret_token), never the raw token itself. Same
-- at-rest-hashing reasoning as the engine's own `tenants.secret_token_hash`
-- (see docs/DESIGN.md §10.1): a session token is a high-entropy,
-- machine-generated bearer credential, so a slow hash buys no
-- brute-force-resistance, but hashing it still means a raw DB dump can't
-- be replayed directly as a live session.
CREATE TABLE sessions (
    token      TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id),
    created_at INTEGER NOT NULL
);
