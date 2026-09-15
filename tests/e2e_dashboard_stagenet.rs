//! Real end-to-end test spanning the *whole* hosted stack, not just the
//! engine: signs up a real control-plane account, connects a store through
//! the real "advanced" connect form (`POST /dashboard/connect`) using the
//! same reusable merchant watch-only wallet `tests/e2e_stagenet.rs` and
//! `mock-woocommerce/tests/e2e_stagenet_connect_flow.rs` already use, pays a
//! real order with a genuine, signed, broadcast stagenet transaction (via
//! `support::StagenetSpendWallet`, same as those two), and asserts the
//! payment shows up on the real control-plane dashboard (`GET /dashboard`)
//! with the real, correct total-received-XMR figure - not just that the
//! engine detected it (that's already `e2e_stagenet.rs`'s own job).
//!
//! The engine here is a **real, network-bound** `moneropay-core` instance
//! (an ephemeral `TcpListener`, not the in-process `oneshot` router
//! `e2e_stagenet.rs` itself uses) - control-plane's own `EngineClient` makes
//! genuine `reqwest` HTTP calls to it, exactly like it does against a real
//! deployment, so this has to be a real bound socket for that to work at
//! all. The control-plane side stays in-process (`tower::ServiceExt::oneshot`),
//! same as `control-plane`'s own test suite - nothing about driving *that*
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

use moneropay_core::config::Config;
use moneropay_core::daemon::MoneroDaemonClient;
use moneropay_core::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use moneropay_core::daemon_rpc::RpcDaemonClient;
use moneropay_core::exchange_rate::ExchangeRateProvider;
use moneropay_core::http::rate_limit::RateLimiter;
use moneropay_core::http::{build_router as build_engine_router, now_unix, AppState as EngineAppState};
use moneropay_core::key_custody::{KeyCustody, PlainKeyCustody, WalletHandle};
use moneropay_core::network::network_str;
use moneropay_core::scanner::run_scan_tick;
use moneropay_core::scanner_status::new_scanner_status_map;
use moneropay_core::store::Store;

use control_plane::db::Db;
use control_plane::engine_client::EngineClient;
use control_plane::http::{build_router as build_control_plane_router, AppState as ControlPlaneAppState};
use control_plane::templates::TemplateEngine;

use support::StagenetSpendWallet;

const ENGINE_CONFIG_PATH: &str = "e2e/moneropay-stagenet.toml";
const WALLETS_PATH: &str = "e2e/stagenet-wallets.json";

/// Same check `e2e_stagenet.rs` opens with, and for the same reason: fail
/// with a clear, actionable message before spending anything, rather than
/// deep inside the send/poll loop.
async fn require_daemon_reachable(daemon: &dyn MoneroDaemonClient, host: &str, port: u16) {
    if let Err(e) = daemon.get_height().await {
        panic!(
            "\n\ncannot reach the stagenet node at {host}:{port} (configured in {ENGINE_CONFIG_PATH}'s \
             [monero_node.stagenet]): {e}\n\
             Check that host/port, your network connection, or try a different public stagenet \
             node (search \"monero stagenet public node\" for alternatives).\n"
        );
    }
}

/// Deliberately duplicated from `e2e_stagenet.rs` rather than shared via
/// `tests/support/mod.rs`, specifically so this test never touches that
/// other, already-real-money-costing test's own file while being written -
/// see this module's own doc comment. Same atomic temp-file-then-rename
/// write-back, same reasoning (two overlapping runs, or a crash mid-write,
/// must never corrupt or lose the shared customer wallet's own spendable-
/// output bookkeeping).
fn record_known_txid(tx_hash: &str) {
    let mut wallets_json: Value = serde_json::from_str(
        &std::fs::read_to_string(WALLETS_PATH).unwrap_or_else(|e| panic!("failed to read {WALLETS_PATH}: {e}")),
    )
    .unwrap_or_else(|e| panic!("failed to parse {WALLETS_PATH}: {e}"));
    let known = wallets_json["customer"]["known_txids"].as_array_mut().expect("customer.known_txids must be an array");
    if !known.iter().any(|v| v.as_str() == Some(tx_hash)) {
        known.push(json!(tx_hash));
    }
    let tmp_path = format!("{WALLETS_PATH}.tmp");
    std::fs::write(&tmp_path, serde_json::to_string_pretty(&wallets_json).unwrap() + "\n")
        .unwrap_or_else(|e| panic!("failed to write {tmp_path}: {e}"));
    std::fs::rename(&tmp_path, WALLETS_PATH).unwrap_or_else(|e| panic!("failed to move {tmp_path} into place over {WALLETS_PATH}: {e}"));
}

/// 1 XMR = 10^12 piconero. Mirrors `control_plane::http::home`'s own
/// (private) `format_piconero_as_xmr` exactly - not reused directly (that
/// function is private to its crate, and this test intentionally computes
/// the *expected* display string independently rather than importing the
/// very function whose output it's checking, so this doesn't just prove
/// "the code agrees with itself"). That function's own doc comment and
/// unit tests (`control-plane/src/http/home.rs`) are the authority on this
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

#[tokio::test]
#[ignore]
async fn real_stagenet_payment_shows_up_in_the_dashboard_with_the_correct_total_received() {
    // ---- load the same real config + reusable wallet fixtures e2e_stagenet.rs uses ----
    let config =
        Config::from_file(ENGINE_CONFIG_PATH).unwrap_or_else(|e| panic!("failed to load {ENGINE_CONFIG_PATH} (run from the repo root): {e}"));
    config.validate().expect("e2e config should be valid");
    let wallet_cfg = config.wallet.as_ref().expect("e2e config must have a [wallet] bootstrap section");
    let node_cfg = config.monero_node.get(Network::Stagenet).expect("e2e config must have [monero_node.stagenet]");
    let node_url = format!("http{}://{}:{}", if node_cfg.ssl { "s" } else { "" }, node_cfg.host, node_cfg.port);

    let wallets_json: Value = serde_json::from_str(
        &std::fs::read_to_string(WALLETS_PATH).unwrap_or_else(|e| panic!("failed to read {WALLETS_PATH}: {e}")),
    )
    .unwrap_or_else(|e| panic!("failed to parse {WALLETS_PATH}: {e}"));
    let customer_address = wallets_json["customer"]["address"].as_str().expect("customer.address missing").to_string();
    let customer_spend_key_hex =
        wallets_json["customer"]["private_spend_key"].as_str().expect("customer.private_spend_key missing").to_string();
    let customer_view_key_hex =
        wallets_json["customer"]["private_view_key"].as_str().expect("customer.private_view_key missing").to_string();
    let known_txids: Vec<String> = wallets_json["customer"]["known_txids"]
        .as_array()
        .expect("customer.known_txids missing")
        .iter()
        .map(|v| v.as_str().expect("known_txids entries must be strings").to_string())
        .collect();

    // ---- boot a REAL, network-bound engine (no tenant bootstrapped here - the
    // "advanced connect" flow below creates it, through control-plane, exactly
    // like a real merchant would) ----
    let store = Store::open_in_memory().unwrap().into_shared();
    let key_custody: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
    let exchange_rate: Arc<dyn ExchangeRateProvider> =
        Arc::new(config.exchange_rate.build_fixed_rate_provider().expect("invalid exchange_rate.rates in e2e config"));
    let daemon: Arc<dyn MoneroDaemonClient> = Arc::new(
        RpcDaemonClient::new(&node_cfg.host, node_cfg.port, node_cfg.ssl, node_cfg.accept_self_signed_certs)
            .expect("failed to build daemon RPC client"),
    );
    require_daemon_reachable(daemon.as_ref(), &node_cfg.host, node_cfg.port).await;

    let fallback_daemon = Arc::new(FallbackDaemonClient::new(vec![FallbackNode {
        label: format!("{}:{}", node_cfg.host, node_cfg.port),
        client: daemon.clone(),
    }]));
    let wallet_handles: Arc<RwLock<HashMap<String, WalletHandle>>> = Arc::new(RwLock::new(HashMap::new()));

    let engine_state = EngineAppState {
        store: store.clone(),
        key_custody: key_custody.clone(),
        key_custody_backend: "plain".to_string(),
        exchange_rate,
        wallet_handles: wallet_handles.clone(),
        rate_limiter: Arc::new(RateLimiter::new(10_000)),
        admin_rate_limiter: Arc::new(RateLimiter::new(10_000)),
        configured_networks: Arc::new(HashSet::from([Network::Stagenet])),
        daemons: Arc::new(HashMap::from([(Network::Stagenet, fallback_daemon)])),
        scanner_status: new_scanner_status_map(),
        scan_poll_interval_secs: 2,
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

    // ---- boot a real control-plane instance in-process (no bind needed - its
    // own router is driven via oneshot below), pointed at the real engine above ----
    let cp_db = Db::open_in_memory().unwrap().into_shared();
    let cp_state = ControlPlaneAppState {
        db: cp_db,
        engine_client: EngineClient::new(engine_base_url.clone()),
        encryption_key: [7u8; 32],
        templates: Arc::new(TemplateEngine::new().unwrap()),
        status_cache: control_plane::http::status_page::new_status_cache(),
    };
    let cp_router = build_control_plane_router(cp_state);

    // ---- 1. create a real control-plane account ----
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
                    ("view_key_hex", &wallet_cfg.private_view_key),
                    ("spend_pubkey_hex", &wallet_cfg.public_spend_key),
                    ("network", "stagenet"),
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
    // control-plane HTTP flow above (not constructed directly) - and the
    // engine's own admin::create_tenant handler already registered its real
    // WalletHandle into the shared wallet_handles registry this test also
    // holds a handle to, exactly like it would for any real caller.
    assert_eq!(wallet_handles.read().unwrap().len(), 1, "the real connect flow should have registered exactly one tenant");

    // ---- 3. create a real order directly against the real engine's public API
    // (this is what a real storefront - or the WooCommerce plugin - calls) ----
    let http = reqwest::Client::new();
    let order_response = http
        .post(format!("{engine_base_url}/api/v1/t/{public_key}/orders"))
        .json(&json!({
            "merchant_order_id": format!("rust-dashboard-e2e-{}", now_unix()),
            "fiat_amount": "0.05",
            "fiat_currency": "USD",
        }))
        .send()
        .await
        .expect("order creation request failed");
    assert_eq!(order_response.status(), reqwest::StatusCode::OK, "real order creation must succeed");
    let order: Value = order_response.json().await.unwrap();
    let payment_id = order["payment_id"].as_str().unwrap().to_string();
    let address = order["address"].as_str().unwrap().to_string();
    let amount_piconero = order["xmr_amount_piconero"].as_u64().unwrap();
    println!("created order {payment_id}: {amount_piconero} piconero to {address}");

    // ---- 4. pay it for real - genuine signed + broadcast stagenet transaction ----
    let spend_wallet =
        StagenetSpendWallet::connect(&node_url, node_cfg.accept_self_signed_certs, &customer_spend_key_hex, &customer_view_key_hex, &customer_address)
            .await
            .unwrap_or_else(|e| panic!("\n\n{e}\n"));
    let tx_hash = spend_wallet.send(daemon.as_ref(), &known_txids, &address, amount_piconero).await.unwrap_or_else(|e| panic!("\n\n{e}\n"));
    let tx_hash_hex = hex::encode(tx_hash);
    println!("sent real stagenet payment, tx {tx_hash_hex}");
    record_known_txid(&tx_hash_hex);

    // ---- 5. tick the real scanner in the foreground and poll the real
    // control-plane dashboard - not the engine's own API - until it shows the
    // payment with the correct total received ----
    let expected_total = expected_total_received_display(amount_piconero);
    let mut last_dashboard_html = String::new();
    for attempt in 1..=30 {
        let tenants: Vec<(String, WalletHandle)> = wallet_handles.read().unwrap().iter().map(|(id, h)| (id.clone(), *h)).collect();
        run_scan_tick(&store, key_custody.as_ref(), daemon.as_ref(), network_str(Network::Stagenet), &tenants, config.payment.reorg_check_depth)
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
