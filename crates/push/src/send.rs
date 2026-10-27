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

/// `Retry-After` (RFC 9110 §10.2.3) as a wait from `now`: seconds, or an
/// HTTP date (a date already past is no wait). `None` when there is none
/// or it reads as neither.
fn retry_after(headers: &HeaderMap, now: i64) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(Duration::from_secs(value.parse().unwrap_or(u64::MAX)));
    }
    let at = httpdate::parse_http_date(value).ok()?;
    let now = UNIX_EPOCH + Duration::from_secs(u64::try_from(now).unwrap_or(0));
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
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
}
