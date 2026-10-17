//! The runner against a small database of its own: what it refuses, what
//! it hides, and where it stops.

use super::*;

const CLASSES: Classification = Classification {
    columns: &[
        ("users", "id", Exposure::Visible),
        ("users", "email", Exposure::Visible),
        ("users", "password_hash", Exposure::Secret),
        ("notes", "id", Exposure::Visible),
        ("notes", "body", Exposure::Visible),
        ("notes", "data", Exposure::Visible),
    ],
};

/// A database with two users, a note per user and a column nobody has
/// classified (`users.nickname`), on a connection that can only read.
fn db() -> Connection {
    let path = std::env::temp_dir().join(format!("sqlite_query_{}.db", uuid::Uuid::new_v4()));
    let path = path.to_string_lossy().into_owned();
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT, password_hash TEXT, nickname TEXT);
             INSERT INTO users VALUES (1, 'ann@example.org', '$argon2id$secret-one', 'annie');
             INSERT INTO users VALUES (2, 'bob@example.org', '$argon2id$secret-two', 'bobby');
             CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT, data BLOB);
             INSERT INTO notes VALUES (1, 'hello', x'9f3a1c07');
             CREATE VIEW user_secrets AS SELECT id, password_hash FROM users;",
        )
        .unwrap();
    crate::sqlite::open_reader(&path).unwrap()
}

fn run_page(conn: &Connection, sql: &str) -> Result<QueryResult, QueryError> {
    run(conn, sql, Limits::PAGE, &CLASSES)
}

/// Every value of a result, as text (NULL as `None`).
fn values(result: &QueryResult) -> Vec<Vec<Option<String>>> {
    result
        .rows
        .iter()
        .map(|row| row.iter().map(Cell::to_text).collect())
        .collect()
}

#[test]
fn a_select_returns_its_columns_and_rows() {
    let conn = db();
    let result = run_page(&conn, "  SELECT id, email FROM users ORDER BY id;  ").unwrap();
    assert_eq!(
        result.columns,
        [
            ResultColumn {
                name: "id".into(),
                decl_type: Some("INTEGER".into()),
                hidden: false
            },
            ResultColumn {
                name: "email".into(),
                decl_type: Some("TEXT".into()),
                hidden: false
            },
        ]
    );
    assert_eq!(
        values(&result),
        [
            [Some("1".into()), Some("ann@example.org".into())],
            [Some("2".into()), Some("bob@example.org".into())],
        ]
    );
    assert!(!result.cut);
    assert!(result.hidden.is_empty());
}

#[test]
fn anything_that_changes_data_or_steps_outside_is_refused() {
    let conn = db();
    for (sql, action) in [
        ("INSERT INTO notes (body) VALUES ('x')", "INSERT"),
        ("UPDATE users SET email = 'x'", "UPDATE"),
        ("DELETE FROM notes", "DELETE"),
        ("CREATE TABLE t (a)", "CREATE"),
        ("CREATE TEMP VIEW v AS SELECT 1", "CREATE"),
        ("DROP TABLE notes", "DROP"),
        ("ALTER TABLE notes ADD COLUMN x", "ALTER TABLE"),
        ("ATTACH DATABASE ':memory:' AS other", "ATTACH"),
        ("PRAGMA query_only = OFF", "PRAGMA"),
        ("PRAGMA table_info(users)", "PRAGMA"),
        ("BEGIN", "A transaction"),
        ("SAVEPOINT s", "SAVEPOINT"),
        (
            "WITH x AS (SELECT 1) INSERT INTO notes (body) SELECT * FROM x",
            "INSERT",
        ),
        ("VACUUM", "VACUUM"),
    ] {
        assert_eq!(
            run_page(&conn, sql),
            Err(QueryError::Refused {
                action: action.into()
            }),
            "{sql}"
        );
    }
    // Nothing changed.
    let count: i64 = conn
        .query_row("SELECT count(*) FROM notes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn only_one_statement_runs_and_nothing_is_nothing() {
    let conn = db();
    assert_eq!(
        run_page(&conn, "SELECT 1; SELECT 2"),
        Err(QueryError::NotOneStatement)
    );
    assert_eq!(
        run_page(&conn, "SELECT 1; DELETE FROM notes"),
        Err(QueryError::Refused {
            action: "DELETE".into()
        })
    );
    // A trailing comment is no second statement.
    assert!(run_page(&conn, "SELECT 1; -- done").is_ok());
    assert_eq!(run_page(&conn, "  \n "), Err(QueryError::Empty));
}

#[test]
fn a_mistake_says_where() {
    let conn = db();
    match run_page(&conn, "SELECT * FORM users") {
        Err(QueryError::Syntax { message, offset }) => {
            assert!(
                message.contains("FORM") || message.contains("syntax"),
                "{message}"
            );
            assert_eq!(offset, Some(9));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_secret_column_reads_as_null_however_it_is_reached() {
    let conn = db();
    for sql in [
        "SELECT password_hash FROM users",
        "SELECT u.password_hash FROM users u JOIN notes n ON n.id = u.id",
        "SELECT password_hash FROM user_secrets",
        "WITH s AS (SELECT password_hash AS p FROM users) SELECT p FROM s",
        "SELECT hex(password_hash) FROM users",
        "SELECT substr(password_hash, 1, 3) FROM users",
        "SELECT (SELECT password_hash FROM users LIMIT 1)",
        // Nor can a search tell what it holds.
        "SELECT id FROM users WHERE password_hash LIKE '$argon2id$secret%'",
        // A column nobody classified counts as secret.
        "SELECT nickname FROM users",
    ] {
        let result = run_page(&conn, sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        let shown: Vec<String> = values(&result).into_iter().flatten().flatten().collect();
        assert!(
            shown.iter().all(|v| !v.contains("secret")
                && !v.contains("7365637265")
                && v != "$ar"
                && !v.contains("annie")),
            "{sql}: {shown:?}"
        );
        assert!(!result.hidden.is_empty(), "{sql}");
        if sql.starts_with("SELECT id FROM users WHERE") {
            assert!(result.rows.is_empty(), "{sql}");
        }
    }
    let direct = run_page(&conn, "SELECT id, password_hash FROM users").unwrap();
    assert!(!direct.columns[0].hidden);
    assert!(direct.columns[1].hidden);
    assert_eq!(direct.hidden, ["users.password_hash"]);
}

#[test]
fn rows_values_and_time_have_limits() {
    let conn = db();
    let few = Limits {
        max_rows: 3,
        max_cell_bytes: 4,
        ..Limits::PAGE
    };
    let result = run(
        &conn,
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n LIMIT 10) SELECT i, 'abcdefgh' FROM n",
        few,
        &CLASSES,
    )
    .unwrap();
    assert_eq!(result.rows.len(), 3);
    assert!(result.cut);
    assert_eq!(
        result.rows[0][1],
        Cell::Text {
            text: "abcd".into(),
            len: 8
        }
    );
    assert!(result.rows[0][1].is_cut());

    // A value is never cut inside a character.
    let result = run(
        &conn,
        "SELECT 'aé'",
        Limits {
            max_cell_bytes: 2,
            ..Limits::PAGE
        },
        &CLASSES,
    )
    .unwrap();
    assert_eq!(
        result.rows[0][0],
        Cell::Text {
            text: "a".into(),
            len: 3
        }
    );

    // Bytes in all.
    let result = run(
        &conn,
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n LIMIT 100) SELECT 'abcdefghij' FROM n",
        Limits { max_result_bytes: 25, ..Limits::PAGE },
        &CLASSES,
    )
    .unwrap();
    assert_eq!(result.rows.len(), 3);
    assert!(result.cut);

    let started = Instant::now();
    assert_eq!(
        run(
            &conn,
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n",
            Limits { timeout: Duration::from_millis(200), ..Limits::PAGE },
            &CLASSES,
        ),
        Err(QueryError::Timeout { seconds: 1 })
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_blob_shows_its_size_and_first_bytes() {
    let conn = db();
    let result = run_page(&conn, "SELECT data FROM notes").unwrap();
    assert_eq!(
        result.rows[0][0],
        Cell::Blob {
            hex: "9f3a1c07".into(),
            len: 4
        }
    );
}

#[test]
fn the_connection_is_left_as_it_was() {
    let conn = db();
    let _ = run_page(&conn, "SELECT password_hash FROM users");
    let _ = run(
        &conn,
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n",
        Limits {
            timeout: Duration::from_millis(50),
            ..Limits::PAGE
        },
        &CLASSES,
    );
    // No authorizer: the pool's own reads see every column, and PRAGMA works.
    let hash: String = conn
        .query_row("SELECT password_hash FROM users WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(hash, "$argon2id$secret-one");
    conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    // No time limit left over either.
    std::thread::sleep(Duration::from_millis(60));
    let n: i64 = conn
        .query_row(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n LIMIT 200000) SELECT count(*) FROM n",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 200_000);
}

#[test]
fn the_schema_lists_tables_columns_and_rows_and_which_columns_are_hidden() {
    let conn = db();
    let tables = schema(&conn, &CLASSES).unwrap();
    assert_eq!(
        tables.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["notes", "users"]
    );
    let users = &tables[1];
    assert_eq!(users.rows, 2);
    assert!(!users.rows_capped());
    assert_eq!(
        users
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.decl_type.as_str(), c.hidden))
            .collect::<Vec<_>>(),
        [
            ("id", "INTEGER", false),
            ("email", "TEXT", false),
            ("password_hash", "TEXT", true),
            ("nickname", "TEXT", true),
        ]
    );
    assert_eq!(
        unclassified(&conn, &CLASSES).unwrap(),
        (vec!["users.nickname".to_string()], Vec::new())
    );
}

#[test]
fn limits_never_go_past_a_ceiling() {
    let asked = Limits {
        max_rows: 1_000_000,
        max_cell_bytes: 10,
        max_result_bytes: usize::MAX,
        timeout: Duration::from_secs(60),
    };
    assert_eq!(
        asked.within(Limits::PAGE),
        Limits {
            max_cell_bytes: 10,
            ..Limits::PAGE
        }
    );
}
