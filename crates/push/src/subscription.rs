//! Where a push goes and the keys it is encrypted to: what a browser's
//! `PushSubscription.toJSON()` holds, and what a UnifiedPush distributor
//! or a relay hands an app.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::{Host, Url};
use zeroize::Zeroize as _;

use crate::base64url;
pub use crate::base64url::KeyError;

/// A push endpoint: the URL a push is `POST`ed to.
///
/// Always `https` (RFC 8030 §8 requires TLS, and the URL is a capability:
/// whoever learns it can push to the device), never an onion address (a
/// sender here doesn't reach Tor), never with a user name or password in
/// it, and at most [`Endpoint::MAX_LEN`] characters.
///
/// `Debug` prints only the origin: the rest of the URL is the capability,
/// so it never reaches a log line by accident. [`Endpoint::as_str`] is
/// there for the one place that needs all of it, storage.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Endpoint(Url);

/// Why an endpoint was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EndpointError {
    #[error("longer than {} characters", Endpoint::MAX_LEN)]
    TooLong,
    #[error("not a URL: {0}")]
    Invalid(#[from] url::ParseError),
    #[error("not https")]
    NotHttps,
    #[error("has no host")]
    NoHost,
    #[error("an onion address; pushes aren't sent over Tor")]
    Onion,
    #[error("has a user name or password in it")]
    Userinfo,
}

impl Endpoint {
    /// The longest endpoint accepted. Push services' URLs are a host and
    /// an opaque token (FCM's run to about 200 characters); this leaves
    /// room for any of them while bounding what a sender stores.
    pub const MAX_LEN: usize = 2048;

    pub fn parse(text: &str) -> Result<Self, EndpointError> {
        if text.len() > Self::MAX_LEN {
            return Err(EndpointError::TooLong);
        }
        let url = Url::parse(text)?;
        if url.scheme() != "https" {
            return Err(EndpointError::NotHttps);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(EndpointError::Userinfo);
        }
        match url.host() {
            None => return Err(EndpointError::NoHost),
            Some(Host::Domain(domain)) if is_onion(domain) => return Err(EndpointError::Onion),
            Some(_) => {}
        }
        Ok(Endpoint(url))
    }

    /// The endpoint's origin: the VAPID `aud` a push to it is signed for.
    pub fn origin(&self) -> Origin {
        Origin(self.0.origin().ascii_serialization())
    }

    pub fn as_url(&self) -> &Url {
        &self.0
    }

    /// The whole URL, capability included. For storing it; log
    /// [`Endpoint::host`] instead.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The host alone (an IPv6 address in brackets), what a log line about
    /// this endpoint may name.
    pub fn host(&self) -> &str {
        // `parse` refused a URL without a host.
        self.0.host_str().unwrap_or_default()
    }
}

/// `example.onion`, `example.onion.` and any subdomain of either, in any
/// case (`Url` lowercases a domain, so this sees it lowercased).
fn is_onion(domain: &str) -> bool {
    let domain = domain.strip_suffix('.').unwrap_or(domain);
    domain == "onion" || domain.ends_with(".onion")
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Endpoint({}/..)", self.origin())
    }
}

impl FromStr for Endpoint {
    type Err = EndpointError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Endpoint::parse(text)
    }
}

impl<'de> Deserialize<'de> for Endpoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Endpoint::parse(&text)
            .map_err(|error| serde::de::Error::custom(format!("endpoint {error}")))
    }
}

/// A web origin, `scheme://host[:port]` (RFC 6454) with the port left out
/// when it is the scheme's default: the audience a VAPID token is signed
/// for (RFC 8292 §2).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Origin(String);

/// Why a URL has no origin a token could be signed for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OriginError {
    #[error("not a URL: {0}")]
    Invalid(#[from] url::ParseError),
    #[error("not an http or https URL with a host")]
    NotHttp,
    #[error("not an origin alone (it has a path, query or trailing slash, or isn't lowercase)")]
    NotOrigin,
}

impl Origin {
    /// The origin of an `http` or `https` URL, such as the address a relay
    /// is reached at: its pushes are signed for that.
    pub fn of(url: &Url) -> Result<Self, OriginError> {
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            return Err(OriginError::NotHttp);
        }
        Ok(Origin(url.origin().ascii_serialization()))
    }

    /// An origin as a token's `aud` claim carries it: exactly the
    /// serialization [`Origin::of`] makes, so two origins compare equal
    /// only when they are the same.
    pub fn parse(text: &str) -> Result<Self, OriginError> {
        let origin = Origin::of(&Url::parse(text)?)?;
        if origin.0 != text {
            return Err(OriginError::NotOrigin);
        }
        Ok(origin)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Origin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Origin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Origin::parse(&text).map_err(|error| serde::de::Error::custom(format!("origin {error}")))
    }
}

/// The device's P-256 public key a push is encrypted to (RFC 8291 §3.1),
/// written as base64url of the 65-byte uncompressed point.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct P256dh(p256::PublicKey);

impl P256dh {
    pub fn from_base64url(text: &str) -> Result<Self, KeyError> {
        base64url::decode_point(text).map(P256dh)
    }

    pub fn to_base64url(&self) -> String {
        base64url::encode_point(&self.0)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn from_key(key: p256::PublicKey) -> Self {
        P256dh(key)
    }

    pub(crate) fn key(&self) -> &p256::PublicKey {
        &self.0
    }
}

impl fmt::Display for P256dh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base64url())
    }
}

impl fmt::Debug for P256dh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "P256dh({self})")
    }
}

impl FromStr for P256dh {
    type Err = KeyError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        P256dh::from_base64url(text)
    }
}

impl<'de> Deserialize<'de> for P256dh {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        P256dh::from_base64url(&text)
            .map_err(|error| serde::de::Error::custom(format!("p256dh {error}")))
    }
}

/// The device's 16-byte authentication secret (RFC 8291 §3.2). It only
/// lets one encrypt to the device, but it is still a key: scrubbed from
/// memory when dropped, and `Debug` doesn't print it.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthSecret([u8; AuthSecret::LEN]);

/// Why an authentication secret was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthSecretError {
    #[error("not base64url")]
    Base64,
    #[error("{0} bytes, not 16")]
    Length(usize),
}

impl AuthSecret {
    pub const LEN: usize = 16;

    pub fn from_base64url(text: &str) -> Result<Self, AuthSecretError> {
        let mut bytes = base64url::decode_key(text).map_err(|_| AuthSecretError::Base64)?;
        let secret = <[u8; Self::LEN]>::try_from(bytes.as_slice())
            .map(AuthSecret)
            .map_err(|_| AuthSecretError::Length(bytes.len()));
        bytes.zeroize();
        secret
    }

    pub fn to_base64url(&self) -> String {
        base64url::encode(&self.0)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        AuthSecret(bytes)
    }

    pub(crate) fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }
}

impl Drop for AuthSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for AuthSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthSecret(..)")
    }
}

impl FromStr for AuthSecret {
    type Err = AuthSecretError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        AuthSecret::from_base64url(text)
    }
}

impl<'de> Deserialize<'de> for AuthSecret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        AuthSecret::from_base64url(&text)
            .map_err(|error| serde::de::Error::custom(format!("auth {error}")))
    }
}

/// The two keys a push is encrypted to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SubscriptionKeys {
    pub p256dh: P256dh,
    pub auth: AuthSecret,
}

/// A push subscription: where to send and what to encrypt to. Reads the
/// shape `PushSubscription.toJSON()` gives, `{"endpoint": "…", "keys":
/// {"p256dh": "…", "auth": "…"}}`; anything else in it (a browser adds
/// `expirationTime`) is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Subscription {
    pub endpoint: Endpoint,
    pub keys: SubscriptionKeys,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8291 Appendix A's receiver.
    const UA_PUBLIC: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

    #[test]
    fn an_https_endpoint_is_accepted_and_names_its_origin_and_host() {
        let endpoint = Endpoint::parse("https://push.example.net:8443/wpush/v2/abc?x=1").unwrap();
        assert_eq!(endpoint.origin().as_str(), "https://push.example.net:8443");
        assert_eq!(endpoint.host(), "push.example.net");
        assert_eq!(
            endpoint.as_str(),
            "https://push.example.net:8443/wpush/v2/abc?x=1"
        );

        let default_port = Endpoint::parse("https://fcm.googleapis.com:443/fcm/send/x").unwrap();
        assert_eq!(default_port.origin().as_str(), "https://fcm.googleapis.com");

        let ipv6 = Endpoint::parse("https://[2001:db8::1]/p").unwrap();
        assert_eq!(ipv6.host(), "[2001:db8::1]");
    }

    #[test]
    fn an_endpoint_that_is_not_https_is_refused() {
        assert_eq!(
            Endpoint::parse("http://push.example.net/x"),
            Err(EndpointError::NotHttps)
        );
        assert_eq!(
            Endpoint::parse("wss://push.example.net/x"),
            Err(EndpointError::NotHttps)
        );
        assert!(matches!(
            Endpoint::parse("push.example.net/x"),
            Err(EndpointError::Invalid(_))
        ));
    }

    #[test]
    fn an_onion_endpoint_is_refused_in_any_spelling() {
        for text in [
            "https://abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvw.onion/x",
            "https://push.abcdefghij.onion/x",
            "https://PUSH.EXAMPLE.ONION/x",
            "https://example.onion./x",
            "https://onion/x",
        ] {
            assert_eq!(Endpoint::parse(text), Err(EndpointError::Onion), "{text}");
        }
        // A name that only contains the word is an ordinary host.
        assert!(Endpoint::parse("https://onion.example.net/x").is_ok());
        assert!(Endpoint::parse("https://example.notonion/x").is_ok());
    }

    #[test]
    fn an_endpoint_with_userinfo_is_refused() {
        assert_eq!(
            Endpoint::parse("https://user@push.example.net/x"),
            Err(EndpointError::Userinfo)
        );
        assert_eq!(
            Endpoint::parse("https://:pw@push.example.net/x"),
            Err(EndpointError::Userinfo)
        );
    }

    #[test]
    fn an_endpoint_longer_than_2048_characters_is_refused() {
        let prefix = "https://push.example.net/";
        let longest = format!("{prefix}{}", "a".repeat(Endpoint::MAX_LEN - prefix.len()));
        assert!(Endpoint::parse(&longest).is_ok());
        let over = format!("{longest}a");
        assert_eq!(Endpoint::parse(&over), Err(EndpointError::TooLong));
    }

    /// The capability part of the URL never reaches `Debug`.
    #[test]
    fn an_endpoint_debug_prints_its_origin_only() {
        let endpoint = Endpoint::parse("https://push.example.net/secret-token").unwrap();
        let printed = format!("{endpoint:?}");
        assert_eq!(printed, "Endpoint(https://push.example.net/..)");
        assert!(!printed.contains("secret-token"));
    }

    #[test]
    fn an_origin_parses_only_in_its_one_spelling() {
        assert_eq!(
            Origin::parse("https://push.example.net").unwrap().as_str(),
            "https://push.example.net"
        );
        assert_eq!(
            Origin::parse("http://127.0.0.1:8444").unwrap().as_str(),
            "http://127.0.0.1:8444"
        );
        for text in [
            "https://push.example.net/",
            "https://push.example.net/path",
            "https://push.example.net:443",
            "https://PUSH.example.net",
        ] {
            assert_eq!(Origin::parse(text), Err(OriginError::NotOrigin), "{text}");
        }
        assert_eq!(
            Origin::parse("mailto:a@example.net"),
            Err(OriginError::NotHttp)
        );
        assert_eq!(
            Origin::of(&Url::parse("https://relay.example.net/v1/push/ph_1").unwrap())
                .unwrap()
                .as_str(),
            "https://relay.example.net"
        );
    }

    #[test]
    fn a_p256dh_round_trips_through_base64url() {
        let key = P256dh::from_base64url(UA_PUBLIC).unwrap();
        assert_eq!(key.to_base64url(), UA_PUBLIC);
        assert_eq!(key.to_string(), UA_PUBLIC);
        // Padding, which some libraries add, reads the same key.
        assert_eq!(
            P256dh::from_base64url(&format!("{UA_PUBLIC}=")).unwrap(),
            key
        );
    }

    #[test]
    fn a_p256dh_that_is_not_an_uncompressed_point_on_the_curve_is_refused() {
        assert_eq!(P256dh::from_base64url("not base64!"), Err(KeyError::Base64));
        // Standard base64's alphabet isn't base64url's.
        assert_eq!(
            P256dh::from_base64url("BCVx+r7N/eNg"),
            Err(KeyError::Base64)
        );
        assert_eq!(
            P256dh::from_base64url(&base64url::encode(&[4u8; 64])),
            Err(KeyError::Length(64))
        );
        assert_eq!(
            P256dh::from_base64url(&base64url::encode(&[4u8; 66])),
            Err(KeyError::Length(66))
        );

        // The same point compressed: a valid SEC 1 encoding, but not the
        // form Web Push uses, and 33 bytes long.
        let key = P256dh::from_base64url(UA_PUBLIC).unwrap();
        use p256::elliptic_curve::sec1::ToSec1Point as _;
        let compressed = key.key().to_sec1_point(true);
        assert_eq!(
            P256dh::from_base64url(&base64url::encode(compressed.as_bytes())),
            Err(KeyError::Length(33))
        );
        // 65 bytes with a compressed point's tag.
        let mut tagged = base64url::point_bytes(key.key());
        tagged[0] = 0x02;
        assert_eq!(
            P256dh::from_base64url(&base64url::encode(&tagged)),
            Err(KeyError::Compressed)
        );

        // y changed by one: still 65 bytes, no longer on the curve.
        let mut off_curve = base64url::point_bytes(key.key());
        off_curve[64] ^= 1;
        assert_eq!(
            P256dh::from_base64url(&base64url::encode(&off_curve)),
            Err(KeyError::NotOnCurve)
        );
    }

    #[test]
    fn an_auth_secret_is_16_bytes_and_never_printed() {
        let auth = AuthSecret::from_base64url(AUTH).unwrap();
        assert_eq!(auth.to_base64url(), AUTH);
        assert_eq!(format!("{auth:?}"), "AuthSecret(..)");
        assert_eq!(
            AuthSecret::from_base64url(&base64url::encode(&[1u8; 15])),
            Err(AuthSecretError::Length(15))
        );
        assert_eq!(
            AuthSecret::from_base64url(&base64url::encode(&[1u8; 17])),
            Err(AuthSecretError::Length(17))
        );
        assert_eq!(
            AuthSecret::from_base64url("***"),
            Err(AuthSecretError::Base64)
        );
    }

    #[test]
    fn a_subscription_reads_the_browser_shape() {
        let json = format!(
            r#"{{"endpoint": "https://updates.push.services.mozilla.com/wpush/v2/gAAA",
                "expirationTime": null,
                "keys": {{"p256dh": "{UA_PUBLIC}", "auth": "{AUTH}"}}}}"#
        );
        let subscription: Subscription = serde_json::from_str(&json).unwrap();
        assert_eq!(
            subscription.endpoint.host(),
            "updates.push.services.mozilla.com"
        );
        assert_eq!(subscription.keys.p256dh.to_base64url(), UA_PUBLIC);
        assert_eq!(subscription.keys.auth.to_base64url(), AUTH);

        let http = json.replace("https://", "http://");
        let error = serde_json::from_str::<Subscription>(&http)
            .unwrap_err()
            .to_string();
        assert!(error.contains("endpoint not https"), "{error}");
    }
}
