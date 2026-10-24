//! The tests allowed to take longer than `cargo xtask test-summary
//! --fail-slow` lets a test take: 5 s for a Rust test (a property test may
//! take longer), 15 s for a browser test. Each is listed by the start of the
//! name the summary gives it (`binary › path` for Rust, `spec › title` for
//! a browser test), with why it takes that long. A test over its limit
//! that isn't listed fails a pull request's summary, so a slow test is
//! either made faster or listed here with its reason.

/// How long a Rust test may take, in seconds.
pub(crate) const RUST_LIMIT_S: f64 = 5.0;
/// How long a browser test may take, in seconds.
pub(crate) const BROWSER_LIMIT_S: f64 = 15.0;

const RANDOMX: &str = "hashes real blocks with RandomX, about 16 ms a hash; F9 gives the logic tests a stand-in hasher";
const SQL_SWEEP: &str =
    "fails each SQL statement of a payment's life in turn; F8 splits the sweep by fault range";
const REAL_CLOCK: &str = "waits on a real clock; T-g gives the wait an explicit tuning";
const REAL_STACK: &str = "one flow against the real engine and monokulo, its steps in order";
const MERCHANT_FLOW: &str = "a whole merchant flow, with several saves and reloads on one page";

pub(crate) const SLOW: &[(&str, &str)] = &[
    // Rust.
    ("engine › proof::", RANDOMX),
    ("engine › pow::", RANDOMX),
    ("engine › work::tests::every_sql_failure_", SQL_SWEEP),
    (
        "engine › work::tests::scale::tenant_groups_recover_without_cross_credit",
        "recovers thousands of tenants group by group",
    ),
    (
        "engine › loops::tests::a_store_whose_backend_was_replaced_is_registered_again_within_seconds",
        REAL_CLOCK,
    ),
    (
        "engine › scanner::tests::random_outages_and_reorgs_never_lose_or_duplicate_a_payment",
        "many rounds of random outages and reorgs",
    ),
    (
        "engine › work::tests::the_fast_path_defers_what_its_budget_does_not_cover",
        "spends a real time budget",
    ),
    (
        "monokulo › http::engine_page::tests::the_machine_event_comes_at_once_then_at_most_every_few_seconds",
        REAL_CLOCK,
    ),
    (
        "monokulo › settings::tests::the_onion_listener_starts_moves_and_stops_when_saved",
        "starts, moves and stops real listeners",
    ),
    (
        "monokulo › http::status_page::tests::http_tests::monokulos_own_calls_to_an_embedded_engine_keep_the_status_dot_green",
        "runs an embedded engine",
    ),
    (
        "mock-woocommerce › tests::a_genuinely_forced_order_expired_webhook_is_delivered_and_verified",
        "a real engine and monokulo deliver a webhook through its retry",
    ),
    (
        "xtask::bin/xtask › pages::",
        "the first to run builds the site's ui islands with Vite, and the rest wait for it",
    ),
    (
        "xtask::bin/xtask › mutations::tests::the_intended_assertion_detects_the_named_defect",
        "compiles and runs a crate, the one real run the judge's table is checked against",
    ),
    // Browser.
    (
        "account-settings.spec.js › with JavaScript a change shows the save bar, and a new email is confirmed in a dialog",
        MERCHANT_FLOW,
    ),
    (
        "checkout.spec.js › real checkout payment problems as the customer sees them (UI stages)",
        MERCHANT_FLOW,
    ),
    (
        "connection-challenge.spec.js › real challenge offers a ten second no-JavaScript continuation",
        "the one real ten-second wait of the no-JavaScript challenge",
    ),
    (
        "connection-challenge.spec.js › real challenge continues inside a cross-site checkout frame with and without JavaScript",
        REAL_CLOCK,
    ),
    (
        "domains.spec.js › a domain is added on its own pages, and its check answers in place",
        MERCHANT_FLOW,
    ),
    (
        "live-view-hidden-tab.spec.js › a tab hidden while the reorg plays lists its last mark when shown again",
        REAL_CLOCK,
    ),
    ("live-view.spec.js › the engine page follows the engine live", REAL_CLOCK),
    ("live-view.spec.js › the machine strip and the Scanning panel follow the engine live", REAL_CLOCK),
    ("pos-terminal.spec.js › payment countdown keeps ticking down", REAL_CLOCK),
    ("pos-terminal.spec.js › real POS backgrounds, reloads, reopens, cancels, and searches an order", MERCHANT_FLOW),
    (
        "pos-terminal.spec.js › customer pays while the order is on screen and the merchant starts the next sale",
        MERCHANT_FLOW,
    ),
    ("refund-camera.spec.js › real checkout scans a refund QR with the camera", MERCHANT_FLOW),
    ("status-network-card.spec.js › the card follows the engine live", REAL_CLOCK),
    ("store-settings.spec.js › a save says so on the card and in a toast", MERCHANT_FLOW),
    ("store-settings.spec.js › a change marks its card and shows the save bar", MERCHANT_FLOW),
    ("store-site.spec.js › a connected plugin locks the website", MERCHANT_FLOW),
    ("store-site.spec.js › disconnecting waits while an order from the shop can be paid", MERCHANT_FLOW),
    ("store-webhooks.spec.js › each webhook says how it is doing", MERCHANT_FLOW),
    ("store-webhooks.spec.js › a delivery's details open as a dialog", MERCHANT_FLOW),
    ("store-webhooks.spec.js › a webhook with many deliveries pages through them", MERCHANT_FLOW),
    ("wallet-details.spec.js › the Details card renames the wallet", MERCHANT_FLOW),
    ("wallet-details.spec.js › without JavaScript the wallet is renamed with the save bar", MERCHANT_FLOW),
    ("wallet-page.spec.js › the retire dialog is blocked while a store takes payments", MERCHANT_FLOW),
    ("wallet-page.spec.js › where it lives says how each wallet was backed up", MERCHANT_FLOW),
    ("wallet-page.spec.js › a wallet nothing uses is retired by typing its name", MERCHANT_FLOW),
    ("wallets-networks.spec.js › with only test wallets the fold starts open", MERCHANT_FLOW),
    ("wallets-networks.spec.js › mainnet wallets are the list and the test networks are folded", MERCHANT_FLOW),
    ("admin-query.spec.js › ", REAL_STACK),
    ("admin-settings-and-nodes.spec.js › ", REAL_STACK),
    ("engine-crash-recovery.spec.js › ", REAL_STACK),
    ("log-search-and-tracing.spec.js › ", REAL_STACK),
    ("node-settings.spec.js › a node saved on a fresh instance applies straight away", REAL_CLOCK),
    ("pos-session-diagnostics.spec.js › ", REAL_STACK),
    ("site-themes.spec.js › ", REAL_STACK),
    ("wallet-setup.spec.js › ", REAL_STACK),
];

/// Whether a test named `name` in a summary may take longer than its limit.
pub(crate) fn listed(name: &str) -> bool {
    SLOW.iter().any(|(start, _)| name.starts_with(start))
}
