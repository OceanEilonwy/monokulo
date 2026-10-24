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

const RANDOMX: &str =
    "the real-RandomX tests hash blocks, about 16 ms a hash; the rest run on a stand-in hasher";
const SQL_SWEEP: &str =
    "fails each SQL statement of a payment's life in turn, a range of them a test";
const REAL_CLOCK: &str = "waits on a real clock";
const REAL_STACK: &str = "one flow against the real engine and monokulo, its steps in order";
const MERCHANT_FLOW: &str = "a whole merchant flow, with several saves and reloads on one page";

pub(crate) const SLOW: &[(&str, &str)] = &[
    // Rust.
    ("engine › proof::", RANDOMX),
    ("engine › pow::", RANDOMX),
    ("engine › work::tests::every_sql_failure_", SQL_SWEEP),
    (
        "engine › work::tests::boundaries::every_recorded_ringct_variant_runs_complete_money_histories",
        "every recorded RingCT variant's whole money history, a sixth of them a test",
    ),
    (
        "engine › work::tests::boundaries::combined_portfolio_interactions_have_fixed_positive_controls",
        "a whole portfolio's money histories, each control run to the end",
    ),
    (
        "engine › work::tests::boundaries::concurrency::",
        "every schedule of concurrent callers, admission and shutdown",
    ),
    (
        "engine › work::mempool::boundaries::scale::",
        "a scale scenario: floods of new transactions",
    ),
    (
        "engine › store::boundaries::",
        "upgrades a real database through each fault, and kills the process mid-migration",
    ),
    (
        "monokulo › api::runtime::reference::tests::the_reference_is_",
        "records every API exchange against a real engine to write the reference",
    ),
    (
        "monokulo › http::prefix_tests::",
        "walks every page, fragment and stream a signed-in admin can reach",
    ),
    (
        "engine › work::tests::scale::tenant_groups_recover_without_cross_credit",
        "recovers thousands of tenants group by group",
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
        "monokulo › settings::tests::the_onion_listener_starts_moves_and_stops_when_saved",
        "starts, moves and stops real listeners",
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
        "connection-challenge.spec.js › real challenge offers a ten second no-JavaScript continuation",
        "the one real ten-second wait of the no-JavaScript challenge",
    ),
    (
        "domains.spec.js › a domain is added on its own pages, and its check answers in place",
        MERCHANT_FLOW,
    ),
    ("live-view.spec.js › the engine page follows the engine live", REAL_CLOCK),
    ("pos-terminal.spec.js › real POS backgrounds, reloads, reopens, cancels, and searches an order", MERCHANT_FLOW),
    (
        "pos-terminal.spec.js › customer pays while the order is on screen and the merchant starts the next sale",
        MERCHANT_FLOW,
    ),
    ("store-settings.spec.js › a save says so on the card and in a toast", MERCHANT_FLOW),
    ("store-site.spec.js › a connected plugin locks the website", MERCHANT_FLOW),
    ("store-webhooks.spec.js › each webhook says how it is doing", MERCHANT_FLOW),
    ("wallet-page.spec.js › a wallet nothing uses is retired by typing its name", MERCHANT_FLOW),
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
