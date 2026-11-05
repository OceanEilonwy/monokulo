-- The domains the last look found this version's proof on, as a JSON list: a page
-- shows only there. So a domain that comes to count after a version was published
-- doesn't frame words it never approved. Existing pages start with none, and the
-- first view looks them up, which adds a column and changes no existing value.
ALTER TABLE checkout_pages ADD COLUMN approved_domains TEXT NOT NULL DEFAULT '[]';
