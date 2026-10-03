-- SEV-SNP key custody's master key, wrapped once per engine image: under a
-- key the security processor derives from the chip and the image's launch
-- measurement, so only that image on that chip opens its row
-- (`key_custody::snp`). A new image gets the key by handoff from the engine
-- it replaces and adds its own row.
CREATE TABLE snp_master_keys (
    measurement     BLOB PRIMARY KEY,   -- 48 bytes
    guest_svn       INTEGER NOT NULL,
    wrapped         BLOB NOT NULL,
    created_at_utc  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
