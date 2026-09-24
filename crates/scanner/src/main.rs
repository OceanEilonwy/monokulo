//! Boot sequence: open storage, ensure an instance admin token exists, bootstrap
//! nothing automatically (provisioning the self-hosted tenant is now an explicit
//! one-time `--bootstrap-wallet` command, not something every boot re-checks -
//! see `cli::Action::BootstrapWallet`), register every tenant's wallet with
//! `KeyCustody`, then run the chain scanner (once per configured network), the
//! webhook delivery loop, and the HTTP server concurrently.
//!
//! Every runtime-configurable setting (Monero node endpoints, confirmation/
//! expiry thresholds, rate limits, webhook policy, ...) is now read from the
//! `settings` table via `scanner::settings` - `env > database > default`, see
//! that module's own doc comment - rather than a TOML config file read once at
//! boot. There is no config file any more; the only thing this binary itself
//! needs to be told is where its own database lives (`cli::database_path`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use key_custody_service::client::SocketKeyCustody;
use monero::Network;
use scanner::cli::{self, Action};
use scanner::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use scanner::daemon_rpc::RpcDaemonClient;
use scanner::http::instance_admin::ensure_admin_token_seeded;
use scanner::http::rate_limit::RateLimiter;
use scanner::http::{build_router, now_unix, AppState};
use scanner::key_custody::{KeyCustody, PlainKeyCustody, WalletHandle};
use scanner::local_admin;
use scanner::network::network_str;
use scanner::scanner::{revalidate_recent_double_spend_voids, run_scan_tick};
use scanner::scanner_status::{self, ScannerStatusMap};
use scanner::settings;
use scanner::store::{SharedStore, Store};
use scanner::webhook_delivery::run_delivery_tick;
use shared::supervise::supervise;

fn open_store() -> Store {
    let db_path = cli::database_path();
    Store::open_file(&db_path.to_string_lossy()).unwrap_or_else(|e| {
        eprintln!("failed to open database at {}: {e}", db_path.display());
        std::process::exit(1);
    })
}

#[tokio::main]
async fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let action = cli::parse_args(&raw).unwrap_or_else(|e| {
        eprintln!("{e}\n\nRun with --help for usage.");
        std::process::exit(1);
    });

    let strict_tls = match action {
        Action::Help => {
            print!("{}", cli::HELP_TEXT);
            std::process::exit(0);
        }
        Action::RotateSecret { pk } => {
            let store = open_store();
            match local_admin::rotate_secret(&store, pk.as_deref()) {
                Ok((pk, secret)) => {
                    println!("Tenant: {pk}");
                    println!("New secret: {secret}");
                    println!("(shown once - store it now; the old secret no longer works)");
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Action::ShowTenant { pk } => {
            let store = open_store();
            match local_admin::show_tenant(&store, pk.as_deref()) {
                Ok(s) => {
                    println!("Public key:            {}", s.public_key);
                    println!("Network:               {}", s.network);
                    println!("Primary address:       {}", s.primary_address);
                    println!(
                        "Allowed origins:       {}",
                        if s.allowed_origins.is_empty() { "(none configured)".to_string() } else { s.allowed_origins.join(", ") }
                    );
                    println!("Confirmations required: {}", s.confirmations_required);
                    println!(
                        "Zero-conf ceiling:     {}",
                        s.zero_conf_max_piconero.map(|p| format!("{p} piconero")).unwrap_or_else(|| "(disabled)".to_string())
                    );
                    println!("Order expiry:          {} minutes", s.order_expiry_seconds / 60);
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Action::BootstrapWallet(args) => {
            let store = open_store();
            let backend: String = settings::get(&store, &settings::KEY_CUSTODY_BACKEND);
            let key_custody = build_key_custody(&store).await;
            match local_admin::bootstrap_wallet(&store, &key_custody, &backend, args).await {
                Ok(created) => {
                    println!(
                        "bootstrapped self-hosted tenant: public_key={} (save this - it goes in your site's JS)",
                        created.tenant.public_key
                    );
                    println!("bootstrap admin secret: {} (shown once - store it now, e.g. in a password manager)", created.secret_token);
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Action::RunServer { strict_tls } => strict_tls,
    };

    let store = open_store().into_shared();

    if let Some(token) = ensure_admin_token_seeded(&store.lock().unwrap()) {
        println!(
            "==> generated a new instance admin token (shown once - it is stored only as a hash from here on):\n    {token}\n\
             Set the SCANNER_ADMIN_TOKEN environment variable to this value on future boots if you'd rather manage it \
             that way than let it live in the database."
        );
    }

    let key_custody: Arc<dyn KeyCustody> = build_key_custody(&store.lock().unwrap()).await;
    let key_custody_backend: String = settings::get(&store.lock().unwrap(), &settings::KEY_CUSTODY_BACKEND);

    // One daemon client per configured network (§DESIGN.md §7) - a single instance
    // can hold mainnet tenants for real customers alongside stagenet/testnet
    // tenants for testing, each scanned against its own node. Each network's client
    // is a `FallbackDaemonClient` wrapping its primary node plus any configured
    // fallbacks, so a single flaky/down public node doesn't stop scanning that
    // network - see `daemon_fallback`'s own doc comment for the failover policy.
    let daemons: HashMap<Network, Arc<FallbackDaemonClient>> = build_daemon_clients(&store.lock().unwrap(), strict_tls);
    if daemons.is_empty() {
        // A warning, not a hard exit: the server still has to come up far enough to
        // serve the instance-admin settings API (`http::instance_admin`) itself,
        // since `POST /api/v1/admin/settings` (unlike every scalar setting) is the
        // *only* way to configure `monero_node.<network>` at all - there is no
        // environment-variable override for it (it's a structured JSON value, not
        // a single scalar). Refusing to boot here on a genuinely fresh install
        // would make that endpoint permanently unreachable - the exact
        // chicken-and-egg problem a settings-driven (rather than config-file-at-
        // boot) model has to avoid. The scanner/webhook-delivery loops below run
        // fine with zero configured networks (they simply do nothing each tick,
        // real behavior already exercised by every test that spawns a harness
        // with no daemon at all).
        eprintln!(
            "warning: no Monero node is configured for any network (mainnet/stagenet/testnet) yet - the server \
             is starting anyway, but no chain scanning happens until you configure at least one via \
             POST /api/v1/admin/settings (monero_node.<network>)"
        );
    }
    let configured_networks: Arc<HashSet<Network>> = Arc::new(daemons.keys().copied().collect());
    let daemons = Arc::new(daemons);
    let scanner_status = scanner_status::new_scanner_status_map();

    let mempool_poll_interval_ms: u64 = settings::get(&store.lock().unwrap(), &settings::PAYMENT_MEMPOOL_POLL_INTERVAL_MS);
    // Rounds down to whole seconds purely for the status page's own
    // "expected every Ns" display - the scan loop itself still sleeps the
    // real, precise millisecond value (`poll_interval` below), this is never
    // used to drive timing.
    let scan_poll_interval_secs = mempool_poll_interval_ms / 1000;

    let wallet_handles = Arc::new(RwLock::new(register_all_tenants(&store, &key_custody).await));

    let rate_limit_per_ip_per_min: u32 = settings::get(&store.lock().unwrap(), &settings::SERVER_RATE_LIMIT_PER_IP_PER_MIN);
    let rate_limit_per_token_per_min: u32 = settings::get(&store.lock().unwrap(), &settings::SERVER_RATE_LIMIT_PER_TOKEN_PER_MIN);
    let expired_order_grace_period_minutes: i64 =
        settings::get(&store.lock().unwrap(), &settings::PAYMENT_EXPIRED_ORDER_GRACE_PERIOD_MINUTES);

    let app_state = AppState {
        store: store.clone(),
        key_custody: key_custody.clone(),
        key_custody_backend,
        wallet_handles: wallet_handles.clone(),
        rate_limiter: Arc::new(RateLimiter::new(rate_limit_per_ip_per_min)),
        admin_rate_limiter: Arc::new(RateLimiter::new(rate_limit_per_token_per_min)),
        configured_networks,
        daemons: daemons.clone(),
        scanner_status: scanner_status.clone(),
        scan_poll_interval_secs,
        expired_order_grace_period_seconds: expired_order_grace_period_minutes * 60,
    };

    let allow_private_urls: bool = settings::get(&store.lock().unwrap(), &settings::WEBHOOKS_ALLOW_PRIVATE_URLS);
    let delivery_timeout_ms: u64 = settings::get(&store.lock().unwrap(), &settings::WEBHOOKS_DELIVERY_TIMEOUT_MS);
    let delivery_max_attempts: u32 = settings::get(&store.lock().unwrap(), &settings::WEBHOOKS_MAX_ATTEMPTS);
    let delivery_store = store.clone();
    supervise("webhook delivery", move || {
        run_webhook_delivery_loop(delivery_store.clone(), allow_private_urls, delivery_timeout_ms, delivery_max_attempts)
    });

    let reorg_check_depth: u64 = settings::get(&store.lock().unwrap(), &settings::PAYMENT_REORG_CHECK_DEPTH);
    let expired_order_grace_period_seconds = expired_order_grace_period_minutes * 60;
    let poll_interval = Duration::from_millis(mempool_poll_interval_ms);

    // Cloned before the scanner loop's own `move` closure below consumes the
    // originals - its own, much slower loop (see `run_double_spend_revalidation_loop`'s
    // doc comment for why it is never folded into the scan-tick loop itself).
    let revalidation_store = store.clone();
    let revalidation_daemons = daemons.clone();
    supervise("double-spend revalidation", move || {
        run_double_spend_revalidation_loop(revalidation_store.clone(), revalidation_daemons.clone())
    });

    supervise("chain scanner", move || {
        run_scanner_loop(
            store.clone(),
            key_custody.clone(),
            daemons.clone(),
            wallet_handles.clone(),
            reorg_check_depth,
            expired_order_grace_period_seconds,
            poll_interval,
            scanner_status.clone(),
        )
    });

    let bind: String = settings::get(&app_state.store.lock().unwrap(), &settings::SERVER_BIND);
    let max_body_bytes: usize = settings::get(&app_state.store.lock().unwrap(), &settings::SERVER_MAX_BODY_BYTES);
    let router = build_router(app_state, max_body_bytes);
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("failed to bind server address");
    println!("moneropay listening on {bind}");
    axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await
        .expect("server error");
}

/// Builds one `FallbackDaemonClient` per configured `[monero_node.<network>]`
/// network - a real `RpcDaemonClient` (its own `reqwest::Client`) per primary
/// node plus its configured fallbacks. Called twice at boot (`main`): once for
/// the live scanner's own `daemons`, once more for `rescan_daemons` - see that
/// call site's own comment for why these must be two physically separate sets
/// of HTTP clients rather than one shared `Arc`, even though both are built
/// from identical node configuration.
fn build_daemon_clients(store: &Store, strict_tls: bool) -> HashMap<Network, Arc<FallbackDaemonClient>> {
    settings::NETWORKS
        .iter()
        .filter_map(|&network_name| {
            let network = scanner::network::parse_network(network_name).ok()?;
            let node_setting = settings::monero_node_setting(store, network_name)?;
            let build = |host: &str, port: u16, ssl: bool, accept_self_signed_certs: bool| {
                let accept_self_signed = accept_self_signed_certs && !strict_tls;
                RpcDaemonClient::new(host, port, ssl, accept_self_signed)
                    .unwrap_or_else(|e| panic!("failed to build Monero daemon RPC client for {network:?}: {e}"))
            };
            let mut nodes = vec![FallbackNode {
                label: format!("{}:{}", node_setting.host, node_setting.port),
                client: Arc::new(build(&node_setting.host, node_setting.port, node_setting.ssl, node_setting.accept_self_signed_certs)),
            }];
            for fallback in &node_setting.fallbacks {
                nodes.push(FallbackNode {
                    label: format!("{}:{}", fallback.host, fallback.port),
                    client: Arc::new(build(&fallback.host, fallback.port, fallback.ssl, fallback.accept_self_signed_certs)),
                });
            }
            Some((network, Arc::new(FallbackDaemonClient::new(nodes))))
        })
        .collect()
}

// `supervise` itself moved to `shared::supervise` (`docs/fx_refactor.md`
// Phase 1.1, imported at the top of this file) so monokulo's own
// background loops (its Coingecko exchange-rate refresh loop) can reuse
// the exact same panic-catching restart shape - see that module's own doc
// comment for the full reasoning (still applies unchanged: a bare
// `tokio::spawn` of an infinite loop panicking would silently kill chain
// scanning/webhook delivery while the HTTP server keeps serving happily,
// with no signal short of a merchant eventually complaining).

/// Builds the one `Arc<dyn KeyCustody>` this whole process shares - `"plain"`
/// (the default, unchanged behavior: key material lives in this process) or
/// `"socket"` (WBS 2.1.3: forwards every call to a separate `key-custody-server`
/// process). The instance-admin settings API's own save-time validation
/// (`http::instance_admin::validate_scalar` plus its cross-field check) has
/// already confirmed `key_custody.backend` is one of these two values, and that
/// `socket_path` is present under `"socket"`, *for whatever was saved through
/// it* - but a value reaching this function could still be a stale default or a
/// hand-edited row that predates that check, so the `expect` below documents the
/// invariant rather than silently producing a `SocketKeyCustody::connect` call
/// against an empty path one layer down, with a worse error.
///
/// There is exactly one `KeyCustody` for the whole running instance - not one per
/// tenant. `tenants.key_custody_backend` (a column on each tenant row, unrelated
/// to this setting despite the similar name) looks at first glance like it might
/// support per-tenant backend choice instead, but it doesn't - see the (still
/// accurate) reasoning `docs/DESIGN.md` §8.1 records for why: it exists so a
/// *future* migration to a different backend can detect a mismatch between a
/// stored row's sealing backend and the backend actually running, not to select
/// one. This setting makes the *whole process* pick one backend.
async fn build_key_custody(store: &Store) -> Arc<dyn KeyCustody> {
    let backend: String = settings::get(store, &settings::KEY_CUSTODY_BACKEND);
    match backend.as_str() {
        "socket" => {
            let socket_path: String = settings::get(store, &settings::KEY_CUSTODY_SOCKET_PATH);
            if socket_path.trim().is_empty() {
                eprintln!(
                    "key_custody.backend is \"socket\" but key_custody.socket_path is missing (or empty) - set it via \
                     POST /api/v1/admin/settings or the SCANNER_KEY_CUSTODY_SOCKET_PATH environment variable"
                );
                std::process::exit(1);
            }
            Arc::new(connect_socket_key_custody(&socket_path).await)
        }
        _ => Arc::new(PlainKeyCustody::default()),
    }
}

/// How many times [`connect_socket_key_custody`] retries a failed connect before
/// giving up, and how long it sleeps between attempts. 10 attempts, 500ms apart,
/// bound the whole retry window to under 5 seconds - generous next to an ordinary
/// process-startup race, tight next to how long an operator would tolerate a
/// service hanging at boot before assuming something is actually wrong.
const KEY_CUSTODY_CONNECT_ATTEMPTS: u32 = 10;
const KEY_CUSTODY_CONNECT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Connects to a `key-custody-server` at `socket_path`, retrying briefly before
/// giving up - never panicking, never hanging indefinitely. See this repo's own
/// prior art (`key-custody-server/tests/socket_key_custody.rs::connect_with_retry`)
/// for why a short, bounded retry loop - not a single attempt, and not retrying
/// forever - is the right shape for this specific startup race between two
/// independently started processes.
async fn connect_socket_key_custody(socket_path: &str) -> SocketKeyCustody {
    let mut last_err = None;
    for attempt in 1..=KEY_CUSTODY_CONNECT_ATTEMPTS {
        match SocketKeyCustody::connect(socket_path).await {
            Ok(client) => return client,
            Err(e) => {
                if attempt < KEY_CUSTODY_CONNECT_ATTEMPTS {
                    eprintln!(
                        "key-custody-server not reachable yet at {socket_path} (attempt \
                         {attempt}/{KEY_CUSTODY_CONNECT_ATTEMPTS}): {e} - retrying in \
                         {KEY_CUSTODY_CONNECT_RETRY_DELAY:?}"
                    );
                    tokio::time::sleep(KEY_CUSTODY_CONNECT_RETRY_DELAY).await;
                }
                last_err = Some(e);
            }
        }
    }
    let last_err = last_err.expect("loop always records an error before exiting without returning");
    eprintln!(
        "failed to connect to key-custody-server at {socket_path} after \
         {KEY_CUSTODY_CONNECT_ATTEMPTS} attempts (~{:?} total): {last_err}\n\
         is key-custody-server running, and is this the socket path it was started with?",
        KEY_CUSTODY_CONNECT_RETRY_DELAY * (KEY_CUSTODY_CONNECT_ATTEMPTS - 1)
    );
    std::process::exit(1);
}

/// Eagerly registers every non-disabled tenant's sealed key material with
/// `KeyCustody`, so `AppState::wallet_handles` starts populated rather than relying
/// solely on the lazy on-first-use path in `http::resolve_wallet_handle`.
async fn register_all_tenants(store: &SharedStore, key_custody: &Arc<dyn KeyCustody>) -> HashMap<String, WalletHandle> {
    let tenants = store.lock().unwrap().list_active_tenants().expect("failed to list tenants at boot");
    let mut handles = HashMap::new();
    for tenant in tenants {
        match key_custody.unseal_and_register(&tenant.sealed_key_material).await {
            Ok(handle) => {
                handles.insert(tenant.id, handle);
            }
            Err(e) => eprintln!("failed to register tenant {} with key custody: {e}", tenant.id),
        }
    }
    handles
}

async fn run_webhook_delivery_loop(
    store: SharedStore,
    allow_private_urls: bool,
    timeout_ms: u64,
    max_attempts: u32,
) {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("failed to build webhook HTTP client");
    let timeout = Duration::from_millis(timeout_ms);

    loop {
        // `run_delivery_tick` locks the store only around its own brief synchronous
        // sections, never across the outbound HTTP `.await`s it performs per
        // delivery - see its doc comment for why that matters.
        if let Err(e) =
            run_delivery_tick(&store, &client, allow_private_urls, timeout, max_attempts, now_unix()).await
        {
            eprintln!("webhook delivery tick failed: {e}");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// Runs one `run_scan_tick` per configured network, per round. Re-reads
/// `wallet_handles` fresh every round (rather than a boot-time snapshot) so a
/// tenant created at runtime via the admin API - on any network - is picked up
/// without a restart; `run_scan_tick` itself filters that full list down to the
/// network it was called for (see its doc comment).
///
/// Sequential across networks, not concurrent: with typically one or two networks
/// configured, the simplicity is worth more than the parallelism, but a slow or
/// unresponsive node on one network delaying the next network's tick within the
/// same round is a real, accepted tradeoff worth revisiting if a deployment ever
/// configures enough networks (or gets an unreliable enough node) for it to matter.
async fn run_scanner_loop(
    store: SharedStore,
    key_custody: Arc<dyn KeyCustody>,
    daemons: Arc<HashMap<Network, Arc<FallbackDaemonClient>>>,
    wallet_handles: Arc<RwLock<HashMap<String, WalletHandle>>>,
    reorg_check_depth: u64,
    expired_order_grace_period_seconds: i64,
    poll_interval: Duration,
    scanner_status: ScannerStatusMap,
) {
    loop {
        let tenants: Vec<(String, WalletHandle)> =
            wallet_handles.read().unwrap().iter().map(|(id, h)| (id.clone(), *h)).collect();
        for (network, daemon) in daemons.iter() {
            let started_at = now_unix();
            let result = run_scan_tick(
                &store,
                key_custody.as_ref(),
                daemon.as_ref(),
                network_str(*network),
                &tenants,
                reorg_check_depth,
                expired_order_grace_period_seconds,
            )
            .await;
            let finished_at = now_unix();
            if let Err(e) = &result {
                eprintln!("scan tick failed for {network:?}: {e}");
            }
            scanner_status::record_tick(&scanner_status, *network, started_at, finished_at, tenants.len(), &result);
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// How often [`revalidate_recent_double_spend_voids`] sweeps each network - much
/// slower than the scan-tick/webhook-delivery loops above, since it exists to catch
/// a rare event (a wrongly-voided payment) within a wide, forgiving window
/// (`scanner::DOUBLE_SPEND_RECHECK_WINDOW_SECS`), not to react quickly. See that
/// function's own doc comment for why this is deliberately not folded into
/// `run_scanner_loop`'s tight per-second cadence.
const DOUBLE_SPEND_REVALIDATION_INTERVAL: Duration = Duration::from_secs(5 * 60);

async fn run_double_spend_revalidation_loop(
    store: SharedStore,
    daemons: Arc<HashMap<Network, Arc<FallbackDaemonClient>>>,
) {
    loop {
        for (network, daemon) in daemons.iter() {
            match revalidate_recent_double_spend_voids(&store, daemon.as_ref(), network_str(*network), now_unix()).await {
                Ok(recovered) if !recovered.is_empty() => {
                    println!(
                        "double-spend revalidation on {network:?} reversed {} previously-voided payment(s): {recovered:?}",
                        recovered.len()
                    );
                }
                Ok(_) => {}
                Err(e) => eprintln!("double-spend revalidation failed for {network:?}: {e}"),
            }
        }
        tokio::time::sleep(DOUBLE_SPEND_REVALIDATION_INTERVAL).await;
    }
}
