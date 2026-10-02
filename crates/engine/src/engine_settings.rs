//! Every engine setting, declared once with the `live-settings` library
//! (admin_settings_v2.md part 1), grouped into the sections each part of the
//! engine depends on, so a saved setting reaches the running engine.
//!
//! Keys, environment variables, defaults and ranges are the same as before
//! (`settings.rs`, `http::instance_admin::validate_scalar`). New: every
//! setting describes itself for the admin page, and `monero_node.<network>`
//! can also be set from an environment variable (`ENGINE_MONERO_NODE_<NETWORK>`,
//! the same JSON).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use live_settings::{
    choice_value, settings, AnySetting, BindAddr, CommaList, FieldError, Json, Live, Registry,
    Section, Snapshot,
};

use key_custody_service::client::SocketKeyCustody;

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
    DATABASE_PATH: PathBuf {
        key: "database.path",
        default: live_settings::paths::data_file("engine.db").unwrap_or_else(|| PathBuf::from("engine.db")),
        description: "The engine's database file, ~/.local/share/monokulo/engine.db unless set (or engine.db in the working directory if that can't be used). Its log store is kept beside it.",
        example: "/var/lib/monokulo/engine.db",
        applies: Restart,
        editable: false,
    },
    SERVER_TOKEN: live_settings::Secret {
        key: "server.token",
        env: "ENGINE_TOKEN",
        default: live_settings::Secret::default(),
        check: |token: &live_settings::Secret| shared::auth::check_engine_token(token.expose()),
        description: "The token every request to the engine must carry; the engine refuses any request without it. Required, at least 32 characters. Generate one with `openssl rand -hex 32` and give the same value to the engine (ENGINE_TOKEN) and monokulo (MONOKULO_ENGINE_TOKEN).",
        applies: Restart,
        sources: [Env],
        required: true,
    },
    LOGGING_FORMAT: telemetry::LogFormat {
        key: "logging.format",
        default: telemetry::LogFormat::Auto,
        description: "How log lines are written to the console: json, pretty, or auto (pretty at a terminal, JSON everywhere else).",
        example: "json",
        applies: Restart,
    },
    MONERO_NODE_STRICT_TLS: bool {
        key: "monero_node.strict_tls",
        default: false,
        description: "Refuse self-signed certificates from every Monero node, whatever each node's own accept_self_signed_certs says. Only for nodes with a certificate from a public authority.",
        example: "false",
    },
    MONERO_NODE_MAINNET: Option<Json<MoneroNodeSetting>> {
        key: "monero_node.mainnet",
        default: None,
        check: crate::settings::check_node,
        description: "The Monero node the engine reads the mainnet chain from, as JSON: host, port, ssl (default false), accept_self_signed_certs (default true), and fallbacks, a list of more nodes in the same shape tried in order when the one before fails (fallbacks can't have fallbacks). Leave empty to not use mainnet.",
        example: r#"{"host":"node.example.com","port":18089,"ssl":true,"fallbacks":[{"host":"node2.example.com","port":18089,"ssl":true}]}"#,
    },
    MONERO_NODE_STAGENET: Option<Json<MoneroNodeSetting>> {
        key: "monero_node.stagenet",
        default: None,
        check: crate::settings::check_node,
        description: "The Monero node for the stagenet test network, in the same JSON shape as mainnet's. Leave empty to not use stagenet.",
        example: NODE_EXAMPLE,
    },
    MONERO_NODE_TESTNET: Option<Json<MoneroNodeSetting>> {
        key: "monero_node.testnet",
        default: None,
        check: crate::settings::check_node,
        description: "The Monero node for testnet, in the same JSON shape as mainnet's. Leave empty to not use testnet.",
        example: r#"{"host":"127.0.0.1","port":28081}"#,
    },
    KEY_CUSTODY_ENABLED_BACKENDS: CommaList<CustodyBackend> {
        key: "key_custody.enabled_backends",
        default: live_settings::parsed_default("plain"),
        description: "Where stores' private view keys may be held (comma-separated in the environment variable): plain (in the engine's own memory) and socket (a separate key-custody-server process). Each store uses one of these; a store whose backend is turned off stops being scanned until it's turned on again or the store moves to another one.",
        example: "plain,socket",
    },
    KEY_CUSTODY_DEFAULT_BACKEND: CustodyBackend {
        key: "key_custody.default_backend",
        default: CustodyBackend::Plain,
        description: "The backend new stores get unless they choose another. Must be one of the enabled ones.",
        example: "plain",
    },
    KEY_CUSTODY_SOCKET_PATH: Option<PathBuf> {
        key: "key_custody.socket_path",
        default: None,
        description: "The Unix socket a running key-custody-server listens on. Required when socket is enabled.",
        example: "/run/key-custody/sock",
    },
    KEY_CUSTODY_SOCKET_CONNECTIONS: Option<usize> {
        key: "key_custody.socket_connections",
        default: None,
        check: range(1, 1024),
        description: "The most connections the engine keeps to the key-custody-server. A connection carries one scan at a time, so no more stores than this are scanned on the socket backend at once. Leave empty for one per CPU core. Connections are opened only as scans overlap.",
        example: "8",
    },
    PAYMENT_CONFIRMATIONS_REQUIRED: u64 {
        key: "payment.confirmations_required",
        default: 10,
        check: range(0, 720),
        description: "Confirmations a payment needs before an order is paid, for new stores that don't set their own. 0 means paid as soon as the payment is seen in the mempool. Each store's own thresholds in monokulo apply to its orders.",
        example: "10",
    },
    PAYMENT_ORDER_EXPIRY_MINUTES: i64 {
        key: "payment.order_expiry_minutes",
        default: 30,
        check: range(1, 525_600),
        description: "Minutes an order waits for payment before it expires, for new stores that don't set their own.",
        example: "30",
    },
    PAYMENT_REORG_CHECK_DEPTH: u64 {
        key: "payment.reorg_check_depth",
        default: 20,
        check: range(1, 10_000),
        description: "How many recent blocks are checked again on every scan for a chain reorganisation.",
        example: "20",
    },
    PAYMENT_MEMPOOL_POLL_INTERVAL_MS: u64 {
        key: "payment.mempool_poll_interval_ms",
        default: 1000,
        check: range(100, 3_600_000),
        description: "Milliseconds between scans of the mempool and new blocks. Lower detects payments sooner and asks more of the node.",
        example: "1000",
    },
    PAYMENT_EXPIRED_ORDER_GRACE_PERIOD_MINUTES: i64 {
        key: "payment.expired_order_grace_period_minutes",
        default: 360,
        check: range(0, 525_600),
        description: "Minutes after an order is paid or expires during which payments to it are still watched for. A payment sent later is found with the store's payment lookup.",
        example: "360",
    },
    PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB: u32 {
        key: "payment.scan_chunk_memory_budget_mb",
        default: 8,
        // Up to 1 TB here; what this machine allows is checked with the
        // other networks' budgets (`max_scan_budget_mb`).
        check: range(1, 1_048_576),
        description: "Megabytes of block data each network's scan holds at once while catching up. Every network's budget together must fit in 80 % of the engine's memory.",
        example: "8",
    },
    SERVER_BIND: BindAddr {
        key: "server.bind",
        default: live_settings::parsed_default("127.0.0.1:8443"),
        description: "The address and port the engine listens on. Keep it loopback or private: only monokulo should reach the engine. After changing it, restart the engine, then set monokulo's engine URL to match.",
        example: "127.0.0.1:8443",
        applies: Restart,
    },
    SERVER_WORKER_THREADS: usize {
        key: "server.worker_threads",
        default: 2,
        check: range(1, 1024),
        description: "Threads the engine uses to serve requests and run its loops (scanning work has its own pool). Takes effect after the engine restarts.",
        example: "2",
        applies: Restart,
    },
    DATABASE_READ_CONNECTIONS: usize {
        key: "database.read_connections",
        default: shared::sqlite::DEFAULT_READ_CONNECTIONS,
        check: range(1, 64),
        description: "Read-only connections the engine opens to its database, each on its own thread. Reads run side by side, so more help up to the number of CPU cores; each keeps its own cache of about 2 MB. Takes effect after a restart.",
        example: "4",
        applies: Restart,
    },
    SERVER_RATE_LIMIT_PER_TOKEN_PER_MIN: u32 {
        key: "server.rate_limit_per_token_per_min",
        default: 120,
        check: range(1, 1_000_000),
        description: "Requests a minute allowed per API token (per store, and for requests without a store key).",
        example: "120",
    },
    SERVER_MAX_BODY_BYTES: usize {
        key: "server.max_body_bytes",
        default: 8192,
        check: range(256, 16 * 1024 * 1024),
        description: "Largest request body the engine accepts, in bytes.",
        example: "8192",
    },
    WEBHOOKS_ALLOW_PRIVATE_URLS: bool {
        key: "webhooks.allow_private_urls",
        default: false,
        description: "Whether webhooks may be sent to private or loopback addresses. Only for testing against your own network.",
        example: "false",
    },
    WEBHOOKS_DELIVERY_TIMEOUT_MS: u64 {
        key: "webhooks.delivery_timeout_ms",
        default: 5000,
        check: range(100, 300_000),
        description: "Milliseconds a store's webhook endpoint has to answer before the attempt counts as failed.",
        example: "5000",
    },
    WEBHOOKS_MAX_ATTEMPTS: u32 {
        key: "webhooks.max_attempts",
        default: 8,
        check: range(1, 64),
        description: "Attempts per webhook delivery before giving up, with the wait doubling from 1 minute up to 1 hour between them.",
        example: "8",
    },
    LOGGING_LEVEL: String {
        key: "logging.level",
        default: telemetry::DEFAULT_LEVEL.to_string(),
        check: telemetry::check_level,
        description: "Which log lines the engine writes: a level (error, warn, info, debug, trace), optionally followed by target=level pairs for parts of the engine.",
        example: "info,engine::loops=debug",
    },
    LOGGING_DEV_MODE_UNTIL: u64 {
        key: "logging.dev_mode_until",
        default: 0,
        check: range(0, i64::MAX),
        description: "Development logging: until this time the engine logs at debug level, then goes back to the level above by itself. Secrets and addresses stay hidden either way.",
        sources: [Database],
    },
    LOGGING_RETENTION_DAYS: u64 {
        key: "logging.retention_days",
        default: telemetry::store::DEFAULT_RETENTION_DAYS,
        check: range(1, 365),
        description: "Days the engine's log store keeps lines for the Logs page. Older lines are deleted once a minute.",
        example: "14",
    },
    LOGGING_MAX_MB: u64 {
        key: "logging.max_mb",
        default: telemetry::store::DEFAULT_MAX_MB,
        check: range(10, 100_000),
        description: "Most megabytes the engine's log store may use. Past it, the oldest lines are deleted first.",
        example: "500",
    },
    LOGGING_OTLP_ENDPOINT: String {
        key: "logging.otlp_endpoint",
        default: String::new(),
        check: telemetry::otlp::check_endpoint,
        description: "An OpenTelemetry collector (OTLP over HTTP) to send the engine's log lines and spans to as well, such as a Collector, Grafana, Seq or the Aspire Dashboard. Leave empty to keep them here only. They are redacted the same way either way.",
        example: "http://127.0.0.1:4318",
    },
    LOGGING_OTLP_HEADERS: live_settings::Secret {
        key: "logging.otlp_headers",
        env: "ENGINE_LOGGING_OTLP_HEADERS",
        default: live_settings::Secret::default(),
        check: telemetry::otlp::check_headers,
        description: "Headers the collector needs, such as an API key, as name=value pairs separated by commas.",
        sources: [Env],
    },
}

/// The networks the engine can scan, with their node setting.
pub const NETWORKS: [(
    &str,
    &live_settings::Setting<Option<Json<MoneroNodeSetting>>>,
); 3] = [
    ("mainnet", &MONERO_NODE_MAINNET),
    ("stagenet", &MONERO_NODE_STAGENET),
    ("testnet", &MONERO_NODE_TESTNET),
];

/// Log level and development mode (structured_logging.md task 1.3), applied
/// to the process-wide subscriber by `telemetry::LogReloadable`.
#[derive(Debug, Clone, PartialEq)]
pub struct LoggingConfig(pub telemetry::LogConfig);

impl AsRef<telemetry::LogConfig> for LoggingConfig {
    fn as_ref(&self) -> &telemetry::LogConfig {
        &self.0
    }
}

impl Section for LoggingConfig {
    const NAME: &'static str = "logging";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &LOGGING_LEVEL,
            &LOGGING_DEV_MODE_UNTIL,
            &LOGGING_RETENTION_DAYS,
            &LOGGING_MAX_MB,
            &LOGGING_OTLP_ENDPOINT,
            &LOGGING_OTLP_HEADERS,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(LoggingConfig(telemetry::LogConfig {
            level: snapshot.get(&LOGGING_LEVEL).trim().to_string(),
            dev_mode_until: snapshot.get(&LOGGING_DEV_MODE_UNTIL),
            retention_days: snapshot.get(&LOGGING_RETENTION_DAYS),
            max_mb: snapshot.get(&LOGGING_MAX_MB),
            otlp_endpoint: snapshot.get(&LOGGING_OTLP_ENDPOINT).trim().to_string(),
            otlp_headers: snapshot.get(&LOGGING_OTLP_HEADERS).expose().to_string(),
        }))
    }
}

/// What the engine needs before its settings store exists, or must never
/// keep in it: read from the environment only (`env_only`), at start.
#[derive(Debug, Clone, PartialEq)]
pub struct BootConfig {
    pub database_path: PathBuf,
    pub token: live_settings::Secret,
    pub log_format: telemetry::LogFormat,
}

impl Section for BootConfig {
    const NAME: &'static str = "boot";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&DATABASE_PATH, &SERVER_TOKEN, &LOGGING_FORMAT]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(BootConfig {
            database_path: snapshot.get(&DATABASE_PATH),
            token: snapshot.get(&SERVER_TOKEN),
            log_format: snapshot.get(&LOGGING_FORMAT),
        })
    }
}

/// Monero nodes per network (task 2.1), and whether self-signed
/// certificates are refused from all of them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodeConfig {
    pub nodes: HashMap<&'static str, MoneroNodeSetting>,
    pub strict_tls: bool,
}

impl Section for NodeConfig {
    const NAME: &'static str = "nodes";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &MONERO_NODE_MAINNET,
            &MONERO_NODE_STAGENET,
            &MONERO_NODE_TESTNET,
            &MONERO_NODE_STRICT_TLS,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        let mut nodes = HashMap::new();
        for (network, setting) in NETWORKS {
            if let Some(Json(node)) = snapshot.get(setting) {
                nodes.insert(network, node);
            }
        }
        Ok(NodeConfig {
            nodes,
            strict_tls: snapshot.get(&MONERO_NODE_STRICT_TLS),
        })
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
        let budget_mb = snapshot.get(&PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB);
        let networks = configured_networks(snapshot);
        if let Some(limit) = shared::resources::memory_limit_bytes() {
            if let Some(problem) = scan_budget_problem(budget_mb, networks, limit) {
                return Err(vec![FieldError::new(
                    PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB.key,
                    problem,
                )]);
            }
        }
        Ok(ScanConfig {
            reorg_check_depth: snapshot.get(&PAYMENT_REORG_CHECK_DEPTH),
            poll_interval: Duration::from_millis(snapshot.get(&PAYMENT_MEMPOOL_POLL_INTERVAL_MS)),
            expired_order_grace_period_seconds: snapshot
                .get(&PAYMENT_EXPIRED_ORDER_GRACE_PERIOD_MINUTES)
                * 60,
            scan_chunk_memory_budget_mb: snapshot.get(&PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB),
        })
    }
}

/// The share of the engine's memory every network's scan budget may take
/// together (docs/engine_scaling.md section 3).
const SCAN_MEMORY_SHARE: f64 = 0.8;
/// What a budget really costs: the block cache, plus a response being
/// decoded (at most an eighth of the budget, held briefly twice).
const SCAN_MEMORY_OVERHEAD: f64 = 1.25;

/// The networks with a node configured, at least one: each scans with its
/// own budget.
pub fn configured_networks(snapshot: &Snapshot) -> u32 {
    let configured = NETWORKS
        .iter()
        .filter(|(_, setting)| snapshot.get(*setting).is_some())
        .count();
    u32::try_from(configured).unwrap_or(u32::MAX).max(1)
}

/// The largest scan budget, in MB, that `limit_bytes` of memory allows for
/// each of `networks` networks.
pub fn max_scan_budget_mb(limit_bytes: u64, networks: u32) -> u32 {
    let per_network = limit_bytes as f64 * SCAN_MEMORY_SHARE
        / (f64::from(networks.max(1)) * SCAN_MEMORY_OVERHEAD);
    (per_network / (1024.0 * 1024.0))
        .floor()
        .clamp(1.0, f64::from(u32::MAX)) as u32
}

/// Why `budget_mb` for each of `networks` networks doesn't fit in
/// `limit_bytes`, saying what would.
pub fn scan_budget_problem(budget_mb: u32, networks: u32, limit_bytes: u64) -> Option<String> {
    let max = max_scan_budget_mb(limit_bytes, networks);
    (budget_mb > max).then(|| {
        format!(
            "At most {max} MB on this machine (80 % of {}, across {networks} network{}).",
            human_bytes(limit_bytes),
            if networks == 1 { "" } else { "s" }
        )
    })
}

/// `bytes` as megabytes or gigabytes, for a message.
fn human_bytes(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{} MB", mb.round())
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
        &[
            &WEBHOOKS_ALLOW_PRIVATE_URLS,
            &WEBHOOKS_DELIVERY_TIMEOUT_MS,
            &WEBHOOKS_MAX_ATTEMPTS,
        ]
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
        &[
            &PAYMENT_CONFIRMATIONS_REQUIRED,
            &PAYMENT_ORDER_EXPIRY_MINUTES,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(TenantDefaults {
            confirmations_required: snapshot.get(&PAYMENT_CONFIRMATIONS_REQUIRED),
            order_expiry_seconds: snapshot.get(&PAYMENT_ORDER_EXPIRY_MINUTES) * 60,
        })
    }
}

/// Read once at start: the listen address, the thread count (tasks 2.7,
/// 2.8, decisions D1 and D8) and the database's read connections.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeConfig {
    pub bind: std::net::SocketAddr,
    pub worker_threads: usize,
    pub read_connections: usize,
}

impl Section for RuntimeConfig {
    const NAME: &'static str = "runtime";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &SERVER_BIND,
            &SERVER_WORKER_THREADS,
            &DATABASE_READ_CONNECTIONS,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(RuntimeConfig {
            bind: snapshot.get(&SERVER_BIND).0,
            worker_threads: snapshot.get(&SERVER_WORKER_THREADS),
            read_connections: snapshot.get(&DATABASE_READ_CONNECTIONS),
        })
    }
}

/// Which key custody backends are enabled, and which new stores get
/// (task 5.2, decision D3).
#[derive(Debug, Clone, PartialEq)]
pub struct CustodyConfig {
    pub enabled: Vec<CustodyBackend>,
    pub default: CustodyBackend,
    pub socket_path: Option<PathBuf>,
    /// `None` is one per CPU core.
    pub socket_connections: Option<usize>,
}

impl CustodyConfig {
    /// The connections the socket backend's client may keep.
    fn socket_connections(&self) -> usize {
        self.socket_connections
            .unwrap_or_else(key_custody_service::client::connections_per_core)
    }
}

impl Section for CustodyConfig {
    const NAME: &'static str = "key custody";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &KEY_CUSTODY_ENABLED_BACKENDS,
            &KEY_CUSTODY_DEFAULT_BACKEND,
            &KEY_CUSTODY_SOCKET_PATH,
            &KEY_CUSTODY_SOCKET_CONNECTIONS,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        let mut enabled = snapshot.get(&KEY_CUSTODY_ENABLED_BACKENDS).0;
        enabled.dedup();
        let default = snapshot.get(&KEY_CUSTODY_DEFAULT_BACKEND);
        let socket_path = snapshot.get(&KEY_CUSTODY_SOCKET_PATH);
        let mut errors = Vec::new();
        if enabled.is_empty() {
            errors.push(FieldError::new(
                KEY_CUSTODY_ENABLED_BACKENDS.key,
                "Enable at least one backend.",
            ));
        } else if !enabled.contains(&default) {
            errors.push(FieldError::new(
                KEY_CUSTODY_DEFAULT_BACKEND.key,
                format!(
                    "The default backend ({}) must be one of the enabled ones.",
                    default.as_str()
                ),
            ));
        }
        if enabled.contains(&CustodyBackend::Socket) && socket_path.is_none() {
            errors.push(FieldError::new(
                KEY_CUSTODY_SOCKET_PATH.key,
                "The socket backend needs the path of a running key-custody-server's socket.",
            ));
        }
        if errors.is_empty() {
            Ok(CustodyConfig {
                enabled,
                default,
                socket_path,
                socket_connections: snapshot.get(&KEY_CUSTODY_SOCKET_CONNECTIONS),
            })
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
/// the database, so enabling it again brings them back. A socket backend
/// that stays at its path keeps its instance whatever else changes: a new
/// number of connections is set on the client in use.
pub struct CustodyReloadable {
    router: Arc<crate::key_custody::CustodyRouter>,
    /// The router's socket backend, as the client it is: the router only
    /// knows it as a `KeyCustody`, which has no connections to set.
    socket: parking_lot::Mutex<Option<Arc<SocketKeyCustody>>>,
}

impl CustodyReloadable {
    pub fn new(router: Arc<crate::key_custody::CustodyRouter>) -> Self {
        Self {
            router,
            socket: parking_lot::Mutex::new(None),
        }
    }
}

/// What `CustodyReloadable::prepare` built: the router's next backends and
/// default, and the socket backend among them with the connections it may
/// keep.
pub struct PreparedCustody {
    backends: HashMap<String, Arc<dyn crate::key_custody::KeyCustody>>,
    default: String,
    socket: Option<(Arc<SocketKeyCustody>, usize)>,
}

#[live_settings::async_trait]
impl live_settings::Reloadable for CustodyReloadable {
    type Config = CustodyConfig;
    type Prepared = PreparedCustody;

    async fn prepare(
        &self,
        new: &CustodyConfig,
        old: &CustodyConfig,
    ) -> Result<(Self::Prepared, Vec<live_settings::Warning>), FieldError> {
        let current = self.router.backends();
        let mut backends: HashMap<String, Arc<dyn crate::key_custody::KeyCustody>> = HashMap::new();
        let mut socket = None;
        let mut warnings = Vec::new();
        for backend in &new.enabled {
            let name = backend.as_str().to_string();
            let custody: Arc<dyn crate::key_custody::KeyCustody> = match backend {
                CustodyBackend::Plain => match current.get(&name) {
                    Some(existing) => existing.clone(),
                    None => Arc::new(crate::key_custody::PlainKeyCustody::default()),
                },
                CustodyBackend::Socket => {
                    let in_use = self.socket.lock().clone().filter(|_| {
                        current.contains_key(&name) && new.socket_path == old.socket_path
                    });
                    let client = match in_use {
                        Some(client) => client,
                        None => {
                            // `CustodyConfig` guarantees the path when socket is on.
                            let path = new.socket_path.clone().unwrap_or_default();
                            let timeout = key_custody_service::client::DEFAULT_CALL_TIMEOUT;
                            match SocketKeyCustody::connect_with_timeout(&path, timeout).await {
                                Ok(client) => Arc::new(client),
                                Err(e) => {
                                    warnings.push(live_settings::Warning::for_key(
                                        KEY_CUSTODY_SOCKET_PATH.key,
                                        format!(
                                            "Saved, but no key-custody-server answers at {} yet ({e}). Stores on the socket backend aren't scanned until it does; it's picked up by itself.",
                                            path.display()
                                        ),
                                    ));
                                    Arc::new(SocketKeyCustody::not_connected_yet(&path, timeout))
                                }
                            }
                        }
                    };
                    socket = Some((client.clone(), new.socket_connections()));
                    client
                }
            };
            backends.insert(name, custody);
        }
        let prepared = PreparedCustody {
            backends,
            default: new.default.as_str().to_string(),
            socket,
        };
        Ok((prepared, warnings))
    }

    async fn install(&self, prepared: Self::Prepared) {
        let PreparedCustody {
            backends,
            default,
            socket,
        } = prepared;
        if let Some((client, connections)) = &socket {
            client.set_connections(*connections);
        }
        *self.socket.lock() = socket.map(|(client, _)| client);
        let dropped = self.router.replace(backends, &default);
        crate::key_custody::router::free_handles(dropped);
    }

    fn boot_policy(&self) -> live_settings::BootPolicy {
        live_settings::BootPolicy::StartDegraded
    }
}

/// The engine's settings store, over its own `settings` table. Its reads
/// and writes take the store's lock and run SQLite, so the async ones run
/// on the blocking pool.
#[derive(Clone)]
pub struct StoreSettings(pub SharedStore);

impl StoreSettings {
    /// Every stored setting, read on the calling thread.
    fn read_now(&self) -> Result<HashMap<String, String>, live_settings::StoreError> {
        self.0
            .lock()
            .list_settings()
            .map_err(live_settings::StoreError::new)
    }
}

#[live_settings::async_trait]
impl live_settings::SettingsStore for StoreSettings {
    async fn read_all(&self) -> Result<HashMap<String, String>, live_settings::StoreError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.read_now())
            .await
            .map_err(live_settings::StoreError::new)?
    }

    async fn write_all(
        &self,
        changes: Vec<(&'static str, Option<String>)>,
    ) -> Result<(), live_settings::StoreError> {
        let store = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || {
            store
                .lock()
                .in_transaction(|s| -> Result<(), crate::store::StoreError> {
                    for (key, value) in &changes {
                        match value {
                            Some(value) => s.set_setting(key, value)?,
                            None => s.delete_setting(key)?,
                        }
                    }
                    Ok(())
                })
                .map_err(live_settings::StoreError::new)
        })
        .await
        .map_err(live_settings::StoreError::new)?
    }
}

/// Every live section a running engine reads, plus the registry that saves
/// and describes settings (absent in tests that don't need one).
pub struct EngineSettings {
    pub registry: Option<Registry>,
    /// The environment the settings were read from: the real process
    /// environment, or a fixed one in tests, so no test depends on the
    /// shell it runs in.
    pub env: live_settings::Env,
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

#[allow(
    clippy::panic,
    reason = "the tests prove every section builds from its defaults"
)]
fn unreachable_defaults<S: Section>(errors: Vec<FieldError>) -> S {
    panic!(
        "{} doesn't build from its own defaults: {errors:?}",
        S::NAME
    )
}

impl EngineSettings {
    /// Default values and no registry, for tests that only need an engine
    /// running with ordinary settings.
    pub fn defaults() -> Arc<Self> {
        Arc::new(EngineSettings {
            registry: None,
            env: live_settings::Env::fixed(Vec::<(String, String)>::new()),
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

/// A daemon client per configured network.
pub type DaemonMap = HashMap<monero::Network, Arc<FallbackDaemonClient>>;

/// The daemon client for each configured network, swapped whole when node
/// settings are saved (task 2.1). Readers take a snapshot per request or per
/// tick; nobody holds the lock across an `.await`.
#[derive(Clone, Default)]
pub struct Daemons(Arc<parking_lot::RwLock<Arc<DaemonMap>>>);

impl Daemons {
    /// A fixed set, for tests and tools that don't change nodes.
    pub fn fixed(map: DaemonMap) -> Self {
        Daemons(Arc::new(parking_lot::RwLock::new(Arc::new(map))))
    }

    pub fn snapshot(&self) -> Arc<DaemonMap> {
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

    fn replace(&self, map: DaemonMap) {
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
        Ok(FallbackNode {
            label: format!("{}:{}", node.host, node.port),
            client: Arc::new(client),
        })
    };
    let mut nodes = vec![build(node)?];
    for fallback in &node.fallbacks {
        nodes.push(build(fallback)?);
    }
    Ok(FallbackDaemonClient::new(nodes))
}

/// Applies saved node settings to the running engine (task 2.1): networks
/// whose node settings didn't change keep their client (and its node health
/// and cooldowns); changed ones get a new client, as does every network when
/// `monero_node.strict_tls` changes; removed ones go.
pub struct NodesReloadable {
    pub daemons: Daemons,
}

#[live_settings::async_trait]
impl live_settings::Reloadable for NodesReloadable {
    type Config = NodeConfig;
    type Prepared = DaemonMap;

    async fn prepare(
        &self,
        new: &NodeConfig,
        old: &NodeConfig,
    ) -> Result<(Self::Prepared, Vec<live_settings::Warning>), FieldError> {
        let current = self.daemons.snapshot();
        let mut map = HashMap::new();
        for (name, node) in &new.nodes {
            let setting = NETWORKS
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, s)| s.key)
                .unwrap_or("monero_node");
            let network = crate::network::parse_network(name)
                .map_err(|e| FieldError::new(setting, e.to_string()))?;
            let unchanged = old.nodes.get(name) == Some(node) && old.strict_tls == new.strict_tls;
            let client = match current.get(&network) {
                Some(existing) if unchanged => existing.clone(),
                _ => Arc::new(
                    build_daemon_client(node, new.strict_tls)
                        .map_err(|e| FieldError::new(setting, e))?,
                ),
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

    async fn prepare(
        &self,
        new: &ApiLimits,
        _old: &ApiLimits,
    ) -> Result<(ApiLimits, Vec<live_settings::Warning>), FieldError> {
        Ok((new.clone(), Vec::new()))
    }

    async fn install(&self, limits: ApiLimits) {
        self.rate_limiter
            .set_limit(limits.rate_limit_per_token_per_min);
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
        router: Arc<crate::key_custody::CustodyRouter>,
        rate_limiter: Arc<shared::rate_limit::RateLimiter<String>>,
        env: live_settings::Env,
        options: live_settings::OptionsFile,
    ) -> Result<Arc<Self>, String> {
        Self::load_full(
            store,
            Some(NodesReloadable { daemons }),
            Some(CustodyReloadable::new(router)),
            rate_limiter,
            env,
            options,
        )
        .await
    }

    /// `load_full` without applying custody settings to a router (tests
    /// with a fixed key custody), over an options file in memory, with the
    /// test engine token unless `env` has one.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn load_with(
        store: SharedStore,
        nodes: Option<NodesReloadable>,
        rate_limiter: Arc<shared::rate_limit::RateLimiter<String>>,
        env: live_settings::Env,
    ) -> Result<Arc<Self>, String> {
        Self::load_full(
            store,
            nodes,
            None,
            rate_limiter,
            env.or_var(SERVER_TOKEN.env_var, shared::auth::TEST_ENGINE_TOKEN),
            live_settings::OptionsFile::in_memory(""),
        )
        .await
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
        options: live_settings::OptionsFile,
    ) -> Result<Arc<Self>, String> {
        // The options file holds the configuration and the database the
        // runtime switches, each key in its own place.
        let layered =
            live_settings::LayeredStore::new(options, Arc::new(StoreSettings(store)), ALL);
        let mut builder = Registry::builder_with_env(Arc::new(layered), ALL, env.clone()).await;
        let nodes = match nodes {
            Some(reloadable) => builder.reloadable(reloadable),
            None => builder.section::<NodeConfig>(),
        };
        let limits = builder.reloadable(LimitsReloadable { rate_limiter });
        let scan = builder.section::<ScanConfig>();
        let webhooks = builder.section::<WebhookConfig>();
        let tenant_defaults = builder.section::<TenantDefaults>();
        let runtime = builder.section::<RuntimeConfig>();
        // Read at start, before the store opened (`main`); registered so it
        // is described, checked and reported like every other setting.
        builder.section::<BootConfig>();
        builder.reloadable(telemetry::LogReloadable::<LoggingConfig>::default());
        let custody = match custody {
            Some(reloadable) => builder.reloadable(reloadable),
            None => builder.section::<CustodyConfig>(),
        };
        let registry = builder.build().map_err(|e| e.to_string())?;
        let report = registry.boot().await.map_err(|e| e.to_string())?;
        for warning in &report.warnings {
            tracing::warn!(
                setting = warning.key.as_deref(),
                "settings: {}",
                warning.message
            );
        }
        for (section, error) in &report.degraded {
            tracing::warn!(section = %section, error = %error, "settings: could not be applied at start, carrying on without it");
        }
        // Each invalid value and each section on its defaults was already
        // logged once, by `build`.
        Ok(Arc::new(EngineSettings {
            registry: Some(registry),
            env,
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
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn the_scan_budget_fits_in_80_percent_of_memory_across_networks() {
        let gb = 1024 * 1024 * 1024;
        // 8 GB: 6.4 GB for scanning, at 1.25 times each budget.
        assert_eq!(max_scan_budget_mb(8 * gb, 1), 5242);
        assert_eq!(max_scan_budget_mb(8 * gb, 3), 1747);
        assert_eq!(scan_budget_problem(5242, 1, 8 * gb), None);
        assert_eq!(
            scan_budget_problem(5243, 1, 8 * gb).as_deref(),
            Some("At most 5242 MB on this machine (80 % of 8.0 GB, across 1 network).")
        );
        assert_eq!(
            scan_budget_problem(2000, 3, 8 * gb).as_deref(),
            Some("At most 1747 MB on this machine (80 % of 8.0 GB, across 3 networks).")
        );
        // A tiny machine still allows the smallest budget.
        assert_eq!(max_scan_budget_mb(1024 * 1024, 3), 1);
        assert_eq!(scan_budget_problem(8, 1, 512 * 1024 * 1024), None);
    }

    #[test]
    fn a_budget_bigger_than_this_machine_allows_is_refused_with_the_maximum() {
        let limit = shared::resources::memory_limit_bytes().expect("the test machine's memory");
        let max = max_scan_budget_mb(limit, 1);
        let snapshot = |budget: u32| {
            Snapshot::new(
                std::collections::HashMap::from([(
                    PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB.key.to_string(),
                    budget.to_string(),
                )]),
                live_settings::Env::fixed(Vec::<(String, String)>::new()),
            )
        };
        assert!(ScanConfig::from_snapshot(&snapshot(max)).is_ok());
        let errors = ScanConfig::from_snapshot(&snapshot(max + 1)).unwrap_err();
        assert_eq!(errors[0].key, PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB.key);
        assert!(
            errors[0]
                .message
                .starts_with(&format!("At most {max} MB on this machine")),
            "{errors:?}"
        );
    }

    #[test]
    fn every_section_builds_from_its_defaults_and_the_node_settings_accept_the_stored_format() {
        let _ = EngineSettings::defaults();
        let node: Option<Json<MoneroNodeSetting>> =
            MONERO_NODE_STAGENET.parse(r#"{"host":"node.monerodevs.org","port":38089,"ssl":false,"accept_self_signed_certs":true,"fallbacks":[]}"#).unwrap();
        assert_eq!(node.unwrap().0.port, 38089);
        assert_eq!(
            MONERO_NODE_STAGENET.parse("").unwrap(),
            None,
            "empty means not configured"
        );
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
    fn socket_connections_may_be_left_empty_or_set_from_1_to_1024() {
        assert_eq!(KEY_CUSTODY_SOCKET_CONNECTIONS.parse("").unwrap(), None);
        assert_eq!(KEY_CUSTODY_SOCKET_CONNECTIONS.parse("8").unwrap(), Some(8));
        assert!(KEY_CUSTODY_SOCKET_CONNECTIONS.parse("1024").is_ok());
        assert!(KEY_CUSTODY_SOCKET_CONNECTIONS.parse("0").is_err());
        assert!(KEY_CUSTODY_SOCKET_CONNECTIONS.parse("1025").is_err());
        assert_eq!(defaults_of::<CustodyConfig>().socket_connections, None);
    }

    /// A stand-in key-custody-server on a socket of its own: it answers
    /// every request with a newly registered wallet.
    fn spawn_registering_server(tag: &str) -> PathBuf {
        use key_custody_service::protocol::{
            read_frame, write_frame, KeyCustodyRequest, KeyCustodyResponse,
        };
        let path =
            std::env::temp_dir().join(format!("engine-settings-{tag}-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _addr)) = listener.accept().await {
                tokio::spawn(async move {
                    while let Ok(Some(_)) = read_frame::<_, KeyCustodyRequest>(&mut stream).await {
                        let handle = crate::key_custody::WalletHandle::generate();
                        let answer = KeyCustodyResponse::RegisterWallet(Ok(handle.into()));
                        if write_frame(&mut stream, &answer).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        path
    }

    async fn save(reloadable: &CustodyReloadable, new: &CustodyConfig, old: &CustodyConfig) {
        use live_settings::Reloadable;
        let (prepared, warnings) = reloadable.prepare(new, old).await.unwrap();
        assert!(warnings.is_empty(), "the server is there");
        reloadable.install(prepared).await;
    }

    /// Saving a new number of connections for the socket backend sets it on
    /// the client in use: the stores registered there stay registered. Only
    /// a new socket path makes a new client, which then has the number too.
    #[tokio::test]
    async fn a_new_number_of_socket_connections_is_set_on_the_client_in_use() {
        use crate::key_custody::{CustodyRouter, KeyCustody, WalletMaterial};
        let custody = |path: &PathBuf, connections| CustodyConfig {
            enabled: vec![CustodyBackend::Plain, CustodyBackend::Socket],
            default: CustodyBackend::Plain,
            socket_path: Some(path.clone()),
            socket_connections: connections,
        };
        let path = spawn_registering_server("connections");
        let router = Arc::new(CustodyRouter::plain());
        let reloadable = CustodyReloadable::new(router.clone());
        let client = || reloadable.socket.lock().clone().unwrap();
        let cores = key_custody_service::client::connections_per_core();

        // Nothing set: one connection per core.
        let unset = custody(&path, None);
        save(&reloadable, &unset, &defaults_of::<CustodyConfig>()).await;
        let first = client();
        assert_eq!(first.connections(), cores);
        let store = router
            .register_wallet_in("socket", WalletMaterial::new([1; 32], [2; 32]))
            .await
            .unwrap();

        let three = custody(&path, Some(3));
        save(&reloadable, &three, &unset).await;
        assert!(Arc::ptr_eq(&first, &client()), "the same client");
        assert_eq!(first.connections(), 3);
        assert_eq!(router.backend_of(store).as_deref(), Some("socket"));

        // Emptied again: back to one per core, still the same client.
        save(&reloadable, &unset, &three).await;
        assert!(Arc::ptr_eq(&first, &client()));
        assert_eq!(first.connections(), cores);
        assert_eq!(router.backend_of(store).as_deref(), Some("socket"));

        // Another server is another client: its stores are registered
        // again there, and it keeps the number of connections saved.
        let elsewhere = spawn_registering_server("connections-elsewhere");
        let moved = custody(&elsewhere, Some(2));
        save(&reloadable, &moved, &unset).await;
        assert!(!Arc::ptr_eq(&first, &client()));
        assert_eq!(client().connections(), 2);
        assert_eq!(router.backend_of(store), None);

        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(elsewhere);
    }

    /// A socket backend turned on while its server is away gets the number
    /// of connections saved as well, for when the server appears.
    #[tokio::test]
    async fn a_socket_backend_whose_server_is_away_still_gets_its_connections() {
        use live_settings::Reloadable;
        let router = Arc::new(crate::key_custody::CustodyRouter::plain());
        let reloadable = CustodyReloadable::new(router);
        let new = CustodyConfig {
            enabled: vec![CustodyBackend::Plain, CustodyBackend::Socket],
            default: CustodyBackend::Plain,
            socket_path: Some(std::env::temp_dir().join("engine-settings-nobody-listens.sock")),
            socket_connections: Some(5),
        };
        let (prepared, warnings) = reloadable
            .prepare(&new, &defaults_of::<CustodyConfig>())
            .await
            .unwrap();
        assert_eq!(warnings.len(), 1, "saved, with a word that nothing answers");
        reloadable.install(prepared).await;
        assert_eq!(reloadable.socket.lock().clone().unwrap().connections(), 5);

        // Turned off again, the client is let go.
        let (prepared, _) = reloadable
            .prepare(&defaults_of::<CustodyConfig>(), &new)
            .await
            .unwrap();
        reloadable.install(prepared).await;
        assert!(reloadable.socket.lock().is_none());
    }
}
