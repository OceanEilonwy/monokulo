-- What this table records is no longer necessarily "fiat" - an order can now
-- be priced directly in XMR (`shared::exchange_rate::XmrIdentityProvider`),
-- which needs the exact same "what was this order quoted, and by which
-- provider" record this table already keeps. Renamed to match: the table
-- itself, and its two currency-shaped columns, drop the "fiat" name rather
-- than keep a name that's now actively misleading for an XMR order's own row.
ALTER TABLE order_fiat_metadata RENAME COLUMN fiat_currency TO currency;
ALTER TABLE order_fiat_metadata RENAME COLUMN fiat_amount TO amount;
ALTER TABLE order_fiat_metadata RENAME TO order_currency_metadata;
