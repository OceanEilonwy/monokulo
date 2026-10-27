//! FCM, Firebase Cloud Messaging's HTTP v1 API: one `POST` to
//! `/v1/projects/{project}/messages:send` per push, a data message the app
//! decrypts, authorised by an OAuth access token.
//!
//! The token comes from the service account given in
//! `MONOKULO_RELAY_FCM_CREDENTIALS` and nowhere else: an RS256-signed
//! assertion (RFC 7523) exchanged at the account's `token_uri`, kept until
//! five minutes before it expires. No credentials are looked for in the
//! environment, gcloud's files or a metadata server.
//!
//! Each push has one budget, `server.upstream_timeout_ms`, for waiting on
//! another push's exchange, its own, and the send, so its sender hears
//! the relay's 503 within it. An exchange that fails is remembered for
//! [`FAILURE_HOLD`], the pushes meanwhile answered 503 at once.

use std::fmt;
use std::time::Duration;

use tokio::time::Instant;

use axum::http::StatusCode;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use push::wire::PushRequest;
use push::{Message, Urgency};
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey as _;
use rsa::sha2::Sha256;
use rsa::signature::{SignatureEncoding as _, Signer as _};
use rsa::RsaPrivateKey;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::handles::{Handle, HandleId, Platform};
use crate::settings::{FcmConfig, ProjectId, ServiceUrl};
use crate::upstream::{self, Delivery};

/// The OAuth scope that sends messages.
const SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";

/// How long the assertion asks for a token for: Google's maximum.
const ASSERTION_LIFETIME: i64 = 3600;

/// A token is renewed this long before it expires.
pub const RENEW_BEFORE: i64 = 5 * 60;

/// Seconds a failed token exchange is remembered: pushes meanwhile are
/// answered 503, to come back once it has passed, rather than each
/// waiting out an exchange of its own.
pub const FAILURE_HOLD: u64 = upstream::DEFAULT_RETRY_AFTER;

/// The most FCM carries in a message's data.
pub const MAX_DATA: usize = 4096;

/// The longest FCM keeps a message for an offline device: 28 days.
const MAX_TTL: u32 = 28 * 86_400;

/// A Firebase service account, as Google's JSON key file has it: the
/// parts that make an access token.
#[derive(Clone, PartialEq)]
pub struct ServiceAccount {
    client_email: String,
    private_key_id: String,
    token_uri: ServiceUrl,
    key: RsaPrivateKey,
}

/// The account names itself; never its key.
impl fmt::Debug for ServiceAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceAccount")
            .field("client_email", &self.client_email)
            .field("token_uri", &self.token_uri)
            .finish_non_exhaustive()
    }
}

/// Why a service account's JSON can't be used. Never the value itself,
/// nor serde_json's message about it, which quotes a refused string in
/// full: the whole key file, private key and all, when it was encoded as
/// a JSON string once more. This goes into the start-up error and the
/// log.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AccountError {
    #[error("isn't a service account's JSON key file: {0}")]
    Json(JsonError),
    #[error("is a JSON string, not a key file: the key file is encoded as JSON twice, so give the file's text as it is")]
    EncodedTwice,
    /// The account's `type`, when it is a short name such as
    /// `authorized_user`; anything else is not named.
    #[error("is a {} key, not a service account's", .0.as_deref().map_or("different kind of".to_owned(), |kind| format!("{kind:?}")))]
    Type(Option<String>),
    #[error("its token_uri: {0}")]
    TokenUri(String),
    #[error("its private_key isn't an RSA key in PKCS #8 PEM")]
    Key,
}

/// Where and how a key file's JSON went wrong, as `serde_json` classifies
/// it, without its message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{category} (line {line}, column {column})")]
pub struct JsonError {
    category: &'static str,
    line: usize,
    column: usize,
}

impl From<serde_json::Error> for JsonError {
    fn from(error: serde_json::Error) -> Self {
        JsonError {
            category: match error.classify() {
                serde_json::error::Category::Io => "unreadable",
                serde_json::error::Category::Syntax => "not JSON",
                serde_json::error::Category::Data => "not a key file's shape",
                serde_json::error::Category::Eof => "cut short",
            },
            line: error.line(),
            column: error.column(),
        }
    }
}

#[derive(Deserialize)]
struct AccountFile {
    #[serde(rename = "type")]
    kind: String,
    client_email: String,
    private_key_id: String,
    private_key: String,
    token_uri: String,
}

impl ServiceAccount {
    pub fn parse(json: &str) -> Result<Self, AccountError> {
        if json.trim_start().starts_with('"') {
            return Err(AccountError::EncodedTwice);
        }
        let file: AccountFile = serde_json::from_str(json)
            .map_err(|error| AccountError::Json(JsonError::from(error)))?;
        if file.kind != "service_account" {
            let named = (1..=32).contains(&file.kind.len())
                && file
                    .kind
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_');
            return Err(AccountError::Type(named.then_some(file.kind)));
        }
        let token_uri = <ServiceUrl as live_settings::SettingValue>::parse(&file.token_uri)
            .map_err(AccountError::TokenUri)?;
        let key =
            RsaPrivateKey::from_pkcs8_pem(&file.private_key).map_err(|_| AccountError::Key)?;
        Ok(ServiceAccount {
            client_email: file.client_email,
            private_key_id: file.private_key_id,
            token_uri,
            key,
        })
    }

    /// The signed assertion asking for a token at `now`.
    fn assertion(&self, now: i64) -> Result<String, serde_json::Error> {
        #[derive(Serialize)]
        struct Header<'a> {
            alg: &'static str,
            typ: &'static str,
            kid: &'a str,
        }
        #[derive(Serialize)]
        struct Claims<'a> {
            iss: &'a str,
            scope: &'static str,
            aud: &'a str,
            iat: i64,
            exp: i64,
        }
        let header = serde_json::to_vec(&Header {
            alg: "RS256",
            typ: "JWT",
            kid: &self.private_key_id,
        })?;
        let claims = serde_json::to_vec(&Claims {
            iss: &self.client_email,
            scope: SCOPE,
            aud: self.token_uri.url().as_str(),
            iat: now,
            exp: now.saturating_add(ASSERTION_LIFETIME),
        })?;
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header),
            URL_SAFE_NO_PAD.encode(claims)
        );
        let signature = SigningKey::<Sha256>::new(self.key.clone()).sign(input.as_bytes());
        Ok(format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ))
    }
}

/// One request to FCM, as [`translate`] makes it from a push.
#[derive(Clone, PartialEq, Eq)]
pub struct FcmPush {
    pub body: Vec<u8>,
}

/// The body holds the device token and the ciphertext: `Debug` gives only
/// its length.
impl fmt::Debug for FcmPush {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FcmPush({} bytes)", self.body.len())
    }
}

/// Why a push can't go to an FCM handle as it is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TranslateError {
    #[error("fcm handles take aes128gcm ciphertext (Content-Encoding: aes128gcm)")]
    WrongBody,
    #[error("{0} bytes of data once carried to FCM, more than its {MAX_DATA}")]
    TooLarge(usize),
}

/// The FCM message for `request` to `handle`: a data message, high
/// priority for a high-urgency push and normal otherwise, kept as long
/// as the push's TTL allows (at most FCM's 28 days), collapsed by its
/// topic.
pub fn translate(handle: &Handle, request: &PushRequest) -> Result<FcmPush, TranslateError> {
    let Message::Encrypted(encrypted) = &request.message else {
        return Err(TranslateError::WrongBody);
    };
    debug_assert_eq!(handle.platform, Platform::Fcm);
    let ciphertext = URL_SAFE_NO_PAD.encode(encrypted.as_bytes());
    let data_size = "aes128gcm".len() + ciphertext.len();
    if data_size > MAX_DATA {
        return Err(TranslateError::TooLarge(data_size));
    }
    let options = &request.options;
    let mut android = json!({
        "priority": if options.urgency == Urgency::High { "HIGH" } else { "NORMAL" },
        "ttl": format!("{}s", options.ttl.as_secs().min(MAX_TTL)),
    });
    if let Some(topic) = &options.topic {
        android["collapse_key"] = topic.as_str().into();
    }
    let message = json!({"message": {
        "token": handle.token.as_str(),
        "android": android,
        "data": {"aes128gcm": ciphertext},
    }});
    Ok(FcmPush {
        body: message.to_string().into_bytes(),
    })
}

/// An access token, and when it stops working (Unix seconds).
struct AccessToken {
    value: String,
    expires_at: i64,
}

/// What the relay knows of its access token.
enum TokenState {
    /// None yet, or the last one refused.
    Unknown,
    Held(AccessToken),
    /// The last exchange failed; none is tried before `retry_at` (Unix
    /// seconds).
    Failed {
        retry_at: i64,
    },
}

/// What the token endpoint answers.
#[derive(Deserialize)]
struct TokenAnswer {
    access_token: String,
    expires_in: i64,
}

/// What FCM answers when it refuses a message: its status, and the FCM
/// error code in its details.
#[derive(Deserialize)]
struct ErrorAnswer {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    status: String,
    #[serde(default)]
    details: Vec<ErrorDetail>,
}

#[derive(Deserialize)]
struct ErrorDetail {
    #[serde(rename = "errorCode", default)]
    error_code: Option<String>,
}

/// FCM, reached with this relay's service account.
pub struct Fcm {
    client: reqwest::Client,
    endpoint: ServiceUrl,
    account: ServiceAccount,
    /// Each push's budget, `server.upstream_timeout_ms`.
    timeout: Duration,
    /// Held across a token exchange, so pushes at once wait for one
    /// exchange rather than each making its own.
    token: tokio::sync::Mutex<TokenState>,
}

/// Why no access token could be had.
#[derive(Debug, thiserror::Error)]
enum TokenError {
    #[error("the token endpoint can't be reached: {0}")]
    Unreachable(reqwest::Error),
    #[error("the token endpoint answered {0}")]
    Refused(StatusCode),
    #[error("the token endpoint's answer doesn't read: {0}")]
    Answer(String),
    #[error("the token endpoint didn't answer within the upstream timeout")]
    TimedOut,
    #[error("another push's token exchange took the whole upstream timeout")]
    Waited,
    #[error("the last token exchange failed; the next is tried in {retry_after} s")]
    Failing { retry_after: u64 },
}

impl TokenError {
    /// Seconds until a push may find a token: what is left of a failure's
    /// hold, or the whole of it for the exchange that failed (or is still
    /// running).
    fn retry_after(&self) -> u64 {
        match self {
            TokenError::Failing { retry_after } => *retry_after,
            _ => FAILURE_HOLD,
        }
    }
}

impl Fcm {
    pub fn new(config: &FcmConfig, timeout: Duration) -> Result<Self, reqwest::Error> {
        Ok(Fcm {
            client: reqwest::Client::builder().timeout(timeout).build()?,
            endpoint: config.endpoint.clone(),
            account: config.account.clone(),
            timeout,
            token: tokio::sync::Mutex::new(TokenState::Unknown),
        })
    }

    /// An access token good at `now`, by `deadline`: the last one until
    /// [`RENEW_BEFORE`] its expiry, else a new one from the token endpoint,
    /// unless the last exchange failed under [`FAILURE_HOLD`] ago. An
    /// exchange that fails or runs past `deadline` is remembered as failed.
    async fn access_token(&self, now: i64, deadline: Instant) -> Result<String, TokenError> {
        let mut state = tokio::time::timeout_at(deadline, self.token.lock())
            .await
            .map_err(|_| TokenError::Waited)?;
        match &*state {
            TokenState::Held(token) if now < token.expires_at - RENEW_BEFORE => {
                return Ok(token.value.clone());
            }
            TokenState::Failed { retry_at } if now < *retry_at => {
                return Err(TokenError::Failing {
                    retry_after: retry_at.abs_diff(now),
                });
            }
            _ => {}
        }
        let exchanged = tokio::time::timeout_at(deadline, self.exchange(now))
            .await
            .unwrap_or(Err(TokenError::TimedOut));
        match exchanged {
            Ok(token) => {
                let value = token.value.clone();
                *state = TokenState::Held(token);
                Ok(value)
            }
            Err(error) => {
                *state = TokenState::Failed {
                    retry_at: now.saturating_add_unsigned(FAILURE_HOLD),
                };
                Err(error)
            }
        }
    }

    /// A new access token from the token endpoint, for an assertion signed
    /// at `now`.
    async fn exchange(&self, now: i64) -> Result<AccessToken, TokenError> {
        let assertion = self
            .account
            .assertion(now)
            .map_err(|error| TokenError::Answer(error.to_string()))?;
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer")
            .append_pair("assertion", &assertion)
            .finish();
        let response = self
            .client
            .post(self.account.token_uri.url().as_str())
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form)
            .send()
            .await
            .map_err(|error| TokenError::Unreachable(error.without_url()))?;
        if !response.status().is_success() {
            return Err(TokenError::Refused(response.status()));
        }
        let answer: TokenAnswer = serde_json::from_slice(&upstream::error_body(response).await)
            .map_err(|error| TokenError::Answer(error.to_string()))?;
        Ok(AccessToken {
            value: answer.access_token,
            expires_at: now.saturating_add(answer.expires_in),
        })
    }

    /// Forgets the access token when FCM refused it.
    async fn forget(&self, used: &str) {
        let mut state = self.token.lock().await;
        if matches!(&*state, TokenState::Held(token) if token.value == used) {
            *state = TokenState::Unknown;
        }
    }

    /// Hands `push` to FCM in the project `project`, and says what became
    /// of it.
    pub async fn send(
        &self,
        handle: &Handle,
        project: &ProjectId,
        push: &FcmPush,
        now: i64,
    ) -> Delivery {
        let deadline = Instant::now() + self.timeout;
        let token = match self.access_token(now, deadline).await {
            Ok(token) => token,
            Err(error) => {
                tracing::warn!(handle = %handle.id.tag(), error = %error, "no FCM access token");
                return Delivery::Unavailable {
                    retry_after: error.retry_after(),
                };
            }
        };
        let url = self
            .endpoint
            .join(&format!("/v1/projects/{}/messages:send", project.as_str()));
        let response = match self
            .client
            .post(url)
            .bearer_auth(&token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(push.body.clone())
            .timeout(deadline.saturating_duration_since(Instant::now()))
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(handle = %handle.id.tag(), error = %error.without_url(), "FCM can't be reached");
                return Delivery::Unavailable {
                    retry_after: upstream::DEFAULT_RETRY_AFTER,
                };
            }
        };
        let status = response.status();
        if status.is_success() {
            return Delivery::Delivered;
        }
        let retry_after = upstream::retry_after(response.headers(), std::time::SystemTime::now());
        let (reason, code) =
            match serde_json::from_slice::<ErrorAnswer>(&upstream::error_body(response).await) {
                Ok(answer) => (
                    answer.error.status,
                    answer
                        .error
                        .details
                        .into_iter()
                        .find_map(|detail| detail.error_code)
                        .unwrap_or_default(),
                ),
                Err(_) => (String::new(), String::new()),
            };
        if status == StatusCode::UNAUTHORIZED {
            self.forget(&token).await;
        }
        let delivery = outcome(status, &reason, &code, retry_after);
        log_refusal(&handle.id, status, &reason, &code, delivery);
        delivery
    }
}

/// What an FCM answer other than success means.
fn outcome(status: StatusCode, reason: &str, code: &str, retry_after: u64) -> Delivery {
    if code == "UNREGISTERED" || reason == "NOT_FOUND" || status == StatusCode::NOT_FOUND {
        return Delivery::Gone;
    }
    match status.as_u16() {
        // The access token was refused: a new one for the next push.
        401 | 429 | 500..=599 => Delivery::Unavailable { retry_after },
        413 => Delivery::TooLarge,
        _ => Delivery::Refused,
    }
}

fn log_refusal(
    handle: &HandleId,
    status: StatusCode,
    reason: &str,
    code: &str,
    delivery: Delivery,
) {
    match delivery {
        Delivery::Gone => {
            tracing::info!(handle = %handle.tag(), status = status.as_u16(), reason, code, "FCM says the token is gone: forgetting the handle");
        }
        Delivery::Refused => {
            tracing::error!(handle = %handle.tag(), status = status.as_u16(), reason, code, "FCM refused a push: check the relay's FCM settings");
        }
        Delivery::TooLarge | Delivery::Unavailable { .. } | Delivery::Delivered => {
            tracing::warn!(handle = %handle.tag(), status = status.as_u16(), reason, code, "FCM didn't take a push");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handles::{AppName, DeviceToken};
    use crate::settings::tests::{fcm_account, fcm_config, GOOGLE_TOKEN_URI};
    use push::{encrypt, Options, Topic, Ttl, VapidKey};
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::signature::Verifier as _;
    use serde_json::Value;

    const NOW: i64 = 1_800_000_000;

    fn handle() -> Handle {
        Handle {
            id: "ph_0123456789abcdef0123456789abcdef".parse().unwrap(),
            platform: Platform::Fcm,
            app: AppName::try_from("monokular".to_owned()).unwrap(),
            token: DeviceToken::parse(Platform::Fcm, "fcm:device-1").unwrap(),
            sender: VapidKey::generate().public(),
            expires_at: NOW + 60,
        }
    }

    fn request(message: Message, urgency: Urgency, ttl: u32, topic: Option<&str>) -> PushRequest {
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
                topic: topic.map(|topic| Topic::parse(topic).unwrap()),
            },
            message,
        }
    }

    fn encrypted(size: usize) -> push::Encrypted {
        let keys = push::encrypt::ReceiverKeys::generate();
        encrypt(&keys.subscription_keys(), &vec![b'x'; size]).unwrap()
    }

    #[test]
    fn a_push_is_a_data_message_with_its_priority_ttl_and_collapse_key() {
        let ciphertext = encrypted(20);
        let carried = URL_SAFE_NO_PAD.encode(ciphertext.as_bytes());
        let push = translate(
            &handle(),
            &request(
                Message::Encrypted(ciphertext),
                Urgency::High,
                1800,
                Some("sale-1"),
            ),
        )
        .unwrap();
        let body: Value = serde_json::from_slice(&push.body).unwrap();
        assert_eq!(
            body,
            json!({"message": {
                "token": "fcm:device-1",
                "android": {"priority": "HIGH", "ttl": "1800s", "collapse_key": "sale-1"},
                "data": {"aes128gcm": carried},
            }})
        );
        let normal = translate(
            &handle(),
            &request(
                Message::Encrypted(encrypted(20)),
                Urgency::Low,
                u32::MAX,
                None,
            ),
        )
        .unwrap();
        let body: Value = serde_json::from_slice(&normal.body).unwrap();
        assert_eq!(
            body["message"]["android"],
            json!({"priority": "NORMAL", "ttl": "2419200s"})
        );
    }

    #[test]
    fn only_ciphertext_that_fits_goes_to_fcm() {
        let plain = Message::PlainJson(push::PlainJson::new(&json!({"phase": "seen"})).unwrap());
        assert_eq!(
            translate(&handle(), &request(plain, Urgency::Normal, 60, None)),
            Err(TranslateError::WrongBody)
        );
        // 3000 bytes of ciphertext is 4000 of base64, past FCM's 4096 with
        // the key once the header and tag are in.
        assert!(matches!(
            translate(
                &handle(),
                &request(Message::Encrypted(encrypted(3000)), Urgency::High, 60, None)
            ),
            Err(TranslateError::TooLarge(_))
        ));
    }

    #[test]
    fn the_assertion_is_an_rs256_jwt_for_the_token_endpoint() {
        let account = ServiceAccount::parse(&fcm_account(GOOGLE_TOKEN_URI)).unwrap();
        let assertion = account.assertion(NOW).unwrap();
        let mut parts = assertion.split('.');
        let (header, claims, signature) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        let read = |part: &str| -> Value {
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
        };
        assert_eq!(
            read(header),
            json!({"alg": "RS256", "typ": "JWT", "kid": "0123456789abcdef0123456789abcdef01234567"})
        );
        assert_eq!(
            read(claims),
            json!({
                "iss": "relay@monokular-test.iam.gserviceaccount.com",
                "scope": "https://www.googleapis.com/auth/firebase.messaging",
                "aud": "https://oauth2.googleapis.com/token",
                "iat": NOW,
                "exp": NOW + 3600,
            })
        );
        let signature =
            Signature::try_from(URL_SAFE_NO_PAD.decode(signature).unwrap().as_slice()).unwrap();
        VerifyingKey::<Sha256>::new(account.key.to_public_key())
            .verify(format!("{header}.{claims}").as_bytes(), &signature)
            .unwrap();
        assert!(!format!("{account:?}").contains("PRIVATE"));
    }

    #[test]
    fn a_service_account_is_checked_where_it_is_read() {
        assert!(matches!(
            ServiceAccount::parse("{}"),
            Err(AccountError::Json(_))
        ));
        let mut account: Value = serde_json::from_str(&fcm_account(GOOGLE_TOKEN_URI)).unwrap();
        account["type"] = "authorized_user".into();
        assert_eq!(
            ServiceAccount::parse(&account.to_string()),
            Err(AccountError::Type(Some("authorized_user".into())))
        );
        account["type"] = "-----BEGIN PRIVATE KEY-----".into();
        assert_eq!(
            ServiceAccount::parse(&account.to_string()),
            Err(AccountError::Type(None))
        );
        let mut account: Value = serde_json::from_str(&fcm_account(GOOGLE_TOKEN_URI)).unwrap();
        account["token_uri"] = "http://oauth2.example.com/token".into();
        assert!(matches!(
            ServiceAccount::parse(&account.to_string()),
            Err(AccountError::TokenUri(_))
        ));
        let mut account: Value = serde_json::from_str(&fcm_account(GOOGLE_TOKEN_URI)).unwrap();
        account["private_key"] =
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n".into();
        assert_eq!(
            ServiceAccount::parse(&account.to_string()),
            Err(AccountError::Key)
        );
    }

    /// A key file encoded as a JSON string once more (a secret store or
    /// a template's `jsonencode`) is refused without a word of it: the
    /// error goes into the start-up error and the log.
    #[test]
    fn a_key_file_encoded_twice_is_refused_without_quoting_it() {
        let file = fcm_account(GOOGLE_TOKEN_URI);
        let pem = include_str!("../tests/fixtures/fcm-test-key.pem");
        let quiet = |error: &AccountError| {
            let text = error.to_string();
            assert!(!text.contains("PRIVATE KEY"), "{text}");
            for line in pem.lines().filter(|line| line.len() > 8) {
                assert!(!text.contains(line), "{text}");
            }
            assert!(!text.contains("relay@monokular-test"), "{text}");
        };
        let twice = serde_json::to_string(&file).unwrap();
        let error = ServiceAccount::parse(&twice).unwrap_err();
        assert_eq!(error, AccountError::EncodedTwice);
        quiet(&error);
        // A field of the wrong type, or broken JSON, is placed, not quoted.
        let mut account: Value = serde_json::from_str(&file).unwrap();
        account["client_email"] = json!([pem]);
        let error = ServiceAccount::parse(&account.to_string()).unwrap_err();
        assert!(matches!(error, AccountError::Json(_)), "{error}");
        assert!(error.to_string().contains("shape"), "{error}");
        quiet(&error);
        let error = ServiceAccount::parse(&file[..file.len() / 2]).unwrap_err();
        assert!(error.to_string().contains("line 1"), "{error}");
        quiet(&error);
    }

    #[test]
    fn fcm_answers_map_to_what_the_sender_is_told() {
        let status = |code| StatusCode::from_u16(code).unwrap();
        assert_eq!(
            outcome(status(404), "NOT_FOUND", "UNREGISTERED", 30),
            Delivery::Gone
        );
        assert_eq!(
            outcome(status(400), "INVALID_ARGUMENT", "UNREGISTERED", 30),
            Delivery::Gone
        );
        assert_eq!(outcome(status(404), "", "", 30), Delivery::Gone);
        for code in [401, 429, 500, 503] {
            assert_eq!(
                outcome(status(code), "UNAVAILABLE", "", 9),
                Delivery::Unavailable { retry_after: 9 },
                "{code}"
            );
        }
        assert_eq!(
            outcome(status(400), "INVALID_ARGUMENT", "INVALID_ARGUMENT", 9),
            Delivery::Refused
        );
        assert_eq!(
            outcome(status(403), "PERMISSION_DENIED", "SENDER_ID_MISMATCH", 9),
            Delivery::Refused
        );
    }

    /// The token is reused until five minutes before it expires; the one
    /// after that comes from the endpoint, which here can't be reached, and
    /// that failure is remembered for [`FAILURE_HOLD`].
    #[tokio::test]
    async fn an_access_token_is_reused_until_five_minutes_before_it_expires() {
        let fcm = Fcm::new(&fcm_config(), Duration::from_millis(200)).unwrap();
        let held = || {
            TokenState::Held(AccessToken {
                value: "cached".into(),
                expires_at: NOW + 3600,
            })
        };
        let token = |now: i64| fcm.access_token(now, Instant::now() + Duration::from_millis(200));
        *fcm.token.lock().await = held();
        assert_eq!(token(NOW).await.unwrap(), "cached");
        assert_eq!(
            token(NOW + 3600 - RENEW_BEFORE - 1).await.unwrap(),
            "cached"
        );
        let renewal = NOW + 3600 - RENEW_BEFORE;
        assert!(matches!(
            token(renewal).await,
            Err(TokenError::Unreachable(_))
        ));
        assert!(matches!(
            token(renewal + 10).await,
            Err(TokenError::Failing { retry_after: 20 })
        ));
        let hold = i64::try_from(FAILURE_HOLD).unwrap();
        assert!(matches!(
            token(renewal + hold).await,
            Err(TokenError::Unreachable(_))
        ));
        *fcm.token.lock().await = held();
        fcm.forget("another").await;
        assert!(matches!(&*fcm.token.lock().await, TokenState::Held(_)));
        fcm.forget("cached").await;
        assert!(matches!(&*fcm.token.lock().await, TokenState::Unknown));
    }
}
