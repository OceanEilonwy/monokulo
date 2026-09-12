//! `shared`: logic shared between the engine (`moneropay-core`) and the
//! control-plane (accounts, store connections, dashboard backend).
//!
//! See WBS items 0.2-0.5 in `docs/WOOCOMMERCE_WBS.md` for what lands here
//! (secret-token hashing, HMAC webhook signing, an argon2 password-hashing
//! helper, and a generic SQLite migration runner).

pub mod auth;
pub mod migrations;
pub mod password;
pub mod webhook_sign;

#[cfg(test)]
mod tests {
    #[test]
    fn compiles() {
        assert!(true);
    }
}
