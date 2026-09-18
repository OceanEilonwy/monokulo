//! The monokulo's own SQLite database — entirely separate from the
//! engine's (`scanner::store::Store`); the two services never share a
//! database file or a connection.
//!
//! Deliberately mirrors the engine's `Store` (see `src/store.rs`) rather than
//! inventing a new shape: one struct wrapping a single `rusqlite::Connection`,
//! migrated on open via `shared::migrations::apply` (the same runner
//! `scanner::store` uses — see WBS 0.5), with `open_in_memory`/
//! `open_file` constructors and an `into_shared` helper producing
//! `Arc<Mutex<Db>>` for handlers to share. SQLite allows exactly one writer
//! regardless of how many handles exist, so a mutex-guarded single connection
//! is a correct — if simple — realization of "single writer", same reasoning
//! as `Store`'s own doc comment.

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, params};

/// Every migration file, applied in order, exactly once each — tracked in
/// `schema_migrations` (created by `shared::migrations::apply`), so
/// re-running this against an already-migrated database file (e.g. every
/// process restart) is a safe no-op.
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_init.sql")),
    (2, include_str!("../migrations/0002_sessions.sql")),
    (3, include_str!("../migrations/0003_store_connections.sql")),
    (4, include_str!("../migrations/0004_connect_tokens.sql")),
    (5, include_str!("../migrations/0005_order_fiat_metadata.sql")),
    (6, include_str!("../migrations/0006_order_fiat_metadata_provider.sql")),
    (7, include_str!("../migrations/0007_store_fx_provider.sql")),
    (8, include_str!("../migrations/0008_remove_fixed_fx_provider.sql")),
    (9, include_str!("../migrations/0009_rename_fiat_to_currency.sql")),
    (10, include_str!("../migrations/0010_utc_suffix_date_columns.sql")),
    (11, include_str!("../migrations/0011_settings_and_admin.sql")),
];

fn apply_migrations(conn: &Connection) -> rusqlite::Result<()> {
    shared::migrations::apply(conn, MIGRATIONS)
}

/// Known credentials every test fixture seeds via [`Db::seed_test_admin`] -
/// public (not `#[cfg(test)]`-gated on the constants themselves) so a test in
/// any module can log in as this account without redefining the literal.
pub const TEST_ADMIN_EMAIL: &str = "admin@monokulo.test";
pub const TEST_ADMIN_PASSWORD: &str = "correct horse battery staple admin";

pub struct Db {
    conn: Connection,
}

pub type SharedDb = Arc<Mutex<Db>>;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

impl DbError {
    /// True for a `UNIQUE` constraint violation (e.g. a duplicate `email`),
    /// as opposed to any other database failure. Callers use this to map a
    /// duplicate-email signup to `409 Conflict` rather than `500` — see
    /// `scanner::store`'s own tests for the same
    /// `SqliteFailure`/`ErrorCode::ConstraintViolation` shape this checks.
    pub fn is_unique_violation(&self) -> bool {
        matches!(
            self,
            DbError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error { code: rusqlite::ErrorCode::ConstraintViolation, .. },
                _
            ))
        )
    }
}

type Result<T> = std::result::Result<T, DbError>;

pub struct UserRow {
    pub id: String,
    pub email: String,
    pub password_hash: String,
    pub created_at: i64,
    /// `true` only for the one instance-admin account the first-run setup
    /// wizard creates (`http::setup`) - `false` for every ordinary merchant
    /// user `POST /dashboard/signup` creates. Gates the nav's own "admin"
    /// link and `/dashboard/admin/*` - see `AuthedUser`'s own doc comment.
    pub is_admin: bool,
}

/// A row from `sessions`. `token_hash` is the SHA-256 hash of the raw
/// bearer token (see the `sessions` migration's own comment on why) - it
/// is never the raw token a client actually presents.
pub struct SessionRow {
    pub token_hash: String,
    pub user_id: String,
    pub created_at: i64,
}

/// A row from `store_connections`. `tenant_secret_token_encrypted` holds the
/// engine's `sk_...` secret token encrypted at rest (WBS 1.2.3, via
/// `crate::crypto::encrypt`) - `Db` itself is crypto-unaware and just stores
/// whatever string it's given; see `http/connections.rs` for where the real
/// encrypt/decrypt calls happen.
pub struct StoreConnectionRow {
    pub id: String,
    pub user_id: String,
    pub platform: String,
    pub site_url: String,
    pub tenant_public_key: String,
    pub tenant_secret_token_encrypted: String,
    pub moneropay_endpoint: String,
    pub created_at: i64,
    /// `"coingecko"` (`exchange_rate_config::COINGECKO`) today - which
    /// fiat exchange-rate provider *this store* uses for a non-XMR order, a
    /// genuine per-merchant choice (a real follow-up to
    /// `docs/fx_refactor.md`) set via its own settings page, not an
    /// instance-wide setting. Irrelevant for an XMR-denominated order,
    /// which always uses the trivial identity rate regardless of this
    /// value - see `exchange_rate_config::ExchangeRateProviders::
    /// piconero_per_unit_for`.
    pub fx_provider: String,
}

/// A row from `order_currency_metadata` (`docs/fx_refactor.md` Phase 1.2) -
/// what a customer was quoted for one order (in fiat, or in XMR itself),
/// recorded at creation time. The engine has no concept of this at all;
/// this is monokulo's own, sole copy of it - see the migration's own
/// comment on why (and the accepted durability trade-off that implies,
/// `docs/fx_refactor.md` decision 4).
pub struct OrderCurrencyMetadataRow {
    pub connection_id: String,
    pub payment_id: String,
    pub currency: String,
    pub amount: String,
    pub piconero_per_unit: u64,
    /// Which provider produced `piconero_per_unit` - `"xmr"` (the trivial
    /// identity rate), `"coingecko"`, `"unknown"` for a row recorded before
    /// this field existed (migration 0006), or a stale `"fixed"` for a row
    /// recorded before that provider's removal.
    pub provider: String,
    pub created_at: i64,
}

impl Db {
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        apply_migrations(&conn)?;
        Ok(Db { conn })
    }

    pub fn open_file(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        apply_migrations(&conn)?;
        Ok(Db { conn })
    }

    pub fn into_shared(self) -> SharedDb {
        Arc::new(Mutex::new(self))
    }

    /// Inserts a new user row. Fails with a unique-violation `DbError` (see
    /// [`DbError::is_unique_violation`]) if `email` is already taken.
    /// `is_admin` is `true` only from the first-run setup wizard's own single
    /// call site (`http::setup`) - every ordinary signup passes `false`.
    pub fn create_user(&self, id: &str, email: &str, password_hash: &str, is_admin: bool, created_at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO users (id, email, password_hash, is_admin, created_at_utc) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, email, password_hash, is_admin, created_at],
        )?;
        Ok(())
    }

    /// Direct row lookup by email — used by tests to confirm what actually
    /// landed in the database (e.g. that `password_hash` is a real Argon2
    /// hash, never the plaintext password).
    pub fn get_user_by_email(&self, email: &str) -> Result<Option<UserRow>> {
        self.conn
            .query_row(
                "SELECT id, email, password_hash, created_at_utc, is_admin FROM users WHERE email = ?1",
                params![email],
                |row| {
                    Ok(UserRow {
                        id: row.get(0)?,
                        email: row.get(1)?,
                        password_hash: row.get(2)?,
                        created_at: row.get(3)?,
                        is_admin: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Direct row lookup by id - used to resolve a session's `user_id` back
    /// to a full user row (see `AuthedUser`'s extractor in `http/mod.rs`).
    pub fn get_user_by_id(&self, id: &str) -> Result<Option<UserRow>> {
        self.conn
            .query_row(
                "SELECT id, email, password_hash, created_at_utc, is_admin FROM users WHERE id = ?1",
                params![id],
                |row| {
                    Ok(UserRow {
                        id: row.get(0)?,
                        email: row.get(1)?,
                        password_hash: row.get(2)?,
                        created_at: row.get(3)?,
                        is_admin: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    /// One runtime-configurable setting's stored value, or `None` if nothing
    /// has ever been saved for `key` - see `crates/scanner/src/store.rs`'s
    /// own `get_setting` (identical shape, identical reasoning) and
    /// `shared::settings`'s own doc comment for the env > database > default
    /// precedence this feeds into.
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.conn.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |row| row.get(0)).optional().map_err(DbError::from)
    }

    /// Persists one setting - an upsert, since the admin settings page's own
    /// "Save" always writes every field it shows regardless of whether a row
    /// already exists for it.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(())
    }

    /// Every stored setting at once - the admin settings page's `GET` reads
    /// the whole table in one query rather than one `get_setting` call per
    /// known key.
    pub fn list_settings(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut stmt = self.conn.prepare("SELECT key, value FROM settings")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        rows.collect::<rusqlite::Result<std::collections::HashMap<_, _>>>().map_err(DbError::from)
    }

    /// `true` once the first-run setup wizard has created the instance admin
    /// account - a dedicated `settings` row (`"setup_complete" = "true"`),
    /// per the user's own explicit "based upon a flag in database" request,
    /// rather than derived from `SELECT COUNT(*) FROM users WHERE is_admin`.
    /// Set exactly once, by `http::admin_setup`'s own successful submission.
    pub fn is_setup_complete(&self) -> Result<bool> {
        Ok(self.get_setting("setup_complete")?.as_deref() == Some("true"))
    }

    pub fn mark_setup_complete(&self) -> Result<()> {
        self.set_setting("setup_complete", "true")
    }

    /// Test-only convenience seeding a known admin account and marking setup
    /// complete in one call - the "tests should seed the admin account with
    /// a known user/pass, which will mean the admin flow won't trigger"
    /// requirement, applied as a single shared helper every test fixture
    /// calls rather than each reimplementing the same two writes. Panics on
    /// a database error - every caller is a test fixture already `.unwrap()`-
    /// ing `Db::open_in_memory()` right next to this, so a failure here is
    /// exactly as fatal to the test as that would be.
    #[cfg(test)]
    pub fn seed_test_admin(&self) {
        let password_hash = shared::password::hash_password(TEST_ADMIN_PASSWORD).expect("hashing the fixed test admin password");
        self.create_user("test-admin", TEST_ADMIN_EMAIL, &password_hash, true, 0).expect("seeding the test admin account");
        self.mark_setup_complete().expect("marking setup complete for the seeded test admin");
    }

    /// Stores a new session. `token_hash` must already be hashed (see
    /// [`SessionRow`]'s doc comment) - `Db` never sees, and never needs to
    /// see, a raw session token.
    pub fn create_session(&self, token_hash: &str, user_id: &str, created_at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (token, user_id, created_at_utc) VALUES (?1, ?2, ?3)",
            params![token_hash, user_id, created_at],
        )?;
        Ok(())
    }

    /// Looks up a session by its hashed token. `None` for an unknown or
    /// already-deleted session - callers (the `AuthedUser` extractor) map
    /// that to 401, same as an unknown tenant secret in the engine's own
    /// `AuthedTenant`.
    pub fn find_session(&self, token_hash: &str) -> Result<Option<SessionRow>> {
        self.conn
            .query_row(
                "SELECT token, user_id, created_at_utc FROM sessions WHERE token = ?1",
                params![token_hash],
                |row| {
                    Ok(SessionRow { token_hash: row.get(0)?, user_id: row.get(1)?, created_at: row.get(2)? })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Deletes a session by its hashed token. Returns whether a row was
    /// actually deleted (`false` if it was already gone), so a future
    /// logout handler (WBS 1.1.3 - not implemented here) can tell "revoked"
    /// from "already revoked" if it ever needs to. Nothing calls this yet.
    pub fn delete_session(&self, token_hash: &str) -> Result<bool> {
        let affected = self.conn.execute("DELETE FROM sessions WHERE token = ?1", params![token_hash])?;
        Ok(affected > 0)
    }

    /// Inserts a new `store_connections` row linking `user_id` to a tenant
    /// already provisioned on a real engine instance (WBS 1.2.2).
    ///
    /// `tenant_secret_token_encrypted` is stored exactly as given, with no
    /// crypto awareness at this layer (WBS 1.2.3) - the caller
    /// (`http/connections.rs`) is responsible for passing an already
    /// `crate::crypto::encrypt`-ed value, never the engine's raw `sk_...`
    /// secret token.
    #[allow(clippy::too_many_arguments)]
    pub fn create_store_connection(
        &self,
        id: &str,
        user_id: &str,
        platform: &str,
        site_url: &str,
        tenant_public_key: &str,
        tenant_secret_token_encrypted: &str,
        moneropay_endpoint: &str,
        created_at: i64,
    ) -> Result<()> {
        // `fx_provider` explicit here (`'coingecko'`), not left to the
        // column's own `DEFAULT` - SQLite can't cheaply change a column
        // `DEFAULT` in place, so after `"fixed"`'s removal this is the one
        // real place a new store's initial provider is decided. Harmless
        // even on an instance that never enables Coingecko: an XMR-priced
        // order never reads this column at all (see `StoreConnectionRow::
        // fx_provider`'s own doc comment), and a merchant can still pick a
        // different available provider from their store's settings page
        // the moment one exists.
        self.conn.execute(
            "INSERT INTO store_connections
                (id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, moneropay_endpoint, created_at_utc, fx_provider)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'coingecko')",
            params![
                id,
                user_id,
                platform,
                site_url,
                tenant_public_key,
                tenant_secret_token_encrypted,
                moneropay_endpoint,
                created_at
            ],
        )?;
        Ok(())
    }

    /// Direct row lookup by id - used by tests to confirm what actually
    /// landed in `store_connections` after `POST /connections` (e.g. that
    /// `tenant_secret_token_encrypted` really holds a real `sk_...` value).
    pub fn get_store_connection_by_id(&self, id: &str) -> Result<Option<StoreConnectionRow>> {
        self.conn
            .query_row(
                "SELECT id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, moneropay_endpoint, created_at_utc, fx_provider
                 FROM store_connections WHERE id = ?1",
                params![id],
                |row| {
                    Ok(StoreConnectionRow {
                        id: row.get(0)?,
                        user_id: row.get(1)?,
                        platform: row.get(2)?,
                        site_url: row.get(3)?,
                        tenant_public_key: row.get(4)?,
                        tenant_secret_token_encrypted: row.get(5)?,
                        moneropay_endpoint: row.get(6)?,
                        created_at: row.get(7)?,
                        fx_provider: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Updates a store's chosen exchange-rate provider - the settings-page
    /// counterpart to `create_store_connection`'s explicit initial
    /// `'coingecko'`. The caller (`http::orders::update_fx_provider`) is responsible for
    /// validating `fx_provider` against `ExchangeRateProviders::
    /// available_providers` first; this layer stores whatever string it's
    /// given, same as every other plain column update in this file.
    pub fn update_store_connection_fx_provider(&self, id: &str, fx_provider: &str) -> Result<()> {
        self.conn.execute("UPDATE store_connections SET fx_provider = ?2 WHERE id = ?1", params![id, fx_provider])?;
        Ok(())
    }

    /// Every `store_connections` row belonging to `user_id`, newest first -
    /// the dashboard home page's own data source. Deliberately no "display
    /// name" column exists on this table (only `site_url`) - the dashboard
    /// derives a display name from `site_url` itself rather than this query
    /// growing a field nothing else needs; see `http/home.rs::display_name`.
    pub fn list_store_connections_for_user(&self, user_id: &str) -> Result<Vec<StoreConnectionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, moneropay_endpoint, created_at_utc, fx_provider
             FROM store_connections WHERE user_id = ?1 ORDER BY created_at_utc DESC",
        )?;
        let rows = stmt
            .query_map(params![user_id], |row| {
                Ok(StoreConnectionRow {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    platform: row.get(2)?,
                    site_url: row.get(3)?,
                    tenant_public_key: row.get(4)?,
                    tenant_secret_token_encrypted: row.get(5)?,
                    moneropay_endpoint: row.get(6)?,
                    created_at: row.get(7)?,
                    fx_provider: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Records what a customer was quoted for one order
    /// (`docs/fx_refactor.md` Phase 1.2) - called once, at order-creation
    /// time, by the same handler that computes the XMR amount from this
    /// exact rate (`http::orders`'s new order-creation endpoint, Phase
    /// 1.4). `piconero_per_unit` is stored as `i64` (SQLite has no native
    /// unsigned integer type) - safe: a real fiat-per-XMR rate is many
    /// orders of magnitude below `i64::MAX`, this only guards against a
    /// `u64` value SQLite genuinely cannot represent, not a plausible real
    /// one.
    #[allow(clippy::too_many_arguments)]
    pub fn create_order_currency_metadata(
        &self,
        connection_id: &str,
        payment_id: &str,
        currency: &str,
        amount: &str,
        piconero_per_unit: u64,
        provider: &str,
        created_at: i64,
    ) -> Result<()> {
        let piconero_per_unit = i64::try_from(piconero_per_unit)
            .expect("piconero_per_unit out of i64 range - not a plausible real exchange rate");
        self.conn.execute(
            "INSERT INTO order_currency_metadata
                (connection_id, payment_id, currency, amount, piconero_per_unit, provider, created_at_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![connection_id, payment_id, currency, amount, piconero_per_unit, provider, created_at],
        )?;
        Ok(())
    }

    /// Looks up one order's currency metadata - `None` when nothing was ever
    /// recorded for this `(connection_id, payment_id)` pair (an order the
    /// engine reports that predates this table, or one created directly
    /// against the engine's own API rather than through monokulo).
    pub fn get_order_currency_metadata(&self, connection_id: &str, payment_id: &str) -> Result<Option<OrderCurrencyMetadataRow>> {
        self.conn
            .query_row(
                "SELECT connection_id, payment_id, currency, amount, piconero_per_unit, provider, created_at_utc
                 FROM order_currency_metadata WHERE connection_id = ?1 AND payment_id = ?2",
                params![connection_id, payment_id],
                |row| {
                    let piconero_per_unit: i64 = row.get(4)?;
                    Ok(OrderCurrencyMetadataRow {
                        connection_id: row.get(0)?,
                        payment_id: row.get(1)?,
                        currency: row.get(2)?,
                        amount: row.get(3)?,
                        piconero_per_unit: piconero_per_unit as u64,
                        provider: row.get(5)?,
                        created_at: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Looks up currency metadata for every order of one connection at once -
    /// the orders-list/dashboard pages need this per-row, not one at a
    /// time, to avoid an N+1 query pattern when rendering a whole list.
    /// Returned as a map keyed by `payment_id` (already scoped to
    /// `connection_id` by the query) for callers to look up by, not a
    /// `Vec` they'd have to re-index themselves.
    pub fn list_order_currency_metadata_for_connection(
        &self,
        connection_id: &str,
    ) -> Result<std::collections::HashMap<String, OrderCurrencyMetadataRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT connection_id, payment_id, currency, amount, piconero_per_unit, provider, created_at_utc
             FROM order_currency_metadata WHERE connection_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![connection_id], |row| {
                let piconero_per_unit: i64 = row.get(4)?;
                Ok(OrderCurrencyMetadataRow {
                    connection_id: row.get(0)?,
                    payment_id: row.get(1)?,
                    currency: row.get(2)?,
                    amount: row.get(3)?,
                    piconero_per_unit: piconero_per_unit as u64,
                    provider: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().map(|row| (row.payment_id.clone(), row)).collect())
    }

    /// Inserts a new single-use connect token (WBS 1.4.1) - `token_hash` is
    /// already hashed by the caller (`shared::auth::hash_secret_token`),
    /// never the raw token; see `http/connect.rs::confirm_submit`.
    pub fn create_connect_token(&self, token_hash: &str, connection_id: &str, nonce: &str, created_at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO connect_tokens (token_hash, connection_id, nonce, created_at_utc) VALUES (?1, ?2, ?3, ?4)",
            params![token_hash, connection_id, nonce, created_at],
        )?;
        Ok(())
    }

    /// Atomically checks and consumes a connect token (WBS 1.4.1): the
    /// single `UPDATE ... WHERE token_hash = ? AND consumed_at_utc IS NULL AND
    /// created_at_utc >= ?` statement, checked by its affected-row count (same
    /// "did this actually change something" pattern [`Self::delete_session`]'s
    /// boolean return already uses), is the one database write that can
    /// never let two concurrent `/finish` calls for the same token both
    /// succeed - a naive "SELECT to check, then UPDATE" would race between
    /// the check and the write. `cutoff` (`now - ttl_seconds`) folds the TTL
    /// check into that same atomic statement, so an expired token is
    /// rejected exactly like an already-consumed one - not a separate check
    /// that could itself race against a concurrent consume.
    ///
    /// Returns the `connection_id` the token pointed at on success; `None`
    /// for an unknown, already-consumed, or expired token - deliberately
    /// indistinguishable to the caller (`http/connect.rs::finish` maps all
    /// three to a bare `401`), the same enumeration-defense principle used
    /// everywhere else in this crate.
    pub fn consume_connect_token(&self, token_hash: &str, now: i64, ttl_seconds: i64) -> Result<Option<String>> {
        let cutoff = now - ttl_seconds;
        let affected = self.conn.execute(
            "UPDATE connect_tokens SET consumed_at_utc = ?1 WHERE token_hash = ?2 AND consumed_at_utc IS NULL AND created_at_utc >= ?3",
            params![now, token_hash, cutoff],
        )?;
        if affected == 0 {
            return Ok(None);
        }
        // The row still exists (just consumed by the write above, not
        // deleted) - reading `connection_id` back here is safe from the same
        // race the UPDATE above already closed off: only the caller that
        // just won the atomic consume reaches this line for this token_hash.
        let connection_id = self.conn.query_row(
            "SELECT connection_id FROM connect_tokens WHERE token_hash = ?1",
            params![token_hash],
            |row| row.get(0),
        )?;
        Ok(Some(connection_id))
    }

    /// Direct row lookup by the tenant's public key rather than the
    /// connection's own id - for callers (currently just
    /// `http/dashboard.rs`'s WBS 1.3.2 tests) that only have the `pk_...`
    /// value a confirmation page showed, not the `store_connections.id` a
    /// browser form flow never hands back to the caller. The engine mints a
    /// fresh, effectively-unique public key per tenant, so this is expected
    /// to resolve to at most one row in practice, same as `get_by_id`.
    pub fn get_store_connection_by_public_key(&self, tenant_public_key: &str) -> Result<Option<StoreConnectionRow>> {
        self.conn
            .query_row(
                "SELECT id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, moneropay_endpoint, created_at_utc, fx_provider
                 FROM store_connections WHERE tenant_public_key = ?1",
                params![tenant_public_key],
                |row| {
                    Ok(StoreConnectionRow {
                        id: row.get(0)?,
                        user_id: row.get(1)?,
                        platform: row.get(2)?,
                        site_url: row.get(3)?,
                        tenant_public_key: row.get(4)?,
                        tenant_secret_token_encrypted: row.get(5)?,
                        moneropay_endpoint: row.get(6)?,
                        created_at: row.get(7)?,
                        fx_provider: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Updates `site_url` on an existing `store_connections` row - used when
    /// a merchant attaches a second (or replacement) storefront to a store
    /// they already have (`connect::confirm_existing_store`), so the
    /// dashboard reflects the most recent site this store was actually
    /// connected from rather than only ever showing wherever it was first
    /// created. Does not touch `platform` - a store's platform still names
    /// how it was first connected, not necessarily its most recent one.
    pub fn update_store_connection_site_url(&self, id: &str, site_url: &str) -> Result<()> {
        self.conn.execute("UPDATE store_connections SET site_url = ?2 WHERE id = ?1", params![id, site_url])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creating_a_user_then_reading_it_back_round_trips() {
        let db = Db::open_in_memory().unwrap();
        db.create_user("id-1", "a@example.com", "hash", false, 1000).unwrap();

        let row = db.get_user_by_email("a@example.com").unwrap().unwrap();
        assert_eq!(row.id, "id-1");
        assert_eq!(row.email, "a@example.com");
        assert_eq!(row.password_hash, "hash");
        assert_eq!(row.created_at, 1000);
        assert!(!row.is_admin, "an ordinary signup must never be an admin by default");
    }

    #[test]
    fn creating_a_user_with_is_admin_true_persists_and_round_trips_the_flag() {
        let db = Db::open_in_memory().unwrap();
        db.create_user("admin-1", "admin@example.com", "hash", true, 1000).unwrap();

        let by_email = db.get_user_by_email("admin@example.com").unwrap().unwrap();
        assert!(by_email.is_admin);
        let by_id = db.get_user_by_id("admin-1").unwrap().unwrap();
        assert!(by_id.is_admin);
    }

    #[test]
    fn a_setting_that_was_never_saved_reads_as_none() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.get_setting("rate_limit_per_ip_per_min").unwrap(), None);
        assert_eq!(db.list_settings().unwrap().len(), 0);
    }

    #[test]
    fn a_saved_setting_round_trips_and_a_second_save_overwrites_rather_than_erroring() {
        let db = Db::open_in_memory().unwrap();
        db.set_setting("rate_limit_per_ip_per_min", "10").unwrap();
        assert_eq!(db.get_setting("rate_limit_per_ip_per_min").unwrap().as_deref(), Some("10"));

        db.set_setting("rate_limit_per_ip_per_min", "25").unwrap();
        assert_eq!(db.get_setting("rate_limit_per_ip_per_min").unwrap().as_deref(), Some("25"));

        let all = db.list_settings().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all.get("rate_limit_per_ip_per_min").map(String::as_str), Some("25"));
    }

    #[test]
    fn deleting_a_setting_removes_its_row_entirely() {
        let db = Db::open_in_memory().unwrap();
        db.set_setting("k", "v").unwrap();
        db.delete_setting("k").unwrap();
        assert_eq!(db.get_setting("k").unwrap(), None);
    }

    #[test]
    fn setup_is_not_complete_until_explicitly_marked() {
        let db = Db::open_in_memory().unwrap();
        assert!(!db.is_setup_complete().unwrap(), "a fresh database must start unsetup");
        db.mark_setup_complete().unwrap();
        assert!(db.is_setup_complete().unwrap());
    }

    #[test]
    fn a_duplicate_email_is_rejected_as_a_unique_violation() {
        let db = Db::open_in_memory().unwrap();
        db.create_user("id-1", "a@example.com", "hash1", false, 1000).unwrap();

        let err = db.create_user("id-2", "a@example.com", "hash2", false, 2000).unwrap_err();
        assert!(err.is_unique_violation(), "expected a unique-violation error, got: {err:?}");
    }

    #[test]
    fn looking_up_an_unknown_email_returns_none_rather_than_an_error() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.get_user_by_email("nobody@example.com").unwrap().is_none());
    }

    #[test]
    fn a_created_session_can_be_found_by_its_token_hash_and_resolves_to_its_user() {
        let db = Db::open_in_memory().unwrap();
        db.create_user("user-1", "a@example.com", "hash", false, 1000).unwrap();
        db.create_session("hashed-token", "user-1", 2000).unwrap();

        let session = db.find_session("hashed-token").unwrap().unwrap();
        assert_eq!(session.token_hash, "hashed-token");
        assert_eq!(session.user_id, "user-1");
        assert_eq!(session.created_at, 2000);

        let user = db.get_user_by_id(&session.user_id).unwrap().unwrap();
        assert_eq!(user.email, "a@example.com");
    }

    #[test]
    fn looking_up_an_unknown_session_token_returns_none_rather_than_an_error() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.find_session("nonexistent").unwrap().is_none());
    }

    #[test]
    fn deleting_a_session_removes_it_and_reports_whether_a_row_was_actually_deleted() {
        let db = Db::open_in_memory().unwrap();
        db.create_user("user-1", "a@example.com", "hash", false, 1000).unwrap();
        db.create_session("hashed-token", "user-1", 2000).unwrap();

        assert!(db.delete_session("hashed-token").unwrap());
        assert!(db.find_session("hashed-token").unwrap().is_none());
        assert!(!db.delete_session("hashed-token").unwrap());
    }

    #[test]
    fn creating_a_store_connection_then_reading_it_back_round_trips() {
        let db = Db::open_in_memory().unwrap();
        db.create_user("user-1", "a@example.com", "hash", false, 1000).unwrap();
        db.create_store_connection(
            "conn-1",
            "user-1",
            "woocommerce",
            "https://shop.example.com",
            "pk_abc",
            "sk_abc",
            "http://127.0.0.1:8080",
            3000,
        )
        .unwrap();

        let row = db.get_store_connection_by_id("conn-1").unwrap().unwrap();
        assert_eq!(row.user_id, "user-1");
        assert_eq!(row.platform, "woocommerce");
        assert_eq!(row.site_url, "https://shop.example.com");
        assert_eq!(row.tenant_public_key, "pk_abc");
        assert_eq!(row.tenant_secret_token_encrypted, "sk_abc");
        assert_eq!(row.moneropay_endpoint, "http://127.0.0.1:8080");
        assert_eq!(row.created_at, 3000);
        assert_eq!(row.fx_provider, "coingecko", "every new store defaults to coingecko");
    }

    #[test]
    fn updating_a_store_connections_fx_provider_only_touches_that_field() {
        let db = Db::open_in_memory().unwrap();
        db.create_user("user-1", "a@example.com", "hash", false, 1000).unwrap();
        db.create_store_connection(
            "conn-1",
            "user-1",
            "woocommerce",
            "https://shop.example.com",
            "pk_abc",
            "sk_abc",
            "http://127.0.0.1:8080",
            3000,
        )
        .unwrap();

        db.update_store_connection_fx_provider("conn-1", "coingecko").unwrap();

        let row = db.get_store_connection_by_id("conn-1").unwrap().unwrap();
        assert_eq!(row.fx_provider, "coingecko");
        // Nothing else changed.
        assert_eq!(row.site_url, "https://shop.example.com");
        assert_eq!(row.tenant_public_key, "pk_abc");
    }

    #[test]
    fn migration_0008_backfills_a_stale_fixed_fx_provider_to_coingecko() {
        // Reproduces a real pre-upgrade database: a store still set to the
        // now-removed "fixed" provider (`create_store_connection` could
        // only ever write that value before migration 0007 added a real
        // choice, and every store predating that migration has it) must
        // land on "coingecko", not be left pointing at a provider name
        // that no longer means anything.
        let conn = Connection::open_in_memory().unwrap();
        shared::migrations::apply(&conn, &MIGRATIONS[..7]).unwrap();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, created_at) VALUES ('user-1', 'a@example.com', 'hash', 1000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO store_connections
                (id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, moneropay_endpoint, created_at, fx_provider)
             VALUES ('conn-1', 'user-1', 'woocommerce', 'https://shop.example.com', 'pk_abc', 'sk_abc', 'http://127.0.0.1:8080', 3000, 'fixed')",
            [],
        )
        .unwrap();

        shared::migrations::apply(&conn, MIGRATIONS).unwrap();

        let db = Db { conn };
        let row = db.get_store_connection_by_id("conn-1").unwrap().unwrap();
        assert_eq!(row.fx_provider, "coingecko");
    }

    #[test]
    fn looking_up_an_unknown_store_connection_id_returns_none_rather_than_an_error() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.get_store_connection_by_id("nonexistent").unwrap().is_none());
    }

    #[test]
    fn updating_a_store_connections_site_url_only_touches_that_field() {
        let db = Db::open_in_memory().unwrap();
        db.create_user("user-1", "a@example.com", "hash", false, 1000).unwrap();
        db.create_store_connection(
            "conn-1",
            "user-1",
            "woocommerce",
            "https://old-site.example.com",
            "pk_abc",
            "sk_abc",
            "http://127.0.0.1:8080",
            3000,
        )
        .unwrap();

        db.update_store_connection_site_url("conn-1", "https://new-site.example.com").unwrap();

        let row = db.get_store_connection_by_id("conn-1").unwrap().unwrap();
        assert_eq!(row.site_url, "https://new-site.example.com");
        // Nothing else changed.
        assert_eq!(row.platform, "woocommerce");
        assert_eq!(row.tenant_public_key, "pk_abc");
        assert_eq!(row.tenant_secret_token_encrypted, "sk_abc");
    }

    #[test]
    fn creating_a_store_connection_then_reading_it_back_by_public_key_round_trips() {
        let db = Db::open_in_memory().unwrap();
        db.create_user("user-2", "b@example.com", "hash", false, 1000).unwrap();
        db.create_store_connection(
            "conn-2",
            "user-2",
            "woocommerce",
            "https://shop.example.com",
            "pk_xyz",
            "sk_xyz",
            "http://127.0.0.1:8080",
            3000,
        )
        .unwrap();

        let row = db.get_store_connection_by_public_key("pk_xyz").unwrap().unwrap();
        assert_eq!(row.id, "conn-2");
        assert_eq!(row.user_id, "user-2");
    }

    #[test]
    fn looking_up_an_unknown_public_key_returns_none_rather_than_an_error() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.get_store_connection_by_public_key("pk_nonexistent").unwrap().is_none());
    }

    fn seed_connection_for_connect_token_tests(db: &Db) -> String {
        db.create_user("user-ct", "connect-tokens@example.com", "hash", false, 1000).unwrap();
        db.create_store_connection(
            "conn-ct",
            "user-ct",
            "woocommerce",
            "https://shop.example.com",
            "pk_ct",
            "sk_ct",
            "http://127.0.0.1:8080",
            1000,
        )
        .unwrap();
        "conn-ct".to_string()
    }

    #[test]
    fn creating_a_connect_token_then_consuming_it_returns_its_connection_id() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_connect_token("hashed-connect-token", &connection_id, "nonce-1", 2000).unwrap();

        let resolved = db.consume_connect_token("hashed-connect-token", 2001, 600).unwrap();
        assert_eq!(resolved, Some(connection_id));
    }

    #[test]
    fn consuming_the_same_connect_token_twice_only_succeeds_once() {
        // The load-bearing single-use proof at the `Db` layer - see
        // `Db::consume_connect_token`'s own doc comment on why the
        // underlying `UPDATE` must be atomic for this to hold under
        // concurrency, not just under this single-threaded test.
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_connect_token("hashed-connect-token", &connection_id, "nonce-1", 2000).unwrap();

        let first = db.consume_connect_token("hashed-connect-token", 2001, 600).unwrap();
        assert_eq!(first, Some(connection_id), "the first consume must actually succeed");

        let second = db.consume_connect_token("hashed-connect-token", 2002, 600).unwrap();
        assert_eq!(second, None, "a second consume of the same token must fail");
    }

    #[test]
    fn consuming_an_expired_connect_token_fails_as_if_it_never_existed() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_connect_token("hashed-connect-token", &connection_id, "nonce-1", 1000).unwrap();

        // created_at = 1000, ttl = 600 seconds - "now" = 1601 is one second
        // past the token's expiry window.
        let resolved = db.consume_connect_token("hashed-connect-token", 1601, 600).unwrap();
        assert_eq!(resolved, None, "expired token must not be consumable");
    }

    #[test]
    fn consuming_an_unknown_connect_token_returns_none() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.consume_connect_token("nonexistent-connect-token", 2000, 600).unwrap(), None);
    }

    #[test]
    fn reopening_an_existing_database_file_does_not_reapply_migrations() {
        // Same regression this guards against in `scanner::store`:
        // re-running the raw `CREATE TABLE` DDL against an already-migrated
        // file crashes with "table already exists".
        let path = std::env::temp_dir().join(format!("monokulo_test_{}.db", uuid::Uuid::new_v4()));
        let path_str = path.to_str().unwrap();

        let db = Db::open_file(path_str).unwrap();
        db.create_user("id-1", "a@example.com", "hash", false, 1000).unwrap();
        drop(db);

        let reopened = Db::open_file(path_str).unwrap();
        let row = reopened.get_user_by_email("a@example.com").unwrap().unwrap();
        assert_eq!(row.id, "id-1");

        drop(reopened);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn creating_order_fiat_metadata_then_reading_it_back_round_trips() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_order_currency_metadata(&connection_id, "pay_1", "USD", "25.00", 6_700_000_000, "fixed", 1000).unwrap();

        let row = db.get_order_currency_metadata(&connection_id, "pay_1").unwrap().unwrap();
        assert_eq!(row.connection_id, connection_id);
        assert_eq!(row.payment_id, "pay_1");
        assert_eq!(row.currency, "USD");
        assert_eq!(row.amount, "25.00");
        assert_eq!(row.piconero_per_unit, 6_700_000_000);
        assert_eq!(row.provider, "fixed");
        assert_eq!(row.created_at, 1000);
    }

    #[test]
    fn looking_up_fiat_metadata_for_an_unknown_payment_id_returns_none() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        assert!(db.get_order_currency_metadata(&connection_id, "nonexistent").unwrap().is_none());
    }

    #[test]
    fn fiat_metadata_is_scoped_by_connection_id_even_for_the_same_payment_id() {
        // Two different connections can each have their own order with the
        // same payment_id (the engine's payment_id is only unique within one
        // tenant) - the composite primary key must keep them apart.
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_user("user-2", "other@example.com", "hash", false, 1000).unwrap();
        db.create_store_connection(
            "conn-2",
            "user-2",
            "custom",
            "https://other.example.com",
            "pk_other",
            "sk_other",
            "http://127.0.0.1:8080",
            1000,
        )
        .unwrap();

        db.create_order_currency_metadata(&connection_id, "pay_shared", "USD", "10.00", 1_000_000, "fixed", 1000).unwrap();
        db.create_order_currency_metadata("conn-2", "pay_shared", "EUR", "20.00", 2_000_000, "coingecko", 2000).unwrap();

        let first = db.get_order_currency_metadata(&connection_id, "pay_shared").unwrap().unwrap();
        let second = db.get_order_currency_metadata("conn-2", "pay_shared").unwrap().unwrap();
        assert_eq!(first.currency, "USD");
        assert_eq!(second.currency, "EUR");
        assert_eq!(first.provider, "fixed");
        assert_eq!(second.provider, "coingecko");
    }

    #[test]
    fn listing_fiat_metadata_for_a_connection_returns_a_map_keyed_by_payment_id() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_order_currency_metadata(&connection_id, "pay_a", "USD", "10.00", 1_000_000, "fixed", 1000).unwrap();
        db.create_order_currency_metadata(&connection_id, "pay_b", "EUR", "20.00", 2_000_000, "fixed", 2000).unwrap();

        let map = db.list_order_currency_metadata_for_connection(&connection_id).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("pay_a").unwrap().currency, "USD");
        assert_eq!(map.get("pay_b").unwrap().currency, "EUR");
    }

    #[test]
    fn listing_fiat_metadata_for_a_connection_with_none_recorded_is_an_empty_map_not_an_error() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        assert!(db.list_order_currency_metadata_for_connection(&connection_id).unwrap().is_empty());
    }
}
