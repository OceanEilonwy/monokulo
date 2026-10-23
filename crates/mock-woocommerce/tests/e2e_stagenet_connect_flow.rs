//! The real stagenet end-to-end test of a shop plugin's whole path: a fresh
//! monokulo account signs up, runs the *real* connect flow
//! (`mock_woocommerce::run_connect_flow_with_wallet`) using the real stagenet
//! **merchant** wallet's view key/spend pubkey from `e2e/wallets/merchant.db`
//! against a real, stagenet-configured engine, creates a real order, pays it
//! with a real, signed stagenet transaction sent from the **customer** wallet
//! (`cli_wallet::send_payment` - the same fast, no-scanning, cached-decoys
//! wallet `crates/e2e-harness/tests/e2e_stagenet.rs` uses), drives a
//! real chain scan against the real node
//! (`engine_test_support::TestEngineHandle::run_scan_tick_now` - see its own
//! doc comment for why this test drives scanning itself rather than through
//! `with_background_loops`'s automatic interval), and asserts the mock's real
//! webhook receiver got a real, correctly-signed delivery for it.
//!
//! Nothing here is mocked except the plugin itself (`mock-woocommerce`,
//! standing in for the WooCommerce plugin): every account, HTTP round trip, tenant, order, transaction, chain
//! scan, and webhook delivery involved is real.
//!
//! ## Why this needs the `e2e` feature
//!
//! Sending the real payment needs `cli_wallet::send_payment`,
//! which needs real transaction-construction dependencies
//! (`monero-wallet`/`monero-daemon-rpc`/`rand_core`/`curve25519-dalek`) gated
//! behind that crate's own `e2e`-flavored optionality (see `Cargo.toml`).
//! This crate's own `e2e` feature (see `mock-woocommerce/Cargo.toml`) just turns
//! that on transitively.
//!
//! ## Running this test
//!
//! `#[ignore]`d by default, like `crates/e2e-harness/tests/e2e_stagenet.rs` -
//! it needs live network access (a real public stagenet node) and takes
//! real wall-clock time (real stagenet block/propagation timing), so it can't
//! be part of the default `cargo test --workspace` pass. Run explicitly, from
//! the repository root:
//!
//! ```sh
//! cargo test -p mock-woocommerce --features e2e -- --ignored --nocapture
//! ```
//!
//! ## Why `confirmations_required = 0`
//!
//! Real stagenet blocks land roughly every ~2 minutes; waiting for the
//! engine's own default `confirmations_required` (10) would make this test
//! spend ~20 minutes waiting on the chain rather than exercising the actual
//! connect-flow/scanning/webhook-delivery logic this test is meant to prove.
//! Native 0-conf resolves the test payment as soon as the scanner sees it in
//! the node's mempool, without waiting for a real block.
//!
//! Success is any of `paid`/`confirming`/`overpaid` - the same non-strict
//! "payment genuinely detected" criterion `e2e_stagenet.rs` uses -
//! since what this test is actually proving (a real connect flow -> real order
//! -> real payment -> real scan -> real signed webhook delivery, all the way
//! through) doesn't depend on which of those three exact terminal-ish statuses
//! the order lands in first.

mod support;

use std::sync::Arc;
use std::time::Duration;

use monero::Network;
use serde_json::Value;

use engine::daemon::MoneroDaemonClient;
use engine::daemon_rpc::RpcDaemonClient;

use mock_woocommerce::{create_order, run_connect_flow_with_wallet, ConnectFlowWallet};
use support::{node_fixture, require_daemon_reachable, retry};

/// A real, directly XMR-denominated order (XMR needs no exchange rate
/// provider at all) - 335_000_000 piconero (0.000335 XMR), matching
/// `e2e_stagenet.rs`'s own tiny-payment target, so both real-stagenet tests move the same order
/// of magnitude of real (worthless, stagenet) XMR. Read as XMR at its
/// own precision (`shared::exchange_rate::compute_order_amount`), with no
/// rate to multiply through.
const TEST_ORDER_AMOUNT: &str = "0.000335";
const TEST_CURRENCY: &str = "XMR";

/// A running, real (network-bound) monokulo instance for this test, with its
/// webhook pipeline running - as `src/lib.rs`'s unit tests make one (that
/// one is `#[cfg(test)]`, so not visible here).
struct TestMonokulo {
    addr: std::net::SocketAddr,
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
    // Bound first so monokulo's public address (its `public_uri`) can
    // be this very listener - see `mock_woocommerce`'s own `spawn_test_monokulo`.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind an ephemeral local port for the test monokulo");
    let addr = listener
        .local_addr()
        .expect("bound listener has no local address");
    let state = AppState {
        engine: monokulo::http::Engine::new(EngineClient::embedded_for_tests(engine.router())),
        // Public signup (the flow signs up with no invite), and the
        // driver's webhook receiver is on this machine.
        settings: AppState::test_settings(Some(&format!("http://{addr}"))).with_webhooks(
            monokulo::settings::WebhookConfig {
                allow_private_urls: true,
                ..Default::default()
            },
        ),
        ..AppState::for_tests_with_db(db.into_shared())
    };
    // monokulo delivers the webhook: its order-event subscriber and its
    // delivery worker, as in production.
    let subscriber = monokulo::webhooks::subscriber::run_subscriber(
        state.db.clone(),
        state.engine.client.clone(),
        state.webhooks.clone(),
        state.apps.clone(),
    );
    let worker = monokulo::webhooks::delivery::run_delivery_loop(
        state.db.clone(),
        state.encryption_key.clone(),
        state.settings.clone(),
        state.webhooks.clone(),
    );
    let router = build_router(state);

    let task = tokio::spawn(async move {
        tokio::join!(
            async {
                let _ = axum::serve(listener, router).await;
            },
            subscriber,
            worker
        );
    });

    TestMonokulo { addr, task }
}

// `flavor = "multi_thread"`: this test runs a manually-driven scan tick
// alongside monokulo's webhook pipeline and its own real daemon HTTP calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the live stagenet node and the funded test wallets"]
async fn live_stagenet_connect_flow_pays_a_real_order_end_to_end() {
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

    // Deliberately not one `RpcDaemonClient` held for the test's whole duration -
    // this specific public node enforces a real, consistent (not flaky) modest
    // concurrent-connections-per-IP limit (observed directly while building this
    // test, with an earlier version whose background scan-tick loop drove a second,
    // independently-ticking daemon client concurrently with this test's own
    // foreground use - see `TestEngineHandle::run_scan_tick_now`'s own doc comment).
    // `cli_wallet::ResolvedWallet::connect`/`send_payment` below open
    // their *own* independent connection for the balance check and the real send -
    // so this reachability check's own client is scoped to just this block and
    // dropped immediately after, rather than kept alive (and pooled) across that
    // window too. A fresh one is built again, below, only once it's actually needed
    // for the post-payment scan-tick polling.
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

    // A cheap pre-flight balance check before this test spends any time on
    // the connect flow/order/engine setup that follows. A fund-starved
    // customer wallet otherwise only reveals itself deep inside
    // `send_payment`'s own retry loop. A fresh
    // connect, not reused for the real send below - `ResolvedWallet::
    // connect` is cheap now (decoy selection is served from a committed
    // cache, not a live fetch - see `cli-wallet`'s own doc
    // comment), so there's no real cost to a second one, and `send_payment`
    // does its own connect internally regardless.
    let balance_wallet = retry(5, Duration::from_secs(5), || spender.connect())
        .await
        .unwrap_or_else(|e| panic!("\n\n{e}\n"));
    const MIN_SPENDABLE_PICONERO: u64 = 10_000_000_000; // 0.01 XMR - comfortably above one test payment + fee.
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

    // A real engine, stagenet-configured, with no background scan loop
    // (monokulo, below, delivers the webhook from the engine's order-event
    // log). That loop's `NoopDaemonClient` always reports height 0 and shares
    // this network's scanned-height watermark with the real daemon this test
    // drives via `run_scan_tick_now`, so it would send the real scan back to
    // block 1. See `TestEngineConfig::with_background_loops`'s doc comment.
    let engine = engine_test_support::TestEngineConfig::new()
        .with_networks(&[Network::Stagenet])
        .spawn()
        .await;
    let monokulo = spawn_test_monokulo(&engine).await;
    let monokulo_base_url = format!("http://{}", monokulo.addr);

    // The real connect flow, using the real stagenet merchant wallet's watch-only key
    // material - exactly what a real merchant would type into the real connect-flow
    // wallet form - instead of this driver's fixed mainnet test scalars.
    let credentials = run_connect_flow_with_wallet(
        &monokulo_base_url,
        ConnectFlowWallet {
            view_key_hex: merchant.private_view_key_hex.clone(),
            spend_pubkey_hex: merchant.spend_public_key_hex().unwrap(),
            network: "stagenet".to_string(),
            confirmations_required: Some(0),

        },
    )
    .await
    .expect("the real stagenet connect flow should succeed end to end against a real engine and monokulo");
    assert_eq!(credentials.api, "/api/v1");
    println!(
        "connected: public_key={} api={}",
        credentials.public_key,
        credentials.api_url()
    );

    // Created the way the plugin does it: on monokulo, with the store's key.
    let order = create_order(
        &credentials.api_url(),
        &credentials.secret_token,
        TEST_ORDER_AMOUNT,
        TEST_CURRENCY,
    )
    .await
    .expect("order creation should succeed against the real stagenet-configured monokulo");

    // Monokulo's own create-order response carries the real derived address
    // and exact XMR amount - nothing needs to ask the (private) engine.
    let address = order.address.clone();
    let amount_piconero = order.xmr_amount_piconero;
    println!(
        "created order {}: {amount_piconero} piconero to {address}",
        order.order_id
    );

    // Pay it for real: construct, sign, and broadcast the transaction ourselves (no
    // wallet-rpc or any other external wallet process) with `cli_wallet::send_payment`,
    // which retries by itself and records this run's change output.
    let tx_hash = cli_wallet::send_payment(spender, &address, amount_piconero)
        .await
        .unwrap_or_else(|e| panic!("\n\n{e}\n"));
    let tx_hash_hex = hex::encode(tx_hash);
    println!("sent real stagenet payment, tx {tx_hash_hex}");

    // Now the real proof: drive a real scan tick against the real node ourselves
    // (`run_scan_tick_now`, the same scan the production loop runs on a timer)
    // and poll (not a fixed sleep) this driver's own webhook receiver -
    // registered via `/finish` during the connect flow above - until a
    // delivered event for this exact order arrives. Each tick has to see the
    // broadcast transaction in the node's mempool or a mined block, match it to
    // this order's watch-only wallet, and recompute its status; monokulo's
    // webhook pipeline then has to POST a real, signed webhook to this
    // receiver. With 0-conf a payment shows within a couple of ticks once the
    // node has the transaction; the deadline is a backstop, and the per-tick
    // diagnostics below say what's wrong.
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    let mut tick = 0u32;
    let matched = loop {
        tick += 1;
        let tick_started = tokio::time::Instant::now();
        // Reduced from 20: `check_for_reorg_and_reconcile` walks this many blocks
        // on *every* tick, not just the first - real per-tick cost against a real
        // (sometimes slow) public node, multiplied by however many ticks run. A
        // test proving detection works doesn't need production-depth reorg
        // protection; a small window is plenty to exercise the same code path.
        let scan_result = engine
            .run_scan_tick_now(daemon.as_ref(), Network::Stagenet, 3)
            .await;
        eprintln!(
            "DIAG tick {tick}: run_scan_tick_now took {:?}",
            tick_started.elapsed()
        );
        if let Err(e) = &scan_result {
            eprintln!("DIAG tick {tick}: scan tick failed, continuing: {e}");
        }

        // Three independent signals, so a failure to detect can be localized to one
        // of: (a) the daemon we're actually scanning against hasn't seen the tx at
        // all yet (a node-lag/propagation problem, nothing to do with our code), (b)
        // the daemon has it but the scanner isn't matching/recomputing anything (a
        // real bug in scan_tx_outputs or the recompute step), or (c) it matched but
        // the webhook never got delivered (a delivery-worker problem, not a
        // detection one).
        let height = daemon.get_height().await;
        let location = daemon.locate_transaction(&tx_hash_hex).await;
        // The order in monokulo's API, as the plugin reads it with its key.
        let order_status: Result<Value, _> =
            mock_woocommerce::read_order(&order, &credentials.secret_token).await;
        eprintln!(
            "DIAG tick {tick}: daemon_height={height:?} tx_location={location:?} order_status={:?} confirmations={:?}",
            order_status.as_ref().ok().and_then(|v| v.get("status")),
            order_status.as_ref().ok().and_then(|v| v.get("confirmations")),
        );

        let found = credentials.webhook_receiver.events().into_iter().find(|e| {
            e.payload.get("order_id").and_then(|v| v.as_str()) == Some(order.order_id.as_str())
                && matches!(
                    e.event.as_str(),
                    "order.paid" | "order.confirming" | "order.overpaid"
                )
        });
        if let Some(found) = found {
            break Some(found);
        }
        if tokio::time::Instant::now() >= deadline {
            break None;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };

    let event = matched.unwrap_or_else(|| {
        panic!(
            "order {} (tx {tx_hash_hex}) never produced a real paid/confirming/overpaid webhook delivery within the deadline",
            order.order_id
        )
    });
    println!(
        "PASS: received a real '{}' webhook delivery for order {} (tx {tx_hash_hex})",
        event.event, order.order_id
    );
    assert!(event.event_id.starts_with("evt_"));

    // Strong, non-circular proof this is a genuine, correctly-signed delivery - not
    // just a request that happened to arrive: independently re-verify the exact raw
    // bytes and signature the receiver actually recorded against
    // `credentials.webhook_signing_secret`, a value obtained through a completely
    // different channel (parsed straight out of `/finish`'s response) than
    // the receiver's internal verification state.
    assert!(
        shared::webhook_sign::verify_signature(
            &credentials.webhook_signing_secret,
            &event.raw_body,
            &event.signature,
            shared::time::now_unix()
        ),
        "the delivered signature must verify against this store's real signing secret"
    );
    assert!(
        !shared::webhook_sign::verify_signature(
            "definitely-the-wrong-secret",
            &event.raw_body,
            &event.signature,
            shared::time::now_unix()
        ),
        "an arbitrary wrong secret must not verify the same real bytes/signature"
    );
}
