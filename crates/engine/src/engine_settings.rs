//! Every engine setting, declared once with the `live-settings` library
//! (`admin_settings_v2.md` part 1).
//!
//! The settings are grouped into the sections each part of the engine depends
//! on, so a saved setting reaches the running engine.
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

use crate::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use crate::daemon_rpc::RpcDaemonClient;
use crate::settings::MoneroNodeSetting;
use crate::store::SharedStore;

choice_value! {
    /// Where tenants' view keys are held (see `docs/DESIGN.md` §6).
    pub enum CustodyBackend { Plain = "plain", Snp = "snp" }
}

choice_value! {
    /// The AMD EPYC generation an SEV-SNP engine runs on: which of AMD's
    /// root certificates its reports lead to.
    pub enum SnpProduct { Milan = "Milan", Genoa = "Genoa", Turin = "Turin" }
}

impl SnpProduct {
    pub fn product(self) -> snp_attest::report::Product {
        match self {
            Self::Milan => snp_attest::report::Product::Milan,
            Self::Genoa => snp_attest::report::Product::Genoa,
            Self::Turin => snp_attest::report::Product::Turin,
        }
    }
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
    PROOF_OF_WORK_MAINNET: bool {
        key: "proof_of_work.mainnet",
        default: true,
        description: "Check the proof of work of every mainnet block an order's confirmations are counted in, so a node can't make up the blocks a payment is in. Takes about 260 MB of memory and a few seconds of CPU a day. Turn it off only for a node you fully trust, such as your own.",
        example: "true",
    },
    PROOF_OF_WORK_STAGENET: bool {
        key: "proof_of_work.stagenet",
        default: false,
        description: "The same check for stagenet. Off by default: stagenet coins are worth nothing.",
        example: "false",
    },
    PROOF_OF_WORK_TESTNET: bool {
        key: "proof_of_work.testnet",
        default: false,
        description: "The same check for testnet. Off by default: testnet coins are worth nothing.",
        example: "false",
    },
    KEY_CUSTODY_ENABLED_BACKENDS: CommaList<CustodyBackend> {
        key: "key_custody.enabled_backends",
        default: live_settings::parsed_default("plain"),
        description: "Where stores' private view keys may be held (comma-separated in the environment variable): plain (in the engine's own memory, stored in the clear) and snp (for an engine inside an AMD SEV-SNP confidential VM: keys arrive encrypted to it and are stored sealed to the engine image). Each store uses one of these; a store whose backend is turned off stops being scanned until it's turned on again or the store moves to another one.",
        example: "plain,snp",
    },
    KEY_CUSTODY_DEFAULT_BACKEND: CustodyBackend {
        key: "key_custody.default_backend",
        default: CustodyBackend::Plain,
        description: "The backend new stores get unless they choose another. Must be one of the enabled ones.",
        example: "plain",
    },
    KEY_CUSTODY_SNP_PRODUCT: Option<SnpProduct> {
        key: "key_custody.snp_product",
        default: None,
        description: "The AMD EPYC generation this engine's confidential VM runs on: Milan, Genoa or Turin. Required for the snp backend.",
        example: "Genoa",
        applies: Restart,
        editable: false,
    },
    KEY_CUSTODY_SNP_TRUSTED_ID_KEY: Option<String> {
        key: "key_custody.snp_trusted_id_key",
        default: None,
        check: check_id_key_digest,
        description: "The SHA-384 digest (96 hex characters) of the ID key that signs the engine images this instance trusts. Leave empty to trust the official monokulo releases. Set it only if you build and sign your own engine image, and set monokulo's key_custody.snp_entry_id_key to the same digest: its key entry checks its own, and stops offering the snp backend while the two differ.",
        example: "",
        applies: Restart,
        editable: false,
    },
    KEY_CUSTODY_SNP_MIN_GUEST_SVN: u32 {
        key: "key_custody.snp_min_guest_svn",
        default: 0,
        description: "The lowest engine image security version (the ID block's guest SVN) trusted with keys: this engine refuses to start below it, and a handoff never goes to an image below it or below the engine handing over. Keep monokulo's key_custody.snp_entry_min_guest_svn the same: its key entry checks its own.",
        example: "1",
        applies: Restart,
        editable: false,
    },
    KEY_CUSTODY_SNP_MIN_TCB: Option<String> {
        key: "key_custody.snp_min_tcb",
        default: None,
        check: check_tcb_floor,
        description: "The lowest firmware trusted with keys, as the security patch levels bootloader,tee,snp,microcode of the attested TCB: this engine refuses to start below it, and a handoff checks it both ways; keep monokulo's key_custody.snp_entry_min_tcb the same, as its key entry checks its own. Set it to the levels AMD's security bulletins name for your EPYC generation; empty checks none.",
        example: "10,0,23,213",
        applies: Restart,
        editable: false,
    },
    KEY_CUSTODY_SNP_HANDOFF_URL: Option<live_settings::HttpUrl> {
        key: "key_custody.snp_handoff_url",
        default: None,
        description: "When this engine runs a new image, the address of the engine it replaces (still running, on the same private network), which hands over the master key the stores' keys are sealed under. Leave empty otherwise.",
        example: "http://10.0.0.5:8443",
        applies: Restart,
        editable: false,
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
        check: range(1, 0x0010_0000),
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
        description: "Threads the engine uses to serve requests and run its loops (scanning work has its own pool).",
        example: "2",
        applies: Restart,
    },
    SERVER_CPUS: String {
        key: "server.cpus",
        default: String::new(),
        check: |cpus: &String| crate::threads::parse_cpu_list(cpus).map(|_| ()),
        description: "The CPUs the engine's threads may run on, as taskset takes them (2,3 or 1-3); empty for all. One scan runs at a time per CPU listed. On a router, leaving some CPUs out keeps them free for routing while the engine catches up with the chain. Linux only.",
        example: "2,3",
        applies: Restart,
    },
    SERVER_NICE: u32 {
        key: "server.nice",
        default: 0,
        check: range(0, 19),
        description: "The niceness of the engine's threads: 0 is normal, 19 the lowest. Higher gives way to everything else on the machine; inside monokulo, to monokulo's own threads too. Linux only.",
        example: "10",
        applies: Restart,
    },
    DATABASE_READ_CONNECTIONS: usize {
        key: "database.read_connections",
        default: shared::sqlite::DEFAULT_READ_CONNECTIONS,
        check: range(1, 64),
        description: "Read-only connections the engine opens to its database, each on its own thread. Reads run side by side, so more help up to the number of CPU cores; each keeps its own cache of about 2 MB.",
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
        default: telemetry::DEFAULT_LEVEL.to_owned(),
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

/// Settings that only mean something for the standalone engine.
///
/// They are its listen address, the token its HTTP clients carry, and its
/// own process's logging (`docs/engine_as_library.md` §4). An engine
/// embedded in monokulo has no listener, is given a token by monokulo, and
/// logs through monokulo's logger, so these do nothing there: given to it,
/// they stop it at start, and its settings API leaves them out.
pub const STANDALONE_ONLY: &[&'static dyn AnySetting] = &[
    &SERVER_BIND,
    &SERVER_TOKEN,
    &LOGGING_FORMAT,
    &LOGGING_LEVEL,
    &LOGGING_DEV_MODE_UNTIL,
    &LOGGING_RETENTION_DAYS,
    &LOGGING_MAX_MB,
    &LOGGING_OTLP_ENDPOINT,
    &LOGGING_OTLP_HEADERS,
];

/// Whether `key` is one of [`STANDALONE_ONLY`].
pub fn standalone_only(key: &str) -> bool {
    STANDALONE_ONLY.iter().any(|setting| setting.key() == key)
}

/// Every setting an embedded engine has: [`ALL`] less [`STANDALONE_ONLY`].
/// What monokulo offers on its command line (`--engine-…`) and in its
/// options file (`[engine.…]`) for the engine inside it.
pub fn embedded_settings() -> Vec<&'static dyn AnySetting> {
    ALL.iter()
        .copied()
        .filter(|setting| !standalone_only(setting.key()))
        .collect()
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

/// Log level and development mode (`structured_logging.md` task 1.3), applied
/// to the process-wide subscriber by `telemetry::LogReloadable`.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        Ok(Self(telemetry::LogConfig {
            level: snapshot.get(&LOGGING_LEVEL).trim().to_owned(),
            dev_mode_until: snapshot.get(&LOGGING_DEV_MODE_UNTIL),
            retention_days: snapshot.get(&LOGGING_RETENTION_DAYS),
            max_mb: snapshot.get(&LOGGING_MAX_MB),
            otlp_endpoint: snapshot.get(&LOGGING_OTLP_ENDPOINT).trim().to_owned(),
            otlp_headers: snapshot.get(&LOGGING_OTLP_HEADERS).expose().to_owned(),
        }))
    }
}

/// What the engine needs before its settings store exists, or must never
/// keep in it: read from the environment only (`env_only`), at start.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        Ok(Self {
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
    pub nodes: std::collections::BTreeMap<&'static str, MoneroNodeSetting>,
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
        let mut nodes = std::collections::BTreeMap::new();
        for (network, setting) in NETWORKS {
            if let Some(Json(node)) = snapshot.get(setting) {
                nodes.insert(network, node);
            }
        }
        Ok(Self {
            nodes,
            strict_tls: snapshot.get(&MONERO_NODE_STRICT_TLS),
        })
    }
}

/// What each scan tick reads (task 2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanConfig {
    pub reorg_check_depth: u64,
    pub poll_interval: Duration,
    pub expired_order_grace_period_seconds: i64,
    pub scan_chunk_memory_budget_mb: u32,
    /// The networks whose blocks' proof of work is checked
    /// (`docs/proof_of_work.md`).
    pub proof_of_work: Vec<monero::Network>,
}

impl ScanConfig {
    pub fn checks_proof_of_work(&self, network: monero::Network) -> bool {
        self.proof_of_work.contains(&network)
    }
}

impl Section for ScanConfig {
    const NAME: &'static str = "scanning";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &PAYMENT_REORG_CHECK_DEPTH,
            &PAYMENT_MEMPOOL_POLL_INTERVAL_MS,
            &PAYMENT_EXPIRED_ORDER_GRACE_PERIOD_MINUTES,
            &PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB,
            &PROOF_OF_WORK_MAINNET,
            &PROOF_OF_WORK_STAGENET,
            &PROOF_OF_WORK_TESTNET,
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
        Ok(Self {
            reorg_check_depth: snapshot.get(&PAYMENT_REORG_CHECK_DEPTH),
            poll_interval: Duration::from_millis(snapshot.get(&PAYMENT_MEMPOOL_POLL_INTERVAL_MS)),
            expired_order_grace_period_seconds: snapshot
                .get(&PAYMENT_EXPIRED_ORDER_GRACE_PERIOD_MINUTES)
                * 60,
            scan_chunk_memory_budget_mb: snapshot.get(&PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB),
            proof_of_work: [
                (monero::Network::Mainnet, &PROOF_OF_WORK_MAINNET),
                (monero::Network::Stagenet, &PROOF_OF_WORK_STAGENET),
                (monero::Network::Testnet, &PROOF_OF_WORK_TESTNET),
            ]
            .into_iter()
            .filter(|(_, setting)| snapshot.get(*setting))
            .map(|(network, _)| network)
            .collect(),
        })
    }
}

/// The share of the engine's memory every network's scan budget may take
/// together (`docs/engine_scaling.md` section 3).
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
        Ok(Self {
            allow_private_urls: snapshot.get(&WEBHOOKS_ALLOW_PRIVATE_URLS),
            delivery_timeout: Duration::from_millis(snapshot.get(&WEBHOOKS_DELIVERY_TIMEOUT_MS)),
            max_attempts: snapshot.get(&WEBHOOKS_MAX_ATTEMPTS),
        })
    }
}

/// API request limits (tasks 2.5, 2.6).
#[derive(Debug, Clone, PartialEq, Eq)]
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
        Ok(Self {
            rate_limit_per_token_per_min: snapshot.get(&SERVER_RATE_LIMIT_PER_TOKEN_PER_MIN),
            max_body_bytes: snapshot.get(&SERVER_MAX_BODY_BYTES),
        })
    }
}

/// Defaults for tenants created without their own values (task 2.9).
#[derive(Debug, Clone, PartialEq, Eq)]
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
        Ok(Self {
            confirmations_required: snapshot.get(&PAYMENT_CONFIRMATIONS_REQUIRED),
            order_expiry_seconds: snapshot.get(&PAYMENT_ORDER_EXPIRY_MINUTES) * 60,
        })
    }
}

/// Read once at start: the listen address, the thread count (tasks 2.7,
/// 2.8, decisions D1 and D8) and the database's read connections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub bind: std::net::SocketAddr,
    pub worker_threads: usize,
    pub read_connections: usize,
    /// The engine's threads: `server.worker_threads`, `server.cpus` and
    /// `server.nice` (`crate::threads`).
    pub threads: crate::threads::ThreadPlan,
}

impl Section for RuntimeConfig {
    const NAME: &'static str = "runtime";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &SERVER_BIND,
            &SERVER_WORKER_THREADS,
            &SERVER_CPUS,
            &SERVER_NICE,
            &DATABASE_READ_CONNECTIONS,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        let worker_threads = snapshot.get(&SERVER_WORKER_THREADS);
        Ok(Self {
            bind: snapshot.get(&SERVER_BIND).0,
            worker_threads,
            read_connections: snapshot.get(&DATABASE_READ_CONNECTIONS),
            threads: crate::threads::ThreadPlan {
                workers: worker_threads,
                // Checked by the setting itself; an invalid value is
                // reported there and the default (every CPU) used.
                cpus: crate::threads::parse_cpu_list(&snapshot.get(&SERVER_CPUS))
                    .unwrap_or_default(),
                nice: i32::try_from(snapshot.get(&SERVER_NICE)).unwrap_or(0),
            },
        })
    }
}

/// An ID key digest setting: empty, or 96 hex characters.
#[expect(
    clippy::ref_option,
    reason = "the settings macro passes a setting's value by reference"
)]
fn check_id_key_digest(digest: &Option<String>) -> Result<(), String> {
    digest.as_deref().map_or(Ok(()), |text| {
        crate::key_custody::transport::parse_id_key_digest(text)
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
}

/// A TCB floor setting: empty, or four numbers.
#[expect(
    clippy::ref_option,
    reason = "the settings macro passes a setting's value by reference"
)]
fn check_tcb_floor(floor: &Option<String>) -> Result<(), String> {
    floor.as_deref().map_or(Ok(()), |text| {
        crate::key_custody::transport::TcbFloor::parse(text)
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
}

/// Which key custody backends are enabled, and which new stores get
/// (task 5.2, decision D3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyConfig {
    pub enabled: Vec<CustodyBackend>,
    pub default: CustodyBackend,
}

/// The `snp` backend's settings, which apply at a restart (`SnpSlot`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnpBootConfig {
    pub product: Option<SnpProduct>,
    pub trusted_id_key: Option<String>,
    pub min_guest_svn: u32,
    pub min_tcb: Option<String>,
    pub handoff_url: Option<live_settings::HttpUrl>,
}

impl SnpBootConfig {
    /// The ID key digest trusted: the one set, else the official one.
    pub fn trusted_id_key_digest(&self) -> Option<[u8; 48]> {
        match &self.trusted_id_key {
            Some(text) => crate::key_custody::transport::parse_id_key_digest(text).ok(),
            None => crate::key_custody::transport::official_id_key_digest(),
        }
    }

    /// The backend's configuration, or why it can't start.
    pub fn snp_config(&self) -> Result<crate::key_custody::snp::SnpConfig, String> {
        let product = self
            .product
            .ok_or("the snp backend needs key_custody.snp_product (Milan, Genoa or Turin)")?;
        let id_key_digest = self.trusted_id_key_digest().ok_or(
            "this build has no official engine ID key: set key_custody.snp_trusted_id_key to the digest of the key your engine image is signed with",
        )?;
        Ok(crate::key_custody::snp::SnpConfig {
            product: product.product(),
            trust: crate::key_custody::transport::TrustPolicy {
                id_key_digest,
                min_guest_svn: self.min_guest_svn,
                min_tcb: crate::key_custody::transport::TcbFloor::parse(
                    self.min_tcb.as_deref().unwrap_or(""),
                )
                .map_err(|e| e.to_string())?,
            },
        })
    }
}

impl Section for SnpBootConfig {
    const NAME: &'static str = "key custody (snp)";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[
            &KEY_CUSTODY_SNP_PRODUCT,
            &KEY_CUSTODY_SNP_TRUSTED_ID_KEY,
            &KEY_CUSTODY_SNP_MIN_GUEST_SVN,
            &KEY_CUSTODY_SNP_MIN_TCB,
            &KEY_CUSTODY_SNP_HANDOFF_URL,
        ]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        Ok(Self {
            product: snapshot.get(&KEY_CUSTODY_SNP_PRODUCT),
            trusted_id_key: snapshot.get(&KEY_CUSTODY_SNP_TRUSTED_ID_KEY),
            min_guest_svn: snapshot.get(&KEY_CUSTODY_SNP_MIN_GUEST_SVN),
            min_tcb: snapshot.get(&KEY_CUSTODY_SNP_MIN_TCB),
            handoff_url: snapshot.get(&KEY_CUSTODY_SNP_HANDOFF_URL),
        })
    }
}

impl Section for CustodyConfig {
    const NAME: &'static str = "key custody";
    fn keys() -> &'static [&'static dyn AnySetting] {
        &[&KEY_CUSTODY_ENABLED_BACKENDS, &KEY_CUSTODY_DEFAULT_BACKEND]
    }
    fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Vec<FieldError>> {
        let mut enabled = snapshot.get(&KEY_CUSTODY_ENABLED_BACKENDS).0;
        enabled.dedup();
        let default = snapshot.get(&KEY_CUSTODY_DEFAULT_BACKEND);
        let mut errors = Vec::new();
        if enabled.is_empty() {
            errors.push(FieldError::new(
                KEY_CUSTODY_ENABLED_BACKENDS.key,
                "Enable at least one backend.",
            ));
        } else if enabled.contains(&default) {
            // The default is one of the enabled backends: nothing to report.
        } else {
            errors.push(FieldError::new(
                KEY_CUSTODY_DEFAULT_BACKEND.key,
                format!(
                    "The default backend ({}) must be one of the enabled ones.",
                    default.as_str()
                ),
            ));
        }
        if errors.is_empty() {
            Ok(Self { enabled, default })
        } else {
            Err(errors)
        }
    }
}

/// Applies saved custody settings to the router (task 5.2).
///
/// Backends that stay enabled keep their instance, so their wallets stay
/// registered. The `snp` backend starts the first time it is enabled and is
/// kept from then on (`SnpSlot`); if it can't start, a stand-in that says why
/// takes its place, so its stores are reported unavailable with the reason
/// and the other backends carry on. A disabled backend's stores stop being
/// scanned; their sealed keys stay in the database, so enabling it again
/// brings them back.
pub struct CustodyReloadable {
    router: Arc<crate::key_custody::CustodyRouter>,
    snp: Arc<crate::key_custody::SnpSlot>,
}

impl CustodyReloadable {
    pub fn new(
        router: Arc<crate::key_custody::CustodyRouter>,
        snp: Arc<crate::key_custody::SnpSlot>,
    ) -> Self {
        Self { router, snp }
    }
}

/// What `CustodyReloadable::prepare` built: the router's next backends and
/// default.
pub struct PreparedCustody {
    backends: HashMap<String, Arc<dyn crate::key_custody::KeyCustody>>,
    default: String,
}

#[live_settings::async_trait]
impl live_settings::Reloadable for CustodyReloadable {
    type Config = CustodyConfig;
    type Prepared = PreparedCustody;

    async fn prepare(
        &self,
        new: &CustodyConfig,
        _old: &CustodyConfig,
    ) -> Result<(Self::Prepared, Vec<live_settings::Warning>), FieldError> {
        let current = self.router.backends();
        let mut backends: HashMap<String, Arc<dyn crate::key_custody::KeyCustody>> = HashMap::new();
        let mut warnings = Vec::new();
        for backend in &new.enabled {
            let name = backend.as_str().to_owned();
            let custody: Arc<dyn crate::key_custody::KeyCustody> = match backend {
                CustodyBackend::Plain => match current.get(&name) {
                    Some(existing) => Arc::clone(existing),
                    None => Arc::new(crate::key_custody::PlainKeyCustody::default()),
                },
                CustodyBackend::Snp => match self.snp.start() {
                    Ok(snp) => snp,
                    Err(e) => {
                        warnings.push(live_settings::Warning::for_key(
                            KEY_CUSTODY_ENABLED_BACKENDS.key,
                            format!(
                                "Saved, but the snp backend can't start: {e}. Stores on it aren't scanned until it can (its settings apply at a restart)."
                            ),
                        ));
                        Arc::new(crate::key_custody::Unstarted(format!(
                            "the snp backend can't start: {e}"
                        )))
                    }
                },
            };
            backends.insert(name, custody);
        }
        let prepared = PreparedCustody {
            backends,
            default: new.default.as_str().to_owned(),
        };
        Ok((prepared, warnings))
    }

    async fn install(&self, prepared: Self::Prepared) {
        let PreparedCustody { backends, default } = prepared;
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
    /// Whether this engine runs inside monokulo: then
    /// [`STANDALONE_ONLY`] settings do nothing, and its settings API leaves
    /// them out.
    pub embedded: bool,
}

fn defaults_of<S: Section>() -> S {
    // Every default is valid (checked by `Registry::build` in the tests), so
    // a section built from defaults alone can't fail; fall back per field
    // anyway rather than panic.
    match S::from_snapshot(&Snapshot::defaults()) {
        Ok(section) => section,
        Err(errors) => unreachable_defaults::<S>(&errors),
    }
}

#[expect(
    clippy::panic,
    reason = "the tests prove every section builds from its defaults"
)]
fn unreachable_defaults<S: Section>(errors: &[FieldError]) -> S {
    panic!(
        "{} doesn't build from its own defaults: {errors:?}",
        S::NAME
    )
}

impl EngineSettings {
    /// Default values and no registry, for tests that only need an engine
    /// running with ordinary settings.
    pub fn defaults() -> Arc<Self> {
        Arc::new(Self {
            registry: None,
            env: live_settings::Env::fixed(Vec::<(String, String)>::new()),
            nodes: Live::new(NodeConfig::default()),
            scan: Live::new(defaults_of()),
            webhooks: Live::new(defaults_of()),
            limits: Live::new(defaults_of()),
            tenant_defaults: Live::new(defaults_of()),
            runtime: Live::new(defaults_of()),
            custody: Live::new(defaults_of()),
            embedded: false,
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
        Self(Arc::new(parking_lot::RwLock::new(Arc::new(map))))
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

/// Applies saved node settings to the running engine (task 2.1).
///
/// Networks whose node settings didn't change keep their client (and its
/// node health and cooldowns); changed ones get a new client, as does every
/// network when `monero_node.strict_tls` changes; removed ones go.
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
                .map_or("monero_node", |(_, s)| s.key);
            let network = crate::network::parse_network(name)
                .map_err(|e| FieldError::new(setting, e.to_string()))?;
            let unchanged = old.nodes.get(name) == Some(node) && old.strict_tls == new.strict_tls;
            let client = match current.get(&network) {
                Some(existing) if unchanged => Arc::clone(existing),
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

    async fn install(&self, prepared: ApiLimits) {
        self.rate_limiter
            .set_limit(prepared.rate_limit_per_token_per_min);
    }

    fn boot_policy(&self) -> live_settings::BootPolicy {
        live_settings::BootPolicy::StartDegraded
    }
}

impl EngineSettings {
    /// Loads every setting from `store` (and the environment), builds the
    /// runtime pieces that depend on them, and returns the live sections plus
    /// the registry the settings API saves through.
    /// `embedded`: the engine runs inside monokulo (see the field).
    pub async fn load(
        store: SharedStore,
        daemons: Daemons,
        custody: CustodyReloadable,
        rate_limiter: Arc<shared::rate_limit::RateLimiter<String>>,
        env: live_settings::Env,
        options: live_settings::OptionsFile,
        embedded: bool,
    ) -> Result<Arc<Self>, String> {
        Self::load_full(
            store,
            Some(NodesReloadable { daemons }),
            Some(custody),
            rate_limiter,
            env,
            options,
            embedded,
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
            false,
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
        embedded: bool,
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
        // Read at start too, for the snp backend (`SnpSlot`).
        builder.section::<SnpBootConfig>();
        if embedded {
            // monokulo's logger is the process's: its logging settings
            // govern it, not these (STANDALONE_ONLY).
            builder.section::<LoggingConfig>();
        } else {
            builder.reloadable(telemetry::LogReloadable::<LoggingConfig>::default());
        }
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
        Ok(Arc::new(Self {
            registry: Some(registry),
            env,
            nodes,
            scan,
            webhooks,
            limits,
            tenant_defaults,
            runtime,
            custody,
            embedded,
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
                HashMap::from([(
                    PAYMENT_SCAN_CHUNK_MEMORY_BUDGET_MB.key.to_owned(),
                    budget.to_string(),
                )]),
                live_settings::Env::fixed(Vec::<(String, String)>::new()),
            )
        };
        ScanConfig::from_snapshot(&snapshot(max)).unwrap();
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
        PAYMENT_REORG_CHECK_DEPTH.parse("10000").unwrap();
        PAYMENT_REORG_CHECK_DEPTH.parse("10001").unwrap_err();
        PAYMENT_CONFIRMATIONS_REQUIRED.parse("0").unwrap();
        PAYMENT_CONFIRMATIONS_REQUIRED.parse("721").unwrap_err();
        SERVER_MAX_BODY_BYTES.parse("255").unwrap_err();
        KEY_CUSTODY_DEFAULT_BACKEND.parse("enclave").unwrap_err();
        KEY_CUSTODY_ENABLED_BACKENDS
            .parse("plain,enclave")
            .unwrap_err();
    }

    #[test]
    fn snp_settings_are_checked_and_say_what_the_backend_still_needs() {
        KEY_CUSTODY_SNP_TRUSTED_ID_KEY
            .parse(&"ab".repeat(48))
            .unwrap();
        KEY_CUSTODY_SNP_TRUSTED_ID_KEY.parse("ab").unwrap_err();
        KEY_CUSTODY_SNP_PRODUCT.parse("Venice").unwrap_err();
        KEY_CUSTODY_SNP_MIN_TCB.parse("1,2,3,4").unwrap();
        KEY_CUSTODY_SNP_MIN_TCB.parse("1,2,3").unwrap_err();
        assert_eq!(
            KEY_CUSTODY_SNP_PRODUCT.parse("Genoa").unwrap(),
            Some(SnpProduct::Genoa)
        );

        let defaults = defaults_of::<SnpBootConfig>();
        assert!(defaults.snp_config().unwrap_err().contains("snp_product"));
        let own_key = SnpBootConfig {
            product: Some(SnpProduct::Turin),
            trusted_id_key: Some("cd".repeat(48)),
            min_guest_svn: 3,
            min_tcb: Some("1,2,3,4".to_owned()),
            ..defaults.clone()
        };
        let config = own_key.snp_config().unwrap();
        assert_eq!(config.product, snp_attest::report::Product::Turin);
        assert_eq!(config.trust.id_key_digest, [0xCD; 48]);
        assert_eq!(config.trust.min_guest_svn, 3);
        assert_eq!(config.trust.min_tcb.to_text(), "1,2,3,4");
        if crate::key_custody::transport::official_id_key_digest().is_none() {
            let official = SnpBootConfig {
                product: Some(SnpProduct::Turin),
                ..defaults
            };
            assert!(official
                .snp_config()
                .unwrap_err()
                .contains("snp_trusted_id_key"));
        }
    }

    /// A slot whose backend runs on a stand-in security processor.
    fn test_slot() -> Arc<crate::key_custody::SnpSlot> {
        use snp_attest::guest::{TestGuest, TestIdentity};
        let store = crate::store::Store::open_in_memory().unwrap().into_shared();
        Arc::new(crate::key_custody::SnpSlot::new(
            Ok(crate::key_custody::snp::SnpConfig {
                product: snp_attest::report::Product::Genoa,
                trust: crate::key_custody::transport::TrustPolicy {
                    id_key_digest: TestIdentity::default().id_key_digest,
                    min_guest_svn: 0,
                    min_tcb: crate::key_custody::transport::TcbFloor::default(),
                },
            }),
            Arc::new(TestGuest::new([1; 32], TestIdentity::default())),
            Arc::new(crate::key_custody::StoreWraps(store)),
        ))
    }

    async fn save(
        reloadable: &CustodyReloadable,
        new: &CustodyConfig,
        old: &CustodyConfig,
    ) -> usize {
        use live_settings::Reloadable as _;
        let (prepared, warnings) = reloadable.prepare(new, old).await.unwrap();
        reloadable.install(prepared).await;
        warnings.len()
    }

    /// The snp backend starts the first time it is enabled and is the same
    /// instance from then on, so the stores registered in it stay
    /// registered whatever else is saved.
    #[tokio::test]
    async fn the_snp_backend_starts_once_and_keeps_its_stores() {
        use crate::key_custody::{CustodyRouter, KeyCustody as _, WalletMaterial};
        let router = Arc::new(CustodyRouter::plain());
        let slot = test_slot();
        let reloadable = CustodyReloadable::new(Arc::clone(&router), Arc::clone(&slot));
        let both = CustodyConfig {
            enabled: vec![CustodyBackend::Plain, CustodyBackend::Snp],
            default: CustodyBackend::Plain,
        };
        assert_eq!(
            save(&reloadable, &both, &defaults_of::<CustodyConfig>()).await,
            0
        );
        let snp = slot.backend().expect("started");
        let spend =
            monero::PublicKey::from_private_key(&monero::PrivateKey::from_slice(&[2; 32]).unwrap());
        let sealed = snp
            .seal(&WalletMaterial::new([1; 32], spend.to_bytes()))
            .await
            .unwrap();
        let store = router.unseal_and_register_in("snp", &sealed).await.unwrap();

        let snp_default = CustodyConfig {
            default: CustodyBackend::Snp,
            ..both.clone()
        };
        save(&reloadable, &snp_default, &both).await;
        assert!(Arc::ptr_eq(&snp, &slot.backend().unwrap()));
        assert_eq!(router.backend_of(store).as_deref(), Some("snp"));
        assert_eq!(router.default_backend(), "snp");
        assert!(!router.takes_raw_keys_in("snp"));
        assert!(router.takes_raw_keys_in("plain"));
    }

    /// An snp backend that can't start (here: no product set) is enabled as
    /// a stand-in that says why, with a warning; plain carries on.
    #[tokio::test]
    async fn an_snp_backend_that_cannot_start_says_why_and_plain_carries_on() {
        use crate::key_custody::{CustodyRouter, KeyCustody as _};
        let store = crate::store::Store::open_in_memory().unwrap().into_shared();
        let slot = Arc::new(crate::key_custody::SnpSlot::new(
            defaults_of::<SnpBootConfig>().snp_config(),
            Arc::new(snp_attest::guest::SevGuest::new("/nonexistent/sev-guest")),
            Arc::new(crate::key_custody::StoreWraps(store)),
        ));
        let router = Arc::new(CustodyRouter::plain());
        let reloadable = CustodyReloadable::new(Arc::clone(&router), slot);
        let both = CustodyConfig {
            enabled: vec![CustodyBackend::Plain, CustodyBackend::Snp],
            default: CustodyBackend::Plain,
        };
        assert_eq!(
            save(&reloadable, &both, &defaults_of::<CustodyConfig>()).await,
            1
        );
        let health = router.backend_health().await;
        assert_eq!(health[0], ("plain".to_owned(), None));
        assert_eq!(health[1].0, "snp");
        assert!(
            health[1].1.as_deref().unwrap().contains("snp_product"),
            "{health:?}"
        );
    }
}
