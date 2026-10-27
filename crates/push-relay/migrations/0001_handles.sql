-- The relay's one table: where each handle's pushes go. A handle is made
-- for one device token, app and sender (its VAPID key); the same four
-- registered again are the same handle, its expiry pushed back. Nothing
-- else is kept: no payload, no store, nothing linking two handles of one
-- device.
CREATE TABLE handles (
    -- `ph_` and 32 hex digits: the last part of the endpoint it was minted
    -- as.
    id TEXT PRIMARY KEY NOT NULL,
    platform TEXT NOT NULL
        CHECK (platform IN ('apns', 'apns-liveactivity', 'apns-pushtostart', 'fcm')),
    app TEXT NOT NULL,
    -- The APNs device or activity token, or the FCM registration token.
    token TEXT NOT NULL,
    -- The sender's VAPID public key, base64url: only pushes it signed are
    -- handed on.
    vapid_public_key TEXT NOT NULL,
    -- Unix seconds.
    created_at INTEGER NOT NULL,
    -- The last registration, or the last push APNs or FCM took.
    last_used_at INTEGER NOT NULL,
    -- `last_used_at` plus `handles.ttl_days`: unknown from then on.
    expires_at INTEGER NOT NULL,
    UNIQUE (platform, app, token, vapid_public_key)
) STRICT;

-- The sweep of expired handles.
CREATE INDEX handles_expiry ON handles (expires_at);
