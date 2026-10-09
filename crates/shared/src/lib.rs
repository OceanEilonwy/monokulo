//! `shared`: logic shared between the engine (`engine`) and the
//! monokulo (accounts, store connections, dashboard backend).
//!
//! See WBS items 0.2-0.5 in `docs/WOOCOMMERCE_WBS.md` for what lands here
//! (secret-token hashing, HMAC webhook signing, an argon2 password-hashing
//! helper, and a generic SQLite migration runner), and helpers both sides use
//! for Monero data (`network`, `monero_tx`). Key custody has a crate of its own
//! (`key-custody`).

pub mod activity;
pub mod announcements;
pub mod auth;
pub mod coinmarketcap;
pub mod exchange_rate;
pub mod haveno;
pub mod http_cache;
pub mod ids;
pub mod log;
pub mod migrations;
pub mod monero_tx;
pub mod network;
pub mod order_status;
pub mod password;
pub mod proof;
pub mod rate_limit;
pub mod resources;
pub mod scaling;
pub mod shutdown;
pub mod sqlite;
pub mod supervise;
pub mod time;
#[cfg(any(test, feature = "test-support"))]
pub mod unreachable;
pub mod webhook_sign;
pub mod xmr_amount;
