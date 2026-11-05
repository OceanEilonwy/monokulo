-- A store has one web domain and one onion address. `db::refuse_extra_domains`
-- stops the process before this migration runs on a database where a store
-- has more, so these indexes never fail and no row is deleted here.
--
-- `replaces_id` is a change of domain underway: the new domain waits for its
-- record beside the one it replaces, and takes its place once verified, so the
-- store's checkout keeps working in between. It isn't counted against the
-- store's one of each, but there can be only one change of each at a time.
ALTER TABLE store_domains ADD COLUMN replaces_id TEXT;
ALTER TABLE store_domains ADD COLUMN onion INTEGER GENERATED ALWAYS AS (domain LIKE '%.onion') VIRTUAL;
CREATE UNIQUE INDEX store_domains_one_of_each ON store_domains (connection_id, onion) WHERE replaces_id IS NULL;
CREATE UNIQUE INDEX store_domains_one_change ON store_domains (connection_id, onion) WHERE replaces_id IS NOT NULL;
