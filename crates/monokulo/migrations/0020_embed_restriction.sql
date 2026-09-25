-- "Only my verified domains can show this checkout" (`crate::embed_domains`):
-- when set, the store's checkout can only be framed by, and take browser
-- requests from, its verified domains and their subdomains.
ALTER TABLE store_connections ADD COLUMN embed_restricted INTEGER NOT NULL DEFAULT 0;

-- Whether this store's existing site and allowed origins (the engine's
-- per-tenant list, which monokulo no longer asks for) have been copied into
-- `store_domains` as domains waiting for DNS. Done once, at startup, by
-- `embed_domains::import_existing_domains`; a store created later gets its
-- site's domain added when it is created.
ALTER TABLE store_connections ADD COLUMN domains_imported INTEGER NOT NULL DEFAULT 0;

-- Every public `/pay/{pk}/...` request now looks its store's embed policy up
-- by public key.
CREATE INDEX store_connections_tenant_public_key ON store_connections (tenant_public_key);
