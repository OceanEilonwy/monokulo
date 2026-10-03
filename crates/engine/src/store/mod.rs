//! The persistence layer.
//!
//! A `Store` wraps one `rusqlite::Connection` and is not `Sync` on its own. The
//! engine writes through two of them: the database worker's own connection
//! (`db::Db`, a thread serving queued jobs) and the shared store
//! (`SharedStore`, an `Arc<Mutex<Store>>`) that tests, tools and a few startup
//! paths use; SQLite's busy timeout and `BEGIN IMMEDIATE` transactions
//! (`Store::in_transaction`) keep the two from deadlocking on each other. Reads
//! can also go through [`ReadStorePool`]: separate read-only connections, as
//! many as needed, alongside the writers in WAL mode.
//!
//! This module intentionally has no `KeyCustody` dependency: deriving a subaddress
//! for a new order happens *before* `create_order` is called, by whatever orchestrates
//! order creation (the future HTTP handler / writer actor) — `Store` only persists the
//! result.

use parking_lot::Mutex;
use std::cell::RefCell;
use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension as _};
use uuid::Uuid;

pub use shared::ids::{OrderId, TenantId, WebhookId};

use crate::auth::{generate_public_key, generate_secret_token, RawToken};
use crate::status::{derive_status, OrderStatus, PaymentView, StatusInputs};

mod conflicts;
pub mod db;
pub mod proof;
mod work;
pub use db::{Db, DbMetrics};
pub use work::{
    position, sql_height, BlockCheckpoint, OpenedReorg, Position, ReorgCandidate, ReorgJob,
    ReorgPhase, StagedPayment,
};

/// Every migration file, applied in order, exactly once each - tracked in
/// `schema_migrations` rather than assumed from `CREATE TABLE`'s own failure mode.
/// Re-running the raw DDL against an already-migrated database (e.g. every time the
/// server restarts against its existing `moneropay.db`) would otherwise crash with
/// "table already exists" - a real bug caught by actually restarting the compiled
/// binary against a file it had already created, not by any unit test, since every
/// unit test in this codebase opens a fresh `:memory:` database exactly once.
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../../migrations/0001_init.sql")),
    (
        2,
        include_str!("../../migrations/0002_active_orders_index.sql"),
    ),
    (
        3,
        include_str!("../../migrations/0003_network_scoped_scanning.sql"),
    ),
    (
        4,
        include_str!("../../migrations/0004_order_scoped_payment_uniqueness.sql"),
    ),
    (
        5,
        include_str!("../../migrations/0005_drop_order_fiat_columns.sql"),
    ),
    (
        6,
        include_str!("../../migrations/0006_drop_tenant_template_dir.sql"),
    ),
    (7, include_str!("../../migrations/0007_order_rescans.sql")),
    (
        8,
        include_str!("../../migrations/0008_order_scanned_range.sql"),
    ),
    (
        9,
        include_str!("../../migrations/0009_utc_suffix_date_columns.sql"),
    ),
    (10, include_str!("../../migrations/0010_settings.sql")),
    (
        11,
        include_str!("../../migrations/0011_order_confirmations_override.sql"),
    ),
    (
        12,
        include_str!("../../migrations/0012_drop_order_rescans.sql"),
    ),
    (
        13,
        include_str!("../../migrations/0013_drop_zero_conf_max_piconero.sql"),
    ),
    (
        14,
        include_str!("../../migrations/0014_drop_tenant_allowed_origins.sql"),
    ),
    (
        15,
        include_str!("../../migrations/0015_tenant_scan_cursor.sql"),
    ),
    (
        16,
        include_str!("../../migrations/0016_order_closed_at.sql"),
    ),
    (
        17,
        include_str!("../../migrations/0017_pending_payment_recomputes.sql"),
    ),
    (
        18,
        include_str!("../../migrations/0018_partial_block_scans.sql"),
    ),
    (19, include_str!("../../migrations/0019_scanner_work.sql")),
    (
        20,
        include_str!("../../migrations/0020_scanner_indexes.sql"),
    ),
    (
        21,
        include_str!("../../migrations/0021_payment_output_keys.sql"),
    ),
    (
        22,
        include_str!("../../migrations/0022_webhook_delivery_gave_up.sql"),
    ),
    (
        23,
        include_str!("../../migrations/0023_order_idempotency_key.sql"),
    ),
    (24, include_str!("../../migrations/0024_proof_of_work.sql")),
    (
        25,
        include_str!("../../migrations/0025_superseded_payments.sql"),
    ),
];

/// The engine's writing connections (the shared store and the database
/// worker) take `shared::sqlite`'s writer settings: WAL, `synchronous =
/// NORMAL`, foreign keys, a busy timeout and a statement cache. This isn't a
/// ledger moving funds, it's a record of payments observed on-chain, so
/// NORMAL's tradeoff (only an OS crash or power loss can lose the last few
/// commits) beats paying an fsync per commit.
fn configure_connection(conn: &Connection) -> rusqlite::Result<()> {
    shared::sqlite::configure_writer(conn)
}

/// A column value as plain text, for comparing database states in tests.
#[cfg(test)]
pub(crate) fn value_text(value: rusqlite::types::ValueRef<'_>) -> String {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Null => "null".into(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(r) => r.to_string(),
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
        ValueRef::Blob(b) => hex::encode(b),
    }
}

/// Each migration's DDL and its `schema_migrations` bookkeeping row commit together
/// or not at all. Without that, a crash in the window between the two re-runs the
/// migration on the next boot, which for anything containing `CREATE TABLE` or
/// `DROP TABLE` (0003 and 0004 both do) fails outright and leaves the server unable
/// to start against its own database.
///
/// The actual mechanism (transactional per-migration apply, tracked in
/// `schema_migrations`) lives in `shared::migrations::apply` (WBS 0.5) - moved there
/// so the monokulo database can reuse it without a second, hand-rolled copy.
/// This wrapper just supplies the engine's own migration list, which - being
/// `include_str!("../../migrations/...")` paths relative to this crate - can't live in
/// `shared` itself.
fn apply_migrations(conn: &Connection) -> rusqlite::Result<()> {
    shared::migrations::apply(conn, MIGRATIONS)
}

pub type SharedStore = Arc<Mutex<Store>>;

/// Independent read-only SQLite connections (`shared::sqlite::Pool`).
///
/// WAL lets these readers run concurrently with the writer; each connection
/// stays on its own thread, so a disk stall never blocks a Tokio worker, and
/// a read goes to whichever connection is free.
#[derive(Clone)]
pub struct ReadStorePool(shared::sqlite::Pool<Store>);

impl ReadStorePool {
    pub fn open(path: &str, count: usize) -> Result<Self> {
        let stores = std::iter::repeat_with(|| {
            Ok(Store::from_connection(shared::sqlite::open_reader(path)?))
        })
        .take(count.max(1))
        .collect::<Result<Vec<_>>>()?;
        shared::sqlite::Pool::start("engine-db-read", stores)
            .map(Self)
            .map_err(|e| StoreError::WorkerUnavailable(e.to_string()))
    }

    /// Reads on the caller, on `store`, with writes refused as on a pool
    /// connection: for tests and their in-memory databases.
    #[cfg(any(test, feature = "test-support"))]
    pub fn inline(store: SharedStore) -> Self {
        Self(shared::sqlite::Pool::Inline(store))
    }

    pub async fn query<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        if self.0.is_inline() {
            self.0.run(move |store| store.read_only(|| f(store))).await
        } else {
            self.0.run(f).await
        }
    }
}

/// How the engine's HTTP handlers reach the database: reads on the read
/// pool, writes on the database worker ([`Db`]), and the order-change
/// notifications its writes publish. No handler holds the shared store.
#[derive(Clone)]
pub struct Database {
    writes: Db,
    reads: ReadStorePool,
    changes: tokio::sync::broadcast::Sender<OrderChange>,
    /// The test's shared store, for [`Database::lock`].
    #[cfg(test)]
    inline: Option<SharedStore>,
}

impl Database {
    /// The worker and read pool (both over the database file), sharing
    /// `store`'s order-change notifications - the store the worker was
    /// opened from ([`Db::open`]), so its commits reach subscribers.
    pub fn from_parts(writes: Db, reads: ReadStorePool, store: &Store) -> Self {
        Self {
            writes,
            reads,
            changes: store.order_changes.clone(),
            #[cfg(test)]
            inline: None,
        }
    }

    /// Everything on the caller, on `store`: for tests and their in-memory
    /// databases. Reads still can't write.
    #[cfg(any(test, feature = "test-support"))]
    pub fn inline(store: SharedStore) -> Self {
        let changes = store.lock().order_changes.clone();
        #[cfg(test)]
        let inline = Some(Arc::clone(&store));
        Self {
            writes: Db::over_shared(Arc::clone(&store)),
            reads: ReadStorePool::inline(store),
            changes,
            #[cfg(test)]
            inline,
        }
    }

    /// Runs `f` on a read-only connection.
    pub async fn read<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.reads.query(f).await
    }

    /// Runs a write (or a read that must see this connection's own writes)
    /// on the database worker, in turn with the scanner's and webhooks' work
    /// (the `Admin` class), never on a Tokio worker: a slow disk or a write
    /// lock held by the scanner delays this request, not every task sharing
    /// its worker.
    pub async fn write<T, E>(
        &self,
        f: impl FnOnce(&Store) -> std::result::Result<T, E> + Send + 'static,
    ) -> std::result::Result<T, E>
    where
        T: Send + 'static,
        E: From<StoreError> + Send + 'static,
    {
        self.writes.run(db::Class::Admin, f).await
    }

    /// Every [`OrderChange`] committed from now on.
    pub fn subscribe_order_changes(&self) -> tokio::sync::broadcast::Receiver<OrderChange> {
        self.changes.subscribe()
    }

    /// The test's shared store, for setting up and checking state directly.
    /// Only for an inline database; production code can't call it.
    #[cfg(test)]
    pub fn lock(&self) -> parking_lot::MutexGuard<'_, Store> {
        self.shared_store_for_test().lock()
    }

    /// The test's shared store itself, for code under test that takes one.
    #[cfg(test)]
    pub fn shared_store_for_test(&self) -> &SharedStore {
        self.inline
            .as_ref()
            .expect("only an inline test database has a shared store")
    }
}

pub struct Store {
    conn: Connection,
    /// Fan-out of "this order's visible state just changed" hints - see
    /// [`OrderChange`]. Lives on the `Store` itself, not on the HTTP layer, because
    /// the writes that cause a change happen in the scanner loop as often as in a
    /// handler, and this is the one place both go through.
    order_changes: tokio::sync::broadcast::Sender<OrderChange>,
    /// `Some` only while [`Store::in_transaction`] is running: changes are held
    /// here and published after the commit, so no subscriber can re-read an order
    /// before the write that announced it is visible - and a rolled-back
    /// transaction announces nothing at all.
    pending_order_changes: RefCell<Option<Vec<OrderChange>>>,
}

/// A hint that one order's customer-visible state (status, confirmations, amount
/// received, payments, double-spend flag or refund address) changed.
///
/// Carries no state of its own on purpose: a subscriber re-reads the order, so a
/// spurious hint costs one read and a coalesced burst loses nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderChange {
    pub tenant_id: TenantId,
    pub order_id: OrderId,
}

/// Enough for a burst of changes in one scan tick; a subscriber that falls
/// further behind gets `RecvError::Lagged` and resyncs everything it watches.
const ORDER_CHANGE_CAPACITY: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("not found")]
    NotFound,
    #[error("database worker unavailable: {0}")]
    WorkerUnavailable(String),
}

impl From<shared::sqlite::PoolError> for StoreError {
    fn from(e: shared::sqlite::PoolError) -> Self {
        Self::WorkerUnavailable(e.to_string())
    }
}

type Result<T> = std::result::Result<T, StoreError>;

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Tenant {
    pub id: TenantId,
    pub public_key: String,
    pub key_custody_backend: String,
    pub sealed_key_material: Vec<u8>,
    pub primary_address: String,
    pub network: String,
    pub next_minor_index: u32,
    pub confirmations_required: u64,
    pub order_expiry_seconds: i64,
    pub created_at: i64,
    pub disabled_at: Option<i64>,
    /// Highest block on this tenant's network fully scanned for it
    /// (migration 0015). `None` until the network is first seeded.
    pub scanned_through_height: Option<u64>,
}

pub struct NewTenant {
    pub key_custody_backend: String,
    pub sealed_key_material: Vec<u8>,
    pub primary_address: String,
    pub network: String,
    pub confirmations_required: Option<u64>,
    pub order_expiry_seconds: Option<i64>,
}

/// A partial update to a tenant's config. Fields are plain `Option<T>` for
/// "unchanged vs. set to a value".
#[derive(Default)]
pub struct TenantConfigPatch {
    pub confirmations_required: Option<u64>,
    pub order_expiry_seconds: Option<i64>,
}

#[derive(Debug)]
pub struct CreatedTenant {
    pub tenant: Tenant,
    /// Shown here exactly once - callers must hand this to the operator and never
    /// persist it themselves; only `secret_token_hash` is stored.
    pub secret_token: RawToken,
}

#[derive(Debug, Clone)]
pub struct Order {
    pub id: OrderId,
    pub tenant_id: TenantId,
    pub merchant_order_id: Option<String>,
    pub minor_index: u32,
    pub address: String,
    pub xmr_amount_piconero: u64,
    pub amount_received_piconero: u64,
    pub status: OrderStatus,
    pub confirmations: u64,
    pub double_spend_detected_at: Option<i64>,
    pub refund_address: Option<String>,
    pub description: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub updated_at: i64,
    /// `docs/order_rescan_wbs.md` Phase 5.1 - the actual block-height range this
    /// order has ever been examined across, accumulated (never replaced) by both
    /// ordinary live scanning and any manual rescan. `None` until the order's very
    /// first tick.
    pub first_scanned_height: Option<i64>,
    pub last_scanned_height: Option<i64>,
    /// A per-order confirmation-count override, locked in at creation - see
    /// `recompute_order_status`'s own doc comment on how this interacts with
    /// `tenants.confirmations_required`.
    pub confirmations_required_override: Option<u64>,
    /// When the order first became terminal (migration 0016), kept while it
    /// stays so; `None` while it is open.
    pub closed_at: Option<i64>,
}

impl Order {
    /// Whether the live scanner still examines this order at `now`: open,
    /// or closed within the last `grace_period_seconds`. The same window
    /// `scan_window_orders` selects, on a row already read.
    pub fn in_scan_window(&self, now: i64, grace_period_seconds: i64) -> bool {
        matches!(
            self.status,
            OrderStatus::Pending
                | OrderStatus::Unconfirmed
                | OrderStatus::Confirming
                | OrderStatus::Partial
        ) || self
            .closed_at
            .is_some_and(|closed_at| closed_at >= now - grace_period_seconds)
    }
}

pub struct NewOrder {
    pub tenant_id: TenantId,
    pub merchant_order_id: Option<String>,
    pub minor_index: u32,
    pub address: String,
    pub xmr_amount_piconero: u64,
    pub description: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    /// `None` for every existing caller (mock-woocommerce, e2e tests, any
    /// direct engine API use) - the tenant's own `confirmations_required`
    /// still applies exactly as before. Only monokulo's own confirmation-
    /// thresholds feature ever sets this, having already resolved an
    /// amount-tiered override before calling here.
    pub confirmations_required_override: Option<u64>,
    /// The caller's key for this purchase (migration 0023): a creation
    /// repeating it gets the order already made with it.
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OrderPaymentRow {
    pub id: i64,
    pub order_id: OrderId,
    pub txid: String,
    pub output_index: i64,
    pub amount_piconero: u64,
    pub key_images_json: String,
    pub first_seen_at: i64,
    pub block_height: Option<i64>,
    pub voided_at: Option<i64>,
    /// The output's one-time key (hex); `None` for a row recorded without it.
    pub output_key: Option<String>,
    /// Voided because another payment sharing its output key is the one
    /// credited (`store::conflicts`): that payment's id.
    pub superseded_by: Option<i64>,
}

pub struct StagedMatch<'a> {
    pub network: monero::Network,
    pub tenant_id: &'a TenantId,
    pub order_id: &'a OrderId,
    pub txid: &'a str,
    pub output_index: i64,
    pub amount: u64,
    pub key_images_json: &'a str,
    pub seen_at: i64,
    /// The output's one-time key (hex), for `record_payment_match`.
    pub output_key: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct Webhook {
    pub id: WebhookId,
    pub tenant_id: TenantId,
    pub url: String,
    pub extra_headers: String,
    /// Hidden in `Debug`; `expose` it only to sign a delivery.
    pub signing_secret: live_settings::Secret,
    pub enabled: bool,
    pub created_at: i64,
}

fn status_to_str(s: OrderStatus) -> &'static str {
    s.as_str()
}

/// Paid, overpaid and expired orders are closed: nothing more is expected.
fn is_terminal(s: OrderStatus) -> bool {
    matches!(
        s,
        OrderStatus::Paid | OrderStatus::Overpaid | OrderStatus::Expired
    )
}

/// A status that tells a merchant to ship: the one kind of transition that
/// must not be announced from a chain that may be discarded.
fn is_settlement(s: OrderStatus) -> bool {
    matches!(s, OrderStatus::Paid | OrderStatus::Overpaid)
}

/// Everything a status recompute decides from.
struct StatusFacts<'a> {
    order: &'a Order,
    /// The order's valid payments as of `current_height`.
    views: &'a [PaymentView],
    confirmations_required: u64,
    /// The tenant is behind the network: blocks not yet scanned for it
    /// could hold a payment.
    tenant_lagging: bool,
    /// A reorg is being reconciled on the order's network.
    settlement_frozen: bool,
    /// Payments sharing an output key, none of them yet in a block (a
    /// proven one, under proof-of-work checking): which is credited isn't
    /// known, so the order can't settle (`store::conflicts`).
    conflicted: bool,
    /// While the order's network checks proof of work
    /// (`docs/proof_of_work.md`), the payments as proven: each counted only
    /// if the block it was found in is the proven block at its height, and
    /// only with confirmations up to the proven, recorded tip. An order may
    /// only newly settle on these.
    proven_views: Option<Vec<PaymentView>>,
    current_height: u64,
    now: i64,
}

/// What a status recompute writes.
#[derive(Debug, PartialEq, Eq)]
struct StatusPlan {
    status: OrderStatus,
    confirmations: u64,
    amount_received: u64,
    next_due_at: Option<i64>,
    next_due_height: Option<i64>,
    /// A settlement had to wait: the recompute obligation stays.
    keep_obligation: bool,
}

/// The status rules, with no I/O: the derived status, the two holds on it,
/// and when the order can next change without a payment changing.
fn plan_status(facts: &StatusFacts<'_>) -> StatusPlan {
    let order = facts.order;
    let amount_received = facts
        .views
        .iter()
        .fold(0u64, |sum, v| sum.saturating_add(v.amount_piconero));
    let confirmations = facts
        .views
        .iter()
        .map(|v| v.confirmations)
        .min()
        .unwrap_or(0);
    let derived = derive_status(
        facts.views,
        StatusInputs {
            xmr_amount_piconero: order.xmr_amount_piconero,
            confirmations_required: facts.confirmations_required,
            now: facts.now,
            expires_at: order.expires_at,
        },
    );
    // While the tenant is behind the network, an order mustn't become
    // expired: its payment may be in a block not yet scanned for it, and an
    // `order.expired` webhook can make a shop cancel an order that turns out
    // to be paid. It expires once the tenant has caught up.
    let expiry_held = derived == OrderStatus::Expired
        && order.status != OrderStatus::Expired
        && facts.tenant_lagging;
    // While a reorg on this network is being reconciled, confirmations may be
    // counted on the losing chain: an order can't newly settle until the
    // rewind. Everything else (expiry, confirmation counts, walking a
    // settlement back) still happens, and it shows where the payment stands.
    //
    // Likewise while confirmations above the ceiling are needed: a block
    // whose proof of work wasn't checked may be made up
    // (docs/proof_of_work.md).
    let settles_on_proven_blocks = || match &facts.proven_views {
        None => true,
        Some(views) => {
            let as_of_ceiling = derive_status(
                views,
                StatusInputs {
                    xmr_amount_piconero: order.xmr_amount_piconero,
                    confirmations_required: facts.confirmations_required,
                    now: facts.now,
                    expires_at: order.expires_at,
                },
            );
            is_settlement(as_of_ceiling)
        }
    };
    let settlement_deferred = is_settlement(derived)
        && !is_settlement(order.status)
        && (facts.settlement_frozen || facts.conflicted || !settles_on_proven_blocks());
    let status = if expiry_held {
        order.status
    } else if settlement_deferred {
        if facts.views.iter().all(|v| v.is_zero_conf) {
            OrderStatus::Unconfirmed
        } else {
            OrderStatus::Confirming
        }
    } else {
        derived
    };
    // When the status can next move without a payment changing (a payment
    // change leaves a `pending_payment_recomputes` row instead):
    // - its deadline, while it is still short of the amount;
    // - the next block, while a mined payment is short of the confirmations
    //   required (the count customers see moves every block);
    // - again next round, when a transition was held back (rescheduled at
    //   `now`, so it queues behind anything due earlier);
    // - never, once terminal.
    let (next_due_at, next_due_height) = if settlement_deferred || expiry_held {
        (Some(facts.now), None)
    } else if is_terminal(status) {
        (None, None)
    } else {
        let short_of_amount = amount_received < order.xmr_amount_piconero;
        let confirming = facts
            .views
            .iter()
            .any(|v| !v.is_zero_conf && v.confirmations < facts.confirmations_required);
        (
            short_of_amount.then_some(order.expires_at.saturating_add(1).max(facts.now)),
            confirming
                .then(|| i64::try_from(facts.current_height.saturating_add(1)).unwrap_or(i64::MAX)),
        )
    };
    StatusPlan {
        status,
        confirmations,
        amount_received,
        next_due_at,
        next_due_height,
        keep_obligation: settlement_deferred,
    }
}

/// The open half of the scan window (task 7.3, decision D10: open, or
/// closed no earlier than `now` minus the grace period - [`Order::
/// in_scan_window`] on a row already read), for orders aliased `o` of one
/// tenant (`orders_tenant_status_idx`). The two halves are queried apart,
/// each from its own index: an OR would defeat both.
const OPEN_ORDERS: &str = "o.status IN ('pending', 'unconfirmed', 'confirming', 'partial')";

/// Whether the tenant whose id is the SQL expression `tenant` has an order
/// in its scan window: two `EXISTS`, each answered from an index.
/// Parameters: `:since_minus_grace`.
fn tenant_in_scope(tenant: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM orders o WHERE o.tenant_id = {tenant} AND {OPEN_ORDERS})
          OR EXISTS (SELECT 1 FROM orders o WHERE o.tenant_id = {tenant} AND o.closed_at_utc >= :since_minus_grace))"
    )
}

/// The ids of the scan window's orders for tenants matching the SQL
/// condition `tenants` on `o.tenant_id`, as a `UNION` of its two indexed
/// halves. Parameters: `:since_minus_grace`.
fn scan_window_orders(tenants: &str) -> String {
    format!(
        "SELECT o.id FROM orders o WHERE {tenants} AND {OPEN_ORDERS}
         UNION SELECT o.id FROM orders o WHERE {tenants} AND o.closed_at_utc >= :since_minus_grace"
    )
}

/// An unknown value can only come from a hand-edited or corrupted row (the
/// schema's CHECK constraint rejects it otherwise). It is reported as a row
/// conversion error rather than a panic, so one bad row fails the query that
/// read it, not the whole scan loop.
fn status_from_str(s: &str) -> rusqlite::Result<OrderStatus> {
    s.parse().map_err(|e: shared::order_status::UnknownStatus| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("{e} in database").into(),
        )
    })
}

/// The bytes of an in-memory database with every migration applied, built
/// on first use. Two first uses at once may each build it; one copy is kept.
fn migrated_template() -> Result<&'static [u8]> {
    static TEMPLATE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    if let Some(template) = TEMPLATE.get() {
        return Ok(template);
    }
    let conn = Connection::open_in_memory()?;
    configure_connection(&conn)?;
    apply_migrations(&conn)?;
    let bytes = conn.serialize(rusqlite::MAIN_DB)?.to_vec();
    Ok(TEMPLATE.get_or_init(|| bytes))
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

impl Store {
    fn from_connection(conn: Connection) -> Self {
        let (order_changes, _) = tokio::sync::broadcast::channel(ORDER_CHANGE_CAPACITY);
        Self {
            conn,
            order_changes,
            pending_order_changes: RefCell::new(None),
        }
    }

    /// A fresh in-memory store is a copy of one migrated once per process:
    /// running every migration costs tens of milliseconds, and tests open
    /// thousands of stores. `open_file` still migrates, and the migration
    /// tests run the migrations themselves.
    pub fn open_in_memory() -> Result<Self> {
        let template = migrated_template()?;
        let mut conn = Connection::open_in_memory()?;
        conn.deserialize_read_exact(rusqlite::MAIN_DB, template, template.len(), false)?;
        configure_connection(&conn)?;
        Ok(Self::from_connection(conn))
    }

    /// Another connection to the same database file, sharing this store's
    /// order-change notifications. Migrations have already run.
    pub(crate) fn connect_again(&self, path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        configure_connection(&conn)?;
        Ok(Self {
            conn,
            order_changes: self.order_changes.clone(),
            pending_order_changes: RefCell::new(None),
        })
    }

    pub fn open_file(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        configure_connection(&conn)?;
        apply_migrations(&conn)?;
        Ok(Self::from_connection(conn))
    }

    /// Every [`OrderChange`] committed from now on, across all tenants - the
    /// subscriber filters by tenant itself.
    pub fn subscribe_order_changes(&self) -> tokio::sync::broadcast::Receiver<OrderChange> {
        self.order_changes.subscribe()
    }

    fn publish_order_change(&self, tenant_id: &TenantId, order_id: &OrderId) {
        if self.order_changes.receiver_count() == 0 {
            return;
        }
        let change = OrderChange {
            tenant_id: tenant_id.clone(),
            order_id: order_id.clone(),
        };
        match self.pending_order_changes.borrow_mut().as_mut() {
            Some(pending) => {
                if !pending.contains(&change) {
                    pending.push(change);
                }
            }
            None => {
                let _ = self.order_changes.send(change);
            }
        }
    }

    /// [`Self::publish_order_change`] for a caller holding only the order id.
    fn publish_order_change_by_id(&self, order_id: &OrderId) -> Result<()> {
        if self.order_changes.receiver_count() == 0 {
            return Ok(());
        }
        if let Some(tenant_id) = self.get_order_tenant_id(order_id)? {
            self.publish_order_change(&tenant_id, order_id);
        }
        Ok(())
    }

    /// Runs `f` with this connection refusing writes, as a read pool's
    /// connections do (`shared::sqlite::read_only`).
    pub fn read_only<T>(&self, f: impl FnOnce() -> T) -> T {
        shared::sqlite::read_only(&self.conn, f)
    }

    pub fn into_shared(self) -> SharedStore {
        Arc::new(Mutex::new(self))
    }

    /// Runs `f` inside one SQLite transaction against this same connection, so a
    /// group of writes that only makes sense together either all land or none do.
    ///
    /// Exists because "persist a status change" and "enqueue the webhook announcing
    /// it" are two separate statements that must not be able to come apart: the new
    /// status commits first, and `recompute_order_status` derives "did anything
    /// change" by comparing against the *stored* status, so once the status write is
    /// visible the transition can never be re-detected. A failure (or a crash)
    /// between the two therefore didn't delay the webhook, it deleted it - the
    /// merchant's `order.paid` never fires, for an order that is `paid`.
    ///
    /// Nested calls are not supported (SQLite has no nested `BEGIN`); every caller
    /// is a top-level orchestration step. Rolls back on any error, including one
    /// raised by `f` itself, because the `Transaction` guard's default drop behaviour
    /// is rollback.
    pub fn in_transaction<T, E, F>(&self, f: F) -> std::result::Result<T, E>
    where
        F: FnOnce(&Self) -> std::result::Result<T, E>,
        E: From<StoreError>,
    {
        // Cleared again even if `f` panics, so a panic can't leave later
        // changes made outside any transaction stuck in the buffer.
        struct ResetOnDrop<'a>(&'a RefCell<Option<Vec<OrderChange>>>);
        impl Drop for ResetOnDrop<'_> {
            fn drop(&mut self) {
                self.0.borrow_mut().take();
            }
        }
        *self.pending_order_changes.borrow_mut() = Some(Vec::new());
        let reset = ResetOnDrop(&self.pending_order_changes);
        let result = self.run_transaction(f);
        let pending = self
            .pending_order_changes
            .borrow_mut()
            .take()
            .unwrap_or_default();
        drop(reset);
        if result.is_ok() {
            for change in pending {
                let _ = self.order_changes.send(change);
            }
        }
        result
    }

    fn run_transaction<T, E, F>(&self, f: F) -> std::result::Result<T, E>
    where
        F: FnOnce(&Self) -> std::result::Result<T, E>,
        E: From<StoreError>,
    {
        // IMMEDIATE takes SQLite's write lock at BEGIN. A deferred
        // transaction that reads first and writes later fails that write with
        // SQLITE_BUSY_SNAPSHOT (which no busy timeout retries) whenever another
        // connection committed in between; this waits for the lock up front
        // instead, so the decision reads and the writes see one snapshot.
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(StoreError::from)?;
        let out = f(self)?;
        tx.commit().map_err(StoreError::from)?;
        Ok(out)
    }

    /// Test-only escape hatch for breaking the schema on purpose, so failure paths
    /// that only a genuine `StoreError` can reach (a scanner tick that must not mark
    /// a block scanned when recording a match failed) can be exercised without a
    /// mock persistence layer. Not compiled into the binary.
    #[cfg(test)]
    pub fn execute_raw_for_test(&self, sql: &str) -> Result<()> {
        self.conn.execute_batch(sql)?;
        Ok(())
    }

    /// Fault injection: the `n`th statement-level access check from now (0-based,
    /// counted over every statement prepared) is denied, once, so that
    /// statement fails with an authorization error. `None` disarms. While
    /// armed the statement cache is off, so every statement is prepared, and
    /// checked, each time it runs. Returns the running count of checks, so a
    /// sweep knows when `n` was past the last one.
    #[cfg(test)]
    pub(crate) fn fail_nth_access(&self, n: Option<usize>) -> Arc<std::sync::atomic::AtomicUsize> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let seen = Arc::new(AtomicUsize::new(0));
        if let Some(n) = n {
            self.conn.set_prepared_statement_cache_capacity(0);
            self.conn.flush_prepared_statement_cache();
            let counter = Arc::clone(&seen);
            self.conn
                .authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                    use rusqlite::hooks::AuthAction::*;
                    // Counted once per statement (its kind), not per
                    // column it reads, and not inside triggers: failing
                    // a later check of the same statement takes the
                    // same path.
                    let statement = context.accessor.is_none()
                        && matches!(
                            context.action,
                            Select
                                | Insert { table_name: _ }
                                | Update {
                                    table_name: _,
                                    column_name: _
                                }
                                | Delete { table_name: _ }
                                | Transaction { operation: _ }
                                | Savepoint {
                                    operation: _,
                                    savepoint_name: _
                                }
                                | Pragma {
                                    pragma_name: _,
                                    pragma_value: _
                                }
                        );
                    if statement && counter.fetch_add(1, Ordering::Relaxed) == n {
                        rusqlite::hooks::Authorization::Deny
                    } else {
                        rusqlite::hooks::Authorization::Allow
                    }
                }))
                .unwrap();
        } else {
            self.conn
                .authorizer(
                    None::<fn(rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization>,
                )
                .unwrap();
            self.conn
                .set_prepared_statement_cache_capacity(shared::sqlite::STATEMENT_CACHE);
        }
        seen
    }

    /// The connection itself, for tests that query it directly.
    #[cfg(test)]
    pub(crate) fn conn_for_test(&self) -> &Connection {
        &self.conn
    }

    /// Every row of every table, as text, for comparing whole-database
    /// states in tests.
    #[cfg(test)]
    pub(crate) fn dump_for_test(&self) -> String {
        use std::fmt::Write as _;
        let tables: Vec<String> = self
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let mut out = String::new();
        for table in tables {
            let mut stmt = self
                .conn
                .prepare(&format!("SELECT * FROM \"{table}\""))
                .unwrap();
            let columns = stmt.column_count();
            let mut rows: Vec<String> = stmt
                .query_map([], |row| {
                    Ok((0..columns)
                        .map(|i| value_text(row.get_ref(i).unwrap()))
                        .collect::<Vec<_>>()
                        .join("|"))
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            rows.sort();
            let _ = writeln!(out, "[{table}]");
            for row in rows {
                let _ = writeln!(out, "{row}");
            }
        }
        out
    }

    // -- Tenants --------------------------------------------------------

    pub fn create_tenant(&self, new: &NewTenant, now: i64) -> Result<CreatedTenant> {
        let id = TenantId::new(new_id("tn"));
        let public_key = generate_public_key();
        let secret_token = generate_secret_token();
        let secret_hash = secret_token.hash();

        self.conn.execute(
            "INSERT INTO tenants (id, public_key, secret_token_hash, key_custody_backend,
                sealed_key_material, primary_address, network, next_minor_index,
                confirmations_required, order_expiry_seconds, created_at_utc, scanned_through_height)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?9, ?10,
                (SELECT MAX(height) FROM scanned_blocks WHERE network = ?7))",
            params![
                id,
                public_key,
                secret_hash,
                new.key_custody_backend,
                new.sealed_key_material,
                new.primary_address,
                new.network,
                new.confirmations_required.unwrap_or(10) as i64,
                new.order_expiry_seconds.unwrap_or(1800),
                now,
            ],
        )?;

        let tenant = self.get_tenant_by_id(&id)?.ok_or(StoreError::NotFound)?;
        Ok(CreatedTenant {
            tenant,
            secret_token,
        })
    }

    fn row_to_tenant(row: &rusqlite::Row<'_>) -> rusqlite::Result<Tenant> {
        Ok(Tenant {
            id: row.get("id")?,
            public_key: row.get("public_key")?,
            key_custody_backend: row.get("key_custody_backend")?,
            sealed_key_material: row.get("sealed_key_material")?,
            primary_address: row.get("primary_address")?,
            network: row.get("network")?,
            next_minor_index: row
                .get::<_, shared::sqlite::Unsigned<u32>>("next_minor_index")?
                .0,
            confirmations_required: row
                .get::<_, shared::sqlite::Unsigned<u64>>("confirmations_required")?
                .0,
            order_expiry_seconds: row.get("order_expiry_seconds")?,
            created_at: row.get("created_at_utc")?,
            disabled_at: row.get("disabled_at_utc")?,
            scanned_through_height: row
                .get::<_, Option<i64>>("scanned_through_height")?
                .map(|h| h as u64),
        })
    }

    /// Public keys of the enabled tenants on `network`, for `/status`'s list
    /// of stores that can't be scanned (task 3.7).
    pub fn tenant_public_keys_on_network(&self, network: monero::Network) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached("SELECT public_key FROM tenants WHERE network = ?1 AND disabled_at_utc IS NULL ORDER BY public_key")?;
        let rows = stmt
            .query_map(params![shared::network::SqlNetwork(network)], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Ids of the enabled tenants on `network`.
    pub fn tenant_ids_on_network(&self, network: monero::Network) -> Result<Vec<TenantId>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM tenants WHERE network = ?1 AND disabled_at_utc IS NULL ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![shared::network::SqlNetwork(network)], |row| {
                row.get::<_, TenantId>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every active tenant's (public key, network, key custody backend).
    pub fn tenant_custody_backends(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT public_key, network, key_custody_backend FROM tenants WHERE disabled_at_utc IS NULL ORDER BY public_key",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// `lagging_tenants`, by public key: (public key, cursor).
    pub fn lagging_tenant_keys(&self, network: monero::Network) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT public_key, scanned_through_height FROM tenants
             WHERE network = ?1 AND disabled_at_utc IS NULL
               AND scanned_through_height < (SELECT MAX(height) FROM scanned_blocks WHERE network = ?1)
             ORDER BY public_key",
        )?;
        let rows = stmt
            .query_map(params![shared::network::SqlNetwork(network)], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, shared::sqlite::Unsigned<u64>>(1)?.0,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Moves one tenant to another key custody backend with its newly sealed
    /// keys, in one statement (task 5.3).
    pub fn update_tenant_key_custody(
        &self,
        tenant_id: &TenantId,
        backend: &str,
        sealed: &[u8],
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE tenants SET key_custody_backend = ?2, sealed_key_material = ?3 WHERE id = ?1 AND disabled_at_utc IS NULL",
            params![tenant_id, backend, sealed],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    /// How many enabled tenants each network has, for the admin page (tasks
    /// 2.2 and 4.4).
    pub fn count_tenants_by_network(&self) -> Result<std::collections::BTreeMap<String, u64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT network, COUNT(*) FROM tenants WHERE disabled_at_utc IS NULL GROUP BY network",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, shared::sqlite::Unsigned<u64>>(1)?.0,
                ))
            })?
            .collect::<rusqlite::Result<std::collections::BTreeMap<_, _>>>()?;
        Ok(rows)
    }

    /// Every non-disabled tenant - used at boot to eagerly register every wallet
    /// with `KeyCustody` before serving any requests, so the lazy-on-first-use path
    /// in the HTTP layer (`http::resolve_wallet_handle`) is a fallback, not the
    /// only path.
    pub fn list_active_tenants(&self) -> Result<Vec<Tenant>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT * FROM tenants WHERE disabled_at_utc IS NULL")?;
        let rows = stmt
            .query_map([], Self::row_to_tenant)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn count_tenants(&self) -> Result<u64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tenants", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    pub fn get_tenant_by_id(&self, id: &TenantId) -> Result<Option<Tenant>> {
        self.conn
            .query_row(
                "SELECT * FROM tenants WHERE id = ?1",
                params![id],
                Self::row_to_tenant,
            )
            .optional()
            .map_err(Into::into)
    }

    /// The *only* sanctioned way to resolve a tenant for an admin request: entirely
    /// from the presented secret token, never from any path parameter. See
    /// `docs/DESIGN.md` §10.1 for why this is structural, not a per-handler check.
    pub fn find_tenant_by_secret_token(&self, raw_token: &RawToken) -> Result<Option<Tenant>> {
        let hash = raw_token.hash();
        self.conn
            .query_row(
                "SELECT * FROM tenants WHERE secret_token_hash = ?1 AND disabled_at_utc IS NULL",
                params![hash],
                Self::row_to_tenant,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn rotate_tenant_secret(&self, tenant_id: &TenantId) -> Result<RawToken> {
        let new_token = generate_secret_token();
        let new_hash = new_token.hash();
        let changed = self.conn.execute(
            "UPDATE tenants SET secret_token_hash = ?2 WHERE id = ?1",
            params![tenant_id, new_hash],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(new_token)
    }

    /// Updates only the fields the admin API allows a tenant to change about
    /// itself. `public_key`, key material, and `network` are deliberately absent -
    /// see `docs/DESIGN.md` §10.2 (rotate by creating a new tenant, not by mutating
    /// key material in place). Each field is `Option<Option<T>>`-free by design:
    /// `None` means "leave unchanged", so a partial PATCH body only touches the
    /// fields it actually included.
    /// One statement, so a patch with both fields applies both or neither.
    /// Returns `false` if there is no such tenant.
    pub fn update_tenant_config(
        &self,
        tenant_id: &TenantId,
        patch: &TenantConfigPatch,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE tenants
             SET confirmations_required = COALESCE(?2, confirmations_required),
                 order_expiry_seconds = COALESCE(?3, order_expiry_seconds)
             WHERE id = ?1",
            params![
                tenant_id,
                patch.confirmations_required.map(shared::sqlite::Unsigned),
                patch.order_expiry_seconds
            ],
        )?;
        Ok(changed > 0)
    }

    /// Paginated, newest first, optionally filtered by status. `cursor` is the
    /// `(created_at, id)` of the last row of the previous page, exclusive: a
    /// keyset, because `created_at` is whole seconds and a burst of orders
    /// (a point of sale) shares one. Without the id, a page boundary inside
    /// such a second would skip the rest of it.
    pub fn list_orders(
        &self,
        tenant_id: &TenantId,
        status_filter: Option<OrderStatus>,
        limit: u32,
        cursor: Option<(i64, &str)>,
    ) -> Result<Vec<Order>> {
        let status_str = status_filter.map(status_to_str);
        let (cursor_at, cursor_id) = match cursor {
            Some((at, id)) => (Some(at), Some(id)),
            None => (None, None),
        };
        let mut stmt = self.conn.prepare_cached(
            "SELECT * FROM orders
             WHERE tenant_id = ?1
               AND (?2 IS NULL OR status = ?2)
               AND (?3 IS NULL OR created_at_utc < ?3 OR (created_at_utc = ?3 AND id < ?4))
             ORDER BY created_at_utc DESC, id DESC
             LIMIT ?5",
        )?;
        let rows = stmt
            .query_map(
                params![tenant_id, status_str, cursor_at, cursor_id, limit],
                Self::row_to_order,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// A page of `tenant_id`'s orders, newest first: only those still open
    /// (pending, unconfirmed, confirming or partial) when `open_only`, only
    /// those whose id or merchant order id contains `search` (ignoring case)
    /// when given, skipping the first `offset`.
    pub fn list_orders_page(
        &self,
        tenant_id: &TenantId,
        open_only: bool,
        search: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Order>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT * FROM orders
             WHERE tenant_id = ?1
               AND (?2 = 0 OR status IN (?3, ?4, ?5, ?6))
               AND (?7 IS NULL OR instr(lower(id), lower(?7)) > 0 OR instr(lower(coalesce(merchant_order_id, '')), lower(?7)) > 0)
             ORDER BY created_at_utc DESC, id DESC
             LIMIT ?8 OFFSET ?9",
        )?;
        let rows = stmt
            .query_map(
                params![
                    tenant_id,
                    open_only,
                    status_to_str(OrderStatus::Pending),
                    status_to_str(OrderStatus::Unconfirmed),
                    status_to_str(OrderStatus::Confirming),
                    status_to_str(OrderStatus::Partial),
                    search,
                    limit,
                    offset,
                ],
                Self::row_to_order,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Payment changes whose status/webhook transaction has not committed yet
    /// (unlike the live scan window this includes old, closed orders): a
    /// bounded, stable page for background status work. Keyset pagination
    /// avoids an OFFSET walk over a large backlog on every tick.
    pub fn pending_payment_recomputes_page(
        &self,
        network: monero::Network,
        after: &str,
        limit: usize,
    ) -> Result<Vec<OrderId>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT p.order_id FROM pending_payment_recomputes p
             JOIN orders o ON o.id = p.order_id JOIN tenants t ON t.id = o.tenant_id
             WHERE t.network = ?1 AND p.order_id > ?2 ORDER BY p.order_id LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(
                params![shared::network::SqlNetwork(network), after, limit as i64],
                |row| row.get(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Call inside the same transaction as the status update and webhook enqueue.
    pub fn clear_pending_payment_recompute(&self, order_id: &OrderId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM pending_payment_recomputes WHERE order_id = ?1",
            [order_id],
        )?;
        Ok(())
    }

    pub fn disable_tenant(&self, tenant_id: &TenantId, now: i64) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE tenants SET disabled_at_utc = ?2 WHERE id = ?1",
            params![tenant_id, now],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    /// Atomically claims the next unused minor index for a tenant and advances the
    /// counter, in one statement - this is what makes the "two orders never share a
    /// subaddress" property hold under concurrent order creation (backstopped by the
    /// `UNIQUE(tenant_id, minor_index)` constraint regardless).
    ///
    /// Order creation deliberately does *not* use this: an index claimed here and
    /// turned into an order row later leaves a window where the scanner considers an
    /// address scannable that no order can be attributed to. See
    /// `create_order_claiming_minor_index`, which advances the counter and inserts
    /// the order together.
    pub fn allocate_minor_index(&self, tenant_id: &TenantId) -> Result<u32> {
        let allocated: i64 = self.conn.query_row(
            "UPDATE tenants SET next_minor_index = next_minor_index + 1
             WHERE id = ?1
             RETURNING next_minor_index - 1",
            params![tenant_id],
            |row| row.get(0),
        )?;
        Ok(allocated as u32)
    }

    /// The index `allocate_minor_index` would hand out next, without claiming it.
    /// Only useful in combination with `create_order_claiming_minor_index` - see
    /// that method for why order creation can't simply allocate first.
    pub fn peek_next_minor_index(&self, tenant_id: &TenantId) -> Result<u32> {
        let next: i64 = self.conn.query_row(
            "SELECT next_minor_index FROM tenants WHERE id = ?1",
            params![tenant_id],
            |row| row.get(0),
        )?;
        Ok(next as u32)
    }

    /// Claims `expected_index` for `tenant_id` and inserts `new` as one atomic unit,
    /// returning `Ok(None)` (having changed nothing) if the tenant's counter has
    /// moved on since `peek_next_minor_index` reported that index.
    ///
    /// The atomicity is the point. `next_minor_index` is what the scanner reads to
    /// decide *which subaddresses it scans* (`0..next_minor_index`), so the moment it
    /// is advanced, the corresponding subaddress becomes scannable. If the order row
    /// doesn't exist yet at that moment - as it didn't while order creation allocated
    /// an index, released the store lock to `await` a subaddress derivation, and only
    /// then inserted the order - a scanner tick landing in that window matches an
    /// output against an index that `find_order_by_minor_index` can't resolve, and
    /// drops the match. For a mempool sighting the next tick re-finds it; for a
    /// sighting inside a mined block it is lost for good, because blocks are scanned
    /// exactly once. Advancing the counter and creating the order together closes
    /// that window entirely.
    ///
    /// Callers derive the subaddress for the *peeked* index before calling this and
    /// retry on `Ok(None)`; a losing racer costs one extra derivation and burns no
    /// index (the conditional UPDATE leaves the counter untouched when it doesn't
    /// match), so two concurrent creations can never end up sharing an address.
    pub fn create_order_claiming_minor_index(
        &self,
        expected_index: u32,
        new: &NewOrder,
    ) -> Result<Option<Order>> {
        // IMMEDIATE, like every other write transaction: the write lock from
        // the start, so its reads and writes see one snapshot.
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        // A key already used by this store: the order it made, and nothing
        // claimed or written. Checked inside the write transaction, so two
        // requests with one key can't both create.
        if let Some(key) = new.idempotency_key.as_deref() {
            let existing: Option<OrderId> = tx
                .query_row(
                    "SELECT id FROM orders WHERE tenant_id = ?1 AND idempotency_key = ?2",
                    params![new.tenant_id, key],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(id) = existing {
                drop(tx);
                return Ok(Some(
                    self.get_order_by_id(&id)?.ok_or(StoreError::NotFound)?,
                ));
            }
        }
        let claimed = tx.execute(
            "UPDATE tenants SET next_minor_index = next_minor_index + 1
             WHERE id = ?1 AND next_minor_index = ?2",
            params![new.tenant_id, expected_index],
        )?;
        if claimed == 0 {
            return Ok(None);
        }
        let id = OrderId::new(new_id("order"));
        Self::insert_order(&tx, &id, new)?;
        tx.commit()?;
        Ok(Some(
            self.get_order_by_id(&id)?.ok_or(StoreError::NotFound)?,
        ))
    }

    // -- Orders -----------------------------------------------------------

    /// Free function over a bare `&Connection` so both `create_order` and
    /// `create_order_claiming_minor_index` (which runs inside a `Transaction`) can
    /// share one copy of the INSERT.
    fn insert_order(conn: &Connection, id: &OrderId, new: &NewOrder) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT INTO orders (id, tenant_id, merchant_order_id, minor_index, address,
                xmr_amount_piconero, description, created_at_utc, expires_at_utc, updated_at_utc,
                confirmations_required_override, next_due_at_utc, idempotency_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?8, ?10, ?9, ?11)",
            params![
                id,
                new.tenant_id,
                new.merchant_order_id,
                new.minor_index,
                new.address,
                shared::sqlite::Unsigned(new.xmr_amount_piconero),
                new.description,
                new.created_at,
                new.expires_at,
                new.confirmations_required_override
                    .map(shared::sqlite::Unsigned),
                new.idempotency_key,
            ],
        )?;
        Ok(())
    }

    pub fn create_order(&self, new: &NewOrder) -> Result<Order> {
        let id = OrderId::new(new_id("order"));
        Self::insert_order(&self.conn, &id, new)?;
        self.get_order_by_id(&id)?.ok_or(StoreError::NotFound)
    }

    fn row_to_order(row: &rusqlite::Row<'_>) -> rusqlite::Result<Order> {
        let status_str: String = row.get("status")?;
        Ok(Order {
            id: row.get("id")?,
            tenant_id: row.get("tenant_id")?,
            merchant_order_id: row.get("merchant_order_id")?,
            minor_index: row
                .get::<_, shared::sqlite::Unsigned<u32>>("minor_index")?
                .0,
            address: row.get("address")?,
            xmr_amount_piconero: row
                .get::<_, shared::sqlite::Unsigned<u64>>("xmr_amount_piconero")?
                .0,
            amount_received_piconero: row
                .get::<_, shared::sqlite::Unsigned<u64>>("amount_received_piconero")?
                .0,
            status: status_from_str(&status_str)?,
            confirmations: row
                .get::<_, shared::sqlite::Unsigned<u64>>("confirmations")?
                .0,
            double_spend_detected_at: row.get("double_spend_detected_at_utc")?,
            refund_address: row.get("refund_address")?,
            description: row.get("description")?,
            created_at: row.get("created_at_utc")?,
            expires_at: row.get("expires_at_utc")?,
            updated_at: row.get("updated_at_utc")?,
            first_scanned_height: row.get("first_scanned_height")?,
            last_scanned_height: row.get("last_scanned_height")?,
            confirmations_required_override: row
                .get::<_, Option<shared::sqlite::Unsigned<u64>>>("confirmations_required_override")?
                .map(|v| v.0),
            closed_at: row.get("closed_at_utc")?,
        })
    }

    fn get_order_by_id(&self, id: &OrderId) -> Result<Option<Order>> {
        self.conn
            .query_row(
                "SELECT * FROM orders WHERE id = ?1",
                params![id],
                Self::row_to_order,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Routes a scanner match (which only knows a subaddress minor index) back to
    /// the order that index was issued for.
    pub fn find_order_by_minor_index(
        &self,
        tenant_id: &TenantId,
        minor_index: u32,
    ) -> Result<Option<Order>> {
        self.conn
            .query_row(
                "SELECT * FROM orders WHERE tenant_id = ?1 AND minor_index = ?2",
                params![tenant_id, minor_index],
                Self::row_to_order,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Scoped by `tenant_id` in the query itself - the IDOR-prevention rule from
    /// `docs/DESIGN.md` §10.1 applied at the row level. A `order_id` belonging to a
    /// different tenant must come back as `Ok(None)`, indistinguishable from a
    /// nonexistent one.
    pub fn get_order(&self, tenant_id: &TenantId, order_id: &OrderId) -> Result<Option<Order>> {
        self.conn
            .query_row(
                "SELECT * FROM orders WHERE id = ?1 AND tenant_id = ?2",
                params![order_id, tenant_id],
                Self::row_to_order,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Scoped by `tenant_id`, same IDOR-prevention rule as `get_order`. Records
    /// only - nothing in this system ever sends to a refund address (§DESIGN.md 3).
    pub fn set_refund_address(
        &self,
        tenant_id: &TenantId,
        order_id: &OrderId,
        refund_address: &str,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE orders SET refund_address = ?3 WHERE id = ?1 AND tenant_id = ?2",
            params![order_id, tenant_id, refund_address],
        )?;
        if changed > 0 {
            self.publish_order_change(tenant_id, order_id);
        }
        Ok(changed > 0)
    }

    // -- Payments / reorg bookkeeping --------------------------------------

    /// Idempotent: relies on `UNIQUE(order_id, txid, output_index)`. Returns `true`
    /// if this call actually inserted a new row (as opposed to the mempool-polling
    /// scanner re-reporting a transaction it already recorded).
    ///
    /// The conflict clause is `DO UPDATE`, not `DO NOTHING`, specifically so a
    /// mempool-first payment can still learn its block height. The scanner polls the
    /// mempool roughly every second, so in practice *every* real payment is recorded
    /// with `block_height = NULL` before it is ever seen in a block; `DO NOTHING`
    /// then silently discarded the `Some(height)` the block scan supplied moments
    /// later, permanently stranding the payment at zero confirmations and invisible
    /// to reorg reconciliation's collection by height (`collect_reorg_candidates`).
    /// `COALESCE` makes the update one-way - a later mempool re-sighting of an
    /// already-mined transaction can never null out a height that is already known -
    /// and the `WHERE` guard stops a duplicate insert from resurrecting a payment
    /// that reorg reconciliation has already voided.
    ///
    /// `output_key` is the output's one-time key (hex). Only one output is
    /// ever credited per key (the "burning bug": the sender reused a
    /// transaction key, and only one output with it can be spent), but every
    /// one is recorded: which is credited isn't decided by which a node
    /// showed first, which a lying node chooses, but at recompute
    /// (`store::conflicts`). A payment voided for another with its key is
    /// still updated here, so it can be credited if that one loses its
    /// block.
    #[expect(
        clippy::too_many_arguments,
        reason = "one column each of the payment row"
    )]
    pub fn record_payment_match(
        &self,
        order_id: &OrderId,
        txid: &str,
        output_index: i64,
        amount_piconero: u64,
        key_images_json: &str,
        first_seen_at: i64,
        block_height: Option<i64>,
        output_key: Option<&str>,
    ) -> Result<bool> {
        // Whether this is a genuinely new row has to be established before the
        // upsert: with `DO UPDATE`, `execute`'s changed-row count is 1 for both
        // paths and can't distinguish them. Two connections write (the database
        // worker and, for tests and tools, the shared store), so a concurrent
        // insert of the same row between this read and the upsert is possible;
        // the upsert stays correct either way (the unique key makes it one row),
        // and only the "is this new" answer, which decides a change
        // notification, could be off.
        let existing_height: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT block_height FROM order_payments WHERE order_id = ?1 AND txid = ?2 AND output_index = ?3",
                params![order_id, txid, output_index],
                |row| row.get(0),
            )
            .optional()?;
        let already_present = existing_height.is_some();
        self.conn.execute(
            "INSERT INTO order_payments (order_id, txid, output_index, amount_piconero,
                key_images_json, first_seen_at_utc, block_height, output_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(order_id, txid, output_index) DO UPDATE SET
                 block_hash = CASE
                     WHEN excluded.block_height IS NOT NULL
                          AND excluded.block_height IS NOT order_payments.block_height
                     THEN NULL ELSE order_payments.block_hash END,
                 block_height = COALESCE(excluded.block_height, order_payments.block_height),
                 output_key = COALESCE(order_payments.output_key, excluded.output_key)
             WHERE order_payments.voided_at_utc IS NULL OR order_payments.superseded_by IS NOT NULL",
            params![
                order_id,
                txid,
                output_index,
                shared::sqlite::Unsigned(amount_piconero),
                key_images_json,
                first_seen_at,
                block_height,
                output_key
            ],
        )?;
        // The mempool poll re-reports every unconfirmed payment about once a
        // second; only a new row or a newly learned height is a real change.
        if !already_present || (existing_height == Some(None) && block_height.is_some()) {
            self.publish_order_change_by_id(order_id)?;
        }
        Ok(!already_present)
    }

    /// Used when a reorg moves a previously-confirmed payment to a different height,
    /// or drops it back into the mempool (`new_height = None`). Addressed by
    /// `(order_id, txid, output_index)` - the same key the uniqueness constraint
    /// uses - so that when two orders legitimately share one output (see migration
    /// 0004) reconciling one of them never rewrites the other's row.
    pub fn update_payment_block_height(
        &self,
        order_id: &OrderId,
        txid: &str,
        output_index: i64,
        new_height: Option<i64>,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE order_payments
             SET block_hash = CASE WHEN block_height IS ?4 THEN block_hash ELSE NULL END,
                 block_height = ?4
             WHERE order_id = ?1 AND txid = ?2 AND output_index = ?3
               AND (voided_at_utc IS NULL OR superseded_by IS NOT NULL)",
            params![order_id, txid, output_index, new_height],
        )?;
        if changed > 0 {
            self.publish_order_change_by_id(order_id)?;
        }
        Ok(())
    }

    /// Marks a payment permanently reversed. Never called on ambiguous evidence -
    /// only once `is_key_image_spent` affirmatively proves a different, unrelated
    /// transaction consumed the same inputs (see `docs/DESIGN.md` §7.5). Returns
    /// `false` if the row didn't exist or was already voided (idempotent).
    pub fn void_payment(
        &self,
        order_id: &OrderId,
        txid: &str,
        output_index: i64,
        voided_at: i64,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE order_payments SET voided_at_utc = ?4
             WHERE order_id = ?1 AND txid = ?2 AND output_index = ?3 AND voided_at_utc IS NULL",
            params![order_id, txid, output_index, voided_at],
        )?;
        if changed > 0 {
            self.publish_order_change_by_id(order_id)?;
        }
        Ok(changed > 0)
    }

    /// Clears `voided_at` on a payment that reorg reconciliation had previously
    /// written off, because the transaction it records has since returned to the
    /// canonical chain (its replacement was itself reorged out). Deliberately does
    /// *not* touch `orders.double_spend_detected_at`, which the schema defines as
    /// sticky: "a double-spend was once observed on this order" stays true forever,
    /// independently of whether the payment ultimately stood. Returns `false` if the
    /// row didn't exist or wasn't voided (idempotent).
    pub fn unvoid_payment(
        &self,
        order_id: &OrderId,
        txid: &str,
        output_index: i64,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE order_payments SET voided_at_utc = NULL
             WHERE order_id = ?1 AND txid = ?2 AND output_index = ?3 AND voided_at_utc IS NOT NULL",
            params![order_id, txid, output_index],
        )?;
        if changed > 0 {
            self.publish_order_change_by_id(order_id)?;
        }
        Ok(changed > 0)
    }

    pub fn get_valid_payments(&self, order_id: &OrderId) -> Result<Vec<OrderPaymentRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT * FROM order_payments WHERE order_id = ?1 AND voided_at_utc IS NULL",
        )?;
        let rows = stmt
            .query_map(params![order_id], Self::row_to_payment)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every payment (voided or not) for an order - the audit trail a merchant can
    /// inspect for "why does this show partial" or "when was this double-spent".
    pub fn get_all_payments(&self, order_id: &OrderId) -> Result<Vec<OrderPaymentRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM order_payments WHERE order_id = ?1 ORDER BY first_seen_at_utc",
        )?;
        let rows = stmt
            .query_map(params![order_id], Self::row_to_payment)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn row_to_payment(row: &rusqlite::Row<'_>) -> rusqlite::Result<OrderPaymentRow> {
        Ok(OrderPaymentRow {
            id: row.get("id")?,
            order_id: row.get("order_id")?,
            txid: row.get("txid")?,
            output_index: row.get("output_index")?,
            amount_piconero: row
                .get::<_, shared::sqlite::Unsigned<u64>>("amount_piconero")?
                .0,
            key_images_json: row.get("key_images_json")?,
            first_seen_at: row.get("first_seen_at_utc")?,
            block_height: row.get("block_height")?,
            voided_at: row.get("voided_at_utc")?,
            output_key: row.get("output_key")?,
            superseded_by: row.get("superseded_by")?,
        })
    }

    /// A bounded page for the routine vanished-mempool sweep. The rowid is a
    /// stable keyset cursor for the life of a payment row; callers wrap to zero
    /// at the end so transactions still absent from the pool are revisited.
    pub fn unconfirmed_payments_page(
        &self,
        network: monero::Network,
        after_rowid: i64,
        limit: usize,
    ) -> Result<Vec<(i64, OrderPaymentRow)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT op.rowid, op.* FROM order_payments op
             JOIN orders o ON o.id = op.order_id
             JOIN tenants t ON t.id = o.tenant_id
             WHERE op.rowid > ?2 AND op.voided_at_utc IS NULL
               AND op.block_height IS NULL AND t.network = ?1
             ORDER BY op.rowid LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(
                params![
                    shared::network::SqlNetwork(network),
                    after_rowid,
                    limit as i64
                ],
                |row| Ok((row.get(0)?, Self::row_to_payment(row)?)),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    #[cfg(test)]
    pub fn overwrite_payment_key_images_for_test(&self, order_id: &OrderId, raw: &str) {
        self.conn
            .execute(
                "UPDATE order_payments SET key_images_json = ?2 WHERE order_id = ?1",
                params![order_id, raw],
            )
            .unwrap();
    }

    /// Recomputes `status`, `confirmations`, and `amount_received_piconero` from the
    /// order's currently-valid payments and persists the result. This is the *only*
    /// place `orders.status` is written - see `docs/DESIGN.md` §7.6. Returns
    /// `(old_status, new_status)` so the caller can decide whether a status-transition
    /// webhook event is warranted.
    /// Uses `order.confirmations_required_override` when the order has one
    /// (set once, at creation, by a caller like monokulo that already
    /// resolved an amount-tiered confirmation requirement of its own),
    /// falling back to `tenant.confirmations_required` exactly as before
    /// when it's `None` - the ordinary case for every order created outside
    /// that feature.
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "the stored order is compared with the plan, whose fields are named for it"
    )]
    pub fn recompute_order_status(
        &self,
        order_id: &OrderId,
        current_height: u64,
        now: i64,
    ) -> Result<(OrderStatus, OrderStatus)> {
        let order = self
            .get_order_by_id(order_id)?
            .ok_or(StoreError::NotFound)?;
        let (confirmations_required, network, lagging) = self.recompute_facts(&order.tenant_id)?;
        let conflicts = self.settle_output_key_conflicts(order_id, network, now)?;
        let views: Vec<PaymentView> = self
            .get_valid_payments(order_id)?
            .iter()
            .filter(|p| !conflicts.uncounted.contains(&p.id))
            .map(|p| PaymentView {
                amount_piconero: p.amount_piconero,
                confirmations: match p.block_height {
                    Some(h) if current_height >= h as u64 => current_height - h as u64 + 1,
                    _ => 0,
                },
                is_zero_conf: p.block_height.is_none(),
            })
            .collect();
        let plan = plan_status(&StatusFacts {
            order: &order,
            views: &views,
            confirmations_required: order
                .confirmations_required_override
                .unwrap_or(confirmations_required),
            tenant_lagging: lagging,
            settlement_frozen: self.settlement_frozen(network)?,
            conflicted: conflicts.unsettled,
            proven_views: self.proven_views(
                network,
                order_id,
                current_height,
                &conflicts.uncounted,
            )?,
            current_height,
            now,
        });

        // `closed_at_utc` (migration 0016): set the first time the order is
        // terminal, kept while it stays terminal, cleared if it reopens. An
        // expired order closed at its deadline, however late expiry was
        // noticed (its store may have been catching up). Written only if
        // something changed: most recomputes change nothing, and a write that
        // changes nothing still costs a page write on slow storage.
        let closed_at = if plan.status == OrderStatus::Expired {
            order.expires_at.min(now)
        } else {
            now
        };
        self.conn.execute(
            "UPDATE orders SET status = ?2, confirmations = ?3, amount_received_piconero = ?4, updated_at_utc = ?5,
                closed_at_utc = CASE WHEN ?6 THEN COALESCE(closed_at_utc, ?7) ELSE NULL END,
                next_due_at_utc = ?8, next_due_height = ?9
             WHERE id = ?1
               AND (status IS NOT ?2 OR confirmations IS NOT ?3 OR amount_received_piconero IS NOT ?4
                    OR closed_at_utc IS NOT (CASE WHEN ?6 THEN COALESCE(closed_at_utc, ?7) ELSE NULL END)
                    OR next_due_at_utc IS NOT ?8 OR next_due_height IS NOT ?9)",
            params![
                order_id,
                status_to_str(plan.status),
                plan.confirmations as i64,
                plan.amount_received as i64,
                now,
                is_terminal(plan.status),
                closed_at,
                plan.next_due_at,
                plan.next_due_height,
            ],
        )?;
        // The payment-change obligation is met by this recompute, unless the
        // settlement it implies had to wait.
        if plan.keep_obligation {
            self.conn.execute(
                "INSERT OR IGNORE INTO pending_payment_recomputes (order_id) VALUES (?1)",
                [order_id],
            )?;
        } else {
            self.clear_pending_payment_recompute(order_id)?;
        }
        if order.status != plan.status
            || order.confirmations != plan.confirmations
            || order.amount_received_piconero != plan.amount_received
        {
            self.publish_order_change(&order.tenant_id, order_id);
        }
        Ok((order.status, plan.status))
    }

    /// What a status recompute needs to know about an order's tenant: its
    /// confirmations requirement, its network, and whether it is behind the
    /// network (a disabled tenant never is). One small row, not the tenant
    /// with its key material.
    fn recompute_facts(&self, tenant_id: &TenantId) -> Result<(u64, monero::Network, bool)> {
        self.conn
            .query_row(
                "SELECT t.confirmations_required, t.network,
                        t.disabled_at_utc IS NULL AND t.scanned_through_height IS NOT NULL
                        AND t.scanned_through_height < (SELECT MAX(height) FROM scanned_blocks WHERE network = t.network)
                 FROM tenants t WHERE t.id = ?1",
                [tenant_id],
                |row| {
                    Ok((
                        row.get::<_, shared::sqlite::Unsigned<u64>>(0)?.0,
                        row.get::<_, shared::network::SqlNetwork>(1)?.0,
                        row.get::<_, Option<bool>>(2)?.unwrap_or(false),
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    /// Sticky, first-occurrence-only - see schema comment on `double_spend_detected_at`.
    pub fn mark_double_spend_detected(&self, order_id: &OrderId, at: i64) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE orders SET double_spend_detected_at_utc = ?2 WHERE id = ?1 AND double_spend_detected_at_utc IS NULL",
            params![order_id, at],
        )?;
        if changed > 0 {
            self.publish_order_change_by_id(order_id)?;
        }
        Ok(changed > 0)
    }

    /// Reverses `mark_double_spend_detected` - deliberately the *only* way this flag
    /// is ever cleared, unlike `unvoid_payment`'s reorg-driven counterpart, which by
    /// design leaves it set (see that method's own doc comment: a real conflicting
    /// transaction genuinely existed there, even if later reorged away). This exists
    /// for `engine::unvoid_as_false_positive`, whose whole premise is that the
    /// original accusation may never have been true at all - see that function's own
    /// doc comment for why it only calls this once every voided payment on the order
    /// has been cleared, never as a side effect of clearing just one of several.
    /// Returns `false` if the flag was already unset (idempotent).
    pub fn clear_double_spend_flag(&self, order_id: &OrderId) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE orders SET double_spend_detected_at_utc = NULL WHERE id = ?1 AND double_spend_detected_at_utc IS NOT NULL",
            params![order_id],
        )?;
        if changed > 0 {
            self.publish_order_change_by_id(order_id)?;
        }
        Ok(changed > 0)
    }

    // -- Reorg bookkeeping --------------------------------------------------

    /// The highest height the scanner has recorded a hash for *on this network*, or
    /// `None` if it has never scanned a block on it yet. Used at boot (per network)
    /// to decide where to resume: rather than replaying the entire chain history on
    /// first run, the scanner seeds this at the current tip and only scans forward
    /// from there (see `engine::run_scan_tick`). Scoped by network because heights
    /// are meaningless across chains - mainnet height 100 and stagenet height 100
    /// are unrelated blocks.
    pub fn max_scanned_height(&self, network: monero::Network) -> Result<Option<u64>> {
        self.conn
            .query_row(
                "SELECT MAX(height) FROM scanned_blocks WHERE network = ?1",
                params![shared::network::SqlNetwork(network)],
                |row| row.get::<_, Option<i64>>(0),
            )
            .map(|opt| opt.map(|h| h as u64))
            .map_err(Into::into)
    }

    /// Not scoped by tenant - internal orchestration use only (the scanner needs to
    /// route a bare `order_id` back to its tenant to look up webhooks). Never
    /// expose this through the HTTP layer; every externally-reachable order lookup
    /// must go through `get_order`'s tenant-scoped query instead.
    pub fn get_order_tenant_id(&self, order_id: &OrderId) -> Result<Option<TenantId>> {
        self.conn
            .query_row(
                "SELECT tenant_id FROM orders WHERE id = ?1",
                params![order_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_scanned_block_hash(
        &self,
        network: monero::Network,
        height: u64,
    ) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT block_hash FROM scanned_blocks WHERE network = ?1 AND height = ?2",
                params![shared::network::SqlNetwork(network), height as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_scanned_block(
        &self,
        network: monero::Network,
        height: u64,
        hash: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO scanned_blocks (network, height, block_hash) VALUES (?1, ?2, ?3)
             ON CONFLICT(network, height) DO UPDATE SET block_hash = excluded.block_hash",
            params![shared::network::SqlNetwork(network), height as i64, hash],
        )?;
        Ok(())
    }

    pub fn stage_partial_match(&self, matched: &StagedMatch<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO partial_block_matches
             (network, tenant_id, order_id, txid, output_index, amount_piconero, key_images_json, seen_at_utc, output_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![shared::network::SqlNetwork(matched.network), matched.tenant_id, matched.order_id, matched.txid, matched.output_index,
                shared::sqlite::Unsigned(matched.amount), matched.key_images_json, matched.seen_at, matched.output_key],
        )?;
        Ok(())
    }

    pub fn clear_partial_block(
        &self,
        network: monero::Network,
        tenant_id: &TenantId,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM partial_block_matches WHERE network = ?1 AND tenant_id = ?2",
            params![shared::network::SqlNetwork(network), tenant_id],
        )?;
        self.conn.execute(
            "DELETE FROM partial_block_progress WHERE network = ?1 AND tenant_id = ?2",
            params![shared::network::SqlNetwork(network), tenant_id],
        )?;
        Ok(())
    }

    /// Forgets every block at or above `height` on this network, walking
    /// `max_scanned_height` back to `height - 1` so the next tick's forward scan
    /// naturally re-covers `height..tip`.
    ///
    /// Called once a reorg starting at `height` has been fully reconciled. Without
    /// it, reconciliation fixes up the payments it already knew about but nothing
    /// ever *scans* the replacement blocks, so a payment that exists only in the new
    /// chain - the transaction was rebroadcast and mined into the fork that won - is
    /// never seen at all: the scanner's high-water mark is still above those heights,
    /// and blocks below it are never revisited.
    pub fn forget_scanned_blocks_at_or_above(
        &self,
        network: monero::Network,
        height: u64,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM scanned_blocks WHERE network = ?1 AND height >= ?2",
            params![shared::network::SqlNetwork(network), height as i64],
        )?;
        Ok(())
    }

    /// Copies what it can of the write-ahead log back into the database
    /// without waiting for anyone (`PASSIVE`). Returns whether the whole log
    /// was copied. With two writers and long-lived readers the log can grow
    /// between SQLite's own automatic checkpoints; once a checkpoint lets it
    /// reset, `journal_size_limit` trims the file.
    pub fn checkpoint_wal(&self) -> Result<bool> {
        let (busy, log, checkpointed): (i64, i64, i64) =
            self.conn
                .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
        Ok(busy == 0 && log == checkpointed)
    }

    pub fn prune_scanned_blocks_below(
        &self,
        network: monero::Network,
        min_height: u64,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM scanned_blocks WHERE network = ?1 AND height < ?2",
            params![shared::network::SqlNetwork(network), min_height as i64],
        )?;
        Ok(())
    }

    // -- Per-tenant scan cursors (admin_settings_v2.md task 5.0) ---------
    //
    // A tenant's cursor is the highest block on its network fully scanned for
    // it. The network's high-water mark (`max_scanned_height`) only moves when
    // every caught-up tenant has been scanned for a block; a tenant that fails
    // is left behind with its cursor where it was, and caught up later.

    /// Gives every tenant on `network` whose cursor was never set the
    /// network's height. Called when a network is first seeded.
    pub fn anchor_unset_cursors(&self, network: monero::Network, height: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE tenants SET scanned_through_height = ?2 WHERE network = ?1 AND scanned_through_height IS NULL",
            params![shared::network::SqlNetwork(network), height as i64],
        )?;
        Ok(())
    }

    /// After a reorg rewinds `network` to `height`, no tenant can be ahead of
    /// it. `None` (the reorg reached genesis) un-anchors every cursor.
    pub fn clamp_cursors(&self, network: monero::Network, height: Option<u64>) -> Result<()> {
        match height {
            Some(h) => self.conn.execute(
                "UPDATE tenants SET scanned_through_height = ?2 WHERE network = ?1 AND scanned_through_height > ?2",
                params![shared::network::SqlNetwork(network), h as i64],
            )?,
            None => self.conn.execute(
                "UPDATE tenants SET scanned_through_height = NULL WHERE network = ?1",
                params![shared::network::SqlNetwork(network)],
            )?,
        };
        Ok(())
    }

    /// Tenants on `network` whose cursor is below the network's high-water
    /// mark, with their cursors, lowest first.
    pub fn lagging_tenants(&self, network: monero::Network) -> Result<Vec<(TenantId, u64)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, scanned_through_height FROM tenants
             WHERE network = ?1 AND disabled_at_utc IS NULL
               AND scanned_through_height < (SELECT MAX(height) FROM scanned_blocks WHERE network = ?1)
             ORDER BY scanned_through_height, id",
        )?;
        let rows = stmt
            .query_map(params![shared::network::SqlNetwork(network)], |row| {
                Ok((
                    row.get::<_, TenantId>(0)?,
                    row.get::<_, shared::sqlite::Unsigned<u64>>(1)?.0,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Disabled tenants on `network` whose cursor is behind: nothing is
    /// scanned for them any more, so they are simply moved along.
    pub fn snap_disabled_cursors(&self, network: monero::Network, height: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE tenants SET scanned_through_height = ?2
             WHERE network = ?1 AND disabled_at_utc IS NOT NULL AND scanned_through_height < ?2",
            params![shared::network::SqlNetwork(network), height as i64],
        )?;
        self.conn.execute(
            "DELETE FROM partial_block_matches WHERE network = ?1 AND tenant_id IN
             (SELECT id FROM tenants WHERE network = ?1 AND disabled_at_utc IS NOT NULL)",
            [shared::network::SqlNetwork(network)],
        )?;
        self.conn.execute(
            "DELETE FROM partial_block_progress WHERE network = ?1 AND tenant_id IN
             (SELECT id FROM tenants WHERE network = ?1 AND disabled_at_utc IS NOT NULL)",
            [shared::network::SqlNetwork(network)],
        )?;
        Ok(())
    }

    // -- Scanned block range (`docs/order_rescan_wbs.md` Phase 5.1) -------

    /// Bumps every one of `tenant_id`'s currently-in-scope orders (the scan
    /// window's predicate, `IN_SCAN_WINDOW`, grace period included) to `height` - one bulk `UPDATE`, not a per-order loop.
    /// Called once per active tenant per scan tick (`engine::run_scan_tick`), after
    /// its block-scanning pass. `first_scanned_height` only moves via `COALESCE`
    /// (set once, on an order's first tick, and never again) - `last_scanned_height`
    /// moves every call while the order stays in scope, and simply stops moving
    /// (not reset) the moment it falls out of scope, since this predicate then no
    /// longer selects it.
    pub fn bump_scanned_heights_for_tenant(
        &self,
        tenant_id: &TenantId,
        height: u64,
        now: i64,
        grace_period_seconds: i64,
    ) -> Result<()> {
        // Only rows it changes: most rounds nothing has moved, and a write
        // that changes nothing still costs a page write on slow storage.
        self.conn.execute(
            &format!(
                "UPDATE orders
                 SET last_scanned_height = :height, first_scanned_height = COALESCE(first_scanned_height, :height)
                 WHERE id IN ({})
                   AND (last_scanned_height IS NOT :height OR first_scanned_height IS NULL)",
                scan_window_orders("o.tenant_id = :tenant")
            ),
            rusqlite::named_params! {
                ":tenant": tenant_id,
                ":height": height as i64,
                ":since_minus_grace": now - grace_period_seconds,
            },
        )?;
        Ok(())
    }

    /// One runtime-configurable setting's stored value (§`migrations/0010_settings.sql`),
    /// or `None` if nothing has ever been saved for `key`. Settings themselves are
    /// resolved by `engine_settings` (environment, then this, then the default).
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Persists one setting - an `INSERT ... ON CONFLICT DO UPDATE` upsert, since the
    /// admin settings page's own "Save" always writes every field it shows regardless
    /// of whether a row already exists for it (docs: the button is always clickable,
    /// including to persist a value that's currently coming from an environment
    /// variable into the database for the first time).
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Removes a setting's stored row entirely - not the same as saving an empty
    /// string, which is still a real, present value (`get_setting` returns
    /// `Some("")`, not `None`). Used for "unconfigure this" choices a setting
    /// genuinely supports (e.g. a `monero_node.<network>` entry being cleared),
    /// where falling through to a hardcoded default would be wrong - there is
    /// no sensible default Monero node to fall back to.
    pub fn delete_setting(&self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(())
    }

    /// Every stored setting at once, as `(key, value)` pairs - the admin settings
    /// page's `GET` reads the whole table in one query rather than one `get_setting`
    /// call per known key, then resolves each known setting's effective value/source
    /// against this map plus the environment.
    pub fn list_settings(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT key, value FROM settings")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()
            .map_err(Into::into)
    }

    // -- Webhooks -------------------------------------------------------

    pub fn create_webhook(
        &self,
        tenant_id: &TenantId,
        url: &str,
        extra_headers_json: &str,
        signing_secret: &str,
        now: i64,
    ) -> Result<Webhook> {
        let id = WebhookId::new(new_id("wh"));
        self.conn.execute(
            "INSERT INTO webhooks (id, tenant_id, url, extra_headers, signing_secret, created_at_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, tenant_id, url, extra_headers_json, signing_secret, now],
        )?;
        Ok(Webhook {
            id,
            tenant_id: tenant_id.clone(),
            url: url.to_owned(),
            extra_headers: extra_headers_json.to_owned(),
            signing_secret: live_settings::Secret::new(signing_secret),
            enabled: true,
            created_at: now,
        })
    }

    pub fn list_webhooks(&self, tenant_id: &TenantId) -> Result<Vec<Webhook>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT * FROM webhooks WHERE tenant_id = ?1 ORDER BY created_at_utc, id",
        )?;
        let rows = stmt
            .query_map(params![tenant_id], |row| {
                Ok(Webhook {
                    id: row.get("id")?,
                    tenant_id: row.get("tenant_id")?,
                    url: row.get("url")?,
                    extra_headers: row.get("extra_headers")?,
                    signing_secret: live_settings::Secret::new(
                        row.get::<_, String>("signing_secret")?,
                    ),
                    enabled: row.get::<_, i64>("enabled")? != 0,
                    created_at: row.get("created_at_utc")?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Scoped by `tenant_id`, same IDOR-prevention rule as `get_order`. Returns
    /// `false` (not an error) if the webhook doesn't exist or belongs to a different
    /// tenant - the two are indistinguishable from the caller's perspective.
    pub fn delete_webhook(&self, tenant_id: &TenantId, webhook_id: &WebhookId) -> Result<bool> {
        let changed = self.conn.execute(
            "DELETE FROM webhooks WHERE id = ?1 AND tenant_id = ?2",
            params![webhook_id, tenant_id],
        )?;
        Ok(changed > 0)
    }

    pub fn enqueue_webhook_delivery(
        &self,
        webhook_id: &WebhookId,
        order_id: &OrderId,
        event_type: &str,
        payload_json: &str,
        next_attempt_at: i64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO webhook_deliveries (webhook_id, order_id, event_type, payload_json, next_attempt_at_utc)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![webhook_id, order_id, event_type, payload_json, next_attempt_at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// One delivery attempt's worth of everything the delivery worker needs,
    /// joined from `webhook_deliveries` and `webhooks` in one query so the worker
    /// never has to look up the owning webhook separately per row.
    /// Due deliveries, oldest first, picked fairly for concurrent sending
    /// (`webhook_delivery::run_delivery_tick`):
    /// - at most one per (webhook, order), the oldest *enqueued* among every
    ///   undelivered one (due or waiting out a retry), so two events for the
    ///   same order are never in flight at once and a later event never
    ///   overtakes an earlier one that is between attempts; a given-up
    ///   delivery holds nothing back;
    /// - at most `per_tenant` per store, counted after that, so one store
    ///   with a big backlog (or a slow endpoint) can't fill the batch and
    ///   hold up every other store, and one order's backlog doesn't use up
    ///   the store's share.
    pub fn due_webhook_deliveries_fair(
        &self,
        now: i64,
        per_tenant: u32,
        limit: u32,
    ) -> Result<Vec<DueDelivery>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, webhook_id, order_id, event_type, payload_json, attempt_count, url, extra_headers, signing_secret
             FROM (
                SELECT *, ROW_NUMBER() OVER (PARTITION BY tenant_id ORDER BY due_at, id) AS per_tenant_rank
                FROM (
                    SELECT d.id, d.webhook_id, d.order_id, d.event_type, d.payload_json, d.attempt_count,
                           w.url, w.extra_headers, w.signing_secret, w.tenant_id, d.next_attempt_at_utc AS due_at,
                           ROW_NUMBER() OVER (PARTITION BY d.webhook_id, d.order_id ORDER BY d.id) AS per_order
                    FROM webhook_deliveries d
                    JOIN webhooks w ON w.id = d.webhook_id
                    WHERE d.delivered_at_utc IS NULL AND d.gave_up_at_utc IS NULL AND w.enabled = 1
                )
                WHERE per_order = 1 AND due_at <= ?1
             )
             WHERE per_tenant_rank <= ?2
             ORDER BY due_at, id
             LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![now, per_tenant, limit], |row| {
                Ok(DueDelivery {
                    delivery_id: row.get(0)?,
                    webhook_id: row.get(1)?,
                    order_id: row.get(2)?,
                    event_type: row.get(3)?,
                    payload_json: row.get(4)?,
                    attempt_count: row.get::<_, shared::sqlite::Unsigned<u32>>(5)?.0,
                    url: row.get(6)?,
                    extra_headers_json: row.get(7)?,
                    signing_secret: live_settings::Secret::new(row.get::<_, String>(8)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// How many webhook deliveries are due and waiting, and since when the
    /// oldest has been (task 7.13). Given-up deliveries aren't counted.
    pub fn webhook_backlog(&self, now: i64) -> Result<(u64, Option<i64>)> {
        self.conn
            .query_row(
                "SELECT COUNT(*), MIN(next_attempt_at_utc) FROM webhook_deliveries
                 WHERE delivered_at_utc IS NULL AND gave_up_at_utc IS NULL AND next_attempt_at_utc <= ?1",
                params![now],
                |row| {
                    Ok((
                        row.get::<_, shared::sqlite::Unsigned<u64>>(0)?.0,
                        row.get::<_, Option<i64>>(1)?,
                    ))
                },
            )
            .map_err(Into::into)
    }

    /// Every undelivered, not given-up delivery due by `now`, oldest first,
    /// for tests that assert on what was enqueued. The engine picks what to
    /// send with [`Self::due_webhook_deliveries_fair`].
    #[cfg(test)]
    pub fn due_webhook_deliveries_for_test(
        &self,
        now: i64,
        limit: u32,
    ) -> Result<Vec<DueDelivery>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT d.id, d.webhook_id, d.order_id, d.event_type, d.payload_json, d.attempt_count,
                    w.url, w.extra_headers, w.signing_secret
             FROM webhook_deliveries d
             JOIN webhooks w ON w.id = d.webhook_id
             WHERE d.delivered_at_utc IS NULL AND d.gave_up_at_utc IS NULL AND d.next_attempt_at_utc <= ?1 AND w.enabled = 1
             ORDER BY d.next_attempt_at_utc, d.id
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![now, limit], |row| {
                Ok(DueDelivery {
                    delivery_id: row.get(0)?,
                    webhook_id: row.get(1)?,
                    order_id: row.get(2)?,
                    event_type: row.get(3)?,
                    payload_json: row.get(4)?,
                    attempt_count: row.get::<_, shared::sqlite::Unsigned<u32>>(5)?.0,
                    url: row.get(6)?,
                    extra_headers_json: row.get(7)?,
                    signing_secret: live_settings::Secret::new(row.get::<_, String>(8)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn mark_webhook_delivered(
        &self,
        delivery_id: i64,
        response_status: u16,
        at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE webhook_deliveries
             SET attempt_count = attempt_count + 1, delivered_at_utc = ?2, last_attempted_at_utc = ?2,
                 last_response_status = ?3
             WHERE id = ?1",
            params![delivery_id, at, response_status as i64],
        )?;
        Ok(())
    }

    /// Records a final failed attempt: the delivery is never retried, holds
    /// no later event for its order back, and stays in the table for
    /// inspection.
    pub fn give_up_webhook_delivery(
        &self,
        delivery_id: i64,
        response_status: Option<u16>,
        error: Option<&str>,
        at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE webhook_deliveries
             SET attempt_count = attempt_count + 1,
                 gave_up_at_utc = ?2,
                 last_attempted_at_utc = ?2,
                 last_response_status = ?3,
                 last_error = ?4
             WHERE id = ?1",
            params![delivery_id, at, response_status.map(|s| s as i64), error],
        )?;
        Ok(())
    }

    pub fn schedule_webhook_retry(
        &self,
        delivery_id: i64,
        next_attempt_at: i64,
        response_status: Option<u16>,
        error: Option<&str>,
        at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE webhook_deliveries
             SET attempt_count = attempt_count + 1,
                 next_attempt_at_utc = ?2,
                 last_attempted_at_utc = ?3,
                 last_response_status = ?4,
                 last_error = ?5
             WHERE id = ?1",
            params![
                delivery_id,
                next_attempt_at,
                at,
                response_status.map(|s| s as i64),
                error
            ],
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct DueDelivery {
    pub delivery_id: i64,
    pub webhook_id: WebhookId,
    pub order_id: OrderId,
    pub event_type: String,
    pub payload_json: String,
    pub attempt_count: u32,
    pub url: String,
    pub extra_headers_json: String,
    /// Hidden in `Debug`; `expose` it only to sign a delivery.
    pub signing_secret: live_settings::Secret,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn order(status: OrderStatus, expires_at: i64) -> Order {
        Order {
            id: "o".into(),
            tenant_id: "t".into(),
            merchant_order_id: None,
            minor_index: 1,
            address: "a".into(),
            xmr_amount_piconero: 100,
            amount_received_piconero: 0,
            status,
            confirmations: 0,
            double_spend_detected_at: None,
            refund_address: None,
            description: None,
            created_at: 0,
            expires_at,
            updated_at: 0,
            first_scanned_height: None,
            last_scanned_height: None,
            confirmations_required_override: None,
            closed_at: None,
        }
    }

    fn mined(amount: u64, confirmations: u64) -> PaymentView {
        PaymentView {
            amount_piconero: amount,
            confirmations,
            is_zero_conf: false,
        }
    }

    fn pooled(amount: u64) -> PaymentView {
        PaymentView {
            amount_piconero: amount,
            confirmations: 0,
            is_zero_conf: true,
        }
    }

    /// The status rules as a table: derived status, the expiry hold, the
    /// settlement freeze, and the next points an order is due.
    #[test]
    fn plan_status_covers_every_rule() {
        use OrderStatus::*;
        struct Case {
            what: &'static str,
            was: OrderStatus,
            views: Vec<PaymentView>,
            lagging: bool,
            frozen: bool,
            now: i64,
            required: u64,
            expect: (OrderStatus, Option<i64>, Option<i64>, bool),
        }
        // Ten confirmations required, deadline 1000, tip 50.
        let cases = [
            Case {
                what: "unpaid, before its deadline",
                was: Pending,
                views: vec![],
                lagging: false,
                frozen: false,
                now: 500,
                required: 10,
                expect: (Pending, Some(1001), None, false),
            },
            Case {
                what: "unpaid, after its deadline",
                was: Pending,
                views: vec![],
                lagging: false,
                frozen: false,
                now: 2000,
                required: 10,
                expect: (Expired, None, None, false),
            },
            Case {
                what: "expiry held while the tenant is behind",
                was: Pending,
                views: vec![],
                lagging: true,
                frozen: false,
                now: 2000,
                required: 10,
                expect: (Pending, Some(2000), None, false),
            },
            Case {
                what: "already expired: a hold doesn't reopen it",
                was: Expired,
                views: vec![],
                lagging: true,
                frozen: false,
                now: 2000,
                required: 10,
                expect: (Expired, None, None, false),
            },
            Case {
                what: "part paid, confirming",
                was: Pending,
                views: vec![mined(40, 3)],
                lagging: false,
                frozen: false,
                now: 500,
                required: 10,
                expect: (Partial, Some(1001), Some(51), false),
            },
            Case {
                what: "paid in the pool",
                was: Pending,
                views: vec![pooled(100)],
                lagging: false,
                frozen: false,
                now: 500,
                required: 10,
                expect: (Unconfirmed, None, None, false),
            },
            Case {
                what: "paid, confirming",
                was: Unconfirmed,
                views: vec![mined(100, 3)],
                lagging: false,
                frozen: false,
                now: 500,
                required: 10,
                expect: (Confirming, None, Some(51), false),
            },
            Case {
                what: "paid and confirmed",
                was: Confirming,
                views: vec![mined(100, 10)],
                lagging: false,
                frozen: false,
                now: 500,
                required: 10,
                expect: (Paid, None, None, false),
            },
            Case {
                what: "overpaid and confirmed",
                was: Confirming,
                views: vec![mined(150, 10)],
                lagging: false,
                frozen: false,
                now: 500,
                required: 10,
                expect: (Overpaid, None, None, false),
            },
            Case {
                what: "settlement waits for a reorg",
                was: Confirming,
                views: vec![mined(100, 10)],
                lagging: false,
                frozen: true,
                now: 500,
                required: 10,
                expect: (Confirming, Some(500), None, true),
            },
            Case {
                what: "a zero-conf settlement waits too",
                was: Pending,
                views: vec![pooled(100)],
                lagging: false,
                frozen: true,
                now: 500,
                required: 0,
                expect: (Unconfirmed, Some(500), None, true),
            },
            Case {
                what: "zero-conf accepted: paid from the pool",
                was: Pending,
                views: vec![pooled(100)],
                lagging: false,
                frozen: false,
                now: 500,
                required: 0,
                expect: (Paid, None, None, false),
            },
            Case {
                what: "already paid: the freeze doesn't hold it",
                was: Paid,
                views: vec![mined(100, 12)],
                lagging: false,
                frozen: true,
                now: 500,
                required: 10,
                expect: (Paid, None, None, false),
            },
        ];
        for case in cases {
            let order = order(case.was, 1000);
            let plan = plan_status(&StatusFacts {
                order: &order,
                views: &case.views,
                confirmations_required: case.required,
                tenant_lagging: case.lagging,
                settlement_frozen: case.frozen,
                conflicted: false,
                proven_views: None,
                current_height: 50,
                now: case.now,
            });
            assert_eq!(
                (
                    plan.status,
                    plan.next_due_at,
                    plan.next_due_height,
                    plan.keep_obligation
                ),
                case.expect,
                "{}",
                case.what
            );
        }
    }

    /// An order settles only on its payments as proven
    /// (`docs/proof_of_work.md`); the counts shown stay the real ones, and an
    /// order already settled isn't walked back.
    #[test]
    fn settlement_waits_for_proven_payments() {
        use OrderStatus::*;
        // Ten confirmations required; the payment has 12 recorded. As
        // proven it has `proven` (0: its block isn't the proven one).
        for (proven, was, expect, deferred) in [
            (None, Confirming, Paid, false),
            (Some(12), Confirming, Paid, false),
            (Some(10), Confirming, Paid, false),
            (Some(9), Confirming, Confirming, true),
            (Some(0), Confirming, Confirming, true),
            (Some(0), Paid, Paid, false),
        ] {
            let order = order(was, 1000);
            let plan = plan_status(&StatusFacts {
                order: &order,
                views: &[mined(100, 12)],
                confirmations_required: 10,
                tenant_lagging: false,
                settlement_frozen: false,
                conflicted: false,
                proven_views: proven.map(|c| vec![mined(100, c)]),
                current_height: 50,
                now: 500,
            });
            assert_eq!(
                (plan.status, plan.keep_obligation, plan.confirmations),
                (expect, deferred, 12),
                "proven {proven:?}, was {was:?}"
            );
            if deferred {
                assert_eq!(plan.next_due_at, Some(500), "looked at again next round");
            }
        }
        // Zero-conf acceptance has no block to wait for.
        let order = order(Pending, 1000);
        let plan = plan_status(&StatusFacts {
            order: &order,
            views: &[pooled(100)],
            confirmations_required: 0,
            tenant_lagging: false,
            settlement_frozen: false,
            conflicted: false,
            proven_views: Some(vec![pooled(100)]),
            current_height: 50,
            now: 500,
        });
        assert_eq!(plan.status, Paid);
    }

    /// A recompute that changes nothing writes nothing (`updated_at` stays).
    #[test]
    fn a_recompute_that_changes_nothing_writes_nothing() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);
        store.recompute_order_status(&order.id, 10, 1_000).unwrap();
        let before = store
            .get_order(&tenant.tenant.id, &order.id)
            .unwrap()
            .unwrap()
            .updated_at;
        store.recompute_order_status(&order.id, 11, 1_500).unwrap();
        let after = store
            .get_order(&tenant.tenant.id, &order.id)
            .unwrap()
            .unwrap()
            .updated_at;
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn read_pool_uses_independent_connections_without_blocking_the_runtime() {
        let path = std::env::temp_dir().join(format!("scanner_read_pool_{}.db", Uuid::new_v4()));
        let path_str = path.to_string_lossy().into_owned();
        let writer = Store::open_file(&path_str).unwrap();
        writer
            .set_scanned_block(monero::Network::Mainnet, 1, "h1")
            .unwrap();
        let pool = ReadStorePool::open(&path_str, 2).unwrap();

        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let held_pool = pool.clone();
        let held = tokio::spawn(async move {
            held_pool
                .query(move |store| {
                    started.send(()).unwrap();
                    wait.recv().unwrap();
                    store.max_scanned_height(monero::Network::Mainnet)
                })
                .await
        });
        ready.await.unwrap();
        // A second read reaches another connection while the first worker is
        // deliberately occupied. No async worker or writer mutex is involved.
        let second = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            pool.query(|store| store.max_scanned_height(monero::Network::Mainnet)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(second, Some(1));
        release.send(()).unwrap();
        assert_eq!(held.await.unwrap().unwrap(), Some(1));
        drop(pool);
        drop(writer);
        let _ = std::fs::remove_file(path);
    }

    fn new_tenant(store: &Store) -> CreatedTenant {
        store
            .create_tenant(
                &NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![0u8; 64],
                    primary_address: "4addr".into(),
                    network: "mainnet".into(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap()
    }

    fn new_order(store: &Store, tenant_id: &str, minor_index: u32) -> Order {
        store
            .create_order(&NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: TenantId::new(tenant_id.to_owned()),
                merchant_order_id: None,
                minor_index,
                address: format!("sub_{minor_index}"),
                xmr_amount_piconero: 100,
                description: None,
                created_at: 1000,
                expires_at: 2000,
            })
            .unwrap()
    }

    #[test]
    fn a_panic_while_holding_the_shared_store_does_not_break_later_users() {
        let shared = Store::open_in_memory().unwrap().into_shared();
        let tenant = new_tenant(&shared.lock());
        let panicking = Arc::clone(&shared);
        let joined = std::thread::spawn(move || {
            let _guard = panicking.lock();
            panic!("simulated bug while holding the store lock");
        })
        .join();
        assert!(
            joined.is_err(),
            "the helper thread must really have panicked"
        );

        // With a poisoning mutex every later `lock()` would fail from here on,
        // taking the scan loop and every HTTP handler down with it.
        let store = shared.lock();
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);
        assert_eq!(
            store.get_order_by_id(&order.id).unwrap().unwrap().id,
            order.id
        );
    }

    #[test]
    fn a_panic_inside_a_transaction_rolls_back_and_leaves_the_store_usable() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let mut changes = store.subscribe_order_changes();

        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: std::result::Result<(), StoreError> = store.in_transaction(|s| {
                new_order(s, tenant.tenant.id.as_str(), 1);
                panic!("simulated bug mid-transaction");
            });
        }));
        assert!(caught.is_err());

        // Rolled back: the order written inside the transaction is gone.
        assert!(store
            .find_order_by_minor_index(&tenant.tenant.id, 1)
            .unwrap()
            .is_none());
        // No transaction left open: a new one begins and commits normally.
        store
            .in_transaction(|s| -> Result<()> {
                new_order(s, tenant.tenant.id.as_str(), 2);
                Ok(())
            })
            .unwrap();
        assert!(store
            .find_order_by_minor_index(&tenant.tenant.id, 2)
            .unwrap()
            .is_some());

        // And changes made outside any transaction are published straight away,
        // not left in the buffer the panicked transaction had set up.
        while changes.try_recv().is_ok() {}
        let order = store
            .find_order_by_minor_index(&tenant.tenant.id, 2)
            .unwrap()
            .unwrap();
        assert!(store
            .set_refund_address(&tenant.tenant.id, &order.id, "refund")
            .unwrap());
        assert_eq!(changes.try_recv().unwrap().order_id, order.id);
    }

    #[test]
    fn an_unknown_order_status_in_a_row_is_an_error_not_a_panic() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);
        store
            .execute_raw_for_test(&format!(
                "PRAGMA ignore_check_constraints = ON; UPDATE orders SET status = 'bogus' WHERE id = '{}'; PRAGMA ignore_check_constraints = OFF;",
                order.id
            ))
            .unwrap();
        let err = store.get_order_by_id(&order.id).unwrap_err();
        assert!(err.to_string().contains("bogus"), "got: {err}");
    }

    #[test]
    fn a_full_disk_is_reported_as_a_disk_full_error_and_nothing_is_half_written() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        // Cap the database at its current size, then fill whatever free space
        // is left, so the next write can't grow it.
        store
            .execute_raw_for_test("PRAGMA max_page_count = 1;")
            .unwrap();
        let mut filled = 0;
        while store
            .set_setting(&format!("filler.{filled}"), &"x".repeat(2000))
            .is_ok()
        {
            filled += 1;
            assert!(filled < 10_000, "the cap didn't take effect");
        }
        let err = store.create_order(&NewOrder {
            idempotency_key: None,
            confirmations_required_override: None,
            tenant_id: tenant.tenant.id.clone(),
            merchant_order_id: None,
            minor_index: 1,
            address: "x".repeat(4000),
            xmr_amount_piconero: 1,
            description: Some("y".repeat(4000)),
            created_at: 1,
            expires_at: 2,
        });
        match err {
            Err(StoreError::Sqlite(e)) => {
                assert_eq!(e.sqlite_error_code(), Some(rusqlite::ErrorCode::DiskFull));
            }
            other => panic!("expected a disk-full error, got {other:?}"),
        }
        assert!(store
            .find_order_by_minor_index(&tenant.tenant.id, 1)
            .unwrap()
            .is_none());

        // Space comes back: writes work again.
        store
            .execute_raw_for_test("PRAGMA max_page_count = 1000000;")
            .unwrap();
        new_order(&store, tenant.tenant.id.as_str(), 1);
    }

    #[test]
    fn a_setting_that_was_never_saved_reads_as_none() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(
            store.get_setting("payment.confirmations_required").unwrap(),
            None
        );
        assert_eq!(store.list_settings().unwrap().len(), 0);
    }

    #[test]
    fn a_saved_setting_round_trips_and_a_second_save_overwrites_rather_than_erroring() {
        let store = Store::open_in_memory().unwrap();
        store
            .set_setting("payment.confirmations_required", "5")
            .unwrap();
        assert_eq!(
            store
                .get_setting("payment.confirmations_required")
                .unwrap()
                .as_deref(),
            Some("5")
        );

        // The admin settings page's "Save" always writes every field it shows,
        // whether or not a row already exists for it - a second save of the same
        // key must update in place, not fail a UNIQUE constraint.
        store
            .set_setting("payment.confirmations_required", "8")
            .unwrap();
        assert_eq!(
            store
                .get_setting("payment.confirmations_required")
                .unwrap()
                .as_deref(),
            Some("8")
        );

        let all = store.list_settings().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(
            all.get("payment.confirmations_required")
                .map(String::as_str),
            Some("8")
        );
    }

    #[test]
    fn list_active_tenants_excludes_disabled_ones() {
        let store = Store::open_in_memory().unwrap();
        let a = new_tenant(&store);
        let b = new_tenant(&store);
        assert_eq!(store.count_tenants().unwrap(), 2);

        store.disable_tenant(&b.tenant.id, 2000).unwrap();
        let active = store.list_active_tenants().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, a.tenant.id);
        // Disabling doesn't delete the row - count_tenants includes it still.
        assert_eq!(store.count_tenants().unwrap(), 2);
    }

    /// A transaction that reads, then writes, must not lose its write to a
    /// commit another connection made in between: with a deferred BEGIN that
    /// write fails with `SQLITE_BUSY_SNAPSHOT`, which no busy timeout retries.
    /// `in_transaction` holds the write lock from its first statement, so the
    /// other writer waits instead and both writes land.
    #[test]
    fn a_transaction_holds_the_write_lock_before_its_first_read() {
        let path = std::env::temp_dir().join(format!("immediate_tx_{}.db", Uuid::new_v4()));
        let path = path.to_str().unwrap().to_owned();
        let store = Store::open_file(&path).unwrap();
        let other = Connection::open(&path).unwrap();
        other.busy_timeout(std::time::Duration::ZERO).unwrap();

        store
            .in_transaction(|s| -> Result<()> {
                let _ = s.get_setting("a")?;
                let competing = other.execute("INSERT INTO settings (key, value) VALUES ('b', '1')", []);
                assert!(
                    matches!(&competing, Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::DatabaseBusy),
                    "another writer must wait for this transaction, got {competing:?}"
                );
                s.set_setting("a", "1")
            })
            .unwrap();
        other
            .execute("INSERT INTO settings (key, value) VALUES ('b', '1')", [])
            .unwrap();
        assert_eq!(store.get_setting("a").unwrap().as_deref(), Some("1"));
        assert_eq!(store.get_setting("b").unwrap().as_deref(), Some("1"));
        drop(other);
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path}{suffix}"));
        }
    }

    #[test]
    fn pending_payment_recomputes_survive_reopening_and_track_real_changes() {
        let path = std::env::temp_dir().join(format!("pending_recomputes_{}.db", Uuid::new_v4()));
        let store = Store::open_file(path.to_str().unwrap()).unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);
        store
            .record_payment_match(&order.id, "tx", 0, 1, "[]", 1000, Some(1), None)
            .unwrap();
        drop(store);
        let store = Store::open_file(path.to_str().unwrap()).unwrap();
        assert_eq!(
            store
                .pending_payment_recomputes_page(monero::Network::Mainnet, "", 10_000)
                .unwrap(),
            vec![order.id.clone()]
        );
        assert!(store
            .pending_payment_recomputes_page(monero::Network::Stagenet, "", 10_000)
            .unwrap()
            .is_empty());
        store.clear_pending_payment_recompute(&order.id).unwrap();
        store
            .record_payment_match(&order.id, "tx", 0, 1, "[]", 1001, Some(1), None)
            .unwrap();
        assert!(
            store
                .pending_payment_recomputes_page(monero::Network::Mainnet, "", 10_000)
                .unwrap()
                .is_empty(),
            "duplicate sightings aren't new work"
        );
        store
            .update_payment_block_height(&order.id, "tx", 0, None)
            .unwrap();
        assert_eq!(
            store
                .pending_payment_recomputes_page(monero::Network::Mainnet, "", 10_000)
                .unwrap(),
            vec![order.id.clone()]
        );
        store.clear_pending_payment_recompute(&order.id).unwrap();
        store.void_payment(&order.id, "tx", 0, 1002).unwrap();
        assert_eq!(
            store
                .pending_payment_recomputes_page(monero::Network::Mainnet, "", 10_000)
                .unwrap(),
            vec![order.id.clone()]
        );
        store.clear_pending_payment_recompute(&order.id).unwrap();
        store.unvoid_payment(&order.id, "tx", 0).unwrap();
        assert_eq!(
            store
                .pending_payment_recomputes_page(monero::Network::Mainnet, "", 10_000)
                .unwrap(),
            vec![order.id.clone()]
        );
        // Reapply just the new DDL to pre-existing payments: an upgrade also
        // schedules recovery for writes made before durable tracking existed.
        store
            .conn
            .execute_batch(
                "DROP TRIGGER payment_insert_needs_recompute;
            DROP TRIGGER payment_update_needs_recompute; DROP TABLE pending_payment_recomputes;",
            )
            .unwrap();
        store
            .conn
            .execute_batch(include_str!(
                "../../migrations/0017_pending_payment_recomputes.sql"
            ))
            .unwrap();
        assert_eq!(
            store
                .pending_payment_recomputes_page(monero::Network::Mainnet, "", 10_000)
                .unwrap(),
            vec![order.id]
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn reopening_an_existing_database_file_does_not_reapply_migrations() {
        // Direct regression test for a real bug: `open_file` used to re-run the raw
        // `CREATE TABLE` migration SQL unconditionally, which crashed with "table
        // already exists" the moment the server was restarted against a database it
        // had already created - caught only by actually restarting the compiled
        // binary, since every other test in this file opens a fresh `:memory:` db
        // exactly once and would never exercise a second `open_file` call against
        // the same path.
        let path = std::env::temp_dir().join(format!("moneropay_test_{}.db", Uuid::new_v4()));
        let path_str = path.to_str().unwrap();

        let store = Store::open_file(path_str).unwrap();
        let created = new_tenant(&store);
        drop(store);

        // Reopening must not panic or error, and must see the data the first
        // connection wrote.
        let reopened = Store::open_file(path_str).unwrap();
        let refetched = reopened.get_tenant_by_id(&created.tenant.id).unwrap();
        assert_eq!(refetched.unwrap().id, created.tenant.id);
        drop(reopened);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn create_and_lookup_tenant_by_secret_token() {
        let store = Store::open_in_memory().unwrap();
        let created = new_tenant(&store);

        let by_secret = store
            .find_tenant_by_secret_token(&created.secret_token)
            .unwrap();
        assert_eq!(by_secret.unwrap().id, created.tenant.id);

        assert!(store
            .find_tenant_by_secret_token(&RawToken::presented("sk_wrong"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn rotated_secret_invalidates_the_old_token() {
        let store = Store::open_in_memory().unwrap();
        let created = new_tenant(&store);
        let new_secret = store.rotate_tenant_secret(&created.tenant.id).unwrap();

        assert!(store
            .find_tenant_by_secret_token(&created.secret_token)
            .unwrap()
            .is_none());
        assert!(store
            .find_tenant_by_secret_token(&new_secret)
            .unwrap()
            .is_some());
    }

    #[test]
    fn disabled_tenant_is_not_found_by_its_secret() {
        let store = Store::open_in_memory().unwrap();
        let created = new_tenant(&store);
        store.disable_tenant(&created.tenant.id, 2000).unwrap();

        assert!(store
            .find_tenant_by_secret_token(&created.secret_token)
            .unwrap()
            .is_none());
    }

    #[test]
    fn minor_index_allocation_starts_at_one_and_increments() {
        let store = Store::open_in_memory().unwrap();
        let created = new_tenant(&store);
        assert_eq!(store.allocate_minor_index(&created.tenant.id).unwrap(), 1);
        assert_eq!(store.allocate_minor_index(&created.tenant.id).unwrap(), 2);
        assert_eq!(store.allocate_minor_index(&created.tenant.id).unwrap(), 3);
    }

    #[test]
    fn concurrent_minor_index_allocation_never_duplicates() {
        // Direct test of docs/TESTING.md §6's top concurrency requirement: N
        // concurrent order-creation requests for one tenant must never allocate the
        // same minor_index. A Mutex-guarded Store is a legitimate, simple
        // realization of "single writer" for SQLite (which disallows concurrent
        // writers regardless) - this proves the allocation logic itself is correct
        // under real concurrent access, not just sequential calls.
        let store = Store::open_in_memory().unwrap();
        let created = new_tenant(&store);
        let tenant_id = created.tenant.id;
        let shared = store.into_shared();

        // Every thread starts before any is joined, so they race.
        #[expect(
            clippy::needless_collect,
            reason = "collecting starts every thread before the first join"
        )]
        let threads: Vec<_> = std::iter::repeat_with(|| {
            let shared = Arc::clone(&shared);
            let tenant_id = tenant_id.clone();
            std::thread::spawn(move || shared.lock().allocate_minor_index(&tenant_id).unwrap())
        })
        .take(50)
        .collect();

        let mut indices: Vec<u32> = threads.into_iter().map(|h| h.join().unwrap()).collect();
        indices.sort_unstable();
        let mut deduped = indices.clone();
        deduped.dedup();
        assert_eq!(
            indices.len(),
            deduped.len(),
            "duplicate minor_index allocated under concurrency"
        );
        assert_eq!(indices, (1..=50).collect::<Vec<_>>());
    }

    #[test]
    fn get_order_is_scoped_by_tenant_and_returns_none_across_tenants() {
        // Row-level IDOR test: tenant A's id plus tenant B's real order_id must
        // come back as None, not tenant B's order. See docs/DESIGN.md §10.1.
        let store = Store::open_in_memory().unwrap();
        let tenant_a = new_tenant(&store);
        let tenant_b = new_tenant(&store);
        let order_b = new_order(&store, tenant_b.tenant.id.as_str(), 1);

        assert!(store
            .get_order(&tenant_a.tenant.id, &order_b.id)
            .unwrap()
            .is_none());
        assert!(store
            .get_order(&tenant_b.tenant.id, &order_b.id)
            .unwrap()
            .is_some());
    }

    #[test]
    fn delete_webhook_is_scoped_by_tenant() {
        let store = Store::open_in_memory().unwrap();
        let tenant_a = new_tenant(&store);
        let tenant_b = new_tenant(&store);
        let webhook_b = store
            .create_webhook(
                &tenant_b.tenant.id,
                "https://b.example/hook",
                "{}",
                "secret",
                1000,
            )
            .unwrap();

        assert!(!store
            .delete_webhook(&tenant_a.tenant.id, &webhook_b.id)
            .unwrap());
        assert_eq!(store.list_webhooks(&tenant_b.tenant.id).unwrap().len(), 1);
        assert!(store
            .delete_webhook(&tenant_b.tenant.id, &webhook_b.id)
            .unwrap());
        assert_eq!(store.list_webhooks(&tenant_b.tenant.id).unwrap().len(), 0);
    }

    #[test]
    fn duplicate_payment_match_is_idempotent() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);

        let first = store
            .record_payment_match(&order.id, "txabc", 0, 100, "[]", 1500, None, None)
            .unwrap();
        let second = store
            .record_payment_match(&order.id, "txabc", 0, 100, "[]", 1600, None, None)
            .unwrap();

        assert!(first);
        assert!(
            !second,
            "re-reporting the same output must be a no-op, not a new row"
        );
        assert_eq!(store.get_all_payments(&order.id).unwrap().len(), 1);
    }

    #[test]
    fn recompute_status_reflects_new_payment_and_persists() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);
        assert_eq!(order.status, OrderStatus::Pending);

        store
            .record_payment_match(&order.id, "txabc", 0, 100, "[]", 1500, Some(50), None)
            .unwrap();
        let (old, new) = store.recompute_order_status(&order.id, 59, 1600).unwrap(); // 10 confirmations
        assert_eq!(old, OrderStatus::Pending);
        assert_eq!(new, OrderStatus::Paid);

        let refetched = store
            .get_order(&tenant.tenant.id, &order.id)
            .unwrap()
            .unwrap();
        assert_eq!(refetched.status, OrderStatus::Paid);
        assert_eq!(refetched.amount_received_piconero, 100);
        assert_eq!(refetched.confirmations, 10);
    }

    #[test]
    fn recompute_status_uses_the_per_order_override_instead_of_the_tenants_default() {
        // The tenant's own default is 10 (`new_tenant`'s own doc comment /
        // the confirmations_required default in `create_tenant`) - an order
        // with a `confirmations_required_override` of 2 must settle at 2
        // confirmations, not wait for the tenant's 10.
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = store
            .create_order(&NewOrder {
                idempotency_key: None,
                confirmations_required_override: Some(2),
                tenant_id: tenant.tenant.id,
                merchant_order_id: None,
                minor_index: 1,
                address: "sub_1".to_owned(),
                xmr_amount_piconero: 100,
                description: None,
                created_at: 1000,
                expires_at: 2000,
            })
            .unwrap();
        assert_eq!(order.confirmations_required_override, Some(2));

        store
            .record_payment_match(&order.id, "txabc", 0, 100, "[]", 1500, Some(50), None)
            .unwrap();
        // current_height 51, payment mined at 50 -> 2 confirmations.
        let (_, status) = store.recompute_order_status(&order.id, 51, 1600).unwrap();
        assert_eq!(
            status,
            OrderStatus::Paid,
            "2 confirmations must already be enough under a Some(2) override"
        );
    }

    #[test]
    fn recompute_status_falls_back_to_the_tenants_default_when_no_override_is_set() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1); // confirmations_required_override: None
        assert_eq!(order.confirmations_required_override, None);

        store
            .record_payment_match(&order.id, "txabc", 0, 100, "[]", 1500, Some(50), None)
            .unwrap();
        // Same 2-confirmation depth as the override test above, but with no
        // override this must still be short of the tenant's default of 10.
        let (_, status) = store.recompute_order_status(&order.id, 51, 1600).unwrap();
        assert_eq!(
            status,
            OrderStatus::Confirming,
            "with no override, the tenant's own default of 10 must still apply"
        );
    }

    #[test]
    fn voiding_one_of_two_payments_drops_status_to_partial_and_recomputes_total() {
        // The exact two-transaction scenario from design review, exercised through
        // the store: two payments summing to the expected amount reach `paid`;
        // voiding one drops the order to `partial` with the total recomputed from
        // the survivor alone.
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);

        store
            .record_payment_match(&order.id, "tx_a", 0, 60, "[\"ki_a\"]", 1500, Some(50), None)
            .unwrap();
        store
            .record_payment_match(&order.id, "tx_b", 0, 40, "[\"ki_b\"]", 1500, Some(50), None)
            .unwrap();
        let (_, paid) = store.recompute_order_status(&order.id, 59, 1600).unwrap();
        assert_eq!(paid, OrderStatus::Paid);

        assert!(store.void_payment(&order.id, "tx_a", 0, 1700).unwrap());
        assert!(store.mark_double_spend_detected(&order.id, 1700).unwrap());
        let (before, after) = store.recompute_order_status(&order.id, 59, 1700).unwrap();
        assert_eq!(before, OrderStatus::Paid);
        assert_eq!(after, OrderStatus::Partial);

        let refetched = store
            .get_order(&tenant.tenant.id, &order.id)
            .unwrap()
            .unwrap();
        assert_eq!(refetched.amount_received_piconero, 40);
        assert!(refetched.double_spend_detected_at.is_some());
    }

    #[test]
    fn double_spend_flag_is_independent_of_status_when_redundant_payment_covers_order() {
        // The other half of the same scenario: voiding one payment when a third,
        // redundant one still covers the order must leave status at `paid` while
        // still recording that a double-spend occurred.
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);

        store
            .record_payment_match(&order.id, "tx_a", 0, 60, "[\"ki_a\"]", 1500, Some(50), None)
            .unwrap();
        store
            .record_payment_match(
                &order.id,
                "tx_c",
                0,
                100,
                "[\"ki_c\"]",
                1500,
                Some(50),
                None,
            )
            .unwrap();
        let (_, before_void) = store.recompute_order_status(&order.id, 59, 1600).unwrap();
        assert_eq!(before_void, OrderStatus::Overpaid); // 160 total against an expected 100

        store.void_payment(&order.id, "tx_a", 0, 1700).unwrap();
        store.mark_double_spend_detected(&order.id, 1700).unwrap();
        let (_, after) = store.recompute_order_status(&order.id, 59, 1700).unwrap();

        // tx_c alone (100) exactly covers the expected amount - still `paid`, not
        // downgraded, even though a double-spend genuinely occurred on this order.
        assert_eq!(after, OrderStatus::Paid);
        let refetched = store
            .get_order(&tenant.tenant.id, &order.id)
            .unwrap()
            .unwrap();
        assert!(refetched.double_spend_detected_at.is_some());
    }

    fn drain_changes(
        receiver: &mut tokio::sync::broadcast::Receiver<OrderChange>,
    ) -> Vec<OrderChange> {
        let mut changes = Vec::new();
        while let Ok(change) = receiver.try_recv() {
            changes.push(change);
        }
        changes
    }

    #[test]
    fn order_changes_are_published_only_when_something_visible_changed() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);
        let mut changes = store.subscribe_order_changes();
        let expected = vec![OrderChange {
            tenant_id: tenant.tenant.id.clone(),
            order_id: order.id.clone(),
        }];

        // Nothing paid, nothing expired: a recompute that changes nothing is silent.
        store
            .recompute_order_status(&order.id, 100, order.created_at)
            .unwrap();
        assert!(drain_changes(&mut changes).is_empty());

        // First sighting in the mempool, then the same sighting again (the ~1s mempool poll).
        store
            .record_payment_match(&order.id, "tx1", 0, 1, "[]", order.created_at, None, None)
            .unwrap();
        assert_eq!(drain_changes(&mut changes), expected);
        store
            .record_payment_match(&order.id, "tx1", 0, 1, "[]", order.created_at, None, None)
            .unwrap();
        assert!(drain_changes(&mut changes).is_empty());
        // Mined: the height is new information.
        store
            .record_payment_match(
                &order.id,
                "tx1",
                0,
                1,
                "[]",
                order.created_at,
                Some(90),
                None,
            )
            .unwrap();
        assert_eq!(drain_changes(&mut changes), expected);

        store
            .recompute_order_status(&order.id, 100, order.created_at)
            .unwrap();
        assert_eq!(drain_changes(&mut changes), expected);
        store
            .recompute_order_status(&order.id, 100, order.created_at)
            .unwrap();
        assert!(drain_changes(&mut changes).is_empty());

        assert!(store
            .set_refund_address(&tenant.tenant.id, &order.id, "refund")
            .unwrap());
        assert_eq!(drain_changes(&mut changes), expected);
        assert!(store.mark_double_spend_detected(&order.id, 1000).unwrap());
        assert_eq!(drain_changes(&mut changes), expected);
        assert!(!store.mark_double_spend_detected(&order.id, 2000).unwrap());
        assert!(drain_changes(&mut changes).is_empty());
    }

    #[test]
    fn order_changes_inside_a_transaction_publish_once_after_commit_and_never_on_rollback() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);
        let mut changes = store.subscribe_order_changes();

        let rolled_back: std::result::Result<(), StoreError> = store.in_transaction(|store| {
            store.set_refund_address(&tenant.tenant.id, &order.id, "refund")?;
            Err(StoreError::NotFound)
        });
        assert!(rolled_back.is_err());
        assert!(
            drain_changes(&mut changes).is_empty(),
            "a rolled-back write must announce nothing"
        );

        store
            .in_transaction(|store| -> Result<()> {
                store.set_refund_address(&tenant.tenant.id, &order.id, "refund")?;
                store.mark_double_spend_detected(&order.id, 1000)?;
                assert!(
                    drain_changes(&mut changes).is_empty(),
                    "nothing is published before the commit"
                );
                Ok(())
            })
            .unwrap();
        assert_eq!(
            drain_changes(&mut changes),
            vec![OrderChange {
                tenant_id: tenant.tenant.id,
                order_id: order.id
            }],
            "one change per order per transaction"
        );
    }

    #[test]
    fn mark_double_spend_detected_is_sticky_first_occurrence_only() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);

        assert!(store.mark_double_spend_detected(&order.id, 1000).unwrap());
        assert!(
            !store.mark_double_spend_detected(&order.id, 2000).unwrap(),
            "must not overwrite the first timestamp"
        );

        let refetched = store
            .get_order(&tenant.tenant.id, &order.id)
            .unwrap()
            .unwrap();
        assert_eq!(refetched.double_spend_detected_at, Some(1000));
    }

    #[test]
    fn update_tenant_config_only_touches_included_fields() {
        let store = Store::open_in_memory().unwrap();
        let created = new_tenant(&store);

        store
            .update_tenant_config(
                &created.tenant.id,
                &TenantConfigPatch {
                    confirmations_required: Some(3),
                    ..Default::default()
                },
            )
            .unwrap();
        let refetched = store.get_tenant_by_id(&created.tenant.id).unwrap().unwrap();
        assert_eq!(refetched.confirmations_required, 3);
        assert_eq!(
            refetched.order_expiry_seconds,
            created.tenant.order_expiry_seconds
        ); // untouched

        // Native 0-conf: a tenant's own default can be patched down to zero directly,
        // no separate ceiling/`_set` flag machinery needed.
        store
            .update_tenant_config(
                &created.tenant.id,
                &TenantConfigPatch {
                    confirmations_required: Some(0),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            store
                .get_tenant_by_id(&created.tenant.id)
                .unwrap()
                .unwrap()
                .confirmations_required,
            0
        );
    }

    #[test]
    fn list_orders_filters_by_status_and_paginates_newest_first() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        for i in 1..=3u32 {
            new_order(&store, tenant.tenant.id.as_str(), i);
        }
        let all = store
            .list_orders(&tenant.tenant.id, None, 10, None)
            .unwrap();
        assert_eq!(all.len(), 3);
        assert!(all[0].created_at >= all[1].created_at); // newest first (all equal here, but ordering must not error)

        let pending_only = store
            .list_orders(&tenant.tenant.id, Some(OrderStatus::Pending), 10, None)
            .unwrap();
        assert_eq!(pending_only.len(), 3);
        let paid_only = store
            .list_orders(&tenant.tenant.id, Some(OrderStatus::Paid), 10, None)
            .unwrap();
        assert_eq!(paid_only.len(), 0);

        let page = store.list_orders(&tenant.tenant.id, None, 2, None).unwrap();
        assert_eq!(page.len(), 2);

        // All three share a `created_at` second: paging on by the last
        // row's (created_at, id) reaches the third, where created_at alone
        // would have skipped it.
        let last = page.last().unwrap();
        let rest = store
            .list_orders(
                &tenant.tenant.id,
                None,
                2,
                Some((last.created_at, last.id.as_str())),
            )
            .unwrap();
        assert_eq!(rest.len(), 1, "the one order left after the page boundary");
        assert!(!page.iter().any(|o| o.id == rest[0].id));
        let ids: std::collections::HashSet<_> = page
            .iter()
            .chain(rest.iter())
            .map(|o| o.id.clone())
            .collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn a_tenant_config_patch_applies_whole_and_knows_an_unknown_tenant() {
        let store = Store::open_in_memory().unwrap();
        let created = new_tenant(&store);
        assert!(store
            .update_tenant_config(
                &created.tenant.id,
                &TenantConfigPatch {
                    confirmations_required: Some(3),
                    order_expiry_seconds: Some(900),
                },
            )
            .unwrap());
        let tenant = store.get_tenant_by_id(&created.tenant.id).unwrap().unwrap();
        assert_eq!(
            (tenant.confirmations_required, tenant.order_expiry_seconds),
            (3, 900)
        );
        assert!(
            !store
                .update_tenant_config(
                    &TenantId::new("nobody".to_owned()),
                    &TenantConfigPatch {
                        confirmations_required: Some(3),
                        order_expiry_seconds: None,
                    },
                )
                .unwrap(),
            "no such tenant"
        );
    }

    /// `Order::in_scan_window` on rows as the store writes them agrees with
    /// the SQL the scan loop selects its window with (`scan_window_orders`):
    /// non-terminal, or closed within the grace period.
    #[test]
    fn an_order_read_back_knows_whether_the_scanner_still_examines_it() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let read = |id: &OrderId| store.get_order_by_id(id).unwrap().unwrap();
        let selected = |id: &OrderId, now: i64, grace: i64| {
            store
                .conn
                .query_row(
                    &format!(
                        "SELECT EXISTS(SELECT 1 FROM ({}) WHERE id = :order)",
                        scan_window_orders("o.tenant_id = o.tenant_id")
                    ),
                    rusqlite::named_params! { ":order": id, ":since_minus_grace": now - grace },
                    |row| row.get::<_, bool>(0),
                )
                .unwrap()
        };

        let pending_order = new_order(&store, tenant.tenant.id.as_str(), 1); // expires_at = 2000, status defaults to pending
        assert!(read(&pending_order.id).in_scan_window(2000, 0));
        assert!(selected(&pending_order.id, 2000, 0));

        let expired_in_grace = new_order(&store, tenant.tenant.id.as_str(), 2);
        store.conn.execute("UPDATE orders SET status = 'expired', closed_at_utc = expires_at_utc WHERE id = ?1", params![expired_in_grace.id]).unwrap();
        assert_eq!(read(&expired_in_grace.id).closed_at, Some(2000));
        assert!(read(&expired_in_grace.id).in_scan_window(2500, 600));
        assert!(selected(&expired_in_grace.id, 2500, 600));

        let expired_past_grace = new_order(&store, tenant.tenant.id.as_str(), 3);
        store.conn.execute("UPDATE orders SET status = 'expired', closed_at_utc = expires_at_utc WHERE id = ?1", params![expired_past_grace.id]).unwrap();
        assert!(!read(&expired_past_grace.id).in_scan_window(2601, 600));
        assert!(!selected(&expired_past_grace.id, 2601, 600));
    }

    #[test]
    fn webhook_delivery_queue_lifecycle() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);
        let webhook = store
            .create_webhook(
                &tenant.tenant.id,
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();

        let id = store
            .enqueue_webhook_delivery(&webhook.id, &order.id, "order.paid", "{\"a\":1}", 1000)
            .unwrap();

        // Not due yet if next_attempt_at is in the future.
        assert!(store
            .due_webhook_deliveries_for_test(999, 10)
            .unwrap()
            .is_empty());
        let due = store.due_webhook_deliveries_for_test(1000, 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].delivery_id, id);
        assert_eq!(due[0].url, "https://merchant.example/hook");
        assert_eq!(due[0].signing_secret.expose(), "whsec_x");
        assert_eq!(due[0].attempt_count, 0);

        // A failed attempt reschedules and increments attempt_count; it stays due
        // once the new next_attempt_at has passed.
        store
            .schedule_webhook_retry(id, 2000, Some(500), Some("server error"), 1000)
            .unwrap();
        assert!(store
            .due_webhook_deliveries_for_test(1500, 10)
            .unwrap()
            .is_empty());
        let due = store.due_webhook_deliveries_for_test(2000, 10).unwrap();
        assert_eq!(due[0].attempt_count, 1);

        // A successful delivery removes it from the due set permanently, even if
        // asked about at a much later time.
        store.mark_webhook_delivered(id, 200, 2000).unwrap();
        assert!(store
            .due_webhook_deliveries_for_test(999_999, 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn disabling_a_webhook_removes_its_deliveries_from_the_due_set() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);
        let webhook = store
            .create_webhook(
                &tenant.tenant.id,
                "https://merchant.example/hook",
                "{}",
                "whsec_x",
                1000,
            )
            .unwrap();
        store
            .enqueue_webhook_delivery(&webhook.id, &order.id, "order.paid", "{}", 1000)
            .unwrap();

        store
            .conn
            .execute(
                "UPDATE webhooks SET enabled = 0 WHERE id = ?1",
                params![webhook.id],
            )
            .unwrap();
        assert!(store
            .due_webhook_deliveries_for_test(1000, 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn max_scanned_height_reflects_the_highest_recorded_block() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(
            store.max_scanned_height(monero::Network::Mainnet).unwrap(),
            None
        );
        store
            .set_scanned_block(monero::Network::Mainnet, 100, "h100")
            .unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 105, "h105")
            .unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 102, "h102")
            .unwrap();
        assert_eq!(
            store.max_scanned_height(monero::Network::Mainnet).unwrap(),
            Some(105)
        );
    }

    #[test]
    fn get_order_tenant_id_is_unscoped_by_design() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);
        assert_eq!(
            store.get_order_tenant_id(&order.id).unwrap(),
            Some(tenant.tenant.id)
        );
        assert_eq!(
            store
                .get_order_tenant_id(&OrderId::new("pay_nonexistent"))
                .unwrap(),
            None
        );
    }

    #[test]
    fn scanned_blocks_round_trip_and_prune() {
        let store = Store::open_in_memory().unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 100, "hash100")
            .unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 101, "hash101")
            .unwrap();
        assert_eq!(
            store
                .get_scanned_block_hash(monero::Network::Mainnet, 100)
                .unwrap(),
            Some("hash100".to_owned())
        );

        // Reorg overwrite at the same height.
        store
            .set_scanned_block(monero::Network::Mainnet, 100, "hash100_v2")
            .unwrap();
        assert_eq!(
            store
                .get_scanned_block_hash(monero::Network::Mainnet, 100)
                .unwrap(),
            Some("hash100_v2".to_owned())
        );

        store
            .prune_scanned_blocks_below(monero::Network::Mainnet, 101)
            .unwrap();
        assert_eq!(
            store
                .get_scanned_block_hash(monero::Network::Mainnet, 100)
                .unwrap(),
            None
        );
        assert!(store
            .get_scanned_block_hash(monero::Network::Mainnet, 101)
            .unwrap()
            .is_some());
    }

    #[test]
    fn scanned_blocks_are_fully_isolated_per_network() {
        // The entire point of the network-scoped rewrite: block heights are only
        // comparable within one chain, so two networks must be able to record
        // *different* hashes at the *same* height without colliding, and querying
        // one network must never see the other's data.
        let store = Store::open_in_memory().unwrap();
        store
            .set_scanned_block(monero::Network::Mainnet, 100, "mainnet_hash_100")
            .unwrap();
        store
            .set_scanned_block(monero::Network::Stagenet, 100, "stagenet_hash_100")
            .unwrap();

        assert_eq!(
            store
                .get_scanned_block_hash(monero::Network::Mainnet, 100)
                .unwrap(),
            Some("mainnet_hash_100".to_owned())
        );
        assert_eq!(
            store
                .get_scanned_block_hash(monero::Network::Stagenet, 100)
                .unwrap(),
            Some("stagenet_hash_100".to_owned())
        );
        assert_eq!(
            store
                .get_scanned_block_hash(monero::Network::Testnet, 100)
                .unwrap(),
            None,
            "a third, never-written network must see nothing"
        );

        // Advancing one network's tip must not affect the other's.
        store
            .set_scanned_block(monero::Network::Mainnet, 105, "mainnet_hash_105")
            .unwrap();
        assert_eq!(
            store.max_scanned_height(monero::Network::Mainnet).unwrap(),
            Some(105)
        );
        assert_eq!(
            store.max_scanned_height(monero::Network::Stagenet).unwrap(),
            Some(100)
        );

        // Pruning one network's old blocks must not touch the other's.
        store
            .prune_scanned_blocks_below(monero::Network::Mainnet, 105)
            .unwrap();
        assert_eq!(
            store
                .get_scanned_block_hash(monero::Network::Mainnet, 100)
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .get_scanned_block_hash(monero::Network::Stagenet, 100)
                .unwrap(),
            Some("stagenet_hash_100".to_owned()),
            "pruning mainnet must not prune stagenet's row at the same height"
        );
    }

    #[test]
    fn the_unconfirmed_payments_page_holds_only_live_mempool_only_rows_of_one_network() {
        // The sweep that catches a plain (reorg-free) zero-conf double-spend runs off
        // this query, so what it must *not* return matters as much as what it does: a
        // mined payment (already anchored to a block), an already-voided one (nothing
        // left to decide), and another network's payment (a daemon must never be
        // asked about a chain it doesn't serve).
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let mempool_order = new_order(&store, tenant.tenant.id.as_str(), 1);
        let mined_order = new_order(&store, tenant.tenant.id.as_str(), 2);
        let voided_order = new_order(&store, tenant.tenant.id.as_str(), 3);
        store
            .record_payment_match(&mempool_order.id, "tx_pool", 0, 100, "[]", 1500, None, None)
            .unwrap();
        store
            .record_payment_match(
                &mined_order.id,
                "tx_mined",
                0,
                100,
                "[]",
                1500,
                Some(50),
                None,
            )
            .unwrap();
        store
            .record_payment_match(
                &voided_order.id,
                "tx_voided",
                0,
                100,
                "[]",
                1500,
                None,
                None,
            )
            .unwrap();
        store
            .void_payment(&voided_order.id, "tx_voided", 0, 1600)
            .unwrap();

        let stagenet_tenant = store
            .create_tenant(
                &NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "5stagenet".into(),
                    network: "stagenet".into(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();
        let stagenet_order = new_order(&store, stagenet_tenant.tenant.id.as_str(), 1);
        store
            .record_payment_match(
                &stagenet_order.id,
                "tx_stagenet_pool",
                0,
                100,
                "[]",
                1500,
                None,
                None,
            )
            .unwrap();

        let found = store
            .unconfirmed_payments_page(monero::Network::Mainnet, 0, 10)
            .unwrap();
        assert_eq!(
            found.len(),
            1,
            "only the live mempool-only mainnet row: {found:?}"
        );
        assert_eq!(found[0].1.txid, "tx_pool");
        assert!(store
            .unconfirmed_payments_page(monero::Network::Mainnet, found[0].0, 10)
            .unwrap()
            .is_empty());

        let stagenet_found = store
            .unconfirmed_payments_page(monero::Network::Stagenet, 0, 10)
            .unwrap();
        assert_eq!(stagenet_found.len(), 1);
        assert_eq!(stagenet_found[0].1.txid, "tx_stagenet_pool");
    }

    #[test]
    fn unconfirmed_payment_pages_rotate_without_repeating_a_row() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        for index in 1..=3 {
            let order = new_order(&store, tenant.tenant.id.as_str(), index);
            store
                .record_payment_match(
                    &order.id,
                    &format!("tx_{index}"),
                    0,
                    100,
                    "[]",
                    1500,
                    None,
                    None,
                )
                .unwrap();
        }
        let first = store
            .unconfirmed_payments_page(monero::Network::Mainnet, 0, 2)
            .unwrap();
        assert_eq!(
            first
                .iter()
                .map(|(_, payment)| payment.txid.as_str())
                .collect::<Vec<_>>(),
            vec!["tx_1", "tx_2"]
        );
        let second = store
            .unconfirmed_payments_page(monero::Network::Mainnet, first.last().unwrap().0, 2)
            .unwrap();
        assert_eq!(
            second
                .iter()
                .map(|(_, payment)| payment.txid.as_str())
                .collect::<Vec<_>>(),
            vec!["tx_3"]
        );
        assert!(store
            .unconfirmed_payments_page(monero::Network::Mainnet, second.last().unwrap().0, 2)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_mempool_first_payment_gains_its_block_height_once_the_tx_is_mined() {
        // Direct regression test for the single most consequential bug in this file:
        // `ON CONFLICT DO NOTHING` meant the block scan's `Some(height)` was thrown
        // away because the mempool poll had already inserted the same output with
        // `block_height = NULL` seconds earlier. Since the scanner polls the mempool
        // roughly every second, essentially *every* real payment arrives in that
        // order, so essentially every real payment was permanently stuck at zero
        // confirmations - never advancing past `unconfirmed`, and invisible to
        // reorg reconciliation, which collects mined payments by their height.
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);

        assert!(store
            .record_payment_match(&order.id, "txabc", 0, 100, "[]", 1500, None, None)
            .unwrap());
        assert_eq!(
            store.get_all_payments(&order.id).unwrap()[0].block_height,
            None
        );

        // Same output, now seen inside a block - the row must learn its height.
        assert!(!store
            .record_payment_match(&order.id, "txabc", 0, 100, "[]", 1600, Some(50), None)
            .unwrap());
        let payments = store.get_all_payments(&order.id).unwrap();
        assert_eq!(
            payments.len(),
            1,
            "still exactly one row - this is an update, not a second payment"
        );
        assert_eq!(payments[0].block_height, Some(50));

        // And with a height known it is a mined payment: no longer one the
        // vanished-payment sweep looks for in the pool.
        assert!(store
            .unconfirmed_payments_page(monero::Network::Mainnet, 0, 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_late_mempool_resighting_never_nulls_out_an_already_known_block_height() {
        // The other direction of the same upsert. A node can still list a
        // just-mined transaction in its pool for a short while, so the mempool scan
        // legitimately re-reports it with `block_height = None` *after* the block
        // scan recorded a real height. COALESCE is what stops that from walking the
        // payment's confirmations back to zero on every subsequent tick.
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);

        store
            .record_payment_match(&order.id, "txabc", 0, 100, "[]", 1500, Some(50), None)
            .unwrap();
        store
            .record_payment_match(&order.id, "txabc", 0, 100, "[]", 1600, None, None)
            .unwrap();

        assert_eq!(
            store.get_all_payments(&order.id).unwrap()[0].block_height,
            Some(50)
        );
    }

    #[test]
    fn a_duplicate_insert_cannot_resurrect_a_voided_payment() {
        // The `WHERE voided_at IS NULL` guard on the upsert. A voided payment is a
        // proven double-spend; the scanner will keep seeing that transaction in the
        // mempool for as long as it propagates, and each re-sighting reaches the
        // same upsert. That must not quietly revise the row it decided against.
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);

        store
            .record_payment_match(&order.id, "txabc", 0, 100, "[]", 1500, None, None)
            .unwrap();
        assert!(store.void_payment(&order.id, "txabc", 0, 1600).unwrap());

        store
            .record_payment_match(&order.id, "txabc", 0, 100, "[]", 1700, Some(50), None)
            .unwrap();
        let payments = store.get_all_payments(&order.id).unwrap();
        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].voided_at, Some(1600), "still voided");
        assert_eq!(
            payments[0].block_height, None,
            "and the guarded update must not have run either"
        );
    }

    #[test]
    fn two_orders_can_each_record_the_same_transaction_output() {
        // Migration 0004. `UNIQUE(txid, output_index)` was global, so when two
        // tenants share a view key - a second instance pointed at one wallet, a
        // staging tenant on production key material - both scanners match the same
        // output and the second one's payment vanished into `DO NOTHING`. Scoping
        // the constraint by `order_id` lets each order keep its own row while
        // mempool-poll idempotency within one order is unchanged.
        let store = Store::open_in_memory().unwrap();
        let tenant_a = new_tenant(&store);
        let tenant_b = new_tenant(&store);
        let order_a = new_order(&store, tenant_a.tenant.id.as_str(), 1);
        let order_b = new_order(&store, tenant_b.tenant.id.as_str(), 1);

        assert!(store
            .record_payment_match(&order_a.id, "shared_tx", 0, 100, "[]", 1500, Some(50), None)
            .unwrap());
        assert!(store
            .record_payment_match(&order_b.id, "shared_tx", 0, 100, "[]", 1500, Some(50), None)
            .unwrap());

        assert_eq!(store.get_all_payments(&order_a.id).unwrap().len(), 1);
        assert_eq!(store.get_all_payments(&order_b.id).unwrap().len(), 1);

        // Within one order it is still an idempotent no-op, exactly as before.
        assert!(!store
            .record_payment_match(&order_a.id, "shared_tx", 0, 100, "[]", 1600, Some(50), None)
            .unwrap());
        assert_eq!(store.get_all_payments(&order_a.id).unwrap().len(), 1);
    }

    #[test]
    fn unvoid_payment_restores_a_row_without_clearing_the_double_spend_flag() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);
        store
            .record_payment_match(&order.id, "tx_a", 0, 100, "[]", 1500, Some(50), None)
            .unwrap();
        store.void_payment(&order.id, "tx_a", 0, 1600).unwrap();
        store.mark_double_spend_detected(&order.id, 1600).unwrap();

        assert!(store.unvoid_payment(&order.id, "tx_a", 0).unwrap());
        assert!(store.get_all_payments(&order.id).unwrap()[0]
            .voided_at
            .is_none());
        assert!(
            !store.unvoid_payment(&order.id, "tx_a", 0).unwrap(),
            "idempotent - already un-voided"
        );

        // Sticky by design (see the schema comment): the incident happened, whether
        // or not the payment ultimately stood.
        let refetched = store
            .get_order(&tenant.tenant.id, &order.id)
            .unwrap()
            .unwrap();
        assert_eq!(refetched.double_spend_detected_at, Some(1600));
    }

    #[test]
    fn claiming_a_minor_index_and_creating_its_order_is_all_or_nothing() {
        // The atomicity that closes the order-creation race: `next_minor_index` is
        // what the scanner reads to decide which subaddresses it scans, so it must
        // never be advanced except together with the order row that explains the
        // advance. Proven here by forcing the order insert to fail (a duplicate
        // `(tenant_id, minor_index)`) and asserting the counter did not move -
        // under the old "allocate, await a derivation, then insert" sequence the
        // counter advanced first and stayed advanced regardless.
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let tenant_id = tenant.tenant.id;

        let first = store.peek_next_minor_index(&tenant_id).unwrap();
        let order = store
            .create_order_claiming_minor_index(
                first,
                &NewOrder {
                    idempotency_key: None,
                    confirmations_required_override: None,
                    tenant_id: tenant_id.clone(),
                    merchant_order_id: None,
                    minor_index: first,
                    address: "sub_1".into(),
                    xmr_amount_piconero: 100,
                    description: None,
                    created_at: 1000,
                    expires_at: 2000,
                },
            )
            .unwrap()
            .expect("the first claim of a fresh index must succeed");
        assert_eq!(order.minor_index, first);
        assert_eq!(store.peek_next_minor_index(&tenant_id).unwrap(), first + 1);

        // A claim of an index the counter has already moved past changes nothing at
        // all - the caller re-derives against the new index rather than burning one.
        let stale = store
            .create_order_claiming_minor_index(
                first,
                &NewOrder {
                    idempotency_key: None,
                    confirmations_required_override: None,
                    tenant_id: tenant_id.clone(),
                    merchant_order_id: None,
                    minor_index: first,
                    address: "sub_1_again".into(),
                    xmr_amount_piconero: 100,
                    description: None,
                    created_at: 1000,
                    expires_at: 2000,
                },
            )
            .unwrap();
        assert!(stale.is_none());
        assert_eq!(
            store.peek_next_minor_index(&tenant_id).unwrap(),
            first + 1,
            "a losing racer must not burn an index"
        );

        // A *failing* insert (this minor_index is already taken, violating
        // UNIQUE(tenant_id, minor_index)) must roll the counter bump back with it.
        let next = store.peek_next_minor_index(&tenant_id).unwrap();
        let failed = store.create_order_claiming_minor_index(
            next,
            &NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: tenant_id.clone(),
                merchant_order_id: None,
                minor_index: first, // deliberately the already-used index, not `next`
                address: "sub_collision".into(),
                xmr_amount_piconero: 100,
                description: None,
                created_at: 1000,
                expires_at: 2000,
            },
        );
        failed.unwrap_err();
        assert_eq!(
            store.peek_next_minor_index(&tenant_id).unwrap(),
            next,
            "a failed order insert must not leave the counter advanced past an order that does not exist"
        );
    }

    #[test]
    fn foreign_keys_are_enforced_on_a_reopened_database_file() {
        // `PRAGMA foreign_keys` is a per-*connection* setting that SQLite defaults
        // to OFF, and it lived in `0001_init.sql`. Migrations run once, so every
        // boot after the very first one against an existing file skipped that
        // statement entirely and ran with foreign key enforcement silently
        // disabled - the exact opposite of what the schema documents. Asserting it
        // on a *reopened* file is the whole point: a fresh database passes either
        // way, which is why this went unnoticed.
        let path = std::env::temp_dir().join(format!("moneropay_fk_test_{}.db", Uuid::new_v4()));
        let path_str = path.to_str().unwrap();

        let store = Store::open_file(path_str).unwrap();
        drop(store); // the connection that ran the migrations is gone

        let reopened = Store::open_file(path_str).unwrap();
        let err = reopened
            .create_order(&NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: "tn_does_not_exist".into(),
                merchant_order_id: None,
                minor_index: 1,
                address: "sub_1".into(),
                xmr_amount_piconero: 1,
                description: None,
                created_at: 1000,
                expires_at: 2000,
            })
            .unwrap_err();
        assert!(
            matches!(
                err,
                StoreError::Sqlite(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error { code: rusqlite::ErrorCode::ConstraintViolation, extended_code: _ },
                    _
                ))
            ),
            "an order referencing a nonexistent tenant must be rejected on a reopened connection, got: {err:?}"
        );

        drop(reopened);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn migration_0004_rebuilds_order_payments_without_losing_existing_rows() {
        // 0004 changes a constraint, which SQLite can only do by rebuilding the
        // table and copying every row across - the one kind of migration that can
        // silently destroy live payment history if the column list drifts. Driven
        // here the way a real upgrade runs it: bring a database up to the *previous*
        // schema version, put real data in it, then apply the rest.
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();
        shared::migrations::apply(&conn, &MIGRATIONS[..3]).unwrap();
        let store = Store::from_connection(conn);

        // Inserted directly via raw SQL, not `new_tenant`/`Store::create_tenant`:
        // those build against the *current* schema (as of migration 9,
        // `created_at_utc`), which this pre-migration-4 snapshot (`created_at`,
        // not yet renamed) doesn't have. Same reasoning as the orders/
        // order_payments inserts just below - reproduce the pre-upgrade row
        // shape directly rather than going through today's API.
        let tenant_id = "tn_before_upgrade";
        store
            .execute_raw_for_test(
                "INSERT INTO tenants (id, public_key, secret_token_hash, key_custody_backend,
                    sealed_key_material, primary_address, allowed_origins, created_at)
                 VALUES ('tn_before_upgrade', 'pk_before_upgrade', 'hash_before_upgrade', 'plain',
                    x'00', '4addr', '[]', 1000)",
            )
            .unwrap();
        // Inserted directly via raw SQL, not `new_order`/`create_order`: those
        // now build an XMR-only `INSERT` (`docs/fx_refactor.md` Phase 3), which
        // this pre-migration-5 schema (fiat columns still `NOT NULL`) would
        // reject. A real pre-upgrade database still has real fiat values in
        // every row, so this reproduces that shape directly instead.
        let order_id = "pay_before_upgrade";
        store
            .execute_raw_for_test(&format!(
                "INSERT INTO orders (id, tenant_id, minor_index, address, fiat_currency, fiat_amount,
                    exchange_rate, xmr_amount_piconero, created_at, expires_at, updated_at)
                 VALUES ('{order_id}', '{tenant_id}', 1, 'sub_1', 'USD', '25.00', '0.0067', 100, 1000, 2000, 1000)",
            ))
            .unwrap();
        // Not fetched back via `get_order_by_id` here - `row_to_order` now selects
        // columns (`first_scanned_height`/`last_scanned_height`, Phase 5.1) that
        // don't exist yet at this pre-migration-8 schema version; `order_id` is
        // already the exact id just inserted, so there's nothing this would add.
        //
        // Inserted directly rather than through `record_payment_match`, whose
        // conflict target names a constraint this schema version doesn't have yet.
        store
            .execute_raw_for_test(&format!(
                "INSERT INTO order_payments (order_id, txid, output_index, amount_piconero,
                    key_images_json, first_seen_at, block_height, voided_at)
                 VALUES ('{order_id}', 'tx_from_before_the_upgrade', 2, 4242, '[\"ki_a\"]', 1500, 77, 1600)"
            ))
            .unwrap();

        shared::migrations::apply(&store.conn, MIGRATIONS).unwrap();

        let payments = store
            .get_all_payments(&OrderId::new(order_id.to_owned()))
            .unwrap();
        assert_eq!(
            payments.len(),
            1,
            "the pre-upgrade payment must survive the table rebuild"
        );
        assert_eq!(payments[0].txid, "tx_from_before_the_upgrade");
        assert_eq!(payments[0].output_index, 2);
        assert_eq!(payments[0].amount_piconero, 4242);
        assert_eq!(payments[0].key_images_json, "[\"ki_a\"]");
        assert_eq!(payments[0].first_seen_at, 1500);
        assert_eq!(
            payments[0].block_height,
            Some(77),
            "every column, not just the ones the new constraint names"
        );
        assert_eq!(payments[0].voided_at, Some(1600));

        // And the new constraint is genuinely in force afterwards.
        let other_tenant = new_tenant(&store);
        let other_order = new_order(&store, other_tenant.tenant.id.as_str(), 1);
        assert!(store
            .record_payment_match(
                &other_order.id,
                "tx_from_before_the_upgrade",
                2,
                100,
                "[]",
                1700,
                Some(77),
                None
            )
            .unwrap());

        // SQLite drops a table's indexes with the table, and does *not* re-derive
        // them for the replacement - so a rebuild migration silently un-indexes
        // whatever it forgets to recreate, with nothing failing and only a
        // progressively slower query to show for it. Assert the full expected set
        // rather than just the one 0004 remembered to recreate.
        let indexes: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='order_payments' AND sql IS NOT NULL ORDER BY name")
                .unwrap();
            let rows = stmt
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            rows
        };
        // Later migrations add their own indexes (0019's reorg collection
        // streams); the one that existed before the rebuild must survive it.
        assert_eq!(
            indexes,
            vec![
                "order_payments_confirmed_height_idx".to_owned(),
                "order_payments_order_idx".to_owned(),
                "order_payments_output_key_idx".to_owned(),
                "order_payments_unconfirmed_idx".to_owned(),
                "order_payments_voided_idx".to_owned(),
            ],
            "every explicitly-declared index that existed on order_payments before the rebuild must exist after it"
        );
        // ...plus the implicit index backing the new constraint, which is what makes
        // the upsert's `ON CONFLICT(order_id, txid, output_index)` target resolvable
        // at all.
        let has_unique_index: bool = store
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_index_list('order_payments') WHERE origin='u' AND \"unique\"=1)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            has_unique_index,
            "the UNIQUE(order_id, txid, output_index) constraint must survive as a real index"
        );
    }

    #[test]
    fn migration_0014_drops_the_tenant_allowed_origins_column_and_keeps_the_tenant() {
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();
        shared::migrations::apply(&conn, &MIGRATIONS[..13]).unwrap();
        conn.execute_batch(
            "INSERT INTO tenants (id, public_key, secret_token_hash, key_custody_backend,
                sealed_key_material, primary_address, allowed_origins, created_at_utc)
             VALUES ('old', 'pk_old', 'hash_old', 'plain', x'00', '4addr', '[\"https://a.example\"]', 1000);",
        )
        .unwrap();

        shared::migrations::apply(&conn, MIGRATIONS).unwrap();
        let columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('tenants')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            !columns.iter().any(|c| c == "allowed_origins"),
            "got columns {columns:?}"
        );
        let store = Store::from_connection(conn);
        assert_eq!(
            store
                .get_tenant_by_id(&TenantId::new("old"))
                .unwrap()
                .unwrap()
                .public_key,
            "pk_old"
        );
    }

    #[test]
    fn migration_0013_preserves_orders_already_paid_by_the_old_ceiling() {
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();
        shared::migrations::apply(&conn, &MIGRATIONS[..12]).unwrap();
        conn.execute_batch(
            "INSERT INTO tenants (id, public_key, secret_token_hash, key_custody_backend,
                sealed_key_material, primary_address, allowed_origins, created_at_utc,
                confirmations_required, zero_conf_max_piconero)
             VALUES ('legacy', 'pk_legacy', 'hash_legacy', 'plain', x'00', '4addr', '[]', 1000, 10, 200);
             INSERT INTO orders (id, tenant_id, minor_index, address, xmr_amount_piconero,
                amount_received_piconero, status, confirmations, created_at_utc, expires_at_utc, updated_at_utc)
             VALUES ('trusted', 'legacy', 1, 'sub_1', 100, 100, 'paid', 0, 1000, 2000, 1000),
                    ('pending', 'legacy', 2, 'sub_2', 100, 0, 'pending', 0, 1000, 2000, 1000),
                    ('confirmed', 'legacy', 3, 'sub_3', 100, 100, 'paid', 10, 1000, 2000, 1000);",
        )
        .unwrap();

        shared::migrations::apply(&conn, MIGRATIONS).unwrap();
        let override_for = |id: &str| -> Option<i64> {
            conn.query_row(
                "SELECT confirmations_required_override FROM orders WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(override_for("trusted"), Some(0));
        assert_eq!(override_for("pending"), None);
        assert_eq!(override_for("confirmed"), None);
    }

    #[test]
    fn clear_double_spend_flag_only_reports_a_real_change_and_is_idempotent() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);

        assert!(
            !store.clear_double_spend_flag(&order.id).unwrap(),
            "nothing to clear yet"
        );

        store.mark_double_spend_detected(&order.id, 1000).unwrap();
        assert!(store
            .get_order_by_id(&order.id)
            .unwrap()
            .unwrap()
            .double_spend_detected_at
            .is_some());

        assert!(store.clear_double_spend_flag(&order.id).unwrap());
        assert!(store
            .get_order_by_id(&order.id)
            .unwrap()
            .unwrap()
            .double_spend_detected_at
            .is_none());

        assert!(
            !store.clear_double_spend_flag(&order.id).unwrap(),
            "already clear - idempotent"
        );
    }

    #[test]
    fn in_transaction_rolls_every_write_back_when_the_closure_fails() {
        let store = Store::open_in_memory().unwrap();
        let tenant = new_tenant(&store);
        let order = new_order(&store, tenant.tenant.id.as_str(), 1);

        let result: Result<()> = store.in_transaction(|s| {
            s.record_payment_match(&order.id, "tx_a", 0, 100, "[]", 1500, Some(50), None)?;
            s.mark_double_spend_detected(&order.id, 1600)?;
            Err(StoreError::NotFound)
        });
        assert!(result.is_err());
        assert!(
            store.get_all_payments(&order.id).unwrap().is_empty(),
            "the whole group must be gone, not just the last write"
        );
        assert!(store
            .get_order(&tenant.tenant.id, &order.id)
            .unwrap()
            .unwrap()
            .double_spend_detected_at
            .is_none());

        // And the successful case commits normally.
        store
            .in_transaction(|s| {
                s.record_payment_match(&order.id, "tx_a", 0, 100, "[]", 1500, Some(50), None)
            })
            .unwrap();
        assert_eq!(store.get_all_payments(&order.id).unwrap().len(), 1);
    }

    #[test]
    fn forget_scanned_blocks_at_or_above_walks_the_high_water_mark_back() {
        let store = Store::open_in_memory().unwrap();
        for h in 100..=105 {
            store
                .set_scanned_block(monero::Network::Mainnet, h, &format!("hash{h}"))
                .unwrap();
        }
        store
            .set_scanned_block(monero::Network::Stagenet, 103, "stagenet_hash")
            .unwrap();

        store
            .forget_scanned_blocks_at_or_above(monero::Network::Mainnet, 103)
            .unwrap();
        assert_eq!(
            store.max_scanned_height(monero::Network::Mainnet).unwrap(),
            Some(102)
        );
        assert!(store
            .get_scanned_block_hash(monero::Network::Mainnet, 103)
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .get_scanned_block_hash(monero::Network::Stagenet, 103)
                .unwrap(),
            Some("stagenet_hash".to_owned()),
            "another network's window at the same height must be untouched"
        );
    }
}
