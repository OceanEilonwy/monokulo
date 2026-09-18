-- Custom, amount-tiered confirmation thresholds for a store (WBS:
-- "Confirmation Thresholds"). The store's own *default* (fallback)
-- confirmation count is the existing engine-side `tenants.confirmations_required`
-- - unchanged, still edited the same way it always was - this table only
-- ever holds the *additional*, amount-keyed overrides layered on top of it.
--
-- `unit_amount` is a decimal string (same "never a REAL/float for money"
-- convention `order_currency_metadata.amount` already uses), denominated in
-- the owning store's own `base_currency` at the moment each row was saved -
-- changing a store's base currency deletes every row here
-- (`Db::delete_confirmation_thresholds_for_connection`, called from
-- `http::orders::update_base_currency`), since an old amount in a
-- since-abandoned currency means nothing any more.
--
-- `UNIQUE(connection_id, unit_amount)` is the DB-level backstop for "you
-- cannot enter two thresholds for the same unit amount" - the real,
-- friendlier rejection happens at the HTTP handler layer first
-- (`Db::create_confirmation_threshold`'s own doc comment).
CREATE TABLE confirmation_thresholds (
    id                      TEXT PRIMARY KEY,
    connection_id           TEXT NOT NULL REFERENCES store_connections (id),
    unit_amount             TEXT NOT NULL,
    confirmations_required  INTEGER NOT NULL,
    created_at_utc          INTEGER NOT NULL,
    UNIQUE (connection_id, unit_amount)
);
