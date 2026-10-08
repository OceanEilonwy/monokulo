-- A merchant's named wallets (docs/wallets.md). A wallet's keys live only in
-- the engine (`engine_wallet_id`); monokulo keeps what it shows and picks
-- by: the name, the network and the primary address. Several stores can
-- take payments into one wallet.
--
-- `origin`: 'created' (made on the "Create a new wallet" page, in the
-- merchant's browser) or 'imported' (keys pasted in: "Bring your own
-- wallet"). `backup`: for a created wallet, how its owner saved the
-- recovery phrase ('cake', 'monerocom', 'stack', 'feather', 'gui', 'paper',
-- or 'skipped'); NULL for an imported one.
CREATE TABLE wallets (
    id                TEXT PRIMARY KEY,
    user_id           TEXT NOT NULL REFERENCES users(id),
    name              TEXT NOT NULL,
    network           TEXT NOT NULL,
    primary_address   TEXT NOT NULL,
    engine_wallet_id  TEXT NOT NULL,
    origin            TEXT NOT NULL,
    backup            TEXT,
    created_at_utc    INTEGER NOT NULL
);

-- Names pick a wallet out of a list, so one account never has two alike.
CREATE UNIQUE INDEX wallets_user_name ON wallets (user_id, name COLLATE NOCASE);
-- The same keys added twice would be two wallets watching one: refused.
CREATE UNIQUE INDEX wallets_user_address ON wallets (user_id, network, primary_address);

-- What a wallet has been through, for its page: made, renamed, a store
-- connected. Payments are read from the engine, not copied here.
CREATE TABLE wallet_events (
    wallet_id   TEXT NOT NULL REFERENCES wallets(id) ON DELETE CASCADE,
    at_utc      INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    detail      TEXT NOT NULL DEFAULT ''
);
CREATE INDEX wallet_events_wallet ON wallet_events (wallet_id, at_utc);

-- The wallet a store takes payments into. NULL for a store made before
-- wallets existed, until it is matched to one (`Db::adopt_store_wallet`).
ALTER TABLE store_connections ADD COLUMN wallet_id TEXT REFERENCES wallets(id);
