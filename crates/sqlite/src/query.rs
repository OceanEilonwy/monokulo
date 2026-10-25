//! Read-only SQL for the admin Query page: one statement an admin typed,
//! run against a service's own database, with nothing it can change and
//! no secret it can read.
//!
//! [`run`] takes a connection it is given (a read connection of the
//! service's pool) and, for the one statement:
//!
//! - refuses anything but a single statement that only reads: SQLite's
//!   authorizer allows only `SELECT`, reading columns, calling functions
//!   and recursive CTEs, so a write, `ATTACH`, `PRAGMA` or a transaction
//!   is refused while it is prepared, and a statement SQLite says isn't
//!   read-only (`VACUUM`) is refused before it runs;
//! - reads every column the [`Classification`] calls [`Exposure::Secret`],
//!   or doesn't name, as NULL (the authorizer's `SQLITE_IGNORE`), so
//!   joins, views, CTEs, `hex()` or `substr()` can't reach the value, and
//!   marks a result column that comes from one as hidden;
//! - stops at [`Limits`]: a time, a number of rows, bytes per value and
//!   bytes in all, saying what was cut.
//!
//! The authorizer and the time limit are taken off the connection again
//! when it returns, so the pool's other jobs never meet them.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// What one query may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Rows returned; past them the result is marked cut.
    pub max_rows: usize,
    /// Bytes kept of one text value (a blob keeps half as many, shown as
    /// hex); the value's full size is still reported.
    pub max_cell_bytes: usize,
    /// Bytes of values in the whole result; past them it is cut.
    pub max_result_bytes: usize,
    /// How long the statement may run before it is stopped.
    #[serde(with = "millis")]
    pub timeout: Duration,
}

impl Limits {
    /// The Query page's: 1,000 rows, 64 KiB a value, 4 MiB in all, 5 s.
    pub const PAGE: Limits = Limits {
        max_rows: 1_000,
        max_cell_bytes: 64 * 1024,
        max_result_bytes: 4 * 1024 * 1024,
        timeout: Duration::from_secs(5),
    };

    /// A download's: 10,000 rows, 32 MiB in all.
    pub const EXPORT: Limits = Limits {
        max_rows: 10_000,
        max_cell_bytes: 64 * 1024,
        max_result_bytes: 32 * 1024 * 1024,
        timeout: Duration::from_secs(5),
    };

    /// These limits, none above `ceiling`'s: what a service allows
    /// whatever a caller asks for.
    pub fn within(self, ceiling: Limits) -> Limits {
        Limits {
            max_rows: self.max_rows.min(ceiling.max_rows),
            max_cell_bytes: self.max_cell_bytes.min(ceiling.max_cell_bytes),
            max_result_bytes: self.max_result_bytes.min(ceiling.max_result_bytes),
            timeout: self.timeout.min(ceiling.timeout),
        }
    }
}

mod millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_millis() as u64)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_millis(u64::deserialize(d)?))
    }
}

/// Whether a column's values may be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    Visible,
    /// A password hash, a token, key material: read as NULL, shown as
    /// hidden.
    Secret,
}

/// Every column of a database's tables, each with its [`Exposure`]. A
/// column it doesn't name is treated as secret, and each service has a
/// test ([`unclassified`]) that fails when a migration adds a column
/// nobody has classified yet.
#[derive(Debug, Clone, Copy)]
pub struct Classification {
    pub columns: &'static [(&'static str, &'static str, Exposure)],
}

impl Classification {
    pub fn exposure(&self, table: &str, column: &str) -> Exposure {
        // SQLite's own tables (the schema, the AUTOINCREMENT counters) and a
        // row's id hold nothing secret.
        if table.to_ascii_lowercase().starts_with("sqlite_")
            || column.is_empty()
            || ["rowid", "_rowid_", "oid"].contains(&column.to_ascii_lowercase().as_str())
        {
            return Exposure::Visible;
        }
        self.columns
            .iter()
            .find(|(t, c, _)| t.eq_ignore_ascii_case(table) && c.eq_ignore_ascii_case(column))
            .map_or(Exposure::Secret, |(_, _, exposure)| *exposure)
    }
}

/// One column of a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultColumn {
    pub name: String,
    /// Its declared type, when it comes straight from a table's column.
    pub decl_type: Option<String>,
    /// It comes from a secret column: every value in it is NULL.
    pub hidden: bool,
}

/// One value of a result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Cell {
    Null,
    Integer {
        value: i64,
    },
    Real {
        value: f64,
    },
    /// `text` is cut at [`Limits::max_cell_bytes`] when `len` (the whole
    /// value's size in bytes) is larger.
    Text {
        text: String,
        len: usize,
    },
    /// `hex` holds the first bytes, `len` the whole size.
    Blob {
        hex: String,
        len: usize,
    },
}

impl Cell {
    /// Whether this value was cut.
    pub fn is_cut(&self) -> bool {
        match self {
            Cell::Text { text, len } => text.len() < *len,
            Cell::Blob { hex, len } => hex.len() / 2 < *len,
            _ => false,
        }
    }

    /// The value as text, for a download: a blob as its hex.
    pub fn to_text(&self) -> Option<String> {
        match self {
            Cell::Null => None,
            Cell::Integer { value } => Some(value.to_string()),
            Cell::Real { value } => Some(value.to_string()),
            Cell::Text { text, .. } => Some(text.clone()),
            Cell::Blob { hex, .. } => Some(hex.clone()),
        }
    }
}

/// What a query returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<ResultColumn>,
    pub rows: Vec<Vec<Cell>>,
    /// Rows stopped at [`Limits::max_rows`] or [`Limits::max_result_bytes`].
    pub cut: bool,
    pub elapsed_ms: u64,
    /// The secret columns the statement read (as `table.column`), each of
    /// which it got as NULL.
    pub hidden: Vec<String>,
}

/// Why a query didn't run, or stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueryError {
    #[error("There is nothing to run.")]
    Empty,
    /// More than one statement: only one runs at a time.
    #[error("Only one statement runs at a time.")]
    NotOneStatement,
    /// `action` (`UPDATE`, `PRAGMA`, `ATTACH`...) would change something,
    /// or step outside this database.
    #[error("{action} is refused: this page only reads.")]
    Refused { action: String },
    /// SQLite couldn't read the statement; `offset` is the byte where it
    /// stopped, when it says.
    #[error("{message}")]
    Syntax {
        message: String,
        offset: Option<usize>,
    },
    #[error("Stopped after {seconds} s.")]
    Timeout { seconds: u64 },
    /// Anything else SQLite reports while running it.
    #[error("{message}")]
    Failed { message: String },
}

/// What the authorizer saw while the statement was prepared.
#[derive(Default)]
struct Seen {
    /// The action it refused, named for the person who wrote it.
    refused: Option<String>,
    /// It refused a change to SQLite's own schema table, the first thing
    /// `CREATE` and `DROP` do: the statement's own first word names it.
    refused_schema: bool,
    /// The secret columns it read as NULL.
    hidden: BTreeSet<String>,
}

/// Takes the authorizer and the time limit off the connection however
/// [`run`] ends.
struct Unhook<'a>(&'a Connection);

impl Drop for Unhook<'_> {
    fn drop(&mut self) {
        let _ = self
            .0
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        let _ = self.0.progress_handler(0, None::<fn() -> bool>);
    }
}

/// The word for an action the authorizer refuses.
fn refused_action(action: &AuthAction<'_>) -> String {
    match action {
        AuthAction::Insert { .. } => "INSERT",
        AuthAction::Update { .. } => "UPDATE",
        AuthAction::Delete { .. } => "DELETE",
        AuthAction::Pragma { .. } => "PRAGMA",
        AuthAction::Attach { .. } => "ATTACH",
        AuthAction::Detach { .. } => "DETACH",
        AuthAction::Transaction { .. } => "A transaction",
        AuthAction::Savepoint { .. } => "SAVEPOINT",
        AuthAction::AlterTable { .. } => "ALTER TABLE",
        AuthAction::Reindex { .. } => "REINDEX",
        AuthAction::Analyze { .. } => "ANALYZE",
        AuthAction::CreateIndex { .. }
        | AuthAction::CreateTable { .. }
        | AuthAction::CreateTempIndex { .. }
        | AuthAction::CreateTempTable { .. }
        | AuthAction::CreateTempTrigger { .. }
        | AuthAction::CreateTempView { .. }
        | AuthAction::CreateTrigger { .. }
        | AuthAction::CreateView { .. }
        | AuthAction::CreateVtable { .. } => "CREATE",
        AuthAction::DropIndex { .. }
        | AuthAction::DropTable { .. }
        | AuthAction::DropTempIndex { .. }
        | AuthAction::DropTempTable { .. }
        | AuthAction::DropTempTrigger { .. }
        | AuthAction::DropTempView { .. }
        | AuthAction::DropTrigger { .. }
        | AuthAction::DropView { .. }
        | AuthAction::DropVtable { .. } => "DROP",
        _ => "That statement",
    }
    .to_string()
}

/// Runs `sql`, one statement that only reads, on `conn` (see the module
/// doc for what it refuses and hides).
pub fn run(
    conn: &Connection,
    sql: &str,
    limits: Limits,
    classes: &Classification,
) -> Result<QueryResult, QueryError> {
    let sql = sql.trim();
    if sql.is_empty() {
        return Err(QueryError::Empty);
    }
    let started = Instant::now();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let _unhook = Unhook(conn);
    {
        let seen = Arc::clone(&seen);
        let classes = *classes;
        conn.authorizer(Some(move |context: AuthContext<'_>| {
            let mut seen = seen.lock().unwrap_or_else(|e| e.into_inner());
            match context.action {
                AuthAction::Select | AuthAction::Function { .. } | AuthAction::Recursive => {
                    Authorization::Allow
                }
                AuthAction::Read {
                    table_name,
                    column_name,
                } => match classes.exposure(table_name, column_name) {
                    Exposure::Visible => Authorization::Allow,
                    Exposure::Secret => {
                        seen.hidden.insert(format!("{table_name}.{column_name}"));
                        Authorization::Ignore
                    }
                },
                other => {
                    let on_schema = matches!(
                        other,
                        AuthAction::Insert { table_name }
                            | AuthAction::Update { table_name, .. }
                            | AuthAction::Delete { table_name }
                            if table_name.to_ascii_lowercase().starts_with("sqlite_")
                    );
                    if on_schema {
                        seen.refused_schema = true;
                    } else {
                        seen.refused.get_or_insert_with(|| refused_action(&other));
                    }
                    Authorization::Deny
                }
            }
        }))
        .map_err(failed)?;
    }
    let deadline = started + limits.timeout;
    conn.progress_handler(1_000, Some(move || Instant::now() >= deadline))
        .map_err(failed)?;

    let first_word = sql.split_whitespace().next().unwrap_or("").to_uppercase();
    let refused = |seen: &Arc<Mutex<Seen>>| {
        let seen = seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.refused
            .clone()
            .or_else(|| seen.refused_schema.then(|| first_word.clone()))
    };
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) => {
            return Err(match refused(&seen) {
                Some(action) => QueryError::Refused { action },
                None => from_sqlite(e, limits),
            })
        }
    };
    if !stmt.readonly() {
        return Err(QueryError::Refused { action: first_word });
    }
    // A secret column read as NULL has no origin any more, so a result
    // column is marked hidden when its name mentions a secret column the
    // statement read (`password_hash`, `hex(password_hash)`); its values
    // are NULL whether it is marked or not.
    let secret_names: Vec<String> = seen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .hidden
        .iter()
        .filter_map(|read| read.split_once('.').map(|(_, c)| c.to_ascii_lowercase()))
        .collect();
    let columns: Vec<ResultColumn> = stmt
        .columns_with_metadata()
        .iter()
        .zip(stmt.columns())
        .map(|(meta, column)| {
            let name = meta.name().to_string();
            let from_secret = match (meta.table_name(), meta.origin_name()) {
                (Some(table), Some(origin)) => classes.exposure(table, origin) == Exposure::Secret,
                _ => false,
            };
            let lower = name.to_ascii_lowercase();
            ResultColumn {
                hidden: from_secret || secret_names.iter().any(|s| lower.contains(s.as_str())),
                decl_type: column.decl_type().map(str::to_string),
                name,
            }
        })
        .collect();
    let width = columns.len();
    let mut rows = Vec::new();
    let mut bytes = 0usize;
    let mut cut = false;
    let mut cursor = stmt.query([]).map_err(|e| from_sqlite(e, limits))?;
    while let Some(row) = cursor.next().map_err(|e| from_sqlite(e, limits))? {
        if rows.len() == limits.max_rows {
            cut = true;
            break;
        }
        let mut cells = Vec::with_capacity(width);
        for i in 0..width {
            let value = row.get_ref(i).map_err(|e| from_sqlite(e, limits))?;
            let cell = cell(value, limits.max_cell_bytes);
            bytes += match &cell {
                Cell::Text { text, .. } => text.len(),
                Cell::Blob { hex, .. } => hex.len(),
                _ => 8,
            };
            cells.push(cell);
        }
        rows.push(cells);
        if bytes > limits.max_result_bytes {
            cut = true;
            break;
        }
    }
    drop(cursor);
    drop(stmt);
    let hidden = seen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .hidden
        .iter()
        .cloned()
        .collect();
    Ok(QueryResult {
        columns,
        rows,
        cut,
        elapsed_ms: started.elapsed().as_millis() as u64,
        hidden,
    })
}

fn cell(value: ValueRef<'_>, max_bytes: usize) -> Cell {
    match value {
        ValueRef::Null => Cell::Null,
        ValueRef::Integer(value) => Cell::Integer { value },
        ValueRef::Real(value) => Cell::Real { value },
        ValueRef::Text(bytes) => {
            let whole = String::from_utf8_lossy(bytes);
            let mut end = whole.len().min(max_bytes);
            while !whole.is_char_boundary(end) {
                end -= 1;
            }
            Cell::Text {
                text: whole[..end].to_string(),
                len: bytes.len(),
            }
        }
        ValueRef::Blob(bytes) => Cell::Blob {
            hex: hex::encode(&bytes[..bytes.len().min(max_bytes / 2)]),
            len: bytes.len(),
        },
    }
}

fn failed(e: rusqlite::Error) -> QueryError {
    QueryError::Failed {
        message: e.to_string(),
    }
}

fn from_sqlite(e: rusqlite::Error, limits: Limits) -> QueryError {
    match e {
        rusqlite::Error::MultipleStatement => QueryError::NotOneStatement,
        rusqlite::Error::SqlInputError { msg, offset, .. } => QueryError::Syntax {
            message: msg,
            offset: usize::try_from(offset).ok(),
        },
        rusqlite::Error::SqliteFailure(error, _)
            if error.code == rusqlite::ErrorCode::OperationInterrupted =>
        {
            QueryError::Timeout {
                seconds: limits.timeout.as_secs().max(1),
            }
        }
        rusqlite::Error::SqliteFailure(_, Some(message)) => QueryError::Syntax {
            message,
            offset: None,
        },
        other => failed(other),
    }
}

/// A table of the database, for the page's table list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableInfo {
    pub name: String,
    /// Its rows, counted up to [`ROW_COUNT_CAP`].
    pub rows: u64,
    pub columns: Vec<ColumnInfo>,
}

impl TableInfo {
    /// Whether there are more rows than were counted.
    pub fn rows_capped(&self) -> bool {
        self.rows > ROW_COUNT_CAP
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub decl_type: String,
    pub hidden: bool,
}

/// Rows counted per table: a big table's count stops past here, so the
/// list never waits on a full scan.
pub const ROW_COUNT_CAP: u64 = 100_000;

/// The database's own tables (not SQLite's), with their columns and rows.
pub fn schema(conn: &Connection, classes: &Classification) -> rusqlite::Result<Vec<TableInfo>> {
    let names: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    names
        .into_iter()
        .map(|name| {
            let columns = conn
                .prepare("SELECT name, type FROM pragma_table_info(?1) ORDER BY cid")?
                .query_map([&name], |row| {
                    let column: String = row.get(0)?;
                    Ok(ColumnInfo {
                        hidden: classes.exposure(&name, &column) == Exposure::Secret,
                        name: column,
                        decl_type: row.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let quoted = name.replace('"', "\"\"");
            let rows: i64 = conn.query_row(
                &format!(
                    "SELECT count(*) FROM (SELECT 1 FROM \"{quoted}\" LIMIT {})",
                    ROW_COUNT_CAP + 1
                ),
                [],
                |row| row.get(0),
            )?;
            Ok(TableInfo {
                name,
                rows: rows.unsigned_abs(),
                columns,
            })
        })
        .collect()
}

/// The columns of `conn`'s tables that `classes` doesn't name, and the
/// ones it names that no table has, each as `table.column`: for each
/// service's test that every column has been classified.
pub fn unclassified(
    conn: &Connection,
    classes: &Classification,
) -> rusqlite::Result<(Vec<String>, Vec<String>)> {
    let mut present = BTreeSet::new();
    for table in schema(conn, classes)? {
        for column in table.columns {
            present.insert(format!("{}.{}", table.name, column.name));
        }
    }
    let listed: BTreeSet<String> = classes
        .columns
        .iter()
        .map(|(t, c, _)| format!("{t}.{c}"))
        .collect();
    Ok((
        present.difference(&listed).cloned().collect(),
        listed.difference(&present).cloned().collect(),
    ))
}

/// Monokulo's request to the engine's `POST /api/v1/admin/query`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryRequest {
    pub sql: String,
    /// What monokulo asks for; the engine keeps them within its own
    /// ceiling ([`Limits::EXPORT`]).
    pub limits: Limits,
}

/// The engine's answer: what the statement returned, or why it didn't
/// run. Both are a `200`: a refused statement is an answer, not a failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryResponse {
    Result(QueryResult),
    Error(QueryError),
}

/// The engine's answer to `GET /api/v1/admin/query/schema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaResponse {
    pub tables: Vec<TableInfo>,
}

/// How many queries may run at once, counted while each [`Running`] lives:
/// each holds a read connection for up to its time limit.
#[derive(Debug)]
pub struct Running {
    count: &'static std::sync::atomic::AtomicUsize,
}

impl Running {
    /// One more running query on `count`, unless `max` already are.
    pub fn take(count: &'static std::sync::atomic::AtomicUsize, max: usize) -> Option<Running> {
        use std::sync::atomic::Ordering;
        count
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < max).then_some(n + 1)
            })
            .ok()
            .map(|_| Running { count })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.count.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests;
