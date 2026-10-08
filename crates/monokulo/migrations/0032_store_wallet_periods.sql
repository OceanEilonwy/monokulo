-- Which wallet each store took payments into, and when (docs/wallets.md,
-- "Changing a store's wallet"). A store has one open period (`until_utc`
-- NULL): its current wallet. Changing the wallet closes it and opens the
-- next, at the same moment. The engine knows only each order's wallet;
-- this history is monokulo's.
--
-- A deleted wallet leaves its periods, with no wallet: the store's history
-- still says when it changed.
CREATE TABLE store_wallet_periods (
    connection_id  TEXT NOT NULL REFERENCES store_connections(id) ON DELETE CASCADE,
    wallet_id      TEXT REFERENCES wallets(id) ON DELETE SET NULL,
    from_utc       INTEGER NOT NULL,
    until_utc      INTEGER
);
CREATE INDEX store_wallet_periods_store ON store_wallet_periods (connection_id, from_utc);
CREATE INDEX store_wallet_periods_wallet ON store_wallet_periods (wallet_id);

-- Every store on a wallet has used it since it was connected.
INSERT INTO store_wallet_periods (connection_id, wallet_id, from_utc)
SELECT id, wallet_id, created_at_utc FROM store_connections WHERE wallet_id IS NOT NULL;
