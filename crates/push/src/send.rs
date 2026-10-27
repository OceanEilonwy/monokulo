//! Sending a push: sign it, `POST` it to the endpoint, and say what the
//! push service's answer means for the subscription (RFC 8030 §5, §7).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, UNIX_EPOCH};

use http::header::RETRY_AFTER;
use http::{HeaderMap, StatusCode};

use crate::message::{Message, Options};
use crate::subscription::{Endpoint, Origin};
use crate::vapid::{Authorization, Contact, VapidError, VapidKey, EXPIRY};
use crate::wire;

/// How long a signed token is reused for pushes to the same origin: an
/// hour short of [`EXPIRY`], so one is never sent close to expiring. A
/// store with ten tills on one push service signs once in 11 hours, not
/// once a push.
const REUSE: Duration = Duration::from_secs(EXPIRY.as_secs() - 60 * 60);

/// Sends pushes signed with one VAPID key.
///
/// The HTTP client is the caller's: its timeouts, its redirect policy
/// (a push service has no reason to redirect, so none is the right one)
/// and which addresses it may reach are decided where the sender runs.
pub struct Sender {
    client: reqwest::Client,
    key: Arc<VapidKey>,
    contact: Option<Contact>,
    /// The token last signed for each push service's origin, and when.
    signed: Mutex<HashMap<Origin, (Authorization, i64)>>,
}

/// A push the push service took (any 2xx; RFC 8030 §5 has it answer
/// `201 Created`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sent {
    pub status: StatusCode,
}

/// A push that wasn't taken, sorted by what the sender should do next.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// `404` or `410`: the subscription is gone (RFC 8030 §7.3). Stop
    /// sending to it.
    #[error("the subscription is gone")]
    Gone,
    /// `413`: the body is more than the push service takes. Sending it
    /// again won't help.
    #[error("the push service refused the push as too large")]
    TooLarge,
    /// `429`: try again later, after `retry_after` when the push service
    /// said.
    #[error("the push service is limiting this sender")]
    RateLimited { retry_after: Option<Duration> },
    /// `5xx`: the push service's own trouble; try again later.
    #[error("the push service failed ({status})")]
    Server {
        status: StatusCode,
        retry_after: Option<Duration>,
    },
    /// Any other answer (`400`, `401`, `403`, …): the push service refused
    /// this push for a reason of its own.
    #[error("the push service refused the push ({status})")]
    Refused { status: StatusCode },
    /// The request didn't get an answer. The error leaves out the URL,
    /// which is the subscription's capability.
    #[error("the request failed: {0}")]
    Request(reqwest::Error),
    #[error("signing failed: {0}")]
    Vapid(#[from] VapidError),
}

impl Sender {
    /// `contact` is the `sub` claim every token carries, how a push
    /// service's operator can reach whoever runs the sender.
    pub fn new(client: reqwest::Client, key: Arc<VapidKey>, contact: Option<Contact>) -> Self {
        Sender {
            client,
            key,
            contact,
            signed: Mutex::new(HashMap::new()),
        }
    }

    /// Sends `message` to `to`. A [`Message::Encrypted`] is already
    /// encrypted to the subscription's keys ([`crate::encrypt()`]), afresh
    /// for this attempt.
    pub async fn send(
        &self,
        to: &Endpoint,
        message: &Message,
        options: &Options,
        now: i64,
    ) -> Result<Sent, SendError> {
        let authorization = self.authorization(&to.origin(), now)?;
        let response = self
            .client
            .post(to.as_url().clone())
            .headers(wire::headers(message, options, &authorization))
            .body(message.body().to_vec())
            .send()
            .await
            .map_err(|error| SendError::Request(error.without_url()))?;
        outcome(response.status(), response.headers(), now)
    }

    /// A token for `origin`: the one signed less than [`REUSE`] ago, or a
    /// new one.
    fn authorization(&self, origin: &Origin, now: i64) -> Result<Authorization, VapidError> {
        let reuse = REUSE.as_secs() as i64;
        let fresh = |signed_at: i64| (signed_at..signed_at.saturating_add(reuse)).contains(&now);
        let mut signed = self.signed.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((authorization, _)) = signed.get(origin).filter(|(_, at)| fresh(*at)) {
            return Ok(authorization.clone());
        }
        let authorization = self.key.authorization(origin, self.contact.as_ref(), now)?;
        // Push services come and go with the subscriptions; a stale entry
        // would never be used again.
        signed.retain(|_, (_, at)| fresh(*at));
        signed.insert(origin.clone(), (authorization.clone(), now));
        Ok(authorization)
    }
}

/// What a push service's answer means (RFC 8030 §5, §7.3, §8.4).
fn outcome(status: StatusCode, headers: &HeaderMap, now: i64) -> Result<Sent, SendError> {
    match status {
        status if status.is_success() => Ok(Sent { status }),
        StatusCode::NOT_FOUND | StatusCode::GONE => Err(SendError::Gone),
        StatusCode::PAYLOAD_TOO_LARGE => Err(SendError::TooLarge),
        StatusCode::TOO_MANY_REQUESTS => Err(SendError::RateLimited {
            retry_after: retry_after(headers, now),
        }),
        status if status.is_server_error() => Err(SendError::Server {
            status,
            retry_after: retry_after(headers, now),
        }),
        status => Err(SendError::Refused { status }),
    }
}

/// The longest wait `Retry-After` is read as: as long as the longest
/// [`Ttl`](crate::Ttl), so a caller adding it to a time can't overflow.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(u32::MAX as u64);

/// `Retry-After` (RFC 9110 §10.2.3) as a wait from `now`: seconds, or an
/// HTTP date (a date already past is no wait), at most [`MAX_RETRY_AFTER`].
/// `None` when there is none or it reads as neither.
fn retry_after(headers: &HeaderMap, now: i64) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        let seconds = value.parse::<u32>().unwrap_or(u32::MAX);
        return Some(Duration::from_secs(u64::from(seconds)));
    }
    let at = httpdate::parse_http_date(value).ok()?;
    let now = UNIX_EPOCH + Duration::from_secs(u64::try_from(now).unwrap_or(0));
    Some(
        at.duration_since(now)
            .unwrap_or(Duration::ZERO)
            .min(MAX_RETRY_AFTER),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encrypt::{decrypt, encrypt, ReceiverKeys};
    use crate::message::{Topic, Ttl, Urgency};
    use crate::wire::PushRequest;
    use http::HeaderValue;

    const NOW: i64 = 1_800_000_000;

    fn retry(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static(value));
        headers
    }

    #[test]
    fn a_2xx_answer_is_a_sent_push() {
        for code in [201, 200, 202] {
            let status = StatusCode::from_u16(code).unwrap();
            assert_eq!(
                outcome(status, &HeaderMap::new(), NOW).unwrap(),
                Sent { status }
            );
        }
    }

    #[test]
    fn a_404_or_410_answer_means_the_subscription_is_gone() {
        assert!(matches!(
            outcome(StatusCode::NOT_FOUND, &HeaderMap::new(), NOW),
            Err(SendError::Gone)
        ));
        assert!(matches!(
            outcome(StatusCode::GONE, &HeaderMap::new(), NOW),
            Err(SendError::Gone)
        ));
    }

    #[test]
    fn a_413_answer_is_too_large() {
        assert!(matches!(
            outcome(StatusCode::PAYLOAD_TOO_LARGE, &HeaderMap::new(), NOW),
            Err(SendError::TooLarge)
        ));
    }

    #[test]
    fn a_429_answer_waits_as_retry_after_says() {
        let limited =
            |headers: &HeaderMap| match outcome(StatusCode::TOO_MANY_REQUESTS, headers, NOW) {
                Err(SendError::RateLimited { retry_after }) => retry_after,
                other => panic!("{other:?}"),
            };
        assert_eq!(limited(&HeaderMap::new()), None);
        assert_eq!(limited(&retry("7")), Some(Duration::from_secs(7)));
        assert_eq!(limited(&retry(" 120 ")), Some(Duration::from_secs(120)));
        // NOW is 2027-01-15T08:00:00Z.
        assert_eq!(
            limited(&retry("Fri, 15 Jan 2027 08:01:30 GMT")),
            Some(Duration::from_secs(90))
        );
        assert_eq!(
            limited(&retry("Fri, 15 Jan 2027 07:00:00 GMT")),
            Some(Duration::ZERO)
        );
        assert_eq!(limited(&retry("soon")), None);
        assert_eq!(limited(&retry("-5")), None);
    }

    #[test]
    fn a_5xx_answer_is_the_push_service_failing() {
        match outcome(StatusCode::INTERNAL_SERVER_ERROR, &HeaderMap::new(), NOW) {
            Err(SendError::Server {
                status,
                retry_after,
            }) => {
                assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
                assert_eq!(retry_after, None);
            }
            other => panic!("{other:?}"),
        }
        match outcome(StatusCode::SERVICE_UNAVAILABLE, &retry("30"), NOW) {
            Err(SendError::Server {
                status,
                retry_after,
            }) => {
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(retry_after, Some(Duration::from_secs(30)));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn any_other_answer_is_a_refusal() {
        for code in [400, 401, 403, 302] {
            let status = StatusCode::from_u16(code).unwrap();
            assert!(
                matches!(outcome(status, &HeaderMap::new(), NOW), Err(SendError::Refused { status: s }) if s == status),
                "{code}"
            );
        }
    }

    fn sender() -> Sender {
        Sender::new(reqwest::Client::new(), Arc::new(VapidKey::generate()), None)
    }

    /// The request `send` makes reads back through `PushRequest::parse`
    /// with its options, a signature that verifies for the endpoint's
    /// origin, and a body the device decrypts.
    #[test]
    fn a_push_request_reads_back_as_the_push_sent() {
        let sender = sender();
        let receiver = ReceiverKeys::generate();
        let endpoint = Endpoint::parse("https://push.example.net/wpush/abc").unwrap();
        let options = Options {
            urgency: Urgency::High,
            ttl: Ttl::seconds(1800),
            topic: Some(Topic::parse("0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f").unwrap()),
        };
        let message =
            Message::Encrypted(encrypt(&receiver.subscription_keys(), b"{\"sale\":{}}").unwrap());
        let authorization = sender.authorization(&endpoint.origin(), NOW).unwrap();

        let request = PushRequest::parse(
            &wire::headers(&message, &options, &authorization),
            message.body(),
        )
        .unwrap();
        assert_eq!(request.options, options);
        let claims = crate::vapid::verify(
            &request.authorization,
            &sender.key.public(),
            &endpoint.origin(),
            NOW,
        )
        .unwrap();
        assert_eq!(claims.aud.as_str(), "https://push.example.net");
        let Message::Encrypted(body) = request.message else {
            panic!("not encrypted");
        };
        assert_eq!(
            decrypt(&receiver, body.as_bytes()).unwrap(),
            b"{\"sale\":{}}"
        );
    }

    #[test]
    fn a_token_is_reused_for_its_origin_for_11_hours() {
        let sender = sender();
        let one = Origin::parse("https://push.example.net").unwrap();
        let two = Origin::parse("https://fcm.googleapis.com").unwrap();
        let first = sender.authorization(&one, NOW).unwrap();
        let reuse = REUSE.as_secs() as i64;
        assert_eq!(sender.authorization(&one, NOW + reuse - 1).unwrap(), first);

        let other = sender.authorization(&two, NOW + 1).unwrap();
        assert_ne!(other, first);
        assert!(crate::vapid::verify(&other, &sender.key.public(), &two, NOW + 1).is_ok());

        let renewed = sender.authorization(&one, NOW + reuse + 1).unwrap();
        assert_ne!(renewed, first);
        assert!(
            crate::vapid::verify(&renewed, &sender.key.public(), &one, NOW + reuse + 1).is_ok()
        );
        // The other origin's token was too old to keep.
        assert_eq!(sender.signed.lock().unwrap().len(), 1);

        // A clock set back doesn't reuse a token from its future.
        assert_ne!(sender.authorization(&one, NOW).unwrap(), renewed);
    }

    /// RFC 9110 §5.6.7 has a recipient read all three date formats.
    #[test]
    fn retry_after_reads_every_http_date_format_and_any_number() {
        let wait = |value: &'static str| retry_after(&retry(value), NOW);
        // NOW is Friday 2027-01-15 08:00:00 UTC.
        for value in [
            "Fri, 15 Jan 2027 08:00:45 GMT",
            "Friday, 15-Jan-27 08:00:45 GMT",
            "Fri Jan 15 08:00:45 2027",
        ] {
            assert_eq!(wait(value), Some(Duration::from_secs(45)), "{value}");
        }
        assert_eq!(wait("0"), Some(Duration::ZERO));
        // Larger than any wait, not an error, but read as the longest TTL
        // so that adding it to a time can't overflow; the caller caps it
        // at the push's own expiry.
        assert_eq!(wait("99999999999999999999999"), Some(MAX_RETRY_AFTER));
        assert_eq!(wait("4294967296"), Some(MAX_RETRY_AFTER));
        assert_eq!(wait("Fri, 31 Dec 9999 23:59:59 GMT"), Some(MAX_RETRY_AFTER));
        assert!(std::time::Instant::now()
            .checked_add(MAX_RETRY_AFTER)
            .is_some());
        assert!(std::time::SystemTime::now()
            .checked_add(MAX_RETRY_AFTER)
            .is_some());
        for value in ["", "1.5", "+5", "5s", "15 Jan 2027"] {
            assert_eq!(wait(value), None, "{value:?}");
        }
        let mut opaque = HeaderMap::new();
        opaque.insert(RETRY_AFTER, HeaderValue::from_bytes(b"\xff").unwrap());
        assert_eq!(retry_after(&opaque, NOW), None);
    }

    /// Only 429 and 5xx carry a wait; a Retry-After on any other answer
    /// changes nothing about it.
    #[test]
    fn retry_after_is_read_only_where_a_retry_is_meant() {
        let headers = retry("30");
        assert!(matches!(
            outcome(StatusCode::GONE, &headers, NOW),
            Err(SendError::Gone)
        ));
        assert!(matches!(
            outcome(StatusCode::PAYLOAD_TOO_LARGE, &headers, NOW),
            Err(SendError::TooLarge)
        ));
        assert!(matches!(
            outcome(StatusCode::BAD_REQUEST, &headers, NOW),
            Err(SendError::Refused {
                status: StatusCode::BAD_REQUEST
            })
        ));
        assert!(matches!(
            outcome(StatusCode::CREATED, &headers, NOW),
            Ok(Sent {
                status: StatusCode::CREATED
            })
        ));
        match outcome(
            StatusCode::BAD_GATEWAY,
            &retry("Fri, 15 Jan 2027 08:00:10 GMT"),
            NOW,
        ) {
            Err(SendError::Server {
                status,
                retry_after,
            }) => {
                assert_eq!(status, StatusCode::BAD_GATEWAY);
                assert_eq!(retry_after, Some(Duration::from_secs(10)));
            }
            other => panic!("{other:?}"),
        }
    }

    /// The delivery worker shares one `Sender` across tasks and spawns
    /// `send`'s future onto the runtime: both have to be thread-safe.
    #[test]
    fn a_sender_and_its_sends_can_cross_threads() {
        fn shared<T: Send + Sync>(_: &T) {}
        fn spawnable<T: Send>(_: T) {}
        let sender = sender();
        shared(&sender);
        let endpoint = Endpoint::parse("https://push.example.net/x").unwrap();
        let message = Message::PlainJson(crate::PlainJson::new(&()).unwrap());
        let options = Options {
            urgency: Urgency::Normal,
            ttl: Ttl::ZERO,
            topic: None,
        };
        spawnable(sender.send(&endpoint, &message, &options, NOW));
    }

    /// Many pushes to one push service at once sign one token between
    /// them, not one each. All at the same second: threads run in any
    /// order, and one whose clock reads earlier than the token's signing
    /// time re-signs by design (the clock-set-back rule).
    #[test]
    fn pushes_at_once_to_one_origin_share_one_token() {
        let sender = sender();
        let origin = Origin::parse("https://push.example.net").unwrap();
        let (shared, at) = (&sender, &origin);
        let tokens: Vec<Authorization> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| scope.spawn(move || shared.authorization(at, NOW).unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(tokens.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(sender.signed.lock().unwrap().len(), 1);
    }

    /// The `sub` claim is the contact the sender was made with, on every
    /// token.
    #[test]
    fn every_token_carries_the_senders_contact() {
        let contact = Contact::parse("mailto:ops@example.net").unwrap();
        let key = Arc::new(VapidKey::generate());
        let sender = Sender::new(reqwest::Client::new(), key.clone(), Some(contact.clone()));
        let origin = Origin::parse("https://push.example.net").unwrap();
        let token = sender.authorization(&origin, NOW).unwrap();
        let claims = crate::vapid::verify(&token, &key.public(), &origin, NOW).unwrap();
        assert_eq!(claims.sub, Some(contact));
    }
}
