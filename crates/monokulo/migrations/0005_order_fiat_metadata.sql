-- Control-plane's own record of what a customer was quoted, in fiat, for an
-- order (`docs/fx_refactor.md` Phase 1.2) - the engine no longer has any
-- concept of fiat/exchange-rate at all (that document's own resolved
-- decision 2), so this data now lives here instead of on the engine's
-- `orders` table. Keyed by `(connection_id, payment_id)` since a
-- `payment_id` alone is only unique *within* one tenant/connection, not
-- globally, and control-plane can see the same engine reused across
-- multiple `store_connections` rows over time (a store re-connected after
-- being disconnected, for instance).
--
-- `piconero_per_unit` is the exact rate actually used to compute the XMR
-- amount at creation time (not re-derived later, which could drift from
-- whatever the live/fixed rate is by the time anyone looks) - a real,
-- historical record of what the customer was actually charged against, the
-- same reason the engine's own former `orders.exchange_rate` column
-- existed.
CREATE TABLE order_fiat_metadata (
    connection_id     TEXT NOT NULL REFERENCES store_connections(id),
    payment_id        TEXT NOT NULL,
    fiat_currency     TEXT NOT NULL,
    fiat_amount       TEXT NOT NULL,
    piconero_per_unit INTEGER NOT NULL,
    created_at        INTEGER NOT NULL,
    PRIMARY KEY (connection_id, payment_id)
);
