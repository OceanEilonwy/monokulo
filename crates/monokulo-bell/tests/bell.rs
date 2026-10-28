//! The Bell called in-process, for what it answers without asking Square
//! or sending a ding: its info, the webhook signature, and the
//! registrations it refuses before any request.

mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::json;
use support::*;

/// The public key `/v1/info` gives is the one `MONOKULAR_BELL_VAPID_KEY`'s
/// private key makes, with the application and its environment.
#[tokio::test]
async fn info_shows_the_application_and_the_public_key_of_the_vapid_key() {
    let bell = TestBell::without_square(&[]).await;
    let answered = bell
        .call(Request::get("/v1/info").body(Body::empty()).unwrap())
        .await;
    assert_eq!(answered.status, StatusCode::OK);
    let mut private = [0u8; 32];
    hex::decode_to_slice(VAPID_HEX, &mut private).unwrap();
    let public = push::VapidKey::from_bytes(&private).unwrap().public();
    assert_eq!(
        answered.json(),
        json!({
            "square_application_id": APPLICATION_ID,
            "environment": "production",
            "vapid_public_key": public.to_base64url(),
            "version": env!("CARGO_PKG_VERSION"),
        })
    );
    let sandbox = TestBell::without_square(&[
        ("bell.square.environment", "sandbox"),
        ("bell.square.application_id", "sandbox-sq0idb-MonokularTest"),
    ])
    .await;
    let answered = sandbox
        .call(Request::get("/v1/info").body(Body::empty()).unwrap())
        .await;
    assert_eq!(answered.json()["environment"], "sandbox");
    assert_eq!(
        answered.json()["square_application_id"],
        "sandbox-sq0idb-MonokularTest"
    );
}

/// Square's signature over the webhook URL and the body is what lets a
/// webhook in; without it, or for another body or URL, it is refused.
#[tokio::test]
async fn a_webhook_is_taken_only_with_squares_signature_for_this_url() {
    let bell = TestBell::without_square(&[]).await;
    let body = payment_webhook("payment.updated", "APPROVED", MERCHANT, LOCATION);
    let signed = bell.webhook(&body).await;
    assert_eq!(signed.status, StatusCode::OK, "{}", signed.text());

    let refused = [
        webhook_request(&body, None),
        webhook_request(&body, Some("not base64!")),
        webhook_request(
            &body.replace(LOCATION, OTHER_LOCATION),
            Some(&sign(WEBHOOK_URL, &body)),
        ),
        webhook_request(&body, Some(&sign(&format!("{WEBHOOK_URL}/"), &body))),
        webhook_request(&body, Some(&sign(&format!("{WEBHOOK_URL}?a=1"), &body))),
        webhook_request(
            &body,
            Some(&sign("https://other.example.org/v1/square/webhook", &body)),
        ),
        webhook_request(
            &body,
            Some(&sign("http://bell.example.org/v1/square/webhook", &body)),
        ),
    ];
    for request in refused {
        let answered = bell.call(request).await;
        assert_eq!(
            answered.status,
            StatusCode::UNAUTHORIZED,
            "{}",
            answered.text()
        );
        assert_eq!(answered.problem(), "invalid-signature");
    }
}

/// The address is the same written with or without its trailing slash;
/// one under a path signs for that path.
#[tokio::test]
async fn the_webhook_url_follows_the_public_address_as_written() {
    let body = payment_webhook("payment.updated", "APPROVED", MERCHANT, LOCATION);
    let slash = TestBell::without_square(&[("bell.public_uri", "https://bell.example.org/")]).await;
    assert_eq!(slash.webhook(&body).await.status, StatusCode::OK);

    let under = TestBell::without_square(&[("bell.public_uri", "https://example.org/bell")]).await;
    assert_eq!(under.webhook(&body).await.status, StatusCode::UNAUTHORIZED);
    let answered = under
        .call(webhook_request(
            &body,
            Some(&sign("https://example.org/bell/v1/square/webhook", &body)),
        ))
        .await;
    assert_eq!(answered.status, StatusCode::OK, "{}", answered.text());
}

#[tokio::test]
async fn a_webhook_over_the_size_taken_is_refused_unread() {
    let bell = TestBell::without_square(&[]).await;
    let body = format!("{{\"pad\":\"{}\"}}", "x".repeat(256 * 1024));
    let answered = bell
        .call(webhook_request(&body, Some(&sign(WEBHOOK_URL, &body))))
        .await;
    assert_eq!(answered.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(answered.problem(), "payload-too-large");
}

/// A subscription the Bell would never send to is refused before Square
/// is asked: an onion address, plain `http` across a network, or a
/// private address or a `localhost` name while those aren't allowed.
#[tokio::test]
async fn a_subscription_the_bell_would_not_send_to_is_refused() {
    let bell = TestBell::without_square(&[("bell.allow_private_endpoints", "false")]).await;
    let till = Till {
        keys: push::encrypt::ReceiverKeys::generate(),
        endpoint: String::new(),
    };
    for endpoint in [
        "https://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/push/1",
        "http://push.example.org/push/1",
        "ftp://push.example.org/push/1",
        "https://user:pass@push.example.org/push/1",
        "https://10.0.0.7/push/1",
        "http://127.0.0.1:8080/push/1",
        "https://[::1]/push/1",
        "http://localhost:8080/push/1",
        "https://LOCALHOST./push/1",
        "https://push.localhost/push/1",
    ] {
        let mut subscription = till.subscription();
        subscription["endpoint"] = json!(endpoint);
        let answered = bell
            .call(registration(TOKEN, LOCATION, &subscription))
            .await;
        assert_eq!(
            answered.status,
            StatusCode::BAD_REQUEST,
            "{endpoint}: {}",
            answered.text()
        );
        assert_eq!(answered.problem(), "invalid-request");
    }
}

#[tokio::test]
async fn a_registration_that_doesnt_read_is_refused() {
    let bell = TestBell::without_square(&[]).await;
    let till = Till {
        keys: push::encrypt::ReceiverKeys::generate(),
        endpoint: "https://push.example.org/push/1".to_owned(),
    };
    let mut bad_keys = till.subscription();
    bad_keys["keys"]["p256dh"] = json!("AAAA");
    let cases = [
        json!({"location_id": LOCATION, "subscription": till.subscription()}),
        json!({"square_access_token": TOKEN, "location_id": "a/b", "subscription": till.subscription()}),
        json!({"square_access_token": "with space", "location_id": LOCATION, "subscription": till.subscription()}),
        json!({"square_access_token": TOKEN, "location_id": LOCATION, "subscription": bad_keys}),
        json!({"square_access_token": TOKEN, "location_id": LOCATION, "subscription": till.subscription(), "extra": 1}),
    ];
    for case in cases {
        let answered = bell
            .call(json_request(Method::POST, "/v1/register", case.clone()))
            .await;
        assert_eq!(
            answered.status,
            StatusCode::BAD_REQUEST,
            "{case}: {}",
            answered.text()
        );
        assert_eq!(answered.problem(), "invalid-request");
    }
}

/// Registrations are counted per address before anything else, so even
/// ones that would be refused use up the minute; behind a trusted proxy
/// the address is the one it forwarded.
#[tokio::test]
async fn registrations_are_limited_per_address() {
    let bell = TestBell::without_square(&[("bell.registrations_per_minute", "2")]).await;
    let attempt = || json_request(Method::POST, "/v1/register", json!({}));
    for _ in 0..2 {
        let answered = bell.call_from("192.0.2.7:1000", attempt()).await;
        assert_eq!(answered.status, StatusCode::BAD_REQUEST);
    }
    let limited = bell.call_from("192.0.2.7:1001", attempt()).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.problem(), "rate-limited");
    let retry_after: u64 = limited.headers["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&retry_after), "{retry_after}");
    // Another address has its own minute.
    assert_eq!(
        bell.call_from("192.0.2.8:1000", attempt()).await.status,
        StatusCode::BAD_REQUEST
    );

    // Through the proxy on this machine (trusted by default), each client
    // is counted by its own address, and the proxy's isn't used up.
    let forwarded = |client: &str| {
        let mut request = attempt();
        request
            .headers_mut()
            .insert("x-forwarded-for", client.parse().unwrap());
        request
    };
    for _ in 0..2 {
        let answered = bell
            .call_from("127.0.0.1:1000", forwarded("198.51.100.1"))
            .await;
        assert_eq!(answered.status, StatusCode::BAD_REQUEST);
    }
    let limited = bell
        .call_from("127.0.0.1:1000", forwarded("198.51.100.1"))
        .await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    let other = bell
        .call_from("127.0.0.1:1000", forwarded("198.51.100.2"))
        .await;
    assert_eq!(other.status, StatusCode::BAD_REQUEST);
    // A client not behind a trusted proxy can't name another address.
    let spoofed = bell
        .call_from("192.0.2.7:1002", forwarded("198.51.100.9"))
        .await;
    assert_eq!(spoofed.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn unregistering_an_id_that_isnt_one_is_unknown() {
    let bell = TestBell::without_square(&[]).await;
    for id in ["br_0123456789abcdef0123456789abcdef", "nope", "br_XYZ"] {
        let answered = bell.call(unregistration(id)).await;
        assert_eq!(answered.status, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(answered.problem(), "unknown-registration");
    }
}

/// A registration is three short fields and a subscription: one past the
/// 8 KiB the Bell reads is refused as too large, before it is parsed.
#[tokio::test]
async fn a_registration_over_the_size_taken_is_refused_unread() {
    let bell = TestBell::without_square(&[]).await;
    let at_limit = format!("{{\"pad\":\"{}\"}}", "x".repeat(8 * 1024 - 10));
    assert_eq!(at_limit.len(), 8 * 1024);
    let over = format!("{at_limit} ");
    let post = |body: String| {
        Request::post("/v1/register")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap()
    };
    // At the limit it is read, and refused for what it says.
    let read = bell.call(post(at_limit)).await;
    assert_eq!(read.status, StatusCode::BAD_REQUEST, "{}", read.text());
    assert_eq!(read.problem(), "invalid-request");
    let refused = bell.call(post(over)).await;
    assert_eq!(refused.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(refused.problem(), "payload-too-large");
}

/// A webhook of exactly the 256 KiB the Bell reads is read and checked;
/// the over-size refusal is for what is past it.
#[tokio::test]
async fn a_webhook_at_the_size_taken_is_read() {
    let bell = TestBell::without_square(&[]).await;
    let payment = payment_webhook("payment.updated", "APPROVED", MERCHANT, LOCATION);
    // Square's bodies are objects: pad inside one, as a long note would.
    let open = &payment[..payment.len() - 1];
    let pad = 256 * 1024 - open.len() - ",\"note\":\"\"}".len();
    let body = format!("{open},\"note\":\"{}\"}}", "x".repeat(pad));
    assert_eq!(body.len(), 256 * 1024);
    let answered = bell.webhook(&body).await;
    assert_eq!(answered.status, StatusCode::OK, "{}", answered.text());
    let unsigned = bell.call(webhook_request(&body, None)).await;
    assert_eq!(unsigned.problem(), "invalid-signature");
}

/// The signature is checked against the whole header value: one with
/// padding dropped, a URL-safe alphabet, spaces around it or a second
/// signature after it isn't Square's.
#[tokio::test]
async fn a_signature_that_is_not_exactly_squares_is_refused() {
    let bell = TestBell::without_square(&[]).await;
    let body = payment_webhook("payment.updated", "APPROVED", MERCHANT, LOCATION);
    let signature = sign(WEBHOOK_URL, &body);
    let url_safe = signature.replace('+', "-").replace('/', "_");
    let mut mangled = vec![
        signature.trim_end_matches('=').to_owned(),
        format!("{signature},{signature}"),
        format!("{signature}x"),
        String::new(),
    ];
    if url_safe != signature {
        mangled.push(url_safe);
    }
    for value in mangled {
        let answered = bell.call(webhook_request(&body, Some(&value))).await;
        assert_eq!(answered.status, StatusCode::UNAUTHORIZED, "{value:?}");
        assert_eq!(answered.problem(), "invalid-signature");
    }
    // Signed with another subscription's key.
    let mut other = signature.into_bytes();
    other[0] = if other[0] == b'A' { b'B' } else { b'A' };
    let answered = bell
        .call(webhook_request(
            &body,
            Some(std::str::from_utf8(&other).unwrap()),
        ))
        .await;
    assert_eq!(answered.status, StatusCode::UNAUTHORIZED);
}

/// IPv6 clients are counted by their /64, so one host can't take a fresh
/// minute by moving through its own prefix; the next /64 is another
/// client.
#[tokio::test]
async fn registrations_from_one_ipv6_prefix_share_a_limit() {
    let bell = TestBell::without_square(&[("bell.registrations_per_minute", "2")]).await;
    let attempt = || json_request(Method::POST, "/v1/register", json!({}));
    for peer in ["[2001:db8:1:2::1]:1000", "[2001:db8:1:2:ffff::9]:1000"] {
        assert_eq!(
            bell.call_from(peer, attempt()).await.status,
            StatusCode::BAD_REQUEST
        );
    }
    let limited = bell.call_from("[2001:db8:1:2::77]:1000", attempt()).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        bell.call_from("[2001:db8:1:3::1]:1000", attempt())
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

/// Behind the trusted proxy, the client is the address the proxy saw: what
/// a client writes into `X-Forwarded-For` itself, ahead of it, doesn't
/// give it a fresh minute.
#[tokio::test]
async fn a_forwarded_chain_is_counted_by_the_address_the_proxy_saw() {
    let bell = TestBell::without_square(&[("bell.registrations_per_minute", "2")]).await;
    let forwarded = |chain: &str| {
        let mut request = json_request(Method::POST, "/v1/register", json!({}));
        request
            .headers_mut()
            .insert("x-forwarded-for", chain.parse().unwrap());
        request
    };
    for spoofed in ["203.0.113.1", "203.0.113.2", "203.0.113.3"] {
        let answered = bell
            .call_from(
                "127.0.0.1:1000",
                forwarded(&format!("{spoofed}, 198.51.100.1")),
            )
            .await;
        let expected = if spoofed == "203.0.113.3" {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            StatusCode::BAD_REQUEST
        };
        assert_eq!(answered.status, expected, "{spoofed}");
    }
    // A header that isn't an address list leaves the proxy's own address.
    let garbage = bell
        .call_from("127.0.0.1:1000", forwarded("not-an-address"))
        .await;
    assert_eq!(garbage.status, StatusCode::BAD_REQUEST);
}

/// A proxy that adds its own `X-Forwarded-For` line, rather than joining
/// the client's, gives the client no fresh minute either: every line is
/// read, the proxy's last, so a different invented first line each time
/// still counts against the address the proxy saw.
#[tokio::test]
async fn registrations_behind_a_line_adding_proxy_are_limited_by_its_line() {
    let bell = TestBell::without_square(&[("bell.registrations_per_minute", "2")]).await;
    for spoofed in ["203.0.113.1", "203.0.113.2", "203.0.113.3"] {
        let mut request = json_request(Method::POST, "/v1/register", json!({}));
        let headers = request.headers_mut();
        headers.append("x-forwarded-for", spoofed.parse().unwrap());
        headers.append("x-forwarded-for", "198.51.100.1".parse().unwrap());
        let answered = bell.call_from("127.0.0.1:1000", request).await;
        let expected = if spoofed == "203.0.113.3" {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            StatusCode::BAD_REQUEST
        };
        assert_eq!(answered.status, expected, "{spoofed}");
    }
}

/// A subscription is refused whole when any part of it isn't one: an
/// endpoint past 2048 characters, an auth secret that isn't 16 bytes, a
/// p256dh that isn't a point on the curve, keys missing, or not JSON.
#[tokio::test]
async fn a_subscription_that_isnt_one_is_refused() {
    let bell = TestBell::without_square(&[]).await;
    let till = Till {
        keys: push::encrypt::ReceiverKeys::generate(),
        endpoint: "https://push.example.org/push/1".to_owned(),
    };
    let with = |pointer: &str, value: serde_json::Value| {
        let mut subscription = till.subscription();
        *subscription.pointer_mut(pointer).unwrap() = value;
        subscription
    };
    let mut no_keys = till.subscription();
    no_keys.as_object_mut().unwrap().remove("keys");
    // 65 bytes, 0x04 first, but not on the curve.
    let mut off_curve = [0u8; 65];
    off_curve[0] = 4;
    off_curve[64] = 1;
    let off_curve =
        base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, off_curve);
    for subscription in [
        with(
            "/endpoint",
            json!(format!("https://push.example.org/{}", "a".repeat(2048))),
        ),
        with("/keys/auth", json!("AAAAAAAAAAAAAAAAAAAAAAA")),
        with("/keys/auth", json!("not base64url!")),
        with("/keys/p256dh", json!(off_curve)),
        with("/endpoint", json!(42)),
        no_keys,
        json!("https://push.example.org/push/1"),
    ] {
        let answered = bell
            .call(registration(TOKEN, LOCATION, &subscription))
            .await;
        assert_eq!(
            answered.status,
            StatusCode::BAD_REQUEST,
            "{subscription}: {}",
            answered.text()
        );
        assert_eq!(answered.problem(), "invalid-request");
    }
    let not_json = bell
        .call(
            Request::post("/v1/register")
                .body(Body::from("square_access_token=x"))
                .unwrap(),
        )
        .await;
    assert_eq!(not_json.problem(), "invalid-request");
}

/// A refused registration's problem never repeats the access token, so a
/// detail shown or reported by the app can't carry it.
#[tokio::test]
async fn a_refused_registration_never_repeats_the_token() {
    let (_guard, log) = capture_log();
    let bell = TestBell::without_square(&[]).await;
    let till = Till {
        keys: push::encrypt::ReceiverKeys::generate(),
        endpoint: "https://push.example.org/push/1".to_owned(),
    };
    let long = format!("EAAAl-{}", "t".repeat(1100));
    let mut bad_keys = till.subscription();
    bad_keys["keys"]["auth"] = json!("AAAA");
    for (token, location, subscription) in [
        (TOKEN, "a/b", till.subscription()),
        (TOKEN, LOCATION, bad_keys),
        (long.as_str(), LOCATION, till.subscription()),
        ("EAAAl-with a space", LOCATION, till.subscription()),
    ] {
        let answered = bell
            .call(registration(token, location, &subscription))
            .await;
        assert_eq!(answered.status, StatusCode::BAD_REQUEST);
        assert!(
            !answered.text().contains("EAAAl-"),
            "the token is in the answer: {}",
            answered.text()
        );
    }
    assert!(!log.text().contains("EAAAl-"), "{}", log.text());
}

/// `/v1/info`'s key is a P-256 point a till's push subscription can be
/// made for, as browsers' `applicationServerKey` takes it.
#[tokio::test]
async fn the_info_key_is_an_uncompressed_p256_point() {
    let bell = TestBell::without_square(&[]).await;
    let info = bell
        .call(Request::get("/v1/info").body(Body::empty()).unwrap())
        .await
        .json();
    let key = info["vapid_public_key"].as_str().unwrap();
    let bytes =
        base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, key).unwrap();
    assert_eq!(bytes.len(), 65);
    assert_eq!(bytes[0], 4);
    assert_eq!(
        push::VapidPublicKey::from_base64url(key).unwrap(),
        vapid_public_key()
    );
}
