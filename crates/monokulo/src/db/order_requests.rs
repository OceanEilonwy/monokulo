//! The idempotency keys a store's orders were asked with (migration 0047,
//! `crate::orders`): what each key's purchase was, the price it was made
//! at, and, once recorded, the order it made.

use rusqlite::{params, OptionalExtension as _};
use shared::sqlite::Unsigned;
use shared::xmr_amount::Piconero;

use super::{ConnectionId, Db, OrderId, Result};

/// What an order was priced at: the XMR it asks for, the rate and provider
/// that gave it, and how its confirmations were decided
/// (`crate::confirmation_thresholds::Resolution`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderPrice {
    pub xmr_amount_piconero: u64,
    pub piconero_per_unit: Piconero,
    pub provider: String,
    pub base_currency: String,
    pub base_currency_piconero_per_unit: Option<Piconero>,
    pub confirmations_required: u64,
}

/// Where a key stands, for the purchase a caller asks for with it.
#[derive(Debug, PartialEq, Eq)]
pub enum Reserved {
    /// Reserved for this purchase at `at`, at `price`, with no order
    /// recorded yet: the engine is asked for exactly that order.
    Priced { price: OrderPrice, at: i64 },
    /// Reserved for another purchase.
    Conflict,
    /// This purchase's order, recorded.
    Made(OrderId),
}

impl Db {
    /// Where `key` stands for the purchase `fingerprint`; `None` when it was
    /// never reserved.
    pub fn order_request(
        &self,
        store: &ConnectionId,
        key: &str,
        fingerprint: &str,
    ) -> Result<Option<Reserved>> {
        let row = self
            .conn
            .query_row(
                "SELECT fingerprint, xmr_amount_piconero, piconero_per_unit, provider, store_base_currency,
                        base_currency_piconero_per_unit, confirmations_required, reserved_at_utc, order_id
                 FROM order_requests WHERE connection_id = ?1 AND idempotency_key = ?2",
                params![store, key],
                |row| {
                    let stored: String = row.get(0)?;
                    let order_id: Option<OrderId> = row.get(8)?;
                    Ok(match order_id {
                        _ if stored != fingerprint => Reserved::Conflict,
                        Some(order_id) => Reserved::Made(order_id),
                        None => Reserved::Priced {
                            price: OrderPrice {
                                xmr_amount_piconero: row.get::<_, Unsigned<u64>>(1)?.0,
                                piconero_per_unit: row.get(2)?,
                                provider: row.get(3)?,
                                base_currency: row.get(4)?,
                                base_currency_piconero_per_unit: row.get(5)?,
                                confirmations_required: row.get::<_, Unsigned<u64>>(6)?.0,
                            },
                            at: row.get(7)?,
                        },
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Reserves `key` for the purchase `fingerprint` at `price`, unless it
    /// is reserved already: then where the first reservation stands, which
    /// a concurrent attempt with the same key may have made a moment ago.
    pub fn reserve_order_request(
        &self,
        store: &ConnectionId,
        key: &str,
        fingerprint: &str,
        price: &OrderPrice,
        at: i64,
    ) -> Result<Reserved> {
        self.conn.execute(
            "INSERT INTO order_requests
                (connection_id, idempotency_key, fingerprint, xmr_amount_piconero, piconero_per_unit, provider,
                 store_base_currency, base_currency_piconero_per_unit, confirmations_required, reserved_at_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (connection_id, idempotency_key) DO NOTHING",
            params![
                store,
                key,
                fingerprint,
                Unsigned(price.xmr_amount_piconero),
                price.piconero_per_unit,
                price.provider,
                price.base_currency,
                price.base_currency_piconero_per_unit,
                Unsigned(price.confirmations_required),
                at,
            ],
        )?;
        Ok(self
            .order_request(store, key, fingerprint)?
            .unwrap_or(Reserved::Conflict))
    }

    /// Records the order `key` made: in the transaction that writes the
    /// order's own record (`crate::orders`), so a key names an order only
    /// once it is recorded. `false` when another attempt recorded it first.
    pub fn order_request_made(
        &self,
        store: &ConnectionId,
        key: &str,
        order_id: &OrderId,
    ) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE order_requests SET order_id = ?3
             WHERE connection_id = ?1 AND idempotency_key = ?2 AND order_id IS NULL",
            params![store, key, order_id],
        )?;
        Ok(updated == 1)
    }

    /// Lets go of a reservation the engine made no order for (it refused
    /// the request), so the next attempt with the key prices it afresh.
    pub fn release_order_request(&self, store: &ConnectionId, key: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM order_requests
             WHERE connection_id = ?1 AND idempotency_key = ?2 AND order_id IS NULL",
            params![store, key],
        )?;
        Ok(())
    }
}
