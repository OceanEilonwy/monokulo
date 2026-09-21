//! Real end-to-end test spanning the *whole* hosted stack, not just the
//! engine: signs up a real monokulo account, connects a store through
//! the real "advanced" connect form (`POST /dashboard/connect`) using the
//! same reusable merchant watch-only wallet `tests/e2e_stagenet.rs` and
//! `mock-woocommerce/tests/e2e_stagenet_connect_flow.rs` already use, pays a
//! real order with a genuine, signed, broadcast stagenet transaction (via
//! `cli_wallet::Wallet`, same as those two), and asserts the
//! payment shows up on the real monokulo dashboard (`GET /dashboard`)
//! with the real, correct total-received-XMR figure - not just that the
//! engine detected it (that's already `e2e_stagenet.rs`'s own job).
//!
//! The engine here is a **real, network-bound** `scanner` instance
//! (an ephemeral `TcpListener`, not the in-process `oneshot` router
//! `e2e_stagenet.rs` itself uses) - monokulo's own `EngineClient` makes
//! genuine `reqwest` HTTP calls to it, exactly like it does against a real
//! deployment, so this has to be a real bound socket for that to work at
//! all. The monokulo side stays in-process (`tower::ServiceExt::oneshot`),
//! same as `monokulo`'s own test suite - nothing about driving *that*
//! side needs a second bound port.
//!
//! No background scan loop is spawned for either side: the scanner is
//! ticked explicitly, in the test's own polling loop, the same deliberate
//! choice `e2e_stagenet.rs` makes and explains in its own comments (precise
//! foreground control, no interference from a concurrent timer).
//!
//! Same execution model as `e2e_stagenet.rs`: `#[ignore]`d (real network
//! access, real stagenet fees, not hermetic), feature-gated behind `e2e`
//! (needs the real transaction-signing dependencies), run explicitly with:
//!
//! ```sh
//! cargo test --test e2e_dashboard_stagenet -- --ignored --nocapture
//! ```
//!
//! Run from the repository root, same as `e2e_stagenet.rs` - `e2e/*` paths
//! below are relative to `cargo test`'s working directory (the package
//! root).

mod support;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use monero::Network;
use serde_json::{json, Value};
use tower::ServiceExt;

use scanner::daemon::MoneroDaemonClient;
use scanner::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use scanner::daemon_rpc::RpcDaemonClient;
use scanner::http::rate_limit::RateLimiter;
use scanner::http::{build_router as build_engine_router, now_unix, AppState as EngineAppState};
use scanner::key_custody::{KeyCustody, PlainKeyCustody, WalletHandle};
use scanner::network::network_str;
use scanner::scanner::run_scan_tick;
use scanner::scanner_status::new_scanner_status_map;
use scanner::store::Store;

use monokulo::db::Db;
use monokulo::engine_client::EngineClient;
use monokulo::http::{build_router as build_monokulo_router, AppState as ControlPlaneAppState};
use monokulo::templates::TemplateEngine;

/// Same check `e2e_stagenet.rs` opens with, and for the same reason: fail
/// with a clear, actionable message before spending anything, rather than
/// deep inside the send/poll loop.
async fn require_daemon_reachable(daemon: &dyn MoneroDaemonClient, host: &str, port: u16) {
    if let Err(e) = daemon.get_height().await {
        panic!(
            "\n\ncannot reach the stagenet node at {host}:{port} (configured in support::e2e_fixture): \
             {e}\n\
             Check that host/port, your network connection, or try a different public stagenet \
             node (search \"monero stagenet public node\" for alternatives).\n"
        );
    }
}

/// 1 XMR = 10^12 piconero. Mirrors `monokulo::http::home`'s own
/// (private) `format_piconero_as_xmr` exactly - not reused directly (that
/// function is private to its crate, and this test intentionally computes
/// the *expected* display string independently rather than importing the
/// very function whose output it's checking, so this doesn't just prove
/// "the code agrees with itself"). That function's own doc comment and
/// unit tests (`monokulo/src/http/home.rs`) are the authority on this
/// format; this is a second, independent implementation of the same
/// documented contract, cross-checked here against a real page render.
fn expected_total_received_display(piconero: u64) -> String {
    const PICONERO_PER_XMR: u64 = 1_000_000_000_000;
    let whole = piconero / PICONERO_PER_XMR;
    let frac = piconero % PICONERO_PER_XMR;
    if frac == 0 {
        return whole.to_string();
    }
    let frac_str = format!("{frac:012}");
    let trimmed = frac_str.trim_end_matches('0');
    format!("{whole}.{trimmed}")
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn form_body(fields: &[(&str, &str)]) -> String {
    fields.iter().map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v))).collect::<Vec<_>>().join("&")
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
#[ignore]
async fn real_stagenet_payment_shows_up_in_the_dashboard_with_the_correct_total_received() {
    // monokulo's `signup.mode` now defaults to `"invite_only"` (added after
    // this test was written - see `e2e_harness.rs`'s own identical fix for
    // its sibling POS suite) - without this, the plain `/dashboard/signup`
    // call below gets silently rejected (a re-rendered `200` form, not the
    // `302` it asserts on) instead of creating the account. `settings::get`
    // resolves this env var live on every call, so setting it here before
    // the signup request is enough.
    std::env::set_var("MONOKULO_SIGNUP_MODE", "public");

    // ---- load the same real fixture + reusable wallet fixtures e2e_stagenet.rs uses ----
    use support::e2e_fixture;

    let ctx = cli_wallet::WalletCtx::default();
    let spender = cli_wallet::WalletStore::load(&ctx)
        .unwrap_or_else(|e| panic!("failed to load {}: {e}", ctx.wallets_path))
        .wallet("spender")
        .unwrap_or_else(|e| panic!("failed to load the spender wallet: {e}"));

    // ---- boot a REAL, network-bound engine (no tenant bootstrapped here - the
    // "advanced connect" flow below creates it, through monokulo, exactly
    // like a real merchant would) ----
    let store = Store::open_in_memory().unwrap().into_shared();
    let key_custody: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
    let daemon: Arc<dyn MoneroDaemonClient> = Arc::new(
        RpcDaemonClient::new(e2e_fixture::NODE_HOST, e2e_fixture::NODE_PORT, e2e_fixture::NODE_SSL, e2e_fixture::NODE_ACCEPT_SELF_SIGNED_CERTS)
            .expect("failed to build daemon RPC client"),
    );
    require_daemon_reachable(daemon.as_ref(), e2e_fixture::NODE_HOST, e2e_fixture::NODE_PORT).await;

    let fallback_daemon = Arc::new(FallbackDaemonClient::new(vec![FallbackNode {
        label: format!("{}:{}", e2e_fixture::NODE_HOST, e2e_fixture::NODE_PORT),
        client: daemon.clone(),
    }]));
    let wallet_handles: Arc<RwLock<HashMap<String, WalletHandle>>> = Arc::new(RwLock::new(HashMap::new()));

    let engine_state = EngineAppState {
        store: store.clone(),
        key_custody: key_custody.clone(),
        key_custody_backend: "plain".to_string(),
        wallet_handles: wallet_handles.clone(),
        rate_limiter: Arc::new(RateLimiter::new(10_000)),
        admin_rate_limiter: Arc::new(RateLimiter::new(10_000)),
        configured_networks: Arc::new(HashSet::from([Network::Stagenet])),
        daemons: Arc::new(HashMap::from([(Network::Stagenet, fallback_daemon)])),
        scanner_status: new_scanner_status_map(),
        scan_poll_interval_secs: 2,
        default_rescan_lookback_days: 7,
        max_rescan_lookback_days: 90,
        expired_order_grace_period_seconds: 0,
    };
    let engine_router = build_engine_router(engine_state, 1_000_000);
    let engine_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("failed to bind an ephemeral engine port");
    let engine_addr = engine_listener.local_addr().expect("bound engine listener has no local address");
    tokio::spawn(async move {
        axum::serve(engine_listener, engine_router.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .await
            .expect("test engine server error");
    });
    let engine_base_url = format!("http://{engine_addr}");
    println!("real engine bound at {engine_base_url}");

    // ---- boot a real monokulo instance in-process (no bind needed - its
    // own router is driven via oneshot below), pointed at the real engine above ----
    let cp_db = Db::open_in_memory().unwrap().into_shared();
    let cp_state = ControlPlaneAppState {
        db: cp_db,
        engine_client: EngineClient::new(engine_base_url.clone()),
        encryption_key: [7u8; 32],
        templates: Arc::new(TemplateEngine::new().unwrap()),
        status_cache: monokulo::http::status_page::new_status_cache(),
        // `docs/fx_refactor.md` Phase 5: order creation now goes through
        // monokulo's own `/pay/{pk}/orders`, the real path a production
        // storefront takes. The order below is priced directly in `"XMR"` -
        // needs no exchange rate provider configured at all - at a genuinely
        // tiny 335_000_000-piconero (0.000335 XMR) amount, matching
        // `tests/e2e_stagenet.rs`'s own target and `mock-woocommerce`'s own
        // real-stagenet test.
        exchange_rate: Arc::new(monokulo::exchange_rate_config::ExchangeRateProviders::xmr_only()),
        rate_limiter: Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
    };
    let cp_router = build_monokulo_router(cp_state);

    // ---- 1. create a real monokulo account ----
    let email = format!("e2e-dashboard-{}@example.com", now_unix());
    let password = "correct horse battery staple";
    let signup_response = cp_router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dashboard/signup")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_body(&[("email", &email), ("password", password)])))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(signup_response.status(), StatusCode::FOUND, "real signup should redirect to login");

    let login_response = cp_router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dashboard/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_body(&[("email", &email), ("password", password)])))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login_response.status(), StatusCode::FOUND, "real login should redirect to /dashboard");
    let set_cookie = login_response.headers().get("set-cookie").unwrap().to_str().unwrap().to_string();
    let session_cookie = set_cookie.split(';').next().unwrap().to_string();

    // ---- 2. connect a store via the real "advanced" connect form, using the
    // same reusable merchant watch-only wallet the engine-only e2e test uses ----
    let site_url = "https://e2e-dashboard-test.example.com";
    let connect_response = cp_router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dashboard/connect")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", &session_cookie)
                .body(Body::from(form_body(&[
                    ("site_url", site_url),
                    ("view_key_hex", e2e_fixture::WALLET_PRIVATE_VIEW_KEY),
                    ("spend_pubkey_hex", e2e_fixture::WALLET_PUBLIC_SPEND_KEY),
                    ("network", "stagenet"),
                    ("base_currency", "XMR"),
                    ("allowed_origins", ""),
                ])))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(connect_response.status(), StatusCode::OK, "expected the connect success page, not a re-rendered form");
    let connect_html = body_text(connect_response).await;
    assert!(connect_html.contains("Store connected"), "expected a real successful connect, got: {connect_html}");
    let pk_start = connect_html.find("pk_").expect("expected a real pk_ value in the connect success page");
    let public_key: String = connect_html[pk_start..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    println!("connected real store, public_key={public_key}");

    // A real tenant now exists on the real engine, created through the real
    // monokulo HTTP flow above (not constructed directly) - and the
    // engine's own admin::create_tenant handler already registered its real
    // WalletHandle into the shared wallet_handles registry this test also
    // holds a handle to, exactly like it would for any real caller.
    assert_eq!(wallet_handles.read().unwrap().len(), 1, "the real connect flow should have registered exactly one tenant");

    // ---- 3. create a real order through monokulo's own public
    // `/pay/{pk}/orders` (this is what a real storefront - or the
    // WooCommerce plugin - actually calls in production; the engine's own
    // API is XMR-only and developer-facing now, `docs/fx_refactor.md` Phase 5) ----
    let order_response = cp_router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/pay/{public_key}/orders"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "amount": "0.000335", "currency": "XMR" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(order_response.status(), StatusCode::OK, "real order creation through monokulo must succeed");
    let order: Value = body_json(order_response).await;
    let payment_id = order["payment_id"].as_str().unwrap().to_string();
    let address = order["address"].as_str().unwrap().to_string();
    let amount_piconero = order["xmr_amount_piconero"].as_u64().unwrap();
    println!("created order {payment_id}: {amount_piconero} piconero to {address}");

    // ---- 4. pay it for real - genuine signed + broadcast stagenet transaction ----
    let tx_hash = cli_wallet::send_payment(spender, &address, amount_piconero, None)
    .await
    .unwrap_or_else(|e| panic!("\n\n{e}\n"));
    let tx_hash_hex = hex::encode(tx_hash);
    println!("sent real stagenet payment, tx {tx_hash_hex}");

    // ---- 5. tick the real scanner in the foreground and poll the real
    // monokulo dashboard - not the engine's own API - until it shows the
    // payment with the correct total received ----
    let expected_total = expected_total_received_display(amount_piconero);
    let mut last_dashboard_html = String::new();
    for attempt in 1..=30 {
        let tenants: Vec<(String, WalletHandle)> = wallet_handles.read().unwrap().iter().map(|(id, h)| (id.clone(), *h)).collect();
        run_scan_tick(&store, key_custody.as_ref(), daemon.as_ref(), network_str(Network::Stagenet), &tenants, e2e_fixture::PAYMENT_REORG_CHECK_DEPTH, 0)
            .await
            .expect("scan tick failed");

        let dashboard_response = cp_router
            .clone()
            .oneshot(Request::builder().method("GET").uri("/dashboard").header("cookie", &session_cookie).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(dashboard_response.status(), StatusCode::OK);
        last_dashboard_html = body_text(dashboard_response).await;

        let order_detected = last_dashboard_html.contains(&payment_id);
        let total_correct = last_dashboard_html.contains(&expected_total);
        println!("[{attempt}/30] order on dashboard: {order_detected}, total_received matches ({expected_total}): {total_correct}");

        if order_detected && total_correct {
            println!("PASS: order {payment_id} appears on the real dashboard with total received {expected_total} XMR (tx {tx_hash_hex})");
            return;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    panic!(
        "order {payment_id} (tx {tx_hash_hex}) never appeared on the dashboard with the correct total received \
         ({expected_total} XMR expected) after 30 scan attempts. Last dashboard HTML:\n{last_dashboard_html}"
    );
}
