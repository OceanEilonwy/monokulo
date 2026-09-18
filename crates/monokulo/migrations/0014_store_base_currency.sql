-- A store's own base currency (WBS: "Confirmation Thresholds") - the unit
-- custom confirmation thresholds are denominated in, and what an order's
-- own currency gets converted into (via XMR, when they differ) to decide
-- which threshold applies. Selected explicitly at store-creation time from
-- here on (`crate::currencies::resolve_currency` validates it, same as any
-- other currency selection in this crate - see that module's own doc
-- comment); existing rows are backfilled to 'XMR', the safe no-op choice
-- for a store nothing else has ever asked it to convert anything into.
ALTER TABLE store_connections ADD COLUMN base_currency TEXT NOT NULL DEFAULT 'XMR';
