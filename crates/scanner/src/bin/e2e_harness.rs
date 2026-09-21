//! Boots the *real* backend for the POS screen's browser-driven e2e suite
//! (`e2e/pos-playwright/`) - a genuinely network-bound `scanner` engine
//! talking to the real public stagenet node, a genuinely network-bound
//! `monokulo` (so an external browser, driven by Playwright over Node, can
//! actually reach it - unlike `tests/e2e_dashboard_stagenet.rs`, which only
//! ever drives monokulo's router in-process via `oneshot` since that test
//! *is* the client), one real signed-up account with one real store
//! connected to that engine, and this process's own internal
//! `/send-payment` endpoint (see `send_payment_handler` below) - the one
//! place `cli-wallet` ever gets called from in this whole suite.
//!
//! A real `[[bin]]`, not another `#[ignore]`d `#[tokio::test]` - see
//! `Cargo.toml`'s own comment on the `[[bin]]` entry for why: Playwright
//! (over Node's `child_process`) needs to spawn, read one line of stdout from,
//! and later cleanly kill this exact process, which is far simpler against a
//! predictable `target/debug/e2e-harness` binary than against `cargo
//! test`'s own hash-suffixed test binary or its `cargo` parent process.
//!
//! Prints exactly one JSON line to stdout once both servers are up and the
//! account/store exist, then blocks forever (both servers, a background
//! scan-tick loop, and the `/send-payment` endpoint keep running on their
//! own spawned tasks) until killed - see `e2e/pos-playwright/README.md` for
//! the full protocol Node's own side follows against that line.
//!
//! `#[cfg(feature = "e2e")]`-equivalent via `required-features` in
//! `Cargo.toml` - this binary doesn't even exist in a normal build (needs the
//! same real transaction-signing dependencies `cli-wallet` does,
//! via `monokulo`'s own `scanner-test-support` -> `scanner` dev-dependency
//! chain being irrelevant here; what actually gates this is the
//! `monokulo`/`cli-wallet`/`http-body-util` optional deps, all
//! behind the same `e2e` feature every other real-stagenet test in this
//! crate uses).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::BodyExt;
use monero::Network;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
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

#[derive(Clone)]
struct SendPaymentState {
    node_url: String,
    /// Held for the *entire* duration of any real call to the stagenet
    /// node - both here and around every `run_scan_tick` in `main`'s own
    /// background loop below - so this process never has two connections to
    /// the node open at once. Added after directly reproducing a real,
    /// consistent (not flaky) failure: the public node (or something in
    /// front of it) appears to allow only one concurrent connection per
    /// source IP, confirmed by running two of this harness's own processes
    /// against it at the same time and watching one fail *every* attempt for
    /// the other's entire ~250-600s decoy-selection window - see git log for
    /// the full investigation. A single in-process `tokio::sync::Mutex` is a
    /// complete fix *within this one harness* (it can't defend against some
    /// unrelated third party also hitting the node, but nothing else here
    /// does) - the reason payment-sending lives here, as an in-process HTTP
    /// endpoint sharing this same lock with the scan loop, rather than as a
    /// separate child process racing it for the node.
    network_lock: Arc<AsyncMutex<()>>,
}

#[derive(Deserialize)]
struct SendPaymentRequest {
    address: String,
    /// A plain numeral string, not a JSON number - the Node side computes
    /// this as a `BigInt` (`piconeroFromXmrDisplay` in `helpers.js`) and
    /// sends it as a string specifically so nothing on either side ever
    /// round-trips it through an IEEE-754 `f64`/JS `number`.
    piconero_amount: String,
}

#[derive(Serialize)]
struct SendPaymentResponse {
    tx_hash: String,
}

/// `POST /send-payment` on this harness's own small internal-only router
/// (bound to a separate ephemeral port, its URL handed to Playwright in the
/// `POS_E2E_READY` line as `send_payment_url`) - signs and broadcasts one
/// real stagenet transaction via `cli_wallet::send_payment` (the
/// fast, no-scanning, cached-decoys wallet this crate replaced
/// `scanner::e2e_wallet::StagenetSpendWallet` with here, with its own
/// built-in connect-then-send retry - see that crate's own doc comments for
/// the full "why"), serialized against this process's own scan loop via
/// `network_lock` (see its own doc comment for why that's still worth
/// keeping even though the new wallet's own live-call count is already far
/// smaller).
async fn send_payment_handler(State(state): State<SendPaymentState>, Json(req): Json<SendPaymentRequest>) -> Response {
    let piconero_amount: u64 = match req.piconero_amount.parse() {
        Ok(n) => n,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("piconero_amount must be a plain integer: {e}")).into_response(),
    };
    let _guard = state.network_lock.lock().await;

    // `state.node_url` and `WalletCtx::default()`'s own are the same value
    // (both built from the same NODE_HOST/NODE_PORT above) - set explicitly
    // anyway, so this stays correct if that ever changes.
    let ctx = cli_wallet::WalletCtx { node_url: state.node_url.clone(), ..Default::default() };
    let spender = match cli_wallet::WalletStore::load(&ctx).and_then(|store| store.wallet("spender")) {
        Ok(w) => w,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to load the spender wallet: {e}")).into_response(),
    };
    let tx_hash = match cli_wallet::send_payment(spender, &req.address, piconero_amount, None).await {
        Ok(hash) => hash,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    Json(SendPaymentResponse { tx_hash: hex::encode(tx_hash) }).into_response()
}

#[tokio::main]
async fn main() {
    // monokulo's `signup.mode` now defaults to `"invite_only"` (added after
    // this harness's sibling `tests/e2e_dashboard_stagenet.rs` was written -
    // that test's own bare signup, with no invite token, is now equally
    // affected) - a fresh in-memory `Db` has no stored override, so without
    // this the plain `/dashboard/signup` call below gets silently rejected
    // (a re-rendered `200` form, not the `302` it asserts on) instead of
    // creating the account. `settings::get` resolves this env var live on
    // every call (env > database > default), so setting it here before
    // `main` does anything else is enough - scoped to this one process only,
    // never touches the shared test file above.
    std::env::set_var("MONOKULO_SIGNUP_MODE", "public");

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
        daemons: Arc::new(HashMap::from([(Network::Stagenet, fallback_daemon.clone())])),
        rescan_daemons: Arc::new(HashMap::from([(Network::Stagenet, fallback_daemon)])),
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

    // See `SendPaymentState::network_lock`'s own doc comment for why this
    // exists at all - shared by the scan loop below and the `/send-payment`
    // handler so this whole process never opens two connections to the real
    // node at once.
    let network_lock: Arc<AsyncMutex<()>> = Arc::new(AsyncMutex::new(()));
    let node_url = format!("http{}://{NODE_HOST}:{NODE_PORT}", if NODE_SSL { "s" } else { "" });

    // ---- the internal-only "send a real payment" endpoint (see
    // `send_payment_handler`'s own doc comment) - its own tiny router, bound
    // to its own ephemeral port, entirely separate from monokulo's real
    // production router above. ----
    let send_payment_state = SendPaymentState { node_url: node_url.clone(), network_lock: network_lock.clone() };
    let send_payment_router = Router::new().route("/send-payment", post(send_payment_handler)).with_state(send_payment_state);
    let send_payment_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("failed to bind an ephemeral send-payment port");
    let send_payment_addr = send_payment_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(send_payment_listener, send_payment_router).await.expect("send-payment server error");
    });
    let send_payment_url = format!("http://{send_payment_addr}/send-payment");

    // ---- a real background scan loop, since an external process (Playwright,
    // over real HTTP polling) - not this process's own sequential test code -
    // is what's watching for payment status changes this time; mirrors
    // `scanner::main`'s own `run_scanner_loop`, simplified to the one network
    // this harness ever configures. A 3s interval - between the real
    // production scanner's typical fast poll and this harness's own earlier,
    // more defensive 10s (back when `/send-payment` still went through
    // `scanner::e2e_wallet`, whose per-send RPC-call count scaled with a
    // known_txids list that only ever grew - see the git history around
    // `cli-wallet`'s introduction). That's no longer the dominant
    // concern: a steady-state send through the new wallet costs a handful
    // of calls regardless of history, so there's little left to protect by
    // ticking slowly, and a snappier loop matters again for the UI actually
    // settling from "seen in the mempool" to "paid" within a test's own
    // wait window. `network_lock` still rules out this loop ever *literally
    // overlapping* a `/send-payment` call either way. ----
    {
        let store = store.clone();
        let key_custody = key_custody.clone();
        let daemon = daemon.clone();
        let wallet_handles = wallet_handles.clone();
        let network_lock = network_lock.clone();
        tokio::spawn(async move {
            loop {
                let tenants: Vec<(String, WalletHandle)> = wallet_handles.read().unwrap().iter().map(|(id, h)| (id.clone(), *h)).collect();
                {
                    let _guard = network_lock.lock().await;
                    if let Err(e) =
                        run_scan_tick(&store, key_custody.as_ref(), daemon.as_ref(), network_str(Network::Stagenet), &tenants, PAYMENT_REORG_CHECK_DEPTH, 0)
                            .await
                    {
                        eprintln!("e2e-harness: scan tick failed: {e}");
                    }
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
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
                    // `ConnectForm::base_currency` (`http::dashboard`) has no
                    // real default despite its `#[serde(default)]` (that
                    // only covers a missing form field, not what the
                    // engine/currency validation accepts - an empty string
                    // fails `crate::currencies::is_known_currency` outright)
                    // - added after `tests/e2e_dashboard_stagenet.rs`'s own
                    // identical connect call was written, which is why that
                    // test's own field list doesn't have this either. `"XMR"`
                    // matches every real amount this harness ever sends
                    // (spec point 2: the POS screen always prices in the
                    // store's own base currency).
                    ("base_currency", "XMR"),
                    // The engine hard-rejects `confirmations_required = 0`
                    // outright (`http::public::create_order`/
                    // `http::admin::validate_tenant_settings`: "0 would
                    // treat an unconfirmed transaction as final") - the
                    // real, engine-supported mechanism for 0-conf trust is
                    // this ceiling instead (`derive_status`'s own
                    // `zero_conf_trusted` branch: any order whose total is
                    // `<=` this, in piconero, is trusted the instant it's
                    // seen in the mempool, *regardless* of
                    // `confirmations_required`). Set strictly between the
                    // two Playwright tests' own amounts (335_000_000 /
                    // 336_000_000 piconero) so exactly one of them is
                    // 0-conf-trusted and the other still needs real
                    // confirmations, on purpose - see `tests/pos.spec.js`'s
                    // own comments on each.
                    ("zero_conf_max_piconero", "335500000"),
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
        "send_payment_url": send_payment_url,
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
