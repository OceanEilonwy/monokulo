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

#[tokio::test]
async fn a_live_activity_is_updated_then_ended_with_an_alert_over_http() {
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
    let body = apns.next().await.json();
    assert_eq!(body["aps"]["event"], "end");
    assert_eq!(body["aps"]["content-state"], paid);
    assert_eq!(body["aps"]["alert"], json!({"title": "Paid · $24.50"}));
}

#[tokio::test]
async fn a_push_to_start_starts_a_live_activity_over_http() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let endpoint = relay
        .register("apns-pushtostart", &apns_token(), &sender)
        .await;
    let start = json!({
        "attributes-type": "SaleAttributes",
        "attributes": {"location": "Front Counter", "store": "Geomart"},
        "content-state": {"phase": "waiting", "amount": "$3.00"},
    });
    let answered = relay
        .push(&endpoint, PushBuilder::new(&sender, plain(start)))
        .await;
    assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.text());
    let received = apns.next().await;
    assert_eq!(received.header("apns-push-type"), "liveactivity");
    assert_eq!(
        received.header("apns-topic"),
        "com.example.monokular.push-type.liveactivity"
    );
    let aps = &received.json()["aps"];
    assert_eq!(aps["event"], "start");
    assert_eq!(aps["attributes-type"], "SaleAttributes");
    assert_eq!(
        aps["attributes"],
        json!({"location": "Front Counter", "store": "Geomart"})
    );
    assert_eq!(
        aps["content-state"],
        json!({"phase": "waiting", "amount": "$3.00"})
    );
    assert_eq!(aps["alert"], json!({"title": "New sale"}));
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
    assert_eq!(
        relay.push(&two, push()).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
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
    assert_eq!(apns.drain().await.len(), 2);
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
    let handle = endpoints[0].rsplit('/').next().unwrap();
    assert!(text.contains(handle), "lines name the handle: {text}");
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
