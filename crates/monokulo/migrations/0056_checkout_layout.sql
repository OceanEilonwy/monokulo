-- The layout a store's checkout shows unless an embed asks for another
-- (`views::checkout_layouts`). Every existing store keeps today's page.
ALTER TABLE store_connections ADD COLUMN checkout_layout TEXT NOT NULL DEFAULT 'detailed';
