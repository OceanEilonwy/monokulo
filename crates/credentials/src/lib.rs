//! Secrets monokulo and the engine hand out and check.
//!
//! - [`auth`]: store keys (`pk_`, `sk_`), the engine token and the other
//!   machine-generated tokens, hashed with SHA-256 and compared in constant
//!   time.
//! - [`password`]: account passwords people choose, hashed with Argon2id.
//! - [`webhook_sign`]: how a webhook delivery is signed and verified, and
//!   how a merchant's webhook URL is checked before anything is sent to it.

pub mod auth;
pub mod password;
pub mod webhook_sign;
