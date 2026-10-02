//! `shared`: logic shared between the engine (`engine`) and the
//! monokulo (accounts, store connections, dashboard backend).
//!
//! See WBS items 0.2-0.5 in `docs/WOOCOMMERCE_WBS.md` for what lands here
//! (secret-token hashing, HMAC webhook signing, an argon2 password-hashing
//! helper, and a generic SQLite migration runner). `key_custody` and `network`
//! are a later, structural addition (WBS 2.1.3) rather than more of the same
//! kind of thing - see `key_custody`'s own module doc comment for why the
//! `KeyCustody` trait and its domain types had to move here specifically to
//! keep `key-custody-service` and `engine` from forming a cyclic Cargo
//! dependency once `main.rs` needed to depend on both.

pub mod agreement;
pub mod announcements;
pub mod auth;
pub mod coinmarketcap;
pub mod exchange_rate;
pub mod haveno;
pub mod http_cache;
pub mod ids;
pub mod key_custody;
pub mod log;
pub mod migrations;
pub mod monero_tx;
pub mod network;
pub mod order_status;
pub mod password;
pub mod rate_limit;
pub mod resources;
pub mod scaling;
pub mod shutdown;
pub mod sqlite;
pub mod supervise;
pub mod time;
pub mod webhook_sign;
pub mod xmr_amount;
