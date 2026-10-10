//! Boots the *real* backend for the POS screen's browser-driven e2e suite
//! (`e2e/browser/`) - a genuinely network-bound `scanner` engine
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
//! own spawned tasks) until killed - see `e2e/browser/README.md` for
//! the full protocol Node's own side follows against that line.
//!
//! `#[cfg(feature = "e2e")]`-equivalent via `required-features` in
//! `Cargo.toml` - this binary doesn't even exist in a normal build (needs the
//! same real transaction-signing dependencies `cli-wallet` does,
//! via `monokulo`'s own `engine-test-support` -> `scanner` dev-dependency
//! chain being irrelevant here; what actually gates this is the
//! `monokulo`/`cli-wallet`/`http-body-util` optional deps, all
//! behind the same `e2e` feature every other real-stagenet test in this
//! crate uses).

// Test infrastructure Playwright drives: it fails loudly, says it is ready
// on stdout and reports a failed tick on stderr.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "an e2e test harness: it fails by panicking and talks over stdout"
)]

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::BodyExt as _;
use monero::Network;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt as _;

use engine::daemon::MoneroDaemonClient;
use engine::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use engine::daemon_rpc::RpcDaemonClient;
use engine::http::rate_limit::RateLimiter;
use engine::http::{build_router as build_engine_router, now_unix, AppState as EngineAppState};
use engine::key_custody::{KeyCustody, PlainKeyCustody, WalletHandle};
use engine::network::network_str;
use engine::scanner::run_scan_tick;
use engine::scanner_status::new_scanner_status_map;
use engine::store::Store;

use monokulo::db::Db;
use monokulo::engine_client::EngineClient;
use monokulo::http::{build_router as build_monokulo_router, AppState as ControlPlaneAppState};

// Deliberately duplicated from `tests/support/mod.rs::e2e_fixture` rather
// than imported - a `[[bin]]` target has no access to `tests/`-local modules
// at all (that's the whole reason this is a `[[bin]]`, see this file's own
// doc comment), and the fixture is a handful of stable constants, not
// meaningfully-sized logic worth restructuring the crate to share.
const NODE_HOST: &str = "node.monerodevs.org";
const NODE_PORT: u16 = 38089;
/// Tried in order when `NODE_HOST` stops answering - the same fallbacks
/// `e2e/moneropay-stagenet.toml` configures. These public nodes rate-limit
/// one address: with the scanner and the payment wallet in this one process
/// both talking to `NODE_HOST`, it resets connections, and the scanner then
/// never sees the payment.
const FALLBACK_NODE_HOSTS: [&str; 2] = ["node2.monerodevs.org", "node3.monerodevs.org"];
const NODE_SSL: bool = false;
const NODE_ACCEPT_SELF_SIGNED_CERTS: bool = true;
const PAYMENT_REORG_CHECK_DEPTH: u64 = 20;

fn urlencode(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

fn form_body(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

async fn body_text(response: Response) -> String {
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
/// `engine::e2e_wallet::StagenetSpendWallet` with here, with its own
/// built-in connect-then-send retry - see that crate's own doc comments for
/// the full "why"), serialized against this process's own scan loop via
/// `network_lock` (see its own doc comment for why that's still worth
/// keeping even though the new wallet's own live-call count is already far
/// smaller).
async fn send_payment_handler(
    State(state): State<SendPaymentState>,
    Json(req): Json<SendPaymentRequest>,
) -> Response {
    let piconero_amount: u64 = match req.piconero_amount.parse() {
        Ok(n) => n,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("piconero_amount must be a plain integer: {e}"),
            )
                .into_response()
        }
    };
    let _guard = state.network_lock.lock().await;

    // The harness's own node first, then `WalletCtx::default()`'s other
    // stagenet nodes: the engine in this process keeps a connection open to
    // that node, and a public node rate-limiting this address would
    // otherwise fail the payment.
    let mut node_urls = vec![state.node_url.clone()];
    node_urls.extend(
        cli_wallet::DEFAULT_STAGENET_NODES
            .iter()
            .map(ToString::to_string)
            .filter(|url| *url != state.node_url),
    );
    let ctx = cli_wallet::WalletCtx {
        node_urls,
        ..Default::default()
    };
    let spender =
        match cli_wallet::WalletStore::load(&ctx).and_then(|store| store.wallet("spender")) {
            Ok(w) => w,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to load the spender wallet: {e}"),
                )
                    .into_response()
            }
        };
    let tx_hash = match cli_wallet::send_payment(spender, &req.address, piconero_amount).await {
        Ok(hash) => hash,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    Json(SendPaymentResponse {
        tx_hash: hex::encode(tx_hash),
    })
    .into_response()
}

#[tokio::main]
async fn main() {
    use std::io::Write as _;
    // ---- real, network-bound engine against the real public stagenet node ----
    let store = Store::open_in_memory().unwrap().into_shared();
    let key_custody: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
    let daemon: Arc<dyn MoneroDaemonClient> = Arc::new(
        RpcDaemonClient::new(
            NODE_HOST,
            NODE_PORT,
            NODE_SSL,
            NODE_ACCEPT_SELF_SIGNED_CERTS,
        )
        .expect("failed to build daemon RPC client"),
    );
    if let Err(e) = daemon.get_height().await {
        panic!("\n\ncannot reach the stagenet node at {NODE_HOST}:{NODE_PORT}: {e}\n");
    }
    let mut nodes = vec![FallbackNode {
        label: format!("{NODE_HOST}:{NODE_PORT}"),
        client: Arc::clone(&daemon),
    }];
    for host in FALLBACK_NODE_HOSTS {
        let client: Arc<dyn MoneroDaemonClient> = Arc::new(
            RpcDaemonClient::new(host, NODE_PORT, NODE_SSL, NODE_ACCEPT_SELF_SIGNED_CERTS)
                .expect("failed to build daemon RPC client"),
        );
        nodes.push(FallbackNode {
            label: format!("{host}:{NODE_PORT}"),
            client,
        });
    }
    let fallback_daemon = Arc::new(FallbackDaemonClient::new(nodes));
    let wallet_handles: Arc<RwLock<HashMap<engine::store::TenantId, WalletHandle>>> =
        Arc::new(RwLock::new(HashMap::new()));

    let engine_state = EngineAppState {
        db: engine::store::Database::inline(Arc::clone(&store)),
        admin_rate_limiter: Arc::new(RateLimiter::new(1_000_000)),
        log_store: None,
        engine_token: Arc::new(
            shared::auth::RawToken::presented(shared::auth::TEST_ENGINE_TOKEN).hash(),
        ),
        settings: engine::engine_settings::EngineSettings::defaults(),
        custody: engine::http::Custody {
            backends: Arc::clone(&key_custody),
            default_backend: "plain".to_owned(),
            wallet_handles: Arc::clone(&wallet_handles),
            snp: None,
        },
        networks: engine::http::Networks {
            daemons: engine::engine_settings::Daemons::fixed(HashMap::from([(
                Network::Stagenet,
                Arc::clone(&fallback_daemon),
            )])),
            scanner_status: new_scanner_status_map(),
        },
    };
    let engine_router = build_engine_router(engine_state, 1_000_000);
    let engine_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind an ephemeral engine port");
    let engine_addr = engine_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            engine_listener,
            engine_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("engine server error");
    });
    let engine_base_url = format!("http://{engine_addr}");

    // ---- real, network-bound monokulo, pointed at the engine above - Playwright's
    // browser needs a real socket to navigate to, unlike `e2e_dashboard_stagenet.rs`'s
    // own in-process-only `oneshot` driving ----
    let cp_state = ControlPlaneAppState {
        db: monokulo::db::Database::inline(Db::open_in_memory().unwrap().into_shared()),
        encryption_key: monokulo::crypto::AtRestKey::new([7u8; 32]),
        exchange_rate: Arc::new(monokulo::exchange_rate_config::ExchangeRateProviders::xmr_only()),
        abuse: Arc::default(),
        dns: Arc::new(monokulo::embed_domains::UnavailableDns(
            "DNS is not available in tests".to_owned(),
        )),
        log_store: None,
        // Public signup: `signup.mode` defaults to invite-only, which would
        // refuse the plain `/dashboard/signup` below (a re-rendered `200`
        // form, not the `302` it asserts on).
        settings: ControlPlaneAppState::test_settings(None),
        engine: monokulo::http::Engine::new(EngineClient::for_tests(engine_base_url.clone())),
    };
    let cp_router = build_monokulo_router(cp_state);
    let cp_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind an ephemeral monokulo port");
    let cp_addr = cp_listener.local_addr().unwrap();
    let cp_router_for_serve = cp_router.clone();
    tokio::spawn(async move {
        axum::serve(
            cp_listener,
            cp_router_for_serve.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("monokulo server error");
    });
    let monokulo_base_url = format!("http://{cp_addr}");

    // See `SendPaymentState::network_lock`'s own doc comment for why this
    // exists at all - shared by the scan loop below and the `/send-payment`
    // handler so this whole process never opens two connections to the real
    // node at once.
    let network_lock: Arc<AsyncMutex<()>> = Arc::new(AsyncMutex::new(()));
    let node_url = format!(
        "http{}://{NODE_HOST}:{NODE_PORT}",
        if NODE_SSL { "s" } else { "" }
    );

    // ---- the internal-only "send a real payment" endpoint (see
    // `send_payment_handler`'s own doc comment) - its own tiny router, bound
    // to its own ephemeral port, entirely separate from monokulo's real
    // production router above. ----
    let send_payment_state = SendPaymentState {
        node_url: node_url.clone(),
        network_lock: Arc::clone(&network_lock),
    };
    let send_payment_router = Router::new()
        .route("/send-payment", post(send_payment_handler))
        .with_state(send_payment_state);
    let send_payment_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind an ephemeral send-payment port");
    let send_payment_addr = send_payment_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(send_payment_listener, send_payment_router)
            .await
            .expect("send-payment server error");
    });
    let send_payment_url = format!("http://{send_payment_addr}/send-payment");

    // ---- a real background scan loop, since an external process (Playwright,
    // over real HTTP polling) - not this process's own sequential test code -
    // is what's watching for payment status changes this time; mirrors
    // `engine::main`'s own `run_scanner_loop`, simplified to the one network
    // this harness ever configures. A 3s interval - between the real
    // production scanner's typical fast poll and this harness's own earlier,
    // more defensive 10s (back when `/send-payment` still went through
    // `engine::e2e_wallet`, whose per-send RPC-call count scaled with a
    // known_txids list that only ever grew - see the git history around
    // `cli-wallet`'s introduction). That's no longer the dominant
    // concern: a steady-state send through the new wallet costs a handful
    // of calls regardless of history, so there's little left to protect by
    // ticking slowly, and a snappier loop matters again for the UI actually
    // settling from "seen in the mempool" to "paid" within a test's own
    // wait window. `network_lock` still rules out this loop ever *literally
    // overlapping* a `/send-payment` call either way. ----
    {
        let store = Arc::clone(&store);
        let key_custody = Arc::clone(&key_custody);
        // Through the fallback list, not the one node directly.
        let daemon: Arc<dyn MoneroDaemonClient> =
            Arc::<FallbackDaemonClient>::clone(&fallback_daemon);
        let wallet_handles = Arc::clone(&wallet_handles);
        let network_lock = Arc::clone(&network_lock);
        tokio::spawn(async move {
            loop {
                let tenants: Vec<(engine::store::TenantId, WalletHandle)> = wallet_handles
                    .read()
                    .iter()
                    .map(|(id, h)| (id.clone(), *h))
                    .collect();
                {
                    let _guard = network_lock.lock().await;
                    if let Err(e) = run_scan_tick(
                        &store,
                        key_custody.as_ref(),
                        daemon.as_ref(),
                        network_str(Network::Stagenet),
                        &tenants,
                        PAYMENT_REORG_CHECK_DEPTH,
                        0,
                    )
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
                .body(Body::from(form_body(&[
                    ("email", &email),
                    ("password", password),
                ])))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        signup_response.status(),
        StatusCode::FOUND,
        "real signup should redirect to login"
    );

    let login_response = cp_router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dashboard/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_body(&[
                    ("email", &email),
                    ("password", password),
                ])))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        login_response.status(),
        StatusCode::FOUND,
        "real login should redirect to /dashboard"
    );
    let set_cookie = login_response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let session_cookie = set_cookie.split(';').next().unwrap().to_owned();

    // Store setup (`/setup`) as a browser without JavaScript walks it: the
    // store step, then the wallet brought in with its keys, which makes the
    // store. The first Playwright payment settles at the mempool sighting
    // (`confirmations_required` 0); the second test changes this default to
    // 1 before creating its order and waits for a real block. The wallet is
    // the merchant's, from the same wallet directory the spender's is.
    let merchant = cli_wallet::WalletStore::load(&cli_wallet::WalletCtx::default())
        .and_then(|store| store.wallet("merchant"))
        .expect("failed to load the merchant wallet");
    let merchant_spend_pubkey = merchant.spend_public_key_hex().unwrap();
    let setup_fields = [
        ("kind", "web"),
        ("store_name", "POS e2e test"),
        ("store_site", "pos-e2e-test.example.com"),
        ("confirmations_required", "0"),
        ("name", "POS e2e wallet"),
        ("network", "stagenet"),
        ("view_key_hex", merchant.private_view_key_hex.as_str()),
        ("spend_pubkey_hex", merchant_spend_pubkey.as_str()),
    ];
    let mut done_path = String::new();
    for step in ["/setup", "/setup/wallet/keys"] {
        let response = cp_router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(step)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("cookie", &session_cookie)
                    .body(Body::from(form_body(&setup_fields)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::SEE_OTHER,
            "expected {step} to go on, not a refused form"
        );
        done_path = response.headers()["location"].to_str().unwrap().to_owned();
    }
    let connect_response = cp_router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&done_path)
                .header("cookie", &session_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let connect_html = body_text(connect_response).await;
    assert!(
        connect_html.contains("POS e2e test is set up"),
        "expected the store to be made, got: {connect_html}"
    );
    let pk_start = connect_html
        .find("pk_")
        .expect("expected a real pk_ value on the Done page");
    let public_key: String = connect_html[pk_start..]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    // The connect flow's own response has no `connection_id` in it (only the
    // public `pk_...`, the identifier a storefront integration would use) -
    // the dashboard's own store link is where monokulo's *internal*
    // `connection_id` (what the POS route path actually needs) first appears.
    let dashboard_response = cp_router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header("cookie", &session_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let dashboard_html = body_text(dashboard_response).await;
    let link_marker = "/dashboard/stores/";
    let link_start = dashboard_html
        .find(link_marker)
        .expect("expected a real store link on the dashboard")
        + link_marker.len();
    let connection_id: String = dashboard_html[link_start..]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '-')
        .collect();

    let ready = json!({
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
    let _ = std::io::stdout().flush();

    // Both servers and the scan loop keep running on their own spawned
    // tasks - this task just needs to never return, so the process stays
    // alive until Node kills it (see `e2e/browser/README.md`).
    std::future::pending::<()>().await;
}
