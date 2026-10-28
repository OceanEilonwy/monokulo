-- The Bell's one table: which tills to ding for which Square location.
-- A till registers its Web Push subscription for one location of one
-- merchant, proven with the merchant's token (which isn't kept). Nothing
-- else is: no webhook, no payment, no token.
CREATE TABLE registrations (
    -- `br_` and 32 hex digits: what the till unregisters with.
    id TEXT PRIMARY KEY NOT NULL,
    -- The merchant Square said the location belongs to.
    merchant_id TEXT NOT NULL,
    location_id TEXT NOT NULL,
    -- The subscription's endpoint: a capability (whoever holds it can push
    -- to the till), so it is never logged. One registration per endpoint;
    -- registering it again moves it.
    endpoint TEXT NOT NULL UNIQUE,
    -- The subscription's keys, base64url: what a ding is encrypted to.
    p256dh TEXT NOT NULL,
    auth TEXT NOT NULL,
    -- Unix seconds.
    created_at INTEGER NOT NULL,
    -- The last ding its push service took; NULL before the first.
    last_ok_at INTEGER,
    -- Dings in a row its push service didn't take; at
    -- `bell.registration_failure_limit` the registration is deleted.
    failures INTEGER NOT NULL DEFAULT 0 CHECK (failures >= 0)
) STRICT;

-- A webhook's dings: every registration of its merchant's location.
CREATE INDEX registrations_location ON registrations (merchant_id, location_id);
