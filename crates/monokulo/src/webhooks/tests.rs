//! The webhook pipeline end to end: the engine's order-event log read into
//! deliveries (an engine in this process, no HTTP), the deliveries sent to
//! a real local endpoint, signed, retried on the real schedule (a clock the
//! test moves), given up on, and sent again.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU16, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use parking_lot::Mutex;
use tower::ServiceExt as _;

use super::delivery::{run_delivery_tick, Clock, WebhookClient};
use super::*;
use crate::db::{Db, DeliveryState, LoggedEvent, OrderId};
use crate::http::test_support::{body_json, signed_up_and_logged_in_session_token};
use crate::http::{build_router, AppState, TEST_ENCRYPTION_KEY};
use crate::settings::WebhookConfig;

/// A clock the test sets.
struct ManualClock(AtomicI64);

impl ManualClock {
    fn at(now: i64) -> Self {
        ManualClock(AtomicI64::new(now))
    }
    fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// What a local endpoint was sent.
#[derive(Clone, Default)]
struct Received {
    requests: Arc<Mutex<Vec<(HeaderMap, String)>>>,
}

/// A real local endpoint answering every POST with `status` (changeable
/// while it runs). Returns its URL.
async fn endpoint(status: Arc<AtomicU16>, received: Received) -> String {
    async fn hook(
        axum::extract::State((status, received)): axum::extract::State<(Arc<AtomicU16>, Received)>,
        headers: HeaderMap,
        body: String,
    ) -> (StatusCode, &'static str) {
        received.requests.lock().push((headers, body));
        (
            StatusCode::from_u16(status.load(Ordering::SeqCst)).unwrap(),
            "<html>Upstream is restarting</html>",
        )
    }
    let app = axum::Router::new()
        .route("/hook", axum::routing::post(hook))
        .with_state((status, received));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/hook")
}

fn config(allow_private_urls: bool, max_attempts: u32) -> WebhookConfig {
    WebhookConfig {
        allow_private_urls,
        delivery_timeout: Duration::from_secs(2),
        max_attempts,
    }
}

fn client() -> WebhookClient {
    WebhookClient::build().unwrap()
}

fn test_db() -> Database {
    let db = Db::open_in_memory().unwrap();
    db.seed_test_admin();
    Database::inline(db.into_shared())
}

/// A store `id` (public key `pk_{id}`) of the seeded admin, on a wallet of
/// its own.
fn add_store(db: &Database, id: &str, name: &str) -> ConnectionId {
    let db = db.lock();
    let wallet = shared::ids::WalletId::new(format!("w_{id}"));
    let user = shared::ids::UserId::new("test-admin");
    db.create_wallet(&crate::db::NewWalletRow {
        id: &wallet,
        user_id: &user,
        name: id,
        network: "mainnet",
        primary_address: &format!("4{id}"),
        engine_wallet_id: &shared::ids::EngineWalletId::new(format!("ew_{id}")),
        origin: crate::db::WalletOrigin::Imported,
        backup: None,
        app: None,
        created_at: 0,
    })
    .unwrap();
    let store = ConnectionId::new(id);
    db.create_store_connection_on_wallet(
        &store,
        &user,
        name,
        &format!("{id}.example"),
        &format!("pk_{id}"),
        "",
        0,
        "EUR",
        &wallet,
    )
    .unwrap();
    store
}

fn event(seq: i64, store: &str, order: &str, event_type: &str) -> LoggedEvent {
    LoggedEvent {
        seq,
        event_id: format!("evt_{seq}"),
        event_type: event_type.to_string(),
        created_at: 1000 + seq,
        tenant_public_key: format!("pk_{store}"),
        order_id: OrderId::new(order),
        status: event_type.strip_prefix("order.").map(str::to_string),
        txid: None,
        merchant_order_id: None,
        xmr_amount_piconero: 1_000_000_000_000,
    }
}

fn queue(db: &Database, event: &LoggedEvent, now: i64) -> usize {
    db.lock()
        .queue_order_event(event, now, |store, metadata| {
            body::body_v2(event, store, metadata)
        })
        .unwrap()
}

/// A webhook of `store` sending to `url`, and its signing secret.
async fn add_webhook(db: &Database, store: &ConnectionId, url: &str) -> (WebhookRow, String) {
    let created = create(db, &TEST_ENCRYPTION_KEY, store, url, &BTreeMap::new())
        .await
        .unwrap();
    (created.webhook, created.signing_secret)
}

async fn tick(
    db: &Database,
    client: &WebhookClient,
    config: &WebhookConfig,
    clock: &ManualClock,
) -> usize {
    run_delivery_tick(db, client, &TEST_ENCRYPTION_KEY, config, clock)
        .await
        .unwrap()
}

fn only_delivery(db: &Database, webhook: &WebhookRow) -> crate::db::DeliveryRow {
    let mut deliveries = db.lock().recent_deliveries(&webhook.id, 10).unwrap();
    assert_eq!(deliveries.len(), 1, "{deliveries:?}");
    deliveries.remove(0)
}

// -- Signing and sending ---------------------------------------------------

/// A delivery carries the body queued for it, signed with the webhook's
/// secret at the moment it's sent (the receiver's check passes), the
/// event's id and type as headers, and the webhook's own extra headers.
#[tokio::test]
async fn a_delivery_is_signed_with_the_webhooks_secret_and_names_its_event() {
    let db = test_db();
    let store = add_store(&db, "s1", "Bakery");
    let received = Received::default();
    let url = endpoint(Arc::new(AtomicU16::new(200)), received.clone()).await;
    let created = create(
        &db,
        &TEST_ENCRYPTION_KEY,
        &store,
        &url,
        &BTreeMap::from([("X-Api-Key".to_string(), "k1".to_string())]),
    )
    .await
    .unwrap();
    assert!(created.signing_secret.starts_with("whsec_"));
    assert!(
        !created
            .webhook
            .signing_secret_encrypted
            .contains(&created.signing_secret),
        "kept encrypted"
    );
    let paid = event(1, "s1", "o1", "order.paid");
    assert_eq!(queue(&db, &paid, 1000), 1);

    let now = crate::now_unix();
    let picked = tick(&db, &client(), &config(true, 8), &ManualClock::at(now)).await;
    assert_eq!(picked, 1);
    let (headers, body) = received.requests.lock()[0].clone();
    let signature = headers["x-monokulo-signature"].to_str().unwrap();
    assert!(shared::webhook_sign::verify_signature(
        &created.signing_secret,
        body.as_bytes(),
        signature,
        now
    ));
    assert!(
        signature.starts_with(&format!("t={now},v1=")),
        "signed when sent"
    );
    assert_eq!(headers["x-monokulo-event"], "order.paid");
    assert_eq!(headers["x-monokulo-event-id"], "evt_1");
    assert_eq!(headers["x-api-key"], "k1");
    assert_eq!(headers["content-type"], "application/json");
    let sent: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(sent["event_id"], "evt_1");
    assert_eq!(sent["store"]["name"], "Bakery");

    let delivery = only_delivery(&db, &created.webhook);
    assert_eq!(delivery.state(), DeliveryState::Delivered);
    assert_eq!(delivery.body, body, "the body sent is the one kept");
    assert_eq!(delivery.last_status_code, Some(200));
    assert_eq!(delivery.attempts.len(), 1);
    assert_eq!(delivery.attempts[0].signature, signature);
    assert!(delivery
        .last_response
        .as_deref()
        .unwrap()
        .starts_with("HTTP/1.1 200 OK"));
}

/// Headers monokulo sets itself can't be a webhook's extra headers, nor
/// can invalid ones; URLs must be http(s).
#[tokio::test]
async fn a_webhook_is_refused_a_reserved_header_or_a_url_that_isnt_http() {
    let db = test_db();
    let store = add_store(&db, "s1", "Bakery");
    for (url, headers, problem) in [
        ("ftp://shop.example/hook", vec![], "http"),
        ("not a url", vec![], "valid URL"),
        ("", vec![], "Enter a webhook URL"),
        (
            "https://shop.example/hook",
            vec![("X-Monokulo-Signature", "forged")],
            "set by Monokulo",
        ),
        (
            "https://shop.example/hook",
            vec![("Bad Header", "x")],
            "valid header name",
        ),
    ] {
        let headers = headers
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        match create(&db, &TEST_ENCRYPTION_KEY, &store, url, &headers).await {
            Err(CreateError::Invalid(message)) => {
                assert!(message.contains(problem), "{url}: {message}");
            }
            Err(other) => panic!("{url}: {other}"),
            Ok(_) => panic!("{url} was accepted"),
        }
    }
    assert!(db.lock().list_webhooks(&store).unwrap().is_empty());
}

// -- The schedule ------------------------------------------------------------

/// A failing endpoint is tried 8 times, waiting 1, 2, 4, 8, 16, 32 and 64
/// minutes between them, and nothing is sent early; after the 8th the
/// delivery is given up on and never tried again by itself.
#[tokio::test]
async fn a_failing_delivery_follows_the_schedule_then_gives_up() {
    let db = test_db();
    let store = add_store(&db, "s1", "Bakery");
    let received = Received::default();
    let url = endpoint(Arc::new(AtomicU16::new(503)), received.clone()).await;
    let (webhook, _) = add_webhook(&db, &store, &url).await;
    queue(&db, &event(1, "s1", "o1", "order.paid"), 0);

    let start = 1_000_000;
    let clock = ManualClock::at(start);
    let (client, config) = (client(), config(true, 8));
    let mut waits = Vec::new();
    for attempt in 1..=8u32 {
        assert_eq!(
            tick(&db, &client, &config, &clock).await,
            1,
            "attempt {attempt}"
        );
        let delivery = only_delivery(&db, &webhook);
        assert_eq!(delivery.attempt_count, attempt);
        assert_eq!(
            delivery.last_status_code,
            Some(503),
            "{:?}",
            delivery.last_error
        );
        assert_eq!(delivery.attempts.last().unwrap().n, attempt);
        match delivery.next_attempt_at {
            Some(next) => {
                waits.push((next - clock.now()) / 60);
                // Not a second early.
                clock.set(next - 1);
                assert_eq!(tick(&db, &client, &config, &clock).await, 0);
                clock.set(next);
            }
            None => {
                assert_eq!(attempt, 8, "gave up early");
                assert_eq!(delivery.state(), DeliveryState::GaveUp);
                assert_eq!(delivery.gave_up_at, Some(clock.now()));
            }
        }
    }
    assert_eq!(
        waits,
        vec![1, 2, 4, 8, 16, 32, 64],
        "minutes between attempts"
    );
    assert_eq!(
        clock.now() - start,
        127 * 60,
        "the last attempt is 2 h 7 min after the first"
    );
    clock.set(clock.now() + 365 * 86_400);
    assert_eq!(
        tick(&db, &client, &config, &clock).await,
        0,
        "never again by itself"
    );
    assert_eq!(received.requests.lock().len(), 8);
}

/// "Send again" on a given-up delivery starts its schedule over at once:
/// one more request, now answered, and its earlier attempts kept.
/// "Retry failed" does it for every given-up delivery of a webhook.
#[tokio::test]
async fn send_again_and_retry_failed_start_the_schedule_over() {
    let db = test_db();
    let store = add_store(&db, "s1", "Bakery");
    let status = Arc::new(AtomicU16::new(500));
    let received = Received::default();
    let url = endpoint(Arc::clone(&status), received.clone()).await;
    let (webhook, _) = add_webhook(&db, &store, &url).await;
    for (seq, order) in [(1, "o1"), (2, "o2"), (3, "o3")] {
        queue(&db, &event(seq, "s1", order, "order.paid"), 0);
    }
    let clock = ManualClock::at(1_000_000);
    // One attempt each: given up at once.
    let (client, config) = (client(), config(true, 1));
    assert_eq!(tick(&db, &client, &config, &clock).await, 3);
    let deliveries = db.lock().recent_deliveries(&webhook.id, 10).unwrap();
    assert!(deliveries
        .iter()
        .all(|d| d.state() == DeliveryState::GaveUp));
    assert_eq!(db.lock().webhook_health(&webhook.id, 0).unwrap().gave_up, 3);

    // Send again, one of them.
    status.store(200, Ordering::SeqCst);
    let one = deliveries[0].id;
    assert!(db
        .lock()
        .send_delivery_again(&store, &webhook.id, one, clock.now())
        .unwrap());
    // Another store's id is refused.
    let other = add_store(&db, "s2", "Other");
    assert!(!db
        .lock()
        .send_delivery_again(&other, &webhook.id, one, clock.now())
        .unwrap());
    assert_eq!(tick(&db, &client, &config, &clock).await, 1);
    let again = db
        .lock()
        .get_delivery(&store, &webhook.id, one)
        .unwrap()
        .unwrap();
    assert_eq!(again.state(), DeliveryState::Delivered);
    assert_eq!(again.attempt_count, 1, "a new schedule");
    assert_eq!(again.attempts.len(), 2, "the given-up attempt is kept");

    // Retry failed: the other two.
    assert_eq!(
        db.lock()
            .retry_failed_deliveries(&store, &webhook.id, clock.now())
            .unwrap(),
        2
    );
    assert_eq!(tick(&db, &client, &config, &clock).await, 2);
    let health = db.lock().webhook_health(&webhook.id, 0).unwrap();
    assert_eq!((health.gave_up, health.waiting), (0, 0));
    assert_eq!(health.delivered_recently, 3);
    assert_eq!(received.requests.lock().len(), 6);
}

/// One event per order is in flight at a time, oldest first, a later one
/// waiting while an earlier one is between attempts; and one store's
/// backlog leaves room for another store's delivery in the same batch.
#[tokio::test]
async fn deliveries_go_one_per_order_oldest_first_and_fairly_across_stores() {
    let db = test_db();
    let busy = add_store(&db, "busy", "Busy");
    let quiet = add_store(&db, "quiet", "Quiet");
    let (busy_hook, _) = add_webhook(&db, &busy, "https://busy.example/hook").await;
    let (quiet_hook, _) = add_webhook(&db, &quiet, "https://quiet.example/hook").await;
    // Two events for one order, then a backlog of other orders.
    queue(&db, &event(1, "busy", "o1", "order.unconfirmed"), 10);
    queue(&db, &event(2, "busy", "o1", "order.paid"), 10);
    for seq in 3..=12 {
        queue(
            &db,
            &event(seq, "busy", &format!("o{seq}"), "order.paid"),
            10,
        );
    }
    queue(&db, &event(13, "quiet", "q1", "order.paid"), 10);

    let due = db.lock().due_deliveries(10, 4, 50).unwrap();
    let busy_due: Vec<_> = due
        .iter()
        .filter(|d| d.webhook_id == busy_hook.id)
        .collect();
    assert_eq!(busy_due.len(), 4, "a store's share of the batch");
    assert_eq!(busy_due[0].event_id, "evt_1", "oldest first");
    assert!(
        !due.iter().any(|d| d.event_id == "evt_2"),
        "the order's later event waits for its earlier one"
    );
    assert!(due.iter().any(|d| d.webhook_id == quiet_hook.id));

    // The earlier event failing and waiting still holds the later one back.
    db.lock()
        .record_delivery_attempt(
            busy_due[0].delivery_id,
            &crate::db::AttemptOutcome {
                attempt: crate::db::Attempt {
                    n: 1,
                    at: 10,
                    status: Some(500),
                    error: None,
                    ms: 3,
                    signature: String::new(),
                },
                delivered: false,
                response: None,
                next_attempt_at: Some(70),
            },
        )
        .unwrap();
    let due = db.lock().due_deliveries(20, 50, 50).unwrap();
    assert!(!due
        .iter()
        .any(|d| d.event_id == "evt_1" || d.event_id == "evt_2"));
}

// -- Nothing private ----------------------------------------------------------

/// A delivery to a loopback address isn't sent unless private addresses
/// are allowed: the attempt fails, saying why, and the endpoint hears
/// nothing.
#[tokio::test]
async fn a_delivery_to_a_private_address_is_refused_unless_allowed() {
    let db = test_db();
    let store = add_store(&db, "s1", "Bakery");
    let received = Received::default();
    let url = endpoint(Arc::new(AtomicU16::new(200)), received.clone()).await;
    // By name and by address.
    let by_name = url.replace("127.0.0.1", "localhost");
    let (by_ip, _) = add_webhook(&db, &store, &url).await;
    let (named, _) = add_webhook(&db, &store, &by_name).await;
    queue(&db, &event(1, "s1", "o1", "order.paid"), 0);

    let clock = ManualClock::at(1000);
    let client = client();
    assert_eq!(tick(&db, &client, &config(false, 8), &clock).await, 2);
    for webhook in [&by_ip, &named] {
        let delivery = only_delivery(&db, webhook);
        assert_eq!(delivery.state(), DeliveryState::Retrying);
        let error = delivery.last_error.unwrap();
        assert!(
            error.contains("private or loopback"),
            "{}: {error}",
            webhook.url
        );
    }
    assert!(received.requests.lock().is_empty());

    // Allowed (testing against your own network): sent.
    clock.set(1060);
    assert_eq!(tick(&db, &client, &config(true, 8), &clock).await, 2);
    assert_eq!(received.requests.lock().len(), 2);
}

/// A redirect isn't followed: the attempt fails with the redirect's own
/// status, and the place it points to hears nothing.
#[tokio::test]
async fn a_redirect_is_not_followed() {
    let db = test_db();
    let store = add_store(&db, "s1", "Bakery");
    let target = Received::default();
    let target_url = endpoint(Arc::new(AtomicU16::new(200)), target.clone()).await;
    let app = axum::Router::new().route(
        "/hook",
        axum::routing::post(move || {
            let target_url = target_url.clone();
            async move { axum::response::Redirect::temporary(&target_url) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (webhook, _) = add_webhook(&db, &store, &format!("http://{addr}/hook")).await;
    queue(&db, &event(1, "s1", "o1", "order.paid"), 0);
    tick(&db, &client(), &config(true, 8), &ManualClock::at(1000)).await;
    let delivery = only_delivery(&db, &webhook);
    assert_eq!(delivery.last_status_code, Some(307));
    assert_eq!(delivery.state(), DeliveryState::Retrying);
    assert!(target.requests.lock().is_empty());
}

// -- The engine's log -----------------------------------------------------------

/// A coingecko stand-in pricing 1 XMR at 160 of any currency.
async fn coingecko() -> String {
    let app = axum::Router::new().route(
        "/api/v3/simple/price",
        axum::routing::get(|| async {
            (
                [("content-type", "application/json")],
                r#"{"monero":{"usd":160.0}}"#,
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// Monokulo with an engine in this process, a merchant's store on it
/// (`pk`), and its router.
struct Shop {
    state: AppState,
    engine: engine_test_support::TestEngineHandle,
    router: axum::Router,
    pk: String,
    store: ConnectionId,
}

async fn shop() -> Shop {
    let engine = engine_test_support::TestEngineConfig::new()
        .with_networks(&[monero::Network::Mainnet])
        .spawn()
        .await;
    let state = AppState {
        exchange_rate: Arc::new(
            crate::exchange_rate_config::ExchangeRateProviders::coingecko_only(coingecko().await),
        ),
        engine: crate::http::Engine::new(crate::engine_client::EngineClient::embedded_for_tests(
            engine.router(),
        )),
        ..AppState::for_tests()
    };
    let router = build_router(state.clone());
    let session = signed_up_and_logged_in_session_token(
        &router,
        "merchant@example.com",
        "correct horse battery staple",
    )
    .await;
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/connections")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {session}"))
                .body(Body::from(
                    serde_json::json!({
                        "site_url": "https://bakery.example",
                        "view_key_hex": "0707070707070707070707070707070707070707070707070707070707070707",
                        "spend_pubkey_hex": "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90",
                        "network": "mainnet",
                        "domains": [],
                        "base_currency": "XMR",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let pk = body_json(response).await["public_key"]
        .as_str()
        .unwrap()
        .to_string();
    let pk_lookup = pk.clone();
    let store = state
        .db
        .read(move |db| db.get_store_connection_by_public_key(&pk_lookup))
        .await
        .unwrap()
        .unwrap()
        .id;
    Shop {
        state,
        engine,
        router,
        pk,
        store,
    }
}

impl Shop {
    /// An order from the shop's checkout: its id.
    async fn order(&self, body: serde_json::Value) -> String {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/pay/{}/orders", self.pk))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        body_json(response).await["order_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Reads the engine's log in the background, as monokulo does.
    fn subscribe(&self) -> tokio::task::JoinHandle<()> {
        let (db, engine, webhooks) = (
            self.state.db.clone(),
            self.state.engine.client.clone(),
            Arc::clone(&self.state.webhooks),
        );
        tokio::spawn(async move {
            let _ =
                subscriber::follow(&db, &engine, &webhooks, &delivery::SystemClock, || {}).await;
        })
    }

    /// The webhook's deliveries, oldest first, once there are `count`.
    async fn deliveries(&self, webhook: &WebhookRow, count: usize) -> Vec<crate::db::DeliveryRow> {
        for _ in 0..500 {
            let mut deliveries = self
                .state
                .db
                .lock()
                .recent_deliveries(&webhook.id, 100)
                .unwrap();
            if deliveries.len() >= count {
                deliveries.reverse();
                return deliveries;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("expected {count} deliveries");
    }
}

/// The engine's events become one delivery each, with the v2 body: a fiat
/// order's price, rate and source, an XMR order's price without them, the
/// shop's order id and the store. Stopping the subscriber (a restart) and
/// starting it again carries on from the next event: nothing queued twice,
/// nothing skipped, and the position saved is the last event read.
#[tokio::test]
async fn the_subscriber_queues_each_event_once_across_a_restart() {
    let shop = shop().await;
    let (webhook, _) =
        add_webhook(&shop.state.db, &shop.store, "https://bakery.example/hook").await;
    let fiat = shop
        .order(serde_json::json!({ "amount": "25.00", "currency": "USD", "merchant_order_id": "wc-1042" }))
        .await;
    let xmr = shop
        .order(serde_json::json!({ "amount": "0.5", "currency": "XMR" }))
        .await;
    shop.engine.mark_order_paid(&fiat).unwrap();
    shop.engine.mark_order_expired(&xmr).unwrap();

    let reading = shop.subscribe();
    let first = shop.deliveries(&webhook, 2).await;
    reading.abort();
    let _ = reading.await;

    let paid: serde_json::Value = serde_json::from_str(&first[0].body).unwrap();
    assert_eq!(paid["api_version"], 2);
    assert_eq!(paid["event"], "order.paid");
    assert_eq!(paid["status"], "paid");
    assert_eq!(paid["order_id"], fiat.as_str());
    assert_eq!(paid["merchant_order_id"], "wc-1042");
    assert_eq!(paid["amount"], "25.00");
    assert_eq!(paid["currency"], "USD");
    assert_eq!(paid["fx_source"], "coingecko");
    assert_eq!(paid["fx_rate"], "160");
    assert_eq!(paid["xmr_amount"], "0.156250000000");
    assert_eq!(paid["store"]["id"], shop.store.as_str());
    let name = shop
        .state
        .db
        .lock()
        .get_store_connection_by_id(&shop.store)
        .unwrap()
        .unwrap()
        .name;
    assert_eq!(paid["store"]["name"], name.as_str());
    let expired: serde_json::Value = serde_json::from_str(&first[1].body).unwrap();
    assert_eq!(expired["event"], "order.expired");
    assert_eq!(expired["currency"], "XMR");
    assert_eq!(expired["amount"], "0.5");
    assert_eq!(expired["xmr_amount"], "0.500000000000");
    assert!(expired.get("fx_rate").is_none() && expired.get("fx_source").is_none());
    assert!(expired.get("merchant_order_id").is_none());

    // Monokulo is down while another order is paid.
    let later = shop
        .order(serde_json::json!({ "amount": "10.00", "currency": "USD" }))
        .await;
    shop.engine.mark_order_paid(&later).unwrap();
    let reading = shop.subscribe();
    let all = shop.deliveries(&webhook, 3).await;
    // Nothing more comes.
    tokio::time::sleep(Duration::from_millis(200)).await;
    reading.abort();
    let all_again = shop
        .state
        .db
        .lock()
        .recent_deliveries(&webhook.id, 100)
        .unwrap();
    assert_eq!(all_again.len(), 3, "nothing queued twice");
    assert_eq!(all[2].order_id.as_str(), later);
    let seqs: Vec<i64> = all.iter().map(|d| d.event_seq).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    let engine_events = shop.engine.store().lock().order_events_for_test().unwrap();
    assert_eq!(
        engine_events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        seqs,
        "every event the engine logged, none skipped"
    );
    assert_eq!(
        engine_events
            .iter()
            .map(|e| e.event_id.clone())
            .collect::<Vec<_>>(),
        all.iter().map(|d| d.event_id.clone()).collect::<Vec<_>>(),
        "the engine's event ids are the webhook's"
    );
    assert_eq!(
        shop.state.db.lock().order_event_position().unwrap(),
        *seqs.last().unwrap()
    );
}

/// An event for a store monokulo doesn't have, or one without webhooks,
/// still moves the position on; an event already read is left alone.
#[tokio::test]
async fn events_without_webhooks_move_the_position_and_old_events_are_ignored() {
    let db = test_db();
    let store = add_store(&db, "s1", "Bakery");
    assert_eq!(queue(&db, &event(1, "nobody", "o1", "order.paid"), 0), 0);
    assert_eq!(queue(&db, &event(2, "s1", "o2", "order.paid"), 0), 0);
    assert_eq!(db.lock().order_event_position().unwrap(), 2);
    let (webhook, _) = add_webhook(&db, &store, "https://bakery.example/hook").await;
    assert_eq!(
        queue(&db, &event(2, "s1", "o2", "order.paid"), 0),
        0,
        "read already"
    );
    assert_eq!(queue(&db, &event(3, "s1", "o3", "order.paid"), 0), 1);
    assert_eq!(
        db.lock().recent_deliveries(&webhook.id, 10).unwrap().len(),
        1
    );
}

/// When the engine pruned events monokulo hadn't read, the subscriber
/// moves past them (logging that they were lost) and carries on with the
/// ones still there.
#[tokio::test]
async fn the_subscriber_skips_events_the_engine_no_longer_has() {
    let shop = shop().await;
    let (webhook, _) =
        add_webhook(&shop.state.db, &shop.store, "https://bakery.example/hook").await;
    let old = shop
        .order(serde_json::json!({ "amount": "1.00", "currency": "USD" }))
        .await;
    shop.engine.mark_order_paid(&old).unwrap();
    // Monokulo had read up to here, then was away for longer than the
    // engine keeps events.
    shop.state.db.lock().skip_order_events_to(0, 0).unwrap();
    let pruned = shop
        .engine
        .store()
        .lock()
        .prune_order_events_before(i64::MAX)
        .unwrap();
    assert!(pruned >= 1);
    let new = shop
        .order(serde_json::json!({ "amount": "2.00", "currency": "USD" }))
        .await;
    shop.engine.mark_order_paid(&new).unwrap();
    // Read from a position that was saved before the pruned events.
    let reading = shop.subscribe();
    let deliveries = shop.deliveries(&webhook, 1).await;
    reading.abort();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].order_id.as_str(), new);
}

/// Delivery and the log together: an engine event, read and delivered to
/// a real endpoint, signed, with the counter of requests as the proof
/// nothing went twice.
#[tokio::test]
async fn an_engine_event_reaches_the_endpoint_once() {
    let shop = shop().await;
    let received = Received::default();
    let url = endpoint(Arc::new(AtomicU16::new(200)), received.clone()).await;
    let (webhook, secret) = add_webhook(&shop.state.db, &shop.store, &url).await;
    let order = shop
        .order(serde_json::json!({ "amount": "5.00", "currency": "USD" }))
        .await;
    shop.engine.mark_order_paid(&order).unwrap();
    let reading = shop.subscribe();
    shop.deliveries(&webhook, 1).await;
    reading.abort();
    let clock = ManualClock::at(crate::now_unix());
    let sent = AtomicUsize::new(0);
    for _ in 0..3 {
        sent.fetch_add(
            tick(&shop.state.db, &client(), &config(true, 8), &clock).await,
            Ordering::SeqCst,
        );
    }
    assert_eq!(sent.load(Ordering::SeqCst), 1);
    let (headers, body) = received.requests.lock()[0].clone();
    assert!(shared::webhook_sign::verify_signature(
        &secret,
        body.as_bytes(),
        headers["x-monokulo-signature"].to_str().unwrap(),
        clock.now()
    ));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["order_id"],
        order.as_str()
    );
}
