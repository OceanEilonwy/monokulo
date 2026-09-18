//! Boots the *real* backend half of the POS screen's browser-driven e2e
//! suite (`e2e/pos-playwright/`) - a genuinely network-bound `scanner`
//! engine talking to the real public stagenet node, a genuinely
//! network-bound `monokulo` (so an external browser, driven by Playwright
//! over Node, can actually reach it - unlike `tests/e2e_dashboard_stagenet.rs`,
//! which only ever drives monokulo's router in-process via `oneshot` since
//! that test *is* the client), and one real signed-up account with one real
//! store connected to that engine, using the exact same reusable merchant
//! watch-only wallet fixture `tests/e2e_dashboard_stagenet.rs`/`tests/e2e_stagenet.rs`
//! already use.
//!
//! A real `[[bin]]`, not another `#[ignore]`d `#[tokio::test]` - see
//! `Cargo.toml`'s own comment on the two `[[bin]]` entries for why: Playwright
//! (over Node's `child_process`) needs to spawn, read one line of stdout from,
//! and later cleanly kill this exact process, which is far simpler against a
//! predictable `target/debug/pos-e2e-server` binary than against `cargo
//! test`'s own hash-suffixed test binary or its `cargo` parent process.
//!
//! Prints exactly one JSON line to stdout once both servers are up and the
//! account/store exist, then blocks forever (both servers, and a background
//! scan-tick loop, keep running on their own spawned tasks) until killed -
//! see `e2e/pos-playwright/README.md` for the full protocol Node's own side
//! follows against that line.
//!
//! `#[cfg(feature = "e2e")]`-equivalent via `required-features` in
//! `Cargo.toml` - this binary doesn't even exist in a normal build (needs the
//! same real transaction-signing dependencies `scanner::e2e_wallet` does, via
//! `monokulo`'s own `scanner-test-support` -> `scanner` dev-dependency chain
//! being irrelevant here; what actually gates this is the `monero-wallet`/
//! `monero-daemon-rpc`/`monokulo`/`http-body-util` optional deps, all behind
//! the same `e2e` feature every other real-stagenet test in this crate uses).

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

// Deliberately duplicated from `tests/support/mod.rs::e2e_fixture` rather
// than imported - a `[[bin]]` target has no access to `tests/`-local modules
// at all (that's the whole reason this is a `[[bin]]`, see this file's own
// doc comment), and the fixture is a handful of stable constants, not
// meaningfully-sized logic worth restructuring the crate to share.
const NODE_HOST: &str = "node.monerodevs.org";
const NODE_PORT: u16 = 38089;
const NODE_SSL: bool = false;
const NODE_ACCEPT_SELF_SIGNED_CERTS: bool = true;
const WALLET_PRIVATE_VIEW_KEY: &str = "fcdc7998f003928b3f409b94d54f690d16ca6df3689de4da4803c5a9c792fb0e";
const WALLET_PUBLIC_SPEND_KEY: &str = "3fa2161d4e2cc7722288d33e46a4cc37e92629d7e45939ec67cc42e8f144b335";
const PAYMENT_REORG_CHECK_DEPTH: u64 = 20;

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

#[tokio::main]
async fn main() {
    // ---- real, network-bound engine against the real public stagenet node ----
    let store = Store::open_in_memory().unwrap().into_shared();
    let key_custody: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
    let daemon: Arc<dyn MoneroDaemonClient> =
        Arc::new(RpcDaemonClient::new(NODE_HOST, NODE_PORT, NODE_SSL, NODE_ACCEPT_SELF_SIGNED_CERTS).expect("failed to build daemon RPC client"));
    if let Err(e) = daemon.get_height().await {
        panic!("\n\ncannot reach the stagenet node at {NODE_HOST}:{NODE_PORT}: {e}\n");
    }
    let fallback_daemon =
        Arc::new(FallbackDaemonClient::new(vec![FallbackNode { label: format!("{NODE_HOST}:{NODE_PORT}"), client: daemon.clone() }]));
    let wallet_handles: Arc<RwLock<HashMap<String, WalletHandle>>> = Arc::new(RwLock::new(HashMap::new()));

    let engine_state = EngineAppState {
        store: store.clone(),
        key_custody: key_custody.clone(),
        key_custody_backend: "plain".to_string(),
        wallet_handles: wallet_handles.clone(),
        rate_limiter: Arc::new(RateLimiter::new(1_000_000)),
        admin_rate_limiter: Arc::new(RateLimiter::new(1_000_000)),
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
    let engine_addr = engine_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(engine_listener, engine_router.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .await
            .expect("engine server error");
    });
    let engine_base_url = format!("http://{engine_addr}");

    // ---- real, network-bound monokulo, pointed at the engine above - Playwright's
    // browser needs a real socket to navigate to, unlike `e2e_dashboard_stagenet.rs`'s
    // own in-process-only `oneshot` driving ----
    let cp_state = ControlPlaneAppState {
        db: Db::open_in_memory().unwrap().into_shared(),
        engine_client: EngineClient::new(engine_base_url.clone()),
        encryption_key: [7u8; 32],
        templates: Arc::new(TemplateEngine::new().unwrap()),
        status_cache: monokulo::http::status_page::new_status_cache(),
        exchange_rate: Arc::new(monokulo::exchange_rate_config::ExchangeRateProviders::xmr_only()),
        rate_limiter: Arc::new(shared::rate_limit::RateLimiter::new(1_000_000)),
    };
    let cp_router = build_monokulo_router(cp_state);
    let cp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("failed to bind an ephemeral monokulo port");
    let cp_addr = cp_listener.local_addr().unwrap();
    let cp_router_for_serve = cp_router.clone();
    tokio::spawn(async move {
        axum::serve(cp_listener, cp_router_for_serve.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .await
            .expect("monokulo server error");
    });
    let monokulo_base_url = format!("http://{cp_addr}");

    // ---- a real background scan loop, since an external process (Playwright,
    // over real HTTP polling) - not this process's own sequential test code -
    // is what's watching for payment status changes this time; mirrors
    // `scanner::main`'s own `run_scanner_loop`, simplified to the one network
    // this harness ever configures. ----
    {
        let store = store.clone();
        let key_custody = key_custody.clone();
        let daemon = daemon.clone();
        let wallet_handles = wallet_handles.clone();
        tokio::spawn(async move {
            loop {
                let tenants: Vec<(String, WalletHandle)> = wallet_handles.read().unwrap().iter().map(|(id, h)| (id.clone(), *h)).collect();
                if let Err(e) =
                    run_scan_tick(&store, key_custody.as_ref(), daemon.as_ref(), network_str(Network::Stagenet), &tenants, PAYMENT_REORG_CHECK_DEPTH, 0)
                        .await
                {
                    eprintln!("pos-e2e-server: scan tick failed: {e}");
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }

    // ---- one real account, one real store connected to the engine above,
    // through the real HTTP flow (not constructed directly) - same wallet
    // fixture `tests/e2e_dashboard_stagenet.rs` uses. ----
    let email = format!("pos-e2e-{}@example.com", now_unix());
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

    let site_url = "https://pos-e2e-test.example.com";
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
                    ("view_key_hex", WALLET_PRIVATE_VIEW_KEY),
                    ("spend_pubkey_hex", WALLET_PUBLIC_SPEND_KEY),
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

    // The connect flow's own response has no `connection_id` in it (only the
    // public `pk_...`, the identifier a storefront integration would use) -
    // the dashboard's own store link is where monokulo's *internal*
    // `connection_id` (what the POS route path actually needs) first appears.
    let dashboard_response = cp_router
        .clone()
        .oneshot(Request::builder().method("GET").uri("/dashboard").header("cookie", &session_cookie).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let dashboard_html = body_text(dashboard_response).await;
    let link_marker = "/dashboard/connections/";
    let link_start = dashboard_html.find(link_marker).expect("expected a real store link on the dashboard") + link_marker.len();
    let connection_id: String = dashboard_html[link_start..].chars().take_while(|c| c.is_alphanumeric() || *c == '-').collect();

    let ready: Value = json!({
        "engine_base_url": engine_base_url,
        "monokulo_base_url": monokulo_base_url,
        "public_key": public_key,
        "connection_id": connection_id,
        "email": email,
        "password": password,
        "session_cookie": session_cookie,
    });
    println!("POS_E2E_READY {ready}");
    use std::io::Write;
    std::io::stdout().flush().ok();

    // Both servers and the scan loop keep running on their own spawned
    // tasks - this task just needs to never return, so the process stays
    // alive until Node kills it (see `e2e/pos-playwright/README.md`).
    std::future::pending::<()>().await;
}
