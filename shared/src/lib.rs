//! `shared`: logic shared between the engine (`moneropay-core`) and the
//! control-plane (accounts, store connections, dashboard backend).
//!
//! See WBS items 0.2-0.5 in `docs/WOOCOMMERCE_WBS.md` for what lands here
//! (secret-token hashing done; HMAC webhook signing, the migration runner,
//! and a new argon2 password-hashing helper still to come).

pub mod auth;

#[cfg(test)]
mod tests {
    #[test]
    fn compiles() {
        assert!(true);
    }
}
