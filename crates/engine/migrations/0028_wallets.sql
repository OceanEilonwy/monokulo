-- A wallet: one set of keys that several stores (tenants) can take payments
-- into. It holds the sealed keys a new store on it starts from, and the one
-- subaddress counter all its stores claim order addresses from, so two
-- stores on the same wallet never hand out the same address (they would
-- otherwise both start at minor index 1, and the scanner would credit one
-- payment to both: see 0004's note on stores sharing a view key).
--
-- A tenant keeps its own copy of the sealed keys, backend, address and
-- network, as before: boot registration, scanning and order creation read
-- the tenant row and are unchanged. `tenants.next_minor_index` mirrors its
-- wallet's counter (every claim updates both), so the scan range
-- `0..next_minor_index` still covers every address any store on the wallet
-- has handed out.
CREATE TABLE wallets (
    id                   TEXT PRIMARY KEY,
    key_custody_backend  TEXT NOT NULL,
    sealed_key_material  BLOB NOT NULL,
    primary_address      TEXT NOT NULL,   -- display-only, as on tenants
    network              TEXT NOT NULL,
    next_minor_index     INTEGER NOT NULL DEFAULT 1,
    created_at_utc       INTEGER NOT NULL, -- unix seconds
    deleted_at_utc       INTEGER
);

ALTER TABLE tenants ADD COLUMN wallet_id TEXT REFERENCES wallets(id);

-- Every existing store gets a wallet of its own, carrying its counter on.
INSERT INTO wallets (id, key_custody_backend, sealed_key_material, primary_address,
                     network, next_minor_index, created_at_utc)
SELECT 'wl_' || id, key_custody_backend, sealed_key_material, primary_address,
       network, next_minor_index, created_at_utc
FROM tenants;

UPDATE tenants SET wallet_id = 'wl_' || id;

CREATE INDEX tenants_wallet_idx ON tenants (wallet_id);
