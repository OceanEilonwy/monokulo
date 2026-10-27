-- Pushes to connected apps (monokular docs/design.md §5): where an app
-- wants its store's sale events pushed while it isn't running, and the key
-- this Monokulo signs its pushes with. Deliveries come in a later
-- migration; nothing here is read or changed by an existing table.
--
-- `vapid_keys`: this instance's VAPID key pair (RFC 8292), made at the
-- first start that finds no row (`crate::push`). `public_key`: the
-- uncompressed P-256 point, base64url, as apps hand it to their push
-- service. `private_key_encrypted`: the private scalar, encrypted with
-- MONOKULO_ENCRYPTION_KEY and bound to this row (`crypto::Binding::VapidKey`):
-- it would let anyone push as this Monokulo. One row.
CREATE TABLE vapid_keys (
    id                    INTEGER PRIMARY KEY CHECK (id = 1),
    public_key            TEXT NOT NULL,
    private_key_encrypted TEXT NOT NULL,
    created_at_utc        INTEGER NOT NULL
);

-- An app's push subscription (`/api/v1/push-targets`). `id`: `pt_` and 32
-- hex digits. `client_id`: the app's own name for it; registering the same
-- one again replaces the subscription in place (same id, same
-- `created_at_utc`). `kind`: `encrypted` (RFC 8291 Web Push, to a
-- UnifiedPush distributor or the push relay) carries the subscription's
-- `p256dh` and `auth`, base64url, kept in the clear: they let one encrypt
-- to the device, and are no use without this Monokulo's VAPID key;
-- `live-activity` (plain JSON to the relay, for an iOS Live Activity)
-- carries neither, and `include_payment_uri` says whether its pushes may
-- carry the payment URI (the app's "Show code in island"). `events`: a
-- JSON array of the sale events wanted (`sale.paid`, ...). `failures`:
-- failed attempts in a row, reset by a success. Disconnecting the app, or
-- deleting the store, deletes its targets.
CREATE TABLE push_targets (
    id                  TEXT PRIMARY KEY,
    store_id            TEXT NOT NULL REFERENCES store_connections(id) ON DELETE CASCADE,
    app_id              TEXT NOT NULL REFERENCES app_connections(id) ON DELETE CASCADE,
    client_id           TEXT NOT NULL,
    kind                TEXT NOT NULL CHECK (kind IN ('encrypted', 'live-activity')),
    endpoint            TEXT NOT NULL,
    p256dh              TEXT,
    auth                TEXT,
    events              TEXT NOT NULL,
    include_payment_uri INTEGER NOT NULL DEFAULT 0 CHECK (include_payment_uri IN (0, 1)),
    created_at_utc      INTEGER NOT NULL,
    last_ok_at_utc      INTEGER,
    failures            INTEGER NOT NULL DEFAULT 0,
    UNIQUE (app_id, client_id),
    CHECK ((kind = 'encrypted') = (p256dh IS NOT NULL AND auth IS NOT NULL)),
    CHECK (kind = 'encrypted' OR (p256dh IS NULL AND auth IS NULL)),
    CHECK (kind = 'live-activity' OR include_payment_uri = 0)
);
CREATE INDEX push_targets_store ON push_targets (store_id);
