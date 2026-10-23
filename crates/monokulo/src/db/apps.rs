//! Connected apps (migration 0043, `crate::apps`): their connections, the
//! single-use codes that make them, and the orders they made.

use rusqlite::{params, OptionalExtension as _};
use shared::auth::TokenHash;
use shared::sqlite::Unsigned;

use super::{ConnectionId, Db, OrderId, Result};

/// A device connected to a store with a key of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConnection {
    pub id: String,
    pub store_id: ConnectionId,
    /// What the app calls itself in URLs and headers (`monokular`).
    pub slug: String,
    /// What the app calls itself on pages ("Monokular"); empty when it gave
    /// none.
    pub name: String,
    pub device_name: String,
    /// What the app said it runs on (`android`, `ios`); empty when it didn't.
    pub platform: String,
    pub version: String,
    pub connected_at: i64,
    pub last_seen_at: Option<i64>,
}

/// An app redeeming a code, for [`Db::redeem_app_connect_token`].
pub struct NewApp<'a> {
    pub slug: &'a str,
    pub name: &'a str,
    /// The app's own name for the device, used when the merchant gave none.
    pub device_name: &'a str,
    pub platform: &'a str,
    pub version: &'a str,
    pub key_hash: &'a TokenHash,
}

/// An order a connected app made, as its store's apps are told about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppOrder {
    pub order_id: OrderId,
    pub store_id: ConnectionId,
    pub app_id: String,
    pub device_name: String,
    pub amount: String,
    pub currency: String,
    pub address: String,
    pub xmr_amount_piconero: u64,
    pub merchant_order_id: Option<String>,
    pub expires_at: i64,
    pub created_at: i64,
}

fn app_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppConnection> {
    Ok(AppConnection {
        id: row.get("id")?,
        store_id: row.get("store_id")?,
        slug: row.get("slug")?,
        name: row.get("name")?,
        device_name: row.get("device_name")?,
        platform: row.get("platform")?,
        version: row.get("version")?,
        connected_at: row.get("connected_at_utc")?,
        last_seen_at: row.get("last_seen_at_utc")?,
    })
}

fn order_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppOrder> {
    Ok(AppOrder {
        order_id: row.get("order_id")?,
        store_id: row.get("store_id")?,
        app_id: row.get("app_id")?,
        device_name: row.get("device_name")?,
        amount: row.get("amount")?,
        currency: row.get("currency")?,
        address: row.get("address")?,
        xmr_amount_piconero: row.get::<_, Unsigned<u64>>("xmr_amount_piconero")?.0,
        merchant_order_id: row.get("merchant_order_id")?,
        expires_at: row.get("expires_at")?,
        created_at: row.get("created_at_utc")?,
    })
}

impl Db {
    /// Records a code the dashboard is about to show. Codes a day old are
    /// dropped as new ones are made (they live minutes).
    pub fn create_app_connect_token(
        &self,
        token_hash: &TokenHash,
        store_id: &ConnectionId,
        device_name: &str,
        created_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM app_connect_tokens WHERE created_at_utc < ?1",
            params![created_at.saturating_sub(24 * 60 * 60)],
        )?;
        self.conn.execute(
            "INSERT INTO app_connect_tokens (token_hash, store_id, device_name, created_at_utc)
             VALUES (?1, ?2, ?3, ?4)",
            params![token_hash, store_id, device_name, created_at],
        )?;
        Ok(())
    }

    /// Spends a code and records the app it connects, in one transaction:
    /// the code's single `UPDATE ... WHERE consumed_at_utc IS NULL` (checked
    /// by its row count, as [`Db::consume_connect_token`]) means two apps
    /// can never both redeem it. `None` for an unknown, spent or expired
    /// code, all alike.
    pub fn redeem_app_connect_token(
        &self,
        token_hash: &TokenHash,
        now: i64,
        ttl_seconds: i64,
        new: &NewApp<'_>,
    ) -> Result<Option<AppConnection>> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let id = format!("app_{}", uuid::Uuid::new_v4().simple());
        let spent = tx.execute(
            "UPDATE app_connect_tokens SET consumed_at_utc = ?1, app_id = ?4
             WHERE token_hash = ?2 AND consumed_at_utc IS NULL AND created_at_utc >= ?3",
            params![now, token_hash, now - ttl_seconds, id],
        )?;
        if spent == 0 {
            return Ok(None);
        }
        let (store_id, merchant_named): (ConnectionId, String) = tx.query_row(
            "SELECT store_id, device_name FROM app_connect_tokens WHERE token_hash = ?1",
            params![token_hash],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let device_name = if merchant_named.is_empty() {
            new.device_name
        } else {
            &merchant_named
        };
        tx.execute(
            "INSERT INTO app_connections
                (id, store_id, slug, name, device_name, platform, version, key_hash, connected_at_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                store_id,
                new.slug,
                new.name,
                device_name,
                new.platform,
                new.version,
                new.key_hash,
                now
            ],
        )?;
        let app = tx.query_row(
            "SELECT * FROM app_connections WHERE id = ?1",
            params![id],
            app_from_row,
        )?;
        tx.commit()?;
        Ok(Some(app))
    }

    /// The app a code connected, once it has: for the page that showed it,
    /// which has the code's hash (as hex) in its address.
    pub fn app_connected_by_token(&self, token_hash: &str) -> Result<Option<AppConnection>> {
        self.conn
            .query_row(
                "SELECT a.* FROM app_connect_tokens t JOIN app_connections a ON a.id = t.app_id
                 WHERE t.token_hash = ?1",
                params![token_hash],
                app_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// The app holding the key whose hash is `key_hash`.
    pub fn app_by_key(&self, key_hash: &TokenHash) -> Result<Option<AppConnection>> {
        self.conn
            .query_row(
                "SELECT * FROM app_connections WHERE key_hash = ?1",
                params![key_hash],
                app_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// A store's connected apps, newest first.
    pub fn list_apps(&self, store_id: &ConnectionId) -> Result<Vec<AppConnection>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT * FROM app_connections WHERE store_id = ?1
             ORDER BY connected_at_utc DESC, rowid DESC",
        )?;
        let rows = stmt
            .query_map(params![store_id], app_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Disconnects one of a store's apps: its key stops working at once.
    /// `false` when the store has no such app.
    pub fn delete_app(&self, store_id: &ConnectionId, app_id: &str) -> Result<bool> {
        let deleted = self.conn.execute(
            "DELETE FROM app_connections WHERE store_id = ?1 AND id = ?2",
            params![store_id, app_id],
        )?;
        Ok(deleted > 0)
    }

    /// An app made an order (or connected) now, with this version.
    pub fn app_seen(&self, app_id: &str, version: &str, at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE app_connections SET last_seen_at_utc = ?2,
                 version = CASE WHEN ?3 = '' THEN version ELSE ?3 END
             WHERE id = ?1",
            params![app_id, at, version],
        )?;
        Ok(())
    }

    /// Records an order an app made. A retry that got the same order back
    /// keeps the first record.
    pub fn record_app_order(&self, order: &AppOrder) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO app_orders
                (order_id, store_id, app_id, device_name, amount, currency, address,
                 xmr_amount_piconero, merchant_order_id, expires_at, created_at_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                order.order_id,
                order.store_id,
                order.app_id,
                order.device_name,
                order.amount,
                order.currency,
                order.address,
                Unsigned(order.xmr_amount_piconero),
                order.merchant_order_id,
                order.expires_at,
                order.created_at
            ],
        )?;
        Ok(())
    }

    /// The app order `order_id`, if an app made it.
    pub fn app_order(&self, order_id: &OrderId) -> Result<Option<AppOrder>> {
        self.conn
            .query_row(
                "SELECT * FROM app_orders WHERE order_id = ?1",
                params![order_id],
                order_from_row,
            )
            .optional()
            .map_err(Into::into)
    }
}
