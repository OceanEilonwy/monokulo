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

/// The key secrets are encrypted with at rest. Its bytes are only read by
/// [`encrypt`] and [`decrypt`], and `Debug` doesn't print them.
#[derive(Clone)]
pub struct AtRestKey([u8; 32]);

/// Scrubbed from memory when dropped.
impl Drop for AtRestKey {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

impl AtRestKey {
    pub const fn new(bytes: [u8; 32]) -> Self {
        AtRestKey(bytes)
    }
}

impl std::fmt::Debug for AtRestKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AtRestKey(..)")
    }
}

/// What a ciphertext is bound to: the row it belongs in. Authenticated
/// with the ciphertext (GCM's associated data) but not stored in it, so a
/// value copied from one row into another - a store's engine secret into
/// another store's row, an invite token into another request's - fails to
/// decrypt there. Anyone able to write the database file could otherwise
/// make one store act with another's secret without knowing the key.
pub enum Binding<'a> {
    /// `store_connections.tenant_secret_token_encrypted` of this connection.
    StoreSecret(&'a str),
    /// `invite_links.token_encrypted` of the request it was made for.
    InviteToken(&'a str),
}

impl Binding<'_> {
    fn bytes(&self) -> Vec<u8> {
        match self {
            Binding::StoreSecret(id) => format!("store_connections.tenant_secret_token:{id}"),
            Binding::InviteToken(id) => format!("invite_links.token:{id}"),
        }
        .into_bytes()
    }
}

/// Encrypts `plaintext` under `key`, bound to `binding`, returning a single
/// hex-encoded string (nonce || ciphertext-with-tag) safe to store in a
/// plain `TEXT` column.
///
/// A fresh random nonce is generated on every call — encrypting the same
/// plaintext twice yields two different encoded strings (see this module's
/// own test), which is required for GCM's security (nonce reuse under the
/// same key breaks the authentication guarantee).
pub fn encrypt(key: &AtRestKey, binding: Binding<'_>, plaintext: &str) -> String {
    use aes_gcm::aead::Payload;
    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(key.0));
    // A fresh, cryptographically random nonce every call - see this
    // module's doc comment on why that matters for GCM.
    let nonce = CipherNonce::generate();
    // Only fails for absurdly large plaintexts (far beyond GCM's ~64GiB
    // limit) - never for anything this module is actually used for (a
    // ~70-byte `sk_...` token).
    let aad = binding.bytes();
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext.as_bytes(),
                aad: &aad,
            },
        )
        .expect("AES-256-GCM encryption failed");

    let mut combined = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    combined.extend_from_slice(nonce.as_ref());
    combined.extend_from_slice(&ciphertext);
    hex::encode(combined)
}

/// Inverse of [`encrypt`]. Fails (never panics) on a malformed encoded
/// string, a truncated nonce/ciphertext, or an authentication-tag mismatch
/// (tampered or corrupted data, the wrong key, or a value from another
/// row's binding).
pub fn decrypt(
    key: &AtRestKey,
    binding: Binding<'_>,
    encoded: &str,
) -> Result<String, CryptoError> {
    use aes_gcm::aead::Payload;
    let combined = hex::decode(encoded).map_err(|_| CryptoError::InvalidEncoding)?;
    if combined.len() < NONCE_LEN {
        return Err(CryptoError::Truncated);
    }
    let (nonce_bytes, ciphertext) = combined.split_at(NONCE_LEN);
    // Already length-checked above, so this can't fail in practice - but
    // handled rather than unwrapped, since nothing about `TryFrom` proves it
    // statically.
    let nonce = CipherNonce::try_from(nonce_bytes).map_err(|_| CryptoError::Truncated)?;

    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(key.0));
    let aad = binding.bytes();
    let plaintext_bytes = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| CryptoError::AuthenticationFailed)?;
    String::from_utf8(plaintext_bytes).map_err(|_| CryptoError::InvalidUtf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: AtRestKey = AtRestKey::new([7u8; 32]);

    /// A value is bound to its row: moved to another row (or another kind
    /// of row) it no longer decrypts.
    #[test]
    fn a_value_bound_to_one_row_does_not_decrypt_in_another() {
        let encoded = encrypt(&TEST_KEY, Binding::StoreSecret("c1"), "sk_one");
        assert_eq!(
            decrypt(&TEST_KEY, Binding::StoreSecret("c1"), &encoded).unwrap(),
            "sk_one"
        );
        assert!(matches!(
            decrypt(&TEST_KEY, Binding::StoreSecret("c2"), &encoded),
            Err(CryptoError::AuthenticationFailed)
        ));
        assert!(matches!(
            decrypt(&TEST_KEY, Binding::InviteToken("c1"), &encoded),
            Err(CryptoError::AuthenticationFailed)
        ));
    }

    #[test]
    fn encrypt_then_decrypt_round_trips_to_the_exact_original_plaintext() {
        let plaintext = "sk_abcdefghijklmnopqrstuvwxyz0123456789";
        let encoded = encrypt(&TEST_KEY, Binding::StoreSecret("c1"), plaintext);
        let decrypted = decrypt(&TEST_KEY, Binding::StoreSecret("c1"), &encoded).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypting_the_same_plaintext_twice_produces_two_different_encoded_strings() {
        let plaintext = "sk_same_token_every_time";
        let first = encrypt(&TEST_KEY, Binding::StoreSecret("c1"), plaintext);
        let second = encrypt(&TEST_KEY, Binding::StoreSecret("c1"), plaintext);
        assert_ne!(
            first, second,
            "fresh nonce per call should make the two encodings differ"
        );

        // Both must still independently decrypt back to the same plaintext.
        assert_eq!(
            decrypt(&TEST_KEY, Binding::StoreSecret("c1"), &first).unwrap(),
            plaintext
        );
        assert_eq!(
            decrypt(&TEST_KEY, Binding::StoreSecret("c1"), &second).unwrap(),
            plaintext
        );
    }

    #[test]
    fn decrypting_a_tampered_ciphertext_returns_an_error_not_a_panic_or_wrong_plaintext() {
        let plaintext = "sk_do_not_trust_a_tampered_value";
        let encoded = encrypt(&TEST_KEY, Binding::StoreSecret("c1"), plaintext);

        let mut bytes = hex::decode(&encoded).unwrap();
        // Flip a byte well past the nonce, inside the ciphertext/tag.
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        let tampered = hex::encode(bytes);

        let result = decrypt(&TEST_KEY, Binding::StoreSecret("c1"), &tampered);
        assert!(
            matches!(result, Err(CryptoError::AuthenticationFailed)),
            "expected an auth failure, got: {result:?}"
        );
    }

    #[test]
    fn decrypting_a_truncated_string_returns_an_error_not_a_panic() {
        let plaintext = "sk_truncate_me";
        let encoded = encrypt(&TEST_KEY, Binding::StoreSecret("c1"), plaintext);
        // Cut it down to fewer bytes than even the nonce alone.
        let truncated = &encoded[..NONCE_LEN]; // hex chars, well short of a full nonce's worth of bytes
        let result = decrypt(&TEST_KEY, Binding::StoreSecret("c1"), truncated);
        assert!(
            matches!(result, Err(CryptoError::Truncated)),
            "expected Truncated, got: {result:?}"
        );
    }

    #[test]
    fn decrypting_a_non_hex_string_returns_an_error_not_a_panic() {
        let result = decrypt(
            &TEST_KEY,
            Binding::StoreSecret("c1"),
            "not valid hex at all!!",
        );
        assert!(
            matches!(result, Err(CryptoError::InvalidEncoding)),
            "expected InvalidEncoding, got: {result:?}"
        );
    }

    #[test]
    fn decrypting_with_the_wrong_key_returns_an_error() {
        let plaintext = "sk_wrong_key_test";
        let encoded = encrypt(&TEST_KEY, Binding::StoreSecret("c1"), plaintext);
        let wrong_key = AtRestKey::new([9u8; 32]);
        let result = decrypt(&wrong_key, Binding::StoreSecret("c1"), &encoded);
        assert!(
            matches!(result, Err(CryptoError::AuthenticationFailed)),
            "expected an auth failure, got: {result:?}"
        );
    }
}
