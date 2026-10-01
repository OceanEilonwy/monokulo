//! Tenant credential generation and verification. See `docs/DESIGN.md` §10.1.
//!
//! Two credential types: `pk_...` (public, embedded in a merchant's static site JS,
//! never authorizes anything) and `sk_...` (secret, resolves "which tenant" for every
//! `/api/v1/admin/tenant/*` route). The structural rule this module exists to support:
//! tenant identity for admin routes comes *only* from the `sk_` token, never from a
//! path parameter - see `Store::find_tenant_by_secret_token` in `store.rs`, which is
//! the only lookup admin auth is allowed to use.

use std::fmt;

use rand::Rng;
use sha2::{Digest, Sha256};

/// A bearer token as issued (shown once) or as a client presents it. Never
/// stored or logged: its `Debug` is redacted, [`RawToken::expose`] is the
/// one way to read it (to hand it to its owner), and [`RawToken::hash`] the
/// one way to what is stored and looked up.
#[derive(Clone)]
pub struct RawToken(String);

/// Compared in constant time (for equal lengths; a token's length isn't
/// secret), so `==` on two tokens can't leak how much of one matched.
impl PartialEq for RawToken {
    fn eq(&self, other: &Self) -> bool {
        use subtle::ConstantTimeEq;
        bool::from(self.0.as_bytes().ct_eq(other.0.as_bytes()))
    }
}

impl Eq for RawToken {}

impl RawToken {
    /// A token from outside this process: one a request carried, one a
    /// peer's response returned, or one decrypted from storage.
    pub fn presented(value: &str) -> Self {
        RawToken(value.to_string())
    }

    /// The token itself, to give to its owner.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// SHA-256 of the token: what is stored at rest (for example
    /// `tenants.secret_token_hash`, monokulo's `sessions.token`). Not
    /// Argon2 on purpose - see the module docs: a high-entropy, machine-made
    /// token gains nothing from a slow hash.
    pub fn hash(&self) -> TokenHash {
        TokenHash(hex::encode(Sha256::digest(self.0.as_bytes())))
    }
}

/// A token in a peer's JSON response (the engine returns a new tenant's
/// `sk_` once).
impl<'de> serde::Deserialize<'de> for RawToken {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(RawToken)
    }
}

impl fmt::Debug for RawToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RawToken(<redacted>)")
    }
}

/// A token's hash ([`RawToken::hash`]): what the database stores and looks
/// up. Only ever made from a raw token or read back from the database, so a
/// raw token can't be looked up (or stored) where its hash belongs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TokenHash(String);

impl TokenHash {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl rusqlite::ToSql for TokenHash {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

impl rusqlite::types::FromSql for TokenHash {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        String::column_result(value).map(TokenHash)
    }
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

pub fn generate_public_key() -> String {
    format!("pk_{}", random_hex(24))
}

pub fn generate_secret_token() -> RawToken {
    RawToken(format!("sk_{}", random_hex(32)))
}

/// A monokulo session's bearer token (WBS 1.1.2), shown once at login.
/// Same underlying generation primitive as `generate_secret_token` - a
/// high-entropy random hex string - just with a distinct prefix: a
/// session token and a tenant's `sk_` admin secret are different credential
/// types (different subject, different lifetime/revocation model) that
/// happen to share the same "random bearer token" shape, and giving them
/// different prefixes keeps that visible rather than reusing `sk_` for
/// something that isn't a tenant secret.
pub fn generate_session_token() -> RawToken {
    RawToken(format!("sess_{}", random_hex(32)))
}

/// A single-use connect-flow token (WBS 1.4.1,
/// `docs/WOOCOMMERCE_ROADMAP.md` Stage 6) - handed to a (mock, then real)
/// platform plugin via a redirect query param once the wallet-connection
/// confirm form succeeds, then redeemed exactly once via
/// `POST /connect/{platform}/finish`. Same generation primitive as
/// `generate_session_token`/`generate_secret_token` - a high-entropy random
/// hex string - with its own `conn_` prefix: a distinct credential type
/// (short-lived, single-use, never itself an admin credential) from either
/// of those.
pub fn generate_connect_token() -> RawToken {
    RawToken(format!("conn_{}", random_hex(32)))
}

/// The header every request to the engine carries the engine's admin token
/// in. The engine refuses a request without it, whatever the route: only
/// monokulo, which is given the token, may talk to the engine.
pub const ENGINE_TOKEN_HEADER: &str = "x-engine-token";

/// The engine admin token every test engine and test client uses.
#[cfg(any(test, feature = "test-support"))]
pub const TEST_ENGINE_TOKEN: &str = "engine_test_token_0123456789abcdef0123456789abcdef";

/// The shortest engine admin token accepted. `openssl rand -hex 32` gives 64
/// characters.
pub const MIN_ENGINE_TOKEN_LEN: usize = 32;

/// The engine admin token from the environment variable `name` (its value
/// `value`): refused when unset, blank, or shorter than
/// [`MIN_ENGINE_TOKEN_LEN`], with a message saying how to make one. The
/// engine (`SCANNER_ADMIN_TOKEN`) and monokulo
/// (`MONOKULO_SCANNER_ADMIN_TOKEN`) both start only with one.
pub fn engine_token_from_env(name: &str, value: Option<String>) -> Result<RawToken, String> {
    let how = "generate one with `openssl rand -hex 32` and give the same value to the engine \
               (SCANNER_ADMIN_TOKEN) and monokulo (MONOKULO_SCANNER_ADMIN_TOKEN)";
    let value = value.map(|v| v.trim().to_string()).unwrap_or_default();
    if value.is_empty() {
        return Err(format!("{name} is not set: {how}"));
    }
    if value.chars().count() < MIN_ENGINE_TOKEN_LEN {
        return Err(format!(
            "{name} is shorter than {MIN_ENGINE_TOKEN_LEN} characters: {how}"
        ));
    }
    Ok(RawToken(value))
}

/// A single-use account-signup invite token (monokulo's `signup.mode ==
/// "invite_only"`, `http::invites`) - redeemable exactly once
/// (`Db::redeem_invite_and_create_user`). Same generation primitive as
/// every other credential here, with its own `invite_` prefix.
pub fn generate_invite_token() -> RawToken {
    RawToken(format!("invite_{}", random_hex(32)))
}

/// A webhook's HMAC signing secret. Unlike `sk_`, this is stored reversibly (see
/// `webhooks.signing_secret` in the schema) since it's needed on every delivery, not
/// just checked once - "shown once" for this value is an API convention, not a
/// hashing guarantee.
pub fn generate_webhook_secret() -> String {
    format!("whsec_{}", random_hex(32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_public_and_secret_keys_have_expected_prefixes_and_are_unique() {
        let pk1 = generate_public_key();
        let pk2 = generate_public_key();
        let sk1 = generate_secret_token();
        let sk2 = generate_secret_token();
        assert!(pk1.starts_with("pk_"));
        assert!(sk1.expose().starts_with("sk_"));
        assert_ne!(pk1, pk2);
        assert_ne!(sk1, sk2);
    }

    #[test]
    fn generated_session_tokens_have_the_expected_prefix_and_are_unique() {
        let t1 = generate_session_token();
        let t2 = generate_session_token();
        assert!(t1.expose().starts_with("sess_"));
        assert_ne!(t1, t2);
    }

    #[test]
    fn an_engine_token_must_be_set_and_long_enough() {
        let long = "a".repeat(MIN_ENGINE_TOKEN_LEN);
        assert_eq!(
            engine_token_from_env("T", Some(format!("  {long}\n")))
                .unwrap()
                .expose(),
            long,
            "surrounding whitespace (a trailing newline from a file) is not part of it"
        );
        for refused in [None, Some(String::new()), Some("   ".into())] {
            let err = engine_token_from_env("T", refused).unwrap_err();
            assert!(err.starts_with("T is not set"), "{err}");
            assert!(err.contains("openssl rand -hex 32"), "{err}");
        }
        let err =
            engine_token_from_env("T", Some("a".repeat(MIN_ENGINE_TOKEN_LEN - 1))).unwrap_err();
        assert!(err.contains("shorter than 32"), "{err}");
    }

    #[test]
    fn generated_connect_tokens_have_the_expected_prefix_and_are_unique() {
        let t1 = generate_connect_token();
        let t2 = generate_connect_token();
        assert!(t1.expose().starts_with("conn_"));
        assert_ne!(t1, t2);
    }

    #[test]
    fn generated_invite_tokens_have_the_expected_prefix_and_are_unique() {
        let t1 = generate_invite_token();
        let t2 = generate_invite_token();
        assert!(t1.expose().starts_with("invite_"));
        assert_ne!(t1, t2);
    }

    #[test]
    fn hash_is_deterministic_and_sensitive_to_every_bit() {
        let token = generate_secret_token();
        assert_eq!(token.hash(), token.hash());

        // Flip the last character - a valid token with one bit different must hash
        // to something else entirely, and must never be treated as a prefix/partial
        // match by whatever compares against the stored hash.
        let mut flipped = token.expose().to_string();
        let last = flipped.pop().unwrap();
        let replacement = if last == 'a' { 'b' } else { 'a' };
        flipped.push(replacement);
        assert_ne!(token.hash(), RawToken::presented(&flipped).hash());
    }

    #[test]
    fn a_raw_token_never_shows_in_debug_output() {
        let token = generate_session_token();
        assert!(!format!("{token:?}").contains(token.expose()));
    }
}
