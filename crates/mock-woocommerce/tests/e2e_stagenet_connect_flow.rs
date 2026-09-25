//! WBS 1.4.5: the real stagenet end-to-end test that closes out Track 1.4 -
//! everything WBS 1.4.1-1.4.4 built, wired together for real: a fresh
//! monokulo account signs up, runs the *real* connect flow
//! (`mock_woocommerce::run_connect_flow_with_wallet`) using the real stagenet
//! **merchant** wallet's view key/spend pubkey from `e2e/stagenet-wallets.json`
//! against a real, stagenet-configured engine, creates a real order, pays it
//! with a real, signed stagenet transaction sent from the **customer** wallet
//! (`cli_wallet::send_payment` - the exact same fast, no-scanning,
//! cached-decoys wallet `tests/e2e_stagenet.rs` at the repo root already
//! proves, reused here as a library dependency, not reimplemented), drives a
//! real chain scan against the real node
//! (`scanner_test_support::TestEngineHandle::run_scan_tick_now` - see its own
//! doc comment for why this test drives scanning itself rather than through
//! `with_background_loops`'s automatic interval), and asserts the mock's real
//! webhook receiver got a real, correctly-signed delivery for it.
//!
//! Nothing here is mocked except the "plugin" itself (`mock-woocommerce` - the
//! whole point of this crate, standing in for the real WordPress plugin until
//! WBS 1.5): every account, HTTP round trip, tenant, order, transaction, chain
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
//! `#[ignore]`d by default, exactly like `tests/e2e_stagenet.rs` at the repo
//! root - it needs live network access (a real public stagenet node) and takes
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
//! connect-flow/scanning/webhook-delivery logic this task is meant to prove.
//! Native 0-conf resolves the test payment as soon as the scanner sees it in
//! the node's mempool, without waiting for a real block.
//!
//! Success is any of `paid`/`confirming`/`overpaid` - the same non-strict
//! "payment genuinely detected" criterion `tests/e2e_stagenet.rs` already uses -
//! since what this test is actually proving (a real connect flow -> real order
//! -> real payment -> real scan -> real signed webhook delivery, all the way
//! through) doesn't depend on which of those three exact terminal-ish statuses
//! the order lands in first.

use std::sync::Arc;
use std::time::Duration;

use monero::Network;
use serde_json::Value;

use scanner::daemon::MoneroDaemonClient;
use scanner::daemon_rpc::RpcDaemonClient;

use mock_woocommerce::{create_order, run_connect_flow_with_wallet, ConnectFlowWallet};

/// The real end-to-end test's fixed stagenet node - the same values
/// `crates/scanner/tests/support/mod.rs::e2e_fixture` uses (duplicated rather
/// than shared cross-crate for a handful of literals only ever read by two
/// `#[ignore]`d, manually-run tests). Replaces what used to be
/// `e2e/moneropay-stagenet.toml`, parsed via the now-removed
/// `scanner::config::Config` - see that module's own doc comment for why a
/// config file's not needed here any more.
mod node_fixture {
    pub const HOST: &str = "node.monerodevs.org";
    pub const PORT: u16 = 38089;
    pub const SSL: bool = false;
    pub const ACCEPT_SELF_SIGNED_CERTS: bool = true;
}

/// A real, directly XMR-denominated order (`docs/fx_refactor.md` follow-up:
/// XMR needs no exchange rate provider at all, not even a real one this
/// suite could otherwise keep nothing-mocked about) - 335_000_000 piconero
/// (0.000335 XMR), matching `tests/e2e_stagenet.rs`'s own tiny-payment
/// target at the repo root, so both real-stagenet tests move the same order
/// of magnitude of real (worthless, stagenet) XMR. Parsed at XMR's own
/// native 12-decimal precision (`shared::exchange_rate::compute_order_amount`),
/// not fiat's 2-decimal-place rounding - this amount genuinely needs that
/// precision (0.01 XMR granularity would round it to zero).
const TEST_ORDER_AMOUNT: &str = "0.000335";
const TEST_CURRENCY: &str = "XMR";

/// Confirms the configured stagenet node is actually reachable before doing anything
/// else with it - same reasoning as `tests/e2e_stagenet.rs`'s own
/// `require_daemon_reachable`: a misconfigured host/port or a temporarily-down node
/// would otherwise only surface deep inside the scan-and-poll loop as an opaque,
/// hard-to-diagnose failure after a real payment has already been sent. Retries a few
/// times first - observed directly while building this test, this specific public
/// node occasionally drops/refuses an individual connection attempt (a genuine,
/// transient low-level "error sending request", not a slow response) even though a
/// bare `curl` moments before or after succeeds reliably; a real, if flaky, node
/// deserves a couple of retries before being declared unreachable.
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

/// Retries `f` up to `attempts` times, sleeping `delay` between tries, returning the
/// last error if every attempt fails. See `require_daemon_reachable`'s doc comment
/// for why this specific public node warrants it: real, occasional transient
/// connection failures, not a fix for a genuinely unreachable/down node (which still
/// fails loudly here after exhausting every attempt).
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

/// A running, real (network-bound) monokulo instance for this test - the same
/// small, private harness `mock-woocommerce/src/lib.rs`'s own `#[cfg(test)] mod tests`
/// already defines, unavoidably duplicated here rather than shared: that module's
/// version is compiled only into the crate's own unit-test binary (a `#[cfg(test)]`
/// item, not part of the public `mock_woocommerce` library crate this integration
/// test links against), and no shared `monokulo-test-support` crate exists yet
/// (see this crate's own WBS 1.4.2 hand-off note on that same, deliberately-deferred
/// gap). This is now a second consumer of the exact same ~25 lines, which is worth
/// flagging as a real signal that extracting a small shared crate is due - left as a
/// follow-up rather than done inline here, to keep this task's diff to what it
/// actually needs.
struct TestControlPlaneHandle {
    addr: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestControlPlaneHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

const TEST_ENCRYPTION_KEY: [u8; 32] = [7u8; 32];

async fn spawn_test_monokulo(engine_addr: std::net::SocketAddr) -> TestControlPlaneHandle {
    use monokulo::db::Db;
    use monokulo::engine_client::EngineClient;
    use monokulo::http::{build_router, AppState};

    let db = Db::open_in_memory().expect("failed to open in-memory monokulo db for test");
    // Same "signup defaults to invite-only" fix `mock_woocommerce::spawn_test_monokulo`
    // (`src/lib.rs`) needs - see that call site's own comment.
    db.set_setting("signup.mode", "public").expect("failed to set signup.mode for test monokulo db");
    let state = AppState {
        db: db.into_shared(),
        engine_client: EngineClient::new(format!("http://{engine_addr}")),
        encryption_key: TEST_ENCRYPTION_KEY,
        status_cache: monokulo::http::status_page::new_status_cache(),
        // `TEST_CURRENCY` is `"XMR"` - needs no provider at all, so this
        // stays genuinely unconfigured, same as everything else in this
        // suite that isn't the "plugin" itself.
        exchange_rate: Arc::new(monokulo::exchange_rate_config::ExchangeRateProviders::xmr_only()),
        rate_limiter: Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
        event_streams: Default::default(),
        dns: Arc::new(monokulo::embed_domains::UnavailableDns("DNS is not available in tests".to_string())),
    };
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

    TestControlPlaneHandle { addr, task }
}

// `flavor = "multi_thread"`, not the bare `#[tokio::test]` default
// (single-threaded/`current_thread`): this test runs a manually-driven scan
// tick concurrently with the real background webhook-delivery-tick loop
// (`with_background_loops`) and its own real daemon HTTP calls, so it
// genuinely benefits from more than one OS thread the way production does.
//
// This was originally (wrongly) blamed for a reliable, node-independent,
// multi-minute-plus stall on this test's very first scan tick, on the theory
// that both loops contending for `Store`'s blocking `std::sync::Mutex` under
// a single-threaded runtime could deadlock. That was a red herring: the real
// cause was `with_background_loops`'s own `NoopDaemonClient`-driven scan-tick
// loop sharing this network's scanned-height watermark in `Store` with the
// real daemon this test drives via `run_scan_tick_now` - `NoopDaemonClient`
// always reports height 0, so its first tick poisoned the watermark to
// `Some(0)`, making the real scan try to walk every stagenet block one at a
// time from block 1 up to the real chain tip (millions of blocks). Fixed by
// `.without_background_scan_loop()` below, not by this attribute - see
// `TestEngineConfig::without_background_scan_loop`'s doc comment.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn real_stagenet_connect_flow_pays_a_real_order_end_to_end() {
    // Same standard `e2e/*` layout every real suite in this repo uses - run
    // from the repository root, same as this file's own doc comment says.
    let ctx = cli_wallet::WalletCtx::default();
    let wallets = cli_wallet::WalletStore::load(&ctx).unwrap_or_else(|e| panic!("failed to load {}: {e}", ctx.wallets_path));

    let merchant = wallets.wallet("merchant").unwrap_or_else(|e| panic!("failed to load the merchant wallet: {e}"));
    let spender = wallets.wallet("spender").unwrap_or_else(|e| panic!("failed to load the spender wallet: {e}"));

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
    // `send_payment`'s own retry loop - failing fast here, with a clear,
    // actionable message, is worth the small amount of code even though
    // this task's scope is otherwise just the one capstone test. A fresh
    // connect, not reused for the real send below - `ResolvedWallet::
    // connect` is cheap now (decoy selection is served from a committed
    // cache, not a live fetch - see `cli-wallet`'s own doc
    // comment), so there's no real cost to a second one, and `send_payment`
    // does its own connect internally regardless.
    let balance_wallet = retry(5, Duration::from_secs(5), || spender.connect()).await.unwrap_or_else(|e| panic!("\n\n{e}\n"));
    const MIN_SPENDABLE_PICONERO: u64 = 10_000_000_000; // 0.01 XMR - comfortably above one test payment + fee.
    let balance = balance_wallet.balance().await.unwrap_or_else(|e| panic!("failed to check spender wallet balance: {e}"));
    if balance.spendable_piconero < MIN_SPENDABLE_PICONERO {
        panic!(
            "\n\nspender wallet has only {} piconero spendable across {} output(s) (needs at least \
             {MIN_SPENDABLE_PICONERO}) - fund it from the stagenet faucet \
             (https://stagenet-faucet.xmr-tw.org/, send to {}), add a new ledger entry for the \
             resulting txid in {}, then wait ~20 minutes for it to mature.\n",
            balance.spendable_piconero, balance.spendable_outputs, spender.address, ctx.ledger_path,
        );
    }
    println!("spender wallet balance check passed: {balance:?}");

    // A real engine, stagenet-configured. `with_background_loops` is used for its
    // webhook-delivery-tick loop only (it's what actually POSTs the real, signed
    // webhook once an order is recomputed as paid) - its own scan-tick loop is
    // explicitly disabled via `without_background_scan_loop`. That loop's inert
    // `NoopDaemonClient` was *not* harmless here as originally assumed: it shares
    // this network's scanned-height watermark in `Store` with the real daemon this
    // test drives directly below via `run_scan_tick_now`, and `NoopDaemonClient`
    // always reports height 0 - so its very first tick poisoned the watermark to
    // `Some(0)`, making the real scan try to walk every stagenet block one at a time
    // from block 1 up to the real chain tip (millions of blocks). That is what was
    // actually behind this test's reliable, node-independent, multi-minute-plus
    // stalls on its first `run_scan_tick_now` call - not a bad node, not a mutex
    // deadlock. See `TestEngineConfig::without_background_scan_loop`'s doc comment.
    let engine = scanner_test_support::TestEngineConfig::new()
        .with_networks(&[Network::Stagenet])
        .with_background_loops()
        .without_background_scan_loop()
        .spawn()
        .await;
    let monokulo = spawn_test_monokulo(engine.addr).await;
    let monokulo_base_url = format!("http://{}", monokulo.addr);

    // The real connect flow, using the real stagenet merchant wallet's watch-only key
    // material - exactly what a real merchant would type into the real connect-flow
    // wallet form - instead of this driver's fixed mainnet test scalars.
    let credentials = run_connect_flow_with_wallet(
        &monokulo_base_url,
        ConnectFlowWallet {
            view_key_hex: merchant.private_view_key_hex.clone(),
            spend_pubkey_hex: merchant.spend_public_key_hex(),
            network: "stagenet".to_string(),
            confirmations_required: Some(0),
            base_currency: "XMR".to_string(),
        },
    )
    .await
    .expect("the real stagenet connect flow should succeed end to end against a real engine + control plane");
    assert_eq!(credentials.endpoint, format!("http://{}", engine.addr));
    println!(
        "connected: public_key={} endpoint={}",
        credentials.public_key, credentials.endpoint
    );

    let order = create_order(
        &monokulo_base_url,
        &credentials.public_key,
        TEST_ORDER_AMOUNT,
        TEST_CURRENCY,
    )
    .await
    .expect("order creation should succeed against the real stagenet-configured control plane");

    // `create_order` only returns `order_id`/`checkout_url` - fetch the real
    // derived address and exact XMR amount directly from the engine's own public,
    // unauthenticated order-status endpoint, the same one a real customer's browser
    // would poll (and the same one `tests/e2e_stagenet.rs` reads at the repo root).
    let order_status: Value = reqwest::get(format!(
        "{}/api/v1/t/{}/orders/{}",
        credentials.endpoint, credentials.public_key, order.order_id
    ))
    .await
    .expect("order status request failed")
    .json()
    .await
    .expect("order status response was not valid JSON");
    let address = order_status["address"]
        .as_str()
        .expect("expected a real derived address")
        .to_string();
    let amount_piconero = order_status["xmr_amount_piconero"]
        .as_u64()
        .expect("expected a real xmr_amount_piconero");
    println!(
        "created order {}: {amount_piconero} piconero to {address}",
        order.order_id
    );

    // Pay it for real: construct, sign, and broadcast the transaction ourselves (no
    // wallet-rpc or any other external wallet process) - the exact same
    // `cli_wallet::send_payment` machinery `tests/e2e_stagenet.rs` already
    // proves, reused here as a library dependency. Its own built-in retry (see that
    // crate's own doc comment) replaces this file's local `retry` helper for this
    // one call - the ledger write-back (this run's own new change output) happens
    // internally too, no separate record-keeping call needed here any more.
    let tx_hash =
        cli_wallet::send_payment(spender, &address, amount_piconero, None).await.unwrap_or_else(|e| panic!("\n\n{e}\n"));
    let tx_hash_hex = hex::encode(tx_hash);
    println!("sent real stagenet payment, tx {tx_hash_hex}");

    // Now the real proof: drive a real scan tick against the real node ourselves
    // (`run_scan_tick_now` - the same `run_scan_tick` `main.rs`'s production loop
    // calls on a timer, just invoked directly here instead of through
    // `with_background_loops`'s own automatic interval, per this test's own module
    // doc comment) and poll (not a fixed sleep) this driver's own real webhook
    // receiver - already registered against the real engine via `/finish` during the
    // connect flow above - until a real, delivered event for this exact order
    // arrives. Each tick has to actually see the broadcast transaction in the node's
    // mempool or a mined block, match it to this order's watch-only wallet, and
    // recompute its status; the engine's own real background delivery-tick loop
    // (still running, from `with_background_loops` above) then has to actually POST
    // a real, signed webhook to this receiver. A generous deadline, per this test's
    // own module doc comment on real stagenet timing - genuinely can take real
    // wall-clock minutes (mempool propagation across a public node, plus however
    // long until the next real block if 0-conf detection alone doesn't apply for any
    // reason).
    // Shortened from a prior 600s: this test either detects a payment within a
    // couple of ticks once the daemon has genuinely seen the broadcast tx (0-conf,
    // seconds), or something is actually wrong and waiting longer just delays
    // finding out. Rich per-tick diagnostics below are what actually answer "what's
    // wrong" - the deadline itself is just a backstop.
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
        let order_status: Result<Value, _> = async {
            reqwest::get(format!(
                "{}/api/v1/t/{}/orders/{}",
                credentials.endpoint, credentials.public_key, order.order_id
            ))
            .await?
            .json()
            .await
        }
        .await;
        eprintln!(
            "DIAG tick {tick}: daemon_height={height:?} tx_location={location:?} order_status={:?} amount_received={:?} confirmations={:?}",
            order_status.as_ref().ok().and_then(|v| v.get("status")),
            order_status.as_ref().ok().and_then(|v| v.get("amount_received_piconero")),
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
    // different channel (parsed straight out of `/finish`'s own JSON response) than
    // the receiver's internal verification state.
    assert!(
        shared::webhook_sign::verify_signature(
            &credentials.webhook_signing_secret,
            &event.raw_body,
            &event.signature
        ),
        "the delivered signature must verify against this tenant's real signing_secret"
    );
    assert!(
        !shared::webhook_sign::verify_signature(
            "definitely-the-wrong-secret",
            &event.raw_body,
            &event.signature
        ),
        "an arbitrary wrong secret must not verify the same real bytes/signature"
    );
}
