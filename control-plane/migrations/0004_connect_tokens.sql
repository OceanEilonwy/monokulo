-- Single-use tokens minted by the generic connect flow (WBS 1.4.1,
-- docs/WOOCOMMERCE_ROADMAP.md Stage 6): `POST /connect/{platform}` mints one
-- of these instead of ever putting the tenant's raw `sk_...` secret in a
-- browser redirect, then `POST /connect/{platform}/finish` (called
-- server-to-server by the plugin, not the browser) redeems it exactly once
-- for the real credentials.
--
-- `token_hash` is the SHA-256 hash of the raw token (see
-- shared::auth::hash_secret_token) - same at-rest-hashing reasoning as
-- `sessions.token`, never the raw token itself. `nonce` is the caller
-- (plugin)-supplied value carried through the whole round trip so the
-- plugin itself can confirm the redirect it receives corresponds to the
-- request it made; the control plane doesn't interpret it, just persists
-- and hands it back on the `return_url` redirect. `consumed_at` is set
-- exactly once, by a single atomic `UPDATE ... WHERE consumed_at IS NULL`
-- statement checking the affected-row count (see
-- `Db::consume_connect_token`) - the mechanism that makes this token
-- genuinely single-use even under two concurrent `/finish` calls.
CREATE TABLE connect_tokens (
    token_hash    TEXT PRIMARY KEY,
    connection_id TEXT NOT NULL REFERENCES store_connections(id),
    nonce         TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    consumed_at   INTEGER
);
