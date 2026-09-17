//! A generic, engine-agnostic SQLite migration runner.
//!
//! Moved into `shared::migrations` (WBS 0.5) so a second SQLite-backed
//! component (e.g. the monokulo database) can reuse the same
//! transactional, tracked-by-version migration mechanism instead of hand-rolling
//! its own and risking reintroducing the failure modes this module's test
//! guards against. The engine (`scanner`) keeps its own `MIGRATIONS`
//! list and `apply_migrations` wrapper in `src/store.rs` - only the generic
//! mechanism moved here, since a migration list of `include_str!("../migrations/...")`
//! paths is meaningless outside the crate that owns those files.
//!
//! Callers are responsible for any connection-level setup that isn't itself
//! persisted in the database file (e.g. `PRAGMA foreign_keys`) and that must run
//! *before* calling `apply`: `PRAGMA foreign_keys` is a no-op if issued inside a
//! transaction, and each migration here runs inside one.

use rusqlite::{Connection, params};

/// Each migration's DDL and its `schema_migrations` bookkeeping row commit together
/// or not at all. Without that, a crash in the window between the two re-runs the
/// migration on the next boot, which for any migration containing `CREATE TABLE` or
/// `DROP TABLE` fails outright and leaves the caller unable to start against its own
/// database.
///
/// Migrations already recorded in `schema_migrations` (created here if it doesn't
/// exist yet) are skipped, so calling this repeatedly against an already-migrated
/// database - e.g. every time a server restarts against its existing database file -
/// is safe and a no-op for anything already applied.
pub fn apply(conn: &Connection, migrations: &[(i64, &str)]) -> rusqlite::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY)")?;
    for (version, sql) in migrations {
        let already_applied: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            params![version],
            |row| row.get(0),
        )?;
        if !already_applied {
            // `unchecked_transaction` rather than `Connection::transaction` since
            // callers generally hold `&Connection`, not `&mut Connection` (see
            // `scanner::store`'s single-writer discipline for why the
            // borrow-level check `Connection::transaction` would enforce is
            // redundant there).
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute("INSERT INTO schema_migrations (version) VALUES (?1)", params![version])?;
            tx.commit()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_migration_leaves_neither_its_schema_changes_nor_its_version_row() {
        // Each migration body and its `schema_migrations` insert were two separate
        // statements, so a crash between them re-ran the migration on the next boot
        // - which, for any migration containing CREATE TABLE or DROP TABLE, fails
        // outright and leaves the caller unable to start against its own database.
        // Wrapping the pair in a transaction makes a half-applied migration
        // impossible; this drives that with a migration that succeeds partway and
        // then fails.
        let conn = Connection::open_in_memory().unwrap();
        let migrations: &[(i64, &str)] = &[
            (1, "CREATE TABLE ok_table (id INTEGER PRIMARY KEY);"),
            (2, "CREATE TABLE half_applied (id INTEGER PRIMARY KEY); THIS IS NOT VALID SQL;"),
        ];
        let err = apply(&conn, migrations).unwrap_err();
        let _ = err;

        let applied: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT version FROM schema_migrations ORDER BY version").unwrap();
            let rows = stmt.query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
            rows
        };
        assert_eq!(applied, vec![1], "the migration that succeeded is recorded; the one that failed is not");

        let half_applied_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='half_applied')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !half_applied_exists,
            "the failed migration's CREATE TABLE must have rolled back - otherwise re-running it next boot fails with 'table already exists'"
        );

        // And the retry a restart would perform now succeeds against a fixed
        // migration, rather than tripping over its own leftovers.
        let fixed: &[(i64, &str)] = &[migrations[0], (2, "CREATE TABLE half_applied (id INTEGER PRIMARY KEY);")];
        apply(&conn, fixed).unwrap();
    }
}
