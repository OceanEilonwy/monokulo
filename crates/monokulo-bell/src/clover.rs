//! The Clover adapter: what the Bell has to do with Clover. Check a
//! webhook's `X-Clover-Auth` header, answer Clover's one-off verification
//! handshake, read the routing fields of a webhook, and check, with a
//! merchant's access token, that a merchant is the token's.
//!
//! A Clover webhook carries ids and times only: the app, each merchant, and
//! each update's object (`P:<payment>`) and type. None of it is kept,
//! logged or put into an error, here or by any caller; what is read is
//! read into types that hold exactly that. The verification code is the
//! one exception: Clover's handshake value, logged once for the operator.
//!
//! Clover has no location below a merchant, so a Clover merchant is its own
//! one location (decision 48).

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use live_settings::Secret;
use reqwest::StatusCode;
use serde::Deserialize;
use subtle::ConstantTimeEq as _;

use crate::pos::{
    self, AccessToken, BodyError, LocationId, MerchantId, PaymentId, PointOfSale, ProofError, Ring,
};
use crate::settings::{CloverApplicationId, CloverEnvironment, ServiceUrl};

/// The header carrying the app's Auth Code on every webhook.
pub const AUTH_HEADER: &str = "x-clover-auth";

/// The Clover app's Auth Code (`MONOKULAR_BELL_CLOVER_AUTH_CODE`): the
/// whole of a webhook's authentication, as Clover signs nothing (decision 49).
#[derive(Clone, PartialEq)]
pub struct AuthCode(Secret);

/// Why a webhook's `X-Clover-Auth` header was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("no {AUTH_HEADER} header")]
    Missing,
    #[error("the {AUTH_HEADER} header isn't this Bell's Clover Auth Code")]
    Mismatch,
}

impl AuthCode {
    pub fn new(code: Secret) -> Self {
        AuthCode(code)
    }

    /// Checks `header` (the header's value) is the Auth Code byte for byte,
    /// compared in constant time: the time taken says nothing of how much
    /// of it matched.
    pub fn verify(&self, header: Option<&[u8]>) -> Result<(), AuthError> {
        let header = header.ok_or(AuthError::Missing)?;
        // `ct_eq` on slices of different lengths is false, at once: the
        // length is no secret, as every Auth Code is a UUID.
        if bool::from(header.ct_eq(self.0.expose().as_bytes())) {
            Ok(())
        } else {
            Err(AuthError::Mismatch)
        }
    }
}

/// The code of Clover's verification handshake: exactly a UUID (8-4-4-4-12
/// hex digits), so what the Bell logs can be nothing else (acceptance D4).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct VerificationCode(String);

impl TryFrom<String> for VerificationCode {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let uuid = text.len() == 36
            && text.bytes().enumerate().all(|(at, byte)| match at {
                8 | 13 | 18 | 23 => byte == b'-',
                _ => byte.is_ascii_hexdigit(),
            });
        if uuid {
            Ok(VerificationCode(text))
        } else {
            Err("not a verification code (a UUID)")
        }
    }
}

impl fmt::Display for VerificationCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a webhook body is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// Clover's one-off `{"verificationCode": "…"}`, sent when the operator
    /// presses Send Verification Code in the developer dashboard.
    Verification(VerificationCode),
    /// A batch of updates of this Bell's app: the payments it asks to ring,
    /// none or more.
    Updates(Vec<Ring>),
    /// A batch of another app's updates: dropped (not authentication, as
    /// the Auth Code is, but another app's batch is never this Bell's).
    OtherApplication,
}

/// The handshake's body: that one field, nothing else.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Verification {
    #[serde(rename = "verificationCode")]
    code: VerificationCode,
}

/// The fields of a batch the Bell reads; serde skips the rest (each
/// update's `ts`) without keeping it.
#[derive(Deserialize)]
struct Batch {
    #[serde(rename = "appId")]
    app_id: String,
    merchants: HashMap<MerchantId, Vec<Update>>,
}

#[derive(Deserialize)]
struct Update {
    #[serde(rename = "objectId")]
    object: Object,
    #[serde(rename = "type")]
    kind: UpdateType,
}

/// An update's object, by its prefix: a payment's id is read, any other
/// object's is not.
#[derive(Deserialize)]
#[serde(try_from = "String")]
enum Object {
    Payment(PaymentId),
    Other,
}

impl TryFrom<String> for Object {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        match text.strip_prefix("P:") {
            Some(id) => PaymentId::try_from(id.to_owned()).map(Object::Payment),
            None => Ok(Object::Other),
        }
    }
}

#[derive(Deserialize)]
enum UpdateType {
    #[serde(rename = "CREATE")]
    Create,
    #[serde(other)]
    Other,
}

/// What `body` is: the handshake, if it is exactly one; else a batch, of
/// which a payment's `CREATE` rings, at its merchant, which is its own
/// location (decision 50). Updates, deletes and every other object are dropped:
/// with no status in the body, a payment's update could be a refund or a
/// void days later. A batch of another app than `application_id` is
/// [`Incoming::OtherApplication`].
pub fn read_webhook(
    body: &[u8],
    application_id: &CloverApplicationId,
) -> Result<Incoming, BodyError> {
    if let Some(code) = read_verification(body) {
        return Ok(Incoming::Verification(code));
    }
    let batch: Batch = serde_json::from_slice(body)?;
    // Compared as text: an app's id is public, the Auth Code the secret.
    if batch.app_id != application_id.as_str() {
        return Ok(Incoming::OtherApplication);
    }
    let mut rings = Vec::new();
    for (merchant_id, updates) in batch.merchants {
        for update in updates {
            if let (Object::Payment(payment_id), UpdateType::Create) = (update.object, update.kind)
            {
                rings.push(Ring {
                    point_of_sale: PointOfSale::Clover,
                    payment_id,
                    location_id: LocationId::from(merchant_id.clone()),
                    merchant_id: merchant_id.clone(),
                });
            }
        }
    }
    Ok(Incoming::Updates(rings))
}

/// Whether `body` is Clover's verification handshake: what a webhook
/// without the header may be, and nothing else.
pub fn read_verification(body: &[u8]) -> Option<VerificationCode> {
    // An object: serde would read a struct from a one-element array too.
    if body.trim_ascii_start().first() != Some(&b'{') {
        return None;
    }
    serde_json::from_slice::<Verification>(body)
        .ok()
        .map(|verification| verification.code)
}

/// The Clover adapter: the app whose webhooks this Bell takes, its Auth
/// Code, and Clover's API.
pub struct Clover {
    client: reqwest::Client,
    api: ServiceUrl,
    application_id: CloverApplicationId,
    environment: CloverEnvironment,
    auth_code: AuthCode,
}

/// A merchant as Clover answers for it: its id alone is read (the owner's
/// name and email beside it land in no type).
#[derive(Deserialize)]
struct Merchant {
    id: MerchantId,
}

impl Clover {
    /// The adapter for `application_id` in `environment`, its webhooks
    /// carrying `auth_code`, asking Clover's API at `api` and waiting
    /// `timeout` for each answer. Fails only if TLS can't be set up.
    pub fn new(
        api: ServiceUrl,
        application_id: CloverApplicationId,
        environment: CloverEnvironment,
        auth_code: AuthCode,
        timeout: Duration,
    ) -> reqwest::Result<Self> {
        Ok(Clover {
            client: pos::api_client(timeout)?,
            api,
            application_id,
            environment,
            auth_code,
        })
    }

    pub fn application_id(&self) -> &CloverApplicationId {
        &self.application_id
    }

    pub fn environment(&self) -> CloverEnvironment {
        self.environment
    }

    /// Checks a webhook's `X-Clover-Auth` header.
    pub fn verify(&self, header: Option<&[u8]>) -> Result<(), AuthError> {
        self.auth_code.verify(header)
    }

    /// What a webhook's `body`, its header checked, is.
    pub fn read_webhook(&self, body: &[u8]) -> Result<Incoming, BodyError> {
        read_webhook(body, &self.application_id)
    }

    /// Checks `merchant` is `token`'s, asked of Clover
    /// (`GET /v3/merchants/{id}`, decision 55): a token is one merchant's, and Clover
    /// answers for that merchant alone, so the answer proves both. The
    /// token is dropped once the request is sent.
    pub async fn prove(&self, token: AccessToken, merchant: &MerchantId) -> Result<(), ProofError> {
        let response = self
            .client
            .get(self.api.join(&format!("/v3/merchants/{merchant}")))
            .bearer_auth(token.expose())
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await;
        drop(token);
        let response =
            response.map_err(|error| ProofError::Unavailable(pos::request_error(error)))?;
        let status = response.status();
        if status != StatusCode::OK {
            return Err(ProofError::from_status(status, PointOfSale::Clover));
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| ProofError::Unavailable(pos::request_error(error)))?;
        let answer: Merchant = serde_json::from_slice(&body).map_err(|error| {
            ProofError::Unavailable(format!(
                "Clover's answer isn't a merchant: {}",
                BodyError::from(error)
            ))
        })?;
        if answer.id != *merchant {
            return Err(ProofError::NotById);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use live_settings::SettingValue as _;

    const CODE: &str = "9f2d4a8c-6b1e-4f3a-9c7d-2e5b8a1f4c6d";

    fn code() -> AuthCode {
        AuthCode::new(Secret::new(CODE))
    }

    fn app() -> CloverApplicationId {
        CloverApplicationId::parse("MONOKULARBELL").unwrap()
    }

    #[test]
    fn the_exact_auth_code_is_accepted() {
        assert_eq!(code().verify(Some(CODE.as_bytes())), Ok(()));
    }

    /// Missing, empty, a prefix, longer, padded, another case, or any one
    /// byte off: refused.
    #[test]
    fn anything_but_the_auth_code_is_refused() {
        assert_eq!(code().verify(None), Err(AuthError::Missing));
        let mut near = vec![
            String::new(),
            CODE[..35].to_owned(),
            CODE[..1].to_owned(),
            format!("{CODE} "),
            format!(" {CODE}"),
            format!("{CODE}0"),
            CODE.to_uppercase(),
        ];
        for at in 0..CODE.len() {
            let mut bytes = CODE.as_bytes().to_vec();
            bytes[at] ^= 1;
            near.push(String::from_utf8(bytes).unwrap());
        }
        for value in near {
            assert_eq!(
                code().verify(Some(value.as_bytes())),
                Err(AuthError::Mismatch),
                "{value:?}"
            );
        }
    }

    fn batch(app_id: &str, merchants: serde_json::Value) -> String {
        serde_json::json!({"appId": app_id, "merchants": merchants}).to_string()
    }

    fn ring(merchant: &str, payment: &str) -> Ring {
        Ring {
            point_of_sale: PointOfSale::Clover,
            payment_id: PaymentId::try_from(payment.to_owned()).unwrap(),
            merchant_id: MerchantId::try_from(merchant.to_owned()).unwrap(),
            location_id: LocationId::try_from(merchant.to_owned()).unwrap(),
        }
    }

    /// Clover's documented shape: a payment created at a merchant is a ring
    /// there, the merchant its own location.
    #[test]
    fn a_created_payment_rings_at_its_merchant() {
        let body = batch(
            "MONOKULARBELL",
            serde_json::json!({"CLVMERCHANT01": [{"objectId": "P:PAYMENT00001", "type": "CREATE", "ts": 1_761_600_000_000_u64}]}),
        );
        assert_eq!(
            read_webhook(body.as_bytes(), &app()),
            Ok(Incoming::Updates(vec![ring(
                "CLVMERCHANT01",
                "PAYMENT00001"
            )]))
        );
    }

    /// A batch of several merchants and updates rings each payment created,
    /// at its own merchant, and nothing else.
    #[test]
    fn a_batch_rings_only_its_created_payments() {
        let body = batch(
            "MONOKULARBELL",
            serde_json::json!({
                "MERCHANTAAAAA": [
                    {"objectId": "P:PAYMENTA0001", "type": "CREATE", "ts": 1},
                    {"objectId": "P:PAYMENTA0002", "type": "UPDATE", "ts": 2},
                    {"objectId": "O:ORDERA000001", "type": "CREATE", "ts": 3},
                ],
                "MERCHANTBBBBB": [
                    {"objectId": "P:PAYMENTB0001", "type": "CREATE", "ts": 4},
                    {"objectId": "P:PAYMENTB0002", "type": "DELETE", "ts": 5},
                    {"objectId": "A:APPAAAAAAAAA", "type": "CREATE", "ts": 6},
                    {"objectId": "P:PAYMENTB0003", "type": "CREATE", "ts": 7},
                ],
            }),
        );
        let Ok(Incoming::Updates(mut rings)) = read_webhook(body.as_bytes(), &app()) else {
            panic!("a batch");
        };
        // A batch's merchants come in no order.
        rings.sort_by_key(|ring| format!("{:?}", ring.payment_id));
        assert_eq!(
            rings,
            [
                ring("MERCHANTAAAAA", "PAYMENTA0001"),
                ring("MERCHANTBBBBB", "PAYMENTB0001"),
                ring("MERCHANTBBBBB", "PAYMENTB0003"),
            ]
        );
    }

    #[test]
    fn updates_deletes_and_other_objects_ring_nothing() {
        for (object, kind) in [
            ("P:PAYMENT00001", "UPDATE"),
            ("P:PAYMENT00001", "DELETE"),
            ("P:PAYMENT00001", "SOMETHING"),
            ("O:ORDER0000001", "CREATE"),
            ("M:MERCHANT0001", "CREATE"),
            ("CA:CASH0000001", "CREATE"),
            // Another object's id isn't read.
            ("O:not/an id at all", "CREATE"),
        ] {
            let body = batch(
                "MONOKULARBELL",
                serde_json::json!({"CLVMERCHANT01": [{"objectId": object, "type": kind, "ts": 1}]}),
            );
            assert_eq!(
                read_webhook(body.as_bytes(), &app()),
                Ok(Incoming::Updates(Vec::new())),
                "{object} {kind}"
            );
        }
    }

    #[test]
    fn another_apps_batch_is_dropped() {
        let body = batch(
            "OTHERAPP00001",
            serde_json::json!({"CLVMERCHANT01": [{"objectId": "P:PAYMENT00001", "type": "CREATE", "ts": 1}]}),
        );
        assert_eq!(
            read_webhook(body.as_bytes(), &app()),
            Ok(Incoming::OtherApplication)
        );
    }

    /// The handshake is exactly `{"verificationCode": <a UUID>}`; any other
    /// value doesn't read as one, so it can't reach the log.
    #[test]
    fn the_verification_handshake_is_exactly_a_uuid() {
        let body = format!("{{\"verificationCode\":\"{CODE}\"}}");
        let expected = VerificationCode::try_from(CODE.to_owned()).unwrap();
        assert_eq!(read_verification(body.as_bytes()), Some(expected.clone()));
        assert_eq!(
            read_webhook(body.as_bytes(), &app()),
            Ok(Incoming::Verification(expected))
        );
        for bad in [
            "{\"verificationCode\":\"<script>alert(1)</script>\"}".to_owned(),
            "{\"verificationCode\":\"9f2d4a8c-6b1e-4f3a-9c7d-2e5b8a1f4c6\"}".to_owned(),
            "{\"verificationCode\":\"9f2d4a8c-6b1e-4f3a-9c7d-2e5b8a1f4c6dd\"}".to_owned(),
            "{\"verificationCode\":\"9f2d4a8c 6b1e-4f3a-9c7d-2e5b8a1f4c6d\"}".to_owned(),
            "{\"verificationCode\":\"9f2d4a8c-6b1e-4f3a-9c7d-2e5b8a1f4c6z\"}".to_owned(),
            "{\"verificationCode\":\"9f2d4a8c-6b1e-4f3a-9c7d-2e5b8a1f4c6d\n\"}".to_owned(),
            "{\"verificationCode\":42}".to_owned(),
            format!("{{\"verificationCode\":\"{CODE}\",\"note\":\"hello\"}}"),
            format!("[\"{CODE}\"]"),
            String::new(),
        ] {
            assert_eq!(read_verification(bad.as_bytes()), None, "{bad}");
        }
    }

    /// A body that doesn't read is told by where it stopped, never by what
    /// was there.
    #[test]
    fn an_unreadable_body_is_described_without_its_contents() {
        let body = batch(
            "MONOKULARBELL",
            serde_json::json!({"CLVMERCHANT01": [{"objectId": "P:not/a/payment", "type": "CREATE"}]}),
        );
        let error = read_webhook(body.as_bytes(), &app())
            .unwrap_err()
            .to_string();
        assert!(!error.contains("not/a/payment"), "{error}");
        assert!(error.contains("not the shape expected"), "{error}");
        let body = batch(
            "MONOKULARBELL",
            serde_json::json!({"not/a/merchant": [{"objectId": "P:PAYMENT00001", "type": "CREATE"}]}),
        );
        let error = read_webhook(body.as_bytes(), &app())
            .unwrap_err()
            .to_string();
        assert!(!error.contains("not/a/merchant"), "{error}");
        let error = read_webhook(b"{\"appId\": \"MONOKULARBELL\", 4111}", &app())
            .unwrap_err()
            .to_string();
        assert!(!error.contains("4111"), "{error}");
        assert!(error.contains("not JSON"), "{error}");
    }
}
