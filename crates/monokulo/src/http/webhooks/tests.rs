//! The Webhooks card and its pages, through monokulo's own router with an
//! engine in this process: adding and deleting webhooks, the card's states,
//! a delivery's detail and the no-JavaScript POSTs ("Send again", "Retry
//! failed").

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::ServiceExt;

use super::parse_extra_headers;
use crate::db::{Attempt, AttemptOutcome, LoggedEvent, OrderId};
use crate::engine_client::EngineClient;
use crate::http::test_support::{
    body_json, body_text, signed_up_and_logged_in_session_token, urlencoding_encode,
};
use crate::http::{build_router, AppState};

async fn test_state_with_real_engine() -> (AppState, engine_test_support::TestEngineHandle) {
    let engine = engine_test_support::TestEngineConfig::new()
        .with_networks(&[monero::Network::Mainnet])
        .spawn()
        .await;
    let state = AppState {
        engine: crate::http::Engine::new(EngineClient::embedded_for_tests(engine.router())),
        ..AppState::for_tests()
    };
    (state, engine)
}

/// A store for `session_token`'s account: `(connection_id, public_key)`.
async fn create_connection(router: &Router, session_token: &str) -> (String, String) {
    static STORES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = STORES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let body = serde_json::json!({
        "site_url": format!("https://shop-{n}.example.com"),
        "view_key_hex": "0707070707070707070707070707070707070707070707070707070707070707",
        "spend_pubkey_hex": "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90",
        "network": "mainnet",
        "domains": [],
        "base_currency": "XMR",
    });
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/connections")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {session_token}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    (
        body["connection_id"].as_str().unwrap().to_string(),
        body["public_key"].as_str().unwrap().to_string(),
    )
}

fn form_post_request(uri: &str, bearer: &str, fields: &[(&str, &str)]) -> Request<Body> {
    let body = fields
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", format!("Bearer {bearer}"))
        .body(Body::from(body))
        .unwrap()
}

async fn get(router: &Router, uri: &str, bearer: &str) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("authorization", format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[test]
fn parse_extra_headers_reads_one_header_name_value_pair_per_line() {
    let parsed =
        parse_extra_headers("X-Api-Key: secret123\nAnother-Header:  spaced value \n\n").unwrap();
    assert_eq!(
        parsed.get("X-Api-Key").map(String::as_str),
        Some("secret123")
    );
    assert_eq!(
        parsed.get("Another-Header").map(String::as_str),
        Some("spaced value")
    );
    assert_eq!(
        parsed.len(),
        2,
        "blank lines must not produce a phantom entry"
    );
}

#[test]
fn parse_extra_headers_on_empty_input_returns_an_empty_map_not_an_error() {
    assert!(parse_extra_headers("").unwrap().is_empty());
    assert!(parse_extra_headers("   \n  \n").unwrap().is_empty());
}

#[test]
fn parse_extra_headers_rejects_a_line_with_no_colon() {
    let err = parse_extra_headers("X-Api-Key: fine\nnot-a-valid-line").unwrap_err();
    assert!(
        err.contains("not-a-valid-line"),
        "expected the real offending line named in the error, got: {err}"
    );
}

#[test]
fn parse_extra_headers_rejects_an_empty_header_name() {
    let err = parse_extra_headers(": value-with-no-name").unwrap_err();
    assert!(
        err.contains(": value-with-no-name"),
        "expected the real offending line named in the error, got: {err}"
    );
}

/// Custom headers are kept with the webhook, lowercased, and sent with
/// every delivery.
#[tokio::test]
async fn creating_a_webhook_with_custom_headers_keeps_them() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "webhook-headers@example.com",
        "correct horse battery staple",
    )
    .await;
    let (connection_id, _public_key) = create_connection(&router, &session_token).await;
    let response = router
        .oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/settings/webhooks"),
            &session_token,
            &[
                ("url", "https://merchant.example/monokulo-webhook"),
                ("extra_headers", "X-Api-Key: secret123\nX-Another: value2"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_text(response).await.contains("Webhook created"));
    let webhooks = state
        .db
        .lock()
        .list_webhooks(&crate::db::ConnectionId::new(connection_id))
        .unwrap();
    assert_eq!(webhooks.len(), 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&webhooks[0].extra_headers).unwrap(),
        serde_json::json!({ "x-api-key": "secret123", "x-another": "value2" })
    );
}

/// The merchant's own server receives the payment notification: a webhook
/// added on the settings page with a custom Authorization header gets
/// `order.paid` when the customer pays, through monokulo's own subscriber
/// and delivery worker, carrying that header, the v2 body and a signature
/// that verifies with the secret the page showed once.
#[tokio::test]
async fn a_merchants_webhook_endpoint_receives_the_paid_notification_with_its_custom_header_and_a_valid_signature(
) {
    use axum::http::HeaderMap;
    type Received = std::sync::Arc<parking_lot::Mutex<Vec<(HeaderMap, String)>>>;
    let received: Received = Default::default();
    let app = Router::new()
        .route(
            "/hook",
            axum::routing::post(
                |axum::extract::State(received): axum::extract::State<Received>,
                 headers: HeaderMap,
                 body: String| async move {
                    received.lock().push((headers, body));
                    StatusCode::OK
                },
            ),
        )
        .with_state(received.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hook = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (state, engine) = test_state_with_real_engine().await;
    // The endpoint is on this machine: allowed for this test, as a
    // self-hoster testing against their own network would.
    let state = state
        .with_options("[signup]\nmode = \"public\"\n[webhooks]\nallow_private_urls = true\n")
        .await;
    let router = build_router(state.clone());
    let subscriber = tokio::spawn(crate::webhooks::subscriber::run_subscriber(
        state.db.clone(),
        state.engine.client.clone(),
        state.webhooks.clone(),
    ));
    let worker = tokio::spawn(crate::webhooks::delivery::run_delivery_loop(
        state.db.clone(),
        state.encryption_key.clone(),
        state.settings.clone(),
        state.webhooks.clone(),
    ));
    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "webhook-delivery@example.com",
        "correct horse battery staple",
    )
    .await;
    let (connection_id, _public_key) = create_connection(&router, &session_token).await;
    let html = body_text(
        router
            .clone()
            .oneshot(form_post_request(
                &format!("/dashboard/stores/{connection_id}/settings/webhooks"),
                &session_token,
                &[
                    ("url", &hook),
                    (
                        "extra_headers",
                        "Authorization: Bearer shop-endpoint-secret",
                    ),
                ],
            ))
            .await
            .unwrap(),
    )
    .await;
    let secret = html
        .split("<pre>")
        .nth(1)
        .and_then(|rest| rest.split("</pre>").next())
        .expect("the signing secret is shown once")
        .to_string();

    let response = router
        .clone()
        .oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/orders/new"),
            &session_token,
            &[
                ("amount", "0.5"),
                ("currency", "XMR"),
                ("merchant_order_id", "wc-1042"),
            ],
        ))
        .await
        .unwrap();
    let order_id = response.headers()["location"]
        .to_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();
    engine.mark_order_paid(&order_id).unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60); // bounds only a hung run
    let (headers, body) = loop {
        if let Some(first) = received.lock().first().cloned() {
            break first;
        }
        assert!(std::time::Instant::now() < deadline, "no webhook delivered");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    subscriber.abort();
    worker.abort();
    assert_eq!(headers["authorization"], "Bearer shop-endpoint-secret");
    assert_eq!(headers["x-monokulo-event"], "order.paid");
    assert!(shared::webhook_sign::verify_signature(
        &secret,
        body.as_bytes(),
        headers["x-monokulo-signature"].to_str().unwrap(),
        shared::time::now_unix()
    ));
    let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(payload["order_id"], order_id.as_str(), "{payload}");
    assert_eq!(payload["api_version"], 2);
    assert_eq!(payload["merchant_order_id"], "wc-1042");
    assert_eq!(payload["currency"], "XMR");
    assert_eq!(
        headers["x-monokulo-event-id"].to_str().unwrap(),
        payload["event_id"].as_str().unwrap()
    );
    // The card says it's delivering.
    let page = body_text(
        get(
            &router,
            &format!("/dashboard/stores/{connection_id}/settings"),
            &session_token,
        )
        .await,
    )
    .await;
    assert!(
        page.contains(r#"<span class="tag tag-ok">delivering</span>"#),
        "{page}"
    );
}

/// A store with a webhook and a delivery of each kind: delivered, waiting
/// to retry, and given up. Returns `(router, session, store, webhook,
/// [delivered, retrying, gave up])`.
async fn store_with_deliveries(
    state: &AppState,
    email: &str,
) -> (Router, String, String, String, [i64; 3]) {
    let router = build_router(state.clone());
    let session =
        signed_up_and_logged_in_session_token(&router, email, "correct horse battery staple").await;
    let (store, pk) = create_connection(&router, &session).await;
    let created = crate::webhooks::create(
        &state.db,
        &state.encryption_key,
        &crate::db::ConnectionId::new(store.clone()),
        "https://erp.bakery.example/payments/in",
        &std::collections::BTreeMap::from([("X-Api-Key".to_string(), "never-shown".to_string())]),
    )
    .await
    .unwrap();
    let webhook = created.webhook.id.to_string();
    let now = crate::now_unix();
    let db = state.db.lock();
    let mut ids = Vec::new();
    for (seq, (event_type, attempts, delivered)) in [
        ("order.paid", 1u32, true),
        ("order.confirming", 4, false),
        ("order.expired", 8, false),
    ]
    .into_iter()
    .enumerate()
    {
        let seq = i64::try_from(seq).unwrap() + 1;
        let event = LoggedEvent {
            seq,
            event_id: format!("evt_{seq}"),
            event_type: event_type.to_string(),
            created_at: now - 600,
            tenant_public_key: pk.clone(),
            order_id: OrderId::new(format!("5f01c9a7d2e14b88a3e0f9c6d1e21b0{seq}")),
            status: event_type.strip_prefix("order.").map(str::to_string),
            txid: None,
            merchant_order_id: None,
            xmr_amount_piconero: 1,
        };
        db.queue_order_event(&event, now - 600, |store, metadata| {
            crate::webhooks::body::body_v2(&event, store, metadata)
        })
        .unwrap();
        let id = db.recent_deliveries(&created.webhook.id, 1).unwrap()[0].id;
        for n in 1..=attempts {
            let last = n == attempts;
            db.record_delivery_attempt(
                id,
                &AttemptOutcome {
                    attempt: Attempt {
                        n,
                        at: now - 300 + i64::from(n),
                        status: Some(if delivered { 200 } else { 503 }),
                        error: None,
                        ms: 184,
                        signature: format!("t={now},v1=abc{n}"),
                    },
                    delivered,
                    response: Some("HTTP/1.1 503 Service Unavailable\ncontent-type: text/html\n\n<html>Upstream is restarting</html>".into()),
                    next_attempt_at: (!delivered && !(last && attempts == 8)).then_some(now + 360),
                },
            )
            .unwrap();
        }
        ids.push(id);
    }
    drop(db);
    (router, session, store, webhook, [ids[0], ids[1], ids[2]])
}

/// The card: one health line per webhook (here, gave up, with "Retry
/// failed (1)"), the deliveries fold open because one gave up, each row's
/// status as a tag word, its order shortened, its response and attempt;
/// "Send again" only on the given-up row; each "Details" a link to its
/// page that opens its dialog, already on the page.
#[tokio::test]
async fn the_card_shows_each_webhooks_health_and_its_deliveries() {
    let (state, _engine) = test_state_with_real_engine().await;
    let (router, session, store, webhook, [delivered, retrying, gave_up]) =
        store_with_deliveries(&state, "card@example.com").await;
    let html = body_text(
        get(
            &router,
            &format!("/dashboard/stores/{store}/settings"),
            &session,
        )
        .await,
    )
    .await;
    let base = format!("/dashboard/stores/{store}/settings/webhooks/{webhook}");
    assert!(
        html.contains(r#"<span class="card-meta">1 webhook</span>"#),
        "{html}"
    );
    assert!(
        html.contains(r#"<span class="tag tag-error">gave up</span><span class="hint">1 delivery"#),
        "{html}"
    );
    assert!(html.contains(&format!(r#"<form method="post" action="{base}/retry-failed" class="inline-form"><button type="submit" class="btn-sm">Retry failed (1)</button>"#)), "{html}");
    assert!(
        html.contains(r#"<details class="wh-deliveries" open>"#),
        "{html}"
    );
    for word in ["delivered", "retrying"] {
        assert!(html.contains(&format!(">{word}</span>")), "{word}");
    }
    assert!(
        html.contains(r#"<td data-label="Attempt">4 of 8</td>"#),
        "{html}"
    );
    assert!(html.contains("5f01c9…e21b03"), "orders shortened: {html}");
    assert_eq!(
        html.matches(r#"<button type="submit" class="icon-only send-again" aria-label="Send again" title="Send again"><svg"#).count(),
        1,
        "only the given-up row, as an icon with its name: {html}"
    );
    assert!(html.contains(&format!(
        r#"action="{base}/deliveries/{gave_up}/send-again""#
    )));
    for id in [delivered, retrying, gave_up] {
        assert!(
            html.contains(&format!(
                r#"href="{base}/deliveries/{id}" data-opens-dialog="delivery-{id}-dialog""#
            )),
            "{id}"
        );
        assert!(
            html.contains(&format!(r#"<dialog id="delivery-{id}-dialog""#)),
            "{id}"
        );
    }
    assert!(html.contains(&format!(
        r#"href="{base}/delete" data-opens-dialog="{webhook}-delete-dialog""#
    )));
    assert!(
        !html.contains("never-shown"),
        "a header's value is never shown"
    );
}

/// A healthy webhook's fold stays closed, its line saying when it last
/// delivered; an empty store says so and offers "Add webhook".
#[tokio::test]
async fn a_healthy_webhooks_deliveries_stay_folded_and_an_empty_card_offers_to_add_one() {
    let (state, _engine) = test_state_with_real_engine().await;
    let (router, session, store, webhook, [_, retrying, gave_up]) =
        store_with_deliveries(&state, "healthy@example.com").await;
    {
        let db = state.db.lock();
        let store_id = crate::db::ConnectionId::new(store.clone());
        let webhook_id = shared::ids::WebhookId::new(webhook.clone());
        // Both sent again and delivered.
        for id in [retrying, gave_up] {
            db.send_delivery_again(&store_id, &webhook_id, id, 0)
                .unwrap();
            db.record_delivery_attempt(
                id,
                &AttemptOutcome {
                    attempt: Attempt {
                        n: 1,
                        at: crate::now_unix() - 120,
                        status: Some(200),
                        error: None,
                        ms: 184,
                        signature: String::new(),
                    },
                    delivered: true,
                    response: None,
                    next_attempt_at: None,
                },
            )
            .unwrap();
        }
    }
    let html = body_text(
        get(
            &router,
            &format!("/dashboard/stores/{store}/settings"),
            &session,
        )
        .await,
    )
    .await;
    assert!(html.contains(r#"<span class="tag tag-ok">delivering</span><span class="hint">last 2 min ago · 200 in 184 ms · 3 delivered in the last 30 days</span>"#), "{html}");
    assert!(
        html.contains(r#"<details class="wh-deliveries">"#),
        "{html}"
    );
    assert!(!html.contains("Retry failed"));

    let router2 = build_router(state.clone());
    let other = signed_up_and_logged_in_session_token(
        &router2,
        "empty@example.com",
        "correct horse battery staple",
    )
    .await;
    let (empty, _) = create_connection(&router2, &other).await;
    let html = body_text(
        get(
            &router2,
            &format!("/dashboard/stores/{empty}/settings"),
            &other,
        )
        .await,
    )
    .await;
    assert!(html.contains("No webhooks yet. Monokulo can tell your server when an order is paid, confirming or expired."), "{html}");
    assert!(html.contains(">Add webhook</button>"));
}

/// "Send again" and "Retry failed" are plain form POSTs: each comes back
/// to the settings page (`303`) and queues the deliveries at once, a
/// given-up one starting its schedule over.
#[tokio::test]
async fn send_again_and_retry_failed_work_as_plain_form_posts() {
    let (state, _engine) = test_state_with_real_engine().await;
    let (router, session, store, webhook, [delivered, _, gave_up]) =
        store_with_deliveries(&state, "posts@example.com").await;
    let base = format!("/dashboard/stores/{store}/settings/webhooks/{webhook}");
    let response = router
        .clone()
        .oneshot(form_post_request(
            &format!("{base}/deliveries/{delivered}/send-again"),
            &session,
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        format!("/dashboard/stores/{store}/settings?saved=webhooks#card-webhooks").as_str()
    );
    let store_id = crate::db::ConnectionId::new(store.clone());
    let webhook_id = shared::ids::WebhookId::new(webhook.clone());
    let again = state
        .db
        .lock()
        .get_delivery(&store_id, &webhook_id, delivered)
        .unwrap()
        .unwrap();
    assert_eq!(
        (again.attempt_count, again.delivered_at),
        (0, None),
        "queued again from the start"
    );

    let response = router
        .clone()
        .oneshot(form_post_request(
            &format!("{base}/retry-failed"),
            &session,
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let retried = state
        .db
        .lock()
        .get_delivery(&store_id, &webhook_id, gave_up)
        .unwrap()
        .unwrap();
    assert_eq!(retried.state(), crate::db::DeliveryState::Queued);
    assert_eq!(retried.attempts.len(), 8, "its earlier attempts are kept");
    let html = body_text(
        get(
            &router,
            &format!("/dashboard/stores/{store}/settings?saved=webhooks"),
            &session,
        )
        .await,
    )
    .await;
    assert!(html.contains("Webhooks updated"), "a toast: {html}");
    assert!(!html.contains("Retry failed"));
}

/// Without JavaScript a delivery's detail is a page: its attempts, newest
/// first, the request with the signature header (and a custom header's
/// name, never its value; never the secret), the start of the last
/// answer, and "Send again now".
#[tokio::test]
async fn a_deliverys_detail_is_a_page_without_javascript() {
    let (state, _engine) = test_state_with_real_engine().await;
    let (router, session, store, webhook, [_, retrying, _]) =
        store_with_deliveries(&state, "detail@example.com").await;
    let base = format!("/dashboard/stores/{store}/settings/webhooks/{webhook}");
    let response = get(&router, &format!("{base}/deliveries/{retrying}"), &session).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains(r#"<h1 id="delivery-"#), "{html}");
    assert!(
        html.contains("Delivery of <code>order.confirming</code>"),
        "{html}"
    );
    assert!(
        html.contains(r#"<span class="tag tag-slow">retrying</span> next try at"#),
        "{html}"
    );
    assert_eq!(
        html.matches("<code>503 Service Unavailable</code>").count(),
        4,
        "{html}"
    );
    let newest = html.find(">4<span class=\"muted\"> of 8</span>").unwrap();
    let oldest = html.find(">1<span class=\"muted\"> of 8</span>").unwrap();
    assert!(newest < oldest, "newest first");
    assert!(
        html.contains(
            "POST /payments/in\nContent-Type: application/json\nX-Monokulo-Signature: t="
        ),
        "{html}"
    );
    assert!(html.contains("x-api-key: •••"), "{html}");
    assert!(!html.contains("never-shown"));
    assert!(!html.contains("whsec_"), "the secret is never shown");
    assert!(html.contains("Upstream is restarting"), "{html}");
    assert!(
        html.contains(&format!(
            r#"<form method="post" action="{base}/deliveries/{retrying}/send-again">"#
        )),
        "{html}"
    );
    assert!(html.contains(">Send again now</button>"));
    // All of the webhook's deliveries, as a page too.
    let all = body_text(get(&router, &format!("{base}/deliveries"), &session).await).await;
    assert!(all.contains("Newest first, 20 a page."), "{all}");
    assert!(!all.contains("Older →"), "one page: {all}");
}

/// Deleting a webhook is asked first: a page without JavaScript, a dialog
/// with it; the POST removes it and its deliveries.
#[tokio::test]
async fn deleting_a_webhook_is_asked_on_a_page_then_removes_its_deliveries() {
    let (state, _engine) = test_state_with_real_engine().await;
    let (router, session, store, webhook, [delivered, ..]) =
        store_with_deliveries(&state, "delete@example.com").await;
    let base = format!("/dashboard/stores/{store}/settings/webhooks/{webhook}");
    let html = body_text(get(&router, &format!("{base}/delete"), &session).await).await;
    assert!(html.contains("Delete this webhook?"), "{html}");
    assert!(
        html.contains("1 delivery still waiting is never sent."),
        "{html}"
    );
    assert!(html.contains(&format!(r#"<form method="post" action="{base}/delete">"#)));
    let response = router
        .clone()
        .oneshot(form_post_request(&format!("{base}/delete"), &session, &[]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let store_id = crate::db::ConnectionId::new(store);
    assert!(state.db.lock().list_webhooks(&store_id).unwrap().is_empty());
    assert!(state
        .db
        .lock()
        .get_delivery(&store_id, &shared::ids::WebhookId::new(webhook), delivered)
        .unwrap()
        .is_none());
}

/// Another account's store's webhooks can't be seen or touched: every
/// page and action answers `404`.
#[tokio::test]
async fn another_accounts_webhooks_and_deliveries_are_not_found() {
    let (state, _engine) = test_state_with_real_engine().await;
    let (router, _session, store, webhook, [delivered, ..]) =
        store_with_deliveries(&state, "owner@example.com").await;
    let intruder = signed_up_and_logged_in_session_token(
        &router,
        "intruder@example.com",
        "correct horse battery staple",
    )
    .await;
    let base = format!("/dashboard/stores/{store}/settings/webhooks/{webhook}");
    for uri in [
        format!("{base}/deliveries/{delivered}"),
        format!("{base}/deliveries"),
        format!("{base}/delete"),
    ] {
        assert_eq!(
            get(&router, &uri, &intruder).await.status(),
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    for uri in [
        format!("{base}/deliveries/{delivered}/send-again"),
        format!("{base}/retry-failed"),
        format!("{base}/delete"),
    ] {
        let response = router
            .clone()
            .oneshot(form_post_request(&uri, &intruder, &[]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
    }
    // And their own store can't reach another webhook's delivery by id.
    let (own, _) = create_connection(&router, &intruder).await;
    let uri = format!("/dashboard/stores/{own}/settings/webhooks/{webhook}/deliveries/{delivered}");
    assert_eq!(
        get(&router, &uri, &intruder).await.status(),
        StatusCode::NOT_FOUND
    );
    assert!(
        state
            .db
            .lock()
            .list_webhooks(&crate::db::ConnectionId::new(store))
            .unwrap()
            .len()
            == 1
    );
}

#[tokio::test]
async fn creating_a_webhook_with_a_malformed_header_line_shows_a_clear_error_and_registers_nothing()
{
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state);

    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "webhook-bad-headers@example.com",
        "correct horse battery staple",
    )
    .await;
    let (connection_id, _public_key) = create_connection(&router, &session_token).await;

    let response = router
        .clone()
        .oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/settings/webhooks"),
            &session_token,
            &[
                ("url", "https://merchant.example/moneropay-webhook"),
                ("extra_headers", "not-a-valid-line"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let html = body_text(response).await;
    assert!(
        html.contains("not-a-valid-line"),
        "expected the real offending line named in the error, got: {html}"
    );
    assert!(
        !html.contains("Webhook created"),
        "a malformed headers submission must not register anything, got: {html}"
    );

    let list_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/dashboard/stores/{connection_id}/settings"))
                .header("authorization", format!("Bearer {session_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_html = body_text(list_response).await;
    assert!(
        !list_html.contains("merchant.example"),
        "the rejected webhook must not have been registered, got: {list_html}"
    );
}

#[tokio::test]
async fn creating_a_webhook_shows_its_signing_secret_once_and_lists_it_afterward() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state);

    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "webhook-create@example.com",
        "correct horse battery staple",
    )
    .await;
    let (connection_id, _public_key) = create_connection(&router, &session_token).await;

    let response = router
        .clone()
        .oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/settings/webhooks"),
            &session_token,
            &[("url", "https://merchant.example/moneropay-webhook")],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(
        html.contains("https://merchant.example/moneropay-webhook"),
        "expected the new webhook listed, got: {html}"
    );
    assert!(
        html.contains("Webhook created"),
        "expected the one-time signing-secret banner, got: {html}"
    );

    // The list itself (a separate GET, simulating a page reload) must
    // show the webhook but never the secret again - it's genuinely
    // gone, not just hidden by this response's own rendering choice.
    let list_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/dashboard/stores/{connection_id}/settings"))
                .header("authorization", format!("Bearer {session_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_html = body_text(list_response).await;
    assert!(list_html.contains("https://merchant.example/moneropay-webhook"));
    assert!(
        !list_html.contains("Webhook created"),
        "the signing secret must not reappear on a later page load, got: {list_html}"
    );
}

#[tokio::test]
async fn creating_a_webhook_with_an_empty_url_shows_a_clear_error() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state);

    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "webhook-empty-url@example.com",
        "correct horse battery staple",
    )
    .await;
    let (connection_id, _public_key) = create_connection(&router, &session_token).await;

    let response = router
        .oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/settings/webhooks"),
            &session_token,
            &[("url", "")],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let html = body_text(response).await;
    assert!(
        html.contains("Enter a webhook URL."),
        "expected a clear validation error, got: {html}"
    );
}

#[tokio::test]
async fn creating_a_webhook_with_an_invalid_url_says_why() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state);

    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "webhook-bad-url@example.com",
        "correct horse battery staple",
    )
    .await;
    let (connection_id, _public_key) = create_connection(&router, &session_token).await;

    let response = router
        .oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/settings/webhooks"),
            &session_token,
            &[("url", "not a url at all")],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let html = body_text(response).await;
    assert!(
        html.contains("class=\"error\""),
        "expected why the URL was refused, got: {html}"
    );
}

#[tokio::test]
async fn deleting_a_webhook_removes_it_from_the_list() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state);

    let session_token = signed_up_and_logged_in_session_token(
        &router,
        "webhook-delete@example.com",
        "correct horse battery staple",
    )
    .await;
    let (connection_id, _public_key) = create_connection(&router, &session_token).await;

    let create_response = router
        .clone()
        .oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/settings/webhooks"),
            &session_token,
            &[("url", "https://merchant.example/to-be-deleted")],
        ))
        .await
        .unwrap();
    let create_html = body_text(create_response).await;
    // The webhook's id is in its Delete link.
    assert!(create_html.contains("https://merchant.example/to-be-deleted"));

    let list_before = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/dashboard/stores/{connection_id}/settings"))
                .header("authorization", format!("Bearer {session_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_before_html = body_text(list_before).await;
    let webhook_id_start = list_before_html
        .find("/webhooks/")
        .expect("expected a Delete link containing the webhook id")
        + "/webhooks/".len();
    let webhook_id: String = list_before_html[webhook_id_start..]
        .chars()
        .take_while(|c| *c != '/')
        .collect();
    assert!(!webhook_id.is_empty());

    let delete_response = router
        .clone()
        .oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/settings/webhooks/{webhook_id}/delete"),
            &session_token,
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(
        delete_response.status(),
        StatusCode::SEE_OTHER,
        "expected a redirect back to the webhook list"
    );

    let list_after = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/dashboard/stores/{connection_id}/settings"))
                .header("authorization", format!("Bearer {session_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_after_html = body_text(list_after).await;
    assert!(
        !list_after_html.contains("https://merchant.example/to-be-deleted"),
        "expected the deleted webhook gone, got: {list_after_html}"
    );
}

#[tokio::test]
async fn a_different_user_cannot_create_or_delete_webhooks_on_someone_elses_connection() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state);

    let owner_token = signed_up_and_logged_in_session_token(
        &router,
        "webhook-owner@example.com",
        "correct horse battery staple",
    )
    .await;
    let (connection_id, _public_key) = create_connection(&router, &owner_token).await;

    let intruder_token = signed_up_and_logged_in_session_token(
        &router,
        "webhook-intruder@example.com",
        "correct horse battery staple",
    )
    .await;

    let create_response = router
        .clone()
        .oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/settings/webhooks"),
            &intruder_token,
            &[("url", "https://attacker.example/steal")],
        ))
        .await
        .unwrap();
    assert_eq!(create_response.status(), StatusCode::NOT_FOUND);

    let delete_response = router
        .oneshot(form_post_request(
            &format!("/dashboard/stores/{connection_id}/settings/webhooks/some-webhook-id/delete"),
            &intruder_token,
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(delete_response.status(), StatusCode::NOT_FOUND);
}

/// A webhook's deliveries come 20 a page, newest first: the settings
/// page's fold shows the first with "Older →"; the page of all of them
/// pages on with "← Newer" and "Older →" until the last; a fixi request
/// gets just that page's part, to swap in place.
#[tokio::test]
async fn a_webhooks_deliveries_page_twenty_at_a_time() {
    let (state, _engine) = test_state_with_real_engine().await;
    let router = build_router(state.clone());
    let session = signed_up_and_logged_in_session_token(
        &router,
        "pages@example.com",
        "correct horse battery staple",
    )
    .await;
    let (store, pk) = create_connection(&router, &session).await;
    let created = crate::webhooks::create(
        &state.db,
        &state.encryption_key,
        &crate::db::ConnectionId::new(store.clone()),
        "https://bakery.example/hook",
        &std::collections::BTreeMap::new(),
    )
    .await
    .unwrap();
    let webhook = created.webhook.id.to_string();
    let base = format!("/dashboard/stores/{store}/settings/webhooks/{webhook}/deliveries");
    let order = |seq: i64| format!("order{seq:030}");
    let add = |from: i64, to: i64| {
        let db = state.db.lock();
        for seq in from..=to {
            let event = LoggedEvent {
                seq,
                event_id: format!("evt_{seq}"),
                event_type: "order.paid".into(),
                created_at: 1000,
                tenant_public_key: pk.clone(),
                order_id: OrderId::new(order(seq)),
                status: Some("paid".into()),
                txid: None,
                merchant_order_id: None,
                xmr_amount_piconero: 1,
            };
            db.queue_order_event(&event, 1000, |store, metadata| {
                crate::webhooks::body::body_v2(&event, store, metadata)
            })
            .unwrap();
        }
    };
    let older = format!(
        r##"<a href="{base}?page=1" rel="next" fx-action="{base}?page=1" fx-target="#deliveries-{webhook}" fx-swap="outerHTML">Older →</a>"##
    );

    // Exactly a page: no more pages.
    add(1, 20);
    let html = body_text(
        get(
            &router,
            &format!("/dashboard/stores/{store}/settings"),
            &session,
        )
        .await,
    )
    .await;
    assert_eq!(html.matches(r#"<td data-label="Status">"#).count(), 20);
    assert!(!html.contains("Older →"), "{html}");

    // One more: the fold shows the newest 20 and links to the rest.
    add(21, 41);
    let html = body_text(
        get(
            &router,
            &format!("/dashboard/stores/{store}/settings"),
            &session,
        )
        .await,
    )
    .await;
    assert_eq!(html.matches(r#"<td data-label="Status">"#).count(), 20);
    assert!(html.contains(&older), "{html}");
    assert!(
        html.contains(&order(41)) && !html.contains(&order(21)),
        "newest first"
    );
    assert!(!html.contains("← Newer"));

    // The page of all of them: page 1 has both links, page 2 the last one.
    let html = body_text(get(&router, &format!("{base}?page=1"), &session).await).await;
    assert_eq!(html.matches(r#"<td data-label="Status">"#).count(), 20);
    assert!(html.contains(&order(21)) && html.contains(&order(2)) && !html.contains(&order(22)));
    assert!(
        html.contains(&format!(r#"<a href="{base}" rel="prev""#)),
        "{html}"
    );
    assert!(
        html.contains(&format!(r#"<a href="{base}?page=2" rel="next""#)),
        "{html}"
    );
    let html = body_text(get(&router, &format!("{base}?page=2"), &session).await).await;
    assert_eq!(html.matches(r#"<td data-label="Status">"#).count(), 1);
    assert!(html.contains(&order(1)));
    assert!(
        html.contains("← Newer") && !html.contains("Older →"),
        "{html}"
    );
    let html = body_text(get(&router, &format!("{base}?page=9"), &session).await).await;
    assert!(html.contains("No older deliveries."), "{html}");

    // fixi: just the page's part.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("{base}?page=1"))
                .header("authorization", format!("Bearer {session}"))
                .header("fx-request", "true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let fragment = body_text(response).await;
    assert!(
        fragment.starts_with(&format!(
            r#"<div id="deliveries-{webhook}" class="deliveries-results">"#
        )),
        "{fragment}"
    );
    assert!(!fragment.contains("<html"));
}
