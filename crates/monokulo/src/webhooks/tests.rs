//! The webhook pipeline end to end: the engine's order-event log read into
//! deliveries (an engine in this process, no HTTP), the deliveries sent to
//! a real local endpoint, signed, retried on the real schedule (a clock the
//! test moves), given up on, and sent again.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU16, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use parking_lot::Mutex;
use tower::ServiceExt as _;

use super::delivery::{run_delivery_tick, Clock, WebhookClient};
use super::*;
use crate::db::{Db, DeliveryState, LoggedEvent, OrderId};
use crate::http::test_support::{
    api_request, body_json, signed_up_and_logged_in_session_token, store_with_keys,
};
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
        ..WebhookConfig::default()
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
        crate::stores::Site::parse(&format!("{id}.example"))
            .ok()
            .as_ref(),
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
        description: None,
        xmr_amount_piconero: 1_000_000_000_000,
    }
}

fn queue(db: &Database, event: &LoggedEvent, now: i64) -> usize {
    db.lock()
        .queue_order_event(event, now, |store, metadata| {
            body::body_v2(event, store, metadata, &Default::default())
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
            "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/hook",
            vec![],
            "onion address",
        ),
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
    /// The store's secret key, for what its POS does through the engine.
    sk: String,
    store: ConnectionId,
    /// The merchant's session, for the dashboard.
    session: String,
}

async fn shop() -> Shop {
    shop_with("").await
}

/// [`shop`], with `options` added to its options file.
async fn shop_with(options: &str) -> Shop {
    let engine = engine_test_support::TestEngineConfig::new()
        .with_networks(&[monero::Network::Mainnet])
        .spawn()
        .await;
    // At a web address, as an instance a plugin connects to is: the
    // bodies' links are on it. (The options set the instance's own rate
    // providers, so the local Coingecko goes in after them.)
    let state = AppState {
        engine: crate::http::Engine::new(crate::engine_client::EngineClient::embedded_for_tests(
            engine.router(),
        )),
        ..AppState::for_tests()
    }
    .with_options(&format!(
        "public_uri = \"https://www.monokulo.com\"\n[signup]\nmode = \"public\"\n{options}"
    ))
    .await;
    let state = AppState {
        exchange_rate: Arc::new(
            crate::exchange_rate_config::ExchangeRateProviders::coingecko_only(coingecko().await),
        ),
        ..state
    };
    let router = build_router(state.clone());
    let session = signed_up_and_logged_in_session_token(
        &router,
        "merchant@example.com",
        "correct horse battery staple",
    )
    .await;
    let made = store_with_keys(&router, &state, &session, "https://bakery.example", "XMR").await;
    let (pk, sk) = (made.public_key, made.secret_key);
    let store = ConnectionId::new(made.id);
    Shop {
        state,
        engine,
        router,
        pk,
        sk,
        store,
        session,
    }
}

impl Shop {
    /// An order from the shop's checkout: its id.
    async fn order(&self, body: serde_json::Value) -> String {
        let response = self
            .router
            .clone()
            .oneshot(api_request(
                "POST",
                "/api/v1/orders",
                Some(&self.pk),
                Some(body),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
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
        let addresses =
            crate::webhooks::Addresses::new(self.state.settings.clone(), self.state.tor.clone());
        tokio::spawn(async move {
            let _ = subscriber::follow(
                &db,
                &engine,
                &webhooks,
                &Default::default(),
                &addresses,
                &delivery::SystemClock,
                || {},
            )
            .await;
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
        .order(serde_json::json!({
            "amount": "25.00",
            "currency": "USD",
            "merchant_order_id": "wc-1042",
            "description": "Order #1042: Blue mug + 2 more",
        }))
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
    assert_eq!(paid["description"], "Order #1042: Blue mug + 2 more");
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
    assert!(expired.get("description").is_none());

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
    assert_eq!(all[2].order_id.as_ref().unwrap().as_str(), later);
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

/// The contract plugins test against: what a real store delivers for each
/// way an order goes has exactly the fields its published vector has
/// (`vectors.rs`), so a plugin passing on the vectors handles what
/// monokulo really sends.
#[tokio::test]
async fn real_deliveries_have_the_fields_of_their_published_vectors() {
    let shop = shop().await;
    let (webhook, _) =
        add_webhook(&shop.state.db, &shop.store, "https://bakery.example/hook").await;
    let order = || {
        shop.order(serde_json::json!({
            "amount": "25.00",
            "currency": "USD",
            "merchant_order_id": "1042",
            "description": "Order #1042: Blue mug + 2 more",
        }))
    };
    let pay = |order: &str, piconero: u64, confirmations: Option<u64>| {
        shop.engine
            .record_order_payment(order, shared::xmr_amount::Piconero(piconero), confirmations)
            .unwrap();
    };
    let seen = order().await;
    pay(&seen, 156_250_000_000, None);
    let partly = order().await;
    pay(&partly, 1, Some(101));
    let paid = order().await;
    shop.engine.mark_order_paid(&paid).unwrap();
    let over = order().await;
    pay(&over, 1_000_000_000_000_000, Some(101));
    let expired = order().await;
    shop.engine.mark_order_expired(&expired).unwrap();
    // Cancelled, as the POS cancels one through the engine.
    let cancelled = order().await;
    shop.state
        .engine
        .client
        .cancel_order(
            &shared::auth::RawToken::presented(&shop.sk),
            &shared::ids::OrderId::new(cancelled),
        )
        .await
        .unwrap();
    // The double-spend events come only from the scanner voiding a real
    // payment, which no test engine fakes: their vectors are checked
    // against the body builder alone (`vectors.rs`).

    let reading = shop.subscribe();
    let engine_events = shop.engine.store().lock().order_events_for_test().unwrap();
    let delivered = shop.deliveries(&webhook, engine_events.len()).await;
    reading.abort();

    let mut events = std::collections::BTreeSet::new();
    for delivery in &delivered {
        let body: serde_json::Value = serde_json::from_str(&delivery.body).unwrap();
        let event = body["event"].as_str().unwrap().to_owned();
        let expected = super::vectors::fields_of(&event)
            .unwrap_or_else(|| panic!("no published vector for {event}"));
        let fields: Vec<String> = body.as_object().unwrap().keys().cloned().collect();
        assert_eq!(fields, expected, "{event}: {body}");
        // Its links lead to its order, on the web address.
        let order = body["order_id"].as_str().unwrap();
        assert_eq!(
            body["_links"]["mk:order"]["href"],
            format!("https://www.monokulo.com/api/v1/orders/{order}"),
            "{event}"
        );
        assert_eq!(
            body["_links"]["mk:dashboard-order"]["href"],
            format!(
                "https://www.monokulo.com/stores/{}/orders/{order}",
                shop.store
            ),
            "{event}"
        );
        events.insert(event);
    }
    for event in [
        "order.unconfirmed",
        "order.partial",
        "order.paid",
        "order.overpaid",
        "order.expired",
        "order.cancelled",
    ] {
        assert!(events.contains(event), "{event} delivered: {events:?}");
    }

    // And a test delivery, as the API's `send-test-webhook` form sends one
    // (sent nowhere: the body is made and listed whatever the receiver
    // does): the published vector's fields, and none an order event has.
    let answer = shop
        .router
        .clone()
        .oneshot(api_request(
            "POST",
            "/api/v1/webhook-tests",
            Some(&shop.sk),
            Some(serde_json::json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(answer.status(), StatusCode::OK);
    let test = &shop.deliveries(&webhook, delivered.len() + 1).await[delivered.len()];
    assert!(test.is_test());
    let body: serde_json::Value = serde_json::from_str(&test.body).unwrap();
    let fields: Vec<String> = body.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        Some(fields),
        super::vectors::fields_of("webhook.test"),
        "{body}"
    );
    for order_field in ["order_id", "status", "txid", "amount", "xmr_amount"] {
        assert!(body.get(order_field).is_none(), "{order_field}: {body}");
    }
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
    assert_eq!(deliveries[0].order_id.as_ref().unwrap().as_str(), new);
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

// -- Keeping deliveries ------------------------------------------------------

/// Delivered deliveries go after `webhooks.keep_delivered_days` (30) and
/// given-up ones after `webhooks.keep_given_up_days` (90), each a second
/// past its period and not a second before; waiting ones always stay.
#[tokio::test]
async fn old_deliveries_are_pruned_delivered_after_30_days_given_up_after_90() {
    let db = test_db();
    let store = add_store(&db, "s1", "Bakery");
    let (webhook, _) = add_webhook(&db, &store, "https://bakery.example/hook").await;
    let now = 1_000_000_000;
    let day = 86_400;
    // (order, delivered, at): `at` is when it was delivered or given up.
    let cases = [
        ("delivered-old", true, now - 30 * day - 1),
        ("delivered-edge", true, now - 30 * day),
        ("gave-up-at-31-days", false, now - 31 * day),
        ("gave-up-old", false, now - 90 * day - 1),
        ("gave-up-edge", false, now - 90 * day),
    ];
    for (seq, (order, delivered, at)) in cases.iter().enumerate() {
        let seq = i64::try_from(seq).unwrap() + 1;
        queue(&db, &event(seq, "s1", order, "order.paid"), at - 60);
        let id = db.lock().recent_deliveries(&webhook.id, 1).unwrap()[0].id;
        db.lock()
            .record_delivery_attempt(
                id,
                &crate::db::AttemptOutcome {
                    attempt: crate::db::Attempt {
                        n: 1,
                        at: *at,
                        status: Some(if *delivered { 200 } else { 500 }),
                        error: None,
                        ms: 5,
                        signature: String::new(),
                    },
                    delivered: *delivered,
                    response: None,
                    next_attempt_at: None,
                },
            )
            .unwrap();
    }
    // Still waiting, from long ago.
    queue(
        &db,
        &event(9, "s1", "waiting", "order.paid"),
        now - 400 * day,
    );

    let pruned = delivery::prune_deliveries(&db, &WebhookConfig::default(), now)
        .await
        .unwrap();
    assert_eq!(pruned, (1, 1));
    let mut left: Vec<String> = db
        .lock()
        .recent_deliveries(&webhook.id, 100)
        .unwrap()
        .into_iter()
        .map(|d| d.order_id.as_ref().unwrap().to_string())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "delivered-edge",
            "gave-up-at-31-days",
            "gave-up-edge",
            "waiting"
        ]
    );

    // Shorter periods, from the settings: both kinds go sooner.
    let short = WebhookConfig {
        keep_delivered_secs: day,
        keep_given_up_secs: 2 * day,
        ..WebhookConfig::default()
    };
    assert_eq!(
        delivery::prune_deliveries(&db, &short, now).await.unwrap(),
        (1, 2)
    );
    assert_eq!(
        db.lock().recent_deliveries(&webhook.id, 100).unwrap().len(),
        1
    );
}

/// An engine event about an order a connected app made reaches the store's
/// app event streams as a sale event; one about any other order doesn't.
#[tokio::test]
async fn the_subscriber_tells_the_stores_apps_about_their_orders_only() {
    let shop = shop().await;
    let key = shared::auth::generate_app_key();
    let token = shared::auth::RawToken::presented("ct_sub").hash();
    {
        let db = shop.state.db.lock();
        db.create_app_connect_token(&token, &shop.store, "Till", crate::now_unix())
            .unwrap();
        db.redeem_app_connect_token(
            &token,
            crate::now_unix(),
            600,
            &crate::db::NewApp {
                slug: "monokular",
                name: "Monokular",
                device_name: "",
                platform: "",
                version: "",
                key_hash: &key.hash(),
            },
        )
        .unwrap()
        .unwrap();
    }
    let by_app = self::order_with_key(&shop, key.expose()).await;
    let other = shop
        .order(serde_json::json!({ "amount": "0.5", "currency": "XMR" }))
        .await;

    let apps = Arc::new(crate::apps::AppEvents::default());
    let mut heard = apps.subscribe();
    let (db, engine, webhooks, listening) = (
        shop.state.db.clone(),
        shop.state.engine.client.clone(),
        Arc::clone(&shop.state.webhooks),
        Arc::clone(&apps),
    );
    let addresses =
        crate::webhooks::Addresses::new(shop.state.settings.clone(), shop.state.tor.clone());
    let reader = tokio::spawn(async move {
        let _ = subscriber::follow(
            &db,
            &engine,
            &webhooks,
            &listening,
            &addresses,
            &delivery::SystemClock,
            || {},
        )
        .await;
    });
    shop.engine.mark_order_expired(&other).unwrap();
    shop.engine.mark_order_paid(&by_app).unwrap();
    let event = tokio::time::timeout(Duration::from_secs(5), heard.recv())
        .await
        .expect("an announcement")
        .unwrap();
    assert_eq!(event.public_key, shop.pk);
    assert_eq!(event.event.event, "sale.paid");
    assert_eq!(event.event.order_id, by_app);
    assert_eq!(event.event.device_name, "Till");
    reader.abort();
}

/// An order made with a connected app's key: its id.
async fn order_with_key(shop: &Shop, key: &str) -> String {
    let response = shop
        .router
        .clone()
        .oneshot(api_request(
            "POST",
            "/api/v1/orders",
            Some(key),
            Some(serde_json::json!({ "amount": "0.25", "currency": "XMR" })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    body_json(response).await["order_id"]
        .as_str()
        .unwrap()
        .to_string()
}

mod test_deliveries;
