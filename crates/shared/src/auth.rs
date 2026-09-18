//! Tenant credential generation and verification. See `docs/DESIGN.md` §10.1.
//!
//! Two credential types: `pk_...` (public, embedded in a merchant's static site JS,
//! never authorizes anything) and `sk_...` (secret, resolves "which tenant" for every
//! `/api/v1/admin/tenant/*` route). The structural rule this module exists to support:
//! tenant identity for admin routes comes *only* from the `sk_` token, never from a
//! path parameter - see `Store::find_tenant_by_secret_token` in `store.rs`, which is
//! the only lookup admin auth is allowed to use.

use rand::Rng;
use sha2::{Digest, Sha256};

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

pub fn generate_public_key() -> String {
    format!("pk_{}", random_hex(24))
}

pub fn generate_secret_token() -> String {
    format!("sk_{}", random_hex(32))
}

/// A monokulo session's bearer token (WBS 1.1.2), shown once at login.
/// Same underlying generation primitive as `generate_secret_token` - a
/// high-entropy random hex string - just with a distinct prefix: a
/// session token and a tenant's `sk_` admin secret are different credential
/// types (different subject, different lifetime/revocation model) that
/// happen to share the same "random bearer token" shape, and giving them
/// different prefixes keeps that visible rather than reusing `sk_` for
/// something that isn't a tenant secret.
pub fn generate_session_token() -> String {
    format!("sess_{}", random_hex(32))
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
pub fn generate_connect_token() -> String {
    format!("conn_{}", random_hex(32))
}

/// An instance-wide scanner admin token - authenticates the settings
/// HTTP API (`scanner::http::instance_admin`), distinct from any tenant's own
/// `sk_` (that authenticates one tenant's own admin API, never server-level
/// settings) and from monokulo's own session tokens. Same generation
/// primitive as every other credential here, with its own `admin_` prefix so
/// the two credential types stay visibly distinct rather than sharing `sk_`
/// for something that isn't a tenant secret.
pub fn generate_admin_token() -> String {
    format!("admin_{}", random_hex(32))
}

/// A webhook's HMAC signing secret. Unlike `sk_`, this is stored reversibly (see
/// `webhooks.signing_secret` in the schema) since it's needed on every delivery, not
/// just checked once - "shown once" for this value is an API convention, not a
/// hashing guarantee.
pub fn generate_webhook_secret() -> String {
    format!("whsec_{}", random_hex(32))
}

/// SHA-256 hex digest of a raw bearer token, for storage at rest (e.g.
/// `tenants.secret_token_hash`, or the control plane's `sessions.token` -
/// see WBS 1.1.2). Not Argon2/bcrypt/scrypt on purpose - see module docs.
/// The name predates the control plane's session tokens, but the hashing
/// logic itself is generic: it doesn't care what kind of high-entropy,
/// machine-generated token it's given, so a second near-identical function
/// for session tokens would be pure duplication.
pub fn hash_secret_token(raw_token: &str) -> String {
    hex::encode(Sha256::digest(raw_token.as_bytes()))
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
        assert!(sk1.starts_with("sk_"));
        assert_ne!(pk1, pk2);
        assert_ne!(sk1, sk2);
    }

    #[test]
    fn generated_session_tokens_have_the_expected_prefix_and_are_unique() {
        let t1 = generate_session_token();
        let t2 = generate_session_token();
        assert!(t1.starts_with("sess_"));
        assert_ne!(t1, t2);
    }

    #[test]
    fn generated_admin_tokens_have_the_expected_prefix_and_are_unique() {
        let t1 = generate_admin_token();
        let t2 = generate_admin_token();
        assert!(t1.starts_with("admin_"));
        assert_ne!(t1, t2);
    }

    #[test]
    fn generated_connect_tokens_have_the_expected_prefix_and_are_unique() {
        let t1 = generate_connect_token();
        let t2 = generate_connect_token();
        assert!(t1.starts_with("conn_"));
        assert_ne!(t1, t2);
    }

    #[test]
    fn hash_is_deterministic_and_sensitive_to_every_bit() {
        let token = generate_secret_token();
        assert_eq!(hash_secret_token(&token), hash_secret_token(&token));

        // Flip the last character - a valid token with one bit different must hash
        // to something else entirely, and must never be treated as a prefix/partial
        // match by whatever compares against the stored hash.
        let mut flipped = token.clone();
        let last = flipped.pop().unwrap();
        let replacement = if last == 'a' { 'b' } else { 'a' };
        flipped.push(replacement);
        assert_ne!(hash_secret_token(&token), hash_secret_token(&flipped));
    }
}
