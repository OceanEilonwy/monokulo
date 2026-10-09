-- A store has a name of its own, apart from its site: the name is what the
-- dashboard, the checkout and the POS call it; the site is where its
-- checkout runs.
--
-- A site is a host, not a page: browsers send only the origin (scheme,
-- host, port) in `Origin`, and trim a cross-site `Referer` to it, so any
-- page on the host works and a path would never be checked. `site` holds
-- the host as `crate::stores::normalize_site` writes it (lowercase, a port
-- only when it isn't the scheme's), or '' for a store that only takes
-- payments in person.
--
-- No two stores on this instance share a site: the checkout's embed policy
-- and the plugin's connect flow both find a store by its host.
ALTER TABLE store_connections ADD COLUMN name TEXT NOT NULL DEFAULT '';
ALTER TABLE store_connections RENAME COLUMN site_url TO site;
CREATE UNIQUE INDEX store_connections_site ON store_connections (site) WHERE site <> '';
