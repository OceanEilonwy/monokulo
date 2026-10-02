-- The highest recorded block a majority of a network's nodes agree on
-- (docs/chain_agreement.md). An order may only newly settle on
-- confirmations counted up to it. No row: no ceiling (one node configured,
-- or no second node has answered for a while).
CREATE TABLE settlement_ceilings (
    network TEXT PRIMARY KEY,
    height INTEGER NOT NULL CHECK (height >= 0),
    updated_at_utc INTEGER NOT NULL
);
