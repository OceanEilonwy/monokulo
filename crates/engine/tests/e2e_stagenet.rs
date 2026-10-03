//! Real end-to-end test: creates an order through the actual `scanner`
//! library (config -> store -> key custody -> scanner -> router - the same pieces
//! `main.rs` wires together, just driven directly instead of over a bound TCP
//! socket), pays it with a genuine tiny transaction constructed, signed, and
//! broadcast entirely in Rust (see `crates/cli-wallet`) from a real,
//! faucet-funded Monero **stagenet** wallet, and asserts the real chain scanner
//! detects it.
//!
//! The only external dependency this test has is the public stagenet node itself
//! (`support::e2e_fixture`) - no wallet-rpc or any other external process.
//! Sending the payment is done by `cli_wallet::Wallet` -
//! see that crate's own doc comment for why it's a separate, narrower wallet
//! from a general-purpose one, built specifically for this kind of real,
//! repeated, e2e-test use.
//!
//! Follows the same pattern as `daemon_rpc::live_node_tests`: excluded from the
//! default `cargo test` run via `#[ignore]` (it needs live network access, so it
//! can't be hermetic), run explicitly with:
//!
//! ```sh
//! cargo test --test e2e_stagenet -- --ignored --nocapture
//! ```
//!
//! Wallet files (`e2e/wallets/`) are found relative to the `cli-wallet` crate,
//! whatever `cargo test`'s working directory is. See `e2e/README.md`
//! for the full picture.

// An integration test crate: every function in it is test code, which
// fails by panicking.
#![expect(
    clippy::tests_outside_test_module,
    clippy::unwrap_used,
    clippy::panic,
    reason = "an integration test crate is all test code"
)]

mod support;

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use monero::Network;
use serde_json::{json, Value};
use tower::ServiceExt as _;

use engine::daemon::MoneroDaemonClient;
use engine::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use engine::daemon_rpc::RpcDaemonClient;
use engine::http::rate_limit::RateLimiter;
use engine::http::{build_router, now_unix, AppState};
use engine::key_custody::{KeyCustody, PlainKeyCustody, WalletMaterial};
use engine::network::network_str;
use engine::scanner::run_scan_tick;
use engine::store::{NewTenant, Store};

/// Confirms the configured stagenet node is actually reachable before doing
/// anything else with it - a misconfigured host/port, or a node that's temporarily
/// down, would otherwise only surface deep inside the scan-and-poll loop as an
/// opaque `DaemonError` after the payment has already been sent.
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

/// One request against the engine's admin API, authenticated with the
/// tenant's `sk_` - the only way orders are created or read now.
async fn oneshot_json(
    router: &axum::Router,
    sk: &str,
    method: &str,
    uri: String,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(
            shared::auth::ENGINE_TOKEN_HEADER,
            shared::auth::TEST_ENGINE_TOKEN,
        )
        .header("authorization", format!("Bearer {sk}"));
    let body = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("response body was not JSON: {e}"));
    (status, json)
}

#[tokio::test]
#[ignore = "needs the live stagenet node and a funded test wallet"]
async fn real_stagenet_payment_is_detected_end_to_end() {
    use support::e2e_fixture;

    let ctx = cli_wallet::WalletCtx::default();
    let spender = cli_wallet::WalletStore::load(&ctx)
        .unwrap_or_else(|e| panic!("failed to load {}: {e}", ctx.wallet_dir.display()))
        .wallet("spender")
        .unwrap_or_else(|e| panic!("failed to load the spender wallet: {e}"));

    // Same boot sequence as main.rs's happy path, minus the webhook loop (not
    // exercised by this test) and the bound TCP listener (the router is driven
    // in-process via `oneshot` instead).
    let store = Store::open_in_memory().unwrap().into_shared();
    let key_custody: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
    let daemon: Arc<dyn MoneroDaemonClient> = Arc::new(
        RpcDaemonClient::new(
            e2e_fixture::NODE_HOST,
            e2e_fixture::NODE_PORT,
            e2e_fixture::NODE_SSL,
            e2e_fixture::NODE_ACCEPT_SELF_SIGNED_CERTS,
        )
        .expect("failed to build daemon RPC client"),
    );
    require_daemon_reachable(
        daemon.as_ref(),
        e2e_fixture::NODE_HOST,
        e2e_fixture::NODE_PORT,
    )
    .await;

    let material = WalletMaterial::from_hex(
        e2e_fixture::WALLET_PRIVATE_VIEW_KEY,
        e2e_fixture::WALLET_PUBLIC_SPEND_KEY,
    )
    .expect("invalid wallet key material in support::e2e_fixture");
    let sealed = key_custody.seal(&material).await.unwrap();
    let created = store
        .lock()
        .create_tenant(
            &NewTenant {
                key_custody_backend: "plain".to_owned(),
                sealed_key_material: sealed,
                primary_address: e2e_fixture::WALLET_PRIMARY_ADDRESS.to_owned(),
                network: e2e_fixture::WALLET_NETWORK.to_owned(),
                confirmations_required: Some(e2e_fixture::PAYMENT_CONFIRMATIONS_REQUIRED),
                order_expiry_seconds: Some(e2e_fixture::PAYMENT_ORDER_EXPIRY_MINUTES * 60),
            },
            now_unix(),
        )
        .expect("failed to create tenant");
    let sk = created.secret_token.clone();
    let handle = key_custody
        .unseal_and_register(&created.tenant.sealed_key_material)
        .await
        .unwrap();
    let wallet_handles = Arc::new(RwLock::new(HashMap::from([(
        created.tenant.id.clone(),
        handle,
    )])));

    let app_state = AppState {
        db: engine::store::Database::inline(Arc::clone(&store)),
        admin_rate_limiter: Arc::new(RateLimiter::new(10_000)),
        settings: engine::engine_settings::EngineSettings::defaults(),
        log_store: None,
        engine_token: Arc::new(
            shared::auth::RawToken::presented(shared::auth::TEST_ENGINE_TOKEN).hash(),
        ),
        custody: engine::http::Custody {
            backends: Arc::clone(&key_custody),
            default_backend: "plain".to_owned(),
            wallet_handles,
        },
        // This test drives scanning directly via `run_scan_tick` below (not
        // through `AppState` at all - see that call site's own comment), so
        // these three exist only to satisfy `AppState`'s shape, not because
        // this test's own logic reads them. Still wired to the same real
        // `daemon` this test already built, rather than a disconnected
        // placeholder, so `AppState` stays internally honest.
        networks: engine::http::Networks {
            daemons: engine::engine_settings::Daemons::fixed(HashMap::from([(
                Network::Stagenet,
                Arc::new(FallbackDaemonClient::new(vec![FallbackNode {
                    label: format!("{}:{}", e2e_fixture::NODE_HOST, e2e_fixture::NODE_PORT),
                    client: Arc::clone(&daemon),
                }])),
            )])),
            scanner_status: engine::scanner_status::new_scanner_status_map(),
        },
    };
    let router = build_router(app_state, 1_000_000);

    // -- create a real order for a genuinely tiny amount (see e2e/README.md for
    // why this is a few hundred thousand piconero, not a realistic amount) --
    let (status, order) = oneshot_json(
        &router,
        sk.expose(),
        "POST",
        "/api/v1/admin/tenant/orders".to_owned(),
        Some(json!({
            "merchant_order_id": format!("rust-e2e-{}", now_unix()),
            "xmr_amount_piconero": 335_000_000u64,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "order creation failed: {order:?}");
    let order_id = order["order_id"].as_str().unwrap().to_owned();
    let address = order["address"].as_str().unwrap().to_owned();
    let amount_piconero = order["xmr_amount_piconero"].as_u64().unwrap();
    println!("created order {order_id}: {amount_piconero} piconero to {address}");

    // -- pay it for real: construct, sign, and broadcast the transaction ourselves
    // (no wallet-rpc or any other external wallet process - see
    // crates/cli-wallet, whose own `send_payment` retries the whole
    // connect-then-send sequence internally on real, observed node flakiness) --
    let tx_hash = cli_wallet::send_payment(spender, &address, amount_piconero)
        .await
        .unwrap_or_else(|e| panic!("\n\n{e}\n"));
    let tx_hash_hex = hex::encode(tx_hash);
    println!("sent real stagenet payment, tx {tx_hash_hex}");

    // -- drive the real scanner ourselves (no background task/sleep loop needed -
    // this *is* the same `run_scan_tick` the production loop in main.rs calls on a
    // timer) and poll until the payment is detected --
    let tenants = vec![(created.tenant.id.clone(), handle)];
    let mut last_status = String::new();
    for attempt in 1..=30 {
        run_scan_tick(
            &store,
            key_custody.as_ref(),
            daemon.as_ref(),
            network_str(Network::Stagenet),
            &tenants,
            e2e_fixture::PAYMENT_REORG_CHECK_DEPTH,
            0,
        )
        .await
        .expect("scan tick failed");

        let (status, order_status) = oneshot_json(
            &router,
            sk.expose(),
            "GET",
            format!("/api/v1/admin/tenant/orders/{order_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        last_status = order_status["status"].as_str().unwrap().to_owned();
        println!("[{attempt}/30] status={last_status}");

        if matches!(last_status.as_str(), "paid" | "confirming" | "overpaid") {
            println!("PASS: order {order_id} reached status '{last_status}' (tx {tx_hash_hex})");
            return;
        }
        assert_ne!(
            last_status, "expired",
            "order expired before the real payment was detected"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    panic!("order {order_id} still '{last_status}' after 30 scan attempts - real stagenet payment (tx {tx_hash_hex}) was not detected");
}
