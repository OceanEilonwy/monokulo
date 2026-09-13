//! Re-exports the real-Monero-transaction spend wallet from the main crate - see
//! `moneropay_core::e2e_wallet` for the actual implementation and its doc
//! comment. Kept as a thin `tests/support` module (rather than importing
//! `moneropay_core::e2e_wallet` directly in `tests/e2e_stagenet.rs`) purely so
//! that test file's `use support::StagenetSpendWallet;` doesn't need to change.
pub use moneropay_core::e2e_wallet::*;
