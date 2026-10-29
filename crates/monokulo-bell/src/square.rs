//! The Square adapter: what the Bell has to do with Square. Check a
//! webhook's signature, read the fields of a payment webhook it acts on,
//! and check, with a merchant's access token, that a location is theirs.
//!
//! A webhook body carries the payment's amount, its card's details and its
//! buyer's: none of it is kept, logged or put into an error, here or by
//! any caller. What is read is read into types that hold exactly that.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit as _, Mac as _};
use live_settings::Secret;
use reqwest::StatusCode;
use serde::Deserialize;
use serde::de::IgnoredAny;
use sha2::Sha256;

use crate::pos::{
    self, AccessToken, BodyError, LocationId, MerchantId, PaymentId, PointOfSale, ProofError,
};
use crate::settings::{ApplicationId, ServiceUrl, SquareEnvironment};

/// The header carrying a webhook's signature.
pub const SIGNATURE_HEADER: &str = "x-square-hmacsha256-signature";

/// The Square API version every request asks for, as the till's
/// (`SquareConfig.API_VERSION`).
const API_VERSION: &str = "2026-09-16";

/// The webhook subscription's signature key.
#[derive(Clone, PartialEq)]
pub struct WebhookKey(Secret);

/// Why a webhook's signature was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    #[error("no {SIGNATURE_HEADER} header")]
    Missing,
    #[error("the {SIGNATURE_HEADER} header isn't base64")]
    NotBase64,
    #[error("the signature isn't this Bell's key's, for this URL and body")]
    Mismatch,
}

impl WebhookKey {
    pub fn new(key: Secret) -> Self {
        WebhookKey(key)
    }

    /// Checks `signature` (the header's value) is Square's for `body`
    /// sent to `notification_url`: base64 of HMAC-SHA256, keyed with the
    /// signature key as it is written, over the URL followed by the body.
    /// The tag is compared in constant time.
    pub fn verify(
        &self,
        notification_url: &str,
        body: &[u8],
        signature: Option<&[u8]>,
    ) -> Result<(), SignatureError> {
        let signature = signature.ok_or(SignatureError::Missing)?;
        let tag = STANDARD
            .decode(signature)
            .map_err(|_| SignatureError::NotBase64)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(self.0.expose().as_bytes())
            .map_err(|_| SignatureError::Mismatch)?;
        mac.update(notification_url.as_bytes());
        mac.update(body);
        // `verify_slice` compares in constant time (`subtle`).
        mac.verify_slice(&tag).map_err(|_| SignatureError::Mismatch)
    }
}

/// What a signed webhook asks of the Bell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notification {
    /// A payment at this merchant's location completed: ding its tills,
    /// once per payment.
    Completed {
        payment_id: PaymentId,
        merchant_id: MerchantId,
        location_id: LocationId,
    },
    /// Anything else: dropped.
    Ignored,
}

/// The event's type, the only field read of every webhook.
#[derive(Deserialize)]
struct Head {
    #[serde(rename = "type")]
    kind: EventType,
}

#[derive(Deserialize)]
enum EventType {
    #[serde(rename = "payment.created")]
    PaymentCreated,
    #[serde(rename = "payment.updated")]
    PaymentUpdated,
    #[serde(other)]
    Other,
}

/// The fields of a payment event the Bell reads; serde skips the rest
/// without keeping it.
#[derive(Deserialize)]
struct PaymentEvent {
    merchant_id: MerchantId,
    data: PaymentData,
}

#[derive(Deserialize)]
struct PaymentData {
    object: PaymentObject,
}

#[derive(Deserialize)]
struct PaymentObject {
    payment: Payment,
}

#[derive(Deserialize)]
struct Payment {
    id: PaymentId,
    location_id: LocationId,
    status: PaymentStatus,
    /// Whether the payment has been refunded at all: what is there is
    /// skipped unread.
    refunded_money: Option<IgnoredAny>,
}

#[derive(Deserialize)]
enum PaymentStatus {
    #[serde(rename = "COMPLETED")]
    Completed,
    #[serde(other)]
    Other,
}

/// What a signed webhook's `body` asks: [`Notification::Completed`] for a
/// `payment.created` or `payment.updated` whose payment is `COMPLETED` and
/// not refunded (a refund sends `payment.updated` of a payment that stays
/// `COMPLETED`, which is no sale); any other type is
/// [`Notification::Ignored`] with nothing but its type read.
pub fn read_notification(body: &[u8]) -> Result<Notification, BodyError> {
    let head: Head = serde_json::from_slice(body)?;
    match head.kind {
        EventType::PaymentCreated | EventType::PaymentUpdated => {}
        EventType::Other => return Ok(Notification::Ignored),
    }
    let event: PaymentEvent = serde_json::from_slice(body)?;
    let payment = event.data.object.payment;
    Ok(match (payment.status, payment.refunded_money) {
        (PaymentStatus::Completed, None) => Notification::Completed {
            payment_id: payment.id,
            merchant_id: event.merchant_id,
            location_id: payment.location_id,
        },
        (PaymentStatus::Completed, Some(_)) | (PaymentStatus::Other, _) => Notification::Ignored,
    })
}

/// The Square adapter: the application whose webhooks this Bell takes,
/// its subscription's key, and Square's API.
pub struct Square {
    client: reqwest::Client,
    api: ServiceUrl,
    application_id: ApplicationId,
    environment: SquareEnvironment,
    /// `bell.public_uri` followed by the webhook path: what Square signs.
    notification_url: String,
    webhook_key: WebhookKey,
}

#[derive(Deserialize)]
struct LocationAnswer {
    location: Location,
}

#[derive(Deserialize)]
struct Location {
    id: LocationId,
    merchant_id: MerchantId,
}

impl Square {
    /// The adapter for `application_id` in `environment`, its webhooks
    /// signed with `webhook_key` for `notification_url`, asking Square's
    /// API at `api` and waiting `timeout` for each answer. Fails only if
    /// TLS can't be set up.
    pub fn new(
        api: ServiceUrl,
        application_id: ApplicationId,
        environment: SquareEnvironment,
        notification_url: String,
        webhook_key: WebhookKey,
        timeout: Duration,
    ) -> reqwest::Result<Self> {
        Ok(Square {
            client: pos::api_client(timeout)?,
            api,
            application_id,
            environment,
            notification_url,
            webhook_key,
        })
    }

    pub fn application_id(&self) -> &ApplicationId {
        &self.application_id
    }

    pub fn environment(&self) -> SquareEnvironment {
        self.environment
    }

    /// The URL Square must send webhooks to: it signs them for it.
    pub fn notification_url(&self) -> &str {
        &self.notification_url
    }

    /// Checks `signature` is Square's for `body`, sent to this Bell's
    /// notification URL.
    pub fn verify(&self, body: &[u8], signature: Option<&[u8]>) -> Result<(), SignatureError> {
        self.webhook_key
            .verify(&self.notification_url, body, signature)
    }

    /// The merchant `location` belongs to, asked of Square with `token`
    /// (`GET /v2/locations/{id}`): Square answers only for a location of
    /// the token's own merchant, so the answer proves both the token and
    /// the location. The token is dropped once the request is sent.
    pub async fn merchant_of(
        &self,
        token: AccessToken,
        location: &LocationId,
    ) -> Result<MerchantId, ProofError> {
        let response = self
            .client
            .get(self.api.join(&format!("/v2/locations/{location}")))
            .bearer_auth(token.expose())
            .header("Square-Version", API_VERSION)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await;
        drop(token);
        let response =
            response.map_err(|error| ProofError::Unavailable(pos::request_error(error)))?;
        let status = response.status();
        if status != StatusCode::OK {
            return Err(ProofError::from_status(status, PointOfSale::Square));
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| ProofError::Unavailable(pos::request_error(error)))?;
        let answer: LocationAnswer = serde_json::from_slice(&body).map_err(|error| {
            ProofError::Unavailable(format!(
                "Square's answer isn't a location: {}",
                BodyError::from(error)
            ))
        })?;
        // Not stored as answered: the till matches webhooks by the ID it
        // chose, so a registration by an alias would never ding.
        if answer.location.id != *location {
            return Err(ProofError::NotById);
        }
        Ok(answer.location.merchant_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Square's own example (its SDKs' webhook helper tests): the key, the
    /// notification URL, the body and the signature Square sent.
    const KEY: &str = "Ibxx_5AKakO-3qeNVR61Dw";
    const URL: &str = "https://webhook.site/679a4f3a-dcfa-49ee-bac5-9d0edad886b9";
    const BODY: &str = r#"{"merchant_id":"MLEFBHHSJGVHD","type":"webhooks.test_notification","event_id":"ac3ac95b-f97d-458c-a6e6-18981597e05f","created_at":"2022-07-13T20:30:59.037339943Z","data":{"type":"webhooks","id":"bc368e64-01aa-407e-b46e-3231809b1129"}}"#;
    const SIGNATURE: &str = "GF4YkrJgGBDZ9NIYbNXBnMzqb2HoL4RW/S6vkZ9/2N4=";

    fn key() -> WebhookKey {
        WebhookKey::new(Secret::new(KEY))
    }

    #[test]
    fn squares_example_signature_is_accepted() {
        assert_eq!(
            key().verify(URL, BODY.as_bytes(), Some(SIGNATURE.as_bytes())),
            Ok(())
        );
    }

    #[test]
    fn a_changed_body_url_or_key_is_refused() {
        let changed = BODY.replace("MLEFBHHSJGVHD", "MLEFBHHSJGVHE");
        assert_eq!(
            key().verify(URL, changed.as_bytes(), Some(SIGNATURE.as_bytes())),
            Err(SignatureError::Mismatch)
        );
        for url in [
            &format!("{URL}/"),
            &format!("{URL}?a=1"),
            &URL.replace("https:", "http:"),
            &URL.replace("webhook.site", "Webhook.site"),
        ] {
            assert_eq!(
                key().verify(url, BODY.as_bytes(), Some(SIGNATURE.as_bytes())),
                Err(SignatureError::Mismatch),
                "{url}"
            );
        }
        let other = WebhookKey::new(Secret::new("Ibxx_5AKakO-3qeNVR61Dx"));
        assert_eq!(
            other.verify(URL, BODY.as_bytes(), Some(SIGNATURE.as_bytes())),
            Err(SignatureError::Mismatch)
        );
    }

    #[test]
    fn a_missing_malformed_or_near_miss_signature_is_refused() {
        assert_eq!(
            key().verify(URL, BODY.as_bytes(), None),
            Err(SignatureError::Missing)
        );
        assert_eq!(
            key().verify(URL, BODY.as_bytes(), Some(b"not base64!")),
            Err(SignatureError::NotBase64)
        );
        let tag = STANDARD.decode(SIGNATURE).unwrap();
        // Every one-bit-off tag, and every truncation.
        for at in 0..tag.len() {
            let mut near = tag.clone();
            near[at] ^= 1;
            let near = STANDARD.encode(&near);
            assert_eq!(
                key().verify(URL, BODY.as_bytes(), Some(near.as_bytes())),
                Err(SignatureError::Mismatch)
            );
            let short = STANDARD.encode(&tag[..at]);
            assert_eq!(
                key().verify(URL, BODY.as_bytes(), Some(short.as_bytes())),
                Err(SignatureError::Mismatch)
            );
        }
    }

    fn payment(kind: &str, status: &str, location: &str) -> String {
        serde_json::json!({
            "merchant_id": "6SSW7HV8K2ST5",
            "type": kind,
            "event_id": "13b867cf-db3d-4b1c-90b6-2f32a9d78124",
            "created_at": "2026-10-27T10:00:00.000Z",
            "data": {
                "type": "payment",
                "id": "KkAkhdMsgzn59SM8A89WgKwekxLZY",
                "object": {"payment": {
                    "id": "KkAkhdMsgzn59SM8A89WgKwekxLZY",
                    "amount_money": {"amount": 2450, "currency": "USD"},
                    "card_details": {"card": {"last_4": "1111", "card_brand": "VISA"}},
                    "location_id": location,
                    "status": status,
                }},
            },
        })
        .to_string()
    }

    #[test]
    fn a_completed_payment_names_its_merchant_and_location() {
        for kind in ["payment.created", "payment.updated"] {
            assert_eq!(
                read_notification(payment(kind, "COMPLETED", "S8GWD5R9QB376").as_bytes()),
                Ok(Notification::Completed {
                    payment_id: PaymentId::try_from("KkAkhdMsgzn59SM8A89WgKwekxLZY".to_owned())
                        .unwrap(),
                    merchant_id: MerchantId::try_from("6SSW7HV8K2ST5".to_owned()).unwrap(),
                    location_id: LocationId::try_from("S8GWD5R9QB376".to_owned()).unwrap(),
                }),
                "{kind}"
            );
        }
    }

    #[test]
    fn other_payments_and_other_events_are_ignored() {
        for status in ["APPROVED", "PENDING", "CANCELED", "FAILED"] {
            assert_eq!(
                read_notification(payment("payment.updated", status, "S8GWD5R9QB376").as_bytes()),
                Ok(Notification::Ignored),
                "{status}"
            );
        }
        // A refund's payment.updated: still COMPLETED, now refunded.
        let mut refunded: serde_json::Value =
            serde_json::from_str(&payment("payment.updated", "COMPLETED", "S8GWD5R9QB376"))
                .unwrap();
        refunded["data"]["object"]["payment"]["refunded_money"] =
            serde_json::json!({"amount": 2450, "currency": "USD"});
        assert_eq!(
            read_notification(refunded.to_string().as_bytes()),
            Ok(Notification::Ignored)
        );
        // Read by its type alone: the rest needn't be a payment.
        for kind in [
            "refund.created",
            "order.updated",
            "webhooks.test_notification",
        ] {
            assert_eq!(
                read_notification(BODY.replace("webhooks.test_notification", kind).as_bytes()),
                Ok(Notification::Ignored),
                "{kind}"
            );
        }
    }

    /// A body that doesn't read is told by where it stopped, never by what
    /// was there.
    #[test]
    fn an_unreadable_body_is_described_without_its_contents() {
        let bad = payment("payment.created", "COMPLETED", "not/a/location");
        let error = read_notification(bad.as_bytes()).unwrap_err().to_string();
        assert!(!error.contains("not/a/location"), "{error}");
        assert!(error.contains("not the shape expected"), "{error}");
        let error = read_notification(b"{\"type\": \"payment.created\", 4111}")
            .unwrap_err()
            .to_string();
        assert!(!error.contains("4111"), "{error}");
        assert!(error.contains("not JSON"), "{error}");
    }
}
