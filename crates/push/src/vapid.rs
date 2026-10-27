//! Voluntary Application Server Identification (RFC 8292): a sender signs
//! each push with its own P-256 key, so a push service (or a relay) can
//! tell which sender a subscription belongs to and refuse anyone else.
//!
//! The signature is a short-lived ES256 JWT for the push service's
//! origin, sent with the sender's public key as `Authorization: vapid
//! t=<jwt>, k=<key>`.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;
use std::time::Duration;

use http::HeaderValue;
use p256::ecdsa::VerifyingKey;
use p256::elliptic_curve::Generate as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::Url;
use zeroize::Zeroizing;

use crate::base64url::{self, KeyError};
use crate::jwt::{self, CompactJwt, Es256Key, JwtError};
use crate::subscription::Origin;

/// How far ahead a token this crate signs expires. Half of
/// [`MAX_EXPIRY`], so a token cached for most of its life is still well
/// inside what a push service accepts.
pub const EXPIRY: Duration = Duration::from_secs(12 * 60 * 60);

/// The furthest ahead a token may expire (RFC 8292 §2: "MUST NOT be more
/// than 24 hours from the time of the request").
pub const MAX_EXPIRY: Duration = Duration::from_secs(24 * 60 * 60);

/// A sender's VAPID private key. Scrubbed from memory when dropped (the
/// inner `SecretKey` does it), and `Debug` doesn't print it.
pub struct VapidKey(p256::SecretKey);

/// Why 32 bytes aren't a P-256 private key: zero, or not below the
/// curve's order.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a P-256 private key")]
pub struct VapidKeyError;

impl VapidKey {
    /// A new key from the operating system's random number generator.
    pub fn generate() -> Self {
        VapidKey(p256::SecretKey::generate_from_rng(&mut rand::rng()))
    }

    /// The key [`VapidKey::to_bytes`] wrote: the private scalar,
    /// big-endian.
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, VapidKeyError> {
        p256::SecretKey::from_slice(bytes)
            .map(VapidKey)
            .map_err(|_| VapidKeyError)
    }

    pub fn to_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.to_bytes().into())
    }

    pub fn public(&self) -> VapidPublicKey {
        VapidPublicKey(self.0.public_key())
    }

    /// The `Authorization` for pushes to endpoints of `aud`, valid for
    /// [`EXPIRY`] from `now`. `sub` is how the push service's operator
    /// can reach the sender (RFC 8292 §2.1).
    pub fn authorization(
        &self,
        aud: &Origin,
        sub: Option<&Contact>,
        now: i64,
    ) -> Result<Authorization, VapidError> {
        let claims = Claims {
            aud: aud.clone(),
            exp: now.saturating_add(EXPIRY.as_secs() as i64),
            sub: sub.cloned(),
        };
        let header = jwt::Header {
            typ: Some("JWT"),
            kid: None,
        };
        let token = Es256Key::from_secret(&self.0).sign(&header, &claims)?;
        Ok(Authorization {
            token,
            key: self.public(),
        })
    }
}

impl fmt::Debug for VapidKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VapidKey(..)")
    }
}

/// A sender's VAPID public key, written as base64url of the 65-byte
/// uncompressed point: what an app hands a push service to say whose
/// pushes it takes, and the `k` of an [`Authorization`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VapidPublicKey(p256::PublicKey);

impl VapidPublicKey {
    pub fn from_base64url(text: &str) -> Result<Self, KeyError> {
        base64url::decode_point(text).map(VapidPublicKey)
    }

    pub fn to_base64url(&self) -> String {
        base64url::encode_point(&self.0)
    }

    fn verifying_key(&self) -> VerifyingKey {
        VerifyingKey::from(&self.0)
    }
}

impl Hash for VapidPublicKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        base64url::point_bytes(&self.0).hash(state);
    }
}

impl fmt::Display for VapidPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base64url())
    }
}

impl fmt::Debug for VapidPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VapidPublicKey({self})")
    }
}

impl FromStr for VapidPublicKey {
    type Err = KeyError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        VapidPublicKey::from_base64url(text)
    }
}

impl Serialize for VapidPublicKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_base64url())
    }
}

impl<'de> Deserialize<'de> for VapidPublicKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        VapidPublicKey::from_base64url(&text)
            .map_err(|error| serde::de::Error::custom(format!("VAPID public key {error}")))
    }
}

/// How a push service's operator can reach a sender: the `sub` claim, a
/// `mailto:` or `https:` URL (RFC 8292 §2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact(Url);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContactError {
    #[error("not a URL: {0}")]
    Invalid(#[from] url::ParseError),
    #[error("not a mailto: or https: URL")]
    Scheme,
}

impl Contact {
    pub fn parse(text: &str) -> Result<Self, ContactError> {
        let url = Url::parse(text)?;
        if !matches!(url.scheme(), "mailto" | "https") {
            return Err(ContactError::Scheme);
        }
        Ok(Contact(url))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for Contact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Contact {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Contact {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Contact::parse(&text).map_err(|error| serde::de::Error::custom(format!("sub {error}")))
    }
}

/// A VAPID token's claims (RFC 8292 §2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// The push service's origin the token is for.
    pub aud: Origin,
    /// When the token stops being valid, Unix seconds.
    pub exp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<Contact>,
}

/// An `Authorization: vapid t=<jwt>, k=<key>` header (RFC 8292 §3). One
/// read from a request is unverified until [`verify`] says otherwise.
#[derive(Clone, PartialEq, Eq)]
pub struct Authorization {
    token: CompactJwt,
    key: VapidPublicKey,
}

#[derive(Debug, thiserror::Error)]
pub enum VapidError {
    #[error("not a vapid Authorization header")]
    Header,
    #[error("signed with another key than the subscription's")]
    KeyMismatch,
    #[error(transparent)]
    Jwt(#[from] JwtError),
    #[error("signed for another audience")]
    Audience,
    #[error("expired")]
    Expired,
    #[error("expires more than 24 hours ahead")]
    ExpiryTooFar,
}

impl Authorization {
    /// Reads the header's value. The scheme and parameter names are
    /// matched in any case and the parameters in any order (RFC 9110
    /// §11); a parameter given twice is refused, one not known ignored.
    pub fn parse(value: &str) -> Result<Self, VapidError> {
        let (scheme, params) = value.trim().split_once(' ').ok_or(VapidError::Header)?;
        if !scheme.eq_ignore_ascii_case("vapid") {
            return Err(VapidError::Header);
        }
        let (mut token, mut key) = (None, None);
        for param in params.split(',') {
            let (name, value) = param.split_once('=').ok_or(VapidError::Header)?;
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .unwrap_or(value);
            let slot = match name.trim().to_ascii_lowercase().as_str() {
                "t" => &mut token,
                "k" => &mut key,
                _ => continue,
            };
            if slot.replace(value).is_some() {
                return Err(VapidError::Header);
            }
        }
        let (Some(token), Some(key)) = (token, key) else {
            return Err(VapidError::Header);
        };
        Ok(Authorization {
            token: CompactJwt::parse(token)?,
            key: VapidPublicKey::from_base64url(key).map_err(|_| VapidError::Header)?,
        })
    }

    /// The header's value, as [`Authorization::parse`] reads it.
    pub fn header_value(&self) -> HeaderValue {
        let text = format!("vapid t={}, k={}", self.token.as_str(), self.key);
        // A token and a key are base64url and dots, all visible ASCII.
        HeaderValue::from_str(&text)
            .unwrap_or_else(|_| unreachable!("a VAPID header is visible ASCII"))
    }

    /// The key the sender says it signed with, `k`.
    pub fn key(&self) -> &VapidPublicKey {
        &self.key
    }
}

/// The token is a bearer credential for an hour or more: `Debug` names
/// only the key.
impl fmt::Debug for Authorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Authorization(k={})", self.key)
    }
}

/// Checks that `authorization` was signed by `key` (the one a subscription
/// was made for), for `aud` (the origin it was received at), and is
/// unexpired at `now` without expiring more than [`MAX_EXPIRY`] ahead.
pub fn verify(
    authorization: &Authorization,
    key: &VapidPublicKey,
    aud: &Origin,
    now: i64,
) -> Result<Claims, VapidError> {
    if authorization.key != *key {
        return Err(VapidError::KeyMismatch);
    }
    let claims: Claims = jwt::verify_es256(&authorization.token, &key.verifying_key())?;
    if claims.aud != *aud {
        return Err(VapidError::Audience);
    }
    if claims.exp <= now {
        return Err(VapidError::Expired);
    }
    if claims.exp > now.saturating_add(MAX_EXPIRY.as_secs() as i64) {
        return Err(VapidError::ExpiryTooFar);
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_800_000_000;

    fn origin() -> Origin {
        Origin::parse("https://push.example.net").unwrap()
    }

    /// RFC 8292 §3.1's example request header, unwrapped.
    const RFC8292_HEADER: &str = "vapid t=eyJ0eXAiOiJKV1QiLCJhbGciOiJFUzI1NiJ9.eyJhdWQiOiJodHRwczovL3B1c2guZXhhbXBsZS5uZXQiLCJleHAiOjE0NTM1MjM3NjgsInN1YiI6Im1haWx0bzpwdXNoQGV4YW1wbGUuY29tIn0.i3CYb7t4xfxCDquptFOepC9GAu_HLGkMlMuCGSK2rpiUfnK9ojFwDXb1JrErtmysazNjjvW2L9OkSSHzvoD1oA, k=BA1Hxzyi1RUM1b5wjxsn7nGxAszw2u61m164i3MrAIxHF6YK5h4SDYic-dRuU_RCPCfA5aq9ojSwk5Y2EmClBPs";
    const RFC8292_EXP: i64 = 1_453_523_768;

    #[test]
    fn the_rfc_8292_example_verifies() {
        let authorization = Authorization::parse(RFC8292_HEADER).unwrap();
        let key = *authorization.key();
        let claims = verify(&authorization, &key, &origin(), RFC8292_EXP - 60).unwrap();
        assert_eq!(
            claims,
            Claims {
                aud: origin(),
                exp: RFC8292_EXP,
                sub: Some(Contact::parse("mailto:push@example.com").unwrap()),
            }
        );
        assert!(matches!(
            verify(&authorization, &key, &origin(), RFC8292_EXP),
            Err(VapidError::Expired)
        ));
    }

    #[test]
    fn an_authorization_verifies_against_its_key_and_audience() {
        let key = VapidKey::generate();
        let contact = Contact::parse("https://monokulo.example/").unwrap();
        let authorization = key.authorization(&origin(), Some(&contact), NOW).unwrap();
        let text = authorization.header_value();
        let text = text.to_str().unwrap();
        assert!(text.starts_with("vapid t="), "{text}");
        assert!(text.ends_with(&format!(", k={}", key.public())), "{text}");

        let read = Authorization::parse(text).unwrap();
        assert_eq!(read, authorization);
        let claims = verify(&read, &key.public(), &origin(), NOW).unwrap();
        assert_eq!(claims.aud, origin());
        assert_eq!(claims.exp, NOW + 12 * 60 * 60);
        assert_eq!(claims.sub, Some(contact));

        // Without a contact the claim is left out.
        let bare = key.authorization(&origin(), None, NOW).unwrap();
        assert_eq!(
            verify(&bare, &key.public(), &origin(), NOW).unwrap().sub,
            None
        );
    }

    #[test]
    fn an_authorization_for_another_key_is_refused() {
        let key = VapidKey::generate();
        let other = VapidKey::generate();
        let authorization = key.authorization(&origin(), None, NOW).unwrap();
        assert!(matches!(
            verify(&authorization, &other.public(), &origin(), NOW),
            Err(VapidError::KeyMismatch)
        ));

        // `k` swapped for the subscription's key: the signature gives it
        // away.
        let forged = Authorization {
            token: authorization.token.clone(),
            key: other.public(),
        };
        assert!(matches!(
            verify(&forged, &other.public(), &origin(), NOW),
            Err(VapidError::Jwt(JwtError::Signature))
        ));
    }

    #[test]
    fn an_authorization_for_another_audience_is_refused() {
        let key = VapidKey::generate();
        let authorization = key.authorization(&origin(), None, NOW).unwrap();
        for aud in [
            "https://push.example.org",
            "http://push.example.net",
            "https://push.example.net:8443",
        ] {
            assert!(
                matches!(
                    verify(
                        &authorization,
                        &key.public(),
                        &Origin::parse(aud).unwrap(),
                        NOW
                    ),
                    Err(VapidError::Audience)
                ),
                "{aud}"
            );
        }
    }

    #[test]
    fn an_expired_authorization_is_refused() {
        let key = VapidKey::generate();
        let authorization = key.authorization(&origin(), None, NOW).unwrap();
        let exp = NOW + EXPIRY.as_secs() as i64;
        assert!(verify(&authorization, &key.public(), &origin(), exp - 1).is_ok());
        assert!(matches!(
            verify(&authorization, &key.public(), &origin(), exp),
            Err(VapidError::Expired)
        ));
    }

    /// A token valid for more than a day, from a sender that isn't this
    /// crate: signed by hand with the same key.
    #[test]
    fn an_authorization_expiring_more_than_24_hours_ahead_is_refused() {
        let key = VapidKey::generate();
        let sign = |exp: i64| Authorization {
            token: Es256Key::from_secret(&key.0)
                .sign(
                    &jwt::Header::default(),
                    &json!({"aud": "https://push.example.net", "exp": exp}),
                )
                .unwrap(),
            key: key.public(),
        };
        let day = MAX_EXPIRY.as_secs() as i64;
        assert!(verify(&sign(NOW + day), &key.public(), &origin(), NOW).is_ok());
        assert!(matches!(
            verify(&sign(NOW + day + 1), &key.public(), &origin(), NOW),
            Err(VapidError::ExpiryTooFar)
        ));
        // An audience that isn't an origin doesn't read as claims.
        let path = Authorization {
            token: Es256Key::from_secret(&key.0)
                .sign(
                    &jwt::Header::default(),
                    &json!({"aud": "https://push.example.net/p", "exp": NOW + 60}),
                )
                .unwrap(),
            key: key.public(),
        };
        assert!(matches!(
            verify(&path, &key.public(), &origin(), NOW),
            Err(VapidError::Jwt(JwtError::Claims(_)))
        ));
    }

    #[test]
    fn an_authorization_header_reads_in_any_case_and_order() {
        let key = VapidKey::generate();
        let authorization = key.authorization(&origin(), None, NOW).unwrap();
        let (t, k) = (authorization.token.as_str(), key.public().to_base64url());
        for text in [
            format!("vapid t={t}, k={k}"),
            format!("VAPID k={k},t={t}"),
            format!("  Vapid T=\"{t}\" ,  K={k}, x=1 "),
        ] {
            assert_eq!(
                Authorization::parse(&text).unwrap(),
                authorization,
                "{text}"
            );
        }
        for text in [
            format!("Bearer t={t}, k={k}"),
            format!("vapid t={t}"),
            format!("vapid k={k}"),
            format!("vapid t={t}, t={t}, k={k}"),
            format!("vapid t={t}, k"),
            format!("vapid t={t}, k=AAAA"),
            format!("vapid t=abc, k={k}"),
            "vapid".to_owned(),
        ] {
            assert!(Authorization::parse(&text).is_err(), "{text}");
        }
    }

    #[test]
    fn a_vapid_key_round_trips_through_its_bytes() {
        let key = VapidKey::generate();
        let again = VapidKey::from_bytes(&key.to_bytes()).unwrap();
        assert_eq!(again.public(), key.public());
        assert_eq!(VapidKey::from_bytes(&[0; 32]).unwrap_err(), VapidKeyError);
        assert_eq!(
            VapidKey::from_bytes(&[0xff; 32]).unwrap_err(),
            VapidKeyError
        );
        assert_eq!(format!("{key:?}"), "VapidKey(..)");
    }

    #[test]
    fn a_vapid_public_key_reads_and_writes_base64url() {
        let key = VapidKey::generate().public();
        let text = key.to_string();
        assert_eq!(text.len(), 87);
        assert_eq!(text.parse::<VapidPublicKey>().unwrap(), key);
        assert_eq!(serde_json::to_value(key).unwrap(), json!(text));
        assert_eq!(
            serde_json::from_value::<VapidPublicKey>(json!(text)).unwrap(),
            key
        );
        assert!(serde_json::from_value::<VapidPublicKey>(json!("BAAA")).is_err());
    }

    #[test]
    fn a_contact_is_a_mailto_or_https_url() {
        assert!(Contact::parse("mailto:ops@example.net").is_ok());
        assert!(Contact::parse("https://example.net/contact").is_ok());
        assert_eq!(
            Contact::parse("http://example.net"),
            Err(ContactError::Scheme)
        );
        assert!(matches!(
            Contact::parse("ops@example.net"),
            Err(ContactError::Invalid(_))
        ));
    }
}
