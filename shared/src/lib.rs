//! `shared`: logic shared between the engine (`moneropay-core`) and the
//! control-plane (accounts, store connections, dashboard backend).
//!
//! See WBS items 0.2-0.5 in `docs/WOOCOMMERCE_WBS.md` for what lands here
//! (secret-token hashing, HMAC webhook signing, and an argon2
//! password-hashing helper done; the migration runner still to come).

pub mod auth;
pub mod password;
pub mod webhook_sign;

#[cfg(test)]
mod tests {
    #[test]
    fn compiles() {
        assert!(true);
    }
}
