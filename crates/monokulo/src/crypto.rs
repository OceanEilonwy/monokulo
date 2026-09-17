//! At-rest encryption for the engine's `sk_...` secret token (WBS 1.2.3).
//!
//! `store_connections.tenant_secret_token_encrypted` used to hold the
//! engine's raw `sk_...` value in plaintext (see WBS 1.2.2's own doc
//! comments on that column and on `Db::create_store_connection` — both
//! explicitly flagged this as temporary). This module is the real
//! encryption those doc comments pointed at.
//!
//! AES-256-GCM (via the `aes-gcm` crate — RustCrypto, the same ecosystem
//! family as `hmac`/`sha2` already used elsewhere in this workspace) rather
//! than anything hand-rolled. GCM is authenticated encryption: tampering
//! with the ciphertext (or the nonce) is detected on decrypt, not silently
//! accepted — see this module's own tests for a corrupted-input case that
//! actually exercises that path.
//!
//! Deliberately pure functions, key-as-parameter: no environment variable
//! is read in this module. Key *sourcing* (env var, test constant,
//! whatever) is entirely the caller's job — see `main.rs` for the real
//! source and the HTTP handler layer (`http/connections.rs`) for where
//! `encrypt`/`decrypt` actually get called. `Db` itself stays crypto-unaware
//! and just stores/returns whatever string it's given.

use aes_gcm::aead::{Aead, Generate, Key, KeyInit};
use aes_gcm::Aes256Gcm;

/// A concrete nonce type for `Aes256Gcm` (`aead::Nonce<A>` = `Array<u8,
/// A::NonceSize>`) - GCM's standard nonce size, 96 bits / 12 bytes.
type CipherNonce = aes_gcm::aead::Nonce<Aes256Gcm>;
const NONCE_LEN: usize = 12;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// The encoded string wasn't valid hex.
    #[error("malformed ciphertext: not valid hex")]
    InvalidEncoding,
    /// The decoded bytes were too short to even contain a nonce.
    #[error("malformed ciphertext: truncated")]
    Truncated,
    /// Decryption failed — either the data was tampered with/corrupted, or
    /// the wrong key was used. AES-GCM can't (and shouldn't) distinguish
    /// these; both mean "do not trust this plaintext."
    #[error("decryption failed: authentication tag mismatch")]
    AuthenticationFailed,
    /// The decrypted bytes weren't valid UTF-8. Can't happen for data this
    /// module itself produced, but a corrupted/foreign input could decrypt
    /// (extremely unlikely, but the tag check happens before this) to
    /// non-UTF-8 bytes.
    #[error("decrypted data was not valid UTF-8")]
    InvalidUtf8,
}

/// Encrypts `plaintext` under `key`, returning a single hex-encoded string
/// (nonce || ciphertext-with-tag) safe to store in a plain `TEXT` column.
///
/// A fresh random nonce is generated on every call — encrypting the same
/// plaintext twice yields two different encoded strings (see this module's
/// own test), which is required for GCM's security (nonce reuse under the
/// same key breaks the authentication guarantee).
pub fn encrypt(key: &[u8; 32], plaintext: &str) -> String {
    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(*key));
    // A fresh, cryptographically random nonce every call - see this
    // module's doc comment on why that matters for GCM.
    let nonce = CipherNonce::generate();
    // Only fails for absurdly large plaintexts (far beyond GCM's ~64GiB
    // limit) - never for anything this module is actually used for (a
    // ~70-byte `sk_...` token).
    let ciphertext = cipher.encrypt(&nonce, plaintext.as_bytes()).expect("AES-256-GCM encryption failed");

    let mut combined = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    combined.extend_from_slice(nonce.as_ref());
    combined.extend_from_slice(&ciphertext);
    hex::encode(combined)
}

/// Inverse of [`encrypt`]. Fails (never panics) on a malformed encoded
/// string, a truncated nonce/ciphertext, or an authentication-tag mismatch
/// (tampered or corrupted data, or the wrong key).
pub fn decrypt(key: &[u8; 32], encoded: &str) -> Result<String, CryptoError> {
    let combined = hex::decode(encoded).map_err(|_| CryptoError::InvalidEncoding)?;
    if combined.len() < NONCE_LEN {
        return Err(CryptoError::Truncated);
    }
    let (nonce_bytes, ciphertext) = combined.split_at(NONCE_LEN);
    // Already length-checked above, so this can't fail in practice - but
    // handled rather than unwrapped, since nothing about `TryFrom` proves it
    // statically.
    let nonce = CipherNonce::try_from(nonce_bytes).map_err(|_| CryptoError::Truncated)?;

    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(*key));
    let plaintext_bytes = cipher.decrypt(&nonce, ciphertext).map_err(|_| CryptoError::AuthenticationFailed)?;
    String::from_utf8(plaintext_bytes).map_err(|_| CryptoError::InvalidUtf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: [u8; 32] = [7u8; 32];

    #[test]
    fn encrypt_then_decrypt_round_trips_to_the_exact_original_plaintext() {
        let plaintext = "sk_abcdefghijklmnopqrstuvwxyz0123456789";
        let encoded = encrypt(&TEST_KEY, plaintext);
        let decrypted = decrypt(&TEST_KEY, &encoded).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypting_the_same_plaintext_twice_produces_two_different_encoded_strings() {
        let plaintext = "sk_same_token_every_time";
        let first = encrypt(&TEST_KEY, plaintext);
        let second = encrypt(&TEST_KEY, plaintext);
        assert_ne!(first, second, "fresh nonce per call should make the two encodings differ");

        // Both must still independently decrypt back to the same plaintext.
        assert_eq!(decrypt(&TEST_KEY, &first).unwrap(), plaintext);
        assert_eq!(decrypt(&TEST_KEY, &second).unwrap(), plaintext);
    }

    #[test]
    fn decrypting_a_tampered_ciphertext_returns_an_error_not_a_panic_or_wrong_plaintext() {
        let plaintext = "sk_do_not_trust_a_tampered_value";
        let encoded = encrypt(&TEST_KEY, plaintext);

        let mut bytes = hex::decode(&encoded).unwrap();
        // Flip a byte well past the nonce, inside the ciphertext/tag.
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        let tampered = hex::encode(bytes);

        let result = decrypt(&TEST_KEY, &tampered);
        assert!(matches!(result, Err(CryptoError::AuthenticationFailed)), "expected an auth failure, got: {result:?}");
    }

    #[test]
    fn decrypting_a_truncated_string_returns_an_error_not_a_panic() {
        let plaintext = "sk_truncate_me";
        let encoded = encrypt(&TEST_KEY, plaintext);
        // Cut it down to fewer bytes than even the nonce alone.
        let truncated = &encoded[..NONCE_LEN]; // hex chars, well short of a full nonce's worth of bytes
        let result = decrypt(&TEST_KEY, truncated);
        assert!(matches!(result, Err(CryptoError::Truncated)), "expected Truncated, got: {result:?}");
    }

    #[test]
    fn decrypting_a_non_hex_string_returns_an_error_not_a_panic() {
        let result = decrypt(&TEST_KEY, "not valid hex at all!!");
        assert!(matches!(result, Err(CryptoError::InvalidEncoding)), "expected InvalidEncoding, got: {result:?}");
    }

    #[test]
    fn decrypting_with_the_wrong_key_returns_an_error() {
        let plaintext = "sk_wrong_key_test";
        let encoded = encrypt(&TEST_KEY, plaintext);
        let wrong_key = [9u8; 32];
        let result = decrypt(&wrong_key, &encoded);
        assert!(matches!(result, Err(CryptoError::AuthenticationFailed)), "expected an auth failure, got: {result:?}");
    }
}
