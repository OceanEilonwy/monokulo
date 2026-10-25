-- Which master key each SEV-SNP wrap holds (`key_custody::snp`): an HMAC of
-- a fixed label under the key, so an engine taking the key by handoff, which
-- can't open the other images' wraps, refuses a key that isn't this
-- database's. Existing rows keep NULL until their own image next starts and
-- fills it in; no row is changed here.
ALTER TABLE snp_master_keys ADD COLUMN key_check BLOB;  -- 32 bytes
