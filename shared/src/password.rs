//! Human-chosen account password hashing for the control plane.
//!
//! This is deliberately a *different* algorithm choice from
//! `shared::auth::hash_secret_token`: that module hashes a machine-generated,
//! high-entropy `sk_...` token with plain SHA-256, because a slow/memory-hard
//! hash buys no brute-force resistance for a token an attacker can't
//! meaningfully guess - it only adds cost. Human-chosen passwords are the
//! opposite case: low-entropy and guessable, so they genuinely need a
//! slow, memory-hard hash. Argon2id (via the `argon2` crate's own
//! recommended API) is that hash. Do not reuse this module for tokens, and
//! do not reuse `shared::auth` for passwords.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::password_hash::rand_core::OsRng;
use argon2::Argon2;

/// Hashes `password` with Argon2id, using the crate's current
/// recommended default parameters and a freshly random salt.
///
/// Returns a self-describing PHC-format string (algorithm, parameters,
/// salt, and hash all encoded together), so `verify_password` needs no
/// separate storage for salt or parameters.
///
/// # Errors
///
/// Returns an error only if the underlying hashing operation itself
/// fails, which the `argon2` crate's API models as fallible but which
/// should not happen in practice for valid input.
pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default().hash_password(password.as_bytes(), &salt)?;
    Ok(hash.to_string())
}

/// Verifies `password` against a previously stored PHC-format hash
/// (as produced by [`hash_password`]).
///
/// Returns `false` both when the password is wrong and when `hashed` is
/// not a valid PHC-format hash string at all - callers should not be able
/// to distinguish "wrong password" from "malformed hash" from the return
/// value alone, which rules out a boolean-shaped side channel.
pub fn verify_password(password: &str, hashed: &str) -> bool {
    let Ok(parsed_hash) = PasswordHash::new(hashed) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hashed_password_verifies_against_the_original() {
        let hashed = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &hashed));
    }

    #[test]
    fn a_hashed_password_rejects_a_wrong_password() {
        let hashed = hash_password("correct horse battery staple").unwrap();
        assert!(!verify_password("wrong password", &hashed));
    }

    #[test]
    fn hashing_the_same_password_twice_produces_different_hashes_but_both_verify() {
        let password = "correct horse battery staple";
        let hash1 = hash_password(password).unwrap();
        let hash2 = hash_password(password).unwrap();

        // Per-call random salt, not fixed/reused.
        assert_ne!(hash1, hash2);

        assert!(verify_password(password, &hash1));
        assert!(verify_password(password, &hash2));
    }

    #[test]
    fn verifying_against_a_malformed_hash_string_returns_false_rather_than_panicking() {
        assert!(!verify_password("whatever", "this is not a PHC-format hash"));
        assert!(!verify_password("whatever", ""));
    }
}
