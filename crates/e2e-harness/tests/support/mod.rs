//! What this crate's real stagenet tests share. The payment itself is sent
//! by `cli_wallet::Wallet`, and the merchant's keys come from its wallet
//! file (`cli_wallet::WalletStore`).

/// The real end-to-end tests' fixed stagenet node and payment settings.
/// Each `tests/*.rs` file compiles as its own binary and uses only part of
/// them (`e2e_dashboard_stagenet.rs` sets its store up through monokulo's
/// pages, so it never seals wallet material itself), hence the
/// `dead_code` expectation.
#[expect(dead_code, reason = "each e2e test binary uses part of this module")]
pub(crate) mod e2e_fixture {
    // Switched to monerodevs.org (the community-curated stagenet node) after
    // stagenet.xmr-tw.org itself proved unreliable during earlier e2e work -
    // real, reproducible mid-request hangs/dropped connections, confirmed
    // independently against both e2e test files, not a fluke of one.
    pub(crate) const NODE_HOST: &str = "node.monerodevs.org";
    pub(crate) const NODE_PORT: u16 = 38089;
    pub(crate) const NODE_SSL: bool = false;
    pub(crate) const NODE_ACCEPT_SELF_SIGNED_CERTS: bool = true;

    // Real stagenet blocks land roughly every ~2 minutes; requiring any
    // confirmations at all would make these tests spend most of their time
    // waiting on the chain rather than exercising the scanner's own detection
    // logic - native 0-conf (see `status::derive_status`'s own doc comment)
    // settles the tiny test payment the instant it's seen in the mempool.
    pub(crate) const PAYMENT_CONFIRMATIONS_REQUIRED: u64 = 0;
    pub(crate) const PAYMENT_ORDER_EXPIRY_MINUTES: i64 = 30;
    pub(crate) const PAYMENT_REORG_CHECK_DEPTH: u64 = 20;
    pub(crate) const PAYMENT_MEMPOOL_POLL_INTERVAL_MS: u64 = 2000;
}
