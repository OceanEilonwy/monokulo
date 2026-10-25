//! `shared`: logic shared between the engine (`engine`) and the
//! monokulo (accounts, stores, merchant pages): secret-token hashing, HMAC
//! webhook signing, an argon2 password-hashing helper, a generic SQLite
//! migration runner, and helpers both sides use
//! for Monero data (`network`, `monero_tx`). Key custody has a crate of its own
//! (`key-custody`).

pub mod auth;
pub mod coinmarketcap;
pub mod dns;
pub mod exchange_rate;
pub mod haveno;
pub mod http_cache;
pub mod log;
pub mod migrations;
pub mod monero_tx;
pub mod password;
pub mod rate_limit;
pub mod resources;
pub mod shutdown;
pub mod sqlite;
pub mod sqlite_query;
pub mod supervise;
pub mod time;
#[cfg(any(test, feature = "test-support"))]
pub mod unreachable;
pub mod webhook_sign;
pub mod xmr_amount;
