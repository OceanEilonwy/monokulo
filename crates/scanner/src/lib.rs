// Engine code must not panic on anything it can recover from: a panic in a
// loop stops payment detection until the supervisor restarts it. Tests may
// unwrap freely. Each remaining allow names the invariant that makes it safe.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
// Test code is left out of coverage reports (`cargo +nightly llvm-cov`).
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod auth;
pub mod cli;
pub mod daemon;
pub mod daemon_fallback;
pub mod daemon_rpc;
pub mod engine_settings;
pub mod http;
pub mod key_custody;
pub mod local_admin;
pub mod loops;
pub mod network;
pub mod scanner;
pub mod scanner_status;
pub mod settings;
pub mod status;
pub mod store;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod test_log;
pub mod webhook_delivery;
pub mod webhook_sign;
pub mod work;

pub fn now_unix() -> i64 {
    // A clock set before 1970 reads as 0 rather than panicking in every loop.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
