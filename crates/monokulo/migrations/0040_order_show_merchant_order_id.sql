-- Whether the checkout shows the payer the order's merchant reference
-- (`merchant_order_id`). A caller may turn it off for a reference that
-- means nothing to the payer, like the WooCommerce plugin's post ID; the
-- dashboard and webhooks show it either way. Orders from before this
-- column existed default to `1`, as every order did then.
ALTER TABLE order_currency_metadata ADD COLUMN show_merchant_order_id INTEGER NOT NULL DEFAULT 1;
