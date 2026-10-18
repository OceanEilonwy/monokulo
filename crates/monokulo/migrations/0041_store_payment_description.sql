-- What a store's payments are for, in the merchant's words ("Coffee and
-- cake at the corner shop"): shown on the checkout under the store's name
-- when an order has no description of its own. Plain text on one line, at
-- most 140 characters (`crate::stores::clean_description`); '' for none.
ALTER TABLE store_connections ADD COLUMN payment_description TEXT NOT NULL DEFAULT '';
