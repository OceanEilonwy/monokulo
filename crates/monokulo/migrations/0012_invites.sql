-- Pending "let me in" requests from the public /request-invite form (shown
-- to a visitor instead of the plain sign-up CTA whenever this instance's
-- signup.mode setting is "invite_only" - see `crate::settings::SIGNUP_MODE`).
-- `actioned` is a soft-delete flag, not a real row delete: the admin invites
-- page (`http::invites`) only ever lists `actioned = 0` rows, but a row is
-- kept around afterward (dismissed by the admin, or automatically actioned
-- once the requester actually signs up - see `invite_links.request_id`)
-- rather than destroyed, the same "delete means hide, not erase" convention
-- `settings`-adjacent admin state in this crate already favors for anything
-- worth a paper trail.
CREATE TABLE invite_requests (
    id               TEXT PRIMARY KEY,
    email            TEXT NOT NULL,
    message          TEXT NOT NULL,
    created_at_utc   INTEGER NOT NULL,
    actioned         INTEGER NOT NULL DEFAULT 0,
    actioned_at_utc  INTEGER
);

-- A single-use invite token, either generated standalone (the admin invites
-- page's own "create invite link" button - `request_id IS NULL`) or tied to
-- one specific `invite_requests` row (the "email invite" action - see that
-- table's own doc comment). `token_hash` is how a presented token is ever
-- looked up or verified - the same hash-at-rest convention every other
-- bearer credential in this crate already uses (sessions, connect tokens).
--
-- `token_encrypted` is the one deliberate exception in this crate to
-- "credentials are hashed, never stored reversibly": it exists *only* so the
-- admin invites page can keep rendering a real `mailto:` link with the raw
-- token embedded for a request-linked invite, across as many separate page
-- loads as it takes the admin to actually click it - there is no click-time
-- hook to generate a token from a plain HTML anchor without JavaScript (see
-- this repo's own no-JS-reliance convention), so the raw value has to already
-- be in the rendered HTML. Encrypted with the same instance `encryption_key`
-- `store_connections.tenant_secret_token_encrypted` already uses for exactly
-- the same reason (a secret the server itself needs to reproduce later, not
-- just verify) - never populated for a standalone link (`request_id IS
-- NULL`), which is shown once on creation and never redisplayed, so it only
-- ever needs the hash.
CREATE TABLE invite_links (
    id               TEXT PRIMARY KEY,
    token_hash       TEXT NOT NULL UNIQUE,
    token_encrypted  TEXT,
    request_id       TEXT REFERENCES invite_requests (id),
    created_at_utc   INTEGER NOT NULL,
    used_at_utc      INTEGER,
    used_by_user_id  TEXT REFERENCES users (id)
);
