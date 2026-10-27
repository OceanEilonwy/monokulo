//! The relay's routes, called in-process: registration, and every push
//! refused before anything is handed to APNs or FCM. What reaches them,
//! and what their answers become, is in `relay_over_http.rs`.

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
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
    assert!((now + 90 * 86_400 - 5..=now + 90 * 86_400).contains(&expires_at));

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
