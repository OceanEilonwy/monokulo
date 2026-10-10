//! The real-money-costing wallet used to *implement* this test previously
//! (`engine::e2e_wallet::StagenetSpendWallet`, re-exported from here) has
//! been replaced by `crates/cli-wallet::Wallet` - see
//! that crate's own doc comment for why. This module now holds only the
//! shared stagenet fixture data every real e2e test in this crate still
//! needs regardless of which wallet implementation sends the payment.

/// The real end-to-end tests' fixed stagenet connection and payment
/// settings (the merchant's keys come from its wallet file,
/// `cli_wallet::WalletStore`) - replaces what used to be `e2e/moneropay-stagenet.toml`, parsed at
/// test time via the now-removed `engine::config::Config`. Plain Rust
/// constants instead of a TOML file: `e2e_stagenet.rs`/`e2e_dashboard_stagenet.rs`
/// were the only things that ever read that file (the real binary now reads
/// its settings from its own database, `engine::settings`, never a file at
/// all), so there is no reason left to keep a config-file-shaped indirection
/// around purely for these two tests. `#[allow(dead_code)]`: each `tests/*.rs`
/// file compiles as its own separate binary, and the two real callers don't use
/// an identical subset of these constants (e.g. `e2e_dashboard_stagenet.rs`
/// never seals wallet material directly, since its own tenant is provisioned
/// through a real HTTP connect flow instead) - a constant unused in *one*
/// binary but used in the other is expected here, not dead code to prune.
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
