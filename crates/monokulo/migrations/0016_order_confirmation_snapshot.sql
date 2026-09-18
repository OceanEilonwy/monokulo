-- Snapshots how an order's confirmation threshold was actually decided, at
-- the moment it was decided (WBS: "the confirmation threshold should be
-- determined based upon the exchange rates, store base currency, and the
-- order currency at that moment in time... this makes it clear how the
-- confirmation threshold was decided"). All three columns are nullable:
-- every order recorded before this migration (or created directly against
-- the engine's own API, bypassing monokulo entirely) simply has none of
-- this - the order detail page shows a plain dash for those, the same
-- "an old row just doesn't have it" convention `order_currency_metadata.provider`'s
-- own `"unknown"` fallback already established for a comparable gap.
--
-- store_base_currency: this store's `base_currency` at the moment this
-- order was created - not read live from `store_connections` later, since
-- an admin could change it afterward and this needs to stay a true
-- historical record of what was actually used.
--
-- base_currency_piconero_per_unit: the rate used to convert the order's own
-- amount into `store_base_currency` terms for threshold comparison - NULL
-- specifically when the order's own currency already *was* the base
-- currency (no separate conversion/rate lookup was ever performed - see
-- `confirmation_thresholds::resolve_for_order`), not merely "unknown".
--
-- confirmations_required_applied: the actual resolved value passed to the
-- engine as this order's own `confirmations_required` override.
ALTER TABLE order_currency_metadata ADD COLUMN store_base_currency TEXT;
ALTER TABLE order_currency_metadata ADD COLUMN base_currency_piconero_per_unit INTEGER;
ALTER TABLE order_currency_metadata ADD COLUMN confirmations_required_applied INTEGER;
