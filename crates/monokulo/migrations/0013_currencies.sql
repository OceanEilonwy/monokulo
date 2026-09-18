-- The reference list every currency dropdown in this crate is populated
-- from (a store's `base_currency` at creation/change, and an order's own
-- `currency` field) - deliberately independent of which exchange-rate
-- provider is currently enabled or what it happens to support. Selecting a
-- currency here only ever asks "is this a real, known currency?"; whether a
-- *rate* can actually be found for it is a separate, later question asked
-- only at the moment a rate is actually needed (order creation, threshold
-- resolution) - see `crate::currencies`'s own module doc comment for the
-- full reasoning.
--
-- `tickers` is a JSON array of alternate symbols/codes a rate provider (or
-- a human) might use for the same currency (e.g. `["USD", "US$", "$"]`) -
-- matched case-insensitively alongside `canonical_code` itself by
-- `crate::currencies::resolve_currency`, so a submitted value doesn't have
-- to exactly match the canonical code to resolve.
--
-- Static seed data, not admin-editable through any UI in this crate today -
-- a reasonable starter set of majors plus XMR itself (also a selectable
-- "currency" here, for a store whose base currency is simply XMR).
CREATE TABLE currencies (
    canonical_code TEXT PRIMARY KEY,
    description    TEXT NOT NULL,
    tickers         TEXT NOT NULL
);

INSERT INTO currencies (canonical_code, description, tickers) VALUES
    ('XMR', 'Monero',                '["XMR"]'),
    ('USD', 'United States Dollar',  '["USD", "US$", "$"]'),
    ('EUR', 'Euro',                  '["EUR", "€"]'),
    ('GBP', 'British Pound Sterling','["GBP", "£"]'),
    ('JPY', 'Japanese Yen',          '["JPY", "¥"]'),
    ('AUD', 'Australian Dollar',     '["AUD", "A$"]'),
    ('CAD', 'Canadian Dollar',       '["CAD", "C$"]'),
    ('CHF', 'Swiss Franc',           '["CHF"]'),
    ('CNY', 'Chinese Yuan',          '["CNY", "RMB", "¥"]'),
    ('NZD', 'New Zealand Dollar',    '["NZD", "NZ$"]'),
    ('SEK', 'Swedish Krona',         '["SEK"]'),
    ('NOK', 'Norwegian Krone',       '["NOK"]'),
    ('SGD', 'Singapore Dollar',      '["SGD", "S$"]'),
    ('HKD', 'Hong Kong Dollar',      '["HKD", "HK$"]'),
    ('INR', 'Indian Rupee',          '["INR", "₹"]'),
    ('BRL', 'Brazilian Real',        '["BRL", "R$"]'),
    ('MXN', 'Mexican Peso',          '["MXN", "MEX$"]'),
    ('ZAR', 'South African Rand',    '["ZAR", "R"]'),
    ('KRW', 'South Korean Won',      '["KRW", "₩"]');
