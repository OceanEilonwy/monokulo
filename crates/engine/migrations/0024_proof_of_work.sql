-- Proof-of-work checking (docs/proof_of_work.md).
--
-- A row in proof_networks means the network's orders settle only on blocks
-- whose proof of work the engine checked: from the moment it is written,
-- before any anchor is taken, so turning checking on never lets a payment
-- through unchecked.
CREATE TABLE proof_networks (
    network       TEXT PRIMARY KEY NOT NULL,
    enabled_at    INTEGER NOT NULL,
    -- The anchor: the block the engine took on the nodes' word, deep enough
    -- below their tips that no reorg reaches it. NULL until one is taken.
    anchor_height INTEGER,
    anchor_hash   TEXT,
    -- How many of how many configured nodes gave the same anchor.
    anchor_agreed INTEGER,
    anchor_nodes  INTEGER,
    anchored_at   INTEGER
);

-- The proven chain: the anchor's window of blocks (taken with the anchor)
-- and every block after it whose proof of work was checked here. Pruned to
-- what the next blocks' rules and the deepest followable reorg need.
CREATE TABLE proven_blocks (
    network               TEXT NOT NULL,
    height                INTEGER NOT NULL,
    block_hash            TEXT NOT NULL,
    timestamp             INTEGER NOT NULL,
    -- Decimal: it can pass what an INTEGER holds.
    cumulative_difficulty TEXT NOT NULL,
    -- 1 if its proof of work was checked here, 0 if it came with the anchor.
    checked               INTEGER NOT NULL,
    PRIMARY KEY (network, height)
);

-- RandomX keys (the ids of blocks at multiples of 2048) older than the
-- proven chain's lowest block: taken with the anchor, or kept when a proven
-- block is pruned.
CREATE TABLE proof_seeds (
    network    TEXT NOT NULL,
    height     INTEGER NOT NULL,
    block_hash TEXT NOT NULL,
    PRIMARY KEY (network, height)
);

-- The id of the block a payment was found in, at its block_height, when it
-- is known from the block itself (a scan, or the block's own transaction
-- list): while checking is on, a payment settles only if this is the proven
-- block at that height. Cleared whenever the height changes.
ALTER TABLE order_payments ADD COLUMN block_hash TEXT;
