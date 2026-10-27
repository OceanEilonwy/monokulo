-- The handles, kept so the file can't link a device's handles: the first
-- shape kept each token and sender key in the clear, so one device's
-- handles for two senders shared a token, and the file named the senders
-- every device takes pushes from. Its rows can't be carried over (their
-- ids were random, and an id is now a MAC, under the relay's handle key,
-- of what it was registered for), so they go: an app registers again when it next starts, as it
-- does anyway, and its sender's pushes to the old endpoint get 404 until
-- then. The relay deletes with `secure_delete` on, so the dropped rows'
-- bytes are overwritten.
DROP TABLE handles;

-- A handle is made for one device token, app and sender (its VAPID key);
-- the same four registered again are the same handle, its expiry pushed
-- back. No payload and no store is kept.
CREATE TABLE handles (
    -- SHA-256 of the handle's id (`ph_` and 32 hex digits, the last part
    -- of its endpoint: an HMAC, under the relay's handle key, of the four
    -- it was registered for): the row is found from its endpoint, and the
    -- id itself isn't kept. The key is never in this file, so a sender's
    -- endpoints open its own rows and no other sender's of those devices.
    key BLOB PRIMARY KEY NOT NULL CHECK (length(key) = 32),
    platform TEXT NOT NULL
        CHECK (platform IN ('apns', 'apns-liveactivity', 'fcm')),
    app TEXT NOT NULL,
    -- The device token and the sender's VAPID key, AES-256-GCM under a key
    -- derived from the handle's id and bound to its platform and app: the
    -- 12-byte nonce, then the ciphertext. Readable only with the endpoint
    -- in hand.
    sealed BLOB NOT NULL,
    -- 1 once APNs or FCM has taken a push for it.
    delivered INTEGER NOT NULL CHECK (delivered IN (0, 1)),
    -- Unix seconds, rounded up to the hour: `handles.unused_ttl_days`
    -- after registration until a push is delivered, then
    -- `handles.ttl_days` after the last registration or delivery.
    -- Unknown from then on.
    expires_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- The sweep of expired handles.
CREATE INDEX handles_expiry ON handles (expires_at);
