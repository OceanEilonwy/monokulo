//! Real end-to-end test for confirmation thresholds: a real store, connected against a real
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
//!    with a nonzero confirmation requirement, the order only reaches `paid`
//!    once a real block gives it its first confirmation, and with the
//!    default's 99 one confirmation would leave it `confirming` - proving
//!    `confirmations_required_override` isn't just recorded but genuinely
//!    enforced by the real engine's own status computation
//!    (`engine::store::recompute_order_status`).
//! 3. The order's page shows the real, snapshotted threshold
//!    info (`http/orders.rs::render_order_detail_page`) - "1"
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
//! test signs up on `/signup` and sets the store up through
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
//! time (a real stagenet block, ~2-45 minutes, to prove point 2 above isn't
//! satisfied by 0-conf alone). Run explicitly, from the repository root:
//!
//! ```sh
//! cargo test -p mock-woocommerce --features e2e -- --ignored --nocapture
//! ```

mod support;

use std::sync::Arc;
use std::time::Duration;

use monero::Network;
use serde_json::{json, Value};

use engine::daemon::{MoneroDaemonClient, TxLocation};
use engine::daemon_rpc::RpcDaemonClient;
use support::{node_fixture, require_daemon_reachable, retry};

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
/// This test's first subaddress index on the shared stagenet merchant
/// wallet. Every live test that pays an order starts at its own (`cargo
/// xtask live` checks they differ), so a payment an earlier test made, still
/// waiting in the pool for stagenet's next block, can never land on this
/// test's order: each test's engine is new, and would otherwise hand out
/// index 1 every time. Kept under 33: the CI merchant wallet watches indices
/// 0-32, so the run's sweep finds every payment.
const FIRST_MINOR_INDEX: u32 = 13;

/// How long a payment may wait for stagenet's next block. Its target is two
/// minutes, but with few miners gaps of ten and twenty minutes happen (one of
/// 22 on 2026-10-23). Under the hour the live profile gives a stagenet test
/// (.config/nextest.toml), so a slow stagenet fails with this test's own
/// message rather than a kill.
const BLOCK_TIMEOUT: Duration = Duration::from_secs(45 * 60);
/// How long the order has to read as paid once its transaction is in a
/// block: a few scan ticks.
const PAID_AFTER_BLOCK: Duration = Duration::from_secs(120);

/// A running, real (network-bound) monokulo instance for this test.
struct TestMonokulo {
    addr: std::net::SocketAddr,
    /// monokulo's database, which holds the store this test sets up.
    db: monokulo::db::Database,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestMonokulo {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn_test_monokulo(engine: &engine_test_support::TestEngineHandle) -> TestMonokulo {
    use monokulo::db::Db;
    use monokulo::engine_client::EngineClient;
    use monokulo::http::{build_router, AppState};

    let db = Db::open_in_memory().expect("failed to open in-memory monokulo db for test");
    let state = AppState {
        engine: monokulo::http::Engine::new(EngineClient::embedded_for_tests(engine.router())),
        // Public signup: the test signs up with no invite.
        settings: AppState::test_settings(None),
        ..AppState::for_tests_with_db(db.into_shared())
    };
    let db = state.db.clone();
    let router = build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind an ephemeral local port for the test monokulo");
    let addr = listener
        .local_addr()
        .expect("bound listener has no local address");
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    TestMonokulo { addr, db, task }
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
        .post(format!("{monokulo_base_url}/signup"))
        .form(&[("email", email.as_str()), ("password", password)])
        .send()
        .await
        .unwrap();
    assert!(
        signup.status().is_success() && !signup.url().path().starts_with("/signup"),
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
        .get_store_connection_by_id(&engine_protocol::ids::ConnectionId::new(
            connection_id.clone(),
        ))
        .unwrap()
        .expect("the store setup made")
        .tenant_public_key;
    println!("connected: connection_id={connection_id} public_key={public_key}");

    // This test's own subaddresses on the shared merchant wallet, from
    // FIRST_MINOR_INDEX (see its doc comment).
    {
        let store = engine.store().lock();
        for tenant in store.list_active_tenants().unwrap() {
            while store.peek_next_minor_index(&tenant.id).unwrap() < FIRST_MINOR_INDEX {
                store.allocate_minor_index(&tenant.id).unwrap();
            }
        }
    }

    // The real point of this test: one real custom confirmation threshold,
    // added through the store settings form (`http::store_settings::save`)
    // as its Payments tab posts it without JavaScript, which saves only the
    // fields it is sent, scoped to this exact store. `client` follows
    // redirects, so a real success lands as a 200 on the Payments tab the
    // save's own 303 points at, showing the new tier on the tier ladder and
    // the saved toast; the store's own threshold rows say the same.
    let threshold_response = client
        .post(format!(
            "{monokulo_base_url}/stores/{connection_id}/settings"
        ))
        .form(&[
            ("tab", "payments"),
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
    assert_eq!(
        threshold_response.url().path(),
        format!("/stores/{connection_id}/settings"),
        "expected the save to land back on the store's settings"
    );
    let threshold_html = threshold_response.text().await.unwrap();
    assert!(
        threshold_html.contains("<strong>Settings saved</strong>"),
        "expected the saved toast on the landing page, got: {threshold_html}"
    );
    assert!(
        threshold_html.contains(&format!(
            "<strong>From {THRESHOLD_UNIT_AMOUNT} XMR</strong><span>{THRESHOLD_CONFIRMATIONS_REQUIRED} confirmation</span>"
        )),
        "expected the real new tier on the landing page's tier ladder, got: {threshold_html}"
    );
    let tiers = monokulo
        .db
        .lock()
        .list_confirmation_thresholds(&engine_protocol::ids::ConnectionId::new(
            connection_id.clone(),
        ))
        .unwrap();
    assert_eq!(
        tiers
            .iter()
            .map(|t| (t.unit_amount.as_str(), t.confirmations_required))
            .collect::<Vec<_>>(),
        [(THRESHOLD_UNIT_AMOUNT, THRESHOLD_CONFIRMATIONS_REQUIRED)],
        "expected exactly the one tier the form added"
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
    // The order in the API, its `self` link resolved against where it was made.
    let order_url = url::Url::parse(&format!("{monokulo_base_url}/api/v1/orders"))
        .unwrap()
        .join(
            order_body["_links"]["self"]["href"]
                .as_str()
                .expect("expected the order's self link"),
        )
        .unwrap()
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
            .get_order(
                &tenant_id,
                &engine_protocol::ids::OrderId::new(order_id.to_string()),
            )
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

    // The order's page must show the same resolved snapshot, so the merchant
    // can see how the confirmation threshold was decided
    // (`views::orders`'s `confirmations_required_display`).
    let detail_html = client
        .get(format!(
            "{monokulo_base_url}/stores/{connection_id}/orders/{order_id}"
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

    // Stagenet's few miners leave its blocks far apart: two minutes is the
    // target, but gaps of ten and more are common. So the wait is for the
    // block itself, as the node reports it, up to `BLOCK_TIMEOUT`; once the
    // transaction is in one, the order has `PAID_AFTER_BLOCK` to read as
    // paid. A slow stagenet fails as one, not as a threshold that isn't
    // enforced.
    let block_deadline = tokio::time::Instant::now() + BLOCK_TIMEOUT;
    let mut paid_deadline = None;
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
            .get(&order_url)
            .bearer_auth(&public_key)
            .send()
            .await
            .expect("order status request failed")
            .json()
            .await
            .expect("order status response was not valid JSON");
        let status = order_status["status"].as_str().unwrap_or("?").to_string();
        // Asked after the status: a transaction still in the pool now was in
        // the pool when the status was read too.
        let location = daemon.locate_transaction(&tx_hash_hex).await;
        eprintln!(
            "DIAG tick {tick}: status={status} confirmations={:?} tx={location:?}",
            order_status.get("confirmations"),
        );
        let mined = matches!(location, Ok(TxLocation::InBlock(_)));
        // With the tenant's default of 99 the first confirmation would leave
        // the order `confirming`; only the tier's 1 makes it `paid`.
        if matches!(status.as_str(), "paid" | "overpaid") {
            assert!(
                !matches!(location, Ok(TxLocation::InPool)),
                "order {order_id} read as {status} while tx {tx_hash_hex} was still unconfirmed: \
                 a threshold of {THRESHOLD_CONFIRMATIONS_REQUIRED} confirmation was not enforced"
            );
            break status;
        }
        assert_ne!(
            status, "expired",
            "order expired before the real payment reached its resolved confirmations_required"
        );
        if mined && paid_deadline.is_none() {
            paid_deadline = Some(tokio::time::Instant::now() + PAID_AFTER_BLOCK);
        }
        let now = tokio::time::Instant::now();
        match paid_deadline {
            Some(deadline) if now >= deadline => break status,
            None if now >= block_deadline => panic!(
                "stagenet put tx {tx_hash_hex} in no block within {BLOCK_TIMEOUT:?} \
                 (last seen: {location:?}); the order is {status}"
            ),
            _ => {}
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    };
    assert!(
        matches!(last_status.as_str(), "paid" | "overpaid"),
        "order {order_id} (tx {tx_hash_hex}) was not paid {PAID_AFTER_BLOCK:?} after its block - last status: {last_status}"
    );
    println!("PASS: order {order_id} (tx {tx_hash_hex}) reached status '{last_status}' once it had its first real confirmation, proving the resolved threshold value ({THRESHOLD_CONFIRMATIONS_REQUIRED}) - not the tenant's default ({TENANT_DEFAULT_CONFIRMATIONS_REQUIRED}) - genuinely drives the real engine's own status computation");
}
