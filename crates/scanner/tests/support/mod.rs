//! Re-exports the real-Monero-transaction spend wallet from the main crate - see
//! `scanner::e2e_wallet` for the actual implementation and its doc
//! comment. Kept as a thin `tests/support` module (rather than importing
//! `scanner::e2e_wallet` directly in `tests/e2e_stagenet.rs`) purely so
//! that test file's `use support::StagenetSpendWallet;` doesn't need to change.
pub use scanner::e2e_wallet::*;

/// The real end-to-end tests' fixed stagenet connection + bootstrap-wallet
/// fixture - replaces what used to be `e2e/moneropay-stagenet.toml`, parsed at
/// test time via the now-removed `scanner::config::Config`. Plain Rust
/// constants instead of a TOML file: `e2e_stagenet.rs`/`e2e_dashboard_stagenet.rs`
/// were the only things that ever read that file (the real binary now reads
/// its settings from its own database, `scanner::settings`, never a file at
/// all), so there is no reason left to keep a config-file-shaped indirection
/// around purely for these two tests. `#[allow(dead_code)]`: each `tests/*.rs`
/// file compiles as its own separate binary, and the two real callers don't use
/// an identical subset of these constants (e.g. `e2e_dashboard_stagenet.rs`
/// never seals wallet material directly, since its own tenant is provisioned
/// through a real HTTP connect flow instead) - a constant unused in *one*
/// binary but used in the other is expected here, not dead code to prune.
#[allow(dead_code)]
pub mod e2e_fixture {
    // Switched to monerodevs.org (the community-curated stagenet node) after
    // stagenet.xmr-tw.org itself proved unreliable during earlier e2e work -
    // real, reproducible mid-request hangs/dropped connections, confirmed
    // independently against both e2e test files, not a fluke of one.
    pub const NODE_HOST: &str = "node.monerodevs.org";
    pub const NODE_PORT: u16 = 38089;
    pub const NODE_SSL: bool = false;
    pub const NODE_ACCEPT_SELF_SIGNED_CERTS: bool = true;

    pub const WALLET_PRIMARY_ADDRESS: &str =
        "54F1KdjaAtnL6Fb4SbLUM1AMQSjSERjYUgYRtVgwjBirA26RyJCzxc4TbWPW65ZvRC6bifBfrTTv3fyu25BFQuvA2ogNiXg";
    pub const WALLET_PRIVATE_VIEW_KEY: &str = "fcdc7998f003928b3f409b94d54f690d16ca6df3689de4da4803c5a9c792fb0e";
    pub const WALLET_PUBLIC_SPEND_KEY: &str = "3fa2161d4e2cc7722288d33e46a4cc37e92629d7e45939ec67cc42e8f144b335";
    pub const WALLET_NETWORK: &str = "stagenet";
    pub const WALLET_ALLOWED_ORIGIN: &str = "http://127.0.0.1:8190";

    // Real stagenet blocks land roughly every ~2 minutes; requiring several
    // confirmations would make these tests spend most of their time waiting on
    // the chain rather than exercising the scanner's own detection logic. The
    // real test payment is tiny by design, so it's covered entirely by
    // PAYMENT_ZERO_CONF_MAX_XMR below.
    pub const PAYMENT_CONFIRMATIONS_REQUIRED: u64 = 1;
    // XMR-denominated - 0.01 XMR comfortably covers a ~0.000335 XMR test
    // payment with a wide margin.
    pub const PAYMENT_ZERO_CONF_MAX_XMR: &str = "0.01";
    pub const PAYMENT_ORDER_EXPIRY_MINUTES: i64 = 30;
    pub const PAYMENT_REORG_CHECK_DEPTH: u64 = 20;
    pub const PAYMENT_MEMPOOL_POLL_INTERVAL_MS: u64 = 2000;
}
