//! APNs, the Apple Push Notification service: one HTTP/2 `POST` to
//! `/3/device/{token}` per push, with token authentication (an ES256 JWT
//! from the team's `.p8` key, reused for 50 minutes: Apple refuses one
//! older than an hour, and one renewed more than every 20 minutes).
//!
//! A push becomes one of two requests ([`translate`]):
//!
//! - `apns`: an alert with `mutable-content`, the ciphertext beside `aps`
//!   for the app's Notification Service Extension to decrypt and replace
//!   the fallback title ("New sale") with.
//! - `apns-liveactivity`: the plain JSON as the Live Activity's
//!   `content-state`, always an update: an alert for `paid` and
//!   `expired`, and a stale date while the sale is open. The relay never
//!   ends an activity: there is one per shift, which only the app idles or
//!   ends, and the app can't start one again from the background.
//!
//! No push-to-start: nothing sends one yet (decisions/push.md 157).

use std::time::Duration;

use axum::http::StatusCode;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use parking_lot::Mutex;
use push::jwt::{CompactJwt, Header, JwtError};
use push::wire::PushRequest;
use push::{Message, PlainJson, Urgency};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::handles::{Handle, HandleId, Platform};
use crate::settings::{ApnsConfig, ApnsKey, AppleId, BundleId, ServiceUrl};
use crate::upstream::{self, Delivery};

/// How long a provider token is reused.
pub const TOKEN_REUSE: i64 = 50 * 60;

/// How far the clock may go back (NTP stepping it, say) with the provider
/// token still reused: further is a clock set back, which signs anew
/// rather than reuse a token from its future. Apple answers a token
/// renewed more than once every 20 minutes with 429.
pub const CLOCK_SKEW: i64 = 60;

/// The largest payload APNs takes for a notification or a Live Activity.
pub const MAX_PAYLOAD: usize = 4096;

/// The title an `apns` alert shows until the extension has decrypted the
/// push, and when it can't.
pub const FALLBACK_TITLE: &str = "New sale";

/// What `apns-push-type` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushType {
    Alert,
    LiveActivity,
}

impl PushType {
    pub const fn as_str(self) -> &'static str {
        match self {
            PushType::Alert => "alert",
            PushType::LiveActivity => "liveactivity",
        }
    }
}

/// One request to APNs, as [`translate`] makes it from a push.
#[derive(Clone, PartialEq, Eq)]
pub struct ApnsPush {
    pub push_type: PushType,
    /// `apns-topic`: the bundle ID, with `.push-type.liveactivity` for a
    /// Live Activity.
    pub topic: String,
    /// `apns-priority`: 10 for a high-urgency push, 5 (as power allows)
    /// for the rest.
    pub priority: u8,
    /// `apns-expiration`: Unix seconds, or 0 for "now or never" (a TTL of
    /// 0).
    pub expiration: i64,
    /// `apns-collapse-id`: the push's topic, so a newer state replaces an
    /// older one still waiting.
    pub collapse_id: Option<String>,
    pub body: Vec<u8>,
}

/// The body is a push's content (a sale's amount, an address): `Debug`
/// gives only its length.
impl std::fmt::Debug for ApnsPush {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApnsPush")
            .field("push_type", &self.push_type)
            .field("topic", &self.topic)
            .field("priority", &self.priority)
            .field("expiration", &self.expiration)
            .field("collapse_id", &self.collapse_id)
            .field("body", &format_args!("{} bytes", self.body.len()))
            .finish()
    }
}

/// Why a push can't go to an APNs handle as it is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TranslateError {
    /// An `apns` handle takes ciphertext, a Live Activity's plain JSON.
    #[error("{platform} handles take {expected}")]
    WrongBody {
        platform: Platform,
        expected: &'static str,
    },
    #[error("the JSON isn't a Live Activity's state: {0}")]
    State(String),
    #[error("{0} bytes once carried to APNs, more than its {MAX_PAYLOAD}")]
    TooLarge(usize),
}

/// A Live Activity's phase, the one field of its state the relay reads to
/// decide how APNs is told about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    Waiting,
    Seen,
    Confirming,
    Paid,
    Expired,
}

/// What the relay reads of a Live Activity's state; the rest is the app's
/// and goes to APNs as it came.
#[derive(Deserialize)]
struct State {
    phase: Phase,
    /// Unix seconds: when the sale's code stops being valid, after which
    /// iOS shows the activity as stale.
    #[serde(rename = "expiresAt", default)]
    expires_at: Option<i64>,
    /// Shown in the alert when the sale is paid, with the currency when
    /// the state names one apart (monokulo sends `"24.50"` and `"USD"`,
    /// not `"$24.50"`: no server formats money for the device's locale).
    #[serde(default)]
    amount: Option<String>,
    #[serde(default)]
    currency: Option<String>,
}

impl State {
    /// The alert's title for a paid sale.
    fn paid_title(&self) -> String {
        match (&self.amount, &self.currency) {
            (Some(amount), Some(currency)) => format!("Paid · {amount} {currency}"),
            (Some(amount), None) => format!("Paid · {amount}"),
            (None, _) => "Paid".to_owned(),
        }
    }
}

/// The APNs request for `request` to `handle` of an app whose bundle ID is
/// `bundle`, at `now` (Unix seconds).
pub fn translate(
    handle: &Handle,
    bundle: &BundleId,
    request: &PushRequest,
    now: i64,
) -> Result<ApnsPush, TranslateError> {
    let options = &request.options;
    let ttl = i64::from(options.ttl.as_secs());
    let mut push = ApnsPush {
        push_type: PushType::Alert,
        topic: bundle.as_str().to_owned(),
        priority: if options.urgency == Urgency::High {
            10
        } else {
            5
        },
        expiration: if ttl == 0 { 0 } else { now.saturating_add(ttl) },
        collapse_id: options
            .topic
            .as_ref()
            .map(|topic| topic.as_str().to_owned()),
        body: Vec::new(),
    };
    let payload = match (handle.platform, &request.message) {
        (Platform::Apns, Message::Encrypted(encrypted)) => json!({
            "aps": {"alert": {"title": FALLBACK_TITLE}, "mutable-content": 1},
            "aes128gcm": URL_SAFE_NO_PAD.encode(encrypted.as_bytes()),
        }),
        (Platform::ApnsLiveActivity, Message::PlainJson(json)) => {
            push.push_type = PushType::LiveActivity;
            push.topic = live_activity_topic(bundle);
            live_activity_update(json, now)?
        }
        (platform @ Platform::Apns, Message::PlainJson(_)) => {
            return Err(TranslateError::WrongBody {
                platform,
                expected: "aes128gcm ciphertext (Content-Encoding: aes128gcm)",
            })
        }
        (platform, _) => {
            return Err(TranslateError::WrongBody {
                platform,
                expected: "a Live Activity's state as JSON (Content-Type: application/json)",
            })
        }
    };
    push.body = payload.to_string().into_bytes();
    if push.body.len() > MAX_PAYLOAD {
        return Err(TranslateError::TooLarge(push.body.len()));
    }
    Ok(push)
}

fn live_activity_topic(bundle: &BundleId) -> String {
    format!("{}.push-type.liveactivity", bundle.as_str())
}

/// An update to a running activity; a paid or expired sale is told with
/// an alert, never by ending the activity.
fn live_activity_update(json: &PlainJson, now: i64) -> Result<Value, TranslateError> {
    let content: serde_json::Map<String, Value> = serde_json::from_slice(json.as_bytes())
        .map_err(|error| TranslateError::State(error.to_string()))?;
    let state: State = serde_json::from_value(Value::Object(content.clone()))
        .map_err(|error| TranslateError::State(error.to_string()))?;
    let mut aps = serde_json::Map::new();
    aps.insert("timestamp".into(), now.into());
    aps.insert("event".into(), "update".into());
    aps.insert("content-state".into(), Value::Object(content));
    match state.phase {
        Phase::Paid => {
            aps.insert("alert".into(), json!({"title": state.paid_title()}));
        }
        Phase::Expired => {
            aps.insert("alert".into(), json!({"title": "No payment yet"}));
        }
        Phase::Waiting | Phase::Seen | Phase::Confirming => {
            if let Some(expires_at) = state.expires_at {
                aps.insert("stale-date".into(), expires_at.into());
            }
        }
    }
    Ok(json!({ "aps": aps }))
}

/// The JWT a request is authorised with, and when it was signed.
struct ProviderToken {
    issued_at: i64,
    jwt: CompactJwt,
}

/// APNs, reached with this relay's key.
pub struct Apns {
    client: reqwest::Client,
    endpoint: ServiceUrl,
    team_id: AppleId,
    key_id: AppleId,
    key: ApnsKey,
    token: Mutex<Option<ProviderToken>>,
}

#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    iat: i64,
}

#[derive(Deserialize)]
struct Reason {
    reason: String,
}

impl Apns {
    /// A client for `config`'s APNs, every request given `timeout`. HTTP/2
    /// only, as APNs is: negotiated over TLS, or with prior knowledge to a
    /// stand-in on this machine.
    pub fn new(config: &ApnsConfig, timeout: Duration) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .http2_prior_knowledge()
            .timeout(timeout)
            .build()?;
        Ok(Apns {
            client,
            endpoint: config.endpoint.clone(),
            team_id: config.team_id.clone(),
            key_id: config.key_id.clone(),
            key: config.key.clone(),
            token: Mutex::new(None),
        })
    }

    /// The provider token for a request now, as `clock` reads it (Unix
    /// seconds): the last one while it is younger than [`TOKEN_REUSE`] and
    /// no more than [`CLOCK_SKEW`] in the future, else a new one. The clock
    /// is read under the lock, not when the request came in, so requests
    /// reaching here out of their arrival order still renew the token once.
    pub fn token(&self, clock: impl FnOnce() -> i64) -> Result<CompactJwt, JwtError> {
        let mut cached = self.token.lock();
        let now = clock();
        if let Some(token) = cached.as_ref().filter(|token| {
            (token.issued_at - CLOCK_SKEW..token.issued_at + TOKEN_REUSE).contains(&now)
        }) {
            return Ok(token.jwt.clone());
        }
        let jwt = self.key.signer().sign(
            &Header {
                typ: None,
                kid: Some(self.key_id.as_str()),
            },
            &Claims {
                iss: self.team_id.as_str(),
                iat: now,
            },
        )?;
        *cached = Some(ProviderToken {
            issued_at: now,
            jwt: jwt.clone(),
        });
        Ok(jwt)
    }

    /// Forgets `used` when APNs refused it, so the next push signs anew.
    fn forget(&self, used: &CompactJwt) {
        let mut cached = self.token.lock();
        if cached.as_ref().is_some_and(|token| token.jwt == *used) {
            *cached = None;
        }
    }

    /// Hands `push` to APNs for `handle`, and says what became of it.
    pub async fn send(&self, handle: &Handle, push: &ApnsPush) -> Delivery {
        let token = match self.token(service_runtime::time::now_unix) {
            Ok(token) => token,
            Err(error) => {
                tracing::error!(error = %error, "the APNs provider token can't be signed");
                return Delivery::Refused;
            }
        };
        let url = self
            .endpoint
            .join(&format!("/3/device/{}", handle.token.as_str()));
        let mut request = self
            .client
            .post(url)
            .bearer_auth(token.as_str())
            .header("apns-push-type", push.push_type.as_str())
            .header("apns-topic", &push.topic)
            .header("apns-priority", push.priority.to_string())
            .header("apns-expiration", push.expiration.to_string())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(push.body.clone());
        if let Some(collapse_id) = &push.collapse_id {
            request = request.header("apns-collapse-id", collapse_id);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                // Without the URL: it carries the device token.
                tracing::warn!(handle = %handle.id.tag(), error = %error.without_url(), "APNs can't be reached");
                return Delivery::Unavailable {
                    retry_after: upstream::DEFAULT_RETRY_AFTER,
                };
            }
        };
        let status = response.status();
        if status == StatusCode::OK {
            return Delivery::Delivered;
        }
        let retry_after = upstream::retry_after(response.headers(), std::time::SystemTime::now());
        let reason = serde_json::from_slice::<Reason>(&upstream::error_body(response).await)
            .map(|reason| reason.reason)
            .unwrap_or_default();
        let delivery = outcome(status, &reason, retry_after);
        if matches!(
            reason.as_str(),
            "ExpiredProviderToken" | "InvalidProviderToken"
        ) {
            self.forget(&token);
        }
        log_refusal(&handle.id, status, &reason, delivery);
        delivery
    }
}

/// What an APNs answer other than `200` means.
fn outcome(status: StatusCode, reason: &str, retry_after: u64) -> Delivery {
    match (status.as_u16(), reason) {
        (410, _) | (400, "BadDeviceToken") => Delivery::Gone,
        (413, _) => Delivery::TooLarge,
        // A token signed with a key APNs doesn't take (yet): signed anew
        // for the next push, which may be in time.
        (403, "ExpiredProviderToken" | "InvalidProviderToken") | (429, _) | (500..=599, _) => {
            Delivery::Unavailable { retry_after }
        }
        _ => Delivery::Refused,
    }
}

fn log_refusal(handle: &HandleId, status: StatusCode, reason: &str, delivery: Delivery) {
    match delivery {
        Delivery::Gone => {
            tracing::info!(handle = %handle.tag(), status = status.as_u16(), reason, "APNs says the token is gone: forgetting the handle");
        }
        Delivery::Refused => {
            tracing::error!(handle = %handle.tag(), status = status.as_u16(), reason, "APNs refused a push: check the relay's APNs settings");
        }
        Delivery::TooLarge | Delivery::Unavailable { .. } | Delivery::Delivered => {
            tracing::warn!(handle = %handle.tag(), status = status.as_u16(), reason, "APNs didn't take a push");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handles::{AppName, DeviceToken};
    use crate::settings::tests::apns_config;
    use push::{encrypt, Options, Topic, Ttl, VapidKey};

    const NOW: i64 = 1_800_000_000;

    fn handle(platform: Platform) -> Handle {
        Handle {
            id: "ph_0123456789abcdef0123456789abcdef".parse().unwrap(),
            platform,
            app: AppName::try_from("monokular".to_owned()).unwrap(),
            token: DeviceToken::parse(platform, &"ab".repeat(32)).unwrap(),
            sender: VapidKey::generate().public(),
            expires_at: NOW + 60,
        }
    }

    fn bundle() -> BundleId {
        BundleId::try_from("com.example.monokular".to_owned()).unwrap()
    }

    fn request(message: Message, urgency: Urgency, ttl: u32) -> PushRequest {
        PushRequest {
            authorization: VapidKey::generate()
                .authorization(
                    &push::Origin::parse("https://relay.example.org").unwrap(),
                    None,
                    NOW,
                )
                .unwrap(),
            options: Options {
                urgency,
                ttl: Ttl::seconds(ttl),
                topic: Some(Topic::parse("0123456789abcdef0123456789abcdef").unwrap()),
            },
            message,
        }
    }

    fn json(value: Value) -> Message {
        Message::PlainJson(PlainJson::new(&value).unwrap())
    }

    fn body(push: &ApnsPush) -> Value {
        serde_json::from_slice(&push.body).unwrap()
    }

    #[test]
    fn an_encrypted_push_is_an_alert_carrying_the_ciphertext() {
        let keys = push::encrypt::ReceiverKeys::generate();
        let encrypted = encrypt(&keys.subscription_keys(), br#"{"sale":"paid"}"#).unwrap();
        let carried = URL_SAFE_NO_PAD.encode(encrypted.as_bytes());
        let request = request(Message::Encrypted(encrypted), Urgency::High, 1800);
        let push = translate(&handle(Platform::Apns), &bundle(), &request, NOW).unwrap();
        assert_eq!(push.push_type, PushType::Alert);
        assert_eq!(push.topic, "com.example.monokular");
        assert_eq!(push.priority, 10);
        assert_eq!(push.expiration, NOW + 1800);
        assert_eq!(
            push.collapse_id.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(
            body(&push),
            json!({
                "aps": {"alert": {"title": "New sale"}, "mutable-content": 1},
                "aes128gcm": carried,
            })
        );
        // Anything below high is sent as power allows; a TTL of 0 is "now
        // or never".
        let low = super::tests::request(
            Message::Encrypted(encrypt(&keys.subscription_keys(), b"{}").unwrap()),
            Urgency::Low,
            0,
        );
        let push = translate(&handle(Platform::Apns), &bundle(), &low, NOW).unwrap();
        assert_eq!((push.priority, push.expiration), (5, 0));
    }

    #[test]
    fn a_live_activity_state_is_its_content_state() {
        let state = json!({
            "phase": "seen", "sale": "#1042", "amount": "$24.50", "xmr": "0.1421",
            "expiresAt": NOW + 900, "waitingCount": 0,
        });
        let request = request(json(state.clone()), Urgency::Low, 900);
        let push = translate(
            &handle(Platform::ApnsLiveActivity),
            &bundle(),
            &request,
            NOW,
        )
        .unwrap();
        assert_eq!(push.push_type, PushType::LiveActivity);
        assert_eq!(push.topic, "com.example.monokular.push-type.liveactivity");
        assert_eq!(push.priority, 5);
        assert_eq!(
            body(&push),
            json!({"aps": {
                "timestamp": NOW, "event": "update", "content-state": state,
                "stale-date": NOW + 900,
            }})
        );
    }

    #[test]
    fn a_paid_or_expired_sale_updates_the_activity_with_an_alert() {
        let paid = json!({"phase": "paid", "amount": "$24.50", "waitingCount": 0});
        let push = translate(
            &handle(Platform::ApnsLiveActivity),
            &bundle(),
            &request(json(paid.clone()), Urgency::High, 1800),
            NOW,
        )
        .unwrap();
        assert_eq!(push.priority, 10);
        assert_eq!(
            body(&push),
            json!({"aps": {
                "timestamp": NOW, "event": "update", "content-state": paid,
                "alert": {"title": "Paid · $24.50"},
            }})
        );
        // Monokulo's state names the currency apart from the amount.
        let paid =
            json!({"phase": "paid", "amount": "24.50", "currency": "USD", "waitingCount": 0});
        let push = translate(
            &handle(Platform::ApnsLiveActivity),
            &bundle(),
            &request(json(paid), Urgency::High, 1800),
            NOW,
        )
        .unwrap();
        assert_eq!(
            body(&push)["aps"]["alert"],
            json!({"title": "Paid · 24.50 USD"})
        );
        let expired = json!({"phase": "expired", "waitingCount": 2});
        let push = translate(
            &handle(Platform::ApnsLiveActivity),
            &bundle(),
            &request(json(expired.clone()), Urgency::High, 1800),
            NOW,
        )
        .unwrap();
        assert_eq!(
            body(&push),
            json!({"aps": {
                "timestamp": NOW, "event": "update", "content-state": expired,
                "alert": {"title": "No payment yet"},
            }})
        );
    }

    #[test]
    fn each_handle_takes_only_its_kind_of_body() {
        let keys = push::encrypt::ReceiverKeys::generate();
        let encrypted = Message::Encrypted(encrypt(&keys.subscription_keys(), b"{}").unwrap());
        let plain = json(json!({"phase": "seen"}));
        let wrong = |platform, message: &Message| {
            translate(
                &handle(platform),
                &bundle(),
                &request(message.clone(), Urgency::Normal, 60),
                NOW,
            )
        };
        assert!(matches!(
            wrong(Platform::Apns, &plain),
            Err(TranslateError::WrongBody { .. })
        ));
        assert!(matches!(
            wrong(Platform::ApnsLiveActivity, &encrypted),
            Err(TranslateError::WrongBody { .. })
        ));
        for state in [json!({"phase": "idle"}), json!({}), json!(["seen"])] {
            assert!(
                matches!(
                    wrong(Platform::ApnsLiveActivity, &json(state.clone())),
                    Err(TranslateError::State(_))
                ),
                "{state}"
            );
        }
    }

    /// A push that fits as Web Push can be too large once base64 and the
    /// APNs envelope are around it.
    #[test]
    fn a_push_too_large_for_apns_is_refused_before_it_is_sent() {
        let state = json!({"phase": "seen", "sale": "x".repeat(4050)});
        assert!(matches!(
            translate(
                &handle(Platform::ApnsLiveActivity),
                &bundle(),
                &request(json(state), Urgency::Low, 60),
                NOW,
            ),
            Err(TranslateError::TooLarge(_))
        ));
        let keys = push::encrypt::ReceiverKeys::generate();
        let large = encrypt(&keys.subscription_keys(), &[b'x'; 3900]).unwrap();
        assert!(matches!(
            translate(
                &handle(Platform::Apns),
                &bundle(),
                &request(Message::Encrypted(large), Urgency::High, 60),
                NOW,
            ),
            Err(TranslateError::TooLarge(_))
        ));
    }

    #[test]
    fn a_provider_token_is_reused_for_50_minutes_then_renewed() {
        let apns = Apns::new(&apns_config(), Duration::from_secs(5)).unwrap();
        let at = |now: i64| move || now;
        let first = apns.token(at(NOW)).unwrap();
        assert_eq!(apns.token(at(NOW + TOKEN_REUSE - 1)).unwrap(), first);
        let renewed = apns.token(at(NOW + TOKEN_REUSE)).unwrap();
        assert_ne!(renewed, first);
        assert_eq!(apns.token(at(NOW + TOKEN_REUSE + 60)).unwrap(), renewed);
        // A clock set back doesn't reuse a token from its future.
        assert_ne!(apns.token(at(NOW)).unwrap(), renewed);
        // Refused by APNs: signed anew, even within the 50 minutes.
        let current = apns.token(at(NOW)).unwrap();
        assert_eq!(apns.token(at(NOW + 1)).unwrap(), current);
        apns.forget(&current);
        assert_ne!(apns.token(at(NOW + 1)).unwrap(), current);
    }

    /// A clock a second (or up to a minute) behind the one that just
    /// renewed the token reuses it: two tokens in a second would have
    /// APNs answer 429 TooManyProviderTokenUpdates. A clock set back
    /// further still signs anew.
    #[test]
    fn a_clock_a_moment_behind_the_renewal_reuses_its_token() {
        let apns = Apns::new(&apns_config(), Duration::from_secs(5)).unwrap();
        let at = |now: i64| move || now;
        let renewed = apns.token(at(NOW)).unwrap();
        assert_eq!(apns.token(at(NOW - 1)).unwrap(), renewed);
        assert_eq!(apns.token(at(NOW - CLOCK_SKEW)).unwrap(), renewed);
        assert_ne!(apns.token(at(NOW - 3600)).unwrap(), renewed);
    }

    #[test]
    fn apns_answers_map_to_what_the_sender_is_told() {
        let status = |code| StatusCode::from_u16(code).unwrap();
        assert_eq!(outcome(status(410), "Unregistered", 30), Delivery::Gone);
        assert_eq!(outcome(status(400), "BadDeviceToken", 30), Delivery::Gone);
        assert_eq!(
            outcome(status(413), "PayloadTooLarge", 30),
            Delivery::TooLarge
        );
        for (code, reason) in [
            (429, "TooManyRequests"),
            (500, "InternalServerError"),
            (503, "ServiceUnavailable"),
            (403, "ExpiredProviderToken"),
        ] {
            assert_eq!(
                outcome(status(code), reason, 7),
                Delivery::Unavailable { retry_after: 7 },
                "{code} {reason}"
            );
        }
        for (code, reason) in [(400, "BadTopic"), (403, "BadCertificate"), (404, "BadPath")] {
            assert_eq!(outcome(status(code), reason, 7), Delivery::Refused);
        }
    }
}
