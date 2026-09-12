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
const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../migrations/0001_init.sql"))];

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
