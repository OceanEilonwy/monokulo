//! Boot sequence: load config, open storage, bootstrap the self-hosted tenant if
//! configured, register every tenant's wallet with `KeyCustody`, then run the
//! chain scanner (once per configured network), the webhook delivery loop, and the
//! HTTP server concurrently.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use key_custody_service::client::SocketKeyCustody;
use monero::Network;
use moneropay_core::cli::{self, Action};
use moneropay_core::config::{Config, KeyCustodyConfig};
use moneropay_core::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use moneropay_core::daemon_rpc::RpcDaemonClient;
use moneropay_core::http::rate_limit::RateLimiter;
use moneropay_core::http::{build_router, now_unix, AppState};
use moneropay_core::init_wizard;
use moneropay_core::key_custody::{KeyCustody, PlainKeyCustody, WalletHandle, WalletMaterial};
use moneropay_core::local_admin;
use moneropay_core::network::network_str;
use moneropay_core::scanner::{revalidate_recent_double_spend_voids, run_scan_tick};
use moneropay_core::scanner_status::{self, ScannerStatusMap};
use moneropay_core::store::{NewTenant, SharedStore, Store};
use moneropay_core::webhook_delivery::run_delivery_tick;
use shared::supervise::supervise;

/// All argv parsing lives in `moneropay_core::cli` (a lib module, unit-testable
/// the normal way) - this function is just the untestable-by-nature glue that
/// turns its result into process exit codes and side effects (stdin/stdout,
/// running the wizard, `std::process::exit`). `async` only because
/// `init_wizard::run_interactive` now makes a real network call when its "test
/// this node" prompt is accepted; already fine to await here since `main` is
/// itself async.
async fn dispatch_args() -> (std::path::PathBuf, bool) {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse_args(&raw) {
        Ok(Action::Help) => {
            print!("{}", cli::HELP_TEXT);
            std::process::exit(0);
        }
        Ok(Action::Init(init_args)) => {
            let path = init_args.config_path_override.map(std::path::PathBuf::from).unwrap_or_else(init_wizard::default_config_path);
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            let mut output = std::io::stdout();
            match init_wizard::run_interactive(&mut input, &mut output, init_args.network, &path).await {
                Ok(init_wizard::WizardOutcome::Written(_)) => std::process::exit(0),
                Ok(init_wizard::WizardOutcome::Cancelled) => std::process::exit(1),
                Err(e) => {
                    eprintln!("setup wizard failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        Ok(Action::RotateSecret { config_path, pk }) => {
            let store = open_local_store(&config_path);
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
        Ok(Action::ShowTenant { config_path, pk }) => {
            let store = open_local_store(&config_path);
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
        Ok(Action::RunServer { config_path, strict_tls }) => (config_path, strict_tls),
        Err(e) => {
            eprintln!("{e}\n\nRun with --help for usage.");
            std::process::exit(1);
        }
    }
}

/// Opens the database co-located with `config_path` (see
/// `init_wizard::database_path_for`) for the local-admin commands, which need
/// direct DB access and nothing else from the config file itself.
fn open_local_store(config_path: &std::path::Path) -> Store {
    let db_path = init_wizard::database_path_for(config_path);
    Store::open_file(&db_path.to_string_lossy()).unwrap_or_else(|e| {
        eprintln!("failed to open database at {}: {e}", db_path.display());
        std::process::exit(1);
    })
}

#[tokio::main]
async fn main() {
    let (config_path, strict_tls) = dispatch_args().await;
    let config_path_display = config_path.display().to_string();
    let config = Config::from_file(&config_path.to_string_lossy()).unwrap_or_else(|e| {
        eprintln!("failed to load config from {config_path_display}: {e}");
        std::process::exit(1);
    });
    if let Err(e) = config.validate() {
        eprintln!("invalid config: {e}");
        std::process::exit(1);
    }

    let db_path = init_wizard::database_path_for(&config_path);
    let store = Store::open_file(&db_path.to_string_lossy()).expect("failed to open database").into_shared();
    // `Config::validate` has already confirmed `key_custody.backend` is one of
    // these two known values (and, for "socket", that `socket_path` is present
    // and non-empty) - `build_key_custody` below is a plain dispatch on an
    // already-validated field, not a second round of validation.
    let key_custody: Arc<dyn KeyCustody> = build_key_custody(&config.key_custody).await;

    // One daemon client per configured network (§DESIGN.md §7) - a single instance
    // can hold mainnet tenants for real customers alongside stagenet/testnet
    // tenants for testing, each scanned against its own node. Each network's client
    // is a `FallbackDaemonClient` wrapping its primary node plus any configured
    // `fallbacks`, so a single flaky/down public node doesn't stop scanning that
    // network - see `daemon_fallback`'s own doc comment for the failover policy.
    // Concrete `Arc<FallbackDaemonClient>`, not `Arc<dyn MoneroDaemonClient>`:
    // `AppState::daemons` (the status page, `http/status_page.rs`) needs
    // `FallbackDaemonClient`'s own `nodes()`/`current_index()` accessors to
    // report on each configured node individually, which the trait object
    // alone can't expose. Every real construction path here always
    // produces a `FallbackDaemonClient` anyway (see the loop body below),
    // so this reflects what's actually built, not an artificial
    // narrowing.
    let daemons: HashMap<Network, Arc<FallbackDaemonClient>> = config
        .monero_node
        .iter()
        .map(|(network, node_config)| {
            let build = |host: &str, port: u16, ssl: bool, accept_self_signed_certs: bool| {
                let accept_self_signed = accept_self_signed_certs && !strict_tls;
                RpcDaemonClient::new(host, port, ssl, accept_self_signed)
                    .unwrap_or_else(|e| panic!("failed to build Monero daemon RPC client for {network:?}: {e}"))
            };
            let mut nodes = vec![FallbackNode {
                label: format!("{}:{}", node_config.host, node_config.port),
                client: Arc::new(build(
                    &node_config.host,
                    node_config.port,
                    node_config.ssl,
                    node_config.accept_self_signed_certs,
                )),
            }];
            for fallback in &node_config.fallbacks {
                nodes.push(FallbackNode {
                    label: format!("{}:{}", fallback.host, fallback.port),
                    client: Arc::new(build(
                        &fallback.host,
                        fallback.port,
                        fallback.ssl,
                        fallback.accept_self_signed_certs,
                    )),
                });
            }
            let client = Arc::new(FallbackDaemonClient::new(nodes));
            (network, client)
        })
        .collect();
    let configured_networks: Arc<HashSet<Network>> = Arc::new(daemons.keys().copied().collect());
    let daemons = Arc::new(daemons);
    let scanner_status = scanner_status::new_scanner_status_map();
    // Rounds down to whole seconds purely for the status page's own
    // "expected every Ns" display - the scan loop itself still sleeps the
    // real, precise `config.payment.mempool_poll_interval_ms` value
    // (`poll_interval` below), this is never used to drive timing.
    let scan_poll_interval_secs = config.payment.mempool_poll_interval_ms / 1000;

    bootstrap_self_hosted_tenant(&store, &key_custody, &config).await;
    let wallet_handles = Arc::new(RwLock::new(register_all_tenants(&store, &key_custody).await));
    resume_running_rescans(&store, &key_custody, &daemons, &wallet_handles).await;

    let app_state = AppState {
        store: store.clone(),
        key_custody: key_custody.clone(),
        key_custody_backend: config.key_custody.backend.clone(),
        wallet_handles: wallet_handles.clone(),
        rate_limiter: Arc::new(RateLimiter::new(config.server.rate_limit_per_ip_per_min)),
        admin_rate_limiter: Arc::new(RateLimiter::new(config.server.rate_limit_per_token_per_min)),
        configured_networks,
        daemons: daemons.clone(),
        scanner_status: scanner_status.clone(),
        scan_poll_interval_secs,
        default_rescan_lookback_days: config.payment.default_rescan_lookback_days,
        max_rescan_lookback_days: config.payment.max_rescan_lookback_days,
        expired_order_grace_period_seconds: config.payment.expired_order_grace_period_minutes * 60,
    };

    let allow_private_urls = config.webhooks.allow_private_urls;
    let delivery_timeout_ms = config.webhooks.delivery_timeout_ms;
    let delivery_max_attempts = config.webhooks.max_attempts;
    let delivery_store = store.clone();
    supervise("webhook delivery", move || {
        run_webhook_delivery_loop(
            delivery_store.clone(),
            allow_private_urls,
            delivery_timeout_ms,
            delivery_max_attempts,
        )
    });

    let reorg_check_depth = config.payment.reorg_check_depth;
    let expired_order_grace_period_seconds = config.payment.expired_order_grace_period_minutes * 60;
    let poll_interval = Duration::from_millis(config.payment.mempool_poll_interval_ms);

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

    let router = build_router(app_state, config.server.max_body_bytes);
    let listener = tokio::net::TcpListener::bind(&config.server.bind).await.expect("failed to bind server address");
    println!("moneropay listening on {}", config.server.bind);
    axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await
        .expect("server error");
}

// `supervise` itself moved to `shared::supervise` (`docs/fx_refactor.md`
// Phase 1.1, imported at the top of this file) so control-plane's own
// background loops (its Coingecko exchange-rate refresh loop) can reuse
// the exact same panic-catching restart shape - see that module's own doc
// comment for the full reasoning (still applies unchanged: a bare
// `tokio::spawn` of an infinite loop panicking would silently kill chain
// scanning/webhook delivery while the HTTP server keeps serving happily,
// with no signal short of a merchant eventually complaining).

/// Builds the one `Arc<dyn KeyCustody>` this whole process shares - `"plain"`
/// (the default, unchanged behavior: key material lives in this process) or
/// `"socket"` (WBS 2.1.3: forwards every call to a separate `key-custody-server`
/// process). `Config::validate` has already confirmed `cfg.backend` is one of
/// these two values, so the `_` arm below covers only "plain" in practice -
/// written as a catch-all rather than an explicit `"plain" =>` purely so a config
/// that somehow reaches this function unvalidated (there is no such code path
/// today, but nothing enforces that at the type level) degrades to the always-
/// safe in-process default instead of panicking on an unmatched pattern.
///
/// There is exactly one `KeyCustody` for the whole running instance - not one per
/// tenant. `tenants.key_custody_backend` (a column on each tenant row, unrelated
/// to this config field despite the similar name) looks at first glance like it
/// might support per-tenant backend choice instead, but it doesn't: it's read
/// back into `Tenant`/`NewTenant` (`store.rs`) and round-tripped through every
/// tenant-creation code path, but never *matched on* anywhere in this codebase to
/// select a `KeyCustody` implementation - grepped for every read site, not just
/// write sites, to confirm this before writing this comment. `migrations/
/// 0001_init.sql`'s own comment on the column, and `docs/DESIGN.md` §8.1, agree:
/// it exists so a *future* migration to a different backend can detect a
/// mismatch between a stored row's sealing backend and the backend actually
/// running (`unseal_and_register` given bytes sealed by a different backend
/// should fail loudly, not misinterpret them - `docs/TESTING.md`'s own gap list
/// already flags that check as not yet implemented, unrelated to this task). It
/// was never wired as a dispatch key, and nothing here adds that: this config
/// option makes the *whole process* pick one backend.
async fn build_key_custody(cfg: &KeyCustodyConfig) -> Arc<dyn KeyCustody> {
    match cfg.backend.as_str() {
        "socket" => {
            // `Config::validate` already rejected `backend = "socket"` with no
            // `socket_path` - this `expect` documents that invariant rather than
            // silently falling back to an empty path `SocketKeyCustody::connect`
            // would just fail on anyway, one layer down, with a worse error.
            let socket_path = cfg
                .socket_path
                .as_deref()
                .expect("Config::validate should have required socket_path for backend = \"socket\"");
            Arc::new(connect_socket_key_custody(socket_path).await)
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
/// giving up - never panicking, never hanging indefinitely.
///
/// **The choice this function embodies, and why it's the right one here.**
/// `key-custody-server`'s own binary doc comment (`key-custody-server/src/bin/
/// key-custody-server.rs`) deliberately pushes restart-policy ownership onto an
/// external process supervisor rather than building any self-restart logic into
/// that binary - "that supervisor is what should own restart policy... not this
/// binary guessing at them." It would be easy to read that as an argument for
/// this function failing on the very first failed connect too, on the theory
/// that *this* process's own supervisor (systemd, a container orchestrator)
/// should likewise own recovering from "the socket isn't there yet." That
/// argument proves too much, though: it's about who restarts a process that has
/// genuinely died, not about how a client should react to an utterly ordinary
/// startup race between two *independently started* processes that both need to
/// be up before either is fully useful - exactly the shape the task that added
/// this config option calls out by name ("normal at boot if two systemd units...
/// start close together"). A single failed connect attempt cannot tell the
/// difference between "the server process hasn't been scheduled onto a thread
/// yet" (typically resolved within milliseconds) and "the server is genuinely
/// down" - failing fast on the former would mean this process's own supervisor
/// has to restart *it* too, adding a second restart-and-backoff cycle on top of
/// whatever the first one already costs, for a race that a few hundred
/// milliseconds of patience resolves for free. This project's own prior art
/// agrees: `key-custody-service`'s own integration test harness
/// (`key-custody-server/tests/socket_key_custody.rs::connect_with_retry`) hit
/// this exact race between spawning its in-process test server and dialing it,
/// and solved it the same way - a short, bounded retry loop, not a single
/// attempt. What this function does *not* do is retry forever, or silently swap
/// in `PlainKeyCustody` as a fallback: a `key-custody-server` that is still
/// unreachable after ~5 seconds is past "ordinary scheduling jitter" territory,
/// and the operator needs a loud, specific, actionable failure - which socket
/// path, how many attempts, the last real error - not a service that quietly
/// runs with key material back in this process (defeating the entire point of
/// choosing this backend) or one that hangs forever with no indication why.
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
    // `last_err` is always `Some` here: the loop only exits without an early
    // `return` after every one of `KEY_CUSTODY_CONNECT_ATTEMPTS` iterations has
    // taken the `Err` arm at least once (the `Ok` arm always returns
    // immediately), so this is a real invariant, not a defensive fallback for a
    // case that can't happen.
    let last_err = last_err.expect("loop always records an error before exiting without returning");
    eprintln!(
        "failed to connect to key-custody-server at {socket_path} after \
         {KEY_CUSTODY_CONNECT_ATTEMPTS} attempts (~{:?} total): {last_err}\n\
         is key-custody-server running, and is this the socket path it was started with?",
        KEY_CUSTODY_CONNECT_RETRY_DELAY * (KEY_CUSTODY_CONNECT_ATTEMPTS - 1)
    );
    std::process::exit(1);
}

/// Creates the one tenant a self-hosted deployment needs, from `[wallet]` in the
/// config - but only once, on first boot. Idempotent across restarts by checking
/// whether any tenant already exists first, rather than tracking a separate
/// "already bootstrapped" flag.
async fn bootstrap_self_hosted_tenant(store: &SharedStore, key_custody: &Arc<dyn KeyCustody>, config: &Config) {
    let Some(wallet) = &config.wallet else { return };
    let already_bootstrapped = store.lock().unwrap().count_tenants().unwrap_or(0) > 0;
    if already_bootstrapped {
        return;
    }

    let material = WalletMaterial::from_hex(&wallet.private_view_key, &wallet.public_spend_key)
        .expect("invalid [wallet] key material in config");
    let sealed = key_custody.seal(&material).await.expect("failed to seal bootstrap wallet material");

    let created = store
        .lock()
        .unwrap()
        .create_tenant(
            NewTenant {
                // Not hardcoded "plain": `tenants.key_custody_backend` records
                // which `KeyCustody` implementation actually produced
                // `sealed_key_material` (see `migrations/0001_init.sql`'s own
                // comment on the column), and as of this config option that is
                // no longer always "plain" - a bootstrap tenant created while
                // `key_custody.backend = "socket"` is configured was genuinely
                // sealed by the remote `key-custody-server`, not this process.
                key_custody_backend: config.key_custody.backend.clone(),
                sealed_key_material: sealed,
                primary_address: wallet.primary_address.clone(),
                network: wallet.network.clone(),
                allowed_origins: wallet.allowed_origins.clone(),
                confirmations_required: Some(config.payment.confirmations_required),
                zero_conf_max_piconero: config
                    .payment
                    .zero_conf_max_xmr
                    .as_deref()
                    .and_then(|s| shared::xmr_amount::parse_xmr_to_piconero(s).ok()),
                order_expiry_seconds: Some(config.payment.order_expiry_minutes * 60),
            },
            now_unix(),
        )
        .expect("failed to create bootstrap tenant");

    println!(
        "bootstrapped self-hosted tenant: public_key={} (save this - it goes in your site's JS)",
        created.tenant.public_key
    );
    println!(
        "bootstrap admin secret: {} (shown once - store it now, e.g. in a password manager)",
        created.secret_token
    );
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

/// Re-spawns `scanner::run_rescan_job` for every rescan job still `running` when
/// this process starts - the restart-resume half of `docs/order_rescan_wbs.md`
/// Phase 1.2's durability guarantee. A row left `running` when the previous process
/// stopped is not a terminal state (the table has no `interrupted` status); this is
/// what actually resumes it, reading `Store::list_running_rescans` once at boot
/// rather than polling for it - each job's own row is what tracks its progress from
/// here on, the same way `register_all_tenants` seeds `wallet_handles` once and then
/// relies on the running system to keep it current.
///
/// Only two things can make a still-`running` row unresumable, both of which mean
/// the world it was triggered against no longer exists at all: its tenant's wallet
/// isn't registered (`KeyCustody` lost its material, or the tenant was disabled
/// between trigger and this boot), or the tenant row itself is gone. Either is
/// marked `failed` rather than silently left `running` forever - a `running` row
/// that can never actually run again would otherwise permanently occupy that
/// tenant's one-job-at-a-time slot (WBS 1.2 decision 4).
async fn resume_running_rescans(
    store: &SharedStore,
    key_custody: &Arc<dyn KeyCustody>,
    daemons: &Arc<HashMap<Network, Arc<FallbackDaemonClient>>>,
    wallet_handles: &Arc<RwLock<HashMap<String, WalletHandle>>>,
) {
    let running = match store.lock().unwrap().list_running_rescans() {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("failed to list still-running rescan jobs at boot - none will be resumed: {e}");
            return;
        }
    };
    for job in running {
        let tenant = store.lock().unwrap().get_tenant_by_id(&job.tenant_id).ok().flatten();
        let Some(tenant) = tenant else {
            eprintln!("rescan {}: tenant {} no longer exists - cannot resume, marking failed", job.id, job.tenant_id);
            let _ = store.lock().unwrap().fail_rescan(&job.id, "tenant no longer exists", now_unix());
            continue;
        };
        let Some(handle) = wallet_handles.read().unwrap().get(&job.tenant_id).copied() else {
            eprintln!(
                "rescan {}: tenant {} has no registered wallet handle at boot - cannot resume, marking failed",
                job.id, job.tenant_id
            );
            let _ = store.lock().unwrap().fail_rescan(&job.id, "tenant wallet not registered at boot", now_unix());
            continue;
        };
        let Ok(network) = moneropay_core::network::parse_network(&tenant.network) else {
            eprintln!(
                "rescan {}: tenant {} has an unrecognized network {:?} - cannot resume, marking failed",
                job.id, job.tenant_id, tenant.network
            );
            let _ = store.lock().unwrap().fail_rescan(&job.id, "tenant network unrecognized", now_unix());
            continue;
        };
        let Some(daemon) = daemons.get(&network) else {
            eprintln!(
                "rescan {}: no daemon configured for tenant {}'s network {:?} - cannot resume, marking failed",
                job.id, job.tenant_id, network
            );
            let _ = store.lock().unwrap().fail_rescan(&job.id, "no daemon configured for tenant's network", now_unix());
            continue;
        };
        println!("resuming rescan {} for order {} from height {} to {}", job.id, job.order_id, job.current_height, job.to_height);
        moneropay_core::scanner::spawn_rescan_job(
            store.clone(),
            key_custody.clone(),
            daemon.clone() as Arc<dyn moneropay_core::daemon::MoneroDaemonClient>,
            handle,
            job.id,
        );
    }
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
