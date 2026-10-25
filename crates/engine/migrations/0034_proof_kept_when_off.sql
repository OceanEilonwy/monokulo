-- Turning proof-of-work checking off no longer forgets the network's anchor,
-- proven chain and RandomX keys (docs/proof_of_work.md): turned on again, it
-- carries on from them. So a row in proof_networks no longer means checking
-- is on by itself; this says whether it is. Every row there now was written
-- with checking on (turning it off deleted the row), so each keeps 1 and no
-- row's meaning changes.
ALTER TABLE proof_networks ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1;
