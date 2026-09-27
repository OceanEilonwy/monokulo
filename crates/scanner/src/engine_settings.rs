//! Every engine setting, declared once with the `live-settings` library
//! (admin_settings_v2.md part 1), grouped into the sections each part of the
//! engine depends on, so a saved setting reaches the running engine.
//!
//! Keys, environment variables, defaults and ranges are the same as before
//! (`settings.rs`, `http::instance_admin::validate_scalar`). New: every
//! setting describes itself for the admin page, and `monero_node.<network>`
//! can also be set from an environment variable (`SCANNER_MONERO_NODE_<NETWORK>`,
//! the same JSON).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use live_settings::{
    choice_value, settings, AnySetting, BindAddr, CommaList, FieldError, Json, Live, Registry, Section, Snapshot,
};

use crate::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use crate::daemon_rpc::RpcDaemonClient;
use crate::settings::MoneroNodeSetting;
use crate::store::SharedStore;

choice_value! {
    /// Where tenants' view keys are held (see `docs/DESIGN.md` §8).
    pub enum CustodyBackend { Plain = "plain", Socket = "socket" }
}

const NODE_EXAMPLE: &str = r#"{"host":"node.monerodevs.org","port":38089,"ssl":false,"accept_self_signed_certs":true,"fallbacks":[{"host":"node2.monerodevs.org","port":38089,"ssl":false}]}"#;

settings! {
    MONERO_NODE_MAINNET: Option<Json<MoneroNodeSetting>> {
        key: "monero_node.mainnet",
        env: "SCANNER_MONERO_NODE_MAINNET",
        default: None,
        description: "The Monero node the engine reads the mainnet chain from, as JSON: host, port, ssl (default false), accept_self_signed_certs (default true), and fallbacks, a list of more nodes in the same shape tried in order when the one before fails (fallbacks can't have fallbacks). Leave empty to not use mainnet.",
        example: r#"{"host":"node.example.com","port":18089,"ssl":true,"fallbacks":[{"host":"node2.example.com","port":18089,"ssl":true}]}"#,
    },
    MONERO_NODE_STAGENET: Option<Json<MoneroNodeSetting>> {
        key: "monero_node.stagenet",
        env: "SCANNER_MONERO_NODE_STAGENET",
        default: None,
        description: "The Monero node for the stagenet test network, in the same JSON shape as mainnet's. Leave empty to not use stagenet.",
        example: NODE_EXAMPLE,
    },
    MONERO_NODE_TESTNET: Option<Json<MoneroNodeSetting>> {
        key: "monero_node.testnet",
        env: "SCANNER_MONERO_NODE_TESTNET",
        default: None,
        description: "The Monero node for testnet, in the same JSON shape as mainnet's. Leave empty to not use testnet.",
        example: r#"{"host":"127.0.0.1","port":28081}"#,
    },
    KEY_CUSTODY_ENABLED_BACKENDS: CommaList<CustodyBackend> {
        key: "key_custody.enabled_backends",
        env: "SCANNER_KEY_CUSTODY_ENABLED_BACKENDS",
        default: live_settings::parsed_default("plain"),
        description: "Where stores' private view keys may be held, comma-separated: plain (in the engine's own memory) and socket (a separate key-custody-server process). Each store uses one of these; a store whose backend is turned off stops being scanned until it's turned on again or the store moves to another one.",
        example: "plain,socket",
    },
    KEY_CUSTODY_DEFAULT_BACKEND: CustodyBackend {
        key: "key_custody.default_backend",
        env: "SCANNER_KEY_CUSTODY_DEFAULT_BACKEND",
        default: CustodyBackend::Plain,
        description: "The backend new stores get unless they choose another. Must be one of the enabled ones.",
        example: "plain",
    },
    KEY_CUSTODY_SOCKET_PATH: Option<PathBuf> {
        key: "key_custody.socket_path",
        env: "SCANNER_KEY_CUSTODY_SOCKET_PATH",
        default: None,
        description: "The Unix socket a running key-custody-server listens on. Required when socket is enabled.",
        example: "/run/key-custody/sock",
    },
    PAYMENT_CONFIRMATIONS_REQUIRED: u64 {
        key: "payment.confirmations_required",
        env: "SCANNER_PAYMENT_CONFIRMATIONS_REQUIRED",
        default: 10,
        check: range(0, 720),
        description: "Confirmations a payment needs before an order is paid, for new stores that don't set their own. 0 means paid as soon as the payment is seen in the mempool. Each store's own thresholds in monokulo apply to its orders.",
        example: "10",
    },
    PAYMENT_ORDER_EXPIRY_MINUTES: i64 {
        key: "payment.order_expiry_minutes",
        env: "SCANNER_PAYMENT_ORDER_EXPIRY_MINUTES",
        default: 30,
        check: range(1, 525_600),
        description: "Minutes an order waits for payment before it expires, for new stores that don't set their own.",
        example: "30",
    },
    PAYMENT_REORG_CHECK_DEPTH: u64 {
        key: "payment.reorg_check_depth",
        env: "SCANNER_PAYMENT_REORG_CHECK_DEPTH",
        default: 20,
        check: range(1, 10_000),
        description: "How many recent blocks are checked again on every scan for a chain reorganisation.",
        example: "20",
    },
    PAYMENT_MEMPOOL_POLL_INTERVAL_MS: u64 {
        key: "payment.mempool_poll_interval_ms",
        env: "SCANNER_PAYMENT_MEMPOOL_POLL_INTERVAL_MS",
        default: 1000,
        check: range(100, 3_600_000),
        description: "Milliseconds between scans of the mempool and new blocks. Lower detects payments sooner and asks more of the node.",
        example: "1000",
    },
    PAYMENT_EXPIRED_ORDER_GRACE_PERIOD_MINUTES: i64 {
        key: "payment.expired_order_grace_period_minutes",
        env: "SCANNER_PAYMENT_EXPIRED_ORDER_GRACE_PERIOD_MINUTES",
        default: 360,
        check: range(0, 525_600),
        description: "Minutes after an order is paid or expires during which payments to it are still watched for. A payment sent later is found with the store's payment lookup.",
        example: "360",
    },
    PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB: u32 {
        key: "payment.scan_chunk_memory_budget_mb",
        env: "SCANNER_PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB",
        default: 8,
        check: range(1, 4096),
        description: "Megabytes of block data fetched at once when catching up on many blocks.",
        example: "8",
    },
    SERVER_BIND: BindAddr {
        key: "server.bind",
        env: "SCANNER_SERVER_BIND",
        default: live_settings::parsed_default("127.0.0.1:8443"),
        description: "The address and port the engine listens on. Keep it loopback or private: only monokulo should reach the engine. After changing it, restart the engine, then set monokulo's engine URL to match.",
        example: "127.0.0.1:8443",
        applies: Restart,
    },
    SERVER_WORKER_THREADS: usize {
        key: "server.worker_threads",
        env: "SCANNER_SERVER_WORKER_THREADS",
        default: 2,
        check: range(1, 1024),
        description: "Threads the engine uses to serve requests and run its loops (scanning work has its own pool). Takes effect after the engine restarts.",
        example: "2",
        applies: Restart,
    },
    SERVER_RATE_LIMIT_PER_TOKEN_PER_MIN: u32 {
        key: "server.rate_limit_per_token_per_min",
        env: "SCANNER_SERVER_RATE_LIMIT_PER_TOKEN_PER_MIN",
        default: 120,
        check: range(1, 1_000_000),
        description: "Requests a minute allowed per API token (per store, and for the instance admin token).",
        example: "120",
    },
    SERVER_MAX_BODY_BYTES: usize {
        key: "server.max_body_bytes",
        env: "SCANNER_SERVER_MAX_BODY_BYTES",
        default: 8192,
        check: range(256, 16 * 1024 * 1024),
        description: "Largest request body the engine accepts, in bytes.",
        example: "8192",
    },
    WEBHOOKS_ALLOW_PRIVATE_URLS: bool {
        key: "webhooks.allow_private_urls",
        env: "SCANNER_WEBHOOKS_ALLOW_PRIVATE_URLS",
        default: false,
        description: "Whether webhooks may be sent to private or loopback addresses. Only for testing against your own network.",
        example: "false",
    },
    WEBHOOKS_DELIVERY_TIMEOUT_MS: u64 {
        key: "webhooks.delivery_timeout_ms",
        env: "SCANNER_WEBHOOKS_DELIVERY_TIMEOUT_MS",
        default: 5000,
        check: range(100, 300_000),
        description: "Milliseconds a store's webhook endpoint has to answer before the attempt counts as failed.",
        example: "5000",
    },
    WEBHOOKS_MAX_ATTEMPTS: u32 {
        key: "webhooks.max_attempts",
        env: "SCANNER_WEBHOOKS_MAX_ATTEMPTS",
        default: 8,
        check: range(1, 64),
        description: "Attempts per webhook delivery before giving up, with the wait doubling from 1 minute up to 1 hour between them.",
        example: "8",
    },
}

/// The networks the engine can scan, with their node setting.
pub const NETWORKS: [(&str, &live_settings::Setting<Option<Json<MoneroNodeSetting>>>); 3] =
    [("mainnet", &MONERO_NODE_MAINNET), ("stagenet", &MONERO_NODE_STAGENET), ("testnet", &MONERO_NODE_TESTNET)];

/// Monero nodes per network (task 2.1).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodeConfig {
    pub nodes: HashMap<&'static str, MoneroNodeSetting>,
}

impl Section for NodeConfig {
    const NAME: &'static str = "nodes";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&MONERO_NODE_MAINNET, &MONERO_NODE_STAGENET, &MONERO_NODE_TESTNET]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        let mut nodes = HashMap::new();
        for (network, setting) in NETWORKS {
            if let Some(Json(node)) = snapshot.get(setting) {
                nodes.insert(network, node);
            }
        }
        Ok(NodeConfig { nodes })
    }
}

/// What each scan tick reads (task 2.3).
#[derive(Debug, Clone, PartialEq)]
pub struct ScanConfig {
    pub reorg_check_depth: u64,
    pub poll_interval: Duration,
    pub expired_order_grace_period_seconds: i64,
    pub scan_chunk_memory_budget_mb: u32,
}

impl Section for ScanConfig {
    const NAME: &'static str = "scanning";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &PAYMENT_REORG_CHECK_DEPTH,
            &PAYMENT_MEMPOOL_POLL_INTERVAL_MS,
            &PAYMENT_EXPIRED_ORDER_GRACE_PERIOD_MINUTES,
            &PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(ScanConfig {
            reorg_check_depth: snapshot.get(&PAYMENT_REORG_CHECK_DEPTH),
            poll_interval: Duration::from_millis(snapshot.get(&PAYMENT_MEMPOOL_POLL_INTERVAL_MS)),
            expired_order_grace_period_seconds: snapshot.get(&PAYMENT_EXPIRED_ORDER_GRACE_PERIOD_MINUTES) * 60,
            scan_chunk_memory_budget_mb: snapshot.get(&PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB),
        })
    }
}

/// What each webhook delivery attempt reads (task 2.4).
#[derive(Debug, Clone, PartialEq)]
pub struct WebhookConfig {
    pub allow_private_urls: bool,
    pub delivery_timeout: Duration,
    pub max_attempts: u32,
}

impl Section for WebhookConfig {
    const NAME: &'static str = "webhooks";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&WEBHOOKS_ALLOW_PRIVATE_URLS, &WEBHOOKS_DELIVERY_TIMEOUT_MS, &WEBHOOKS_MAX_ATTEMPTS]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(WebhookConfig {
            allow_private_urls: snapshot.get(&WEBHOOKS_ALLOW_PRIVATE_URLS),
            delivery_timeout: Duration::from_millis(snapshot.get(&WEBHOOKS_DELIVERY_TIMEOUT_MS)),
            max_attempts: snapshot.get(&WEBHOOKS_MAX_ATTEMPTS),
        })
    }
}

/// API request limits (tasks 2.5, 2.6).
#[derive(Debug, Clone, PartialEq)]
pub struct ApiLimits {
    pub rate_limit_per_token_per_min: u32,
    pub max_body_bytes: usize,
}

impl Section for ApiLimits {
    const NAME: &'static str = "api limits";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&SERVER_RATE_LIMIT_PER_TOKEN_PER_MIN, &SERVER_MAX_BODY_BYTES]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(ApiLimits {
            rate_limit_per_token_per_min: snapshot.get(&SERVER_RATE_LIMIT_PER_TOKEN_PER_MIN),
            max_body_bytes: snapshot.get(&SERVER_MAX_BODY_BYTES),
        })
    }
}

/// Defaults for tenants created without their own values (task 2.9).
#[derive(Debug, Clone, PartialEq)]
pub struct TenantDefaults {
    pub confirmations_required: u64,
    pub order_expiry_seconds: i64,
}

impl Section for TenantDefaults {
    const NAME: &'static str = "tenant defaults";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&PAYMENT_CONFIRMATIONS_REQUIRED, &PAYMENT_ORDER_EXPIRY_MINUTES]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(TenantDefaults {
            confirmations_required: snapshot.get(&PAYMENT_CONFIRMATIONS_REQUIRED),
            order_expiry_seconds: snapshot.get(&PAYMENT_ORDER_EXPIRY_MINUTES) * 60,
        })
    }
}

/// Read once at start: the listen address and the thread count (tasks 2.7,
/// 2.8, decisions D1 and D8).
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeConfig {
    pub bind: std::net::SocketAddr,
    pub worker_threads: usize,
}

impl Section for RuntimeConfig {
    const NAME: &'static str = "runtime";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&SERVER_BIND, &SERVER_WORKER_THREADS]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(RuntimeConfig { bind: snapshot.get(&SERVER_BIND).0, worker_threads: snapshot.get(&SERVER_WORKER_THREADS) })
    }
}

/// Which key custody backends are enabled, and which new stores get
/// (task 5.2, decision D3).
#[derive(Debug, Clone, PartialEq)]
pub struct CustodyConfig {
    pub enabled: Vec<CustodyBackend>,
    pub default: CustodyBackend,
    pub socket_path: Option<PathBuf>,
}

impl Section for CustodyConfig {
    const NAME: &'static str = "key custody";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&KEY_CUSTODY_ENABLED_BACKENDS, &KEY_CUSTODY_DEFAULT_BACKEND, &KEY_CUSTODY_SOCKET_PATH]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        let mut enabled = snapshot.get(&KEY_CUSTODY_ENABLED_BACKENDS).0;
        enabled.dedup();
        let default = snapshot.get(&KEY_CUSTODY_DEFAULT_BACKEND);
        let socket_path = snapshot.get(&KEY_CUSTODY_SOCKET_PATH);
        let mut errors = Vec::new();
        if enabled.is_empty() {
            errors.push(FieldError::new(KEY_CUSTODY_ENABLED_BACKENDS.key, "Enable at least one backend."));
        } else if !enabled.contains(&default) {
            errors.push(FieldError::new(
                KEY_CUSTODY_DEFAULT_BACKEND.key,
                format!("The default backend ({}) must be one of the enabled ones.", default.as_str()),
            ));
        }
        if enabled.contains(&CustodyBackend::Socket) && socket_path.is_none() {
            errors.push(FieldError::new(
                KEY_CUSTODY_SOCKET_PATH.key,
                "The socket backend needs the path of a running key-custody-server's socket.",
            ));
        }
        if errors.is_empty() {
            Ok(CustodyConfig { enabled, default, socket_path })
        } else {
            Err(errors)
        }
    }
}

/// Applies saved custody settings to the router (task 5.2). Backends that
/// stay enabled keep their instance, so their wallets stay registered; a
/// newly enabled socket backend connects now, and if nothing answers yet it
/// is enabled anyway with a warning, and connects when the server appears.
/// A disabled backend's stores stop being scanned; their sealed keys stay in
/// the database, so enabling it again brings them back.
pub struct CustodyReloadable {
    pub router: Arc<crate::key_custody::CustodyRouter>,
}

#[live_settings::async_trait]
impl live_settings::Reloadable for CustodyReloadable {
    type Config = CustodyConfig;
    type Prepared = (HashMap<String, Arc<dyn crate::key_custody::KeyCustody>>, String);

    async fn prepare(&self, new: &CustodyConfig, old: &CustodyConfig) -> Result<(Self::Prepared, Vec<live_settings::Warning>), FieldError> {
        let current = self.router.backends();
        let mut backends: HashMap<String, Arc<dyn crate::key_custody::KeyCustody>> = HashMap::new();
        let mut warnings = Vec::new();
        for backend in &new.enabled {
            let name = backend.as_str().to_string();
            let reuse = current.get(&name).filter(|_| *backend != CustodyBackend::Socket || new.socket_path == old.socket_path);
            let custody: Arc<dyn crate::key_custody::KeyCustody> = match (reuse, backend) {
                (Some(existing), _) => existing.clone(),
                (None, CustodyBackend::Plain) => Arc::new(crate::key_custody::PlainKeyCustody::default()),
                (None, CustodyBackend::Socket) => {
                    // `CustodyConfig` guarantees the path when socket is on.
                    let path = new.socket_path.clone().unwrap_or_default();
                    let timeout = key_custody_service::client::DEFAULT_CALL_TIMEOUT;
                    match key_custody_service::client::SocketKeyCustody::connect_with_timeout(&path, timeout).await {
                        Ok(client) => Arc::new(client),
                        Err(e) => {
                            warnings.push(live_settings::Warning::for_key(
                                KEY_CUSTODY_SOCKET_PATH.key,
                                format!(
                                    "Saved, but no key-custody-server answers at {} yet ({e}). Stores on the socket backend aren't scanned until it does; it's picked up by itself.",
                                    path.display()
                                ),
                            ));
                            Arc::new(key_custody_service::client::SocketKeyCustody::not_connected_yet(&path, timeout))
                        }
                    }
                }
            };
            backends.insert(name, custody);
        }
        Ok(((backends, new.default.as_str().to_string()), warnings))
    }

    async fn install(&self, (backends, default): Self::Prepared) {
        self.router.replace(backends, &default);
    }

    fn boot_policy(&self) -> live_settings::BootPolicy {
        live_settings::BootPolicy::StartDegraded
    }
}

/// Marks that `migrate_key_custody_setting` has run.
const KEY_CUSTODY_MIGRATION_MARKER: &str = "migration.key_custody_per_store";

/// Converts the old single `key_custody.backend` setting to per-store
/// custody (task 5.1), once. The old value that was really in effect wins:
/// its environment variable, else the saved row, else plain (an invalid
/// value meant plain, as it always did). It becomes the only enabled
/// backend and the default, unless those were already saved, and every
/// tenant row is labelled with it: until now every tenant was registered in
/// that one backend whatever its row said, and both backends seal keys the
/// same way, so this moves no key material. A marker row keeps it from
/// running again, so a still-set old environment variable can't undo stores
/// switched since. Returns what it did, for the log.
pub fn migrate_key_custody_setting(store: &crate::store::Store) -> Result<Option<String>, crate::store::StoreError> {
    if store.get_setting(KEY_CUSTODY_MIGRATION_MARKER)?.is_some() {
        if shared::settings::env_value("SCANNER_KEY_CUSTODY_BACKEND").is_some_and(|v| !v.trim().is_empty()) {
            eprintln!(
                "warning: SCANNER_KEY_CUSTODY_BACKEND is set but no longer used; key custody is chosen per store now \
                 (key_custody.enabled_backends and key_custody.default_backend)"
            );
        }
        return Ok(None);
    }
    let old = shared::settings::env_value("SCANNER_KEY_CUSTODY_BACKEND")
        .filter(|v| !v.trim().is_empty())
        .or(store.get_setting("key_custody.backend")?);
    let backend = match old.as_deref().map(str::trim) {
        Some("socket") => "socket",
        _ => "plain",
    };
    store.in_transaction(|s| -> Result<(), crate::store::StoreError> {
        if s.get_setting(KEY_CUSTODY_ENABLED_BACKENDS.key)?.is_none() {
            s.set_setting(KEY_CUSTODY_ENABLED_BACKENDS.key, backend)?;
        }
        if s.get_setting(KEY_CUSTODY_DEFAULT_BACKEND.key)?.is_none() {
            s.set_setting(KEY_CUSTODY_DEFAULT_BACKEND.key, backend)?;
        }
        s.relabel_all_tenants_key_custody(backend)?;
        s.delete_setting("key_custody.backend")?;
        s.set_setting(KEY_CUSTODY_MIGRATION_MARKER, "done")
    })?;
    Ok(Some(format!("key custody is now per store; existing stores use {backend}")))
}

/// The engine's settings store, over its own `settings` table.
pub struct StoreSettings(pub SharedStore);

impl live_settings::SettingsStore for StoreSettings {
    fn read_all(&self) -> Result<HashMap<String, String>, live_settings::StoreError> {
        self.0.lock().list_settings().map_err(live_settings::StoreError::new)
    }

    fn write_all(&self, changes: &[(&str, Option<String>)]) -> Result<(), live_settings::StoreError> {
        let store = self.0.lock();
        store
            .in_transaction(|s| -> Result<(), crate::store::StoreError> {
                for (key, value) in changes {
                    match value {
                        Some(value) => s.set_setting(key, value)?,
                        None => s.delete_setting(key)?,
                    }
                }
                Ok(())
            })
            .map_err(live_settings::StoreError::new)
    }
}

/// Every live section a running engine reads, plus the registry that saves
/// and describes settings (absent in tests that don't need one).
pub struct EngineSettings {
    pub registry: Option<Registry>,
    pub nodes: Live<NodeConfig>,
    pub scan: Live<ScanConfig>,
    pub webhooks: Live<WebhookConfig>,
    pub limits: Live<ApiLimits>,
    pub tenant_defaults: Live<TenantDefaults>,
    pub runtime: Live<RuntimeConfig>,
    pub custody: Live<CustodyConfig>,
}

fn defaults_of<S: Section>() -> S {
    // Every default is valid (checked by `Registry::build` in the tests), so
    // a section built from defaults alone can't fail; fall back per field
    // anyway rather than panic.
    match S::from_snapshot(&Snapshot::defaults()) {
        Ok(section) => section,
        Err(errors) => unreachable_defaults::<S>(errors),
    }
}

#[allow(clippy::panic, reason = "the tests prove every section builds from its defaults")]
fn unreachable_defaults<S: Section>(errors: Vec<FieldError>) -> S {
    panic!("{} doesn't build from its own defaults: {errors:?}", S::NAME)
}

impl EngineSettings {
    /// Default values and no registry, for tests that only need an engine
    /// running with ordinary settings.
    pub fn defaults() -> Arc<Self> {
        Arc::new(EngineSettings {
            registry: None,
            nodes: Live::new(NodeConfig::default()),
            scan: Live::new(defaults_of()),
            webhooks: Live::new(defaults_of()),
            limits: Live::new(defaults_of()),
            tenant_defaults: Live::new(defaults_of()),
            runtime: Live::new(defaults_of()),
            custody: Live::new(defaults_of()),
        })
    }
}

/// The daemon client for each configured network, swapped whole when node
/// settings are saved (task 2.1). Readers take a snapshot per request or per
/// tick; nobody holds the lock across an `.await`.
#[derive(Clone, Default)]
pub struct Daemons(Arc<parking_lot::RwLock<Arc<HashMap<monero::Network, Arc<FallbackDaemonClient>>>>>);

impl Daemons {
    /// A fixed set, for tests and tools that don't change nodes.
    pub fn fixed(map: HashMap<monero::Network, Arc<FallbackDaemonClient>>) -> Self {
        Daemons(Arc::new(parking_lot::RwLock::new(Arc::new(map))))
    }

    pub fn snapshot(&self) -> Arc<HashMap<monero::Network, Arc<FallbackDaemonClient>>> {
        self.0.read().clone()
    }

    pub fn get(&self, network: monero::Network) -> Option<Arc<FallbackDaemonClient>> {
        self.0.read().get(&network).cloned()
    }

    pub fn is_configured(&self, network: monero::Network) -> bool {
        self.0.read().contains_key(&network)
    }

    pub fn networks(&self) -> Vec<monero::Network> {
        self.0.read().keys().copied().collect()
    }

    fn replace(&self, map: HashMap<monero::Network, Arc<FallbackDaemonClient>>) {
        *self.0.write() = Arc::new(map);
    }
}

/// Builds one network's client: its node and fallbacks, in order. Refuses
/// (never panics on) a node whose client can't be built.
pub fn build_daemon_client(
    node: &MoneroNodeSetting,
    strict_tls: bool,
) -> Result<FallbackDaemonClient, String> {
    let build = |node: &MoneroNodeSetting| -> Result<FallbackNode, String> {
        let accept_self_signed = node.accept_self_signed_certs && !strict_tls;
        let client = RpcDaemonClient::new(&node.host, node.port, node.ssl, accept_self_signed)
            .map_err(|e| format!("can't set up a client for {}:{}: {e}", node.host, node.port))?;
        Ok(FallbackNode { label: format!("{}:{}", node.host, node.port), client: Arc::new(client) })
    };
    let mut nodes = vec![build(node)?];
    for fallback in &node.fallbacks {
        nodes.push(build(fallback)?);
    }
    Ok(FallbackDaemonClient::new(nodes))
}

/// Applies saved node settings to the running engine (task 2.1): networks
/// whose node settings didn't change keep their client (and its node health
/// and cooldowns); changed ones get a new client; removed ones go.
pub struct NodesReloadable {
    pub daemons: Daemons,
    pub strict_tls: bool,
}

#[live_settings::async_trait]
impl live_settings::Reloadable for NodesReloadable {
    type Config = NodeConfig;
    type Prepared = HashMap<monero::Network, Arc<FallbackDaemonClient>>;

    async fn prepare(&self, new: &NodeConfig, old: &NodeConfig) -> Result<(Self::Prepared, Vec<live_settings::Warning>), FieldError> {
        let current = self.daemons.snapshot();
        let mut map = HashMap::new();
        for (name, node) in &new.nodes {
            let setting = NETWORKS.iter().find(|(n, _)| n == name).map(|(_, s)| s.key).unwrap_or("monero_node");
            let network = crate::network::parse_network(name).map_err(|e| FieldError::new(setting, e.to_string()))?;
            let unchanged = old.nodes.get(name) == Some(node);
            let client = match current.get(&network) {
                Some(existing) if unchanged => existing.clone(),
                _ => Arc::new(build_daemon_client(node, self.strict_tls).map_err(|e| FieldError::new(setting, e))?),
            };
            map.insert(network, client);
        }
        Ok((map, Vec::new()))
    }

    async fn install(&self, prepared: Self::Prepared) {
        self.daemons.replace(prepared);
    }

    fn boot_policy(&self) -> live_settings::BootPolicy {
        live_settings::BootPolicy::StartDegraded
    }
}

/// Applies the per-token rate limit (task 2.5). The body limit (task 2.6)
/// is read from the live section on every request.
pub struct LimitsReloadable {
    pub rate_limiter: Arc<shared::rate_limit::RateLimiter<String>>,
}

#[live_settings::async_trait]
impl live_settings::Reloadable for LimitsReloadable {
    type Config = ApiLimits;
    type Prepared = ApiLimits;

    async fn prepare(&self, new: &ApiLimits, _old: &ApiLimits) -> Result<(ApiLimits, Vec<live_settings::Warning>), FieldError> {
        Ok((new.clone(), Vec::new()))
    }

    async fn install(&self, limits: ApiLimits) {
        self.rate_limiter.set_limit(limits.rate_limit_per_token_per_min);
    }

    fn boot_policy(&self) -> live_settings::BootPolicy {
        live_settings::BootPolicy::StartDegraded
    }
}

impl EngineSettings {
    /// Loads every setting from `store` (and the environment), builds the
    /// runtime pieces that depend on them, and returns the live sections plus
    /// the registry the settings API saves through.
    pub async fn load(
        store: SharedStore,
        daemons: Daemons,
        strict_tls: bool,
        router: Arc<crate::key_custody::CustodyRouter>,
        rate_limiter: Arc<shared::rate_limit::RateLimiter<String>>,
    ) -> Result<Arc<Self>, String> {
        Self::load_full(
            store,
            Some(NodesReloadable { daemons, strict_tls }),
            Some(CustodyReloadable { router }),
            rate_limiter,
            live_settings::Env::process(),
        )
        .await
    }

    /// `load_full` without applying custody settings to a router (tests
    /// with a fixed key custody).
    pub async fn load_with(
        store: SharedStore,
        nodes: Option<NodesReloadable>,
        rate_limiter: Arc<shared::rate_limit::RateLimiter<String>>,
        env: live_settings::Env,
    ) -> Result<Arc<Self>, String> {
        Self::load_full(store, nodes, None, rate_limiter, env).await
    }

    /// `load`, optionally without applying node settings to daemon clients:
    /// for test engines whose daemons are fixed fakes. Node settings are
    /// still saved and described.
    pub async fn load_full(
        store: SharedStore,
        nodes: Option<NodesReloadable>,
        custody: Option<CustodyReloadable>,
        rate_limiter: Arc<shared::rate_limit::RateLimiter<String>>,
        env: live_settings::Env,
    ) -> Result<Arc<Self>, String> {
        let mut builder = Registry::builder_with_env(Arc::new(StoreSettings(store)), ALL, env);
        let nodes = match nodes {
            Some(reloadable) => builder.reloadable(reloadable),
            None => builder.section::<NodeConfig>(),
        };
        let limits = builder.reloadable(LimitsReloadable { rate_limiter });
        let scan = builder.section::<ScanConfig>();
        let webhooks = builder.section::<WebhookConfig>();
        let tenant_defaults = builder.section::<TenantDefaults>();
        let runtime = builder.section::<RuntimeConfig>();
        let custody = match custody {
            Some(reloadable) => builder.reloadable(reloadable),
            None => builder.section::<CustodyConfig>(),
        };
        let registry = builder.build().map_err(|e| e.to_string())?;
        let report = registry.boot().await.map_err(|e| e.to_string())?;
        for warning in &report.warnings {
            eprintln!("settings: {}", warning.message);
        }
        for (section, error) in &report.degraded {
            eprintln!("settings: {section} could not be applied at start, carrying on without it: {error}");
        }
        for (key, problem) in registry.describe().iter().filter_map(|v| v.problem.as_ref().map(|p| (v.key, p))) {
            eprintln!("settings: {key}: {}", problem.message);
        }
        Ok(Arc::new(EngineSettings {
            registry: Some(registry),
            nodes,
            scan,
            webhooks,
            limits,
            tenant_defaults,
            runtime,
            custody,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_builds_from_its_defaults_and_the_node_settings_accept_the_stored_format() {
        let _ = EngineSettings::defaults();
        let node: Option<Json<MoneroNodeSetting>> =
            MONERO_NODE_STAGENET.parse(r#"{"host":"node.monerodevs.org","port":38089,"ssl":false,"accept_self_signed_certs":true,"fallbacks":[]}"#).unwrap();
        assert_eq!(node.unwrap().0.port, 38089);
        assert_eq!(MONERO_NODE_STAGENET.parse("").unwrap(), None, "empty means not configured");
    }

    #[test]
    fn the_old_ranges_are_kept() {
        assert!(PAYMENT_REORG_CHECK_DEPTH.parse("10000").is_ok());
        assert!(PAYMENT_REORG_CHECK_DEPTH.parse("10001").is_err());
        assert!(PAYMENT_CONFIRMATIONS_REQUIRED.parse("0").is_ok());
        assert!(PAYMENT_CONFIRMATIONS_REQUIRED.parse("721").is_err());
        assert!(SERVER_MAX_BODY_BYTES.parse("255").is_err());
        assert!(KEY_CUSTODY_DEFAULT_BACKEND.parse("enclave").is_err());
        assert!(KEY_CUSTODY_ENABLED_BACKENDS.parse("plain,enclave").is_err());
    }

    #[test]
    fn the_old_backend_setting_becomes_the_enabled_default_and_labels_every_tenant_once() {
        let store = crate::store::Store::open_in_memory().unwrap();
        store.set_setting("key_custody.backend", "socket").unwrap();
        let tenant = store
            .create_tenant(
                crate::store::NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4x".into(),
                    network: "mainnet".into(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                1,
            )
            .unwrap()
            .tenant;
        assert!(migrate_key_custody_setting(&store).unwrap().is_some());
        assert_eq!(store.get_setting("key_custody.enabled_backends").unwrap().as_deref(), Some("socket"));
        assert_eq!(store.get_setting("key_custody.default_backend").unwrap().as_deref(), Some("socket"));
        assert_eq!(store.get_setting("key_custody.backend").unwrap(), None);
        assert_eq!(store.get_tenant_by_id(&tenant.id).unwrap().unwrap().key_custody_backend, "socket");

        // Once only: a store switched since keeps its backend.
        store.relabel_all_tenants_key_custody("plain").unwrap();
        assert!(migrate_key_custody_setting(&store).unwrap().is_none());
        assert_eq!(store.get_tenant_by_id(&tenant.id).unwrap().unwrap().key_custody_backend, "plain");
    }

    #[test]
    fn with_nothing_saved_the_migration_enables_plain() {
        let store = crate::store::Store::open_in_memory().unwrap();
        migrate_key_custody_setting(&store).unwrap();
        assert_eq!(store.get_setting("key_custody.enabled_backends").unwrap().as_deref(), Some("plain"));
        assert_eq!(store.get_setting("key_custody.default_backend").unwrap().as_deref(), Some("plain"));
    }
}
