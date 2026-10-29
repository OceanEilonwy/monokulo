//! What the relay hands APNs and FCM, over real HTTP to stand-ins on
//! loopback (HTTP/2 with prior knowledge for APNs, as an `http`
//! `apns.endpoint` is reached), and what their answers become for the
//! sender. The routes themselves are called in-process, except in the
//! one test that serves the relay and pushes to it with `push::Sender`.

mod support;

use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, StatusCode, Version};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
use p256::pkcs8::DecodePrivateKey as _;
use push::encrypt::{decrypt, ReceiverKeys};
use push::{Endpoint, Options, SendError, Sender, Topic, Ttl, Urgency, VapidKey};
use serde_json::{json, Value};
use support::*;

fn now() -> i64 {
    service_runtime::time::now_unix()
}

/// A JWT's header and claims, once its ES256 signature verifies under the
/// test APNs key.
fn verified_provider_token(authorization: &str) -> (Value, Value) {
    let token = authorization
        .strip_prefix("Bearer ")
        .unwrap_or_else(|| panic!("{authorization}"));
    let mut parts = token.split('.');
    let (header, claims, signature) = (
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
    );
    let key = p256::SecretKey::from_pkcs8_pem(APNS_KEY).unwrap();
    let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(signature).unwrap()).unwrap();
    VerifyingKey::from(key.public_key())
        .verify(format!("{header}.{claims}").as_bytes(), &signature)
        .unwrap();
    let read = |part: &str| -> Value {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
    };
    (read(header), read(claims))
}

/// The ciphertext a JSON body carries as `aes128gcm`, decrypted with the
/// device's keys.
fn decrypted(keys: &ReceiverKeys, carried: &Value) -> Vec<u8> {
    let ciphertext = URL_SAFE_NO_PAD.decode(carried.as_str().unwrap()).unwrap();
    decrypt(keys, &ciphertext).unwrap()
}

fn within(value: i64, expected: i64) -> bool {
    (expected - 5..=expected + 5).contains(&value)
}

#[tokio::test]
async fn an_encrypted_push_reaches_apns_as_an_alert_over_http() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let endpoint = relay.register("apns", &apns_token(), &sender).await;
    let keys = ReceiverKeys::generate();
    let answered = relay
        .push(
            &endpoint,
            PushBuilder::new(&sender, encrypted(&keys, br#"{"sale":"paid"}"#)),
        )
        .await;
    assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.text());

    let received = apns.next().await;
    assert_eq!(received.version, Version::HTTP_2);
    assert_eq!(received.path, format!("/3/device/{}", apns_token()));
    let (header, claims) = verified_provider_token(received.header("authorization"));
    assert_eq!(header, json!({"alg": "ES256", "kid": "KEY1234567"}));
    assert_eq!(claims["iss"], "TEAM123456");
    assert!(within(claims["iat"].as_i64().unwrap(), now()));
    assert_eq!(received.header("apns-push-type"), "alert");
    assert_eq!(received.header("apns-topic"), BUNDLE);
    assert_eq!(received.header("apns-priority"), "10");
    let expiration: i64 = received.header("apns-expiration").parse().unwrap();
    assert!(within(expiration, now() + 1800), "{expiration}");
    assert_eq!(received.header("apns-collapse-id"), "sale-1042");
    let body = received.json();
    assert_eq!(
        body["aps"],
        json!({"alert": {"title": "New sale"}, "mutable-content": 1})
    );
    assert_eq!(decrypted(&keys, &body["aes128gcm"]), br#"{"sale":"paid"}"#);

    // The next push is authorised with the same provider token.
    let answered = relay
        .push(
            &endpoint,
            PushBuilder {
                urgency: Urgency::Normal,
                ..PushBuilder::new(&sender, encrypted(&keys, b"{}"))
            },
        )
        .await;
    assert_eq!(answered.status, StatusCode::CREATED);
    let again = apns.next().await;
    assert_eq!(
        again.header("authorization"),
        received.header("authorization")
    );
    assert_eq!(again.header("apns-priority"), "5");
}

/// Every state is an update, a paid sale's with the alert and no stale
/// date: the relay never ends the shift's activity, whatever waits.
#[tokio::test]
async fn a_live_activity_is_updated_then_told_of_the_payment_with_an_alert_over_http() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let endpoint = relay
        .register("apns-liveactivity", &apns_token(), &sender)
        .await;
    let expires_at = now() + 900;
    let seen = json!({
        "phase": "seen", "sale": "#1042", "amount": "$24.50", "xmr": "0.1421",
        "expiresAt": expires_at, "waitingCount": 0,
    });
    let answered = relay
        .push(
            &endpoint,
            PushBuilder {
                urgency: Urgency::Low,
                ..PushBuilder::new(&sender, plain(seen.clone()))
            },
        )
        .await;
    assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.text());
    let received = apns.next().await;
    assert_eq!(received.header("apns-push-type"), "liveactivity");
    assert_eq!(
        received.header("apns-topic"),
        "com.example.monokular.push-type.liveactivity"
    );
    assert_eq!(received.header("apns-priority"), "5");
    let body = received.json();
    assert!(within(body["aps"]["timestamp"].as_i64().unwrap(), now()));
    assert_eq!(body["aps"]["event"], "update");
    assert_eq!(body["aps"]["content-state"], seen);
    assert_eq!(body["aps"]["stale-date"], expires_at);
    assert_eq!(body["aps"].get("alert"), None);

    let paid = json!({"phase": "paid", "sale": "#1042", "amount": "$24.50", "waitingCount": 0});
    let answered = relay
        .push(&endpoint, PushBuilder::new(&sender, plain(paid.clone())))
        .await;
    assert_eq!(answered.status, StatusCode::CREATED);
    let aps = apns.next().await.json()["aps"].clone();
    assert_eq!(aps["event"], "update");
    assert_eq!(aps["content-state"], paid);
    assert_eq!(aps["alert"], json!({"title": "Paid · $24.50"}));
    assert_eq!(aps.get("stale-date"), None);
}

/// FCM gets a data message, its priority from the push's urgency; the
/// access token is fetched once and reused.
#[tokio::test]
async fn an_encrypted_push_reaches_fcm_with_its_priority_over_http() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let endpoint = relay.register("fcm", FCM_TOKEN, &sender).await;
    let keys = ReceiverKeys::generate();
    let answered = relay
        .push(
            &endpoint,
            PushBuilder::new(&sender, encrypted(&keys, br#"{"sale":"seen"}"#)),
        )
        .await;
    assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.text());

    let token_request = fcm.next().await;
    assert_eq!(token_request.path, "/token");
    let form: Vec<(String, String)> = url::form_urlencoded::parse(&token_request.body)
        .into_owned()
        .collect();
    assert_eq!(
        form[0],
        (
            "grant_type".to_owned(),
            "urn:ietf:params:oauth:grant-type:jwt-bearer".to_owned()
        )
    );
    let assertion = &form[1].1;
    let claims: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(assertion.split('.').nth(1).unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(claims["aud"], format!("{}/token", fcm.url()));
    assert_eq!(
        claims["scope"],
        "https://www.googleapis.com/auth/firebase.messaging"
    );

    let send = fcm.next().await;
    assert_eq!(send.path, "/v1/projects/monokular-test/messages:send");
    assert_eq!(
        send.header("authorization"),
        "Bearer ya29.test-access-token"
    );
    let message = &send.json()["message"];
    assert_eq!(message["token"], FCM_TOKEN);
    assert_eq!(
        message["android"],
        json!({"priority": "HIGH", "ttl": "1800s", "collapse_key": "sale-1042"})
    );
    assert_eq!(
        decrypted(&keys, &message["data"]["aes128gcm"]),
        br#"{"sale":"seen"}"#
    );

    let answered = relay
        .push(
            &endpoint,
            PushBuilder {
                urgency: Urgency::Normal,
                ttl: Some(Ttl::seconds(60)),
                topic: None,
                ..PushBuilder::new(&sender, encrypted(&keys, b"{}"))
            },
        )
        .await;
    assert_eq!(answered.status, StatusCode::CREATED);
    let send = fcm.next().await;
    assert_eq!(
        send.path, "/v1/projects/monokular-test/messages:send",
        "no second token"
    );
    assert_eq!(
        send.json()["message"]["android"],
        json!({"priority": "NORMAL", "ttl": "60s"})
    );
}

/// A push handed on is answered as one even when the handles database
/// then refuses to write: a `500` would have the sender send it again, and
/// the device show it twice. The failure is logged, with the handle.
#[tokio::test]
async fn a_push_handed_on_is_answered_so_when_the_handles_database_fails_over_http() {
    let (_guard, log) = capture_log();
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let endpoint = relay.register("apns", &apns_token(), &sender).await;
    let id = log_tag(&endpoint);
    // Reads still work; every write to the handles fails.
    rusqlite::Connection::open(&relay.store_path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER no_updates BEFORE UPDATE ON handles
                 BEGIN SELECT RAISE(FAIL, 'the disk is full'); END;
             CREATE TRIGGER no_deletes BEFORE DELETE ON handles
                 BEGIN SELECT RAISE(FAIL, 'the disk is full'); END;",
        )
        .unwrap();

    let answered = relay
        .push(
            &endpoint,
            PushBuilder::new(&sender, encrypted(&keys, b"{}")),
        )
        .await;
    assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.text());
    apns.next().await;
    let logged = log.text();
    let line = logged
        .lines()
        .find(|line| line.contains("can't be kept alive"))
        .unwrap_or_else(|| panic!("{logged}"));
    assert!(line.contains("ERROR") && line.contains(&id) && line.contains("the disk is full"));

    // Gone, but not forgotten: still 404 to the sender, and logged.
    apns.answer(|_| Answer::status(410).json(json!({"reason": "Unregistered"})));
    let answered = relay
        .push(
            &endpoint,
            PushBuilder::new(&sender, encrypted(&keys, b"{}")),
        )
        .await;
    assert_eq!(
        (answered.status, answered.problem().as_str()),
        (StatusCode::NOT_FOUND, "unknown-handle")
    );
    let logged = log.text();
    let line = logged
        .lines()
        .find(|line| line.contains("can't be forgotten"))
        .unwrap_or_else(|| panic!("{logged}"));
    assert!(line.contains("ERROR") && line.contains(&id));
    assert_eq!(relay.handles(), 1);
}

/// APNs or FCM saying the token is gone forgets the handle, and the
/// sender is told to drop its subscription.
#[tokio::test]
async fn a_token_apns_or_fcm_says_is_gone_deletes_the_handle_over_http() {
    let apns = Stub::start(|_| Answer::status(410).json(json!({"reason": "Unregistered"}))).await;
    let fcm = Stub::accepting().await;
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let to_apns = relay.register("apns", &apns_token(), &sender).await;
    let to_fcm = relay.register("fcm", FCM_TOKEN, &sender).await;
    assert_eq!(relay.handles(), 2);

    let answered = relay
        .push(&to_apns, PushBuilder::new(&sender, encrypted(&keys, b"{}")))
        .await;
    assert_eq!(
        (answered.status, answered.problem().as_str()),
        (StatusCode::NOT_FOUND, "unknown-handle")
    );
    assert_eq!(relay.handles(), 1);
    apns.next().await;
    // Gone for good: the next push doesn't reach APNs.
    let answered = relay
        .push(&to_apns, PushBuilder::new(&sender, encrypted(&keys, b"{}")))
        .await;
    assert_eq!(answered.status, StatusCode::NOT_FOUND);
    assert!(apns.drain().await.is_empty());

    // A bad token is gone too.
    apns.answer(|_| Answer::status(400).json(json!({"reason": "BadDeviceToken"})));
    let again = relay.register("apns", &apns_token(), &sender).await;
    let answered = relay
        .push(&again, PushBuilder::new(&sender, encrypted(&keys, b"{}")))
        .await;
    assert_eq!(answered.status, StatusCode::NOT_FOUND);

    fcm.answer(|received| {
        if received.path == "/token" {
            Answer::status(200).json(json!({"access_token": "t", "expires_in": 3600}))
        } else {
            Answer::status(404).json(json!({"error": {
                "code": 404, "status": "NOT_FOUND", "message": "Requested entity was not found.",
                "details": [{"@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError", "errorCode": "UNREGISTERED"}],
            }}))
        }
    });
    let answered = relay
        .push(&to_fcm, PushBuilder::new(&sender, encrypted(&keys, b"{}")))
        .await;
    assert_eq!(answered.status, StatusCode::NOT_FOUND);
    assert_eq!(relay.handles(), 0);
}

/// APNs or FCM busy, limiting or down: the sender is told to come back
/// when they said, or in 30 seconds; the handle stays.
#[tokio::test]
async fn an_unavailable_apns_or_fcm_is_upstream_unavailable_with_its_retry_after_over_http() {
    let apns = Stub::start(|_| {
        Answer::status(429)
            .json(json!({"reason": "TooManyRequests"}))
            .header("retry-after", "120")
    })
    .await;
    let fcm = Stub::start(|received| {
        if received.path == "/token" {
            Answer::status(200).json(json!({"access_token": "t", "expires_in": 3600}))
        } else {
            Answer::status(503)
                .json(json!({"error": {"code": 503, "status": "UNAVAILABLE"}}))
                .header("retry-after", "45")
        }
    })
    .await;
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let to_apns = relay.register("apns", &apns_token(), &sender).await;
    let to_fcm = relay.register("fcm", FCM_TOKEN, &sender).await;

    let unavailable = |answered: &Answered, wait: &str| {
        assert_eq!(
            answered.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{}",
            answered.text()
        );
        assert_eq!(answered.problem(), "upstream-unavailable");
        assert_eq!(answered.headers[header::RETRY_AFTER], wait);
    };
    let push = || PushBuilder::new(&sender, encrypted(&keys, b"{}"));
    unavailable(&relay.push(&to_apns, push()).await, "120");
    unavailable(&relay.push(&to_fcm, push()).await, "45");

    apns.answer(|_| Answer::status(500).json(json!({"reason": "InternalServerError"})));
    unavailable(&relay.push(&to_apns, push()).await, "30");

    // APNs refusing the relay's own settings is the relay's to fix.
    apns.answer(|_| Answer::status(400).json(json!({"reason": "BadTopic"})));
    let answered = relay.push(&to_apns, push()).await;
    assert_eq!(
        (answered.status, answered.problem().as_str()),
        (StatusCode::BAD_GATEWAY, "upstream-refused")
    );
    assert_eq!(relay.handles(), 2);
}

/// Pushes are limited per handle and per sender, once they verify.
#[tokio::test]
async fn pushes_past_a_limit_are_rate_limited_with_retry_after_over_http() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(
        &apns,
        &fcm,
        &[
            ("limits.per_handle_per_minute", "1"),
            ("limits.per_sender_per_minute", "2"),
        ],
    )
    .await;
    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let push = || PushBuilder::new(&sender, encrypted(&keys, b"{}"));
    let one = relay.register("apns", &apns_token(), &sender).await;
    let two = relay.register("apns", &"1f".repeat(32), &sender).await;
    let three = relay.register("apns", &"2f".repeat(32), &sender).await;

    assert_eq!(relay.push(&one, push()).await.status, StatusCode::CREATED);
    let refused = relay.push(&one, push()).await;
    assert_eq!(
        (refused.status, refused.problem().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "rate-limited")
    );
    assert!(refused.headers.contains_key(header::RETRY_AFTER));
    // The push handle one refused didn't count against the sender: it has
    // one left, for handle two.
    assert_eq!(relay.push(&two, push()).await.status, StatusCode::CREATED);
    let refused = relay.push(&three, push()).await;
    assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(refused.json()["detail"]
        .as_str()
        .unwrap()
        .contains("sender"));
    // Another sender isn't held back by this one.
    let other = Arc::new(VapidKey::generate());
    let four = relay.register("apns", &"3f".repeat(32), &other).await;
    assert_eq!(
        relay
            .push(&four, PushBuilder::new(&other, encrypted(&keys, b"{}")))
            .await
            .status,
        StatusCode::CREATED
    );
    assert_eq!(apns.drain().await.len(), 3);
}

/// The relay served on a socket, as both binaries serve it: an app
/// registers over HTTP, and a sender's `push::Sender` (what monokulo and
/// the Bell push with) reaches APNs through it, then hears the token is
/// gone.
#[tokio::test]
async fn a_senders_push_reaches_apns_through_a_served_relay_over_http() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let relay = TestRelay::at(&origin, &apns, &fcm, &[]).await;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = relay.relay.clone();
    let served = tokio::spawn(async move {
        serving
            .serve(listener, async {
                let _ = stopped.await;
            })
            .await
    });

    let client = reqwest::Client::new();
    let key = Arc::new(VapidKey::generate());
    let registered: Value = serde_json::from_slice(
        &client
            .post(format!("{origin}/v1/register"))
            .header("content-type", "application/json")
            .body(
                json!({
                    "platform": "apns", "token": apns_token(), "app": APP,
                    "vapid_public_key": key.public().to_base64url(),
                })
                .to_string(),
            )
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
    )
    .unwrap();
    let endpoint = Endpoint::parse(registered["endpoint"].as_str().unwrap()).unwrap();
    assert_eq!(endpoint.origin().as_str(), origin);

    let sender = Sender::new(reqwest::Client::new(), key, None);
    let keys = ReceiverKeys::generate();
    let options = Options {
        urgency: Urgency::High,
        ttl: Ttl::seconds(60),
        topic: Some(Topic::parse("wake").unwrap()),
    };
    let sent = sender
        .send(
            &endpoint,
            &encrypted(&keys, br#"{"wake":"square"}"#),
            &options,
            now(),
        )
        .await
        .unwrap();
    assert_eq!(sent.status, StatusCode::CREATED);
    let received = apns.next().await;
    assert_eq!(
        decrypted(&keys, &received.json()["aes128gcm"]),
        br#"{"wake":"square"}"#
    );

    apns.answer(|_| Answer::status(410).json(json!({"reason": "Unregistered"})));
    let gone = sender
        .send(&endpoint, &encrypted(&keys, b"{}"), &options, now())
        .await;
    assert!(matches!(gone, Err(SendError::Gone)), "{gone:?}");

    let _ = stop.send(());
    tokio::time::timeout(Duration::from_secs(10), served)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// Registering and pushing to every platform, with every outcome, leaves
/// no device token, endpoint, VAPID token or push content in the log, at
/// any level.
#[tokio::test]
async fn the_log_carries_no_token_endpoint_or_body_over_http() {
    let (_guard, log) = capture_log();
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let secret_state =
        json!({"phase": "seen", "amount": "$987.65", "paymentURI": "monero:4SecretAddress"});
    let mut endpoints = Vec::new();
    for (platform, token) in [
        ("apns", apns_token()),
        ("apns-liveactivity", "4e".repeat(32)),
        ("fcm", FCM_TOKEN.to_owned()),
    ] {
        let endpoint = relay.register(platform, &token, &sender).await;
        let message = if platform == "apns-liveactivity" {
            plain(secret_state.clone())
        } else {
            encrypted(&keys, b"secret plaintext")
        };
        assert_eq!(
            relay
                .push(&endpoint, PushBuilder::new(&sender, message))
                .await
                .status,
            StatusCode::CREATED
        );
        endpoints.push(endpoint);
    }
    apns.answer(|_| Answer::status(410).json(json!({"reason": "Unregistered"})));
    relay
        .push(
            &endpoints[0],
            PushBuilder::new(&sender, encrypted(&keys, b"secret plaintext")),
        )
        .await;

    let text = log.text();
    assert!(text.contains("registered"), "the relay logged: {text}");
    assert!(
        text.contains(&log_tag(&endpoints[0])),
        "lines name the handle by its tag: {text}"
    );
    // Never by its id: with a copy of the handles file, that opens its row.
    for endpoint in &endpoints {
        let id = endpoint.rsplit('/').next().unwrap();
        assert!(!text.contains(&id[3..]), "{id} is in the log: {text}");
    }
    for secret in [
        apns_token().as_str(),
        "4e4e4e4e4e4e4e4e",
        FCM_TOKEN,
        "ya29.test-access-token",
        "$987.65",
        "4SecretAddress",
        "secret plaintext",
        "/v1/push/ph_",
        "eyJ",
    ] {
        assert!(!text.contains(secret), "{secret} is in the log: {text}");
    }
}

/// A handle lives for `handles.ttl_days` after its last delivered push:
/// one APNs or FCM took moves its expiry on, one they didn't take leaves
/// it where it was (so holding an endpoint can't keep a dead one alive).
#[tokio::test]
async fn only_a_delivered_push_moves_a_handles_expiry_over_http() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let endpoint = relay.register("apns", &apns_token(), &sender).await;
    let soon = now() + 100;
    let push = || PushBuilder::new(&sender, encrypted(&keys, b"{}"));

    relay.expire(&endpoint, soon);
    assert_eq!(
        relay.push(&endpoint, push()).await.status,
        StatusCode::CREATED
    );
    // Kept to the hour.
    let expires_at = relay.expires_at(&endpoint);
    let delivered = now() + 90 * 86_400;
    assert!(
        (delivered - 5..=delivered + 3_600).contains(&expires_at),
        "{expires_at}"
    );

    relay.expire(&endpoint, soon);
    for answer in [
        Answer::status(503).json(json!({"reason": "ServiceUnavailable"})),
        Answer::status(400).json(json!({"reason": "BadTopic"})),
        Answer::status(413).json(json!({"reason": "PayloadTooLarge"})),
    ] {
        apns.answer(move |_| answer.clone());
        assert_ne!(
            relay.push(&endpoint, push()).await.status,
            StatusCode::CREATED
        );
        assert_eq!(relay.expires_at(&endpoint), soon);
    }
    assert_eq!(apns.drain().await.len(), 4);
}

/// APNs's own 413 is the sender's `413` too, and the handle stays.
#[tokio::test]
async fn apns_finding_a_push_too_large_is_payload_too_large_over_http() {
    let apns =
        Stub::start(|_| Answer::status(413).json(json!({"reason": "PayloadTooLarge"}))).await;
    let fcm = Stub::accepting().await;
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let endpoint = relay.register("apns", &apns_token(), &sender).await;
    let keys = ReceiverKeys::generate();
    let answered = relay
        .push(
            &endpoint,
            PushBuilder::new(&sender, encrypted(&keys, b"{}")),
        )
        .await;
    assert_eq!(
        (answered.status, answered.problem().as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "payload-too-large")
    );
    assert_eq!(relay.handles(), 1);
}

/// `Retry-After` as an HTTP date (APNs) or seconds (FCM, on a 429) is
/// handed to the sender as seconds; an APNs or FCM that can't be reached
/// at all is "try again in 30 seconds". The handles stay.
#[tokio::test]
async fn retry_after_from_apns_and_fcm_reaches_the_sender_as_seconds_over_http() {
    let apns = Stub::start(|_| {
        let at = std::time::SystemTime::now() + Duration::from_secs(120);
        Answer::status(503)
            .json(json!({"reason": "ServiceUnavailable"}))
            .header("retry-after", &httpdate::fmt_http_date(at))
    })
    .await;
    let fcm = Stub::start(|received| {
        if received.path == "/token" {
            Answer::status(200).json(json!({"access_token": "t", "expires_in": 3600}))
        } else {
            Answer::status(429)
                .json(
                    json!({"error": {"code": 429, "status": "RESOURCE_EXHAUSTED",
                    "details": [{"errorCode": "QUOTA_EXCEEDED"}]}}),
                )
                .header("retry-after", "75")
        }
    })
    .await;
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let push = || PushBuilder::new(&sender, encrypted(&keys, b"{}"));
    let to_apns = relay.register("apns", &apns_token(), &sender).await;
    let to_fcm = relay.register("fcm", FCM_TOKEN, &sender).await;

    let wait = |answered: &Answered| -> u64 {
        assert_eq!(
            (answered.status, answered.problem().as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, "upstream-unavailable"),
            "{}",
            answered.text()
        );
        let header: u64 = answered.headers[header::RETRY_AFTER]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(answered.json()["retry_after"], header);
        header
    };
    let from_date = wait(&relay.push(&to_apns, push()).await);
    assert!((118..=120).contains(&from_date), "{from_date}");
    assert_eq!(wait(&relay.push(&to_fcm, push()).await), 75);

    // Neither can be reached.
    let (dead_apns, dead_fcm) = (unreachable_url(), unreachable_url());
    let offline = TestRelay::start(
        &apns,
        &fcm,
        &[("apns.endpoint", &dead_apns), ("fcm.endpoint", &dead_fcm)],
    )
    .await;
    let to_apns = offline.register("apns", &apns_token(), &sender).await;
    let to_fcm = offline.register("fcm", FCM_TOKEN, &sender).await;
    assert_eq!(wait(&offline.push(&to_apns, push()).await), 30);
    assert_eq!(wait(&offline.push(&to_fcm, push()).await), 30);
    assert_eq!((relay.handles(), offline.handles()), (2, 2));
}

/// FCM's token endpoint refusing, or not answering a token, is FCM being
/// unavailable: nothing is sent to FCM without a token.
#[tokio::test]
async fn no_fcm_access_token_is_upstream_unavailable_and_nothing_is_sent_over_http() {
    for answer in [
        Answer::status(500),
        Answer::status(400).json(json!({"error": "invalid_grant"})),
        Answer::status(200).json(json!({"token_type": "Bearer"})),
    ] {
        let (apns, fcm) = (
            Stub::accepting().await,
            Stub::start(move |_| answer.clone()).await,
        );
        let relay = TestRelay::start(&apns, &fcm, &[]).await;
        let sender = Arc::new(VapidKey::generate());
        let keys = ReceiverKeys::generate();
        let endpoint = relay.register("fcm", FCM_TOKEN, &sender).await;
        let push = || PushBuilder::new(&sender, encrypted(&keys, b"{}"));
        // The failed exchange is remembered: the push straight after is
        // answered the same, with what is left of the wait, and tries none.
        for _ in 0..2 {
            let answered = relay.push(&endpoint, push()).await;
            assert_eq!(
                (answered.status, answered.problem().as_str()),
                (StatusCode::SERVICE_UNAVAILABLE, "upstream-unavailable")
            );
            let retry_after: u64 = answered.headers[header::RETRY_AFTER]
                .to_str()
                .unwrap()
                .parse()
                .unwrap();
            assert!((28..=30).contains(&retry_after), "{retry_after}");
        }
        let asked: Vec<String> = fcm.drain().await.into_iter().map(|r| r.path).collect();
        assert_eq!(asked, ["/token"]);
        assert_eq!(relay.handles(), 1);
    }
}

/// A token endpoint that takes the request and never answers (black-holed
/// beyond the first hop): pushes arriving together wait for one exchange,
/// and all are answered 503 within the upstream timeout, before a
/// sender's own timeout (monokulo's is 5 s), rather than one exchange
/// each in turn. The push after them tries none.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pushes_behind_a_token_endpoint_that_never_answers_are_503_within_the_timeout_over_http() {
    let apns = Stub::accepting().await;
    let fcm = Stub::start(|received| {
        if received.path == "/token" {
            Answer::never()
        } else {
            Answer::status(200).json(json!({"name": "projects/x/messages/1"}))
        }
    })
    .await;
    let timeout = Duration::from_millis(1000);
    let relay = TestRelay::start(&apns, &fcm, &[("server.upstream_timeout_ms", "1000")]).await;
    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let mut endpoints = Vec::new();
    for n in 0..10 {
        endpoints.push(
            relay
                .register("fcm", &format!("{FCM_TOKEN}-{n}"), &sender)
                .await,
        );
    }
    let retry_after = |answered: &Answered| -> u64 {
        assert_eq!(
            (answered.status, answered.problem().as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, "upstream-unavailable"),
            "{}",
            answered.text()
        );
        answered.headers[header::RETRY_AFTER]
            .to_str()
            .unwrap()
            .parse()
            .unwrap()
    };
    let started = std::time::Instant::now();
    let mut pushing = tokio::task::JoinSet::new();
    for endpoint in &endpoints {
        let request = PushBuilder::new(&sender, encrypted(&keys, b"{}")).request(endpoint);
        let router = relay.router.clone();
        pushing.spawn(async move { call_router(router, "192.0.2.1:1", request).await });
    }
    while let Some(answered) = pushing.join_next().await {
        let waited = retry_after(&answered.unwrap());
        assert!((28..=30).contains(&waited), "{waited}");
    }
    let took = started.elapsed();
    assert!(took < timeout * 3 / 2, "{took:?}");

    let started = std::time::Instant::now();
    let answered = relay
        .push(
            &endpoints[0],
            PushBuilder::new(&sender, encrypted(&keys, b"{}")),
        )
        .await;
    assert!((28..=30).contains(&retry_after(&answered)));
    assert!(started.elapsed() < timeout / 2);
    let exchanges = fcm.drain().await;
    assert_eq!(exchanges.len(), 1);
    assert_eq!(exchanges[0].path, "/token");
}

/// FCM refusing the access token (`401`) is "try again", and the next
/// push exchanges the assertion for a new token rather than reuse the
/// refused one.
#[tokio::test]
async fn an_access_token_fcm_refuses_is_replaced_for_the_next_push_over_http() {
    let issued = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counting = issued.clone();
    let fcm = Stub::start(move |received| {
        if received.path == "/token" {
            let n = counting.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            Answer::status(200)
                .json(json!({"access_token": format!("token-{n}"), "expires_in": 3600}))
        } else if received.header("authorization") == "Bearer token-1" {
            Answer::status(401).json(json!({"error": {"code": 401, "status": "UNAUTHENTICATED"}}))
        } else {
            Answer::status(200).json(json!({"name": "projects/x/messages/2"}))
        }
    })
    .await;
    let apns = Stub::accepting().await;
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let endpoint = relay.register("fcm", FCM_TOKEN, &sender).await;
    let push = || PushBuilder::new(&sender, encrypted(&keys, b"{}"));

    let answered = relay.push(&endpoint, push()).await;
    assert_eq!(
        (answered.status, answered.problem().as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "upstream-unavailable")
    );
    assert_eq!(
        relay.push(&endpoint, push()).await.status,
        StatusCode::CREATED
    );
    let seen: Vec<(String, String)> = fcm
        .drain()
        .await
        .into_iter()
        .map(|r| {
            let auth = r
                .headers
                .get("authorization")
                .map(|v| v.to_str().unwrap().to_owned());
            (r.path, auth.unwrap_or_default())
        })
        .collect();
    let send = "/v1/projects/monokular-test/messages:send".to_owned();
    assert_eq!(
        seen,
        [
            ("/token".to_owned(), String::new()),
            (send.clone(), "Bearer token-1".to_owned()),
            ("/token".to_owned(), String::new()),
            (send, "Bearer token-2".to_owned()),
        ]
    );
}

/// Pushes arriving together when no access token is held wait for one
/// exchange at the token endpoint, not one each.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pushes_at_once_share_one_fcm_token_exchange_over_http() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let mut endpoints = Vec::new();
    for n in 0..8 {
        endpoints.push(
            relay
                .register("fcm", &format!("{FCM_TOKEN}-{n}"), &sender)
                .await,
        );
    }
    let mut pushing = tokio::task::JoinSet::new();
    for endpoint in endpoints {
        let request = PushBuilder::new(&sender, encrypted(&keys, b"{}")).request(&endpoint);
        let router = relay.router.clone();
        pushing.spawn(async move { call_router(router, "192.0.2.1:1", request).await });
    }
    while let Some(answered) = pushing.join_next().await {
        let answered = answered.unwrap();
        assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.text());
    }
    let received = fcm.drain().await;
    let exchanges = received.iter().filter(|r| r.path == "/token").count();
    assert_eq!((exchanges, received.len()), (1, 9));
}

/// A push without a topic has no `apns-collapse-id`, and a TTL of 0 is
/// APNs's "now or never" (`apns-expiration: 0`).
#[tokio::test]
async fn a_push_without_topic_or_ttl_reaches_apns_as_now_or_never_over_http() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let endpoint = relay.register("apns", &apns_token(), &sender).await;
    let answered = relay
        .push(
            &endpoint,
            PushBuilder {
                topic: None,
                ttl: Some(Ttl::seconds(0)),
                urgency: Urgency::VeryLow,
                ..PushBuilder::new(&sender, encrypted(&keys, b"{}"))
            },
        )
        .await;
    assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.text());
    let received = apns.next().await;
    assert_eq!(received.header("apns-expiration"), "0");
    assert_eq!(received.header("apns-priority"), "5");
    assert!(!received.headers.contains_key("apns-collapse-id"));
}

/// Only pushes the handle's sender signed count against the limits: a
/// stranger's forged pushes can't use up a sender's or a device's budget.
#[tokio::test]
async fn unsigned_pushes_spend_no_senders_or_handles_budget_over_http() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(
        &apns,
        &fcm,
        &[
            ("limits.per_handle_per_minute", "1"),
            ("limits.per_sender_per_minute", "1"),
        ],
    )
    .await;
    let sender = Arc::new(VapidKey::generate());
    let stranger = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let endpoint = relay.register("apns", &apns_token(), &sender).await;
    for _ in 0..5 {
        let forged = relay
            .push(
                &endpoint,
                PushBuilder::new(&stranger, encrypted(&keys, b"{}")),
            )
            .await;
        assert_eq!(forged.status, StatusCode::UNAUTHORIZED);
    }
    let answered = relay
        .push(
            &endpoint,
            PushBuilder::new(&sender, encrypted(&keys, b"{}")),
        )
        .await;
    assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.text());
    assert_eq!(apns.drain().await.len(), 1);
}

/// Pushes that fail, every way they can, log what failed and never the
/// token, endpoint, signature or body: a forged or malformed push, APNs
/// and FCM refusing or unreachable, FCM refusing its access token.
#[tokio::test]
async fn the_log_carries_no_token_endpoint_or_body_when_pushes_fail_over_http() {
    let (_guard, log) = capture_log();
    let apns = Stub::start(|_| Answer::status(403).json(json!({"reason": "BadCertificate"}))).await;
    let fcm = Stub::start(|received| {
        if received.path == "/token" {
            Answer::status(200)
                .json(json!({"access_token": "ya29.secret-access", "expires_in": 3600}))
        } else {
            Answer::status(401).json(json!({"error": {"code": 401, "status": "UNAUTHENTICATED"}}))
        }
    })
    .await;
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let push = || PushBuilder::new(&sender, encrypted(&keys, b"secret plaintext"));
    let to_apns = relay.register("apns", &apns_token(), &sender).await;
    let to_fcm = relay.register("fcm", FCM_TOKEN, &sender).await;
    let activity = relay
        .register("apns-liveactivity", &"4e".repeat(32), &sender)
        .await;

    assert_eq!(
        relay.push(&to_apns, push()).await.status,
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        relay.push(&to_fcm, push()).await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let stranger = Arc::new(VapidKey::generate());
    let forged = PushBuilder::new(&stranger, encrypted(&keys, b"secret plaintext"));
    assert_eq!(
        relay.push(&to_apns, forged).await.status,
        StatusCode::UNAUTHORIZED
    );
    let secret_state = plain(json!({"phase": "teleported", "paymentURI": "monero:4SecretAddress"}));
    assert_eq!(
        relay
            .push(&activity, PushBuilder::new(&sender, secret_state))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    let offline = TestRelay::start(&apns, &fcm, &[("apns.endpoint", &unreachable_url())]).await;
    let unreachable = offline.register("apns", &apns_token(), &sender).await;
    assert_eq!(
        offline.push(&unreachable, push()).await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );

    let text = log.text();
    assert!(
        text.contains("APNs refused a push"),
        "the relay logged: {text}"
    );
    assert!(
        text.contains("APNs can't be reached"),
        "the relay logged: {text}"
    );
    for secret in [
        apns_token().as_str(),
        "4e4e4e4e4e4e4e4e",
        FCM_TOKEN,
        "ya29.secret-access",
        "4SecretAddress",
        "secret plaintext",
        "/v1/push/ph_",
        "/3/device/",
        "eyJ",
    ] {
        assert!(!text.contains(secret), "{secret} is in the log: {text}");
    }
}

/// The status says what the relay listens on, its apps, how many handles
/// it holds, which platforms are ready and how the last push handed on
/// went: never a token, an endpoint or a body. Served on a loopback
/// listener of its own, it answers this machine and refuses a page that
/// points another name at it.
#[tokio::test]
async fn the_status_says_how_the_last_push_went_and_nothing_private_over_http() {
    let apns = Stub::accepting().await;
    let fcm = Stub::accepting().await;
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let listen: std::net::SocketAddr = "0.0.0.0:8444".parse().unwrap();
    let started_at = now();
    let report =
        || async { serde_json::to_value(relay.relay.report(listen, started_at).await).unwrap() };
    let before = report().await;
    assert_eq!(
        before,
        json!({
            "service": "monokulo-push-relay",
            "version": env!("CARGO_PKG_VERSION"),
            "commit": service_runtime::status::commit(),
            "started_at": started_at,
            "listen": "0.0.0.0:8444",
            "public_uri": PUBLIC_URI,
            "apps": [APP],
            "handles": { "stored": 0 },
            "platforms": { "apns": "ready", "fcm": "ready" },
            "last_delivery": null,
        })
    );

    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let token = apns_token();
    let endpoint = relay.register("apns", &token, &sender).await;
    let answered = relay
        .push(
            &endpoint,
            PushBuilder::new(&sender, encrypted(&keys, b"{}")),
        )
        .await;
    assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.text());
    let after = report().await;
    assert_eq!(after["handles"], json!({ "stored": 1 }));
    assert_eq!(after["last_delivery"]["outcome"], "sent");
    assert!(within(
        after["last_delivery"]["at"].as_i64().unwrap(),
        now()
    ));

    apns.answer(|_| Answer::status(429).json(json!({"reason": "TooManyRequests"})));
    relay
        .push(
            &endpoint,
            PushBuilder::new(&sender, encrypted(&keys, b"{}")),
        )
        .await;
    let limited = report().await;
    assert_eq!(limited["last_delivery"]["outcome"], "unavailable");
    let text = limited.to_string();
    let handle = endpoint.rsplit('/').next().unwrap();
    for private in [token.as_str(), handle, endpoint.as_str()] {
        assert!(!text.contains(private), "{private} in {text}");
    }

    // Served as the binary serves it, on loopback.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = listener.local_addr().unwrap();
    let served = relay.relay.clone();
    let stop = tokio_util::sync::CancellationToken::new();
    let serving = tokio::spawn(service_runtime::status::serve(
        listener,
        move || {
            let relay = served.clone();
            async move { relay.report(listen, started_at).await }
        },
        stop.clone().cancelled_owned(),
    ));
    let client = reqwest::Client::new();
    let answer = client
        .get(format!("http://{at}/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(answer.status(), 200);
    let served: Value = serde_json::from_str(&answer.text().await.unwrap()).unwrap();
    assert_eq!(served, report().await);
    let rebound = client
        .get(format!("http://{at}/status"))
        .header("host", format!("relay-status.example:{}", at.port()))
        .send()
        .await
        .unwrap();
    assert_eq!(rebound.status(), 403);
    stop.cancel();
    serving.await.unwrap().unwrap();
}
