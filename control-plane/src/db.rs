//! The control-plane's own SQLite database — entirely separate from the
//! engine's (`moneropay_core::store::Store`); the two services never share a
//! database file or a connection.
//!
//! Deliberately mirrors the engine's `Store` (see `src/store.rs`) rather than
//! inventing a new shape: one struct wrapping a single `rusqlite::Connection`,
//! migrated on open via `shared::migrations::apply` (the same runner
//! `moneropay_core::store` uses — see WBS 0.5), with `open_in_memory`/
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
];

fn apply_migrations(conn: &Connection) -> rusqlite::Result<()> {
    shared::migrations::apply(conn, MIGRATIONS)
}

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
    /// `moneropay_core::store`'s own tests for the same
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
    pub fn create_user(&self, id: &str, email: &str, password_hash: &str, created_at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO users (id, email, password_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, email, password_hash, created_at],
        )?;
        Ok(())
    }

    /// Direct row lookup by email — used by tests to confirm what actually
    /// landed in the database (e.g. that `password_hash` is a real Argon2
    /// hash, never the plaintext password).
    pub fn get_user_by_email(&self, email: &str) -> Result<Option<UserRow>> {
        self.conn
            .query_row(
                "SELECT id, email, password_hash, created_at FROM users WHERE email = ?1",
                params![email],
                |row| {
                    Ok(UserRow {
                        id: row.get(0)?,
                        email: row.get(1)?,
                        password_hash: row.get(2)?,
                        created_at: row.get(3)?,
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
                "SELECT id, email, password_hash, created_at FROM users WHERE id = ?1",
                params![id],
                |row| {
                    Ok(UserRow {
                        id: row.get(0)?,
                        email: row.get(1)?,
                        password_hash: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Stores a new session. `token_hash` must already be hashed (see
    /// [`SessionRow`]'s doc comment) - `Db` never sees, and never needs to
    /// see, a raw session token.
    pub fn create_session(&self, token_hash: &str, user_id: &str, created_at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (token, user_id, created_at) VALUES (?1, ?2, ?3)",
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
                "SELECT token, user_id, created_at FROM sessions WHERE token = ?1",
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
        self.conn.execute(
            "INSERT INTO store_connections
                (id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, moneropay_endpoint, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
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
                "SELECT id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, moneropay_endpoint, created_at
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
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Inserts a new single-use connect token (WBS 1.4.1) - `token_hash` is
    /// already hashed by the caller (`shared::auth::hash_secret_token`),
    /// never the raw token; see `http/connect.rs::confirm_submit`.
    pub fn create_connect_token(&self, token_hash: &str, connection_id: &str, nonce: &str, created_at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO connect_tokens (token_hash, connection_id, nonce, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![token_hash, connection_id, nonce, created_at],
        )?;
        Ok(())
    }

    /// Atomically checks and consumes a connect token (WBS 1.4.1): the
    /// single `UPDATE ... WHERE token_hash = ? AND consumed_at IS NULL AND
    /// created_at >= ?` statement, checked by its affected-row count (same
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
            "UPDATE connect_tokens SET consumed_at = ?1 WHERE token_hash = ?2 AND consumed_at IS NULL AND created_at >= ?3",
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
                "SELECT id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, moneropay_endpoint, created_at
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
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creating_a_user_then_reading_it_back_round_trips() {
        let db = Db::open_in_memory().unwrap();
        db.create_user("id-1", "a@example.com", "hash", 1000).unwrap();

        let row = db.get_user_by_email("a@example.com").unwrap().unwrap();
        assert_eq!(row.id, "id-1");
        assert_eq!(row.email, "a@example.com");
        assert_eq!(row.password_hash, "hash");
        assert_eq!(row.created_at, 1000);
    }

    #[test]
    fn a_duplicate_email_is_rejected_as_a_unique_violation() {
        let db = Db::open_in_memory().unwrap();
        db.create_user("id-1", "a@example.com", "hash1", 1000).unwrap();

        let err = db.create_user("id-2", "a@example.com", "hash2", 2000).unwrap_err();
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
        db.create_user("user-1", "a@example.com", "hash", 1000).unwrap();
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
        db.create_user("user-1", "a@example.com", "hash", 1000).unwrap();
        db.create_session("hashed-token", "user-1", 2000).unwrap();

        assert!(db.delete_session("hashed-token").unwrap());
        assert!(db.find_session("hashed-token").unwrap().is_none());
        assert!(!db.delete_session("hashed-token").unwrap());
    }

    #[test]
    fn creating_a_store_connection_then_reading_it_back_round_trips() {
        let db = Db::open_in_memory().unwrap();
        db.create_user("user-1", "a@example.com", "hash", 1000).unwrap();
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
    }

    #[test]
    fn looking_up_an_unknown_store_connection_id_returns_none_rather_than_an_error() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.get_store_connection_by_id("nonexistent").unwrap().is_none());
    }

    #[test]
    fn creating_a_store_connection_then_reading_it_back_by_public_key_round_trips() {
        let db = Db::open_in_memory().unwrap();
        db.create_user("user-2", "b@example.com", "hash", 1000).unwrap();
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
        db.create_user("user-ct", "connect-tokens@example.com", "hash", 1000).unwrap();
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
        // Same regression this guards against in `moneropay_core::store`:
        // re-running the raw `CREATE TABLE` DDL against an already-migrated
        // file crashes with "table already exists".
        let path = std::env::temp_dir().join(format!("control_plane_test_{}.db", uuid::Uuid::new_v4()));
        let path_str = path.to_str().unwrap();

        let db = Db::open_file(path_str).unwrap();
        db.create_user("id-1", "a@example.com", "hash", 1000).unwrap();
        drop(db);

        let reopened = Db::open_file(path_str).unwrap();
        let row = reopened.get_user_by_email("a@example.com").unwrap().unwrap();
        assert_eq!(row.id, "id-1");

        drop(reopened);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }
}
