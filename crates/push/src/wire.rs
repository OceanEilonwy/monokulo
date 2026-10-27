//! A push as it travels (RFC 8030 §5): the headers a sender writes and a
//! push service reads. [`crate::Sender`] writes them and
//! [`PushRequest::parse`] reads them, both from the names and rules here,
//! so the two can't disagree.

use http::header::{AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE};
use http::{HeaderMap, HeaderName, HeaderValue};

use crate::encrypt::{Encrypted, EncryptedError};
use crate::message::{Message, Options, PlainJson, PlainJsonError, Topic, Ttl, Urgency};
use crate::vapid::{Authorization, VapidError};
use crate::MAX_BODY;

/// The RFC 8030 header names, and the content coding of an encrypted push.
pub mod header {
    use http::HeaderName;

    pub const TTL: HeaderName = HeaderName::from_static("ttl");
    pub const URGENCY: HeaderName = HeaderName::from_static("urgency");
    pub const TOPIC: HeaderName = HeaderName::from_static("topic");
    /// `Content-Encoding` of an encrypted push (RFC 8291 §4).
    pub const AES128GCM: &str = "aes128gcm";
    /// `Content-Type` of an encrypted push: the bytes mean nothing to the
    /// push service.
    pub const OCTET_STREAM: &str = "application/octet-stream";
    /// `Content-Type` of a [`PlainJson`](crate::PlainJson) push.
    pub const JSON: &str = "application/json";
}

/// A push request as a push service receives it: who signed it (not yet
/// verified: [`crate::vapid::verify`] is the receiver's to call with the
/// key the subscription was made for), the RFC 8030 options and the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRequest {
    pub authorization: Authorization,
    pub options: Options,
    pub message: Message,
}

/// Why a request isn't a push this crate's [`crate::Sender`] could have
/// sent.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("no Authorization header")]
    NoAuthorization,
    #[error("Authorization: {0}")]
    Authorization(VapidError),
    /// RFC 8030 §5.2: a push service may refuse a push without one.
    #[error("no TTL header")]
    NoTtl,
    #[error("the TTL header isn't a number of seconds")]
    Ttl,
    #[error("the Urgency header isn't very-low, low, normal or high")]
    Urgency,
    #[error("the Topic header isn't 1 to 32 URL-safe characters")]
    Topic,
    #[error("{0} bytes, more than a push may carry ({MAX_BODY})")]
    TooLarge(usize),
    #[error("no body")]
    Empty,
    #[error("the body is neither aes128gcm nor application/json")]
    ContentType,
    #[error("the aes128gcm body: {0}")]
    Encrypted(EncryptedError),
    #[error("the JSON body: {0}")]
    Json(PlainJsonError),
}

impl PushRequest {
    pub fn parse(headers: &HeaderMap, body: &[u8]) -> Result<Self, WireError> {
        // First, so a body too large is answered as that whatever else is
        // wrong with the request.
        if body.len() > MAX_BODY {
            return Err(WireError::TooLarge(body.len()));
        }
        let authorization = headers
            .get(AUTHORIZATION)
            .ok_or(WireError::NoAuthorization)?;
        let authorization = authorization
            .to_str()
            .map_err(|_| WireError::Authorization(VapidError::Header))
            .and_then(|value| Authorization::parse(value).map_err(WireError::Authorization))?;

        let ttl = text(headers, &header::TTL, WireError::Ttl)?.ok_or(WireError::NoTtl)?;
        let ttl = parse_ttl(ttl).ok_or(WireError::Ttl)?;
        let urgency = match text(headers, &header::URGENCY, WireError::Urgency)? {
            Some(value) => value.parse().map_err(|_| WireError::Urgency)?,
            None => Urgency::default(),
        };
        let topic = text(headers, &header::TOPIC, WireError::Topic)?
            .map(|value| Topic::parse(value).map_err(|_| WireError::Topic))
            .transpose()?;

        if body.is_empty() {
            return Err(WireError::Empty);
        }
        let encoding = text(headers, &CONTENT_ENCODING, WireError::ContentType)?;
        let message = match encoding {
            Some(encoding) if encoding.eq_ignore_ascii_case(header::AES128GCM) => {
                Message::Encrypted(Encrypted::parse(body).map_err(WireError::Encrypted)?)
            }
            Some(_) => return Err(WireError::ContentType),
            None if is_json(headers) => {
                Message::PlainJson(PlainJson::from_slice(body).map_err(WireError::Json)?)
            }
            None => return Err(WireError::ContentType),
        };

        Ok(PushRequest {
            authorization,
            options: Options {
                urgency,
                ttl,
                topic,
            },
            message,
        })
    }
}

/// The headers [`PushRequest::parse`] reads, for `message` sent with
/// `options` and signed by `authorization`.
pub(crate) fn headers(
    message: &Message,
    options: &Options,
    authorization: &Authorization,
) -> HeaderMap {
    let mut headers = HeaderMap::with_capacity(6);
    headers.insert(AUTHORIZATION, authorization.header_value());
    headers.insert(header::TTL, HeaderValue::from(options.ttl.as_secs()));
    headers.insert(
        header::URGENCY,
        HeaderValue::from_static(options.urgency.header_value()),
    );
    if let Some(topic) = &options.topic {
        headers.insert(header::TOPIC, visible_ascii(topic.as_str()));
    }
    match message {
        Message::Encrypted(_) => {
            headers.insert(
                CONTENT_ENCODING,
                HeaderValue::from_static(header::AES128GCM),
            );
            headers.insert(CONTENT_TYPE, HeaderValue::from_static(header::OCTET_STREAM));
        }
        Message::PlainJson(_) => {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static(header::JSON));
        }
    }
    headers
}

/// A header value of characters a [`Topic`] is made of.
fn visible_ascii(text: &str) -> HeaderValue {
    HeaderValue::from_str(text)
        .unwrap_or_else(|_| unreachable!("a topic is URL-safe base64 characters"))
}

/// A header's value as text, if it is there; `invalid` if it isn't
/// visible ASCII.
fn text<'a>(
    headers: &'a HeaderMap,
    name: &HeaderName,
    invalid: WireError,
) -> Result<Option<&'a str>, WireError> {
    headers
        .get(name)
        .map(|value| value.to_str().map(str::trim).map_err(|_| invalid))
        .transpose()
}

/// `delta-seconds`: digits only. A TTL past `u32::MAX` seconds (136 years)
/// is read as that, the longest any push service would keep anything.
fn parse_ttl(text: &str) -> Option<Ttl> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let seconds = text.parse::<u64>().unwrap_or(u64::MAX);
    Some(Ttl::seconds(u32::try_from(seconds).unwrap_or(u32::MAX)))
}

/// `Content-Type: application/json`, with or without parameters.
fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case(header::JSON))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encrypt::{encrypt, ReceiverKeys};
    use crate::vapid::VapidKey;
    use crate::Origin;

    fn authorization() -> Authorization {
        VapidKey::generate()
            .authorization(
                &Origin::parse("https://relay.example.net").unwrap(),
                None,
                1_800_000_000,
            )
            .unwrap()
    }

    fn encrypted() -> Message {
        Message::Encrypted(encrypt(&ReceiverKeys::generate().subscription_keys(), b"{}").unwrap())
    }

    fn options() -> Options {
        Options {
            urgency: Urgency::High,
            ttl: Ttl::seconds(60),
            topic: Some(Topic::parse("0123456789abcdef0123456789abcdef").unwrap()),
        }
    }

    #[test]
    fn what_a_sender_writes_reads_back_as_the_same_push() {
        let authorization = authorization();
        for message in [
            encrypted(),
            Message::PlainJson(PlainJson::new(&serde_json::json!({"phase": "paid"})).unwrap()),
        ] {
            let headers = headers(&message, &options(), &authorization);
            let request = PushRequest::parse(&headers, message.body()).unwrap();
            assert_eq!(
                request,
                PushRequest {
                    authorization: authorization.clone(),
                    options: options(),
                    message,
                }
            );
        }
    }

    fn base() -> (HeaderMap, Vec<u8>) {
        let message = encrypted();
        (
            headers(&message, &options(), &authorization()),
            message.body().to_vec(),
        )
    }

    #[test]
    fn urgency_and_topic_may_be_left_out() {
        let (mut headers, body) = base();
        headers.remove(header::URGENCY);
        headers.remove(header::TOPIC);
        let request = PushRequest::parse(&headers, &body).unwrap();
        assert_eq!(request.options.urgency, Urgency::Normal);
        assert_eq!(request.options.topic, None);
    }

    #[test]
    fn a_ttl_is_required_and_read_as_seconds() {
        let (mut headers, body) = base();
        for (value, ttl) in [
            ("0", 0),
            ("2419200", 2_419_200),
            ("99999999999999999999999", u32::MAX),
        ] {
            headers.insert(header::TTL, HeaderValue::from_static(value));
            assert_eq!(
                PushRequest::parse(&headers, &body).unwrap().options.ttl,
                Ttl::seconds(ttl),
                "{value}"
            );
        }
        for value in ["", "-1", "1.5", "60s", "+60"] {
            headers.insert(header::TTL, HeaderValue::from_static(value));
            assert!(
                matches!(PushRequest::parse(&headers, &body), Err(WireError::Ttl)),
                "{value}"
            );
        }
        headers.remove(header::TTL);
        assert!(matches!(
            PushRequest::parse(&headers, &body),
            Err(WireError::NoTtl)
        ));
    }

    #[test]
    fn a_push_without_a_valid_vapid_authorization_is_refused() {
        let (mut headers, body) = base();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer abc"));
        assert!(matches!(
            PushRequest::parse(&headers, &body),
            Err(WireError::Authorization(_))
        ));
        headers.remove(AUTHORIZATION);
        assert!(matches!(
            PushRequest::parse(&headers, &body),
            Err(WireError::NoAuthorization)
        ));
    }

    #[test]
    fn a_bad_urgency_or_topic_is_refused() {
        let (mut headers, body) = base();
        headers.insert(header::URGENCY, HeaderValue::from_static("urgent"));
        assert!(matches!(
            PushRequest::parse(&headers, &body),
            Err(WireError::Urgency)
        ));
        let (mut headers, body) = base();
        headers.insert(header::TOPIC, HeaderValue::from_static("a/b"));
        assert!(matches!(
            PushRequest::parse(&headers, &body),
            Err(WireError::Topic)
        ));
    }

    #[test]
    fn a_body_must_be_aes128gcm_or_json_and_fit() {
        let (mut headers, body) = base();
        assert!(matches!(
            PushRequest::parse(&headers, &[]),
            Err(WireError::Empty)
        ));
        assert!(matches!(
            PushRequest::parse(&headers, &vec![0; MAX_BODY + 1]),
            Err(WireError::TooLarge(4097))
        ));
        // Too large is said before anything else is looked at.
        assert!(matches!(
            PushRequest::parse(&HeaderMap::new(), &vec![0; MAX_BODY + 1]),
            Err(WireError::TooLarge(4097))
        ));
        assert!(matches!(
            PushRequest::parse(&headers, &body[..40]),
            Err(WireError::Encrypted(_))
        ));

        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("aesgcm"));
        assert!(matches!(
            PushRequest::parse(&headers, &body),
            Err(WireError::ContentType)
        ));

        headers.remove(CONTENT_ENCODING);
        assert!(matches!(
            PushRequest::parse(&headers, &body),
            Err(WireError::ContentType)
        ));
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        assert!(matches!(
            PushRequest::parse(&headers, &body),
            Err(WireError::Json(_))
        ));
        assert!(matches!(
            PushRequest::parse(&headers, br#"{"phase":"seen"}"#)
                .unwrap()
                .message,
            Message::PlainJson(_)
        ));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        assert!(matches!(
            PushRequest::parse(&headers, br#"{}"#),
            Err(WireError::ContentType)
        ));
    }

    /// `TTL: 0` (deliver now or never) is written and read back as zero,
    /// not taken for a missing header.
    #[test]
    fn a_ttl_of_zero_travels_as_zero() {
        let authorization = authorization();
        let message = encrypted();
        let options = Options {
            urgency: Urgency::VeryLow,
            ttl: Ttl::ZERO,
            topic: None,
        };
        let headers = headers(&message, &options, &authorization);
        assert_eq!(headers[header::TTL], "0");
        assert!(!headers.contains_key(header::TOPIC));
        let request = PushRequest::parse(&headers, message.body()).unwrap();
        assert_eq!(request.options, options);
    }

    /// Every urgency is written as its header value and read back.
    #[test]
    fn every_urgency_travels_as_itself() {
        let authorization = authorization();
        let message = encrypted();
        for urgency in [
            Urgency::VeryLow,
            Urgency::Low,
            Urgency::Normal,
            Urgency::High,
        ] {
            let options = Options {
                urgency,
                ..options()
            };
            let headers = headers(&message, &options, &authorization);
            assert_eq!(headers[header::URGENCY], urgency.header_value());
            assert_eq!(
                PushRequest::parse(&headers, message.body())
                    .unwrap()
                    .options
                    .urgency,
                urgency
            );
        }
    }

    /// Header values that aren't visible ASCII are refused as the header
    /// they are in, never read lossily.
    #[test]
    fn a_header_that_is_not_visible_ascii_is_refused_as_itself() {
        let opaque = HeaderValue::from_bytes(b"\xff60").unwrap();
        type Expected = fn(&WireError) -> bool;
        let cases: [(HeaderName, Expected); 5] = [
            (AUTHORIZATION, |e| {
                matches!(e, WireError::Authorization(VapidError::Header))
            }),
            (header::TTL, |e| matches!(e, WireError::Ttl)),
            (header::URGENCY, |e| matches!(e, WireError::Urgency)),
            (header::TOPIC, |e| matches!(e, WireError::Topic)),
            (CONTENT_ENCODING, |e| matches!(e, WireError::ContentType)),
        ];
        for (name, expected) in cases {
            let (mut headers, body) = base();
            headers.insert(name.clone(), opaque.clone());
            let error = PushRequest::parse(&headers, &body).unwrap_err();
            assert!(expected(&error), "{name}: {error:?}");
        }
    }

    /// The parts of the request a sender may write in another case or with
    /// spaces around them.
    #[test]
    fn content_coding_and_type_are_read_in_any_case() {
        let (mut headers, body) = base();
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("AES128GCM"));
        headers.insert(header::TTL, HeaderValue::from_static(" 60 "));
        let request = PushRequest::parse(&headers, &body).unwrap();
        assert!(matches!(request.message, Message::Encrypted(_)));
        assert_eq!(request.options.ttl, Ttl::seconds(60));

        let (mut headers, _) = base();
        headers.remove(CONTENT_ENCODING);
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("Application/JSON ;charset=UTF-8"),
        );
        assert!(matches!(
            PushRequest::parse(&headers, b"{}").unwrap().message,
            Message::PlainJson(_)
        ));
    }

    /// The largest JSON a push may carry goes through; a byte more is too
    /// large, and JSON with something after it isn't JSON.
    #[test]
    fn a_json_push_of_4096_bytes_reads_and_more_does_not() {
        let (mut headers, _) = base();
        headers.remove(CONTENT_ENCODING);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static(header::JSON));
        let largest = serde_json::to_vec(&"a".repeat(MAX_BODY - 2)).unwrap();
        assert_eq!(largest.len(), MAX_BODY);
        let request = PushRequest::parse(&headers, &largest).unwrap();
        assert_eq!(request.message.body(), largest.as_slice());

        let over = serde_json::to_vec(&"a".repeat(MAX_BODY - 1)).unwrap();
        assert!(matches!(
            PushRequest::parse(&headers, &over),
            Err(WireError::TooLarge(4097))
        ));
        for body in [&b"{} {}"[..], b"{}x", b"\xff"] {
            assert!(
                matches!(PushRequest::parse(&headers, body), Err(WireError::Json(_))),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    /// A relay's whole check of an incoming push: the request parses, then
    /// its signature is verified against the key the handle was made for
    /// and the relay's own origin. A sender signing for another push
    /// service, or with another key, is turned away at the second step.
    #[test]
    fn a_relay_verifies_what_it_parsed_against_the_handle_key_and_its_origin() {
        let relay = Origin::parse("https://relay.example.net").unwrap();
        let sender = VapidKey::generate();
        let message = encrypted();
        let now = 1_800_000_000;
        let parse_and_verify = |authorization: &Authorization, at: &Origin| {
            let headers = headers(&message, &options(), authorization);
            let request = PushRequest::parse(&headers, message.body()).unwrap();
            crate::vapid::verify(&request.authorization, &sender.public(), at, now)
        };

        let signed = sender.authorization(&relay, None, now).unwrap();
        assert!(parse_and_verify(&signed, &relay).is_ok());

        let elsewhere = sender
            .authorization(
                &Origin::parse("https://fcm.googleapis.com").unwrap(),
                None,
                now,
            )
            .unwrap();
        assert!(matches!(
            parse_and_verify(&elsewhere, &relay),
            Err(VapidError::Audience)
        ));

        let stranger = VapidKey::generate()
            .authorization(&relay, None, now)
            .unwrap();
        assert!(matches!(
            parse_and_verify(&stranger, &relay),
            Err(VapidError::KeyMismatch)
        ));

        let stale = sender
            .authorization(&relay, None, now - crate::vapid::EXPIRY.as_secs() as i64)
            .unwrap();
        assert!(matches!(
            parse_and_verify(&stale, &relay),
            Err(VapidError::Expired)
        ));
    }
}
