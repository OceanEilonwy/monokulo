//! The relay's routes, called in-process: registration, and every push
//! refused before anything is handed to APNs or FCM. What reaches them,
//! and what their answers become, is in `relay_over_http.rs`.

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use push::encrypt::ReceiverKeys;
use push::VapidKey;
use serde_json::json;
use support::*;

async fn relay() -> TestRelay {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    TestRelay::start(&apns, &fcm, &[]).await
}

/// A registration is answered with an endpoint under the relay's address;
/// the same device, platform, app and sender again get the same one.
#[tokio::test]
async fn registering_mints_a_handle_under_the_public_uri_and_again_keeps_it() {
    let relay = relay().await;
    let sender = VapidKey::generate();
    let first = relay
        .call(registration("apns", &apns_token(), &sender))
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text());
    let body = first.json();
    let endpoint = body["endpoint"].as_str().unwrap();
    let handle = endpoint
        .strip_prefix("https://relay.example.org/v1/push/ph_")
        .unwrap_or_else(|| panic!("{endpoint}"));
    assert_eq!(handle.len(), 32);
    let now = service_runtime::time::now_unix();
    let expires_at = body["expires_at"].as_i64().unwrap();
    // No push has reached it yet: `handles.unused_ttl_days`, to the hour.
    let unused = now + 7 * 86_400;
    assert!(
        (unused - 5..=unused + 3_600).contains(&expires_at),
        "{expires_at}"
    );

    let again = relay
        .call(registration("apns", &apns_token(), &sender))
        .await
        .json();
    assert_eq!(again["endpoint"], endpoint);
    assert!(again["expires_at"].as_i64().unwrap() >= expires_at);
    assert_eq!(relay.handles(), 1);
}

/// One device registered for two senders (two stores' monokulos) has two
/// handles, and neither sender can push to the other's.
#[tokio::test]
async fn two_senders_of_one_device_get_two_handles_that_cannot_push_to_each_other() {
    let relay = relay().await;
    let (a, b) = (
        Arc::new(VapidKey::generate()),
        Arc::new(VapidKey::generate()),
    );
    let to_a = relay.register("fcm", FCM_TOKEN, &a).await;
    let to_b = relay.register("fcm", FCM_TOKEN, &b).await;
    assert_ne!(to_a, to_b);
    let keys = ReceiverKeys::generate();
    for (endpoint, wrong) in [(&to_a, &b), (&to_b, &a)] {
        let answered = relay
            .push(endpoint, PushBuilder::new(wrong, encrypted(&keys, b"{}")))
            .await;
        assert_eq!(answered.status, StatusCode::UNAUTHORIZED);
        assert_eq!(answered.problem(), "unauthorized");
        assert_eq!(answered.headers[header::WWW_AUTHENTICATE], "vapid");
    }
}

#[tokio::test]
async fn registrations_the_relay_cannot_serve_are_refused_with_their_problem() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    // An app on APNs alone.
    let apps = json!({APP: {"apns_topic": BUNDLE}, "other": {"fcm_project_id": PROJECT}});
    let relay = TestRelay::start(&apns, &fcm, &[("handles.apps", &apps.to_string())]).await;
    let sender = VapidKey::generate().public().to_base64url();
    let cases = [
        (
            json!({"platform": "fcm", "token": FCM_TOKEN, "app": APP, "vapid_public_key": sender}),
            "unsupported-platform",
        ),
        (
            json!({"platform": "apns", "token": apns_token(), "app": "unknown", "vapid_public_key": sender}),
            "unknown-app",
        ),
        (
            json!({"platform": "apns", "token": apns_token(), "app": APP, "vapid_public_key": "BAAA"}),
            "invalid-request",
        ),
        (
            json!({"platform": "apns", "token": "not hex", "app": APP, "vapid_public_key": sender}),
            "invalid-request",
        ),
        (
            json!({"platform": "web", "token": apns_token(), "app": APP, "vapid_public_key": sender}),
            "invalid-request",
        ),
        // No sender writes a push-to-start, so no endpoint is minted for
        // one (decisions/push.md 157).
        (
            json!({"platform": "apns-pushtostart", "token": apns_token(), "app": APP, "vapid_public_key": sender}),
            "invalid-request",
        ),
        (
            json!({"platform": "apns", "token": apns_token(), "app": APP, "vapid_public_key": sender, "store": "s"}),
            "invalid-request",
        ),
        (
            json!({"platform": "apns", "app": APP, "vapid_public_key": sender}),
            "invalid-request",
        ),
    ];
    for (body, problem) in cases {
        let answered = relay.call(json_request("/v1/register", body.clone())).await;
        assert_eq!(answered.status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(answered.problem(), problem, "{body}");
    }
    let answered = relay
        .call(
            Request::post("/v1/register")
                .body(Body::from("not json"))
                .unwrap(),
        )
        .await;
    assert_eq!(answered.problem(), "invalid-request");
    assert_eq!(relay.handles(), 0);
}

/// Registrations are limited per address (IPv6 by its /64); behind a
/// trusted proxy, by the address it forwards for.
#[tokio::test]
async fn registrations_are_limited_per_address() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(
        &apns,
        &fcm,
        &[
            ("limits.registrations_per_minute", "2"),
            ("server.trusted_proxies", "10.0.0.1"),
        ],
    )
    .await;
    let sender = VapidKey::generate();
    let register = |peer: &'static str| {
        let relay = &relay;
        let sender = &sender;
        async move {
            relay
                .call_from(peer, registration("apns", &apns_token(), sender))
                .await
        }
    };
    assert_eq!(
        register("[2001:db8::1]:1").await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        register("[2001:db8::2]:1").await.status,
        StatusCode::CREATED
    );
    let refused = register("[2001:db8::3]:1").await;
    assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(refused.problem(), "rate-limited");
    let wait: u64 = refused.headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&wait), "{wait}");
    assert_eq!(refused.json()["retry_after"], wait);
    // Another /64 has its own budget.
    assert_eq!(
        register("[2001:db8:0:1::1]:1").await.status,
        StatusCode::CREATED
    );

    // The proxy is trusted: each address it forwards for counts alone,
    // and the proxy itself not at all.
    let forwarded = |client: &str| {
        let mut request = registration("apns", &apns_token(), &sender);
        request
            .headers_mut()
            .insert("x-forwarded-for", client.parse().unwrap());
        request
    };
    for client in [
        "198.51.100.1",
        "198.51.100.1",
        "198.51.100.2",
        "198.51.100.2",
    ] {
        let answered = relay.call_from("10.0.0.1:443", forwarded(client)).await;
        assert_eq!(answered.status, StatusCode::CREATED, "{client}");
    }
    let answered = relay
        .call_from("10.0.0.1:443", forwarded("198.51.100.1"))
        .await;
    assert_eq!(answered.status, StatusCode::TOO_MANY_REQUESTS);
    // From anyone else the header is the client's own word: ignored.
    for _ in 0..2 {
        relay
            .call_from("192.0.2.9:1", forwarded("198.51.100.3"))
            .await;
    }
    let answered = relay
        .call_from("192.0.2.9:1", forwarded("198.51.100.4"))
        .await;
    assert_eq!(answered.status, StatusCode::TOO_MANY_REQUESTS);
}

/// A trusted proxy that adds its own `X-Forwarded-For` line (HAProxy's
/// `option forwardfor`) leaves the client's invented line first: the
/// registration limit counts the address the proxy's line names, so a new
/// invented address on each request buys no new budget.
#[tokio::test]
async fn registrations_behind_a_line_adding_proxy_are_limited_by_its_line() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(
        &apns,
        &fcm,
        &[
            ("limits.registrations_per_minute", "2"),
            ("server.trusted_proxies", "10.0.0.1"),
        ],
    )
    .await;
    let sender = VapidKey::generate();
    let mut statuses = Vec::new();
    for invented in ["203.0.113.1", "203.0.113.2", "203.0.113.3"] {
        let mut request = registration("apns", &apns_token(), &sender);
        let headers = request.headers_mut();
        headers.append("x-forwarded-for", invented.parse().unwrap());
        headers.append("x-forwarded-for", "198.51.100.7".parse().unwrap());
        statuses.push(relay.call_from("10.0.0.1:443", request).await.status);
    }
    assert_eq!(
        statuses,
        [
            StatusCode::CREATED,
            StatusCode::CREATED,
            StatusCode::TOO_MANY_REQUESTS
        ]
    );
}

/// Pushes refused before anything is handed on: none of them reaches APNs
/// or FCM.
#[tokio::test]
async fn pushes_that_are_not_the_handles_senders_or_not_well_formed_are_refused() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let endpoint = relay.register("apns", &apns_token(), &sender).await;
    let keys = ReceiverKeys::generate();
    let push = || PushBuilder::new(&sender, encrypted(&keys, br#"{"sale":"paid"}"#));

    // Signed for another push service.
    let elsewhere = PushBuilder {
        audience: "https://fcm.googleapis.com".into(),
        ..push()
    };
    let answered = relay.push(&endpoint, elsewhere).await;
    assert_eq!(
        (answered.status, answered.problem().as_str()),
        (StatusCode::UNAUTHORIZED, "unauthorized")
    );
    // No signature at all.
    let mut request = push().request(&endpoint);
    request.headers_mut().remove(header::AUTHORIZATION);
    let answered = relay.call(request).await;
    assert_eq!(answered.problem(), "unauthorized");

    // No TTL.
    let answered = relay
        .push(
            &endpoint,
            PushBuilder {
                ttl: None,
                ..push()
            },
        )
        .await;
    assert_eq!(
        (answered.status, answered.problem().as_str()),
        (StatusCode::BAD_REQUEST, "invalid-request")
    );
    assert!(answered.json()["detail"].as_str().unwrap().contains("TTL"));

    // Plain JSON to a handle that takes ciphertext.
    let answered = relay
        .push(
            &endpoint,
            PushBuilder::new(&sender, plain(json!({"phase": "paid"}))),
        )
        .await;
    assert_eq!(
        (answered.status, answered.problem().as_str()),
        (StatusCode::BAD_REQUEST, "invalid-request")
    );

    // More than a push may be, read no further than that.
    let mut request = push().request(&endpoint);
    *request.body_mut() = Body::from(vec![0u8; 4097]);
    let answered = relay.call(request).await;
    assert_eq!(
        (answered.status, answered.problem().as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "payload-too-large")
    );
    let mut request = push().request(&endpoint);
    *request.body_mut() = Body::from(vec![0u8; 1 << 20]);
    assert_eq!(
        relay.call(request).await.status,
        StatusCode::PAYLOAD_TOO_LARGE
    );

    // A handle never made, or not one at all.
    for unknown in [
        "https://relay.example.org/v1/push/ph_00000000000000000000000000000000",
        "https://relay.example.org/v1/push/not-a-handle",
    ] {
        let answered = relay.push(unknown, push()).await;
        assert_eq!(
            (answered.status, answered.problem().as_str()),
            (StatusCode::NOT_FOUND, "unknown-handle")
        );
    }
    assert!(apns.drain().await.is_empty(), "nothing reached APNs");
    assert!(fcm.drain().await.is_empty(), "nothing reached FCM");
}

/// A handle unused for its TTL is unknown: its sender is told to drop it.
#[tokio::test]
async fn an_expired_handle_is_unknown() {
    let relay = relay().await;
    let sender = Arc::new(VapidKey::generate());
    let endpoint = relay.register("apns", &apns_token(), &sender).await;
    relay.expire(&endpoint, service_runtime::time::now_unix());
    let keys = ReceiverKeys::generate();
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
    // Registered again, the device has a handle again (the same one, or a
    // new one if the hourly sweep deleted the old in between).
    relay.register("apns", &apns_token(), &sender).await;
    assert_eq!(relay.handles(), 1);
}

/// A VAPID token is refused unless it is the handle's sender's, signed
/// for this relay, unexpired and expiring within 24 hours. Every one of
/// these is `401` with the challenge, and none reaches APNs or FCM.
#[tokio::test]
async fn forged_expired_and_misaddressed_vapid_tokens_are_unauthorized() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let endpoint = relay.register("fcm", FCM_TOKEN, &sender).await;
    let keys = ReceiverKeys::generate();
    let push = || PushBuilder::new(&sender, encrypted(&keys, b"{}"));
    let now = service_runtime::time::now_unix();
    let twelve_hours = 12 * 60 * 60;

    let mut refused = Vec::new();
    // Expired ten seconds ago.
    refused.push(
        PushBuilder {
            signed_at: Some(now - twelve_hours - 10),
            ..push()
        }
        .request(&endpoint),
    );
    // Valid for longer than the 24 hours a push service takes.
    refused.push(
        PushBuilder {
            signed_at: Some(now + twelve_hours + 10),
            ..push()
        }
        .request(&endpoint),
    );
    // Signed for an origin that only looks like this relay's.
    for audience in [
        "http://relay.example.org",
        "https://relay.example.org:8443",
        "https://relay.example.org.example.net",
    ] {
        refused.push(
            PushBuilder {
                audience: audience.into(),
                ..push()
            }
            .request(&endpoint),
        );
    }
    // Another key's token presented under the handle's key, and the
    // handle's own token with its signature altered.
    let other = Arc::new(VapidKey::generate());
    let theirs = PushBuilder::new(&other, encrypted(&keys, b"{}")).request(&endpoint);
    let (token, _) = vapid_parts(&theirs);
    let ours = push().request(&endpoint);
    let (own_token, key) = vapid_parts(&ours);
    let mut altered = own_token.clone().into_bytes();
    let at = own_token.rfind('.').unwrap() + 10;
    altered[at] = if altered[at] == b'A' { b'B' } else { b'A' };
    let altered = String::from_utf8(altered).unwrap();
    // An unsigned token (`alg: none`) under the handle's key.
    let none = format!(
        "{}.{}.",
        URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"none"}"#),
        own_token.split('.').nth(1).unwrap()
    );
    for authorization in [
        format!("vapid t={token}, k={key}"),
        format!("vapid t={altered}, k={key}"),
        format!("vapid t={none}, k={key}"),
        format!("Bearer {own_token}"),
        format!("vapid t={own_token}"),
        format!("vapid t={own_token}, k={key}, k={key}"),
        format!("WebPush {own_token}"),
    ] {
        let mut request = push().request(&endpoint);
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, authorization.parse().unwrap());
        refused.push(request);
    }

    for (n, request) in refused.into_iter().enumerate() {
        let answered = relay.call(request).await;
        assert_eq!(
            (answered.status, answered.problem().as_str()),
            (StatusCode::UNAUTHORIZED, "unauthorized"),
            "case {n}: {}",
            answered.text()
        );
        assert_eq!(answered.headers[header::WWW_AUTHENTICATE], "vapid");
    }
    assert!(fcm.drain().await.is_empty(), "nothing reached FCM");
    assert_eq!(relay.handles(), 1, "a refused push leaves the handle");
}

/// The `t` and `k` of a request's `Authorization: vapid t=…, k=…`.
fn vapid_parts(request: &Request<Body>) -> (String, String) {
    let value = request.headers()[header::AUTHORIZATION].to_str().unwrap();
    let params = value.strip_prefix("vapid ").unwrap();
    let (t, k) = params.split_once(", ").unwrap();
    (
        t.strip_prefix("t=").unwrap().to_owned(),
        k.strip_prefix("k=").unwrap().to_owned(),
    )
}

/// RFC 8030 headers and bodies the relay can't read are `400`, before the
/// handle's key is even looked at; none reaches APNs or FCM.
#[tokio::test]
async fn malformed_push_headers_and_bodies_are_invalid_requests() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let endpoint = relay.register("apns", &apns_token(), &sender).await;
    let keys = ReceiverKeys::generate();
    let push = || PushBuilder::new(&sender, encrypted(&keys, b"{}")).request(&endpoint);
    let with = |name: &'static str, value: &str| {
        let mut request = push();
        request.headers_mut().insert(name, value.parse().unwrap());
        request
    };
    let without = |name: header::HeaderName| {
        let mut request = push();
        request.headers_mut().remove(name);
        request
    };
    let with_body = |body: &'static [u8]| {
        let mut request = push();
        *request.body_mut() = Body::from(body);
        request
    };
    let cases = [
        ("TTL in words", with("ttl", "thirty")),
        ("negative TTL", with("ttl", "-1")),
        ("fractional TTL", with("ttl", "1.5")),
        ("empty TTL", with("ttl", "")),
        ("unknown urgency", with("urgency", "urgent")),
        ("topic past 32 characters", with("topic", &"t".repeat(33))),
        ("topic outside base64url", with("topic", "sale/1042")),
        (
            "the old aesgcm encoding",
            with("content-encoding", "aesgcm"),
        ),
        (
            "neither encoding nor JSON",
            without(header::CONTENT_ENCODING),
        ),
        ("no body", with_body(b"")),
        ("a body too short for aes128gcm", with_body(b"\x00\x01")),
    ];
    for (case, request) in cases {
        let answered = relay.call(request).await;
        assert_eq!(
            (answered.status, answered.problem().as_str()),
            (StatusCode::BAD_REQUEST, "invalid-request"),
            "{case}: {}",
            answered.text()
        );
    }
    // JSON that isn't JSON, to a Live Activity.
    let activity = relay
        .register("apns-liveactivity", &"1e".repeat(32), &sender)
        .await;
    let mut request = PushBuilder::new(&sender, plain(json!({"phase": "seen"}))).request(&activity);
    *request.body_mut() = Body::from("{\"phase\":");
    let answered = relay.call(request).await;
    assert_eq!(answered.problem(), "invalid-request", "{}", answered.text());
    assert!(apns.drain().await.is_empty(), "nothing reached APNs");
}

/// Each handle takes one kind of body: ciphertext for `apns` and `fcm`, a
/// Live Activity's JSON for the other two, and that JSON must be one.
#[tokio::test]
async fn a_body_the_handles_platform_does_not_take_is_an_invalid_request() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let to_fcm = relay.register("fcm", FCM_TOKEN, &sender).await;
    let activity = relay
        .register("apns-liveactivity", &"2e".repeat(32), &sender)
        .await;
    let cases = [
        (&to_fcm, plain(json!({"phase": "paid"}))),
        (&activity, encrypted(&keys, b"{}")),
        (&activity, plain(json!({"phase": "idle"}))),
        (&activity, plain(json!({"phase": "paid", "amount": 24.5}))),
        (
            &activity,
            plain(json!({"phase": "seen", "expiresAt": "soon"})),
        ),
    ];
    for (n, (endpoint, message)) in cases.into_iter().enumerate() {
        let answered = relay
            .push(endpoint, PushBuilder::new(&sender, message))
            .await;
        assert_eq!(
            (answered.status, answered.problem().as_str()),
            (StatusCode::BAD_REQUEST, "invalid-request"),
            "case {n}: {}",
            answered.text()
        );
    }
    assert!(apns.drain().await.is_empty(), "nothing reached APNs");
    assert!(fcm.drain().await.is_empty(), "nothing reached FCM");
    assert_eq!(relay.handles(), 2);
}

/// A push may be 4096 bytes, but base64 inside APNs's or FCM's JSON
/// makes one that size too large for either: `413` before anything is
/// sent, and the handle stays.
#[tokio::test]
async fn a_push_too_large_once_wrapped_for_apns_or_fcm_is_413_and_never_sent() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let relay = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let keys = ReceiverKeys::generate();
    let to_apns = relay.register("apns", &apns_token(), &sender).await;
    let to_fcm = relay.register("fcm", FCM_TOKEN, &sender).await;
    let activity = relay
        .register("apns-liveactivity", &"4e".repeat(32), &sender)
        .await;
    let largest = encrypted(&keys, &[b'x'; push::MAX_PLAINTEXT]);
    assert_eq!(largest.body().len(), push::MAX_BODY);
    let state = plain(json!({"phase": "seen", "sale": "x".repeat(4050)}));
    for (n, (endpoint, message)) in [
        (&to_apns, largest.clone()),
        (&to_fcm, largest),
        (&to_apns, encrypted(&keys, &[b'x'; 3000])),
        (&to_fcm, encrypted(&keys, &[b'x'; 3000])),
        (&activity, state),
    ]
    .into_iter()
    .enumerate()
    {
        let answered = relay
            .push(endpoint, PushBuilder::new(&sender, message))
            .await;
        assert_eq!(
            (answered.status, answered.problem().as_str()),
            (StatusCode::PAYLOAD_TOO_LARGE, "payload-too-large"),
            "case {n}: {}",
            answered.text()
        );
    }
    assert!(apns.drain().await.is_empty(), "nothing reached APNs");
    assert!(fcm.drain().await.is_empty(), "nothing reached FCM");
    assert_eq!(relay.handles(), 3);
}

/// A registration is four short fields: a body past 8 KiB isn't read.
#[tokio::test]
async fn a_registration_past_8_kib_is_too_large() {
    let relay = relay().await;
    let sender = VapidKey::generate();
    let body = json!({
        "platform": "fcm",
        "token": "a".repeat(9 * 1024),
        "app": APP,
        "vapid_public_key": sender.public().to_base64url(),
    });
    let answered = relay.call(json_request("/v1/register", body)).await;
    assert_eq!(
        (answered.status, answered.problem().as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "payload-too-large")
    );
    // A token past the 1024 characters a row holds, within the 8 KiB.
    let body = json!({
        "platform": "fcm",
        "token": "a".repeat(1025),
        "app": APP,
        "vapid_public_key": sender.public().to_base64url(),
    });
    let answered = relay.call(json_request("/v1/register", body)).await;
    assert_eq!(answered.problem(), "invalid-request");
    assert_eq!(relay.handles(), 0);
}

/// An APNs token is hex however an app spells it: the same device in
/// capitals is the same handle, not a second one.
#[tokio::test]
async fn an_apns_token_in_capitals_is_the_same_handle() {
    let relay = relay().await;
    let sender = VapidKey::generate();
    let lower = relay.register("apns", &"ab".repeat(32), &sender).await;
    let upper = relay.register("apns", &"AB".repeat(32), &sender).await;
    assert_eq!(lower, upper);
    assert_eq!(relay.handles(), 1);
}

/// An app registering on several threads at once (launch, foreground and
/// token refresh together) still has one handle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registrations_of_one_device_at_once_make_one_handle() {
    let relay = relay().await;
    let sender = Arc::new(VapidKey::generate());
    let mut registering = tokio::task::JoinSet::new();
    for n in 0..16 {
        let (router, sender) = (relay.router.clone(), sender.clone());
        registering.spawn(async move {
            let peer = format!("192.0.2.{n}:1");
            call_router(router, &peer, registration("fcm", FCM_TOKEN, &sender)).await
        });
    }
    let mut endpoints = std::collections::BTreeSet::new();
    while let Some(answered) = registering.join_next().await {
        let answered = answered.unwrap();
        assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.text());
        endpoints.insert(answered.json()["endpoint"].as_str().unwrap().to_owned());
    }
    assert_eq!(endpoints.len(), 1, "{endpoints:?}");
    assert_eq!(relay.handles(), 1);
}

/// A handle whose app has since been taken out of `handles.apps`, or out
/// of a platform, takes no more pushes: it is forgotten, and its sender
/// told to drop it, without APNs or FCM being asked.
#[tokio::test]
async fn a_handle_of_an_app_no_longer_served_is_forgotten() {
    let (apns, fcm) = (Stub::accepting().await, Stub::accepting().await);
    let before = TestRelay::start(&apns, &fcm, &[]).await;
    let sender = Arc::new(VapidKey::generate());
    let to_apns = before.register("apns", &apns_token(), &sender).await;
    let to_fcm = before.register("fcm", FCM_TOKEN, &sender).await;
    // The same database, restarted serving the app on FCM alone.
    let apps = json!({APP: {"fcm_project_id": PROJECT}}).to_string();
    let store = before.store_path.to_str().unwrap().to_owned();
    let after = TestRelay::start(
        &apns,
        &fcm,
        &[("handles.apps", &apps), ("store.path", &store)],
    )
    .await;
    let keys = ReceiverKeys::generate();
    let answered = after
        .push(&to_apns, PushBuilder::new(&sender, encrypted(&keys, b"{}")))
        .await;
    assert_eq!(
        (answered.status, answered.problem().as_str()),
        (StatusCode::NOT_FOUND, "unknown-handle")
    );
    assert!(apns.drain().await.is_empty(), "nothing reached APNs");
    assert_eq!(before.handles(), 1, "only the FCM handle is left");
    assert!(before.expires_at(&to_fcm) > 0, "and it is the FCM one");
}
