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

// See `lib.rs`: no panics in loop code. `main` itself may still exit at boot
// (a listener that can't bind), which is marked where it happens.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::collections::HashMap;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;

use scanner::cli::{self, Action};
use scanner::engine_settings::{
    migrate_key_custody_setting, CustodyConfig, CustodyReloadable, Daemons, EngineSettings, RuntimeConfig, StoreSettings,
};
use scanner::http::instance_admin::ensure_admin_token_seeded;
use scanner::http::rate_limit::RateLimiter;
use scanner::http::{build_router, AppState};
use scanner::key_custody::{CustodyRouter, KeyCustody, WalletHandle};
use scanner::local_admin;
use scanner::loops;
use scanner::scanner_status;
use scanner::store::{SharedStore, Store};
use shared::supervise::supervise;

/// A fixed outer ceiling on request bodies; `server.max_body_bytes` (the
/// live limit, task 2.6) is what normally applies, and its own range tops
/// out here.
const MAX_BODY_CEILING: usize = 16 * 1024 * 1024;

fn open_store() -> Store {
    let db_path = cli::database_path();
    Store::open_file(&db_path.to_string_lossy()).unwrap_or_else(|e| {
        eprintln!("failed to open database at {}: {e}", db_path.display());
        std::process::exit(1);
    })
}

/// Builds the runtime with `server.worker_threads` threads (task 2.8: read
/// before the runtime exists, so it applies at the next start), then runs.
fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let action = cli::parse_args(&raw).unwrap_or_else(|e| {
        eprintln!("{e}\n\nRun with --help for usage.");
        std::process::exit(1);
    });
    if matches!(action, Action::Help) {
        print!("{}", cli::HELP_TEXT);
        std::process::exit(0);
    }
    // Only the server reads its thread count; the one-off commands don't
    // need more than the default.
    let worker_threads = match action {
        Action::RunServer { .. } => {
            let store = open_store().into_shared();
            live_settings::read_sync::<RuntimeConfig>(&StoreSettings(store)).worker_threads
        }
        _ => 2,
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread().worker_threads(worker_threads).enable_all().build() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("failed to start the async runtime with {worker_threads} worker threads: {e}");
            std::process::exit(1);
        }
    };
    runtime.block_on(run(action));
}

#[allow(clippy::expect_used, reason = "boot-time: a listener that can't bind or a server that can't start ends the process")]
async fn run(action: Action) {

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
                    println!("Confirmations required: {}", s.confirmations_required);
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
            let store = open_store().into_shared();
            if let Err(e) = migrate_key_custody_setting(&store.lock()) {
                eprintln!("failed to move key custody settings to per-store custody: {e}");
                std::process::exit(1);
            }
            let custody = live_settings::read_sync::<CustodyConfig>(&StoreSettings(store.clone()));
            let backend = custody.default.as_str().to_string();
            let router = Arc::new(CustodyRouter::default());
            if let Err(e) = apply_custody(&router, &custody).await {
                eprintln!("{e}");
                std::process::exit(1);
            }
            let key_custody: Arc<dyn KeyCustody> = router;
            let bootstrapped = {
                let store = store.lock();
                local_admin::bootstrap_wallet(&store, &key_custody, &backend, args).await
            };
            match bootstrapped {
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

    if let Some(token) = ensure_admin_token_seeded(&store.lock()) {
        println!(
            "==> generated a new instance admin token (shown once - it is stored only as a hash from here on):\n    {token}\n\
             Set the SCANNER_ADMIN_TOKEN environment variable to this value on future boots if you'd rather manage it \
             that way than let it live in the database."
        );
    }

    match migrate_key_custody_setting(&store.lock()) {
        Ok(Some(done)) => println!("{done}"),
        Ok(None) => {}
        Err(e) => {
            eprintln!("failed to move key custody settings to per-store custody: {e}");
            std::process::exit(1);
        }
    }

    // Every setting, live (admin_settings_v2.md part 1). Node clients are
    // built into `daemons` from the saved node settings, and rebuilt whenever
    // they are saved; the rate limiter and the key custody backends (part 5)
    // follow their settings the same way.
    let daemons = Daemons::default();
    let admin_rate_limiter = Arc::new(RateLimiter::new(1));
    let router = Arc::new(CustodyRouter::default());
    let engine_settings =
        match EngineSettings::load(store.clone(), daemons.clone(), strict_tls, router.clone(), admin_rate_limiter.clone()).await
        {
        Ok(settings) => settings,
        Err(e) => {
            eprintln!("failed to load settings: {e}");
            std::process::exit(1);
        }
    };
    if daemons.networks().is_empty() {
        // A warning, not an exit: the settings API has to be reachable to
        // configure a node at all, and one saved there applies straight away.
        eprintln!(
            "warning: no Monero node is configured for any network (mainnet/stagenet/testnet) yet - nothing is \
             scanned until one is saved on the admin settings page (or POST /api/v1/admin/settings, \
             monero_node.<network>); it applies without a restart"
        );
    }

    let enabled = router.enabled_backends();
    let stranded: Vec<(String, usize)> = {
        let tenants = store.lock().tenant_custody_backends().unwrap_or_default();
        let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
        for (_, _, backend) in tenants.into_iter().filter(|(_, _, backend)| !enabled.contains(backend)) {
            *counts.entry(backend).or_default() += 1;
        }
        counts.into_iter().collect()
    };
    for (backend, count) in stranded {
        eprintln!(
            "WARNING: {count} store(s) keep their keys in the {backend:?} key custody backend, which is not enabled \
             (key_custody.enabled_backends = {}). Their payments are NOT being detected until it is enabled again \
             or they move their keys to an enabled backend.",
            enabled.join(",")
        );
    }

    let key_custody: Arc<dyn KeyCustody> = router.clone();
    let key_custody_backend = router.default_backend();
    let scanner_status = scanner_status::new_scanner_status_map();
    let wallet_handles = Arc::new(RwLock::new(register_all_tenants(&store, &key_custody).await));

    let app_state = AppState {
        store: store.clone(),
        key_custody: key_custody.clone(),
        key_custody_backend,
        wallet_handles: wallet_handles.clone(),
        admin_rate_limiter,
        daemons: daemons.clone(),
        scanner_status: scanner_status.clone(),
        settings: engine_settings.clone(),
    };

    let delivery_store = store.clone();
    let delivery_settings = engine_settings.clone();
    supervise("webhook delivery", move || loops::run_webhook_delivery_loop(delivery_store.clone(), delivery_settings.clone()));

    // One scanner loop and one revalidation loop per configured network
    // (task 7.4), started and stopped as node settings are saved (task 2.1).
    // Supervised like the loops it starts: if it panics, dropping it stops
    // them, and its restart starts them again.
    let (loops_store, loops_custody, loops_daemons, loops_handles, loops_status, loops_settings) = (
        store.clone(),
        key_custody.clone(),
        daemons.clone(),
        wallet_handles.clone(),
        scanner_status.clone(),
        engine_settings.clone(),
    );
    supervise("network loop manager", move || {
        loops::manage_network_loops(
            loops_store.clone(),
            loops_custody.clone(),
            loops_daemons.clone(),
            loops_handles.clone(),
            loops_status.clone(),
            loops_settings.clone(),
        )
    });

    // Read once: the listen address is restart-only (decision D8).
    let bind = engine_settings.runtime.load().bind;
    let router = build_router(app_state, MAX_BODY_CEILING);
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("failed to bind server address");
    println!("moneropay listening on {bind}");
    // The engine is private: only monokulo, on this machine or a private
    // network, should ever reach it. Nothing stops an operator binding it
    // elsewhere, but it must not happen by accident.
    if let Ok(local) = listener.local_addr() {
        if !scanner::settings::is_private_bind_address(local.ip()) {
            eprintln!(
                "WARNING: the engine is listening on {local}, which is not a loopback or private address. \
                 The engine is meant to be reached only by monokulo; anything that can connect to it can \
                 create tenants and hit its API directly. Set server.bind (SCANNER_SERVER_BIND) to a \
                 loopback or private address such as 127.0.0.1:8443 unless you really mean this."
            );
        }
    }
    // On SIGTERM or Ctrl-C (task 7.11): stop accepting connections and let
    // requests in flight finish, for up to SHUTDOWN_GRACE, then exit. The
    // background loops simply stop with the process: every step they take is
    // safe to interrupt (payments are recorded idempotently, a block is only
    // marked scanned after everything in it is recorded, webhooks are marked
    // delivered only after they went out), so the next start carries on.
    let server = axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(shutdown_signal());
    let served = tokio::spawn(async move { server.await });
    let _ = shutdown_signal().await;
    println!("shutting down: finishing requests in flight (up to {SHUTDOWN_GRACE:?})");
    match tokio::time::timeout(SHUTDOWN_GRACE, served).await {
        Ok(Ok(Ok(()))) => println!("shut down cleanly"),
        Ok(Ok(Err(e))) => eprintln!("server error while shutting down: {e}"),
        Ok(Err(e)) => eprintln!("server task failed while shutting down: {e}"),
        Err(_) => eprintln!("requests still running after {SHUTDOWN_GRACE:?}, exiting anyway"),
    }
}

// `supervise` itself moved to `shared::supervise` (`docs/fx_refactor.md`
// Phase 1.1, imported at the top of this file) so monokulo's own
// background loops (its Coingecko exchange-rate refresh loop) can reuse
// the exact same panic-catching restart shape - see that module's own doc
// comment for the full reasoning (still applies unchanged: a bare
// `tokio::spawn` of an infinite loop panicking would silently kill chain
// scanning/webhook delivery while the HTTP server keeps serving happily,
// with no signal short of a merchant eventually complaining).

/// Builds the router's backends from `custody` once, for the one-off CLI
/// commands (the server applies them through its settings registry).
async fn apply_custody(router: &Arc<CustodyRouter>, custody: &CustodyConfig) -> Result<(), String> {
    use live_settings::Reloadable;
    let reloadable = CustodyReloadable { router: router.clone() };
    let (prepared, warnings) = reloadable.prepare(custody, custody).await.map_err(|e| e.to_string())?;
    for warning in warnings {
        eprintln!("{}", warning.message);
    }
    reloadable.install(prepared).await;
    Ok(())
}

/// Eagerly registers every non-disabled tenant's sealed key material with
/// `KeyCustody`, so `AppState::wallet_handles` starts populated rather than relying
/// solely on the lazy on-first-use path in `http::resolve_wallet_handle`.
async fn register_all_tenants(store: &SharedStore, key_custody: &Arc<dyn KeyCustody>) -> HashMap<String, WalletHandle> {
    // A database error here must not kill the engine at boot: retry with
    // backoff until the store answers, logging each failure.
    let mut delay = Duration::from_millis(500);
    let tenants = loop {
        match store.lock().list_active_tenants() {
            Ok(tenants) => break tenants,
            Err(e) => {
                eprintln!("failed to list tenants at boot, retrying in {delay:?}: {e}");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(30));
            }
        }
    };
    let mut handles = HashMap::new();
    for tenant in tenants {
        match key_custody.unseal_and_register_in(&tenant.key_custody_backend, &tenant.sealed_key_material).await {
            Ok(handle) => {
                handles.insert(tenant.id, handle);
            }
            Err(e) => eprintln!("failed to register tenant {} with key custody: {e}", tenant.id),
        }
    }
    handles
}

/// How long requests in flight get to finish after SIGTERM or Ctrl-C.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Resolves on SIGTERM (what a service manager sends) or Ctrl-C.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            eprintln!("could not listen for Ctrl-C: {e}");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(e) => {
                eprintln!("could not listen for SIGTERM: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
