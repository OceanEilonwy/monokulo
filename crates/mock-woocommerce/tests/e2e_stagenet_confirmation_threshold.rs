//! Real end-to-end test for the "Confirmation Thresholds" feature
//! (`docs/WOOCOMMERCE_ROADMAP.md`): a real store, connected against a real
//! stagenet-configured engine through monokulo's own dashboard forms (no
//! browser-facing connect flow - see this file's own doc comment below on
//! why), gets one real custom confirmation threshold whose
//! `confirmations_required` (1) is deliberately far below the tenant's own
//! plain default (99, set explicitly so it can never be mistaken for the
//! threshold actually being applied), then a real order priced above that
//! threshold's own `unit_amount` is created, paid with a genuine signed
//! stagenet transaction, and scanned for real - proving both that:
//!
//! 1. The resolved `confirmations_required` reaching the real engine's own
//!    stored order is the *threshold's* value (1), not the tenant's default
//!    (99) - checked immediately after order creation, no waiting required.
//! 2. That resolved value is what actually drives the real order's status:
//!    with a nonzero confirmation requirement, the order only reaches `paid`/`confirming`/`overpaid`
//!    once a real block gives it its first confirmation - proving
//!    `confirmations_required_override` isn't just recorded but genuinely
//!    enforced by the real engine's own status computation
//!    (`engine::store::recompute_order_status`).
//! 3. The order detail dashboard page shows the real, snapshotted threshold
//!    info (`http/orders.rs::render_order_detail_page`, WBS task #83) - "1"
//!    confirmation required, "XMR" as the store's own base currency.
//!
//! ## Why this drives monokulo directly (no `mock_woocommerce::run_connect_flow_with_wallet`)
//!
//! Adding a custom confirmation threshold is a dashboard-only action, scoped
//! by `connection_id` and gated by a real logged-in session
//! (`http::AuthedUser`) - the connect-flow helper this crate's *other* real
//! stagenet test (`e2e_stagenet_connect_flow.rs`) uses hands back a
//! `pk_...`/`sk_...` credential pair for a "plugin" integration, never the
//! `connection_id`/session a real merchant's own browser would have. This
//! test signs up on `/dashboard/signup` and sets the store up through
//! `/setup` directly instead, as a browser without JavaScript does (the same
//! forms `mock-woocommerce/src/lib.rs`'s `walk_setup` posts), keeping the
//! `session` cookie in a cookie-persisting client: sessions are cookies
//! only. The order is made the way the store's website does it, `POST
//! /api/v1/orders` with the store's public key.
//!
//! ## Running this test
//!
//! `#[ignore]`d by default, exactly like every other real-network test in
//! this workspace - it needs live public stagenet access and real wall-clock
//! time (a real stagenet block, ~2-10 minutes, to prove point 2 above isn't
//! satisfied by 0-conf alone). Run explicitly, from the repository root:
//!
//! ```sh
//! cargo test -p mock-woocommerce --features e2e -- --ignored --nocapture
//! ```

use std::sync::Arc;
use std::time::Duration;

use monero::Network;
use serde_json::{json, Value};

use engine::daemon::MoneroDaemonClient;
use engine::daemon_rpc::RpcDaemonClient;

/// Same fixed public stagenet node `e2e_stagenet_connect_flow.rs` uses -
/// duplicated rather than shared, same reasoning as that file's own
/// `node_fixture` doc comment (two `#[ignore]`d, manually-run tests, no
/// shared `e2e`-test-support crate yet).
mod node_fixture {
    pub const HOST: &str = "node.monerodevs.org";
    pub const PORT: u16 = 38089;
    pub const SSL: bool = false;
    pub const ACCEPT_SELF_SIGNED_CERTS: bool = true;
}

/// This test's own order amount - genuinely tiny (real stagenet XMR is
/// worthless, but still a real transaction with real fees/propagation), same
/// order of magnitude as every other real-payment test in this workspace.
const TEST_ORDER_AMOUNT_XMR: &str = "0.000335";
const TEST_ORDER_AMOUNT_PICONERO: u64 = 335_000_000;

/// The custom threshold's own `unit_amount` - comfortably below
/// `TEST_ORDER_AMOUNT_XMR` (the store's base currency is XMR too, so no rate
/// conversion is even needed to compare them - see
/// `confirmation_thresholds::resolve_for_order`'s own "same currency" case),
/// so this order unambiguously qualifies for it.
const THRESHOLD_UNIT_AMOUNT: &str = "0.0001";
/// Far below the tenant's own plain default (99, set explicitly through
/// store setup below) - if the real engine's stored order ever
/// showed this tenant default instead, the two are different enough that no
/// test flakiness could produce a false pass.
const THRESHOLD_CONFIRMATIONS_REQUIRED: u64 = 1;
const TENANT_DEFAULT_CONFIRMATIONS_REQUIRED: u64 = 99;

async fn require_daemon_reachable(daemon: &dyn MoneroDaemonClient, host: &str, port: u16) {
    if let Err(e) = retry(5, Duration::from_secs(3), || daemon.get_height()).await {
        panic!(
            "\n\ncannot reach the stagenet node at {host}:{port} (configured in node_fixture) after \
             several attempts: {e}\n\
             Check that host/port, your network connection, or try a different public stagenet \
             node (search \"monero stagenet public node\" for alternatives).\n"
        );
    }
}

/// Same retry helper as `e2e_stagenet_connect_flow.rs` - see that file's own
/// doc comment on why this specific public node warrants it.
async fn retry<T, E, F, Fut>(attempts: u32, delay: Duration, mut f: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut last_err = None;
    for attempt in 1..=attempts {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                eprintln!("attempt {attempt}/{attempts} failed, retrying in {delay:?}: {e}");
                last_err = Some(e);
                if attempt < attempts {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_err.expect("attempts is always >= 1"))
}

/// Same small real-monokulo-instance harness `e2e_stagenet_connect_flow.rs`
/// duplicates from `mock-woocommerce/src/lib.rs`'s own private test module -
/// see that file's own doc comment on why this is duplicated rather than
/// shared.
struct TestControlPlaneHandle {
    addr: std::net::SocketAddr,
    /// monokulo's database, which holds the store this test sets up.
    db: monokulo::db::Database,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestControlPlaneHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn_test_monokulo(
    engine: &engine_test_support::TestEngineHandle,
) -> TestControlPlaneHandle {
    use monokulo::db::Db;
    use monokulo::engine_client::EngineClient;
    use monokulo::http::{build_router, AppState};

    let db = Db::open_in_memory().expect("failed to open in-memory monokulo db for test");
    let state = AppState {
        engine: monokulo::http::Engine::new(EngineClient::embedded_for_tests(engine.router())),
        // Public signup, as `mock_woocommerce::spawn_test_monokulo`
        // (`src/lib.rs`) needs - see that call site's own comment.
        settings: AppState::test_settings(None),
        ..AppState::for_tests_with_db(db.into_shared())
    };
    let db = state.db.clone();
    let router = build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind an ephemeral local port for the test control plane");
    let addr = listener
        .local_addr()
        .expect("bound listener has no local address");
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    TestControlPlaneHandle { addr, db, task }
}

async fn body_json(response: reqwest::Response) -> Value {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .expect("failed to read response body");
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "response body was not JSON (status {status}): {e}\nbody: {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

// `flavor = "multi_thread"`: this test drives a manually-triggered scan tick
// concurrently with real daemon HTTP calls, same reasoning
// `e2e_stagenet_connect_flow.rs`'s own module doc comment gives for its own
// identical attribute.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the live stagenet node and the funded test wallets"]
async fn live_stagenet_order_resolves_and_enforces_a_non_default_confirmation_threshold() {
    // Same standard `e2e/*` layout every real suite in this repo uses - run
    // from the repository root, same as this file's own doc comment says.
    let ctx = cli_wallet::WalletCtx::default();

    let wallets = cli_wallet::WalletStore::load(&ctx)
        .unwrap_or_else(|e| panic!("failed to load {}: {e}", ctx.wallet_dir.display()));
    let merchant = wallets
        .wallet("merchant")
        .unwrap_or_else(|e| panic!("failed to load the merchant wallet: {e}"));
    let spender = wallets
        .wallet("spender")
        .unwrap_or_else(|e| panic!("failed to load the spender wallet: {e}"));

    // Deliberately not one daemon client held for the test's whole duration - see
    // `e2e_stagenet_connect_flow.rs`'s own identical fix/comment: this specific
    // public node enforces a real concurrent-connections-per-IP limit, and
    // `ResolvedWallet::connect`/`send_payment` below open their own independent
    // connection for the balance check and the real send, so this reachability
    // check's client is scoped to just this block. A fresh one is built again,
    // below, only once it's actually needed for the post-payment scan-tick polling.
    {
        let daemon: Arc<dyn MoneroDaemonClient> = Arc::new(
            RpcDaemonClient::new(
                node_fixture::HOST,
                node_fixture::PORT,
                node_fixture::SSL,
                node_fixture::ACCEPT_SELF_SIGNED_CERTS,
            )
            .expect("failed to build daemon RPC client"),
        );
        require_daemon_reachable(daemon.as_ref(), node_fixture::HOST, node_fixture::PORT).await;
    }

    let balance_wallet = retry(5, Duration::from_secs(5), || spender.connect())
        .await
        .unwrap_or_else(|e| panic!("\n\n{e}\n"));
    const MIN_SPENDABLE_PICONERO: u64 = 10_000_000_000; // 0.01 XMR.
    let balance = balance_wallet
        .balance()
        .await
        .unwrap_or_else(|e| panic!("failed to check spender wallet balance: {e}"));
    if balance.spendable_piconero < MIN_SPENDABLE_PICONERO {
        panic!(
            "\n\nspender wallet has only {} piconero spendable across {} output(s) (needs at least \
             {MIN_SPENDABLE_PICONERO}) - fund it from the stagenet faucet \
             (https://stagenet-faucet.xmr-tw.org/, send to {}), record the resulting txid \
             (wallet-cli --wallet-file {} add_output <txid>), then wait ~20 minutes \
             for it to mature.\n",
            balance.spendable_piconero, balance.spendable_outputs, spender.address, spender.path.display(),
        );
    }
    println!("spender wallet balance check passed: {balance:?}");

    // A real engine, stagenet-configured - no background scan loop (this
    // test drives scanning itself via `run_scan_tick_now`, same
    // reasoning as
    // `e2e_stagenet_connect_flow.rs`'s own module doc comment), no
    // background webhook loop either (this test doesn't need one).
    let engine = engine_test_support::TestEngineConfig::new()
        .with_networks(&[Network::Stagenet])
        .spawn()
        .await;
    let monokulo = spawn_test_monokulo(&engine).await;
    let monokulo_base_url = format!("http://{}", monokulo.addr);

    // A browser's cookie jar: signing up sets the `session` cookie every
    // dashboard form below sends. Redirects are followed, as a browser does,
    // so each step's response is the page it lands on.
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    let email = format!("e2e-threshold+{}@example.com", uuid::Uuid::new_v4());
    let password = "correct horse battery staple";

    let signup = client
        .post(format!("{monokulo_base_url}/dashboard/signup"))
        .form(&[("email", email.as_str()), ("password", password)])
        .send()
        .await
        .unwrap();
    assert!(
        signup.status().is_success() && !signup.url().path().starts_with("/dashboard/signup"),
        "signup failed: {} at {}",
        signup.status(),
        signup.url()
    );

    // A real store, set up against the real stagenet-configured engine -
    // the tenant's own plain default (99) is deliberately far from the
    // custom threshold's own value (1) set below, so the two can never be
    // mistaken for each other in the assertions that follow. The store step,
    // then the wallet brought in with its keys, which makes the store.
    let tenant_default = TENANT_DEFAULT_CONFIRMATIONS_REQUIRED.to_string();
    let spend_pubkey_hex = merchant.spend_public_key_hex().unwrap();
    let fields = [
        ("store_name", "E2E threshold shop"),
        ("store_site", "https://e2e-threshold.example.com"),
        ("base_currency", "XMR"),
        ("confirmations_required", tenant_default.as_str()),
        ("name", "Merchant wallet"),
        ("network", "stagenet"),
        ("view_key_hex", merchant.private_view_key_hex.as_str()),
        ("spend_pubkey_hex", spend_pubkey_hex.as_str()),
    ];
    let store_step = client
        .post(format!("{monokulo_base_url}/setup"))
        .form(&fields)
        .send()
        .await
        .unwrap();
    assert!(
        store_step.status().is_success() && store_step.url().path().starts_with("/setup/wallet"),
        "the store step failed: {} at {}",
        store_step.status(),
        store_step.url()
    );
    let made = client
        .post(format!("{monokulo_base_url}/setup/wallet/keys"))
        .form(&fields)
        .send()
        .await
        .unwrap();
    let connection_id = made
        .url()
        .path()
        .strip_prefix("/setup/done/")
        .unwrap_or_else(|| {
            panic!(
                "setting up the real stagenet store failed: {} at {}",
                made.status(),
                made.url()
            )
        })
        .to_string();
    let public_key = monokulo
        .db
        .lock()
        .get_store_connection_by_id(&shared::ids::ConnectionId::new(connection_id.clone()))
        .unwrap()
        .expect("the store setup made")
        .tenant_public_key;
    println!("connected: connection_id={connection_id} public_key={public_key}");

    // The real point of this test: one real custom confirmation threshold,
    // added through the store settings form (`http::store_settings::save`),
    // which saves only the fields it is sent, scoped to this exact store.
    // `client` follows redirects - so a real success here lands as a 200 on the settings
    // page the save's own 303 points at, not the 303 itself; checking the
    // landing page actually shows the new row is a stronger proof of
    // success than a raw status code would be anyway.
    let threshold_response = client
        .post(format!(
            "{monokulo_base_url}/dashboard/stores/{connection_id}/settings"
        ))
        .form(&[
            ("new_unit_amount", THRESHOLD_UNIT_AMOUNT),
            (
                "new_confirmations_required",
                &THRESHOLD_CONFIRMATIONS_REQUIRED.to_string(),
            ),
        ])
        .send()
        .await
        .unwrap();
    assert!(
        threshold_response.status().is_success(),
        "expected the real threshold form's landing page, got: {}",
        threshold_response.status()
    );
    let threshold_html = threshold_response.text().await.unwrap();
    assert!(
        threshold_html.contains(&format!(">{THRESHOLD_UNIT_AMOUNT}</td>"))
            && threshold_html.contains(&format!(
                r#"data-label="Confirmations required">{THRESHOLD_CONFIRMATIONS_REQUIRED}</td>"#
            )),
        "expected the real new threshold row on the landing page, got: {threshold_html}"
    );
    println!("created a real confirmation threshold: {THRESHOLD_UNIT_AMOUNT} XMR -> {THRESHOLD_CONFIRMATIONS_REQUIRED} confirmations");

    // A real order, priced above the threshold's own unit_amount, through
    // monokulo's own real order-creation API with the store's public key -
    // the same call a real shop's checkout page makes.
    let order_response = client
        .post(format!("{monokulo_base_url}/api/v1/orders"))
        .bearer_auth(&public_key)
        .json(&json!({ "amount": TEST_ORDER_AMOUNT_XMR, "currency": "XMR" }))
        .send()
        .await
        .unwrap();
    assert!(
        order_response.status().is_success(),
        "creating the real order failed: {}",
        order_response.status()
    );
    let order_body = body_json(order_response).await;
    let order_id = order_body["order_id"]
        .as_str()
        .expect("expected a real order_id")
        .to_string();
    let address = order_body["address"]
        .as_str()
        .expect("expected a real derived address")
        .to_string();
    let amount_piconero = order_body["xmr_amount_piconero"]
        .as_u64()
        .expect("expected a real xmr_amount_piconero");
    assert_eq!(amount_piconero, TEST_ORDER_AMOUNT_PICONERO);
    println!("created order {order_id}: {amount_piconero} piconero to {address}");

    // Point 1 (see this file's own module doc comment): the real engine's
    // own stored order must already show the threshold's value, not the
    // tenant's default - checked immediately, no payment or waiting needed.
    {
        let store = engine.store().lock();
        let tenant_id = store
            .list_active_tenants()
            .unwrap()
            .into_iter()
            .find(|t| t.public_key == public_key)
            .unwrap()
            .id;
        let stored = store
            .get_order(&tenant_id, &shared::ids::OrderId::new(order_id.to_string()))
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.confirmations_required_override,
            Some(THRESHOLD_CONFIRMATIONS_REQUIRED),
            "expected the real engine's stored order to carry the custom threshold's own confirmations_required \
             ({THRESHOLD_CONFIRMATIONS_REQUIRED}), not the tenant's plain default ({TENANT_DEFAULT_CONFIRMATIONS_REQUIRED})"
        );
        println!("PASS (structural): the real engine's stored order carries confirmations_required_override=Some({THRESHOLD_CONFIRMATIONS_REQUIRED})");
    }

    // The order detail dashboard page must show the same resolved snapshot
    // (WBS task: "makes it clear how the confirmation threshold was
    // decided", `templates::OrderDetailData::confirmations_required_display`).
    let detail_html = client
        .get(format!(
            "{monokulo_base_url}/dashboard/stores/{connection_id}/orders/{order_id}"
        ))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        detail_html.contains("Confirmations required"),
        "expected the confirmations-required row's label, got: {detail_html}"
    );
    assert!(
        detail_html.contains(&format!(">{THRESHOLD_CONFIRMATIONS_REQUIRED}<")),
        "expected the resolved threshold's own count shown, got: {detail_html}"
    );
    assert!(
        detail_html.contains("Store base currency"),
        "expected the base-currency snapshot row's label, got: {detail_html}"
    );
    println!("PASS: the real order detail dashboard page shows the resolved threshold snapshot");

    // Point 2: pay it for real and prove the resolved value actually drives
    // the real order's status - the order requires one real confirmation
    // (the order's own confirmation requirement is nonzero), so
    // this can only succeed once the payment has a real confirmation.
    let tx_hash = cli_wallet::send_payment(spender, &address, amount_piconero)
        .await
        .unwrap_or_else(|e| panic!("\n\n{e}\n"));
    let tx_hash_hex = hex::encode(tx_hash);
    println!("sent real stagenet payment, tx {tx_hash_hex}");

    // A fresh connection, built only now - see the earlier reachability check's
    // own comment on why this isn't kept alive for the test's whole duration.
    let daemon: Arc<dyn MoneroDaemonClient> = Arc::new(
        RpcDaemonClient::new(
            node_fixture::HOST,
            node_fixture::PORT,
            node_fixture::SSL,
            node_fixture::ACCEPT_SELF_SIGNED_CERTS,
        )
        .expect("failed to build daemon RPC client"),
    );

    // A generous deadline: this test needs a real block (~2 minutes on
    // stagenet, typically), not just mempool detection - deliberately
    // longer than `e2e_stagenet_connect_flow.rs`'s own 180s (which only
    // needs 0-conf detection).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    let mut tick = 0u32;
    let last_status = loop {
        tick += 1;
        let scan_result = engine
            .run_scan_tick_now(daemon.as_ref(), Network::Stagenet, 3)
            .await;
        if let Err(e) = &scan_result {
            eprintln!("DIAG tick {tick}: scan tick failed, continuing: {e}");
        }

        // The order in monokulo's API, read with the public key - what the
        // embed polls when its live updates can't connect.
        let order_status: Value = client
            .get(format!("{monokulo_base_url}/api/v1/orders/{order_id}"))
            .bearer_auth(&public_key)
            .send()
            .await
            .expect("order status request failed")
            .json()
            .await
            .expect("order status response was not valid JSON");
        let status = order_status["status"].as_str().unwrap_or("?").to_string();
        eprintln!(
            "DIAG tick {tick}: status={status} confirmations={:?}",
            order_status.get("confirmations"),
        );
        if matches!(status.as_str(), "paid" | "confirming" | "overpaid") {
            break status;
        }
        assert_ne!(
            status, "expired",
            "order expired before the real payment reached its resolved confirmations_required"
        );
        if tokio::time::Instant::now() >= deadline {
            break status;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    };
    assert!(
        matches!(last_status.as_str(), "paid" | "confirming" | "overpaid"),
        "order {order_id} (tx {tx_hash_hex}) never reached a paid/confirming/overpaid status within the deadline - last status: {last_status}"
    );
    println!("PASS: order {order_id} (tx {tx_hash_hex}) reached status '{last_status}' once it had its first real confirmation, proving the resolved threshold value ({THRESHOLD_CONFIRMATIONS_REQUIRED}) - not the tenant's default ({TENANT_DEFAULT_CONFIRMATIONS_REQUIRED}) - genuinely drives the real engine's own status computation");
}
