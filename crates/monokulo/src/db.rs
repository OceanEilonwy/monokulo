//! The monokulo's own SQLite database — entirely separate from the
//! engine's (`engine::store::Store`); the two services never share a
//! database file or a connection.
//!
//! [`Db`] wraps one `rusqlite::Connection` and holds every query, migrated
//! on open via `shared::migrations::apply` (the same runner `engine::store`
//! uses). Handlers reach it through [`Database`]: read-only connections for
//! reads, one writing connection for writes, each on its own thread.

use parking_lot::Mutex;
pub use shared::ids::{ConnectionId, EngineWalletId, OrderId, UserId, WalletId};
use shared::sqlite::{Pool, PoolError, Unsigned};
use shared::xmr_amount::Piconero;
use std::sync::Arc;

use crate::fx_provider_settings::FxProviderSettings;
use rusqlite::{params, Connection, OptionalExtension};

/// Reads a `store_connections.fx_providers` cell: a JSON array of provider
/// names (migration 0027). A bare name is tolerated as a one-element list, so
/// a row written before that migration's rename, or by anything ignoring
/// the new format, still means what it said.
pub fn parse_fx_providers(raw: &str) -> Vec<String> {
    match serde_json::from_str::<Vec<String>>(raw) {
        Ok(list) => list,
        Err(_) if raw.trim().is_empty() => Vec::new(),
        Err(_) => vec![raw.trim().to_string()],
    }
}

/// Every migration file, applied in order, exactly once each — tracked in
/// `schema_migrations` (created by `shared::migrations::apply`), so
/// re-running this against an already-migrated database file (e.g. every
/// process restart) is a safe no-op.
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_init.sql")),
    (2, include_str!("../migrations/0002_sessions.sql")),
    (3, include_str!("../migrations/0003_store_connections.sql")),
    (4, include_str!("../migrations/0004_connect_tokens.sql")),
    (
        5,
        include_str!("../migrations/0005_order_fiat_metadata.sql"),
    ),
    (
        6,
        include_str!("../migrations/0006_order_fiat_metadata_provider.sql"),
    ),
    (7, include_str!("../migrations/0007_store_fx_provider.sql")),
    (
        8,
        include_str!("../migrations/0008_remove_fixed_fx_provider.sql"),
    ),
    (
        9,
        include_str!("../migrations/0009_rename_fiat_to_currency.sql"),
    ),
    (
        10,
        include_str!("../migrations/0010_utc_suffix_date_columns.sql"),
    ),
    (
        11,
        include_str!("../migrations/0011_settings_and_admin.sql"),
    ),
    (12, include_str!("../migrations/0012_invites.sql")),
    (13, include_str!("../migrations/0013_currencies.sql")),
    (
        14,
        include_str!("../migrations/0014_store_base_currency.sql"),
    ),
    (
        15,
        include_str!("../migrations/0015_confirmation_thresholds.sql"),
    ),
    (
        16,
        include_str!("../migrations/0016_order_confirmation_snapshot.sql"),
    ),
    (17, include_str!("../migrations/0017_user_theme.sql")),
    (
        18,
        include_str!("../migrations/0018_rename_payment_id_to_order_id.sql"),
    ),
    (19, include_str!("../migrations/0019_store_domains.sql")),
    (20, include_str!("../migrations/0020_embed_restriction.sql")),
    (
        21,
        include_str!("../migrations/0021_order_created_with_key.sql"),
    ),
    (22, include_str!("../migrations/0022_pos_orders.sql")),
    (23, include_str!("../migrations/0023_order_source.sql")),
    (
        24,
        include_str!("../migrations/0024_saved_log_searches.sql"),
    ),
    (
        25,
        include_str!("../migrations/0025_store_client_logging.sql"),
    ),
    (26, include_str!("../migrations/0026_user_timezone.sql")),
    (
        27,
        include_str!("../migrations/0027_store_fx_provider_order.sql"),
    ),
    (
        28,
        include_str!("../migrations/0028_store_fx_provider_settings.sql"),
    ),
    (
        29,
        include_str!("../migrations/0029_drop_store_connection_endpoint.sql"),
    ),
    (
        30,
        include_str!("../migrations/0030_dashboard_order_requests.sql"),
    ),
    (31, include_str!("../migrations/0031_wallets.sql")),
    (
        32,
        include_str!("../migrations/0032_store_wallet_periods.sql"),
    ),
    (33, include_str!("../migrations/0033_retired_wallets.sql")),
];

fn apply_migrations(conn: &Connection) -> rusqlite::Result<()> {
    shared::migrations::apply(conn, MIGRATIONS)
}

/// Known credentials every test fixture seeds via [`Db::seed_test_admin`] -
/// public (not `#[cfg(test)]`-gated on the constants themselves) so a test in
/// any module can log in as this account without redefining the literal.
pub const TEST_ADMIN_EMAIL: &str = "admin@monokulo.test";
pub const TEST_ADMIN_PASSWORD: &str = "correct horse battery staple admin";

/// [`TEST_ADMIN_PASSWORD`]'s Argon2id hash, computed once and kept here:
/// hashing happens only on the blocking pool (`shared::password::run`),
/// and a synchronous test fixture has no business waiting for it.
#[cfg(any(test, feature = "test-support"))]
const TEST_ADMIN_PASSWORD_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$DbEpa9uloUGQgzhdDwix1g$fzhZPyckZkFC3STfRtsxa6LuDNdV9ej3r7sxuX1S1yw";

pub struct Db {
    conn: Connection,
}

/// How long a session is good for after it was made: 30 days. A session
/// that never expired would stay usable from a lost device, or a leaked
/// cookie, for as long as the row existed.
pub const SESSION_LIFETIME_SECONDS: i64 = 30 * 24 * 60 * 60;

/// How long an invite link can be redeemed after it was made: 14 days. A
/// link that never expired would stay good for an account for as long as
/// the row existed, in an inbox or an unactioned request's row.
pub const INVITE_LIFETIME_SECONDS: i64 = 14 * 24 * 60 * 60;

#[derive(Debug, Clone)]
pub struct PosOrderRow {
    pub order_id: OrderId,
    pub backgrounded: bool,
    pub cancelled_at: Option<i64>,
    pub created_at: i64,
}

/// What monokulo knows about an order beyond the engine's view, for the
/// store's orders list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OrderListingDetail {
    /// `pos`, `dashboard`, `api`, `website`, or `None` when unknown.
    pub source: Option<String>,
    /// Set when the order was cancelled on the POS.
    pub pos_cancelled_at: Option<i64>,
}

impl Db {
    /// The connection itself, for a test that sets up a state no method
    /// makes (a store from before wallets).
    #[cfg(test)]
    pub(crate) fn conn_for_test(&self) -> &Connection {
        &self.conn
    }

    pub fn insert_pos_order(
        &self,
        connection_id: &ConnectionId,
        order_id: &OrderId,
        request_key: Option<&str>,
        reference: Option<&str>,
        created_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            // A retry of the same sale writes nothing new.
            "INSERT INTO pos_orders (connection_id, order_id, request_key, reference, created_at_utc) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (connection_id, order_id) DO NOTHING",
            params![connection_id, order_id, request_key, reference, created_at],
        )?;
        Ok(())
    }

    pub fn pos_order_by_request_key(
        &self,
        connection_id: &ConnectionId,
        request_key: &str,
    ) -> Result<Option<OrderId>> {
        self.conn
            .query_row(
                "SELECT order_id FROM pos_orders WHERE connection_id = ?1 AND request_key = ?2",
                params![connection_id, request_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(DbError::from)
    }

    pub fn get_pos_order(
        &self,
        connection_id: &ConnectionId,
        order_id: &OrderId,
    ) -> Result<Option<PosOrderRow>> {
        self.conn.query_row(
            "SELECT order_id, backgrounded, cancelled_at_utc, created_at_utc FROM pos_orders WHERE connection_id = ?1 AND order_id = ?2",
            params![connection_id, order_id], |row| Ok(PosOrderRow {
                order_id: row.get(0)?, backgrounded: row.get(1)?, cancelled_at: row.get(2)?, created_at: row.get(3)?,
            }),
        ).optional().map_err(DbError::from)
    }

    pub fn list_pos_orders(
        &self,
        connection_id: &ConnectionId,
        limit: i64,
        offset: i64,
        search: Option<&str>,
    ) -> Result<Vec<PosOrderRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT order_id, backgrounded, cancelled_at_utc, created_at_utc FROM pos_orders
             WHERE connection_id = ?1 AND (?4 IS NULL OR instr(lower(order_id), lower(?4)) > 0 OR instr(lower(coalesce(reference, '')), lower(?4)) > 0)
             ORDER BY created_at_utc DESC, order_id DESC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![connection_id, limit, offset, search], |row| {
            Ok(PosOrderRow {
                order_id: row.get(0)?,
                backgrounded: row.get(1)?,
                cancelled_at: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(DbError::from)
    }

    pub fn count_pos_orders(
        &self,
        connection_id: &ConnectionId,
        search: Option<&str>,
    ) -> Result<i64> {
        self.conn.query_row("SELECT count(*) FROM pos_orders WHERE connection_id = ?1 AND (?2 IS NULL OR instr(lower(order_id), lower(?2)) > 0 OR instr(lower(coalesce(reference, '')), lower(?2)) > 0)", params![connection_id, search], |row| row.get(0)).map_err(DbError::from)
    }

    /// The POS rows among `order_ids`, in no particular order.
    pub fn get_pos_orders(
        &self,
        connection_id: &ConnectionId,
        order_ids: &[OrderId],
    ) -> Result<Vec<PosOrderRow>> {
        let mut rows = Vec::with_capacity(order_ids.len());
        let mut stmt = self.conn.prepare(
            "SELECT order_id, backgrounded, cancelled_at_utc, created_at_utc FROM pos_orders WHERE connection_id = ?1 AND order_id = ?2",
        )?;
        for order_id in order_ids {
            if let Some(row) = stmt
                .query_row(params![connection_id, order_id], |row| {
                    Ok(PosOrderRow {
                        order_id: row.get(0)?,
                        backgrounded: row.get(1)?,
                        cancelled_at: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                })
                .optional()?
            {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    /// Source and POS cancellation for each of `order_ids` monokulo knows
    /// anything about; orders it knows nothing about are left out.
    pub fn order_listing_details(
        &self,
        connection_id: &ConnectionId,
        order_ids: &[OrderId],
    ) -> Result<std::collections::HashMap<OrderId, OrderListingDetail>> {
        let mut details = std::collections::HashMap::new();
        let mut source = self.conn.prepare(
            "SELECT source FROM order_currency_metadata WHERE connection_id = ?1 AND order_id = ?2",
        )?;
        let mut pos = self.conn.prepare(
            "SELECT cancelled_at_utc FROM pos_orders WHERE connection_id = ?1 AND order_id = ?2",
        )?;
        for order_id in order_ids {
            let known_source: Option<Option<String>> = source
                .query_row(params![connection_id, order_id], |row| row.get(0))
                .optional()?;
            let pos_row: Option<Option<i64>> = pos
                .query_row(params![connection_id, order_id], |row| row.get(0))
                .optional()?;
            if known_source.is_none() && pos_row.is_none() {
                continue;
            }
            details.insert(
                order_id.clone(),
                OrderListingDetail {
                    source: known_source
                        .flatten()
                        .or_else(|| pos_row.map(|_| "pos".to_string())),
                    pos_cancelled_at: pos_row.flatten(),
                },
            );
        }
        Ok(details)
    }

    pub fn background_pos_order(
        &self,
        connection_id: &ConnectionId,
        order_id: &OrderId,
    ) -> Result<bool> {
        Ok(self.conn.execute("UPDATE pos_orders SET backgrounded = 1 WHERE connection_id = ?1 AND order_id = ?2 AND cancelled_at_utc IS NULL", params![connection_id, order_id])? > 0)
    }

    pub fn cancel_pos_order(
        &self,
        connection_id: &ConnectionId,
        order_id: &OrderId,
        cancelled_at: i64,
    ) -> Result<bool> {
        Ok(self.conn.execute("UPDATE pos_orders SET cancelled_at_utc = ?3, backgrounded = 1 WHERE connection_id = ?1 AND order_id = ?2 AND cancelled_at_utc IS NULL", params![connection_id, order_id, cancelled_at])? > 0)
    }

    /// Takes a cancellation back, leaving the order backgrounded (for
    /// review): for an order a payment reached between the check that it
    /// had none and the cancellation.
    pub fn uncancel_pos_order(
        &self,
        connection_id: &ConnectionId,
        order_id: &OrderId,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE pos_orders SET cancelled_at_utc = NULL WHERE connection_id = ?1 AND order_id = ?2 AND cancelled_at_utc IS NOT NULL",
            params![connection_id, order_id],
        )? > 0)
    }
}

/// One connection behind a lock: the one in-memory connection a test's
/// [`Database`] runs on.
pub type SharedDb = Arc<Mutex<Db>>;

/// How monokulo reaches its database: a pool of read-only connections and
/// one writing connection, each on its own thread (`shared::sqlite::Pool`).
/// Reads run side by side (WAL) and never wait behind a write; writes run
/// one at a time, in order. Nothing runs on a Tokio worker thread.
///
/// A job that writes, or that reads and then writes on what it read, goes
/// to [`Database::write`] whole, so no other write lands in between.
#[derive(Clone)]
pub struct Database {
    reads: Pool<Db>,
    writes: Pool<Db>,
}

impl Database {
    /// The database file at `path`, migrated, with `read_connections`
    /// readers (at least one).
    pub fn open(path: &str, read_connections: usize) -> Result<Self> {
        let writer = Db::open_file(path)?;
        let readers = (0..read_connections.max(1))
            .map(|_| Db::open_reader(path))
            .collect::<Result<Vec<_>>>()?;
        let unavailable = |e: std::io::Error| DbError::Pool(PoolError::Unavailable(e.to_string()));
        Ok(Database {
            reads: Pool::start("monokulo-db-read", readers).map_err(unavailable)?,
            writes: Pool::start("monokulo-db-write", vec![writer]).map_err(unavailable)?,
        })
    }

    /// Every job on the caller, on `db`, locked for the job: for tests and
    /// their in-memory databases (which can't be opened twice). Reads still
    /// can't write (`Db::read_only`), so a read that writes fails its tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn inline(db: SharedDb) -> Self {
        Database {
            reads: Pool::Inline(db.clone()),
            writes: Pool::Inline(db),
        }
    }

    /// Runs `f` on a read-only connection.
    pub async fn read<T, E>(
        &self,
        f: impl FnOnce(&Db) -> std::result::Result<T, E> + Send + 'static,
    ) -> std::result::Result<T, E>
    where
        T: Send + 'static,
        E: From<PoolError> + Send + 'static,
    {
        if self.reads.is_inline() {
            self.reads.run(move |db| db.read_only(|| f(db))).await
        } else {
            self.reads.run(f).await
        }
    }

    /// Runs `f` on the writing connection.
    pub async fn write<T, E>(
        &self,
        f: impl FnOnce(&Db) -> std::result::Result<T, E> + Send + 'static,
    ) -> std::result::Result<T, E>
    where
        T: Send + 'static,
        E: From<PoolError> + Send + 'static,
    {
        self.writes.run(f).await
    }

    /// The test's connection, for setting up and checking state directly.
    /// Only for an inline database; production code can't call it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn lock(&self) -> parking_lot::MutexGuard<'_, Db> {
        match &self.writes {
            Pool::Inline(db) => db.lock(),
            Pool::Threads { .. } => panic!("Database::lock is only for an inline test database"),
        }
    }
}

/// A search saved on the Logs page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedLogSearch {
    pub id: String,
    pub name: String,
    /// The page's query string, without the leading `?`.
    pub query_string: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Pool(#[from] PoolError),
}

impl DbError {
    /// True for a `UNIQUE` constraint violation (e.g. a duplicate `email`),
    /// as opposed to any other database failure. Callers use this to map a
    /// duplicate-email signup to `409 Conflict` rather than `500` — see
    /// `engine::store`'s own tests for the same
    /// `SqliteFailure`/`ErrorCode::ConstraintViolation` shape this checks.
    pub fn is_unique_violation(&self) -> bool {
        matches!(
            self,
            DbError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error {
                    code: rusqlite::ErrorCode::ConstraintViolation,
                    ..
                },
                _
            ))
        )
    }
}

type Result<T> = std::result::Result<T, DbError>;

#[derive(Clone)]
pub struct UserRow {
    pub id: UserId,
    pub email: String,
    pub password_hash: String,
    pub created_at: i64,
    /// `true` only for the one instance-admin account the first-run setup
    /// wizard creates (`http::setup`) - `false` for every ordinary merchant
    /// user `POST /dashboard/signup` creates. Gates the nav's own "admin"
    /// link and `/dashboard/admin/*` - see `AuthedUser`'s own doc comment.
    pub is_admin: bool,
    /// This user's stored light/dark preference (`users.theme`, migration
    /// 0017) - read fresh on every request via [`AuthedUser`]'s own DB
    /// lookup, so a theme change (`POST /dashboard/theme`) takes effect on
    /// the very next page load with no session/cookie invalidation needed.
    pub theme: Theme,
    /// The time zone this user chose (`users.timezone`, migration 0026), or
    /// `None` for automatic: `browser_timezone`, else UTC.
    pub timezone: Option<String>,
    /// Not stored: the browser's own zone on the current request (the `tz`
    /// cookie), filled in by [`AuthedUser`](crate::http::AuthedUser) for an
    /// automatic `timezone`.
    pub browser_timezone: Option<String>,
}

/// A user's stored light/dark preference. `System` (the default, and every
/// pre-migration row's backfilled value) means "no explicit choice - follow
/// the browser's own `prefers-color-scheme`"; `Light`/`Dark` mean the user
/// explicitly overrode it via the nav's theme-toggle form, which always wins
/// over the OS preference either way (`_styles.html.hbs`'s
/// `:root[data-theme="dark"]` block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    System,
    Light,
    Dark,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    /// An unrecognized stored value (shouldn't happen - only this type ever
    /// writes the column - but a raw `TEXT` column is still not a closed
    /// set at the SQL level) falls back to `System`, the safe "just follow
    /// the OS" default, rather than erroring on every page load.
    pub fn from_db_str(s: &str) -> Theme {
        match s {
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            _ => Theme::System,
        }
    }

    /// The toggle's cycle order: System -> Light -> Dark -> System.
    pub fn next(self) -> Theme {
        match self {
            Theme::System => Theme::Light,
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::System,
        }
    }
}

/// A row from `sessions`. `token_hash` is the SHA-256 hash of the raw
/// bearer token (see the `sessions` migration's own comment on why) - it
/// is never the raw token a client actually presents.
pub struct SessionRow {
    pub token_hash: shared::auth::TokenHash,
    pub user_id: UserId,
    pub created_at: i64,
}

/// A row from `store_connections`. `tenant_secret_token_encrypted` holds the
/// engine's `sk_...` secret token encrypted at rest (WBS 1.2.3, via
/// `crate::crypto::encrypt`) - `Db` itself is crypto-unaware and just stores
/// whatever string it's given; see `http/connections.rs` for where the real
/// encrypt/decrypt calls happen.
/// One `store_domains` row - see migration `0019_store_domains.sql` and
/// `crate::embed_domains`.
#[derive(Debug, Clone)]
pub struct StoreDomainRow {
    pub id: String,
    pub connection_id: ConnectionId,
    pub domain: String,
    pub token: String,
    pub created_at: i64,
    pub verified_at: Option<i64>,
    pub failing_since: Option<i64>,
    pub last_checked_at: Option<i64>,
    pub last_error: Option<String>,
}

const STORE_DOMAIN_COLUMNS: &str =
    "id, connection_id, domain, token, created_at_utc, verified_at_utc, failing_since_utc, last_checked_at_utc, last_error";

fn store_domain_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoreDomainRow> {
    Ok(StoreDomainRow {
        id: row.get(0)?,
        connection_id: row.get(1)?,
        domain: row.get(2)?,
        token: row.get(3)?,
        created_at: row.get(4)?,
        verified_at: row.get(5)?,
        failing_since: row.get(6)?,
        last_checked_at: row.get(7)?,
        last_error: row.get(8)?,
    })
}

pub struct StoreConnectionRow {
    pub id: ConnectionId,
    pub user_id: UserId,
    pub platform: String,
    pub site_url: String,
    pub tenant_public_key: String,
    pub tenant_secret_token_encrypted: String,
    pub created_at: i64,
    /// The exchange-rate providers *this store* has turned on, most
    /// preferred first (`exchange_rate_config::{COINGECKO, COINMARKETCAP}`) -
    /// a genuine per-merchant choice set on its settings page, not an
    /// instance-wide setting. A non-XMR order is priced by the first of
    /// these that is available on this instance and has a rate for the
    /// order's currency; a provider not listed is off for this store.
    /// Irrelevant for an XMR-denominated order, which always uses the
    /// trivial identity rate - see `exchange_rate_config::
    /// ExchangeRateProviders::piconero_per_unit_for`.
    pub fx_providers: Vec<String>,
    /// The unit custom confirmation thresholds are denominated in, and what
    /// an order's own currency is converted into (via XMR, when they
    /// differ) to decide which threshold applies - see migration
    /// `0014_store_base_currency.sql` and `crate::currencies`. Selected
    /// explicitly at store creation; changing it later clears every custom
    /// threshold (`Db::set_store_base_currency`'s own doc comment).
    pub base_currency: String,
    /// Per-provider limits this store sets - today when Haveno may quote
    /// for it (`crate::fx_provider_settings`). Defaults for a store that
    /// never touched them.
    pub fx_provider_settings: FxProviderSettings,
    /// The wallet this store takes payments into (migration 0031); `None`
    /// for a store made before wallets, until it's matched to one.
    pub wallet_id: Option<WalletId>,
}

/// How a wallet came to monokulo (`wallets.origin`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletOrigin {
    /// Made on the "Create a new wallet" page, in its owner's browser.
    Created,
    /// Keys pasted in ("Bring your own wallet").
    Imported,
}

impl WalletOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Imported => "imported",
        }
    }

    fn parse(text: &str) -> Self {
        if text == "created" {
            Self::Created
        } else {
            Self::Imported
        }
    }
}

/// A merchant's named wallet (`wallets`, migration 0031).
#[derive(Clone, Debug)]
pub struct WalletRow {
    pub id: WalletId,
    pub user_id: UserId,
    pub name: String,
    pub network: String,
    pub primary_address: String,
    pub engine_wallet_id: EngineWalletId,
    pub origin: WalletOrigin,
    /// How the recovery phrase of a created wallet was saved
    /// (`crate::wallets::Backup`).
    pub backup: Option<String>,
    pub created_at: i64,
    /// When it was retired: its keys deleted, offered nowhere.
    pub retired_at: Option<i64>,
}

pub struct NewWalletRow<'a> {
    pub id: &'a WalletId,
    pub user_id: &'a UserId,
    pub name: &'a str,
    pub network: &'a str,
    pub primary_address: &'a str,
    pub engine_wallet_id: &'a EngineWalletId,
    pub origin: WalletOrigin,
    pub backup: Option<&'a str>,
    pub created_at: i64,
}

pub struct WalletSummary {
    pub wallet: WalletRow,
    pub store_count: u64,
}

/// One entry in a wallet's history (`wallet_events`).
pub struct WalletEventRow {
    pub at: i64,
    /// `created`, `imported`, `renamed` (detail: the old name),
    /// `store_connected`, `store_changed_to` (a store changed to this
    /// wallet) or `store_changed_away` (a store changed to another one),
    /// the store ones' detail being the store's id; `retired` (its keys
    /// deleted) or `restored` (brought back with them).
    pub kind: String,
    pub detail: String,
}

/// A stretch of time a store took payments into one wallet
/// (`store_wallet_periods`, migration 0032).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreWalletPeriod {
    pub connection_id: ConnectionId,
    /// `None` once the wallet was deleted.
    pub wallet_id: Option<WalletId>,
    pub wallet_name: Option<String>,
    pub wallet_retired: bool,
    pub from: i64,
    /// `None` for the store's current wallet.
    pub until: Option<i64>,
    /// Orders the store made in the period.
    pub orders: u64,
}

/// Changing a store's wallet (`Db::change_store_wallet`).
#[derive(Debug, PartialEq, Eq)]
pub enum ChangeStoreWallet {
    Changed,
    /// The store already takes payments into that wallet.
    Unchanged,
}

/// A row from `order_currency_metadata` (`docs/fx_refactor.md` Phase 1.2) -
/// what a customer was quoted for one order (in fiat, or in XMR itself),
/// recorded at creation time. The engine has no concept of this at all;
/// this is monokulo's own, sole copy of it - see the migration's own
/// comment on why (and the accepted durability trade-off that implies,
/// `docs/fx_refactor.md` decision 4).
pub struct OrderCurrencyMetadataRow {
    pub connection_id: ConnectionId,
    pub order_id: OrderId,
    pub currency: String,
    pub amount: String,
    pub piconero_per_unit: Piconero,
    /// Which provider produced `piconero_per_unit` - `"xmr"` (the trivial
    /// identity rate), `"coingecko"`, `"unknown"` for a row recorded before
    /// this field existed (migration 0006), or a stale `"fixed"` for a row
    /// recorded before that provider's removal.
    pub provider: String,
    pub created_at: i64,
    /// This store's `base_currency` at the moment this order was created -
    /// `None` for a row predating migration 0016, or one created directly
    /// against the engine's API - see that migration's own doc comment.
    pub store_base_currency: Option<String>,
    /// The rate used to convert this order's amount into
    /// `store_base_currency` terms - `None` either because the order's own
    /// currency already *was* the base currency (no conversion needed), or
    /// the row predates this snapshot entirely.
    pub base_currency_piconero_per_unit: Option<Piconero>,
    /// The confirmations_required this order was actually created with on
    /// the engine (`crate::confirmation_thresholds::Resolution::confirmations_required`) -
    /// `None` only for a row predating this snapshot.
    pub confirmations_required_applied: Option<u64>,
    /// Whether whoever created the order held the store's secret key (a
    /// shop's server, the dashboard or the POS) rather than being a browser
    /// page using the public embed library - see migration
    /// `0021_order_created_with_key.sql`.
    pub created_with_key: bool,
}

/// An `order_currency_metadata` row, columns in the order both queries
/// select them; every number is read with a checked conversion.
fn metadata_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OrderCurrencyMetadataRow> {
    Ok(OrderCurrencyMetadataRow {
        connection_id: row.get(0)?,
        order_id: row.get(1)?,
        currency: row.get(2)?,
        amount: row.get(3)?,
        piconero_per_unit: row.get(4)?,
        provider: row.get(5)?,
        created_at: row.get(6)?,
        store_base_currency: row.get(7)?,
        base_currency_piconero_per_unit: row.get(8)?,
        confirmations_required_applied: row.get::<_, Option<Unsigned<u64>>>(9)?.map(|v| v.0),
        created_with_key: row.get(10)?,
    })
}

/// One row from the static `currencies` reference table - see that
/// migration's own comment. `tickers` is the raw JSON array text as stored;
/// `crate::currencies::resolve_currency` is what actually parses and
/// matches against it.
pub struct CurrencyRow {
    pub canonical_code: String,
    pub description: String,
    pub tickers_json: String,
}

/// One custom, amount-tiered confirmation threshold - see that migration's
/// own comment. `unit_amount` is a decimal string, denominated in the
/// owning store's `base_currency` at the moment it was saved.
#[derive(Debug, Clone)]
pub struct ConfirmationThresholdRow {
    pub id: String,
    pub connection_id: ConnectionId,
    pub unit_amount: String,
    pub confirmations_required: u64,
    pub created_at: i64,
}

/// A pending "let me in" request from the public `/request-invite` form -
/// see `invite_requests`'s own migration comment. Only ever fetched for an
/// `actioned = 0` row (the admin invites page's own listing) or a specific
/// id (the just-deleted-row addendum, `http::invites`).
pub struct InviteRequestRow {
    pub id: String,
    pub email: String,
    pub message: String,
    pub created_at: i64,
    /// This request's own still-unused invite link, encrypted at rest -
    /// `None` for the rare/pathological case of a request whose link was
    /// somehow already consumed without the request itself being
    /// auto-actioned, or one that never got a link at all. The admin
    /// invites page decrypts this (and only this - see the `invite_links`
    /// migration's own doc comment on why this is the one reversibly
    /// stored credential in this crate) to build its "email invite"
    /// `mailto:` link.
    pub invite_token_encrypted: Option<String>,
}

/// What redeeming a presented invite token during signup can result in -
/// see [`Db::redeem_invite_and_create_user`]'s own doc comment for the
/// ordering (and the one accepted edge case) this maps to.
#[derive(Debug, PartialEq, Eq)]
pub enum RedeemInviteResult {
    /// No `invite_links` row has this token's hash with `used_at_utc IS
    /// NULL` - either the token is entirely unknown, or (the common real
    /// case) it already redeemed once before.
    InvalidOrAlreadyUsed,
    /// The token was valid and has now been claimed, but `email` was
    /// already registered - see this method's own doc comment on why the
    /// token stays burned regardless.
    DuplicateEmail,
    Created,
}

impl Db {
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        shared::sqlite::configure_writer(&conn)?;
        apply_migrations(&conn)?;
        Ok(Db { conn })
    }

    /// The writing connection to the database file at `path`: WAL, foreign
    /// keys and the rest of `shared::sqlite::configure_writer`, then any
    /// pending migrations.
    pub fn open_file(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        shared::sqlite::configure_writer(&conn)?;
        apply_migrations(&conn)?;
        Ok(Db { conn })
    }

    /// A read-only connection to the database file at `path`, for the read
    /// pool.
    pub fn open_reader(path: &str) -> Result<Self> {
        Ok(Db {
            conn: shared::sqlite::open_reader(path)?,
        })
    }

    /// Runs `f` with this connection refusing writes, as the read pool's
    /// connections do (`shared::sqlite::read_only`).
    pub fn read_only<T>(&self, f: impl FnOnce() -> T) -> T {
        shared::sqlite::read_only(&self.conn, f)
    }

    pub fn into_shared(self) -> SharedDb {
        Arc::new(Mutex::new(self))
    }

    /// Inserts a new user row. Fails with a unique-violation `DbError` (see
    /// [`DbError::is_unique_violation`]) if `email` is already taken.
    /// `is_admin` is `true` only from the first-run setup wizard's own single
    /// call site (`http::setup`) - every ordinary signup passes `false`.
    pub fn create_user(
        &self,
        id: &UserId,
        email: &str,
        password_hash: &str,
        is_admin: bool,
        created_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO users (id, email, password_hash, is_admin, created_at_utc) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, email, password_hash, is_admin, created_at],
        )?;
        Ok(())
    }

    /// `POST /dashboard/timezone`: a zone name, or `None` for automatic.
    pub fn update_user_timezone(&self, id: &UserId, timezone: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET timezone = ?2 WHERE id = ?1",
            params![id, timezone],
        )?;
        Ok(())
    }

    /// `POST /dashboard/theme` - the nav's own no-JS toggle form. Same
    /// "update a single column, keyed by id" shape as
    /// `update_store_connection_fx_providers`.
    pub fn update_user_theme(&self, id: &UserId, theme: Theme) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET theme = ?2 WHERE id = ?1",
            params![id, theme.as_str()],
        )?;
        Ok(())
    }

    /// Direct row lookup by email — used by tests to confirm what actually
    /// landed in the database (e.g. that `password_hash` is a real Argon2
    /// hash, never the plaintext password).
    pub fn get_user_by_email(&self, email: &str) -> Result<Option<UserRow>> {
        self.conn
            .query_row(
                "SELECT id, email, password_hash, created_at_utc, is_admin, theme, timezone FROM users WHERE email = ?1",
                params![email],
                |row| {
                    Ok(UserRow {
                        id: row.get(0)?,
                        email: row.get(1)?,
                        password_hash: row.get(2)?,
                        created_at: row.get(3)?,
                        is_admin: row.get(4)?,
                        theme: Theme::from_db_str(&row.get::<_, String>(5)?),
                        timezone: row.get(6)?,
                        browser_timezone: None,
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Direct row lookup by id - used to resolve a session's `user_id` back
    /// to a full user row (see `AuthedUser`'s extractor in `http/mod.rs`).
    pub fn get_user_by_id(&self, id: &UserId) -> Result<Option<UserRow>> {
        self.conn
            .query_row(
                "SELECT id, email, password_hash, created_at_utc, is_admin, theme, timezone FROM users WHERE id = ?1",
                params![id],
                |row| {
                    Ok(UserRow {
                        id: row.get(0)?,
                        email: row.get(1)?,
                        password_hash: row.get(2)?,
                        created_at: row.get(3)?,
                        is_admin: row.get(4)?,
                        theme: Theme::from_db_str(&row.get::<_, String>(5)?),
                        timezone: row.get(6)?,
                        browser_timezone: None,
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    /// One runtime-configurable setting's stored value, or `None` if nothing
    /// has ever been saved for `key` - see `crates/engine/src/store.rs`'s
    /// own `get_setting` (identical shape, identical reasoning). The
    /// settings registry (`crate::settings`) resolves the command line and
    /// the environment over it.
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(DbError::from)
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
        self.conn
            .execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(())
    }

    /// Stores and deletes several settings in one transaction (the
    /// settings registry's save): either all of them land or none do.
    pub fn write_settings(&self, changes: &[(&str, Option<String>)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for (key, value) in changes {
            match value {
                Some(value) => self.set_setting(key, value)?,
                None => self.delete_setting(key)?,
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every stored setting at once - the admin settings page's `GET` reads
    /// the whole table in one query rather than one `get_setting` call per
    /// known key.
    pub fn list_settings(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut stmt = self.conn.prepare("SELECT key, value FROM settings")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()
            .map_err(DbError::from)
    }

    /// `true` once the first-run setup wizard has created the instance admin
    /// account - a dedicated `settings` row (`"setup_complete" = "true"`),
    /// per the user's own explicit "based upon a flag in database" request,
    /// rather than derived from `SELECT COUNT(*) FROM users WHERE is_admin`.
    /// Set exactly once, with the account, by
    /// [`Db::create_admin_and_complete_setup`].
    pub fn is_setup_complete(&self) -> Result<bool> {
        Ok(self.get_setting("setup_complete")?.as_deref() == Some("true"))
    }

    /// Test-only convenience seeding a known admin account and marking setup
    /// complete - the "tests should seed the admin account with a known
    /// user/pass, which will mean the admin flow won't trigger" requirement.
    /// Panics on a database error - every caller is a test fixture already
    /// `.unwrap()`-ing `Db::open_in_memory()` right next to this, so a
    /// failure here is exactly as fatal to the test as that would be.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_test_admin(&self) {
        self.create_user(
            &UserId::new("test-admin"),
            TEST_ADMIN_EMAIL,
            TEST_ADMIN_PASSWORD_HASH,
            true,
            0,
        )
        .expect("seeding the test admin account");
        self.set_setting("setup_complete", "true")
            .expect("marking setup complete for the seeded test admin");
    }

    /// Stores a new session. `token_hash` must already be hashed (see
    /// [`SessionRow`]'s doc comment) - `Db` never sees, and never needs to
    /// see, a raw session token.
    pub fn create_session(
        &self,
        token_hash: &shared::auth::TokenHash,
        user_id: &UserId,
        created_at: i64,
    ) -> Result<()> {
        // Sessions that expired are dropped as new ones are made, so the
        // table doesn't hold every session ever made.
        self.conn.execute(
            "DELETE FROM sessions WHERE created_at_utc < ?1",
            params![created_at.saturating_sub(SESSION_LIFETIME_SECONDS)],
        )?;
        self.conn.execute(
            "INSERT INTO sessions (token, user_id, created_at_utc) VALUES (?1, ?2, ?3)",
            params![token_hash, user_id, created_at],
        )?;
        Ok(())
    }

    /// Looks up a session by its hashed token. `None` for an unknown,
    /// already-deleted or expired session (one made more than
    /// [`SESSION_LIFETIME_SECONDS`] before `now`) - callers (the
    /// `AuthedUser` extractor) map that to 401, same as an unknown tenant
    /// secret in the engine's own `AuthedTenant`. A read: expired rows are
    /// removed by [`Db::create_session`], a write.
    pub fn find_session(
        &self,
        token_hash: &shared::auth::TokenHash,
        now: i64,
    ) -> Result<Option<SessionRow>> {
        let oldest_live = now.saturating_sub(SESSION_LIFETIME_SECONDS);
        self.conn
            .query_row(
                "SELECT token, user_id, created_at_utc FROM sessions WHERE token = ?1 AND created_at_utc >= ?2",
                params![token_hash, oldest_live],
                |row| {
                    Ok(SessionRow {
                        token_hash: row.get(0)?,
                        user_id: row.get(1)?,
                        created_at: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Deletes a session by its hashed token (logging out, both the JSON
    /// API's and the dashboard's). Returns whether a row was actually
    /// deleted (`false` if it was already gone).
    pub fn delete_session(&self, token_hash: &shared::auth::TokenHash) -> Result<bool> {
        let affected = self
            .conn
            .execute("DELETE FROM sessions WHERE token = ?1", params![token_hash])?;
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
        id: &ConnectionId,
        user_id: &UserId,
        platform: &str,
        site_url: &str,
        tenant_public_key: &str,
        tenant_secret_token_encrypted: &str,
        created_at: i64,
        base_currency: &str,
    ) -> Result<()> {
        // `fx_providers` explicit here (`["coingecko"]`), not left to the
        // column's own `DEFAULT` - SQLite can't cheaply change a column
        // `DEFAULT` in place, so after `"fixed"`'s removal this is the one
        // real place a new store's initial provider is decided. Harmless
        // even on an instance that never enables Coingecko: an XMR-priced
        // order never reads this column at all (see `StoreConnectionRow::
        // fx_providers`'s own doc comment), and a merchant can still pick a
        // different available provider from their store's settings page
        // the moment one exists. `base_currency` is *not* similarly
        // defaulted here - the caller (`http::connections::create_connection_for_user`)
        // is responsible for having already validated it via
        // `crate::currencies::resolve_currency` before ever reaching this
        // call, since (unlike `fx_providers`) there is no single safe
        // implicit choice for it.
        self.conn.execute(
            "INSERT INTO store_connections
                (id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, created_at_utc, fx_providers, base_currency)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, json_array('coingecko'), ?8)",
            params![
                id,
                user_id,
                platform,
                site_url,
                tenant_public_key,
                tenant_secret_token_encrypted,
                created_at,
                base_currency,
            ],
        )?;
        Ok(())
    }

    /// A store taking payments into `wallet_id` (migration 0031): the row
    /// `create_store_connection` makes, with its wallet.
    #[allow(clippy::too_many_arguments)]
    pub fn create_store_connection_on_wallet(
        &self,
        id: &ConnectionId,
        user_id: &UserId,
        platform: &str,
        site_url: &str,
        tenant_public_key: &str,
        tenant_secret_token_encrypted: &str,
        created_at: i64,
        base_currency: &str,
        wallet_id: &WalletId,
    ) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        tx.execute(
            "INSERT INTO store_connections
                (id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, created_at_utc, fx_providers, base_currency, wallet_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, json_array('coingecko'), ?8, ?9)",
            params![
                id,
                user_id,
                platform,
                site_url,
                tenant_public_key,
                tenant_secret_token_encrypted,
                created_at,
                base_currency,
                wallet_id,
            ],
        )?;
        tx.execute(
            "INSERT INTO wallet_events (wallet_id, at_utc, kind, detail) VALUES (?1, ?2, 'store_connected', ?3)",
            params![wallet_id, created_at, id],
        )?;
        tx.execute(
            "INSERT INTO store_wallet_periods (connection_id, wallet_id, from_utc) VALUES (?1, ?2, ?3)",
            params![id, wallet_id, created_at],
        )?;
        tx.commit()?;
        Ok(())
    }

    // -- Wallets (migration 0031) --------------------------------------

    /// Adds a wallet and its first history entry. A name or address the
    /// account already has is a unique violation (`DbError::is_unique_violation`).
    pub fn create_wallet(&self, new: &NewWalletRow<'_>) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        tx.execute(
            "INSERT INTO wallets (id, user_id, name, network, primary_address, engine_wallet_id, origin, backup, created_at_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                new.id,
                new.user_id,
                new.name,
                new.network,
                new.primary_address,
                new.engine_wallet_id,
                new.origin.as_str(),
                new.backup,
                new.created_at,
            ],
        )?;
        tx.execute(
            "INSERT INTO wallet_events (wallet_id, at_utc, kind, detail) VALUES (?1, ?2, ?3, ?4)",
            params![
                new.id,
                new.created_at,
                match new.origin {
                    WalletOrigin::Created => "created",
                    WalletOrigin::Imported => "imported",
                },
                new.backup.unwrap_or(""),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn row_to_wallet(row: &rusqlite::Row<'_>) -> rusqlite::Result<WalletRow> {
        Ok(WalletRow {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            name: row.get("name")?,
            network: row.get("network")?,
            primary_address: row.get("primary_address")?,
            engine_wallet_id: row.get("engine_wallet_id")?,
            origin: WalletOrigin::parse(&row.get::<_, String>("origin")?),
            backup: row.get("backup")?,
            created_at: row.get("created_at_utc")?,
            retired_at: row.get("retired_at_utc")?,
        })
    }

    /// `user_id`'s wallets in use, oldest first, each with how many stores
    /// use it: what pickers offer. Retired ones are left out
    /// ([`Self::list_retired_wallets`]).
    pub fn list_wallets(&self, user_id: &UserId) -> Result<Vec<WalletSummary>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT w.*, (SELECT COUNT(*) FROM store_connections s WHERE s.wallet_id = w.id) AS stores
             FROM wallets w WHERE w.user_id = ?1 AND w.retired_at_utc IS NULL
             ORDER BY w.created_at_utc, w.name",
        )?;
        let rows = stmt
            .query_map(params![user_id], |row| {
                Ok(WalletSummary {
                    wallet: Self::row_to_wallet(row)?,
                    store_count: row.get::<_, i64>("stores")? as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// One of `user_id`'s wallets. Someone else's and a missing one look
    /// the same: `None`.
    pub fn get_wallet(&self, user_id: &UserId, id: &WalletId) -> Result<Option<WalletRow>> {
        self.conn
            .query_row(
                "SELECT * FROM wallets WHERE id = ?1 AND user_id = ?2",
                params![id, user_id],
                Self::row_to_wallet,
            )
            .optional()
            .map_err(DbError::from)
    }

    /// The wallet of `user_id`'s with this address, if they already added it.
    pub fn find_wallet_by_address(
        &self,
        user_id: &UserId,
        network: &str,
        primary_address: &str,
    ) -> Result<Option<WalletRow>> {
        self.conn
            .query_row(
                "SELECT * FROM wallets WHERE user_id = ?1 AND network = ?2 AND primary_address = ?3",
                params![user_id, network, primary_address],
                Self::row_to_wallet,
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Renames one of `user_id`'s wallets, noting the old name in its
    /// history. `false` if it isn't theirs; a name they already use is a
    /// unique violation.
    pub fn rename_wallet(
        &self,
        user_id: &UserId,
        id: &WalletId,
        name: &str,
        now: i64,
    ) -> Result<bool> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let old: Option<String> = tx
            .query_row(
                "SELECT name FROM wallets WHERE id = ?1 AND user_id = ?2",
                params![id, user_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(old) = old else {
            return Ok(false);
        };
        if old == name {
            return Ok(true);
        }
        tx.execute(
            "UPDATE wallets SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        tx.execute(
            "INSERT INTO wallet_events (wallet_id, at_utc, kind, detail) VALUES (?1, ?2, 'renamed', ?3)",
            params![id, now, old],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// `user_id`'s retired wallets, most recently retired first.
    pub fn list_retired_wallets(&self, user_id: &UserId) -> Result<Vec<WalletRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT * FROM wallets WHERE user_id = ?1 AND retired_at_utc IS NOT NULL
             ORDER BY retired_at_utc DESC",
        )?;
        let rows = stmt
            .query_map(params![user_id], Self::row_to_wallet)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every name `user_id`'s wallets have, retired ones too: a new wallet
    /// can't take one (`wallets_user_name`).
    pub fn wallet_names(&self, user_id: &UserId) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT name FROM wallets WHERE user_id = ?1")?;
        let rows = stmt
            .query_map(params![user_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Records that one of `user_id`'s wallets was retired at `at` (the
    /// engine deleted its keys then). `false` if it isn't theirs, is
    /// already retired, or a store still uses it.
    pub fn retire_wallet(&self, user_id: &UserId, id: &WalletId, at: i64) -> Result<bool> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let changed = tx.execute(
            "UPDATE wallets SET retired_at_utc = ?3
             WHERE id = ?1 AND user_id = ?2 AND retired_at_utc IS NULL
               AND NOT EXISTS (SELECT 1 FROM store_connections s WHERE s.wallet_id = ?1)",
            params![id, user_id, at],
        )?;
        if changed > 0 {
            tx.execute(
                "INSERT INTO wallet_events (wallet_id, at_utc, kind) VALUES (?1, ?2, 'retired')",
                params![id, at],
            )?;
        }
        tx.commit()?;
        Ok(changed > 0)
    }

    /// Brings back one of `user_id`'s retired wallets (the engine has its
    /// keys again).
    pub fn restore_wallet(&self, user_id: &UserId, id: &WalletId, now: i64) -> Result<bool> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let changed = tx.execute(
            "UPDATE wallets SET retired_at_utc = NULL
             WHERE id = ?1 AND user_id = ?2 AND retired_at_utc IS NOT NULL",
            params![id, user_id],
        )?;
        if changed > 0 {
            tx.execute(
                "INSERT INTO wallet_events (wallet_id, at_utc, kind) VALUES (?1, ?2, 'restored')",
                params![id, now],
            )?;
        }
        tx.commit()?;
        Ok(changed > 0)
    }

    /// A wallet's history, newest first.
    pub fn wallet_events(&self, id: &WalletId) -> Result<Vec<WalletEventRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT at_utc, kind, detail FROM wallet_events WHERE wallet_id = ?1
             ORDER BY at_utc DESC, rowid DESC",
        )?;
        let rows = stmt
            .query_map(params![id], |row| {
                Ok(WalletEventRow {
                    at: row.get(0)?,
                    kind: row.get(1)?,
                    detail: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Links a store made before wallets to the wallet it takes payments
    /// into (`http::wallets::adopt_unlinked_stores`).
    pub fn adopt_store_wallet(
        &self,
        connection_id: &ConnectionId,
        wallet_id: &WalletId,
        now: i64,
    ) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let changed = tx.execute(
            "UPDATE store_connections SET wallet_id = ?2 WHERE id = ?1 AND wallet_id IS NULL",
            params![connection_id, wallet_id],
        )?;
        if changed > 0 {
            tx.execute(
                "INSERT INTO wallet_events (wallet_id, at_utc, kind, detail) VALUES (?1, ?2, 'store_connected', ?3)",
                params![wallet_id, now, connection_id],
            )?;
            // It has used the wallet since it was connected.
            tx.execute(
                "INSERT INTO store_wallet_periods (connection_id, wallet_id, from_utc)
                 SELECT id, wallet_id, created_at_utc FROM store_connections WHERE id = ?1",
                params![connection_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Records that a store now takes payments into `wallet_id` (the engine
    /// has made the change): its current period ends and the next begins,
    /// and both wallets' histories say so.
    pub fn change_store_wallet(
        &self,
        connection_id: &ConnectionId,
        wallet_id: &WalletId,
        now: i64,
    ) -> Result<ChangeStoreWallet> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let old: Option<WalletId> = tx.query_row(
            "SELECT wallet_id FROM store_connections WHERE id = ?1",
            params![connection_id],
            |row| row.get(0),
        )?;
        if old.as_ref() == Some(wallet_id) {
            return Ok(ChangeStoreWallet::Unchanged);
        }
        tx.execute(
            "UPDATE store_connections SET wallet_id = ?2 WHERE id = ?1",
            params![connection_id, wallet_id],
        )?;
        tx.execute(
            "UPDATE store_wallet_periods SET until_utc = ?2 WHERE connection_id = ?1 AND until_utc IS NULL",
            params![connection_id, now],
        )?;
        tx.execute(
            "INSERT INTO store_wallet_periods (connection_id, wallet_id, from_utc) VALUES (?1, ?2, ?3)",
            params![connection_id, wallet_id, now],
        )?;
        if let Some(old) = &old {
            tx.execute(
                "INSERT INTO wallet_events (wallet_id, at_utc, kind, detail) VALUES (?1, ?2, 'store_changed_away', ?3)",
                params![old, now, connection_id],
            )?;
        }
        tx.execute(
            "INSERT INTO wallet_events (wallet_id, at_utc, kind, detail) VALUES (?1, ?2, 'store_changed_to', ?3)",
            params![wallet_id, now, connection_id],
        )?;
        tx.commit()?;
        Ok(ChangeStoreWallet::Changed)
    }

    /// The wallets a store has taken payments into, newest first, with how
    /// many orders it made in each period.
    pub fn store_wallet_periods(
        &self,
        connection_id: &ConnectionId,
    ) -> Result<Vec<StoreWalletPeriod>> {
        self.wallet_periods("p.connection_id = ?1", params![connection_id])
    }

    /// Every store's periods on one wallet, newest first.
    pub fn wallet_store_periods(&self, wallet_id: &WalletId) -> Result<Vec<StoreWalletPeriod>> {
        self.wallet_periods("p.wallet_id = ?1", params![wallet_id])
    }

    fn wallet_periods(
        &self,
        condition: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<StoreWalletPeriod>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT p.connection_id, p.wallet_id, w.name, w.retired_at_utc IS NOT NULL, p.from_utc, p.until_utc,
                (SELECT COUNT(*) FROM order_currency_metadata m
                 WHERE m.connection_id = p.connection_id AND m.created_at_utc >= p.from_utc
                   AND (p.until_utc IS NULL OR m.created_at_utc < p.until_utc)) AS orders
             FROM store_wallet_periods p LEFT JOIN wallets w ON w.id = p.wallet_id
             WHERE {condition}
             ORDER BY p.from_utc DESC, p.rowid DESC"
        ))?;
        let rows = stmt
            .query_map(params, |row| {
                Ok(StoreWalletPeriod {
                    connection_id: row.get(0)?,
                    wallet_id: row.get(1)?,
                    wallet_name: row.get(2)?,
                    wallet_retired: row.get(3)?,
                    from: row.get(4)?,
                    until: row.get(5)?,
                    orders: row.get::<_, i64>(6)? as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Direct row lookup by id - used by tests to confirm what actually
    /// landed in `store_connections` after `POST /connections` (e.g. that
    /// `tenant_secret_token_encrypted` really holds a real `sk_...` value).
    pub fn get_store_connection_by_id(
        &self,
        id: &ConnectionId,
    ) -> Result<Option<StoreConnectionRow>> {
        self.conn
            .query_row(
                "SELECT id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, created_at_utc, fx_providers, base_currency, fx_provider_settings, wallet_id
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
                        created_at: row.get(6)?,
                        fx_providers: parse_fx_providers(&row.get::<_, String>(7)?),
                    base_currency: row.get(8)?,
                    fx_provider_settings: FxProviderSettings::parse(&row.get::<_, String>(9)?),
                    wallet_id: row.get(10)?,
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Updates a store's ordered exchange-rate provider list together with
    /// its per-provider settings, in one statement so the two can never be
    /// seen half-changed - the settings-page counterpart to
    /// `create_store_connection`'s explicit initial `["coingecko"]`. The
    /// caller (`http::orders::update_fx_providers`) is responsible for
    /// validating every name against `ExchangeRateProviders::
    /// available_providers` and every setting first; this layer stores
    /// whatever it's given, same as every other plain column update in this
    /// file.
    pub fn update_store_connection_fx(
        &self,
        _proof: crate::confirmation_thresholds::PolicyProof,
        id: &ConnectionId,
        fx_providers: &[String],
        settings: &FxProviderSettings,
    ) -> Result<()> {
        let providers_json =
            serde_json::to_string(fx_providers).expect("a list of strings always serializes");
        self.conn.execute(
            "UPDATE store_connections SET fx_providers = ?2, fx_provider_settings = ?3 WHERE id = ?1",
            params![id, providers_json, settings.to_json()],
        )?;
        Ok(())
    }

    /// Every `store_connections` row belonging to `user_id`, newest first -
    /// the dashboard home page's own data source. Deliberately no "display
    /// name" column exists on this table (only `site_url`) - the dashboard
    /// derives a display name from `site_url` itself rather than this query
    /// growing a field nothing else needs; see `http/home.rs::display_name`.
    pub fn list_store_connections_for_user(
        &self,
        user_id: &UserId,
    ) -> Result<Vec<StoreConnectionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, created_at_utc, fx_providers, base_currency, fx_provider_settings, wallet_id
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
                    created_at: row.get(6)?,
                    fx_providers: parse_fx_providers(&row.get::<_, String>(7)?),
                    base_currency: row.get(8)?,
                    fx_provider_settings: FxProviderSettings::parse(&row.get::<_, String>(9)?),
                    wallet_id: row.get(10)?,
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
    ///
    /// `confirmations_required_applied`/`base_currency`/`base_currency_piconero_per_unit`
    /// are the order-creation-time snapshot of how its confirmation
    /// threshold was decided (migration `0016_order_confirmation_snapshot.sql`'s
    /// own doc comment) - `base_currency_piconero_per_unit` is `None`
    /// specifically when the order's own currency already was the base
    /// currency (see `confirmation_thresholds::Resolution`), never merely
    /// "not recorded".
    #[allow(clippy::too_many_arguments)]
    pub fn create_order_currency_metadata(
        &self,
        connection_id: &ConnectionId,
        order_id: &OrderId,
        currency: &str,
        amount: &str,
        piconero_per_unit: Piconero,
        provider: &str,
        created_at: i64,
        base_currency: &str,
        base_currency_piconero_per_unit: Option<Piconero>,
        confirmations_required_applied: u64,
        created_with_key: bool,
        source: Option<&str>,
    ) -> Result<()> {
        // An amount too large for SQLite is refused (a conversion error), not
        // wrapped or panicked on.
        self.conn.execute(
            "INSERT INTO order_currency_metadata
                (connection_id, order_id, currency, amount, piconero_per_unit, provider, created_at_utc,
                 store_base_currency, base_currency_piconero_per_unit, confirmations_required_applied, created_with_key,
                 source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT (connection_id, order_id) DO NOTHING",
            params![
                connection_id,
                order_id,
                currency,
                amount,
                piconero_per_unit,
                provider,
                created_at,
                base_currency,
                base_currency_piconero_per_unit,
                Unsigned(confirmations_required_applied),
                created_with_key,
                source,
            ],
        )?;
        Ok(())
    }

    /// Looks up one order's currency metadata - `None` when nothing was ever
    /// recorded for this `(connection_id, order_id)` pair (an order the
    /// engine reports that predates this table, or one created directly
    /// against the engine's own API rather than through monokulo).
    pub fn get_order_currency_metadata(
        &self,
        connection_id: &ConnectionId,
        order_id: &OrderId,
    ) -> Result<Option<OrderCurrencyMetadataRow>> {
        self.conn
            .query_row(
                "SELECT connection_id, order_id, currency, amount, piconero_per_unit, provider, created_at_utc,
                        store_base_currency, base_currency_piconero_per_unit, confirmations_required_applied, created_with_key
                 FROM order_currency_metadata WHERE connection_id = ?1 AND order_id = ?2",
                params![connection_id, order_id],
                metadata_row,
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Currency metadata for each of `order_ids` (one page of a listing)
    /// that has any, keyed by `order_id`: one primary-key lookup per order,
    /// so a page costs the same however many orders the store has.
    pub fn order_currency_metadata_for(
        &self,
        connection_id: &ConnectionId,
        order_ids: &[OrderId],
    ) -> Result<std::collections::HashMap<OrderId, OrderCurrencyMetadataRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT connection_id, order_id, currency, amount, piconero_per_unit, provider, created_at_utc,
                    store_base_currency, base_currency_piconero_per_unit, confirmations_required_applied, created_with_key
             FROM order_currency_metadata WHERE connection_id = ?1 AND order_id = ?2",
        )?;
        let mut rows = std::collections::HashMap::new();
        for order_id in order_ids {
            if let Some(row) = stmt
                .query_row(params![connection_id, order_id], metadata_row)
                .optional()?
            {
                rows.insert(row.order_id.clone(), row);
            }
        }
        Ok(rows)
    }

    /// Inserts a new single-use connect token (WBS 1.4.1) - `token_hash` is
    /// already hashed by the caller (`shared::auth::hash_secret_token`),
    /// never the raw token; see `http/connect.rs::confirm_submit`.
    pub fn create_connect_token(
        &self,
        token_hash: &shared::auth::TokenHash,
        connection_id: &ConnectionId,
        nonce: &str,
        created_at: i64,
    ) -> Result<()> {
        // Tokens a day old are long past any use (they live minutes):
        // dropped as new ones are made, so the table doesn't grow forever.
        self.conn.execute(
            "DELETE FROM connect_tokens WHERE created_at_utc < ?1",
            params![created_at.saturating_sub(24 * 60 * 60)],
        )?;
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
    pub fn consume_connect_token(
        &self,
        token_hash: &shared::auth::TokenHash,
        now: i64,
        ttl_seconds: i64,
    ) -> Result<Option<ConnectionId>> {
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
    pub fn get_store_connection_by_public_key(
        &self,
        tenant_public_key: &str,
    ) -> Result<Option<StoreConnectionRow>> {
        self.conn
            .query_row(
                "SELECT id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, created_at_utc, fx_providers, base_currency, fx_provider_settings, wallet_id
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
                        created_at: row.get(6)?,
                        fx_providers: parse_fx_providers(&row.get::<_, String>(7)?),
                    base_currency: row.get(8)?,
                    fx_provider_settings: FxProviderSettings::parse(&row.get::<_, String>(9)?),
                    wallet_id: row.get(10)?,
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
    pub fn update_store_connection_site_url(
        &self,
        id: &ConnectionId,
        site_url: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE store_connections SET site_url = ?2 WHERE id = ?1",
            params![id, site_url],
        )?;
        Ok(())
    }

    /// Every row of the static `currencies` reference table, ordered by
    /// `canonical_code` for a stable, alphabetical dropdown - see that
    /// migration's own comment and `crate::currencies` for how this is
    /// actually used.
    pub fn list_currencies(&self) -> Result<Vec<CurrencyRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT canonical_code, description, tickers FROM currencies ORDER BY canonical_code",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(CurrencyRow {
                canonical_code: row.get(0)?,
                description: row.get(1)?,
                tickers_json: row.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(DbError::from)
    }

    /// Updates a store's base currency - and, since every custom threshold
    /// was denominated in whatever the *old* currency was, deletes every
    /// `confirmation_thresholds` row for this connection in the same call
    /// (an old amount in a since-abandoned currency means nothing any more -
    /// see that table's own migration comment). The default/fallback
    /// threshold (`tenants.confirmations_required`, on the engine) is
    /// untouched - it has no currency dimension to invalidate.
    pub fn update_store_connection_base_currency(
        &self,
        _proof: crate::confirmation_thresholds::PolicyProof,
        id: &ConnectionId,
        base_currency: &str,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE store_connections SET base_currency = ?2 WHERE id = ?1",
            params![id, base_currency],
        )?;
        tx.execute(
            "DELETE FROM confirmation_thresholds WHERE connection_id = ?1",
            params![id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn dashboard_order_request(&self, id: &ConnectionId, key: &str) -> Result<Option<OrderId>> {
        Ok(self.conn.query_row("SELECT order_id FROM dashboard_order_requests WHERE connection_id = ?1 AND request_key = ?2", params![id, key], |row| row.get(0)).optional()?)
    }

    pub fn record_dashboard_order_request(
        &self,
        id: &ConnectionId,
        key: &str,
        order: &OrderId,
    ) -> Result<()> {
        self.conn.execute("INSERT INTO dashboard_order_requests (connection_id, request_key, order_id) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING", params![id, key, order])?;
        Ok(())
    }

    /// Saves the common setup fields together and invalidates thresholds only
    /// when their currency changes. The caller holds the store policy lock.
    pub fn update_store_setup(
        &self,
        _proof: crate::confirmation_thresholds::PolicyProof,
        id: &ConnectionId,
        currency: &str,
        providers: &[String],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM confirmation_thresholds WHERE connection_id = ?1 AND EXISTS (SELECT 1 FROM store_connections WHERE id = ?1 AND base_currency != ?2)", params![id, currency])?;
        tx.execute(
            "UPDATE store_connections SET base_currency = ?2, fx_providers = ?3 WHERE id = ?1",
            params![
                id,
                currency,
                serde_json::to_string(providers).expect("strings serialize")
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every verified-embed domain of `connection_id`, alphabetically.
    pub fn list_store_domains(&self, connection_id: &ConnectionId) -> Result<Vec<StoreDomainRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {STORE_DOMAIN_COLUMNS} FROM store_domains WHERE connection_id = ?1 ORDER BY domain"))?;
        let rows = stmt.query_map(params![connection_id], store_domain_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(DbError::from)
    }

    /// One domain, only if it belongs to `connection_id`.
    pub fn get_store_domain(
        &self,
        connection_id: &ConnectionId,
        id: &str,
    ) -> Result<Option<StoreDomainRow>> {
        self.conn
            .query_row(
                &format!("SELECT {STORE_DOMAIN_COLUMNS} FROM store_domains WHERE id = ?1 AND connection_id = ?2"),
                params![id, connection_id],
                store_domain_from_row,
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Adds a domain waiting for its DNS record. `Ok(false)` when the store
    /// already has `max` domains (checked in the same statement, so two
    /// concurrent adds can't both take the last slot); a unique-violation
    /// `DbError` when it already has this domain.
    pub fn create_store_domain(
        &self,
        id: &str,
        connection_id: &ConnectionId,
        domain: &str,
        token: &str,
        created_at: i64,
        max: usize,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "INSERT INTO store_domains (id, connection_id, domain, token, created_at_utc)
             SELECT ?1, ?2, ?3, ?4, ?5
             WHERE (SELECT COUNT(*) FROM store_domains WHERE connection_id = ?2) < ?6",
            params![id, connection_id, domain, token, created_at, Unsigned(max)],
        )?;
        Ok(changed == 1)
    }

    /// The Logs page searches `user_id` saved, by name.
    pub fn list_saved_log_searches(&self, user_id: &UserId) -> Result<Vec<SavedLogSearch>> {
        let mut statement = self
            .conn
            .prepare("SELECT id, name, query_string FROM saved_log_searches WHERE user_id = ?1 ORDER BY name, created_at_utc")?;
        let rows = statement
            .query_map(params![user_id], |row| {
                Ok(SavedLogSearch {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    query_string: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Saves a Logs page search for `user_id`. `Ok(false)` when they
    /// already have `max`.
    pub fn create_saved_log_search(
        &self,
        id: &str,
        user_id: &UserId,
        name: &str,
        query_string: &str,
        now: i64,
        max: usize,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "INSERT INTO saved_log_searches (id, user_id, name, query_string, created_at_utc)
             SELECT ?1, ?2, ?3, ?4, ?5
             WHERE (SELECT COUNT(*) FROM saved_log_searches WHERE user_id = ?2) < ?6",
            params![id, user_id, name, query_string, now, Unsigned(max)],
        )?;
        Ok(changed == 1)
    }

    /// Removes one of `user_id`'s saved searches; `false` when there's no
    /// such search of theirs.
    pub fn delete_saved_log_search(&self, user_id: &UserId, id: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "DELETE FROM saved_log_searches WHERE id = ?1 AND user_id = ?2",
            params![id, user_id],
        )?;
        Ok(changed == 1)
    }

    /// Removes a domain; `false` when `connection_id` has no such domain.
    pub fn delete_store_domain(&self, connection_id: &ConnectionId, id: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "DELETE FROM store_domains WHERE id = ?1 AND connection_id = ?2",
            params![id, connection_id],
        )?;
        Ok(changed == 1)
    }

    /// Records one check of a domain. `error: None` means the record was
    /// found: the domain is (still) verified and no longer failing. Any
    /// error leaves a never-verified domain waiting, and starts the grace
    /// period of a verified one (unless it had already started).
    pub fn record_store_domain_check(
        &self,
        id: &str,
        checked_at: i64,
        error: Option<&str>,
    ) -> Result<()> {
        match error {
            None => self.conn.execute(
                "UPDATE store_domains SET verified_at_utc = ?2, failing_since_utc = NULL, last_checked_at_utc = ?2, last_error = NULL
                 WHERE id = ?1",
                params![id, checked_at],
            )?,
            Some(error) => self.conn.execute(
                "UPDATE store_domains SET last_checked_at_utc = ?2, last_error = ?3,
                    failing_since_utc = CASE WHEN verified_at_utc IS NULL THEN NULL ELSE COALESCE(failing_since_utc, ?2) END
                 WHERE id = ?1",
                params![id, checked_at, error],
            )?,
        };
        Ok(())
    }

    /// Verified domains whose next scheduled re-check is due: every
    /// `every_secs`, or every `failing_every_secs` while failing. Domains
    /// never verified are only checked when the merchant asks.
    pub fn list_store_domains_due_for_recheck(
        &self,
        now: i64,
        every_secs: i64,
        failing_every_secs: i64,
    ) -> Result<Vec<StoreDomainRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {STORE_DOMAIN_COLUMNS} FROM store_domains
             WHERE verified_at_utc IS NOT NULL
               AND (last_checked_at_utc IS NULL
                    OR (failing_since_utc IS NULL AND last_checked_at_utc <= ?1 - ?2)
                    OR (failing_since_utc IS NOT NULL AND last_checked_at_utc <= ?1 - ?3))
             ORDER BY last_checked_at_utc"
        ))?;
        let rows = stmt.query_map(
            params![now, every_secs, failing_every_secs],
            store_domain_from_row,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(DbError::from)
    }

    /// Adds a domain waiting for DNS unless the store already has it or is
    /// at `max` domains - for domains monokulo suggests on the merchant's
    /// behalf (their site's own domain), where either is fine to skip.
    pub fn suggest_store_domain(
        &self,
        connection_id: &ConnectionId,
        domain: &str,
        created_at: i64,
        max: usize,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO store_domains (id, connection_id, domain, token, created_at_utc)
             SELECT ?1, ?2, ?3, ?4, ?5
             WHERE (SELECT COUNT(*) FROM store_domains WHERE connection_id = ?2) < ?6",
            params![
                uuid::Uuid::new_v4().to_string(),
                connection_id,
                domain,
                crate::embed_domains::new_token(),
                created_at,
                Unsigned(max)
            ],
        )?;
        Ok(())
    }

    /// The embed policy of the store with this public key: whether it's
    /// restricted to its verified domains, and its domains. `None` for an
    /// unknown key.
    pub fn embed_policy_for_public_key(
        &self,
        public_key: &str,
    ) -> Result<Option<(bool, Vec<StoreDomainRow>)>> {
        let store: Option<(ConnectionId, i64)> = self
            .conn
            .query_row(
                "SELECT id, embed_restricted FROM store_connections WHERE tenant_public_key = ?1",
                params![public_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((connection_id, restricted)) = store else {
            return Ok(None);
        };
        Ok(Some((
            restricted != 0,
            self.list_store_domains(&connection_id)?,
        )))
    }

    pub fn embed_restricted(&self, connection_id: &ConnectionId) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT embed_restricted FROM store_connections WHERE id = ?1",
                params![connection_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|value| value != 0)
            .map_err(DbError::from)
    }

    pub fn set_embed_restricted(
        &self,
        connection_id: &ConnectionId,
        restricted: bool,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE store_connections SET embed_restricted = ?2 WHERE id = ?1",
            params![connection_id, restricted],
        )?;
        Ok(())
    }

    /// Whether the store has opted in to client logs (migration
    /// `0025_store_client_logging.sql`). `false` for a store that doesn't
    /// exist.
    pub fn client_logging(&self, connection_id: &ConnectionId) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT client_logging FROM store_connections WHERE id = ?1",
                params![connection_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map(|value| value.is_some_and(|value| value != 0))
            .map_err(DbError::from)
    }

    /// [`Db::client_logging`], for a store known by its public key.
    pub fn client_logging_by_public_key(&self, tenant_public_key: &str) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT client_logging FROM store_connections WHERE tenant_public_key = ?1",
                params![tenant_public_key],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map(|value| value.is_some_and(|value| value != 0))
            .map_err(DbError::from)
    }

    pub fn set_client_logging(&self, connection_id: &ConnectionId, on: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE store_connections SET client_logging = ?2 WHERE id = ?1",
            params![connection_id, on],
        )?;
        Ok(())
    }

    /// Stores whose own site domain hasn't been copied
    /// into `store_domains` yet (see migration `0020_embed_restriction.sql`).
    pub fn list_store_connections_awaiting_domain_import(&self) -> Result<Vec<StoreConnectionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, created_at_utc, fx_providers, base_currency, fx_provider_settings, wallet_id
             FROM store_connections WHERE domains_imported = 0",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(StoreConnectionRow {
                id: row.get(0)?,
                user_id: row.get(1)?,
                platform: row.get(2)?,
                site_url: row.get(3)?,
                tenant_public_key: row.get(4)?,
                tenant_secret_token_encrypted: row.get(5)?,
                created_at: row.get(6)?,
                fx_providers: parse_fx_providers(&row.get::<_, String>(7)?),
                base_currency: row.get(8)?,
                fx_provider_settings: FxProviderSettings::parse(&row.get::<_, String>(9)?),
                wallet_id: row.get(10)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(DbError::from)
    }

    #[cfg(test)]
    pub fn reset_store_domains_imported_for_test(&self, connection_id: &ConnectionId) {
        self.conn
            .execute(
                "UPDATE store_connections SET domains_imported = 0 WHERE id = ?1",
                params![connection_id],
            )
            .unwrap();
        self.conn
            .execute(
                "DELETE FROM store_domains WHERE connection_id = ?1",
                params![connection_id],
            )
            .unwrap();
    }

    pub fn mark_store_domains_imported(&self, connection_id: &ConnectionId) -> Result<()> {
        self.conn.execute(
            "UPDATE store_connections SET domains_imported = 1 WHERE id = ?1",
            params![connection_id],
        )?;
        Ok(())
    }

    /// Whether the merchant has shrunk the store page's "any website can
    /// show this checkout" warning to one line.
    pub fn embed_warning_dismissed(&self, connection_id: &ConnectionId) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT embed_warning_dismissed FROM store_connections WHERE id = ?1",
                params![connection_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|value| value != 0)
            .map_err(DbError::from)
    }

    pub fn dismiss_embed_warning(&self, connection_id: &ConnectionId) -> Result<()> {
        self.conn.execute(
            "UPDATE store_connections SET embed_warning_dismissed = 1 WHERE id = ?1",
            params![connection_id],
        )?;
        Ok(())
    }

    /// How many custom thresholds `connection_id` already has.
    pub fn count_confirmation_thresholds(&self, connection_id: &ConnectionId) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM confirmation_thresholds WHERE connection_id = ?1",
                params![connection_id],
                |row| row.get(0),
            )
            .map_err(DbError::from)
    }

    /// Every custom threshold for `connection_id`, ordered ascending by
    /// amount - "custom thresholds should be displayed in ascending order
    /// of the unit amount" (the default/fallback always renders first, but
    /// separately - it isn't a row in this table at all). Exact decimal
    /// sorting avoids float ties at neighboring piconero-sized boundaries.
    pub fn list_confirmation_thresholds(
        &self,
        connection_id: &ConnectionId,
    ) -> Result<Vec<ConfirmationThresholdRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, connection_id, unit_amount, confirmations_required, created_at_utc
             FROM confirmation_thresholds WHERE connection_id = ?1",
        )?;
        let rows = stmt.query_map(params![connection_id], |row| {
            Ok(ConfirmationThresholdRow {
                id: row.get(0)?,
                connection_id: row.get(1)?,
                unit_amount: row.get(2)?,
                confirmations_required: row.get::<_, Unsigned<u64>>(3)?.0,
                created_at: row.get(4)?,
            })
        })?;
        let mut rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        // By amount; one that doesn't parse (a hand-edited row) sorts last
        // and is logged, rather than quietly coming first.
        rows.sort_by_key(|row| {
            let amount = crate::confirmation_thresholds::ThresholdAmount::parse(&row.unit_amount);
            if amount.is_err() {
                tracing::warn!(unit_amount = %row.unit_amount, "a confirmation threshold's amount does not parse");
            }
            let amount = amount.ok();
            (amount.is_none(), amount)
        });
        Ok(rows)
    }

    #[cfg(test)]
    pub fn break_confirmation_thresholds_for_test(&self) {
        self.conn
            .execute("DROP TABLE confirmation_thresholds", [])
            .unwrap();
    }

    /// The count predicate and insert run in one SQLite write statement, so
    /// concurrent requests cannot both claim the last available slot.
    pub fn create_confirmation_threshold_with_limit(
        &self,
        _proof: crate::confirmation_thresholds::PolicyProof,
        id: &str,
        connection_id: &ConnectionId,
        unit_amount: &str,
        confirmations_required: u64,
        created_at: i64,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "INSERT INTO confirmation_thresholds (id, connection_id, unit_amount, confirmations_required, created_at_utc)
             SELECT ?1, ?2, ?3, ?4, ?5
             WHERE (SELECT COUNT(*) FROM confirmation_thresholds WHERE connection_id = ?2) < 5",
            params![id, connection_id, unit_amount, Unsigned(confirmations_required), created_at],
        )?;
        Ok(changed == 1)
    }

    /// Applies a dashboard Save as a single local transaction. A failed
    /// insertion rolls back its deletes as well.
    pub fn replace_confirmation_thresholds(
        &self,
        _proof: crate::confirmation_thresholds::PolicyProof,
        connection_id: &ConnectionId,
        deleted_ids: &[String],
        new: Option<(&str, &str, u64, i64)>,
    ) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        for id in deleted_ids {
            tx.execute(
                "DELETE FROM confirmation_thresholds WHERE id = ?1 AND connection_id = ?2",
                params![id, connection_id],
            )?;
        }
        if let Some((id, unit_amount, confirmations_required, created_at)) = new {
            let changed = tx.execute(
                "INSERT INTO confirmation_thresholds (id, connection_id, unit_amount, confirmations_required, created_at_utc)
                 SELECT ?1, ?2, ?3, ?4, ?5
                 WHERE (SELECT COUNT(*) FROM confirmation_thresholds WHERE connection_id = ?2) < 5",
                params![id, connection_id, unit_amount, Unsigned(confirmations_required), created_at],
            )?;
            if changed == 0 {
                return Ok(false);
            }
        }
        tx.commit()?;
        Ok(true)
    }

    /// Deletes one custom threshold, scoped to `connection_id` so one
    /// store's owner can never delete another store's threshold by id alone
    /// — the same ownership-scoping convention `delete_webhook` already
    /// uses. Returns whether a row was actually deleted (`false` for an
    /// unknown id, or one belonging to a different connection - the caller
    /// treats both identically, same enumeration-defense convention this
    /// crate already applies to every other owned-resource lookup).
    pub fn delete_confirmation_threshold(
        &self,
        _proof: crate::confirmation_thresholds::PolicyProof,
        connection_id: &ConnectionId,
        id: &str,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "DELETE FROM confirmation_thresholds WHERE id = ?1 AND connection_id = ?2",
            params![id, connection_id],
        )?;
        Ok(changed > 0)
    }

    /// `POST /request-invite` (`http::invites::request_invite_submit`) -
    /// records the request itself. Does *not* create its matching
    /// `invite_links` row (see [`Db::create_invite_link`]) - the caller
    /// creates both in immediate succession so the admin invites page can
    /// build a real `mailto:` link for this row on its very first render,
    /// with no separate "generate" step - but they're two separate calls,
    /// not one, since a standalone invite link (the admin invites page's
    /// own "create invite link" button) has no request to attach to at all.
    /// Whether `email` has an invite request the admin hasn't actioned.
    pub fn has_pending_invite_request(&self, email: &str) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM invite_requests WHERE email = ?1 AND actioned = 0)",
                params![email],
                |row| row.get(0),
            )
            .map_err(DbError::from)
    }

    pub fn create_invite_request(
        &self,
        id: &str,
        email: &str,
        message: &str,
        created_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO invite_requests (id, email, message, created_at_utc) VALUES (?1, ?2, ?3, ?4)",
            params![id, email, message, created_at],
        )?;
        Ok(())
    }

    /// One page of unactioned invite requests, newest first, each carrying
    /// its own still-unused invite link's encrypted token (a `LEFT JOIN`,
    /// not a separate query per row - see [`InviteRequestRow::invite_token_encrypted`]'s
    /// own doc comment).
    pub fn list_unactioned_invite_requests(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<InviteRequestRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.email, r.message, r.created_at_utc, l.token_encrypted
             FROM invite_requests r
             LEFT JOIN invite_links l ON l.request_id = r.id AND l.used_at_utc IS NULL
             WHERE r.actioned = 0
             ORDER BY r.created_at_utc DESC, r.id DESC
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![limit, offset], |row| {
            Ok(InviteRequestRow {
                id: row.get(0)?,
                email: row.get(1)?,
                message: row.get(2)?,
                created_at: row.get(3)?,
                invite_token_encrypted: row.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(DbError::from)
    }

    /// How many unactioned requests exist in total - the admin invites
    /// page's own pagination math (`http::invites::clamp_page`) needs this
    /// independent of whatever one page's `LIMIT`/`OFFSET` returns.
    pub fn count_unactioned_invite_requests(&self) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM invite_requests WHERE actioned = 0",
                [],
                |row| row.get(0),
            )
            .map_err(DbError::from)
    }

    /// A specific request by id, regardless of its `actioned` state - used
    /// to render the just-deleted row's own one-time struck-through
    /// addendum (`?deleted=<id>`, `http::invites::invites_page`), which must
    /// still be found *after* [`Db::delete_invite_request`] already marked
    /// it actioned.
    pub fn get_invite_request(&self, id: &str) -> Result<Option<InviteRequestRow>> {
        self.conn
            .query_row(
                "SELECT r.id, r.email, r.message, r.created_at_utc, l.token_encrypted
                 FROM invite_requests r
                 LEFT JOIN invite_links l ON l.request_id = r.id AND l.used_at_utc IS NULL
                 WHERE r.id = ?1",
                params![id],
                |row| {
                    Ok(InviteRequestRow {
                        id: row.get(0)?,
                        email: row.get(1)?,
                        message: row.get(2)?,
                        created_at: row.get(3)?,
                        invite_token_encrypted: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    /// Soft-deletes (`actioned = 1`) one request and revokes its own
    /// still-unused invite link, if it has one - a real row delete, not a
    /// further soft-delete, since an orphaned, never-sent, never-used token
    /// serves no purpose once its one request has been dismissed. A token
    /// that's already been used is left untouched (a real account exists
    /// behind it; nothing to revoke, and the request itself would already
    /// have been auto-actioned by [`Db::redeem_invite_and_create_user`], so
    /// this path shouldn't normally even be reached for one).
    pub fn delete_invite_request(&self, id: &str, now: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE invite_requests SET actioned = 1, actioned_at_utc = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        tx.execute(
            "DELETE FROM invite_links WHERE request_id = ?1 AND used_at_utc IS NULL",
            params![id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The admin invites page's "delete all" button - every currently
    /// unactioned request, soft-deleted and its unused link revoked in the
    /// same two-statement shape [`Db::delete_invite_request`] uses, just
    /// applied to the whole set at once rather than row by row. Returns how
    /// many requests were actually cleared, for the page's own confirmation
    /// banner.
    pub fn delete_all_unactioned_invite_requests(&self, now: i64) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let cleared = tx.execute(
            "UPDATE invite_requests SET actioned = 1, actioned_at_utc = ?1 WHERE actioned = 0",
            params![now],
        )?;
        tx.execute(
            "DELETE FROM invite_links WHERE used_at_utc IS NULL AND request_id IN (SELECT id FROM invite_requests WHERE actioned = 1)",
            [],
        )?;
        tx.commit()?;
        Ok(cleared)
    }

    /// Creates one invite link. `token_encrypted` is `Some` only when
    /// `request_id` is also `Some` - see `invite_links`'s own migration
    /// comment on why a standalone link (shown once, on this same response,
    /// and never redisplayed) has no need to be stored reversibly at all.
    pub fn create_invite_link(
        &self,
        id: &str,
        token_hash: &shared::auth::TokenHash,
        token_encrypted: Option<&str>,
        request_id: Option<&str>,
        created_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO invite_links (id, token_hash, token_encrypted, request_id, created_at_utc) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, token_hash, token_encrypted, request_id, created_at],
        )?;
        Ok(())
    }

    /// Redeems a presented invite token and creates the account it grants,
    /// in one transaction: the token is claimed (a single `UPDATE ... WHERE
    /// used_at_utc IS NULL`, checked by its affected-row count, so an
    /// invalid, already-used or expired ([`INVITE_LIFETIME_SECONDS`]) token
    /// can never result in a free account),
    /// the account is created, and the originating request is cleared, or
    /// none of it is. An account that can't be created (the email is
    /// taken) leaves the invite unclaimed, for the person to try again
    /// with the right email.
    pub fn redeem_invite_and_create_user(
        &self,
        token_hash: &shared::auth::TokenHash,
        user_id: &UserId,
        email: &str,
        password_hash: &str,
        now: i64,
    ) -> Result<RedeemInviteResult> {
        let tx = self.conn.unchecked_transaction()?;
        // `used_at_utc` first (not yet `used_by_user_id`, which has a real
        // `REFERENCES users (id)` this connection enforces - that column is
        // filled in once `user_id` exists as a row).
        let claimed = tx.execute(
            "UPDATE invite_links SET used_at_utc = ?2 WHERE token_hash = ?1 AND used_at_utc IS NULL AND created_at_utc >= ?3",
            params![token_hash, now, now.saturating_sub(INVITE_LIFETIME_SECONDS)],
        )?;
        if claimed == 0 {
            return Ok(RedeemInviteResult::InvalidOrAlreadyUsed);
        }
        let created = tx.execute(
            "INSERT INTO users (id, email, password_hash, is_admin, created_at_utc) VALUES (?1, ?2, ?3, 0, ?4)",
            params![user_id, email, password_hash, now],
        );
        if let Err(e) = created {
            let e = DbError::from(e);
            if e.is_unique_violation() {
                // Dropped without a commit: the claim is rolled back.
                return Ok(RedeemInviteResult::DuplicateEmail);
            }
            return Err(e);
        }
        tx.execute(
            "UPDATE invite_links SET used_by_user_id = ?2 WHERE token_hash = ?1",
            params![token_hash, user_id],
        )?;
        // Auto-clears the originating request (if any) from the admin's
        // pending list - the person it was about just joined, there's
        // nothing left to action.
        tx.execute(
            "UPDATE invite_requests SET actioned = 1, actioned_at_utc = ?2
             WHERE actioned = 0 AND id = (SELECT request_id FROM invite_links WHERE token_hash = ?1)",
            params![token_hash, now],
        )?;
        tx.commit()?;
        Ok(RedeemInviteResult::Created)
    }

    /// First-run setup: creates the one admin account and marks setup
    /// complete in one transaction, or does nothing when setup is already
    /// complete (`Ok(false)`): two submissions racing each other, or a
    /// replayed form, can't make two admins, and a crash between the two
    /// writes can't leave the wizard open with an admin already made.
    pub fn create_admin_and_complete_setup(
        &self,
        id: &UserId,
        email: &str,
        password_hash: &str,
        now: i64,
    ) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let complete: Option<String> = tx
            .query_row(
                "SELECT value FROM settings WHERE key = 'setup_complete'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if complete.as_deref() == Some("true") {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO users (id, email, password_hash, is_admin, created_at_utc) VALUES (?1, ?2, ?3, 1, ?4)",
            params![id, email, password_hash, now],
        )?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('setup_complete', 'true')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        tx.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token's hash, for tests that store and look up tokens directly.
    fn th(token: &str) -> shared::auth::TokenHash {
        shared::auth::RawToken::presented(token).hash()
    }

    fn temp_path() -> String {
        std::env::temp_dir()
            .join(format!("monokulo_db_{}.db", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .into_owned()
    }

    /// The file-backed database: a write is visible to the next read, a
    /// read can't write, and foreign keys are enforced.
    #[tokio::test]
    async fn readers_see_the_writers_commits_and_cannot_write() {
        let db = Database::open(&temp_path(), 2).unwrap();
        db.write(|db| db.set_setting("signup.mode", "public"))
            .await
            .unwrap();
        let read = db.read(|db| db.get_setting("signup.mode")).await.unwrap();
        assert_eq!(read.as_deref(), Some("public"));

        let refused = db.read(|db| db.set_setting("signup.mode", "x")).await;
        assert!(matches!(refused, Err(DbError::Sqlite(_))), "{refused:?}");

        let orphan = db
            .write(|db| {
                db.insert_pos_order(
                    &shared::ids::ConnectionId::new("no-such-store"),
                    &shared::ids::OrderId::new("o1"),
                    None,
                    None,
                    1,
                )
            })
            .await;
        assert!(matches!(orphan, Err(DbError::Sqlite(_))), "{orphan:?}");
    }

    /// The seeded test admin's stored hash really is its password's.
    #[tokio::test]
    async fn the_test_admin_hash_matches_the_test_admin_password() {
        let matches =
            shared::password::run(|h| h.verify(TEST_ADMIN_PASSWORD, TEST_ADMIN_PASSWORD_HASH))
                .await;
        assert_eq!(matches, Some(true));
    }

    /// A test's inline database refuses a write inside a read, as the read
    /// pool would, and is writable again afterwards.
    #[tokio::test]
    async fn an_inline_read_cannot_write_either() {
        let db = Database::inline(Db::open_in_memory().unwrap().into_shared());
        let refused = db.read(|db| db.set_setting("signup.mode", "x")).await;
        assert!(refused.is_err());
        db.write(|db| db.set_setting("signup.mode", "public"))
            .await
            .unwrap();
        assert_eq!(
            db.lock().get_setting("signup.mode").unwrap().as_deref(),
            Some("public")
        );
    }

    #[test]
    fn pos_orders_are_store_scoped_persist_background_and_cancel_state() {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("merchant"),
            "merchant@example.com",
            "hash",
            false,
            1,
        )
        .unwrap();
        for (id, pk) in [("store-a", "pk_a"), ("store-b", "pk_b")] {
            db.create_store_connection(
                &shared::ids::ConnectionId::new(id.to_string()),
                &shared::ids::UserId::new("merchant"),
                "custom",
                "https://example.com",
                pk,
                "encrypted",
                1,
                "XMR",
            )
            .unwrap();
        }
        db.insert_pos_order(
            &shared::ids::ConnectionId::new("store-a"),
            &shared::ids::OrderId::new("order-1"),
            Some("key-1"),
            Some("Mia coffee"),
            10,
        )
        .unwrap();
        assert!(db
            .get_pos_order(
                &shared::ids::ConnectionId::new("store-b"),
                &shared::ids::OrderId::new("order-1")
            )
            .unwrap()
            .is_none());
        assert_eq!(
            db.pos_order_by_request_key(&shared::ids::ConnectionId::new("store-a"), "key-1")
                .unwrap(),
            Some(shared::ids::OrderId::new("order-1"))
        );
        assert!(db
            .pos_order_by_request_key(&shared::ids::ConnectionId::new("store-b"), "key-1")
            .unwrap()
            .is_none());
        assert!(db
            .insert_pos_order(
                &shared::ids::ConnectionId::new("store-a"),
                &shared::ids::OrderId::new("order-2"),
                Some("key-1"),
                None,
                11
            )
            .is_err());
        assert!(!db
            .background_pos_order(
                &shared::ids::ConnectionId::new("store-b"),
                &shared::ids::OrderId::new("order-1")
            )
            .unwrap());
        assert!(db
            .background_pos_order(
                &shared::ids::ConnectionId::new("store-a"),
                &shared::ids::OrderId::new("order-1")
            )
            .unwrap());
        assert!(
            db.get_pos_order(
                &shared::ids::ConnectionId::new("store-a"),
                &shared::ids::OrderId::new("order-1")
            )
            .unwrap()
            .unwrap()
            .backgrounded
        );
        assert!(db
            .cancel_pos_order(
                &shared::ids::ConnectionId::new("store-a"),
                &shared::ids::OrderId::new("order-1"),
                20
            )
            .unwrap());
        assert_eq!(
            db.get_pos_order(
                &shared::ids::ConnectionId::new("store-a"),
                &shared::ids::OrderId::new("order-1")
            )
            .unwrap()
            .unwrap()
            .cancelled_at,
            Some(20)
        );
        assert!(!db
            .cancel_pos_order(
                &shared::ids::ConnectionId::new("store-a"),
                &shared::ids::OrderId::new("order-1"),
                21
            )
            .unwrap());
        assert_eq!(
            db.list_pos_orders(&shared::ids::ConnectionId::new("store-a"), 1, 0, None)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.count_pos_orders(&shared::ids::ConnectionId::new("store-b"), None)
                .unwrap(),
            0
        );
        for i in 0..45 {
            db.insert_pos_order(
                &shared::ids::ConnectionId::new("store-a"),
                &shared::ids::OrderId::new(format!("order-{i:02}")),
                None,
                None,
                100 + i,
            )
            .unwrap();
        }
        assert_eq!(
            db.count_pos_orders(&shared::ids::ConnectionId::new("store-a"), None)
                .unwrap(),
            46
        );
        assert_eq!(
            db.list_pos_orders(&shared::ids::ConnectionId::new("store-a"), 40, 0, None)
                .unwrap()
                .len(),
            40
        );
        assert_eq!(
            db.list_pos_orders(&shared::ids::ConnectionId::new("store-a"), 40, 40, None)
                .unwrap()
                .len(),
            6
        );
        assert_eq!(
            db.list_pos_orders(&shared::ids::ConnectionId::new("store-a"), 40, 0, None)
                .unwrap()[0]
                .order_id,
            shared::ids::OrderId::new("order-44")
        );
        assert_eq!(
            db.count_pos_orders(&shared::ids::ConnectionId::new("store-a"), Some("mia"))
                .unwrap(),
            1
        );
        assert_eq!(
            db.list_pos_orders(
                &shared::ids::ConnectionId::new("store-a"),
                40,
                0,
                Some("MIA")
            )
            .unwrap()[0]
                .order_id,
            shared::ids::OrderId::new("order-1")
        );
    }

    #[test]
    fn creating_a_user_then_reading_it_back_round_trips() {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("id-1"),
            "a@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();

        let row = db.get_user_by_email("a@example.com").unwrap().unwrap();
        assert_eq!(row.id, shared::ids::UserId::new("id-1"));
        assert_eq!(row.email, "a@example.com");
        assert_eq!(row.password_hash, "hash");
        assert_eq!(row.created_at, 1000);
        assert!(
            !row.is_admin,
            "an ordinary signup must never be an admin by default"
        );
    }

    #[test]
    fn creating_a_user_with_is_admin_true_persists_and_round_trips_the_flag() {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("admin-1"),
            "admin@example.com",
            "hash",
            true,
            1000,
        )
        .unwrap();

        let by_email = db.get_user_by_email("admin@example.com").unwrap().unwrap();
        assert!(by_email.is_admin);
        let by_id = db
            .get_user_by_id(&shared::ids::UserId::new("admin-1"))
            .unwrap()
            .unwrap();
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
        assert_eq!(
            db.get_setting("rate_limit_per_ip_per_min")
                .unwrap()
                .as_deref(),
            Some("10")
        );

        db.set_setting("rate_limit_per_ip_per_min", "25").unwrap();
        assert_eq!(
            db.get_setting("rate_limit_per_ip_per_min")
                .unwrap()
                .as_deref(),
            Some("25")
        );

        let all = db.list_settings().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(
            all.get("rate_limit_per_ip_per_min").map(String::as_str),
            Some("25")
        );
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
        assert!(
            !db.is_setup_complete().unwrap(),
            "a fresh database must start unsetup"
        );
        db.set_setting("setup_complete", "true").unwrap();
        assert!(db.is_setup_complete().unwrap());
    }

    #[test]
    fn a_duplicate_email_is_rejected_as_a_unique_violation() {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("id-1"),
            "a@example.com",
            "hash1",
            false,
            1000,
        )
        .unwrap();

        let err = db
            .create_user(
                &shared::ids::UserId::new("id-2"),
                "a@example.com",
                "hash2",
                false,
                2000,
            )
            .unwrap_err();
        assert!(
            err.is_unique_violation(),
            "expected a unique-violation error, got: {err:?}"
        );
    }

    #[test]
    fn looking_up_an_unknown_email_returns_none_rather_than_an_error() {
        let db = Db::open_in_memory().unwrap();
        assert!(db
            .get_user_by_email("nobody@example.com")
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_created_session_can_be_found_by_its_token_hash_and_resolves_to_its_user() {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("user-1"),
            "a@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        db.create_session(
            &th("hashed-token"),
            &shared::ids::UserId::new("user-1"),
            2000,
        )
        .unwrap();

        let session = db.find_session(&th("hashed-token"), 1000).unwrap().unwrap();
        assert_eq!(session.token_hash, th("hashed-token"));
        assert_eq!(session.user_id, shared::ids::UserId::new("user-1"));
        assert_eq!(session.created_at, 2000);

        let user = db.get_user_by_id(&session.user_id).unwrap().unwrap();
        assert_eq!(user.email, "a@example.com");
    }

    #[test]
    fn looking_up_an_unknown_session_token_returns_none_rather_than_an_error() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.find_session(&th("nonexistent"), 1000).unwrap().is_none());
    }

    #[test]
    fn deleting_a_session_removes_it_and_reports_whether_a_row_was_actually_deleted() {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("user-1"),
            "a@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        db.create_session(
            &th("hashed-token"),
            &shared::ids::UserId::new("user-1"),
            2000,
        )
        .unwrap();

        assert!(db.delete_session(&th("hashed-token")).unwrap());
        assert!(db
            .find_session(&th("hashed-token"), 1000)
            .unwrap()
            .is_none());
        assert!(!db.delete_session(&th("hashed-token")).unwrap());
    }

    #[test]
    fn creating_a_store_connection_then_reading_it_back_round_trips() {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("user-1"),
            "a@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        db.create_store_connection(
            &shared::ids::ConnectionId::new("conn-1"),
            &shared::ids::UserId::new("user-1"),
            "woocommerce",
            "https://shop.example.com",
            "pk_abc",
            "sk_abc",
            3000,
            "XMR",
        )
        .unwrap();

        let row = db
            .get_store_connection_by_id(&shared::ids::ConnectionId::new("conn-1"))
            .unwrap()
            .unwrap();
        assert_eq!(row.user_id, shared::ids::UserId::new("user-1"));
        assert_eq!(row.platform, "woocommerce");
        assert_eq!(row.site_url, "https://shop.example.com");
        assert_eq!(row.tenant_public_key, "pk_abc");
        assert_eq!(row.tenant_secret_token_encrypted, "sk_abc");
        assert_eq!(row.created_at, 3000);
        assert_eq!(
            row.fx_providers,
            vec!["coingecko"],
            "every new store defaults to coingecko"
        );
    }

    #[test]
    fn updating_a_store_connections_fx_providers_only_touches_that_field() {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("user-1"),
            "a@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        db.create_store_connection(
            &shared::ids::ConnectionId::new("conn-1"),
            &shared::ids::UserId::new("user-1"),
            "woocommerce",
            "https://shop.example.com",
            "pk_abc",
            "sk_abc",
            3000,
            "XMR",
        )
        .unwrap();

        let order = vec!["coinmarketcap".to_string(), "coingecko".to_string()];
        db.update_store_connection_fx(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            &shared::ids::ConnectionId::new("conn-1"),
            &order,
            &FxProviderSettings::default(),
        )
        .unwrap();

        let row = db
            .get_store_connection_by_id(&shared::ids::ConnectionId::new("conn-1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            row.fx_providers, order,
            "the saved order is the order read back"
        );
        // Nothing else changed.
        assert_eq!(row.site_url, "https://shop.example.com");
        assert_eq!(row.tenant_public_key, "pk_abc");
    }

    fn db_with_store() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("user-1"),
            "a@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        db.create_store_connection(
            &shared::ids::ConnectionId::new("conn-1"),
            &shared::ids::UserId::new("user-1"),
            "woocommerce",
            "https://shop.example.com",
            "pk_abc",
            "sk_abc",
            3000,
            "XMR",
        )
        .unwrap();
        db
    }

    #[test]
    fn a_new_store_has_default_provider_settings() {
        let row = db_with_store()
            .get_store_connection_by_id(&shared::ids::ConnectionId::new("conn-1"))
            .unwrap()
            .unwrap();
        assert_eq!(row.fx_provider_settings, FxProviderSettings::default());
    }

    #[test]
    fn provider_settings_are_saved_with_the_order_and_read_back_on_every_lookup() {
        let db = db_with_store();
        let mut settings = FxProviderSettings::default();
        settings.haveno.currencies = vec!["USD".to_string(), "EUR".to_string()];
        settings.haveno.max_spread_pct = 2.5;
        settings.haveno.min_offers_per_side = 3;
        settings.haveno.min_depth_xmr_per_side = 1.5;
        db.update_store_connection_fx(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            &shared::ids::ConnectionId::new("conn-1"),
            &["haveno".to_string()],
            &settings,
        )
        .unwrap();

        let by_id = db
            .get_store_connection_by_id(&shared::ids::ConnectionId::new("conn-1"))
            .unwrap()
            .unwrap();
        assert_eq!(by_id.fx_provider_settings, settings);
        assert_eq!(by_id.fx_providers, vec!["haveno"]);
        // The other lookups read the same column.
        assert_eq!(
            db.get_store_connection_by_public_key("pk_abc")
                .unwrap()
                .unwrap()
                .fx_provider_settings,
            settings
        );
        assert_eq!(
            db.list_store_connections_for_user(&shared::ids::UserId::new("user-1"))
                .unwrap()[0]
                .fx_provider_settings,
            settings
        );
    }

    #[test]
    fn saving_one_stores_settings_leaves_every_other_store_untouched() {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("user-a"),
            "a@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        db.create_user(
            &shared::ids::UserId::new("user-b"),
            "b@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        for (id, user, pk) in [
            ("conn-a", "user-a", "pk_a"),
            ("conn-b", "user-b", "pk_b"),
            ("conn-a2", "user-a", "pk_a2"),
        ] {
            db.create_store_connection(
                &shared::ids::ConnectionId::new(id.to_string()),
                &shared::ids::UserId::new(user.to_string()),
                "woocommerce",
                &format!("https://{id}.example.com"),
                pk,
                "sk",
                3000,
                "XMR",
            )
            .unwrap();
        }
        let before_b = db
            .get_store_connection_by_id(&shared::ids::ConnectionId::new("conn-b"))
            .unwrap()
            .unwrap();
        let before_a2 = db
            .get_store_connection_by_id(&shared::ids::ConnectionId::new("conn-a2"))
            .unwrap()
            .unwrap();

        let mut settings = FxProviderSettings::default();
        settings.haveno.max_spread_pct = 0.5;
        settings.haveno.currencies = vec!["USD".to_string()];
        db.update_store_connection_fx(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            &shared::ids::ConnectionId::new("conn-a"),
            &["haveno".to_string(), "coingecko".to_string()],
            &settings,
        )
        .unwrap();

        let a = db
            .get_store_connection_by_id(&shared::ids::ConnectionId::new("conn-a"))
            .unwrap()
            .unwrap();
        assert_eq!(
            (a.fx_providers, a.fx_provider_settings),
            (
                vec!["haveno".to_string(), "coingecko".to_string()],
                settings
            )
        );
        for (id, before) in [("conn-b", before_b), ("conn-a2", before_a2)] {
            let after = db
                .get_store_connection_by_id(&shared::ids::ConnectionId::new(id.to_string()))
                .unwrap()
                .unwrap();
            assert_eq!(
                after.fx_providers, before.fx_providers,
                "{id}: providers unchanged"
            );
            assert_eq!(
                after.fx_provider_settings, before.fx_provider_settings,
                "{id}: settings unchanged"
            );
            assert_eq!(
                after.fx_provider_settings,
                FxProviderSettings::default(),
                "{id}: still the defaults"
            );
        }
    }

    #[test]
    fn a_corrupt_settings_cell_reads_as_the_defaults_not_a_failed_lookup() {
        let db = db_with_store();
        db.conn
            .execute(
                "UPDATE store_connections SET fx_provider_settings = 'garbage' WHERE id = 'conn-1'",
                [],
            )
            .unwrap();
        let row = db
            .get_store_connection_by_id(&shared::ids::ConnectionId::new("conn-1"))
            .unwrap()
            .unwrap();
        assert_eq!(row.fx_provider_settings, FxProviderSettings::default());
    }

    #[test]
    fn migration_0028_gives_existing_stores_the_default_settings() {
        let conn = Connection::open_in_memory().unwrap();
        shared::migrations::apply(&conn, &MIGRATIONS[..27]).unwrap();
        conn.execute("INSERT INTO users (id, email, password_hash, created_at_utc) VALUES ('user-1', 'a@example.com', 'hash', 1000)", []).unwrap();
        conn.execute(
            "INSERT INTO store_connections
                (id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, moneropay_endpoint, created_at_utc, fx_providers, base_currency)
             VALUES ('conn-1', 'user-1', 'woocommerce', 'https://shop.example.com', 'pk_abc', 'sk_abc', 'http://127.0.0.1:8080', 3000, '[\"coingecko\"]', 'XMR')",
            [],
        )
        .unwrap();

        shared::migrations::apply(&conn, MIGRATIONS).unwrap();

        let db = Db { conn };
        let row = db
            .get_store_connection_by_id(&shared::ids::ConnectionId::new("conn-1"))
            .unwrap()
            .unwrap();
        assert_eq!(row.fx_provider_settings, FxProviderSettings::default());
        assert_eq!(row.fx_providers, vec!["coingecko"]);
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
        let row = db
            .get_store_connection_by_id(&shared::ids::ConnectionId::new("conn-1"))
            .unwrap()
            .unwrap();
        assert_eq!(row.fx_providers, vec!["coingecko"]);
    }

    #[test]
    fn migration_0027_turns_the_single_provider_into_a_one_element_list() {
        let conn = Connection::open_in_memory().unwrap();
        shared::migrations::apply(&conn, &MIGRATIONS[..26]).unwrap();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, created_at_utc) VALUES ('user-1', 'a@example.com', 'hash', 1000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO store_connections
                (id, user_id, platform, site_url, tenant_public_key, tenant_secret_token_encrypted, moneropay_endpoint, created_at_utc, fx_provider, base_currency)
             VALUES ('conn-1', 'user-1', 'woocommerce', 'https://shop.example.com', 'pk_abc', 'sk_abc', 'http://127.0.0.1:8080', 3000, 'coingecko', 'XMR')",
            [],
        )
        .unwrap();

        shared::migrations::apply(&conn, MIGRATIONS).unwrap();

        let db = Db { conn };
        assert_eq!(
            db.get_store_connection_by_id(&shared::ids::ConnectionId::new("conn-1"))
                .unwrap()
                .unwrap()
                .fx_providers,
            vec!["coingecko"]
        );
    }

    #[test]
    fn a_bare_provider_name_in_the_column_still_reads_as_a_one_element_list() {
        assert_eq!(parse_fx_providers(r#"["a","b"]"#), vec!["a", "b"]);
        assert_eq!(parse_fx_providers("coingecko"), vec!["coingecko"]);
        assert!(parse_fx_providers("[]").is_empty());
        assert!(parse_fx_providers("").is_empty());
    }

    #[test]
    fn looking_up_an_unknown_store_connection_id_returns_none_rather_than_an_error() {
        let db = Db::open_in_memory().unwrap();
        assert!(db
            .get_store_connection_by_id(&shared::ids::ConnectionId::new("nonexistent"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn updating_a_store_connections_site_url_only_touches_that_field() {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("user-1"),
            "a@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        db.create_store_connection(
            &shared::ids::ConnectionId::new("conn-1"),
            &shared::ids::UserId::new("user-1"),
            "woocommerce",
            "https://old-site.example.com",
            "pk_abc",
            "sk_abc",
            3000,
            "XMR",
        )
        .unwrap();

        db.update_store_connection_site_url(
            &shared::ids::ConnectionId::new("conn-1"),
            "https://new-site.example.com",
        )
        .unwrap();

        let row = db
            .get_store_connection_by_id(&shared::ids::ConnectionId::new("conn-1"))
            .unwrap()
            .unwrap();
        assert_eq!(row.site_url, "https://new-site.example.com");
        // Nothing else changed.
        assert_eq!(row.platform, "woocommerce");
        assert_eq!(row.tenant_public_key, "pk_abc");
        assert_eq!(row.tenant_secret_token_encrypted, "sk_abc");
    }

    #[test]
    fn client_logging_is_off_until_the_store_opts_in() {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("user-cl"),
            "cl@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        db.create_store_connection(
            &shared::ids::ConnectionId::new("conn-cl"),
            &shared::ids::UserId::new("user-cl"),
            "woocommerce",
            "https://shop.example.com",
            "pk_cl",
            "sk_cl",
            3000,
            "XMR",
        )
        .unwrap();
        assert!(!db
            .client_logging(&shared::ids::ConnectionId::new("conn-cl"))
            .unwrap());
        assert!(!db.client_logging_by_public_key("pk_cl").unwrap());
        db.set_client_logging(&shared::ids::ConnectionId::new("conn-cl"), true)
            .unwrap();
        assert!(db
            .client_logging(&shared::ids::ConnectionId::new("conn-cl"))
            .unwrap());
        assert!(db.client_logging_by_public_key("pk_cl").unwrap());
        assert!(
            !db.client_logging(&shared::ids::ConnectionId::new("conn-missing"))
                .unwrap(),
            "no store, no logs"
        );
        assert!(!db.client_logging_by_public_key("pk_missing").unwrap());
    }

    #[test]
    fn creating_a_store_connection_then_reading_it_back_by_public_key_round_trips() {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("user-2"),
            "b@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        db.create_store_connection(
            &shared::ids::ConnectionId::new("conn-2"),
            &shared::ids::UserId::new("user-2"),
            "woocommerce",
            "https://shop.example.com",
            "pk_xyz",
            "sk_xyz",
            3000,
            "XMR",
        )
        .unwrap();

        let row = db
            .get_store_connection_by_public_key("pk_xyz")
            .unwrap()
            .unwrap();
        assert_eq!(row.id, shared::ids::ConnectionId::new("conn-2"));
        assert_eq!(row.user_id, shared::ids::UserId::new("user-2"));
    }

    #[test]
    fn looking_up_an_unknown_public_key_returns_none_rather_than_an_error() {
        let db = Db::open_in_memory().unwrap();
        assert!(db
            .get_store_connection_by_public_key("pk_nonexistent")
            .unwrap()
            .is_none());
    }

    fn seed_connection_for_connect_token_tests(db: &Db) -> String {
        db.create_user(
            &shared::ids::UserId::new("user-ct"),
            "connect-tokens@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        db.create_store_connection(
            &shared::ids::ConnectionId::new("conn-ct"),
            &shared::ids::UserId::new("user-ct"),
            "woocommerce",
            "https://shop.example.com",
            "pk_ct",
            "sk_ct",
            1000,
            "XMR",
        )
        .unwrap();
        "conn-ct".to_string()
    }

    #[test]
    fn creating_a_connect_token_then_consuming_it_returns_its_connection_id() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_connect_token(
            &th("hashed-connect-token"),
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "nonce-1",
            2000,
        )
        .unwrap();

        let resolved = db
            .consume_connect_token(&th("hashed-connect-token"), 2001, 600)
            .unwrap();
        assert_eq!(
            resolved,
            Some(shared::ids::ConnectionId::new(connection_id))
        );
    }

    #[test]
    fn consuming_the_same_connect_token_twice_only_succeeds_once() {
        // The load-bearing single-use proof at the `Db` layer - see
        // `Db::consume_connect_token`'s own doc comment on why the
        // underlying `UPDATE` must be atomic for this to hold under
        // concurrency, not just under this single-threaded test.
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_connect_token(
            &th("hashed-connect-token"),
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "nonce-1",
            2000,
        )
        .unwrap();

        let first = db
            .consume_connect_token(&th("hashed-connect-token"), 2001, 600)
            .unwrap();
        assert_eq!(
            first,
            Some(shared::ids::ConnectionId::new(connection_id.clone())),
            "the first consume must actually succeed"
        );

        let second = db
            .consume_connect_token(&th("hashed-connect-token"), 2002, 600)
            .unwrap();
        assert_eq!(second, None, "a second consume of the same token must fail");
    }

    #[test]
    fn consuming_an_expired_connect_token_fails_as_if_it_never_existed() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_connect_token(
            &th("hashed-connect-token"),
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "nonce-1",
            1000,
        )
        .unwrap();

        // created_at = 1000, ttl = 600 seconds - "now" = 1601 is one second
        // past the token's expiry window.
        let resolved = db
            .consume_connect_token(&th("hashed-connect-token"), 1601, 600)
            .unwrap();
        assert_eq!(resolved, None, "expired token must not be consumable");
    }

    #[test]
    fn consuming_an_unknown_connect_token_returns_none() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(
            db.consume_connect_token(&th("nonexistent-connect-token"), 2000, 600)
                .unwrap(),
            None
        );
    }

    #[test]
    fn reopening_an_existing_database_file_does_not_reapply_migrations() {
        // Same regression this guards against in `engine::store`:
        // re-running the raw `CREATE TABLE` DDL against an already-migrated
        // file crashes with "table already exists".
        let path = std::env::temp_dir().join(format!("monokulo_test_{}.db", uuid::Uuid::new_v4()));
        let path_str = path.to_str().unwrap();

        let db = Db::open_file(path_str).unwrap();
        db.create_user(
            &shared::ids::UserId::new("id-1"),
            "a@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        drop(db);

        let reopened = Db::open_file(path_str).unwrap();
        let row = reopened
            .get_user_by_email("a@example.com")
            .unwrap()
            .unwrap();
        assert_eq!(row.id, shared::ids::UserId::new("id-1"));

        drop(reopened);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    /// A rate too large for SQLite's integers is an error to the caller,
    /// never a panic in the request that tried to store it.
    #[test]
    fn an_out_of_range_rate_is_refused_not_panicked_on() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        let stored = db.create_order_currency_metadata(
            &ConnectionId::new(connection_id.to_string()),
            &OrderId::new("pay_huge"),
            "USD",
            "1.00",
            Piconero(u64::MAX),
            "fixed",
            1000,
            "XMR",
            None,
            10,
            false,
            None,
        );
        assert!(matches!(stored, Err(DbError::Sqlite(_))), "{stored:?}");
    }

    #[test]
    fn creating_order_fiat_metadata_then_reading_it_back_round_trips() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_order_currency_metadata(
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            &shared::ids::OrderId::new("pay_1"),
            "USD",
            "25.00",
            shared::xmr_amount::Piconero(6_700_000_000),
            "fixed",
            1000,
            "XMR",
            None,
            10,
            false,
            None,
        )
        .unwrap();

        let row = db
            .get_order_currency_metadata(
                &shared::ids::ConnectionId::new(connection_id.to_string()),
                &shared::ids::OrderId::new("pay_1"),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            row.connection_id,
            shared::ids::ConnectionId::new(connection_id)
        );
        assert_eq!(row.order_id, shared::ids::OrderId::new("pay_1"));
        assert_eq!(row.currency, "USD");
        assert_eq!(row.amount, "25.00");
        assert_eq!(
            row.piconero_per_unit,
            shared::xmr_amount::Piconero(6_700_000_000)
        );
        assert_eq!(row.provider, "fixed");
        assert_eq!(row.created_at, 1000);
        assert_eq!(row.store_base_currency, Some("XMR".to_string()));
        assert_eq!(row.base_currency_piconero_per_unit, None);
        assert_eq!(row.confirmations_required_applied, Some(10));
    }

    #[test]
    fn looking_up_fiat_metadata_for_an_unknown_order_id_returns_none() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        assert!(db
            .get_order_currency_metadata(
                &shared::ids::ConnectionId::new(connection_id.to_string()),
                &shared::ids::OrderId::new("nonexistent")
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn fiat_metadata_is_scoped_by_connection_id_even_for_the_same_order_id() {
        // Two different connections can each have their own order with the
        // same order_id (the engine's order_id is only unique within one
        // tenant) - the composite primary key must keep them apart.
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_user(
            &shared::ids::UserId::new("user-2"),
            "other@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        db.create_store_connection(
            &shared::ids::ConnectionId::new("conn-2"),
            &shared::ids::UserId::new("user-2"),
            "custom",
            "https://other.example.com",
            "pk_other",
            "sk_other",
            1000,
            "XMR",
        )
        .unwrap();

        db.create_order_currency_metadata(
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            &shared::ids::OrderId::new("pay_shared"),
            "USD",
            "10.00",
            shared::xmr_amount::Piconero(1_000_000),
            "fixed",
            1000,
            "XMR",
            None,
            10,
            false,
            None,
        )
        .unwrap();
        db.create_order_currency_metadata(
            &shared::ids::ConnectionId::new("conn-2"),
            &shared::ids::OrderId::new("pay_shared"),
            "EUR",
            "20.00",
            shared::xmr_amount::Piconero(2_000_000),
            "coingecko",
            2000,
            "XMR",
            None,
            10,
            false,
            None,
        )
        .unwrap();

        let first = db
            .get_order_currency_metadata(
                &shared::ids::ConnectionId::new(connection_id.to_string()),
                &shared::ids::OrderId::new("pay_shared"),
            )
            .unwrap()
            .unwrap();
        let second = db
            .get_order_currency_metadata(
                &shared::ids::ConnectionId::new("conn-2"),
                &shared::ids::OrderId::new("pay_shared"),
            )
            .unwrap()
            .unwrap();
        assert_eq!(first.currency, "USD");
        assert_eq!(second.currency, "EUR");
        assert_eq!(first.provider, "fixed");
        assert_eq!(second.provider, "coingecko");
    }

    #[test]
    fn listing_fiat_metadata_for_a_connection_returns_a_map_keyed_by_order_id() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_order_currency_metadata(
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            &shared::ids::OrderId::new("pay_a"),
            "USD",
            "10.00",
            shared::xmr_amount::Piconero(1_000_000),
            "fixed",
            1000,
            "XMR",
            None,
            10,
            false,
            None,
        )
        .unwrap();
        db.create_order_currency_metadata(
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            &shared::ids::OrderId::new("pay_b"),
            "EUR",
            "20.00",
            shared::xmr_amount::Piconero(2_000_000),
            "fixed",
            2000,
            "XMR",
            None,
            10,
            false,
            None,
        )
        .unwrap();

        let map = db
            .order_currency_metadata_for(
                &shared::ids::ConnectionId::new(connection_id.to_string()),
                &[
                    shared::ids::OrderId::new("pay_a"),
                    shared::ids::OrderId::new("pay_b"),
                    shared::ids::OrderId::new("pay_unrecorded"),
                ],
            )
            .unwrap();
        assert_eq!(map.len(), 2, "an order with nothing recorded is left out");
        assert_eq!(
            map.get(&shared::ids::OrderId::new("pay_a"))
                .unwrap()
                .currency,
            "USD"
        );
        assert_eq!(
            map.get(&shared::ids::OrderId::new("pay_b"))
                .unwrap()
                .currency,
            "EUR"
        );
    }

    #[test]
    fn listing_fiat_metadata_for_a_connection_with_none_recorded_is_an_empty_map_not_an_error() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        assert!(db
            .order_currency_metadata_for(
                &shared::ids::ConnectionId::new(connection_id.to_string()),
                &[shared::ids::OrderId::new("pay_a")],
            )
            .unwrap()
            .is_empty());
    }

    #[test]
    fn list_currencies_returns_the_seeded_reference_data_in_alphabetical_order() {
        let db = Db::open_in_memory().unwrap();
        let all = db.list_currencies().unwrap();
        assert!(all.len() >= 15);
        let codes: Vec<&str> = all.iter().map(|c| c.canonical_code.as_str()).collect();
        let mut sorted = codes.clone();
        sorted.sort();
        assert_eq!(
            codes, sorted,
            "expected the query's own ORDER BY to already be alphabetical"
        );
        let usd = all
            .iter()
            .find(|c| c.canonical_code == "USD")
            .expect("USD must be seeded");
        assert_eq!(usd.description, "United States Dollar");
    }

    #[test]
    fn a_created_confirmation_threshold_round_trips() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "thresh-1",
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "50.00",
            20,
            1000,
        )
        .unwrap();

        let rows = db
            .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string(),
            ))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].unit_amount, "50.00");
        assert_eq!(rows[0].confirmations_required, 20);
    }

    #[test]
    fn concurrent_threshold_adds_never_exceed_five() {
        let db = Arc::new(Mutex::new(Db::open_in_memory().unwrap()));
        let connection_id = seed_connection_for_connect_token_tests(&db.lock());
        let threads: Vec<_> = (0..16)
            .map(|i| {
                let db = db.clone();
                let connection_id = connection_id.clone();
                std::thread::spawn(move || {
                    db.lock()
                        .create_confirmation_threshold_with_limit(
                            crate::confirmation_thresholds::PolicyProof::for_test(),
                            &format!("id-{i}"),
                            &shared::ids::ConnectionId::new(connection_id.to_string()),
                            &i.to_string(),
                            10,
                            1000,
                        )
                        .unwrap()
                })
            })
            .collect();
        assert_eq!(
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .filter(|inserted| *inserted)
                .count(),
            5
        );
        assert_eq!(
            db.lock()
                .count_confirmation_thresholds(&shared::ids::ConnectionId::new(
                    connection_id.to_string()
                ))
                .unwrap(),
            5
        );
    }

    #[test]
    fn a_failed_threshold_replacement_rolls_back_deletions() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "original",
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "50",
            20,
            1000,
        )
        .unwrap();
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "taken",
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "100",
            20,
            1000,
        )
        .unwrap();
        let result = db.replace_confirmation_thresholds(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            &["original".to_string()],
            Some(("taken", "200", 10, 1000)),
        );
        assert!(result.is_err());
        assert!(db
            .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string()
            ))
            .unwrap()
            .iter()
            .any(|row| row.id == "original"));
    }

    #[test]
    fn replacing_confirmation_thresholds_at_the_cap_is_atomic() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        for i in 0..5 {
            db.create_confirmation_threshold_with_limit(
                crate::confirmation_thresholds::PolicyProof::for_test(),
                &format!("id-{i}"),
                &shared::ids::ConnectionId::new(connection_id.to_string()),
                &i.to_string(),
                10,
                1000,
            )
            .unwrap();
        }
        assert!(!db
            .replace_confirmation_thresholds(
                crate::confirmation_thresholds::PolicyProof::for_test(),
                &shared::ids::ConnectionId::new(connection_id.to_string()),
                &[],
                Some(("sixth", "50", 20, 1000))
            )
            .unwrap());
        assert_eq!(
            db.count_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string()
            ))
            .unwrap(),
            5
        );
        assert!(db
            .replace_confirmation_thresholds(
                crate::confirmation_thresholds::PolicyProof::for_test(),
                &shared::ids::ConnectionId::new(connection_id.to_string()),
                &["id-0".to_string()],
                Some(("replacement", "50", 20, 1000))
            )
            .unwrap());
        let rows = db
            .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string(),
            ))
            .unwrap();
        assert_eq!(rows.len(), 5);
        assert!(!rows.iter().any(|row| row.id == "id-0"));
        assert!(rows.iter().any(|row| row.id == "replacement"));
    }

    #[test]
    fn confirmation_thresholds_list_in_ascending_numeric_order_not_lexicographic() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        // Lexicographically "10" < "9" - this must not happen here.
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "thresh-a",
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "9",
            15,
            1000,
        )
        .unwrap();
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "thresh-b",
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "10",
            20,
            1000,
        )
        .unwrap();
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "thresh-c",
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "2.5",
            12,
            1000,
        )
        .unwrap();

        let rows = db
            .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string(),
            ))
            .unwrap();
        let amounts: Vec<&str> = rows.iter().map(|r| r.unit_amount.as_str()).collect();
        assert_eq!(amounts, vec!["2.5", "9", "10"]);
    }

    #[test]
    fn confirmation_thresholds_preserve_adjacent_twelve_decimal_order() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "upper",
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "9007.199254740981",
            20,
            1000,
        )
        .unwrap();
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "lower",
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "9007.199254740980",
            10,
            1000,
        )
        .unwrap();
        let rows = db
            .list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string(),
            ))
            .unwrap();
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec!["lower", "upper"]
        );
    }

    #[test]
    fn creating_a_second_threshold_at_the_same_amount_is_a_unique_violation() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "thresh-1",
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "50.00",
            20,
            1000,
        )
        .unwrap();

        let err = db
            .create_confirmation_threshold_with_limit(
                crate::confirmation_thresholds::PolicyProof::for_test(),
                "thresh-2",
                &shared::ids::ConnectionId::new(connection_id.to_string()),
                "50.00",
                5,
                1000,
            )
            .unwrap_err();
        assert!(
            err.is_unique_violation(),
            "expected a unique-violation error, got {err:?}"
        );
        // Nothing about the original row changed.
        assert_eq!(
            db.list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string()
            ))
            .unwrap()[0]
                .confirmations_required,
            20
        );
    }

    #[test]
    fn the_same_amount_is_allowed_again_on_a_different_connection() {
        let db = Db::open_in_memory().unwrap();
        let connection_id_a = seed_connection_for_connect_token_tests(&db);
        db.create_user(
            &shared::ids::UserId::new("user-b"),
            "b@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        db.create_store_connection(
            &shared::ids::ConnectionId::new("conn-b"),
            &shared::ids::UserId::new("user-b"),
            "custom",
            "https://b.example.com",
            "pk_b",
            "sk_b",
            1000,
            "XMR",
        )
        .unwrap();

        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "thresh-1",
            &shared::ids::ConnectionId::new(connection_id_a.to_string()),
            "50.00",
            20,
            1000,
        )
        .unwrap();
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "thresh-2",
            &shared::ids::ConnectionId::new("conn-b"),
            "50.00",
            5,
            1000,
        )
        .unwrap();

        assert_eq!(
            db.list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id_a.to_string()
            ))
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            db.list_confirmation_thresholds(&shared::ids::ConnectionId::new("conn-b"))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn count_confirmation_thresholds_reflects_the_real_count() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        assert_eq!(
            db.count_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string()
            ))
            .unwrap(),
            0
        );
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "thresh-1",
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "50.00",
            20,
            1000,
        )
        .unwrap();
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "thresh-2",
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "100.00",
            30,
            1000,
        )
        .unwrap();
        assert_eq!(
            db.count_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string()
            ))
            .unwrap(),
            2
        );
    }

    #[test]
    fn deleting_a_confirmation_threshold_is_scoped_to_the_owning_connection() {
        let db = Db::open_in_memory().unwrap();
        let connection_id_a = seed_connection_for_connect_token_tests(&db);
        db.create_user(
            &shared::ids::UserId::new("user-b"),
            "b@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        db.create_store_connection(
            &shared::ids::ConnectionId::new("conn-b"),
            &shared::ids::UserId::new("user-b"),
            "custom",
            "https://b.example.com",
            "pk_b",
            "sk_b",
            1000,
            "XMR",
        )
        .unwrap();
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "thresh-1",
            &shared::ids::ConnectionId::new(connection_id_a.to_string()),
            "50.00",
            20,
            1000,
        )
        .unwrap();

        // conn-b cannot delete connection_id_a's own threshold.
        assert!(!db
            .delete_confirmation_threshold(
                crate::confirmation_thresholds::PolicyProof::for_test(),
                &shared::ids::ConnectionId::new("conn-b"),
                "thresh-1"
            )
            .unwrap());
        assert_eq!(
            db.list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id_a.to_string()
            ))
            .unwrap()
            .len(),
            1
        );

        assert!(db
            .delete_confirmation_threshold(
                crate::confirmation_thresholds::PolicyProof::for_test(),
                &shared::ids::ConnectionId::new(connection_id_a.to_string()),
                "thresh-1"
            )
            .unwrap());
        assert_eq!(
            db.list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id_a.to_string()
            ))
            .unwrap()
            .len(),
            0
        );

        // A second delete of the same (now-gone) row is a clean no-op.
        assert!(!db
            .delete_confirmation_threshold(
                crate::confirmation_thresholds::PolicyProof::for_test(),
                &shared::ids::ConnectionId::new(connection_id_a.to_string()),
                "thresh-1"
            )
            .unwrap());
    }

    #[test]
    fn changing_a_stores_base_currency_deletes_every_one_of_its_custom_thresholds() {
        let db = Db::open_in_memory().unwrap();
        let connection_id = seed_connection_for_connect_token_tests(&db);
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "thresh-1",
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "50.00",
            20,
            1000,
        )
        .unwrap();
        db.create_confirmation_threshold_with_limit(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            "thresh-2",
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "100.00",
            30,
            1000,
        )
        .unwrap();

        db.update_store_connection_base_currency(
            crate::confirmation_thresholds::PolicyProof::for_test(),
            &shared::ids::ConnectionId::new(connection_id.to_string()),
            "EUR",
        )
        .unwrap();

        assert_eq!(
            db.get_store_connection_by_id(&shared::ids::ConnectionId::new(
                connection_id.to_string()
            ))
            .unwrap()
            .unwrap()
            .base_currency,
            "EUR"
        );
        assert_eq!(
            db.list_confirmation_thresholds(&shared::ids::ConnectionId::new(
                connection_id.to_string()
            ))
            .unwrap()
            .len(),
            0,
            "every custom threshold must be gone"
        );
    }

    #[test]
    fn a_request_invite_submission_creates_a_row_only_the_admin_can_see_and_it_carries_no_link_until_one_is_made(
    ) {
        let db = Db::open_in_memory().unwrap();
        db.create_invite_request("req-1", "hopeful@example.com", "please let me in", 1000)
            .unwrap();
        assert_eq!(db.count_unactioned_invite_requests().unwrap(), 1);
        let rows = db.list_unactioned_invite_requests(10, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].email, "hopeful@example.com");
        assert_eq!(rows[0].message, "please let me in");
        assert_eq!(
            rows[0].invite_token_encrypted, None,
            "no link has been created for this request yet"
        );
    }

    #[test]
    fn a_request_linked_invite_link_is_visible_on_the_request_row_once_created() {
        let db = Db::open_in_memory().unwrap();
        db.create_invite_request("req-1", "hopeful@example.com", "please", 1000)
            .unwrap();
        db.create_invite_link(
            "link-1",
            &th("hash-of-token"),
            Some("encrypted-blob"),
            Some("req-1"),
            1000,
        )
        .unwrap();

        let rows = db.list_unactioned_invite_requests(10, 0).unwrap();
        assert_eq!(
            rows[0].invite_token_encrypted.as_deref(),
            Some("encrypted-blob")
        );
    }

    #[test]
    fn deleting_a_request_hides_it_and_revokes_its_own_unused_link() {
        let db = Db::open_in_memory().unwrap();
        db.create_invite_request("req-1", "a@example.com", "m", 1000)
            .unwrap();
        db.create_invite_link("link-1", &th("hash-1"), Some("enc-1"), Some("req-1"), 1000)
            .unwrap();

        db.delete_invite_request("req-1", 2000).unwrap();

        assert_eq!(
            db.count_unactioned_invite_requests().unwrap(),
            0,
            "a deleted request must not still be listed as pending"
        );
        assert_eq!(
            db.redeem_invite_and_create_user(
                &th("hash-1"),
                &shared::ids::UserId::new("u1"),
                "a@example.com",
                "hash",
                3000
            )
            .unwrap(),
            RedeemInviteResult::InvalidOrAlreadyUsed,
            "its own never-used link must have been revoked, not left silently valid"
        );
    }

    #[test]
    fn deleting_a_request_does_not_touch_a_link_that_was_already_used() {
        let db = Db::open_in_memory().unwrap();
        db.create_invite_request("req-1", "a@example.com", "m", 1000)
            .unwrap();
        db.create_invite_link("link-1", &th("hash-1"), Some("enc-1"), Some("req-1"), 1000)
            .unwrap();
        // Someone already redeemed it before the admin got around to
        // deleting the (by then auto-actioned) request - a no-op path this
        // handler should still tolerate cleanly.
        db.redeem_invite_and_create_user(
            &th("hash-1"),
            &shared::ids::UserId::new("u1"),
            "a@example.com",
            "hash",
            1500,
        )
        .unwrap();

        db.delete_invite_request("req-1", 2000).unwrap();
        // The already-created account's own session/lookup path is
        // untouched - deleting the request never un-creates a real user.
        assert!(db.get_user_by_email("a@example.com").unwrap().is_some());
    }

    #[test]
    fn delete_all_clears_every_unactioned_request_and_revokes_every_unused_link_but_leaves_actioned_ones_alone(
    ) {
        let db = Db::open_in_memory().unwrap();
        db.create_invite_request("req-1", "a@example.com", "m", 1000)
            .unwrap();
        db.create_invite_link("link-1", &th("hash-1"), Some("enc-1"), Some("req-1"), 1000)
            .unwrap();
        db.create_invite_request("req-2", "b@example.com", "m", 1000)
            .unwrap();
        db.create_invite_link("link-2", &th("hash-2"), Some("enc-2"), Some("req-2"), 1000)
            .unwrap();
        db.create_invite_request("req-3", "c@example.com", "m", 1000)
            .unwrap();
        db.delete_invite_request("req-3", 1500).unwrap(); // already actioned before delete-all runs

        let cleared = db.delete_all_unactioned_invite_requests(2000).unwrap();
        assert_eq!(
            cleared, 2,
            "req-3 was already actioned and must not be double-counted"
        );
        assert_eq!(db.count_unactioned_invite_requests().unwrap(), 0);
        assert_eq!(
            db.redeem_invite_and_create_user(
                &th("hash-1"),
                &shared::ids::UserId::new("u1"),
                "a@example.com",
                "h",
                3000
            )
            .unwrap(),
            RedeemInviteResult::InvalidOrAlreadyUsed
        );
        assert_eq!(
            db.redeem_invite_and_create_user(
                &th("hash-2"),
                &shared::ids::UserId::new("u2"),
                "b@example.com",
                "h",
                3000
            )
            .unwrap(),
            RedeemInviteResult::InvalidOrAlreadyUsed
        );
    }

    #[test]
    fn a_valid_invite_token_redeems_exactly_once() {
        let db = Db::open_in_memory().unwrap();
        db.create_invite_link("link-1", &th("hash-1"), None, None, 1000)
            .unwrap();

        let first = db
            .redeem_invite_and_create_user(
                &th("hash-1"),
                &shared::ids::UserId::new("user-1"),
                "first@example.com",
                "hashed-pw",
                2000,
            )
            .unwrap();
        assert_eq!(first, RedeemInviteResult::Created);
        assert!(db.get_user_by_email("first@example.com").unwrap().is_some());

        // The real point of this whole feature: a second attempt against
        // the exact same token, even with a different email, must fail -
        // "more than one account cannot be registered using the same link".
        let second = db
            .redeem_invite_and_create_user(
                &th("hash-1"),
                &shared::ids::UserId::new("user-2"),
                "second@example.com",
                "hashed-pw",
                3000,
            )
            .unwrap();
        assert_eq!(second, RedeemInviteResult::InvalidOrAlreadyUsed);
        assert!(
            db.get_user_by_email("second@example.com")
                .unwrap()
                .is_none(),
            "a rejected redemption must not create an account"
        );
    }

    #[test]
    fn an_unknown_token_is_rejected_without_creating_an_account() {
        let db = Db::open_in_memory().unwrap();
        let result = db
            .redeem_invite_and_create_user(
                &th("no-such-hash"),
                &shared::ids::UserId::new("user-1"),
                "a@example.com",
                "hashed-pw",
                1000,
            )
            .unwrap();
        assert_eq!(result, RedeemInviteResult::InvalidOrAlreadyUsed);
        assert!(db.get_user_by_email("a@example.com").unwrap().is_none());
    }

    /// A sign-up with an email that is taken creates nothing and leaves the
    /// invite unclaimed: the whole redemption is one transaction, so the
    /// person can try again with the right email.
    #[test]
    fn a_duplicate_email_leaves_the_invite_unclaimed_for_another_try() {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("existing"),
            "taken@example.com",
            "hash",
            false,
            500,
        )
        .unwrap();
        db.create_invite_link("link-1", &th("hash-1"), None, None, 1000)
            .unwrap();

        let result = db
            .redeem_invite_and_create_user(
                &th("hash-1"),
                &shared::ids::UserId::new("user-2"),
                "taken@example.com",
                "hashed-pw",
                2000,
            )
            .unwrap();
        assert_eq!(result, RedeemInviteResult::DuplicateEmail);
        assert!(db.get_user_by_email("retry@example.com").unwrap().is_none());

        let retry = db
            .redeem_invite_and_create_user(
                &th("hash-1"),
                &shared::ids::UserId::new("user-3"),
                "retry@example.com",
                "hashed-pw",
                3000,
            )
            .unwrap();
        assert_eq!(retry, RedeemInviteResult::Created);
        assert!(db.get_user_by_email("retry@example.com").unwrap().is_some());
    }

    /// A session is good for `SESSION_LIFETIME_SECONDS` and no longer; the
    /// rows of expired ones go when a new session is made.
    #[test]
    fn a_session_expires_and_its_row_is_dropped_by_the_next_login() {
        let db = Db::open_in_memory().unwrap();
        db.create_user(
            &shared::ids::UserId::new("user-1"),
            "a@example.com",
            "hash",
            false,
            1000,
        )
        .unwrap();
        db.create_session(&th("old"), &shared::ids::UserId::new("user-1"), 1000)
            .unwrap();
        assert!(db.find_session(&th("old"), 1000).unwrap().is_some());
        assert!(db
            .find_session(&th("old"), 1000 + SESSION_LIFETIME_SECONDS - 1)
            .unwrap()
            .is_some());
        assert!(db
            .find_session(&th("old"), 1000 + SESSION_LIFETIME_SECONDS + 1)
            .unwrap()
            .is_none());
        db.create_session(
            &th("new"),
            &shared::ids::UserId::new("user-1"),
            1000 + SESSION_LIFETIME_SECONDS + 1,
        )
        .unwrap();
        assert!(
            db.find_session(&th("old"), 1000).unwrap().is_none(),
            "the expired row is gone, whatever clock asks"
        );
    }

    /// An invite link is good for `INVITE_LIFETIME_SECONDS` and no longer.
    #[test]
    fn an_invite_link_expires() {
        let db = Db::open_in_memory().unwrap();
        db.create_invite_link("link-1", &th("hash-1"), None, None, 1000)
            .unwrap();
        let late = db
            .redeem_invite_and_create_user(
                &th("hash-1"),
                &shared::ids::UserId::new("user-1"),
                "a@example.com",
                "hash",
                1000 + INVITE_LIFETIME_SECONDS + 1,
            )
            .unwrap();
        assert_eq!(late, RedeemInviteResult::InvalidOrAlreadyUsed);
        let in_time = db
            .redeem_invite_and_create_user(
                &th("hash-1"),
                &shared::ids::UserId::new("user-1"),
                "a@example.com",
                "hash",
                1000 + INVITE_LIFETIME_SECONDS,
            )
            .unwrap();
        assert_eq!(in_time, RedeemInviteResult::Created);
    }

    /// First-run setup makes one admin, once: a second submission (a race,
    /// or a replay) finds setup complete and makes nothing.
    #[test]
    fn first_run_setup_creates_one_admin_and_completes_setup_atomically() {
        let db = Db::open_in_memory().unwrap();
        assert!(db
            .create_admin_and_complete_setup(
                &shared::ids::UserId::new("admin-1"),
                "admin@example.com",
                "hash",
                1000
            )
            .unwrap());
        assert!(db.is_setup_complete().unwrap());
        assert!(!db
            .create_admin_and_complete_setup(
                &shared::ids::UserId::new("admin-2"),
                "other@example.com",
                "hash",
                1001
            )
            .unwrap());
        assert!(db.get_user_by_email("other@example.com").unwrap().is_none());
    }

    #[test]
    fn redeeming_a_request_linked_token_auto_actions_its_request() {
        let db = Db::open_in_memory().unwrap();
        db.create_invite_request("req-1", "a@example.com", "let me in", 1000)
            .unwrap();
        db.create_invite_link("link-1", &th("hash-1"), Some("enc-1"), Some("req-1"), 1000)
            .unwrap();
        assert_eq!(db.count_unactioned_invite_requests().unwrap(), 1);

        db.redeem_invite_and_create_user(
            &th("hash-1"),
            &shared::ids::UserId::new("user-1"),
            "a@example.com",
            "hashed-pw",
            2000,
        )
        .unwrap();

        assert_eq!(
            db.count_unactioned_invite_requests().unwrap(),
            0,
            "a fulfilled request should clear itself off the pending list"
        );
    }

    #[test]
    fn pagination_helpers_respect_limit_offset_and_ordering() {
        let db = Db::open_in_memory().unwrap();
        for i in 0..5 {
            db.create_invite_request(
                &format!("req-{i}"),
                &format!("user{i}@example.com"),
                "m",
                1000 + i,
            )
            .unwrap();
        }
        assert_eq!(db.count_unactioned_invite_requests().unwrap(), 5);

        let page1 = db.list_unactioned_invite_requests(2, 0).unwrap();
        assert_eq!(page1.len(), 2);
        // Newest first.
        assert_eq!(page1[0].email, "user4@example.com");
        assert_eq!(page1[1].email, "user3@example.com");

        let page2 = db.list_unactioned_invite_requests(2, 2).unwrap();
        assert_eq!(page2[0].email, "user2@example.com");
        assert_eq!(page2[1].email, "user1@example.com");

        let page3 = db.list_unactioned_invite_requests(2, 4).unwrap();
        assert_eq!(page3.len(), 1);
        assert_eq!(page3[0].email, "user0@example.com");
    }

    #[test]
    fn get_invite_request_finds_a_row_even_after_it_has_been_actioned() {
        let db = Db::open_in_memory().unwrap();
        db.create_invite_request("req-1", "a@example.com", "m", 1000)
            .unwrap();
        db.delete_invite_request("req-1", 2000).unwrap();

        let row = db
            .get_invite_request("req-1")
            .unwrap()
            .expect("a soft-deleted request must still be individually fetchable");
        assert_eq!(row.email, "a@example.com");
    }
}
