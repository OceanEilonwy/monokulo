//! The Bell asking a stand-in Square on loopback to check a registration,
//! and sending dings to a stand-in push service there, read back as a
//! till would read them.

mod support;

use axum::http::StatusCode;
use push::wire::PushRequest;
use push::{Message, Origin};
use support::*;

/// Registered with the merchant's token, the till is kept with the
/// merchant Square named for the location; the token went to Square once,
/// and is nowhere in the database or the log.
#[tokio::test]
async fn registration_over_http_keeps_the_merchant_square_names() {
    let (_guard, log) = capture_log();
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    let till = Till::at(&push, "till-1");

    let answered = bell.register(TOKEN, LOCATION, &till).await;
    assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.text());
    let id = answered.json()["registration_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(id.starts_with("br_") && id.len() == 35, "{id}");
    assert_eq!(answered.json()["square_application_id"], APPLICATION_ID);

    let asked = square.next().await;
    assert_eq!(asked.path, format!("/v2/locations/{LOCATION}"));
    assert_eq!(asked.header("authorization"), format!("Bearer {TOKEN}"));
    assert_eq!(asked.header("square-version"), "2026-09-16");

    let rows = bell.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (&rows[0].0, rows[0].1.as_str(), rows[0].2.as_str()),
        (&id, MERCHANT, LOCATION)
    );
    let stored = String::from_utf8_lossy(&bell.stored_bytes()).into_owned();
    assert!(!stored.contains(TOKEN), "the token is in the database");
    let log = log.text();
    assert!(log.contains("registered"), "{log}");
    for secret in [TOKEN, till.endpoint.as_str()] {
        assert!(!log.contains(secret), "{secret} is in the log: {log}");
    }

    // Registered again, the till keeps its registration.
    let again = bell.registered(TOKEN, LOCATION, &till).await;
    assert_eq!(again, id);
    assert_eq!(bell.rows().len(), 1);
}

/// A token Square refuses, and a location that isn't the token's
/// merchant's, are refused as Square refused them, and nothing is kept.
#[tokio::test]
async fn a_refused_token_or_another_merchants_location_is_square_refused_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    let till = Till::at(&push, "till-1");
    for (token, location) in [
        ("EAAAl-a-token-square-never-gave", LOCATION),
        (TOKEN, OTHER_MERCHANTS_LOCATION),
        (OTHER_TOKEN, LOCATION),
    ] {
        let answered = bell.register(token, location, &till).await;
        assert_eq!(
            answered.status,
            StatusCode::UNAUTHORIZED,
            "{}",
            answered.text()
        );
        assert_eq!(answered.problem(), "square-refused");
    }
    assert!(bell.rows().is_empty());
}

/// Square down, slow to the point of the timeout, limiting the Bell, or
/// not there at all: try again later.
#[tokio::test]
async fn a_square_that_cant_answer_is_square_unavailable_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    let till = Till::at(&push, "till-1");
    for status in [500, 503, 429] {
        square.answer(move |_| Answer::status(status));
        let answered = bell.register(TOKEN, LOCATION, &till).await;
        assert_eq!(answered.status, StatusCode::SERVICE_UNAVAILABLE, "{status}");
        assert_eq!(answered.problem(), "square-unavailable");
    }
    square.answer(|_| Answer::status(200).json(serde_json::json!({"nothing": "here"})));
    let answered = bell.register(TOKEN, LOCATION, &till).await;
    assert_eq!(answered.problem(), "square-unavailable");

    let gone = TestBell::start(&unreachable_url(), &[]).await;
    let answered = gone.register(TOKEN, LOCATION, &till).await;
    assert_eq!(answered.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(answered.problem(), "square-unavailable");
    assert!(bell.rows().is_empty() && gone.rows().is_empty());
}

/// A completed payment dings each till at its merchant's location, and
/// only those: encrypted `{"wake":"square"}`, high urgency, a minute to
/// live, topic `wake`, signed with the Bell's VAPID key for the push
/// service.
#[tokio::test]
async fn a_completed_payment_dings_every_till_at_its_location_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    let (front, back) = (Till::at(&push, "front"), Till::at(&push, "back"));
    let elsewhere = Till::at(&push, "elsewhere");
    let other_merchant = Till::at(&push, "other-merchant");
    bell.registered(TOKEN, LOCATION, &front).await;
    bell.registered(TOKEN, LOCATION, &back).await;
    bell.registered(TOKEN, OTHER_LOCATION, &elsewhere).await;
    bell.registered(OTHER_TOKEN, OTHER_MERCHANTS_LOCATION, &other_merchant)
        .await;

    let answered = bell
        .webhook(&payment_webhook(
            "payment.created",
            "COMPLETED",
            MERCHANT,
            LOCATION,
        ))
        .await;
    assert_eq!(answered.status, StatusCode::OK, "{}", answered.text());

    let mut dings = push.drain().await;
    dings.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(
        dings
            .iter()
            .map(|ding| ding.path.as_str())
            .collect::<Vec<_>>(),
        ["/push/back", "/push/front"]
    );
    let mut private = [0u8; 32];
    hex::decode_to_slice(VAPID_HEX, &mut private).unwrap();
    let bell_key = push::VapidKey::from_bytes(&private).unwrap().public();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    for (ding, till) in dings.iter().zip([&back, &front]) {
        assert_eq!(ding.header("urgency"), "high");
        assert_eq!(ding.header("ttl"), "60");
        assert_eq!(ding.header("topic"), "wake");
        assert_eq!(ding.header("content-encoding"), "aes128gcm");
        // Read as the push service reads it, signed as it checks.
        let request = PushRequest::parse(&ding.headers, &ding.body).unwrap();
        let claims = push::vapid::verify(
            &request.authorization,
            &bell_key,
            &Origin::parse(&push.url()).unwrap(),
            now,
        )
        .unwrap();
        assert_eq!(
            claims.sub.map(|sub| sub.to_string()).as_deref(),
            Some("https://bell.example.org/")
        );
        let Message::Encrypted(body) = request.message else {
            panic!("a ding is encrypted");
        };
        let plaintext = push::encrypt::decrypt(&till.keys, body.as_bytes()).unwrap();
        assert_eq!(plaintext, br#"{"wake":"square"}"#);
    }
    // Each took it: noted, with nothing failed.
    let rows = bell.rows();
    for row in &rows {
        let at_location = row.2 == LOCATION && row.1 == MERCHANT;
        assert_eq!(row.3.is_some(), at_location, "{row:?}");
        assert_eq!(row.4, 0);
    }

    // payment.updated to COMPLETED (a card payment captured) dings too.
    bell.webhook(&payment_webhook(
        "payment.updated",
        "COMPLETED",
        MERCHANT,
        OTHER_LOCATION,
    ))
    .await;
    let dings = push.drain().await;
    assert_eq!(
        dings
            .iter()
            .map(|ding| ding.path.as_str())
            .collect::<Vec<_>>(),
        ["/push/elsewhere"]
    );
}

/// A payment that hasn't completed, and every other event, ding nobody.
#[tokio::test]
async fn other_events_ding_nothing_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    bell.registered(TOKEN, LOCATION, &Till::at(&push, "front"))
        .await;
    let refund = payment_webhook("payment.created", "COMPLETED", MERCHANT, LOCATION)
        .replace("payment.created", "refund.created");
    for body in [
        payment_webhook("payment.updated", "APPROVED", MERCHANT, LOCATION),
        payment_webhook("payment.created", "PENDING", MERCHANT, LOCATION),
        payment_webhook("payment.updated", "FAILED", MERCHANT, LOCATION),
        payment_webhook("payment.updated", "CANCELED", MERCHANT, LOCATION),
        refund,
        // Completed, but for a location (and merchant) nobody registered.
        payment_webhook("payment.created", "COMPLETED", MERCHANT, OTHER_LOCATION),
        payment_webhook("payment.created", "COMPLETED", OTHER_MERCHANT, LOCATION),
        "{\"type\":\"payment.created\",\"merchant_id\":\"x\"}".to_owned(),
        "not json".to_owned(),
    ] {
        let answered = bell.webhook(&body).await;
        assert_eq!(answered.status, StatusCode::OK, "{body}");
    }
    assert!(push.drain().await.is_empty());
    // The registration is still there, and dinged by the next payment.
    bell.webhook(&payment_webhook(
        "payment.created",
        "COMPLETED",
        MERCHANT,
        LOCATION,
    ))
    .await;
    assert_eq!(push.drain().await.len(), 1);
}

/// A push service that says the subscription is gone (`410`, or `404`)
/// has its registration deleted at once.
#[tokio::test]
async fn a_gone_subscription_deletes_its_registration_over_http() {
    let square = Stub::square().await;
    let push = Stub::start(|received| {
        if received.path.ends_with("/gone") {
            Answer::status(410)
        } else if received.path.ends_with("/missing") {
            Answer::status(404)
        } else {
            Answer::status(201)
        }
    })
    .await;
    let bell = TestBell::start(&square.url(), &[]).await;
    let kept = bell
        .registered(TOKEN, LOCATION, &Till::at(&push, "kept"))
        .await;
    bell.registered(TOKEN, LOCATION, &Till::at(&push, "gone"))
        .await;
    bell.registered(TOKEN, LOCATION, &Till::at(&push, "missing"))
        .await;
    bell.webhook(&payment_webhook(
        "payment.created",
        "COMPLETED",
        MERCHANT,
        LOCATION,
    ))
    .await;
    assert_eq!(push.drain().await.len(), 3);
    let rows = bell.rows();
    assert_eq!(rows.iter().map(|row| &row.0).collect::<Vec<_>>(), [&kept]);
}

/// Dings a push service doesn't take are counted, and at
/// `bell.registration_failure_limit` in a row the registration goes; one
/// taken starts the count again, and a push service limiting the Bell
/// isn't the subscription's failure.
#[tokio::test]
async fn failing_dings_delete_a_registration_at_the_limit_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[("bell.registration_failure_limit", "3")]).await;
    let id = bell
        .registered(TOKEN, LOCATION, &Till::at(&push, "front"))
        .await;
    let ring = || payment_webhook("payment.created", "COMPLETED", MERCHANT, LOCATION);

    push.answer(|_| Answer::status(500));
    bell.webhook(&ring()).await;
    push.answer(|_| Answer::status(400));
    bell.webhook(&ring()).await;
    assert_eq!(bell.rows()[0].4, 2);
    push.answer(|_| Answer::status(429).header("retry-after", "30"));
    bell.webhook(&ring()).await;
    assert_eq!(bell.rows()[0].4, 2, "a rate limit isn't a failure");
    push.answer(|_| Answer::status(201));
    bell.webhook(&ring()).await;
    assert_eq!(bell.rows()[0].4, 0);

    push.answer(|_| Answer::status(503));
    for _ in 0..2 {
        bell.webhook(&ring()).await;
    }
    assert_eq!(bell.rows()[0].0, id);
    bell.webhook(&ring()).await;
    assert!(
        bell.rows().is_empty(),
        "deleted at the third failure in a row"
    );
    assert_eq!(push.drain().await.len(), 7);
}

/// A push service that can't be reached fails the ding like any other.
#[tokio::test]
async fn an_unreachable_push_service_counts_as_a_failure_over_http() {
    let square = Stub::square().await;
    let bell = TestBell::start(&square.url(), &[("bell.registration_failure_limit", "1")]).await;
    let till = Till {
        keys: push::encrypt::ReceiverKeys::generate(),
        endpoint: format!("{}/push/1", unreachable_url()),
    };
    bell.registered(TOKEN, LOCATION, &till).await;
    bell.webhook(&payment_webhook(
        "payment.created",
        "COMPLETED",
        MERCHANT,
        LOCATION,
    ))
    .await;
    assert!(bell.rows().is_empty());
}

/// A webhook with a payment's amounts, card and buyer leaves none of them
/// in the database or the log, at any level.
#[tokio::test]
async fn a_webhook_leaves_no_trace_of_its_payment_over_http() {
    let (_guard, log) = capture_log();
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    let till = Till::at(&push, "front");
    bell.registered(TOKEN, LOCATION, &till).await;
    for body in [
        payment_webhook_of(
            PAYMENT_ID,
            "payment.created",
            "COMPLETED",
            MERCHANT,
            LOCATION,
        ),
        payment_webhook_of(
            PAYMENT_ID,
            "payment.updated",
            "APPROVED",
            MERCHANT,
            LOCATION,
        ),
        payment_webhook_of(
            PAYMENT_ID,
            "payment.created",
            "COMPLETED",
            MERCHANT,
            LOCATION,
        )
        .replace(LOCATION, "a/b"),
    ] {
        assert_eq!(bell.webhook(&body).await.status, StatusCode::OK);
    }
    let unsigned = payment_webhook("payment.created", "COMPLETED", MERCHANT, LOCATION);
    let refused = bell
        .call(webhook_request(
            &unsigned,
            Some(&sign(&format!("{WEBHOOK_URL}/"), &unsigned)),
        ))
        .await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert!(!refused.text().contains(CARD_LAST_4));
    assert_eq!(push.drain().await.len(), 1);

    let log = log.text();
    assert!(log.contains("a signed webhook was dropped"), "{log}");
    let stored = String::from_utf8_lossy(&bell.stored_bytes()).into_owned();
    for trace in [
        CARD_LAST_4,
        FINGERPRINT,
        BUYER_EMAIL,
        PAYMENT_ID,
        &AMOUNT.to_string(),
        "VISA",
        "424242",
        "13b867cf-db3d-4b1c-90b6-2f32a9d78124",
        "uZhPq6R6rkK3ZT9lYBJ8NawqHTMZY",
        TOKEN,
    ] {
        assert!(!log.contains(trace), "{trace} is in the log: {log}");
        assert!(!stored.contains(trace), "{trace} is in the database");
    }
    // The endpoint is kept, to ding, and never logged.
    assert!(
        !log.contains(&till.endpoint),
        "the endpoint is in the log: {log}"
    );
}

/// Unregistering takes the registration back once: then it is unknown,
/// and its till isn't dinged.
#[tokio::test]
async fn unregistering_twice_is_no_content_then_unknown_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    let id = bell
        .registered(TOKEN, LOCATION, &Till::at(&push, "front"))
        .await;
    let first = bell.call(unregistration(&id)).await;
    assert_eq!(first.status, StatusCode::NO_CONTENT);
    assert!(first.body.is_empty());
    let second = bell.call(unregistration(&id)).await;
    assert_eq!(second.status, StatusCode::NOT_FOUND);
    assert_eq!(second.problem(), "unknown-registration");
    bell.webhook(&payment_webhook(
        "payment.created",
        "COMPLETED",
        MERCHANT,
        LOCATION,
    ))
    .await;
    assert!(push.drain().await.is_empty());
}

/// A location takes so many tills; one more is refused until one goes.
#[tokio::test]
async fn a_full_location_refuses_another_till_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[("bell.registrations_per_minute", "100")]).await;
    let mut first = None;
    for n in 0..32 {
        let id = bell
            .registered(TOKEN, LOCATION, &Till::at(&push, &n.to_string()))
            .await;
        first.get_or_insert(id);
    }
    let answered = bell.register(TOKEN, LOCATION, &Till::at(&push, "33")).await;
    assert_eq!(answered.status, StatusCode::CONFLICT);
    assert_eq!(answered.problem(), "too-many-registrations");
    bell.call(unregistration(&first.unwrap())).await;
    bell.registered(TOKEN, LOCATION, &Till::at(&push, "33"))
        .await;
}

/// The sandbox's Bell asks the Square `bell.square.api_url` names, as it
/// would the sandbox's own.
#[tokio::test]
async fn a_sandbox_bell_asks_the_square_it_is_pointed_at_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(
        &square.url(),
        &[
            ("bell.square.environment", "sandbox"),
            ("bell.square.application_id", "sandbox-sq0idb-MonokularTest"),
        ],
    )
    .await;
    let answered = bell
        .register(TOKEN, LOCATION, &Till::at(&push, "front"))
        .await;
    assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.text());
    assert_eq!(
        answered.json()["square_application_id"],
        "sandbox-sq0idb-MonokularTest"
    );
    assert_eq!(
        square.next().await.path,
        format!("/v2/locations/{LOCATION}")
    );
}

/// A Square that redirects isn't followed: the merchant's token goes to
/// the API it was configured for and nowhere else.
#[tokio::test]
async fn a_redirecting_square_never_gets_the_token_followed_over_http() {
    let elsewhere = Stub::start(|_| Answer::status(200)).await;
    let square = Stub::start({
        let target = format!("{}/v2/locations/{LOCATION}", elsewhere.url());
        move |_| Answer::status(307).header("location", &target)
    })
    .await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    let answered = bell
        .register(TOKEN, LOCATION, &Till::at(&push, "front"))
        .await;
    assert_ne!(answered.status, StatusCode::CREATED, "{}", answered.text());
    assert_eq!(answered.problem(), "square-refused");
    assert_eq!(square.drain().await.len(), 1);
    assert!(
        elsewhere.drain().await.is_empty(),
        "the redirect was followed"
    );
    assert!(bell.rows().is_empty());
}

/// A Square slower than `bell.timeout_ms` is unavailable, not waited on.
#[tokio::test]
async fn a_square_slower_than_the_timeout_is_unavailable_over_http() {
    let (_silent, square) = silent_url();
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square, &[("bell.timeout_ms", "100")]).await;
    let started = std::time::Instant::now();
    let answered = bell
        .register(TOKEN, LOCATION, &Till::at(&push, "front"))
        .await;
    assert_eq!(answered.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(answered.problem(), "square-unavailable");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(bell.rows().is_empty());
}

/// Square's `200` is believed only for the location asked about, with a
/// merchant that is one: a merchant id that isn't one, or no merchant, is
/// a Square that couldn't be asked; another location is the till's
/// mistake (an ID that isn't the location's own), not to be retried.
/// Nothing is kept.
#[tokio::test]
async fn a_square_answer_that_isnt_the_location_asked_for_is_refused_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    let till = Till::at(&push, "front");
    square.answer(|_| {
        Answer::status(200)
            .json(serde_json::json!({"location": {"id": OTHER_LOCATION, "merchant_id": MERCHANT}}))
    });
    let answered = bell.register(TOKEN, LOCATION, &till).await;
    assert_eq!(
        answered.status,
        StatusCode::BAD_REQUEST,
        "{}",
        answered.text()
    );
    assert_eq!(answered.problem(), "invalid-request");
    for location in [
        serde_json::json!({"id": LOCATION, "merchant_id": "not/a/merchant"}),
        serde_json::json!({"id": LOCATION}),
        serde_json::json!({"id": LOCATION, "merchant_id": ""}),
    ] {
        square.answer(move |_| {
            Answer::status(200).json(serde_json::json!({"location": location.clone()}))
        });
        let answered = bell.register(TOKEN, LOCATION, &till).await;
        assert_eq!(
            answered.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{}",
            answered.text()
        );
        assert_eq!(answered.problem(), "square-unavailable");
    }
    // A 403 (the token lacks the scope) and any other 4xx are refusals.
    for status in [403, 400] {
        square.answer(move |_| Answer::status(status));
        assert_eq!(
            bell.register(TOKEN, LOCATION, &till).await.problem(),
            "square-refused",
            "{status}"
        );
    }
    assert!(bell.rows().is_empty());
}

/// Whatever Square answers, and whatever it says in its errors, the token
/// is in no log line and no answer.
#[tokio::test]
async fn a_failed_check_leaves_the_token_nowhere_over_http() {
    let (_guard, log) = capture_log();
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    let till = Till::at(&push, "front");
    // Square echoing the token back in its error is still never repeated.
    for status in [401, 404, 500, 429] {
        square.answer(move |_| {
            Answer::status(status).json(serde_json::json!({"errors": [{"detail": TOKEN}]}))
        });
        let answered = bell.register(TOKEN, LOCATION, &till).await;
        assert!(!answered.text().contains(TOKEN), "{}", answered.text());
    }
    square.answer(|_| Answer::status(200).json(serde_json::json!({"location": TOKEN})));
    let answered = bell.register(TOKEN, LOCATION, &till).await;
    assert_eq!(answered.problem(), "square-unavailable");
    assert!(!answered.text().contains(TOKEN), "{}", answered.text());
    let log = log.text();
    assert!(log.contains("Square"), "{log}");
    assert!(!log.contains(TOKEN), "the token is in the log: {log}");
    assert!(!String::from_utf8_lossy(&bell.stored_bytes()).contains(TOKEN));
}

/// A push service that redirects isn't followed (a redirect could point
/// inside the Bell's network): the ding fails and is counted.
#[tokio::test]
async fn a_redirecting_push_service_is_not_followed_over_http() {
    let square = Stub::square().await;
    let elsewhere = Stub::push_service().await;
    let push = Stub::start({
        let target = format!("{}/push/elsewhere", elsewhere.url());
        move |_| Answer::status(307).header("location", &target)
    })
    .await;
    let bell = TestBell::start(&square.url(), &[]).await;
    bell.registered(TOKEN, LOCATION, &Till::at(&push, "front"))
        .await;
    bell.webhook(&completed_payment()).await;
    assert_eq!(push.drain().await.len(), 1);
    assert!(
        elsewhere.drain().await.is_empty(),
        "the redirect was followed"
    );
    let rows = bell.rows();
    assert_eq!((rows[0].3, rows[0].4), (None, 1));
}

/// Square is answered at once: the dings go out after, on their own, so a
/// push service slower than `bell.timeout_ms` keeps neither Square waiting
/// nor the ding: it fails at the timeout, and is counted.
#[tokio::test]
async fn a_webhook_is_answered_before_a_slow_ding_times_out_over_http() {
    let square = Stub::square().await;
    let (_silent, slow) = silent_url();
    let bell = TestBell::start(&square.url(), &[("bell.timeout_ms", "1000")]).await;
    let till = Till {
        keys: push::encrypt::ReceiverKeys::generate(),
        endpoint: format!("{slow}/push/front"),
    };
    bell.registered(TOKEN, LOCATION, &till).await;
    let body = completed_payment();
    let answered = bell
        .call(webhook_request(&body, Some(&sign(WEBHOOK_URL, &body))))
        .await;
    assert_eq!(answered.status, StatusCode::OK);
    // The ding is still waiting on the push service.
    let early = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        bell.bell.dings_sent(),
    )
    .await;
    assert!(early.is_err(), "the webhook waited for its ding");
    tokio::time::timeout(std::time::Duration::from_secs(10), bell.bell.dings_sent())
        .await
        .expect("the ding gave up at the timeout");
    assert_eq!(bell.rows()[0].4, 1);
}

/// Every answer that refuses the ding without saying the subscription is
/// gone counts against it: too large, unauthorised, forbidden, a client
/// error of the push service's own.
#[tokio::test]
async fn a_push_service_refusing_a_ding_counts_a_failure_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    bell.registered(TOKEN, LOCATION, &Till::at(&push, "front"))
        .await;
    let refusals = [413, 401, 403, 400, 502];
    for (n, status) in refusals.into_iter().enumerate() {
        push.answer(move |_| Answer::status(status));
        bell.webhook(&completed_payment()).await;
        assert_eq!(bell.rows()[0].4, n as i64 + 1, "{status}");
    }
    // A 200 or 202 is a ding taken, as 201 is.
    for status in [200, 202] {
        push.answer(move |_| Answer::status(status));
        bell.webhook(&completed_payment()).await;
        assert_eq!(bell.rows()[0].4, 0, "{status}");
        assert!(bell.rows()[0].3.is_some());
    }
}

/// Each push service gets a token for its own origin, signed with the
/// Bell's key, unexpired and at most a day ahead (RFC 8292): one taken to
/// another push service, checked with another key or past its expiry
/// doesn't verify.
#[tokio::test]
async fn each_push_service_gets_a_vapid_token_for_its_own_origin_over_http() {
    let square = Stub::square().await;
    let (one, two) = (Stub::push_service().await, Stub::push_service().await);
    let bell = TestBell::start(&square.url(), &[]).await;
    bell.registered(TOKEN, LOCATION, &Till::at(&one, "front"))
        .await;
    bell.registered(TOKEN, LOCATION, &Till::at(&two, "back"))
        .await;
    bell.webhook(&completed_payment()).await;
    let now = now();
    let key = vapid_public_key();
    let (at_one, at_two) = (one.next().await, two.next().await);
    let read = |ding: &Received| PushRequest::parse(&ding.headers, &ding.body).unwrap();
    let (at_one, at_two) = (read(&at_one), read(&at_two));
    let origin = |stub: &Stub| Origin::parse(&stub.url()).unwrap();
    for (request, own, other) in [
        (&at_one, origin(&one), origin(&two)),
        (&at_two, origin(&two), origin(&one)),
    ] {
        assert_eq!(*request.authorization.key(), key);
        let claims = push::vapid::verify(&request.authorization, &key, &own, now).unwrap();
        assert_eq!(claims.aud, own);
        assert!(
            claims.exp > now + 11 * 3600 && claims.exp <= now + 24 * 3600,
            "{}",
            claims.exp - now
        );
        assert!(matches!(
            push::vapid::verify(&request.authorization, &key, &other, now),
            Err(push::vapid::VapidError::Audience)
        ));
        assert!(matches!(
            push::vapid::verify(&request.authorization, &key, &own, claims.exp),
            Err(push::vapid::VapidError::Expired)
        ));
        let forged = push::VapidKey::generate().public();
        assert!(matches!(
            push::vapid::verify(&request.authorization, &forged, &own, now),
            Err(push::vapid::VapidError::KeyMismatch)
        ));
    }
}

/// Private endpoints turned off after tills registered on them: every
/// ding to one is refused before a request, by its IP literal or its
/// `localhost` name, and counted, so the registration goes at the limit;
/// nothing reaches the push service.
#[tokio::test]
async fn private_endpoints_turned_off_are_never_dinged_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let open = TestBell::start(&square.url(), &[]).await;
    let by_ip = Till::at(&push, "by-ip");
    let by_name = Till {
        keys: push::encrypt::ReceiverKeys::generate(),
        endpoint: format!("http://localhost:{}/push/by-name", push.address.port()),
    };
    open.registered(TOKEN, LOCATION, &by_ip).await;
    open.registered(TOKEN, LOCATION, &by_name).await;
    let store_path = open.store_path.to_str().unwrap().to_owned();
    drop(open);

    let closed = TestBell::start(
        &square.url(),
        &[
            ("bell.store_path", &store_path),
            ("bell.allow_private_endpoints", "false"),
            ("bell.registration_failure_limit", "2"),
        ],
    )
    .await;
    closed.webhook(&completed_payment()).await;
    assert!(
        closed.rows().iter().all(|row| row.4 == 1),
        "{:?}",
        closed.rows()
    );
    closed.webhook(&completed_payment()).await;
    assert!(push.drain().await.is_empty(), "a ding reached loopback");
    assert!(closed.rows().is_empty(), "deleted at the limit");
}

/// A till registered again with new keys (its app reinstalled), or moved
/// to another location, is dinged where it is now and with the keys it
/// has now; once unregistered, registering again makes a new
/// registration.
#[tokio::test]
async fn a_till_registered_again_is_dinged_where_and_as_it_is_now_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    let old = Till::at(&push, "front");
    let id = bell.registered(TOKEN, LOCATION, &old).await;
    let new = Till {
        keys: push::encrypt::ReceiverKeys::generate(),
        endpoint: old.endpoint.clone(),
    };
    assert_eq!(bell.registered(TOKEN, OTHER_LOCATION, &new).await, id);

    bell.webhook(&completed_payment()).await;
    assert!(push.drain().await.is_empty(), "dinged where it was");
    bell.webhook(&payment_webhook(
        "payment.created",
        "COMPLETED",
        MERCHANT,
        OTHER_LOCATION,
    ))
    .await;
    let ding = push.next().await;
    let request = PushRequest::parse(&ding.headers, &ding.body).unwrap();
    let Message::Encrypted(body) = request.message else {
        panic!("a ding is encrypted");
    };
    assert_eq!(
        push::encrypt::decrypt(&new.keys, body.as_bytes()).unwrap(),
        br#"{"wake":"square"}"#
    );
    assert!(push::encrypt::decrypt(&old.keys, body.as_bytes()).is_err());

    assert_eq!(
        bell.call(unregistration(&id)).await.status,
        StatusCode::NO_CONTENT
    );
    let again = bell.registered(TOKEN, OTHER_LOCATION, &new).await;
    assert_ne!(again, id);
    assert_eq!(
        bell.call(unregistration(&id)).await.status,
        StatusCode::NOT_FOUND
    );
}

/// Each ding is encrypted afresh: two dings to one till share no salt or
/// key, so a push service can't tell they say the same.
#[tokio::test]
async fn every_ding_is_encrypted_afresh_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    bell.registered(TOKEN, LOCATION, &Till::at(&push, "front"))
        .await;
    bell.webhook(&completed_payment()).await;
    bell.webhook(&completed_payment()).await;
    let dings = push.drain().await;
    assert_eq!(dings.len(), 2);
    // RFC 8188's header: a 16-byte salt, then the record size, the key's
    // length and the sender's ephemeral key.
    assert_ne!(dings[0].body[..16], dings[1].body[..16], "the salt");
    assert_ne!(dings[0].body[21..86], dings[1].body[21..86], "the key");
    assert_ne!(dings[0].body, dings[1].body);
}

/// Registrations racing for a location's last places fill it exactly to
/// its bound, never past it: the count and the insert are one
/// transaction.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_registrations_fill_a_location_to_its_bound_exactly_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = std::sync::Arc::new(
        TestBell::start(&square.url(), &[("bell.registrations_per_minute", "1000")]).await,
    );
    let mut racing = tokio::task::JoinSet::new();
    for n in 0..40 {
        let (bell, till) = (bell.clone(), Till::at(&push, &n.to_string()));
        racing.spawn(async move { bell.register(TOKEN, LOCATION, &till).await.status });
    }
    let mut statuses = Vec::new();
    while let Some(status) = racing.join_next().await {
        statuses.push(status.unwrap());
    }
    let created = statuses
        .iter()
        .filter(|status| **status == StatusCode::CREATED)
        .count();
    let full = statuses
        .iter()
        .filter(|status| **status == StatusCode::CONFLICT)
        .count();
    assert_eq!((created, full), (32, 8), "{statuses:?}");
    assert_eq!(bell.rows().len(), 32);
}

/// Payments arriving together, every ding failing: each failure is
/// counted once, the registration goes at the limit exactly, and nothing
/// fails to be recorded on the way.
#[tokio::test]
async fn concurrent_failing_dings_delete_a_registration_once_over_http() {
    let (_guard, log) = capture_log();
    let square = Stub::square().await;
    let push = Stub::start(|_| Answer::status(500)).await;
    let bell = TestBell::start(&square.url(), &[("bell.registration_failure_limit", "5")]).await;
    bell.registered(TOKEN, LOCATION, &Till::at(&push, "front"))
        .await;
    let bodies: Vec<String> = (0..8).map(|_| completed_payment()).collect();
    let mut calls = Vec::new();
    for body in &bodies {
        calls.push(bell.call(webhook_request(body, Some(&sign(WEBHOOK_URL, body)))));
    }
    for call in calls {
        assert_eq!(call.await.status, StatusCode::OK);
    }
    bell.bell.dings_sent().await;
    let sent = push.drain().await.len();
    assert!((5..=8).contains(&sent), "{sent} dings");
    assert!(bell.rows().is_empty());
    let log = log.text();
    assert_eq!(
        log.matches("dings failed too often in a row").count(),
        1,
        "{log}"
    );
    assert!(!log.contains("couldn't be"), "{log}");
}

/// `main`, Square's alias for the merchant's main location, reads as a
/// location ID, but Square answers it with the location's own: a
/// registration by it would never ding (webhooks name the real ID), so it
/// is refused as the till's mistake, `400` and not "try again later", and
/// nothing is kept.
#[tokio::test]
async fn a_location_named_by_squares_main_alias_is_refused_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    let answered = bell
        .register(TOKEN, "main", &Till::at(&push, "front"))
        .await;
    assert_eq!(
        answered.status,
        StatusCode::BAD_REQUEST,
        "{}",
        answered.text()
    );
    assert_eq!(answered.problem(), "invalid-request");
    assert!(
        answered.json()["detail"]
            .as_str()
            .unwrap()
            .contains("own ID")
    );
    assert_eq!(square.next().await.path, "/v2/locations/main");
    assert!(bell.rows().is_empty());
}

/// An endpoint alone doesn't give its registration away: another
/// merchant registering a till's endpoint with keys of its own is refused
/// (`409`, and no registration id), and the till's own merchant's payments
/// still ding it with its own keys. The till itself, signed in to another
/// merchant's account, moves its registration with the keys it registered.
#[tokio::test]
async fn another_merchant_cant_take_a_tills_registration_by_its_endpoint_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    let till = Till::at(&push, "front");
    let id = bell.registered(TOKEN, LOCATION, &till).await;

    let attacker = Till {
        keys: push::encrypt::ReceiverKeys::generate(),
        endpoint: till.endpoint.clone(),
    };
    let answered = bell
        .register(OTHER_TOKEN, OTHER_MERCHANTS_LOCATION, &attacker)
        .await;
    assert_eq!(answered.status, StatusCode::CONFLICT, "{}", answered.text());
    assert_eq!(answered.problem(), "endpoint-registered");
    assert!(!answered.text().contains(&id), "{}", answered.text());
    let rows = bell.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (&rows[0].0, rows[0].1.as_str(), rows[0].2.as_str()),
        (&id, MERCHANT, LOCATION)
    );

    bell.webhook(&payment_webhook(
        "payment.created",
        "COMPLETED",
        OTHER_MERCHANT,
        OTHER_MERCHANTS_LOCATION,
    ))
    .await;
    assert!(push.drain().await.is_empty(), "dinged for the attacker");
    bell.webhook(&completed_payment()).await;
    let ding = push.next().await;
    let request = PushRequest::parse(&ding.headers, &ding.body).unwrap();
    let Message::Encrypted(body) = request.message else {
        panic!("a ding is encrypted");
    };
    assert_eq!(
        push::encrypt::decrypt(&till.keys, body.as_bytes()).unwrap(),
        br#"{"wake":"square"}"#
    );

    // The till, with its own keys, signed in to the other merchant.
    assert_eq!(
        bell.registered(OTHER_TOKEN, OTHER_MERCHANTS_LOCATION, &till)
            .await,
        id
    );
    assert_eq!(bell.rows()[0].1, OTHER_MERCHANT);
}

/// A payment rings once. Square sends `payment.updated` of a payment that
/// stays `COMPLETED` when its processing fee is filled in, and again when
/// it is refunded: neither is a sale, and on an iPhone or iPad a ding is
/// an alert. Another payment rings again; a refund of a payment rung long
/// ago (or before a restart) doesn't.
#[tokio::test]
async fn a_payment_rings_once_and_a_refund_never_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(&square.url(), &[]).await;
    bell.registered(TOKEN, LOCATION, &Till::at(&push, "front"))
        .await;
    let event =
        |id: &str, kind: &str| payment_webhook_of(id, kind, "COMPLETED", MERCHANT, LOCATION);
    let refunded = |id: &str| {
        let mut body: serde_json::Value =
            serde_json::from_str(&event(id, "payment.updated")).unwrap();
        body["data"]["object"]["payment"]["refunded_money"] =
            serde_json::json!({"amount": AMOUNT, "currency": "USD"});
        body.to_string()
    };
    let p = "PaymentPPPPPPPPPPPPPPPPPPPPPP";
    for body in [
        event(p, "payment.created"),
        // The fee filled in.
        event(p, "payment.updated"),
        refunded(p),
    ] {
        assert_eq!(bell.webhook(&body).await.status, StatusCode::OK);
    }
    assert_eq!(push.drain().await.len(), 1, "one payment, one ding");

    bell.webhook(&event("PaymentQQQQQQQQQQQQQQQQQQQQQQ", "payment.created"))
        .await;
    assert_eq!(push.drain().await.len(), 1, "another payment dings");

    // A refund of a payment this Bell never rang.
    bell.webhook(&refunded("PaymentRRRRRRRRRRRRRRRRRRRRRR"))
        .await;
    assert!(push.drain().await.is_empty(), "a refund dinged");
}

/// Each merchant's tills share `bell.dings_per_merchant_per_minute`
/// across its locations, and another merchant has a share of its own:
/// every ding is signed with the Bell's one key, which a push relay
/// counts as one sender, so one merchant's payments can't spend what the
/// relay gives every merchant. A ding over its merchant's share is
/// dropped, not counted as its till's failure.
#[tokio::test]
async fn one_merchant_cant_spend_another_merchants_dings_over_http() {
    let square = Stub::square().await;
    let push = Stub::push_service().await;
    let bell = TestBell::start(
        &square.url(),
        &[("bell.dings_per_merchant_per_minute", "3")],
    )
    .await;
    for name in ["front", "back"] {
        bell.registered(TOKEN, LOCATION, &Till::at(&push, name))
            .await;
    }
    bell.registered(TOKEN, OTHER_LOCATION, &Till::at(&push, "elsewhere"))
        .await;
    bell.registered(
        OTHER_TOKEN,
        OTHER_MERCHANTS_LOCATION,
        &Till::at(&push, "other-merchant"),
    )
    .await;
    let dinged = async |merchant: &str, location: &str| {
        let body = payment_webhook("payment.created", "COMPLETED", merchant, location);
        assert_eq!(bell.webhook(&body).await.status, StatusCode::OK);
        push.drain().await.len()
    };

    assert_eq!(dinged(MERCHANT, LOCATION).await, 2);
    // The share is the merchant's, not the location's.
    assert_eq!(dinged(MERCHANT, OTHER_LOCATION).await, 1);
    assert_eq!(dinged(MERCHANT, LOCATION).await, 0, "over the share");
    assert_eq!(dinged(OTHER_MERCHANT, OTHER_MERCHANTS_LOCATION).await, 1);
    assert!(
        bell.rows().iter().all(|row| row.4 == 0),
        "a dropped ding counted as a failure: {:?}",
        bell.rows()
    );
}
