//! `shared`: logic shared between the engine (`moneropay-core`) and the
//! control-plane (accounts, store connections, dashboard backend).
//!
//! See WBS items 0.2-0.5 in `docs/WOOCOMMERCE_WBS.md` for what lands here
//! (secret-token hashing, HMAC webhook signing, an argon2 password-hashing
//! helper, and a generic SQLite migration runner). `key_custody` and `network`
//! are a later, structural addition (WBS 2.1.3) rather than more of the same
//! kind of thing - see `key_custody`'s own module doc comment for why the
//! `KeyCustody` trait and its domain types had to move here specifically to
//! keep `key-custody-service` and `moneropay-core` from forming a cyclic Cargo
//! dependency once `main.rs` needed to depend on both.

pub mod auth;
pub mod exchange_rate;
pub mod key_custody;
pub mod migrations;
pub mod network;
pub mod password;
pub mod rate_limit;
pub mod supervise;
pub mod webhook_sign;
pub mod xmr_amount;

#[cfg(test)]
mod tests {
    #[test]
    fn compiles() {
        assert!(true);
    }
}
