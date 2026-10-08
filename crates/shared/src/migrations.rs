//! A generic, engine-agnostic SQLite migration runner.
//!
//! Moved into `shared::migrations` (WBS 0.5) so a second SQLite-backed
//! component (e.g. the monokulo database) can reuse the same
//! transactional, tracked-by-version migration mechanism instead of hand-rolling
//! its own and risking reintroducing the failure modes this module's test
//! guards against. The engine (`engine`) keeps its own `MIGRATIONS`
//! list and `apply_migrations` wrapper in `src/store.rs` - only the generic
//! mechanism moved here, since a migration list of `include_str!("../migrations/...")`
//! paths is meaningless outside the crate that owns those files.
//!
//! Callers are responsible for any connection-level setup that isn't itself
//! persisted in the database file (e.g. `PRAGMA foreign_keys`) and that must run
//! *before* calling `apply`: `PRAGMA foreign_keys` is a no-op if issued inside a
//! transaction, and each migration here runs inside one.

use rusqlite::{params, Connection};

/// The first line of a migration that rebuilds a table other tables point
/// at. SQLite can only change a table's constraints by copying it to a new
/// table, and with foreign keys on, dropping the old one fails. Such a
/// migration runs with foreign keys off, as SQLite's own procedure for
/// altering a table describes, and is refused unless
/// `PRAGMA foreign_key_check` finds nothing wrong before it commits.
pub const FOREIGN_KEYS_OFF: &str = "-- foreign_keys: off";

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
    // A duplicated or out-of-order version would be skipped as "already
    // applied", or applied out of order, without a word.
    if let Some(pair) = migrations.windows(2).find(|w| w[0].0 >= w[1].0) {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "migration versions must strictly increase: {} then {}",
            pair[0].0, pair[1].0
        )));
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY)",
    )?;
    for (version, sql) in migrations {
        let already_applied: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            params![version],
            |row| row.get(0),
        )?;
        if !already_applied {
            // `PRAGMA foreign_keys` is a no-op inside a transaction: set
            // around it, and put back however it went.
            let rebuild = sql.starts_with(FOREIGN_KEYS_OFF);
            let restore: bool =
                rebuild && conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
            if restore {
                conn.execute_batch("PRAGMA foreign_keys = OFF")?;
            }
            let applied = apply_one(conn, *version, sql, rebuild);
            if restore {
                conn.execute_batch("PRAGMA foreign_keys = ON")?;
            }
            applied?;
        }
    }
    Ok(())
}

fn apply_one(conn: &Connection, version: i64, sql: &str, rebuild: bool) -> rusqlite::Result<()> {
    // `unchecked_transaction` rather than `Connection::transaction` since
    // callers generally hold `&Connection`, not `&mut Connection` (see
    // `engine::store`'s single-writer discipline for why the borrow-level
    // check `Connection::transaction` would enforce is redundant there).
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(sql)?;
    if rebuild {
        let broken: i64 =
            tx.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        if broken > 0 {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
                Some(format!(
                    "migration {version} left {broken} row(s) pointing at rows that don't exist"
                )),
            ));
        }
    }
    tx.execute(
        "INSERT INTO schema_migrations (version) VALUES (?1)",
        params![version],
    )?;
    tx.commit()
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
            (
                2,
                "CREATE TABLE half_applied (id INTEGER PRIMARY KEY); THIS IS NOT VALID SQL;",
            ),
        ];
        let err = apply(&conn, migrations).unwrap_err();
        let _ = err;

        let applied: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT version FROM schema_migrations ORDER BY version")
                .unwrap();
            let rows = stmt
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            rows
        };
        assert_eq!(
            applied,
            vec![1],
            "the migration that succeeded is recorded; the one that failed is not"
        );

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
        let fixed: &[(i64, &str)] = &[
            migrations[0],
            (2, "CREATE TABLE half_applied (id INTEGER PRIMARY KEY);"),
        ];
        apply(&conn, fixed).unwrap();
    }

    fn foreign_keys_on(conn: &Connection) -> bool {
        conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap()
    }

    /// A table other tables point at is rebuilt with a new constraint, as
    /// SQLite allows only with foreign keys off; they are on again after.
    #[test]
    fn a_migration_marked_foreign_keys_off_can_rebuild_a_parent_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        let setup = "CREATE TABLE parent (id TEXT PRIMARY KEY, a TEXT, n INTEGER, UNIQUE (a, n));
                     CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id TEXT NOT NULL REFERENCES parent(id));
                     INSERT INTO parent VALUES ('p1', 'x', 1);
                     INSERT INTO child (parent_id) VALUES ('p1');";
        let rebuild = format!(
            "{FOREIGN_KEYS_OFF}
             CREATE TABLE parent_new (id TEXT PRIMARY KEY, a TEXT, b TEXT, n INTEGER, UNIQUE (b, n));
             INSERT INTO parent_new SELECT id, a, a, n FROM parent;
             DROP TABLE parent;
             ALTER TABLE parent_new RENAME TO parent;"
        );
        // Without the marker the same rebuild is refused.
        let unmarked = rebuild.replacen(FOREIGN_KEYS_OFF, "", 1);
        assert!(apply(&conn, &[(1, setup), (2, &unmarked)]).is_err());
        assert!(foreign_keys_on(&conn));

        apply(&conn, &[(1, setup), (2, &rebuild)]).unwrap();
        assert!(foreign_keys_on(&conn));
        let child_parent: String = conn
            .query_row(
                "SELECT p.b FROM child c JOIN parent p ON p.id = c.parent_id",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(child_parent, "x");
        // The new constraint is the one in force.
        conn.execute("INSERT INTO parent VALUES ('p2', 'x', 'y', 1)", [])
            .unwrap();
    }

    /// A rebuild that loses rows other tables point at is refused, and
    /// rolled back.
    #[test]
    fn a_rebuild_that_leaves_a_dangling_reference_is_refused() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        let setup = "CREATE TABLE parent (id TEXT PRIMARY KEY);
                     CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id TEXT NOT NULL REFERENCES parent(id));
                     INSERT INTO parent VALUES ('p1');
                     INSERT INTO child (parent_id) VALUES ('p1');";
        let lossy = format!(
            "{FOREIGN_KEYS_OFF}
             CREATE TABLE parent_new (id TEXT PRIMARY KEY);
             DROP TABLE parent;
             ALTER TABLE parent_new RENAME TO parent;"
        );
        let error = apply(&conn, &[(1, setup), (2, &lossy)]).unwrap_err();
        assert!(error.to_string().contains("pointing at rows"), "{error}");
        assert!(foreign_keys_on(&conn));
        let parents: i64 = conn
            .query_row("SELECT COUNT(*) FROM parent", [], |row| row.get(0))
            .unwrap();
        assert_eq!(parents, 1, "rolled back");
    }
}
