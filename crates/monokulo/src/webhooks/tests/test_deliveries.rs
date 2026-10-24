//! Test deliveries end to end (`super::super::test_delivery`): the store
//! settings' "Send a test webhook" and the API's `send-test-webhook` form
//! each send a real local receiver a signed `webhook.test`, checked the way
//! the docs tell a receiver to check one, and list it among the webhook's
//! deliveries; a receiver that's down, answers an error or takes too long
//! is told apart, and the delivery is retried like any other.

use axum::body::Body;
use axum::http::Request;

use super::*;
use crate::http::test_support::body_text;

/// The shop's options for a receiver on this machine.
const PRIVATE: &str = "[webhooks]\nallow_private_urls = true\n";

/// Whether `body` arrived as the docs (webhooks.md, "Check it") say to
/// check a delivery: `X-Monokulo-Signature` is `t=<unix seconds>,v1=<hex>`,
/// the hex HMAC-SHA256 of `"<t>.<body>"` under the webhook's signing
/// secret, and `t` within five minutes of the receiver's clock. Written
/// here from the docs, not with monokulo's own signing code.
fn checks_out_as_documented(secret: &str, headers: &HeaderMap, body: &str) -> bool {
    use hmac::{Hmac, KeyInit, Mac};
    let header = headers["x-monokulo-signature"].to_str().unwrap();
    let Some((t, v1)) = header.split_once(',') else {
        return false;
    };
    let t: i64 = t.strip_prefix("t=").unwrap().parse().unwrap();
    let v1 = hex::decode(v1.strip_prefix("v1=").unwrap()).unwrap();
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{t}.{body}").as_bytes());
    (crate::now_unix() - t).abs() <= 300 && mac.verify_slice(&v1).is_ok()
}

impl Shop {
    /// "Send a test webhook" on `webhook`, as a browser without
    /// JavaScript sends it, or as fixi does.
    async fn dashboard_test(&self, webhook: &WebhookRow, fixi: bool) -> axum::response::Response {
        let mut request = Request::builder()
            .method("POST")
            .uri(format!(
                "/stores/{}/settings/webhooks/{}/test",
                self.store, webhook.id
            ))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("cookie", format!("session={}", self.session));
        if fixi {
            request = request.header("fx-request", "true");
        }
        self.router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    /// The API's `send-test-webhook` form, sent with the store's secret
    /// key and `input`.
    async fn api_test(&self, input: serde_json::Value) -> axum::response::Response {
        self.router
            .clone()
            .oneshot(api_request(
                "POST",
                "/api/v1/webhook-tests",
                Some(&self.sk),
                Some(input),
            ))
            .await
            .unwrap()
    }

    /// A dashboard page.
    async fn page(&self, uri: &str) -> String {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("cookie", format!("session={}", self.session))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        body_text(response).await
    }
}

/// What a receiver got for a test of `webhook`: a delivery that checks out
/// as documented, naming its event in the headers as in the body, with the
/// published vector's fields, the store, the webhook and the store's link,
/// and no order. Returns its event id.
fn assert_a_test_delivery(
    shop: &Shop,
    webhook: &WebhookRow,
    secret: &str,
    headers: &HeaderMap,
    body: &str,
) -> String {
    assert!(
        checks_out_as_documented(secret, headers, body),
        "{headers:?}"
    );
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(headers["x-monokulo-event"], "webhook.test");
    assert_eq!(parsed["event"], "webhook.test");
    assert_eq!(
        headers["x-monokulo-event-id"].to_str().unwrap(),
        parsed["event_id"].as_str().unwrap()
    );
    assert_eq!(headers["content-type"], "application/json");
    let fields: Vec<String> = parsed.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        Some(fields),
        super::super::vectors::fields_of("webhook.test"),
        "{body}"
    );
    assert_eq!(parsed["store"]["id"], shop.store.as_str());
    assert_eq!(parsed["webhook"]["id"], webhook.id.as_str());
    assert_eq!(
        parsed["_links"]["mk:store"]["href"],
        "https://www.monokulo.com/api/v1/store"
    );
    assert!(parsed.get("order_id").is_none(), "{body}");
    parsed["event_id"].as_str().unwrap().to_owned()
}

/// "Send a test webhook" without JavaScript: a form POST that sends the
/// receiver a signed `webhook.test` and comes back to the settings page at
/// the webhook, saying it was delivered, with the delivery listed as a
/// test. With fixi, the same POST answers the webhook alone, to put in its
/// place, saying so too.
#[tokio::test]
async fn the_dashboard_button_sends_a_signed_test_and_shows_it() {
    let shop = shop_with(PRIVATE).await;
    let received = Received::default();
    let url = endpoint(Arc::new(AtomicU16::new(200)), received.clone()).await;
    let (webhook, secret) = add_webhook(&shop.state.db, &shop.store, &url).await;

    let sent = shop.dashboard_test(&webhook, false).await;
    assert_eq!(sent.status(), StatusCode::SEE_OTHER);
    let delivery = only_delivery(&shop.state.db, &webhook);
    let location = sent.headers()["location"].to_str().unwrap().to_owned();
    assert_eq!(
        location,
        format!(
            "/stores/{}/settings?tab=connections&tested={}#webhook-{}",
            shop.store, delivery.id, webhook.id
        )
    );
    let (headers, body) = received.requests.lock()[0].clone();
    let event_id = assert_a_test_delivery(&shop, &webhook, &secret, &headers, &body);

    // In the webhook's deliveries: a test, delivered at the first attempt,
    // the body and signature as sent, the receiver's answer kept.
    assert!(delivery.is_test());
    assert_eq!(delivery.event_type, "webhook.test");
    assert_eq!(delivery.event_id, event_id);
    assert_eq!(delivery.body, body);
    assert_eq!(delivery.state(), DeliveryState::Delivered);
    assert_eq!(delivery.last_status_code, Some(200));
    assert_eq!(delivery.attempts.len(), 1);
    assert_eq!(
        delivery.attempts[0].signature,
        headers["x-monokulo-signature"].to_str().unwrap()
    );
    assert!(delivery
        .last_response
        .as_deref()
        .is_some_and(|r| r.starts_with("HTTP/1.1 200 OK")));

    let html = shop.page(location.split('#').next().unwrap()).await;
    assert!(html.contains("Test webhook delivered."), "{html}");
    assert!(html.contains("data-test-delivery"), "{html}");
    assert!(html.contains(&format!(
        "action=\"/stores/{}/settings/webhooks/{}/test\"",
        shop.store, webhook.id
    )));

    let fixi = shop.dashboard_test(&webhook, true).await;
    assert_eq!(fixi.status(), StatusCode::OK);
    let block = body_text(fixi).await;
    assert!(
        block.starts_with(&format!("<div class=\"wh\" id=\"webhook-{}\"", webhook.id)),
        "{block}"
    );
    assert!(block.contains("Test webhook delivered."), "{block}");
    assert!(!block.contains("<html"), "{block}");
    assert_eq!(received.requests.lock().len(), 2);
    let latest = &shop
        .state
        .db
        .lock()
        .recent_deliveries(&webhook.id, 10)
        .unwrap()[0];
    assert!(
        block.contains(&format!("deliveries/{}", latest.id)),
        "{block}"
    );
}

/// The API's `send-test-webhook` form, offered on the entry to the secret
/// key only: it sends the store's webhook a signed `webhook.test` and
/// answers how it went, the receiver's answer and its event id included;
/// the delivery is listed like the dashboard's. Without `webhook_id` it
/// picks the store's only webhook; with several, the one named.
#[tokio::test]
async fn the_api_form_sends_a_signed_test_and_answers_how_it_went() {
    let shop = shop_with(PRIVATE).await;
    let entry = body_json(
        shop.router
            .clone()
            .oneshot(api_request("GET", "/api/v1", Some(&shop.sk), None))
            .await
            .unwrap(),
    )
    .await;
    let form = &entry["_templates"]["send-test-webhook"];
    assert_eq!(form["target"], "/api/v1/webhook-tests");
    assert_eq!(form["method"], "POST");
    let public = body_json(
        shop.router
            .clone()
            .oneshot(api_request("GET", "/api/v1", Some(&shop.pk), None))
            .await
            .unwrap(),
    )
    .await;
    assert!(public["_templates"].get("send-test-webhook").is_none());

    let received = Received::default();
    let url = endpoint(Arc::new(AtomicU16::new(200)), received.clone()).await;
    let (webhook, secret) = add_webhook(&shop.state.db, &shop.store, &url).await;
    let answer = shop.api_test(serde_json::json!({})).await;
    assert_eq!(answer.status(), StatusCode::OK);
    let outcome = body_json(answer).await;
    let (headers, body) = received.requests.lock()[0].clone();
    let event_id = assert_a_test_delivery(&shop, &webhook, &secret, &headers, &body);
    assert_eq!(outcome["webhook_id"], webhook.id.as_str());
    assert_eq!(outcome["event_id"], event_id);
    assert_eq!(outcome["delivered"], true);
    assert_eq!(outcome["status"], 200);
    assert!(outcome["duration_ms"].as_u64().is_some());
    assert!(outcome["response"]
        .as_str()
        .unwrap()
        .ends_with("<html>Upstream is restarting</html>"));
    for absent in ["error", "next_attempt_at"] {
        assert!(outcome.get(absent).is_none(), "{absent}: {outcome}");
    }
    assert_eq!(outcome["_links"]["mk:store"]["href"], "/api/v1/store");
    let delivery = only_delivery(&shop.state.db, &webhook);
    assert!(delivery.is_test());
    assert_eq!(delivery.event_id, event_id);
    assert_eq!(delivery.state(), DeliveryState::Delivered);

    // A second webhook: named, it gets the test; unnamed, there's no one
    // webhook to pick; another store's, or none, isn't found.
    let other = Received::default();
    let other_url = endpoint(Arc::new(AtomicU16::new(204)), other.clone()).await;
    let (second, second_secret) = add_webhook(&shop.state.db, &shop.store, &other_url).await;
    let named = shop
        .api_test(serde_json::json!({"webhook_id": second.id.as_str()}))
        .await;
    assert_eq!(named.status(), StatusCode::OK);
    assert_eq!(body_json(named).await["status"], 204);
    let (headers, body) = other.requests.lock()[0].clone();
    assert_a_test_delivery(&shop, &second, &second_secret, &headers, &body);
    for input in [
        serde_json::json!({}),
        serde_json::json!({"webhook_id": "wh_0000"}),
    ] {
        let answer = shop.api_test(input).await;
        crate::http::test_support::assert_problem(answer, "not-found").await;
    }
    assert_eq!(received.requests.lock().len(), 1);
}

/// A receiver that's down, one that answers an error and one that takes
/// longer than `webhooks.delivery_timeout_ms`: each test answers
/// `delivered: false` saying which, is listed as retrying with that
/// failure, and is tried again on the delivery schedule, a minute later;
/// the dashboard says it wasn't delivered and when it's tried next.
#[tokio::test]
async fn a_failed_test_says_why_and_is_retried_like_any_delivery() {
    let shop =
        shop_with("[webhooks]\nallow_private_urls = true\ndelivery_timeout_ms = 300\n").await;

    // Down: nothing listens there any more.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let down = format!("http://{}/hook", listener.local_addr().unwrap());
    drop(listener);
    // An error.
    let status = Arc::new(AtomicU16::new(500));
    let failing = endpoint(status.clone(), Received::default()).await;
    // Too slow.
    let slow = {
        let app = axum::Router::new().route(
            "/hook",
            axum::routing::post(|| async {
                tokio::time::sleep(Duration::from_secs(3)).await;
                "late"
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/hook")
    };

    for (url, answered, error) in [
        (down.as_str(), None, "could not connect"),
        (failing.as_str(), Some(500u16), "answered 500"),
        (slow.as_str(), None, "timed out"),
    ] {
        let (webhook, _) = add_webhook(&shop.state.db, &shop.store, url).await;
        let answer = shop
            .api_test(serde_json::json!({"webhook_id": webhook.id.as_str()}))
            .await;
        assert_eq!(answer.status(), StatusCode::OK, "{url}");
        let outcome = body_json(answer).await;
        assert_eq!(outcome["delivered"], false, "{outcome}");
        assert_eq!(
            outcome["status"].as_u64(),
            answered.map(u64::from),
            "{outcome}"
        );
        assert!(
            outcome["error"].as_str().unwrap().starts_with(error),
            "{error}: {outcome}"
        );
        let delivery = only_delivery(&shop.state.db, &webhook);
        assert_eq!(delivery.state(), DeliveryState::Retrying, "{url}");
        assert_eq!(delivery.last_error.as_deref(), outcome["error"].as_str());
        let next = delivery.next_attempt_at.unwrap();
        assert_eq!(outcome["next_attempt_at"], next);
        let attempted = delivery.last_attempt_at.unwrap();
        assert!((60..=62).contains(&(next - attempted)), "{delivery:?}");

        // On the dashboard (with fixi), it wasn't delivered and is tried
        // again.
        let block = body_text(shop.dashboard_test(&webhook, true).await).await;
        assert!(block.contains("Test webhook not delivered."), "{block}");
        assert!(block.contains("Monokulo tries it again at"), "{block}");
    }

    // Once the receiver answers, the delivery worker's next attempt, at
    // its time, delivers the failed test.
    status.store(200, Ordering::SeqCst);
    let webhook = shop
        .state
        .db
        .lock()
        .list_webhooks(&shop.store)
        .unwrap()
        .into_iter()
        .find(|w| w.url == failing)
        .unwrap();
    let first = shop
        .state
        .db
        .lock()
        .recent_deliveries(&webhook.id, 10)
        .unwrap();
    let oldest = first.last().unwrap().clone();
    let clock = ManualClock::at(oldest.next_attempt_at.unwrap());
    tick(&shop.state.db, &client(), &config(true, 8), &clock).await;
    let retried = shop
        .state
        .db
        .lock()
        .get_delivery(&shop.store, &webhook.id, oldest.id)
        .unwrap()
        .unwrap();
    assert_eq!(retried.state(), DeliveryState::Delivered);
    assert_eq!(retried.attempts.len(), 2);
    assert_eq!(retried.body, oldest.body);
}

/// At most six tests a minute for a store, the dashboard's and the API's
/// together: the seventh sends nothing, the API answering `429`
/// `rate-limited` with how long to wait, the dashboard saying so on the
/// webhook (or, without JavaScript, the page refusing it).
#[tokio::test]
async fn a_store_sends_at_most_six_tests_a_minute() {
    let shop = shop_with(PRIVATE).await;
    // The limit's clock (the abuse protection's) stopped 15 s into a
    // minute, so it can't roll over halfway through.
    let state = AppState {
        abuse: Arc::new(
            crate::abuse::AbuseProtection::new(shop.state.abuse.config())
                .with_clock(crate::abuse::Clock::Fixed(1_767_225_615)),
        ),
        ..shop.state.clone()
    };
    let shop = Shop {
        router: build_router(state.clone()),
        state,
        ..shop
    };
    let received = Received::default();
    let url = endpoint(Arc::new(AtomicU16::new(200)), received.clone()).await;
    let (webhook, _) = add_webhook(&shop.state.db, &shop.store, &url).await;
    for n in 0..test_delivery::TESTS_PER_MINUTE {
        let sent = if n % 2 == 0 {
            shop.api_test(serde_json::json!({})).await.status()
        } else {
            shop.dashboard_test(&webhook, false).await.status()
        };
        assert!(
            sent.is_success() || sent == StatusCode::SEE_OTHER,
            "{n}: {sent}"
        );
    }
    let refused = shop.api_test(serde_json::json!({})).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    let wait: u64 = refused.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let problem = body_json(refused).await;
    assert_eq!(
        problem["type"],
        "https://docs.monokulo.com/problems/rate-limited"
    );
    assert_eq!(problem["retry_after"], wait);
    assert_eq!(wait, 45);

    let block = body_text(shop.dashboard_test(&webhook, true).await).await;
    assert!(
        block.contains("test webhooks in the last minute"),
        "{block}"
    );
    let page = shop.dashboard_test(&webhook, false).await;
    assert_eq!(page.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body_text(page)
        .await
        .contains("test webhooks in the last minute"));
    assert_eq!(
        received.requests.lock().len(),
        test_delivery::TESTS_PER_MINUTE as usize
    );
}
