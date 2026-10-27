//! Compact JSON Web Tokens signed with ES256 (RFC 7515, RFC 7518 §3.4):
//! the one kind both VAPID (RFC 8292) and APNs token authentication use.
//!
//! The algorithm is fixed, not read from the token: a header naming any
//! other (`none` among them) is refused before the signature is looked at,
//! and signing always writes `ES256`.

use std::fmt;

use p256::ecdsa::signature::{Signer as _, Verifier as _};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::base64url;

const ALGORITHM: &str = "ES256";

/// A P-256 key that signs ES256 tokens. `Debug` doesn't print it.
pub struct Es256Key(SigningKey);

/// What a token's header says besides its algorithm, which is always
/// ES256.
#[derive(Debug, Clone, Copy, Default)]
pub struct Header<'a> {
    /// `typ`: VAPID tokens say `JWT` (RFC 8292 §2).
    pub typ: Option<&'a str>,
    /// `kid`: an APNs token names its key.
    pub kid: Option<&'a str>,
}

#[derive(Serialize)]
struct WrittenHeader<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    typ: Option<&'a str>,
    alg: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    kid: Option<&'a str>,
}

/// The header fields a verifier reads.
#[derive(Deserialize)]
struct ReadHeader {
    alg: String,
    /// Extensions a verifier must understand (RFC 7515 §4.1.11); none are.
    crit: Option<serde_json::Value>,
}

/// A signed token, `header.claims.signature`. A bearer credential:
/// `Debug` doesn't print it.
#[derive(Clone, PartialEq, Eq)]
pub struct CompactJwt(String);

impl CompactJwt {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A token as received, split for verification: three base64url
    /// segments (the third a 64-byte signature), unverified.
    pub(crate) fn parse(text: &str) -> Result<Self, JwtError> {
        split(text)?;
        Ok(CompactJwt(text.to_owned()))
    }
}

impl fmt::Debug for CompactJwt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CompactJwt(..)")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    #[error("not a compact JWT")]
    Malformed,
    #[error("algorithm {0:?}, not ES256")]
    Algorithm(String),
    #[error("has critical header extensions")]
    Critical,
    #[error("signature doesn't verify")]
    Signature,
    #[error("claims don't read: {0}")]
    Claims(serde_json::Error),
    #[error("can't be written as JSON: {0}")]
    Serialize(serde_json::Error),
}

impl Es256Key {
    pub(crate) fn from_secret(secret: &p256::SecretKey) -> Self {
        Es256Key(SigningKey::from(secret))
    }

    pub fn sign(
        &self,
        header: &Header<'_>,
        claims: &impl Serialize,
    ) -> Result<CompactJwt, JwtError> {
        let header = WrittenHeader {
            typ: header.typ,
            alg: ALGORITHM,
            kid: header.kid,
        };
        let header = serde_json::to_vec(&header).map_err(JwtError::Serialize)?;
        let claims = serde_json::to_vec(claims).map_err(JwtError::Serialize)?;
        let input = format!(
            "{}.{}",
            base64url::encode(&header),
            base64url::encode(&claims)
        );
        let signature: Signature = self.0.sign(input.as_bytes());
        Ok(CompactJwt(format!(
            "{input}.{}",
            base64url::encode(&signature.to_bytes())
        )))
    }
}

impl fmt::Debug for Es256Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Es256Key(..)")
    }
}

/// A token with three well-formed segments, decoded.
struct Segments<'a> {
    /// What the signature is over: `header.claims` as written.
    input: &'a str,
    header: Vec<u8>,
    claims: Vec<u8>,
    signature: Signature,
}

fn split(token: &str) -> Result<Segments<'_>, JwtError> {
    let mut segments = token.split('.');
    let (Some(header), Some(claims), Some(signature), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return Err(JwtError::Malformed);
    };
    let input = &token[..header.len() + 1 + claims.len()];
    let header = base64url::decode_segment(header).map_err(|_| JwtError::Malformed)?;
    let claims = base64url::decode_segment(claims).map_err(|_| JwtError::Malformed)?;
    let signature = base64url::decode_segment(signature).map_err(|_| JwtError::Malformed)?;
    let signature = Signature::from_slice(&signature).map_err(|_| JwtError::Malformed)?;
    Ok(Segments {
        input,
        header,
        claims,
        signature,
    })
}

/// The claims of `token` once its ES256 signature verifies under `key`.
/// Only the signature and algorithm are checked here; what the claims
/// must say is the caller's to check.
pub fn verify_es256<C: DeserializeOwned>(
    token: &CompactJwt,
    key: &VerifyingKey,
) -> Result<C, JwtError> {
    let segments = split(token.as_str())?;
    let header: ReadHeader =
        serde_json::from_slice(&segments.header).map_err(|_| JwtError::Malformed)?;
    if header.alg != ALGORITHM {
        return Err(JwtError::Algorithm(header.alg));
    }
    if header.crit.is_some() {
        return Err(JwtError::Critical);
    }
    key.verify(segments.input.as_bytes(), &segments.signature)
        .map_err(|_| JwtError::Signature)?;
    serde_json::from_slice(&segments.claims).map_err(JwtError::Claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::elliptic_curve::Generate as _;
    use serde_json::{json, Value};

    fn key() -> (Es256Key, VerifyingKey) {
        let secret = p256::SecretKey::generate_from_rng(&mut rand::rng());
        let verifying = VerifyingKey::from(&SigningKey::from(&secret));
        (Es256Key::from_secret(&secret), verifying)
    }

    fn segment(value: &Value) -> String {
        base64url::encode(&serde_json::to_vec(value).unwrap())
    }

    /// `token` with its header or claims replaced, the signature kept.
    fn replace(token: &CompactJwt, index: usize, value: &Value) -> CompactJwt {
        let mut segments: Vec<String> = token.as_str().split('.').map(str::to_owned).collect();
        segments[index] = segment(value);
        CompactJwt(segments.join("."))
    }

    #[test]
    fn a_signed_token_verifies_and_says_es256() {
        let (signer, verifying) = key();
        let token = signer
            .sign(
                &Header {
                    typ: Some("JWT"),
                    kid: Some("ABC123DEFG"),
                },
                &json!({"iss": "T"}),
            )
            .unwrap();
        let claims: Value = verify_es256(&token, &verifying).unwrap();
        assert_eq!(claims, json!({"iss": "T"}));

        let header = base64url::decode_segment(token.as_str().split('.').next().unwrap()).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&header).unwrap(),
            json!({"typ": "JWT", "alg": "ES256", "kid": "ABC123DEFG"})
        );
        let bare = signer.sign(&Header::default(), &json!({})).unwrap();
        let header = base64url::decode_segment(bare.as_str().split('.').next().unwrap()).unwrap();
        assert_eq!(header, br#"{"alg":"ES256"}"#);
    }

    #[test]
    fn a_token_from_another_key_does_not_verify() {
        let (signer, _) = key();
        let (_, other) = key();
        let token = signer.sign(&Header::default(), &json!({"a": 1})).unwrap();
        assert!(matches!(
            verify_es256::<Value>(&token, &other),
            Err(JwtError::Signature)
        ));
    }

    #[test]
    fn a_tampered_claim_does_not_verify() {
        let (signer, verifying) = key();
        let token = signer
            .sign(&Header::default(), &json!({"aud": "https://a.example"}))
            .unwrap();
        let tampered = replace(&token, 1, &json!({"aud": "https://b.example"}));
        assert!(matches!(
            verify_es256::<Value>(&tampered, &verifying),
            Err(JwtError::Signature)
        ));
    }

    /// `alg: none` (and any other algorithm) is refused for what it says,
    /// whatever the signature: the algorithm isn't the token's to choose.
    #[test]
    fn a_token_naming_another_algorithm_is_refused() {
        let (signer, verifying) = key();
        let token = signer.sign(&Header::default(), &json!({})).unwrap();
        for alg in ["none", "HS256", "ES384", "es256"] {
            let other = replace(&token, 0, &json!({"alg": alg}));
            assert!(
                matches!(verify_es256::<Value>(&other, &verifying), Err(JwtError::Algorithm(ref a)) if a == alg),
                "{alg}"
            );
        }
        let none = CompactJwt(format!(
            "{}.{}.",
            segment(&json!({"alg": "none"})),
            segment(&json!({}))
        ));
        assert!(matches!(
            verify_es256::<Value>(&none, &verifying),
            Err(JwtError::Malformed)
        ));

        let critical = replace(&token, 0, &json!({"alg": "ES256", "crit": ["exp"]}));
        assert!(matches!(
            verify_es256::<Value>(&critical, &verifying),
            Err(JwtError::Critical)
        ));
    }

    #[test]
    fn a_token_without_three_base64url_segments_is_malformed() {
        let (signer, _) = key();
        let token = signer.sign(&Header::default(), &json!({})).unwrap();
        let text = token.as_str();
        for bad in [
            "",
            "a.b",
            &format!("{text}.d"),
            &format!("{text}="),
            &text.replacen('.', "+.", 1),
        ] {
            assert!(
                matches!(CompactJwt::parse(bad), Err(JwtError::Malformed)),
                "{bad}"
            );
        }
        // A signature that isn't 64 bytes.
        let short = format!(
            "{}.{}",
            text.rsplit_once('.').unwrap().0,
            base64url::encode(&[1; 63])
        );
        assert!(matches!(
            CompactJwt::parse(&short),
            Err(JwtError::Malformed)
        ));
        assert!(CompactJwt::parse(text).is_ok());
    }

    /// `header.claims` signed by hand, so a token can say what no
    /// `Es256Key::sign` call would write.
    fn signed_raw(signer: &Es256Key, header: &[u8], claims: &[u8]) -> CompactJwt {
        let input = format!(
            "{}.{}",
            base64url::encode(header),
            base64url::encode(claims)
        );
        let signature: Signature = signer.0.sign(input.as_bytes());
        CompactJwt(format!(
            "{input}.{}",
            base64url::encode(&signature.to_bytes())
        ))
    }

    #[test]
    fn a_token_with_any_signature_bit_changed_does_not_verify() {
        let (signer, verifying) = key();
        let token = signer.sign(&Header::default(), &json!({"a": 1})).unwrap();
        let (input, signature) = token.as_str().rsplit_once('.').unwrap();
        let signature = base64url::decode_segment(signature).unwrap();
        for index in [0, 31, 32, 63] {
            let mut flipped = signature.clone();
            flipped[index] ^= 1;
            let tampered = CompactJwt(format!("{input}.{}", base64url::encode(&flipped)));
            assert!(
                matches!(
                    verify_es256::<Value>(&tampered, &verifying),
                    Err(JwtError::Signature | JwtError::Malformed)
                ),
                "{index}"
            );
        }
        // Another token's signature from the same key.
        let other = signer.sign(&Header::default(), &json!({"a": 2})).unwrap();
        let spliced = CompactJwt(format!(
            "{input}.{}",
            other.as_str().rsplit_once('.').unwrap().1
        ));
        assert!(matches!(
            verify_es256::<Value>(&spliced, &verifying),
            Err(JwtError::Signature)
        ));
    }

    /// The header is read strictly enough that a second `alg`, or one that
    /// isn't a string, can't slip an algorithm past the check.
    #[test]
    fn a_header_that_is_not_one_es256_object_is_refused() {
        let (signer, verifying) = key();
        for header in [
            &br#"{"alg":"ES256","alg":"none"}"#[..],
            br#"{"alg":"none","alg":"ES256"}"#,
            br#"{"alg":["ES256"]}"#,
            br#"{"typ":"JWT"}"#,
            br#"["ES256"]"#,
            b"not json",
        ] {
            let token = signed_raw(&signer, header, b"{}");
            assert!(
                matches!(
                    verify_es256::<Value>(&token, &verifying),
                    Err(JwtError::Malformed)
                ),
                "{}",
                String::from_utf8_lossy(header)
            );
        }
        // Fields a verifier doesn't read are fine, as RFC 7515 has them.
        let token = signed_raw(&signer, br#"{"alg":"ES256","typ":"JWT","x5u":"u"}"#, b"{}");
        assert!(verify_es256::<Value>(&token, &verifying).is_ok());
    }

    /// A correctly signed token whose claims aren't the JSON the caller
    /// expects is an error, not a default.
    #[test]
    fn signed_claims_that_do_not_read_are_refused() {
        #[derive(Debug, Deserialize)]
        struct Expected {
            exp: i64,
        }
        let (signer, verifying) = key();
        let token = signed_raw(&signer, br#"{"alg":"ES256"}"#, br#"{"exp":7}"#);
        assert_eq!(verify_es256::<Expected>(&token, &verifying).unwrap().exp, 7);
        for claims in [
            &b"not json"[..],
            b"{}",
            br#"{"exp":"soon"}"#,
            br#"{"exp":1.5}"#,
        ] {
            let token = signed_raw(&signer, br#"{"alg":"ES256"}"#, claims);
            assert!(
                matches!(
                    verify_es256::<Expected>(&token, &verifying),
                    Err(JwtError::Claims(_))
                ),
                "{}",
                String::from_utf8_lossy(claims)
            );
        }
    }

    /// A zero `r` or `s` is no signature at all; it never reaches the
    /// verifier.
    #[test]
    fn a_zero_signature_is_malformed() {
        let (signer, _) = key();
        let token = signer.sign(&Header::default(), &json!({})).unwrap();
        let input = token.as_str().rsplit_once('.').unwrap().0;
        let zero = format!("{input}.{}", base64url::encode(&[0; 64]));
        assert!(matches!(CompactJwt::parse(&zero), Err(JwtError::Malformed)));
    }

    #[test]
    fn a_token_prints_as_nothing_but_its_type() {
        let (signer, _) = key();
        let token = signer.sign(&Header::default(), &json!({})).unwrap();
        assert_eq!(format!("{token:?}"), "CompactJwt(..)");
        assert_eq!(format!("{signer:?}"), "Es256Key(..)");
    }
}
