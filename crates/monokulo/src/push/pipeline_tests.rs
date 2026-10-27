//! Pushes end to end: sale events queued for the targets that want them
//! (in the event's own transaction, beside its webhook deliveries), an
//! app's new order queued as `sale.waiting`, superseding and expiry, and
//! the worker sending them to a push service on this machine, on a clock
//! the test moves. The store, its apps, their targets and their orders are
//! made through the API, against an engine in this process.
//!
//! Only the tests where a push has to land go over real HTTP (to a stub
//! UnifiedPush endpoint on `127.0.0.1:0`), and say `over_http`.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tower::ServiceExt as _;

use super::delivery::{run_delivery_tick, PushClient, NO_ATTEMPTS_LEFT};
use super::Queueing;
use crate::api::runtime::recording::{connect_app, fixture, Fixture, Recorder, PUBLIC_URI};
use crate::db::{
    AppId, LoggedEvent, OrderId, PushDeliveryRow, PushTargetId, Queued, EXPIRED_UNSENT,
};
use crate::http::test_support::{api_request, body_json};
use crate::outbound::Reach;
use crate::settings::PushConfig;
use crate::webhooks::delivery::Clock;

/// A clock the test sets.
struct ManualClock(AtomicI64);

impl ManualClock {
    fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
    fn advance(&self, seconds: i64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A store with a connected app, on an engine in this process, and what
/// pushes are sent with.
struct World {
    engine: engine_test_support::TestEngineHandle,
    fixture: Fixture,
    client: PushClient,
    clock: ManualClock,
}

async fn world() -> World {
    let engine =
        engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let client = PushClient::build(fixture.state.push.vapid(), None).unwrap();
    World {
        engine,
        fixture,
        client,
        clock: ManualClock(AtomicI64::new(crate::now_unix())),
    }
}

impl World {
    async fn call(&self, method: &str, uri: &str, key: &str, body: Option<Value>) -> Value {
        let answer = self
            .fixture
            .router
            .clone()
            .oneshot(api_request(method, uri, Some(key), body))
            .await
            .unwrap();
        assert!(
            answer.status().is_success(),
            "{method} {uri}: {}",
            answer.status()
        );
        body_json(answer).await
    }

    /// Registers `body` as a push target of the app with `key`.
    async fn register(&self, key: &str, body: Value) -> PushTargetId {
        let made = self
            .call("POST", "/api/v1/push-targets", key, Some(body))
            .await;
        PushTargetId::new(made["push_target_id"].as_str().unwrap())
    }

    /// Another app connected to the store: its key.
    async fn another_app(&self) -> String {
        let connected = connect_app(
            &self.fixture.router,
            &self.fixture.store_id,
            &self.fixture.cookie,
        )
        .await;
        connected["app_key"].as_str().unwrap().to_owned()
    }

    /// An order made with `key` (an app's, or the store's secret key).
    async fn order(&self, key: &str) -> OrderId {
        let made = self
            .call(
                "POST",
                "/api/v1/orders",
                key,
                Some(json!({"amount": "24.50", "currency": "XMR", "merchant_order_id": "#1042"})),
            )
            .await;
        OrderId::new(made["order_id"].as_str().unwrap())
    }

    /// The engine's order event `kind` about `order`, at `seq` in its log.
    fn event(&self, seq: i64, order: &OrderId, kind: &str) -> LoggedEvent {
        LoggedEvent {
            seq,
            event_id: format!("evt_{seq}"),
            kind: kind.into(),
            created_at: self.clock.now(),
            tenant_public_key: self.fixture.pk.clone(),
            order_id: order.clone(),
            txid: None,
            merchant_order_id: Some("#1042".into()),
            description: None,
            xmr_amount_piconero: pricing::xmr_amount::Piconero(24_500_000_000_000),
        }
    }

    /// Queues `event` as the subscriber does, now.
    fn queue(&self, event: &LoggedEvent) -> Queued {
        let queueing = Queueing::now(&self.fixture.state.settings);
        self.fixture
            .state
            .db
            .lock()
            .queue_order_event(event, self.clock.now(), &queueing, |_, _| "{}".to_owned())
            .unwrap()
    }

    fn pushes(&self, target: &PushTargetId) -> Vec<PushDeliveryRow> {
        self.fixture
            .state
            .db
            .lock()
            .push_deliveries(target)
            .unwrap()
    }

    fn position(&self) -> i64 {
        self.fixture
            .state
            .db
            .lock()
            .order_event_position()
            .unwrap()
            .seq
    }

    fn target_exists(&self, app: &AppId, target: &PushTargetId) -> bool {
        self.fixture
            .state
            .db
            .lock()
            .get_push_target(app, target)
            .unwrap()
            .is_some()
    }

    async fn app_id(&self, key: &str) -> AppId {
        let app = self.call("GET", "/api/v1/app", key, None).await;
        AppId::new(app["app_id"].as_str().unwrap())
    }

    /// One batch of the worker, as configured, to anywhere (the stub is
    /// on this machine).
    async fn tick_with(&self, config: &PushConfig) -> usize {
        run_delivery_tick(
            &self.fixture.state.db,
            &self.client,
            config,
            Reach::AnyAddress,
            &self.clock,
        )
        .await
        .unwrap()
    }

    async fn tick(&self) -> usize {
        self.tick_with(&PushConfig::default()).await
    }
}

/// An `encrypted` target named `client_id` at `endpoint`, to `receiver`'s
/// keys, wanting `events` (every one when `None`).
fn encrypted(
    client_id: &str,
    endpoint: &str,
    receiver: &::push::encrypt::ReceiverKeys,
    events: Option<&[&str]>,
) -> Value {
    let keys = receiver.subscription_keys();
    let mut body = json!({
        "client_id": client_id,
        "kind": "encrypted",
        "endpoint": endpoint,
        "keys": {"p256dh": keys.p256dh.to_base64url(), "auth": keys.auth.to_base64url()},
    });
    if let Some(events) = events {
        body["events"] = json!(events);
    }
    body
}

fn live_activity(client_id: &str, endpoint: &str, include_payment_uri: bool) -> Value {
    json!({
        "client_id": client_id,
        "kind": "live-activity",
        "endpoint": endpoint,
        "include_payment_uri": include_payment_uri,
    })
}

/// What the stub push service was sent.
struct Received {
    path: String,
    headers: HeaderMap,
    body: Vec<u8>,
}

/// A push service on this machine answering every push with `status`
/// (and `Retry-After`, when set), after `delay`, all changeable while it
/// runs; it counts the pushes it holds at once.
#[derive(Clone, Default)]
struct Stub {
    received: Arc<Mutex<Vec<Received>>>,
    answer: Arc<Mutex<(u16, Option<&'static str>)>>,
    delay: Arc<Mutex<std::time::Duration>>,
    in_flight: Arc<AtomicUsize>,
    most_in_flight: Arc<AtomicUsize>,
}

impl Stub {
    /// Starts one: it and its origin (`http://127.0.0.1:…`).
    async fn start(status: u16) -> (Stub, String) {
        async fn push(
            axum::extract::State(stub): axum::extract::State<Stub>,
            uri: axum::http::Uri,
            headers: HeaderMap,
            body: axum::body::Bytes,
        ) -> axum::response::Response {
            use axum::response::IntoResponse as _;
            stub.received.lock().push(Received {
                path: uri.path().to_owned(),
                headers,
                body: body.to_vec(),
            });
            let held = stub.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            stub.most_in_flight.fetch_max(held, Ordering::SeqCst);
            let delay = *stub.delay.lock();
            tokio::time::sleep(delay).await;
            stub.in_flight.fetch_sub(1, Ordering::SeqCst);
            let (status, retry_after) = *stub.answer.lock();
            let mut answer = StatusCode::from_u16(status).unwrap().into_response();
            if let Some(retry_after) = retry_after {
                answer.headers_mut().insert(
                    axum::http::header::RETRY_AFTER,
                    axum::http::HeaderValue::from_static(retry_after),
                );
            }
            answer
        }
        let stub = Stub {
            answer: Arc::new(Mutex::new((status, None))),
            ..Stub::default()
        };
        let app = axum::Router::new().fallback(push).with_state(stub.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (stub, format!("http://{addr}"))
    }

    fn answer(&self, status: u16, retry_after: Option<&'static str>) {
        *self.answer.lock() = (status, retry_after);
    }

    fn count(&self) -> usize {
        self.received.lock().len()
    }

    /// Answers each push only after `delay`.
    fn hold(&self, delay: std::time::Duration) {
        *self.delay.lock() = delay;
    }
}

/// What the tests' log lines say, at every level.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Lines {
    fn capture() -> (Lines, tracing::subscriber::DefaultGuard) {
        let lines = Lines::default();
        let writer = lines.clone();
        let (_telemetry, subscriber) = telemetry::build(
            "monokulo",
            telemetry::Format::Json,
            false,
            "trace",
            move || writer.clone(),
        );
        (lines, tracing::subscriber::set_default(subscriber))
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).into_owned()
    }
}

// -- Queueing ----------------------------------------------------------------

/// An app order's `order.paid` queues one push for each target of the
/// store's apps that wants `sale.paid`, none for one that doesn't, in the
/// same transaction as the event's webhook delivery and the one move of
/// the position. An order the store's server made is no app's: its
/// webhook is queued, no push.
#[tokio::test]
async fn an_app_orders_paid_event_queues_a_push_for_each_target_that_wants_it() {
    let world = world().await;
    let ak = world.fixture.ak.clone();
    let device = ::push::encrypt::ReceiverKeys::generate();
    let wants_all = world
        .register(
            &ak,
            encrypted("all", "https://push.example/a", &device, None),
        )
        .await;
    let not_paid = world
        .register(
            &ak,
            encrypted(
                "not-paid",
                "https://push.example/b",
                &device,
                Some(&["sale.seen", "sale.expired"]),
            ),
        )
        .await;
    let island = world
        .register(
            &ak,
            live_activity("island", "https://relay.example/c", false),
        )
        .await;
    let order = world.order(&ak).await;

    let queued = world.queue(&world.event(1, &order, "order.paid"));
    assert_eq!(
        queued,
        Queued {
            webhooks: 1,
            pushes: 2
        }
    );
    assert_eq!(world.position(), 1);
    let paid = world.pushes(&wants_all);
    assert_eq!(paid.len(), 1, "{paid:?}");
    let paid = &paid[0];
    assert_eq!(
        (
            paid.event_seq,
            paid.event_id.as_str(),
            paid.event_type.as_str(),
            &paid.order_id,
            paid.urgency.as_str()
        ),
        (1, "evt_1", "sale.paid", &order, "high")
    );
    assert_eq!(paid.topic, policy_topic(&wants_all, &order));
    assert_eq!(paid.next_attempt_at, Some(world.clock.now()));
    assert_eq!(
        paid.expires_at,
        world.clock.now() + i64::from(PushConfig::default().default_ttl.as_secs())
    );
    let payload: Value = serde_json::from_str(&paid.payload).unwrap();
    assert_eq!(payload["event"], "sale.paid");
    assert_eq!(payload["api_version"], crate::webhooks::body::API_VERSION);
    assert!(world.pushes(&not_paid).is_empty());
    assert_eq!(world.pushes(&island).len(), 1);

    // Queued once: the same event again is behind the position.
    assert_eq!(
        world.queue(&world.event(1, &order, "order.paid")),
        Queued::default()
    );
    assert_eq!(world.pushes(&wants_all).len(), 1);

    let shops = world.order(&world.fixture.sk.clone()).await;
    assert_eq!(
        world.queue(&world.event(2, &shops, "order.paid")),
        Queued {
            webhooks: 1,
            pushes: 0
        }
    );
    assert_eq!(world.position(), 2);
    // An event apps aren't told about queues no push either.
    assert_eq!(
        world
            .queue(&world.event(3, &order, "order.double_spend_reversed"))
            .pushes,
        0
    );
}

fn policy_topic(target: &PushTargetId, order: &OrderId) -> String {
    super::policy::topic(target, order).as_str().to_owned()
}

/// Ten targets of two apps, one event: ten pushes, and the position moves
/// once, to the event.
#[tokio::test]
async fn ten_targets_and_one_event_make_ten_pushes_and_one_position_move() {
    let world = world().await;
    let other = world.another_app().await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let mut targets = Vec::new();
    for (key, n) in (0..8)
        .map(|n| (world.fixture.ak.clone(), n))
        .chain((8..10).map(|n| (other.clone(), n)))
    {
        let endpoint = format!("https://push.example/{n}");
        targets.push(
            world
                .register(&key, encrypted(&format!("t{n}"), &endpoint, &device, None))
                .await,
        );
    }
    // Made by a third app, so none of the ten is pushed its `sale.waiting`.
    let maker = world.another_app().await;
    let order = world.order(&maker).await;
    for target in &targets {
        assert_eq!(world.pushes(target).len(), 1, "{target}");
    }
    assert_eq!(
        world
            .queue(&world.event(7, &order, "order.unconfirmed"))
            .pushes,
        10
    );
    assert_eq!(world.position(), 7);
    for target in &targets {
        let pushes = world.pushes(target);
        let events: Vec<&str> = pushes.iter().map(|p| p.event_type.as_str()).collect();
        assert_eq!(events, ["sale.waiting", "sale.seen"], "{target}");
    }
}

/// When an app makes an order, the store's other apps' targets are pushed
/// `sale.waiting` at once: no place in the engine's log (0), an event id
/// of its own. The app that made it shows it already, so isn't pushed it;
/// the engine's `order.pending` later (waiting again) is pushed to all.
#[tokio::test]
async fn sale_waiting_is_queued_for_the_other_apps_when_an_app_makes_an_order() {
    let world = world().await;
    let other = world.another_app().await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let maker = world
        .register(
            &world.fixture.ak.clone(),
            encrypted("mine", "https://push.example/mine", &device, None),
        )
        .await;
    let theirs = world
        .register(
            &other,
            encrypted("theirs", "https://push.example/theirs", &device, None),
        )
        .await;
    let order = world.order(&world.fixture.ak.clone()).await;

    assert!(world.pushes(&maker).is_empty());
    let waiting = world.pushes(&theirs);
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    let waiting = &waiting[0];
    assert_eq!(
        (
            waiting.event_seq,
            waiting.event_type.as_str(),
            waiting.urgency.as_str()
        ),
        (0, "sale.waiting", "normal")
    );
    assert!(
        waiting.event_id.starts_with("evt_") && waiting.event_id.len() == 36,
        "{}",
        waiting.event_id
    );
    assert_eq!(waiting.order_id, order);
    // The engine's log wasn't read for it.
    assert_eq!(world.position(), 0);
    // A sale in progress lasts until it expires, at most the default.
    let ttl = waiting.expires_at - waiting.created_at;
    assert!(
        (60..=i64::from(PushConfig::default().default_ttl.as_secs())).contains(&ttl),
        "{ttl}"
    );

    // Waiting again after its payment was spent twice: news to every app,
    // the one that made it included.
    assert_eq!(
        world.queue(&world.event(1, &order, "order.pending")).pushes,
        2
    );
    let again = world.pushes(&maker);
    assert_eq!(
        (
            again.len(),
            again[0].event_type.as_str(),
            again[0].event_seq
        ),
        (1, "sale.waiting", 1)
    );
}

/// A newer push about the same order supersedes one not sent yet; one
/// already delivered stays as it was.
#[tokio::test]
async fn a_newer_push_supersedes_an_unsent_one_about_the_same_order() {
    let world = world().await;
    let ak = world.fixture.ak.clone();
    let device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(&ak, encrypted("t", "https://push.example/t", &device, None))
        .await;
    let order = world.order(&ak).await;
    let other_order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.confirming"));
    world.queue(&world.event(2, &other_order, "order.confirming"));
    world.clock.advance(30);
    world.queue(&world.event(3, &order, "order.confirming"));

    let pushes = world.pushes(&target);
    let ids: Vec<&str> = pushes.iter().map(|p| p.event_id.as_str()).collect();
    assert_eq!(ids, ["evt_1", "evt_2", "evt_3"]);
    assert_eq!(pushes[0].superseded_at, Some(world.clock.now()));
    assert_eq!(pushes[0].next_attempt_at, None);
    // Another order's topic is its own.
    assert_eq!(pushes[1].superseded_at, None);
    assert!(pushes[1].next_attempt_at.is_some());
    assert_eq!(pushes[2].superseded_at, None);
    assert!(pushes[2].next_attempt_at.is_some());
}

/// Each target's pushes about an order carry a topic of its own: two
/// tills of one store are never pushed the same value, which the relay,
/// Apple, Google or a distributor would read as "one store's devices".
/// A later event about the order supersedes only each target's own unsent
/// push, and another order gives each target a new topic.
#[tokio::test]
async fn each_target_gets_its_own_topic_for_an_order() {
    let world = world().await;
    let other = world.another_app().await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let mine = world
        .register(
            &world.fixture.ak.clone(),
            encrypted("mine", "https://push.example/mine", &device, None),
        )
        .await;
    let theirs = world
        .register(
            &other,
            encrypted("theirs", "https://push.example/theirs", &device, None),
        )
        .await;
    let maker = world.another_app().await;
    let order = world.order(&maker).await;
    world.queue(&world.event(1, &order, "order.unconfirmed"));
    let topic = |target: &PushTargetId, n: usize| world.pushes(target)[n].topic.clone();
    for n in [0, 1] {
        assert_ne!(topic(&mine, n), topic(&theirs, n), "push {n}");
    }
    assert_eq!(topic(&mine, 0), topic(&mine, 1));
    assert_eq!(topic(&mine, 0), policy_topic(&mine, &order));

    world.queue(&world.event(2, &order, "order.confirming"));
    for target in [&mine, &theirs] {
        let superseded: Vec<bool> = world
            .pushes(target)
            .iter()
            .map(|p| p.superseded_at.is_some())
            .collect();
        assert_eq!(superseded, [true, true, false], "{target}");
    }

    let next = world.order(&maker).await;
    for target in [&mine, &theirs] {
        let pushes = world.pushes(target);
        let last = pushes.last().unwrap();
        assert_eq!(last.order_id, next);
        assert_ne!(last.topic, topic(target, 0), "{target}");
        assert_eq!(last.topic, policy_topic(target, &next));
    }
    assert_ne!(
        world.pushes(&mine).last().unwrap().topic,
        world.pushes(&theirs).last().unwrap().topic
    );
}

/// A disconnected app's targets went with it: its store's later events
/// push nothing.
#[tokio::test]
async fn a_disconnected_apps_targets_are_pushed_nothing() {
    let world = world().await;
    let other = world.another_app().await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(
            &other,
            encrypted("t", "https://push.example/t", &device, None),
        )
        .await;
    let app = world.app_id(&other).await;
    let order = world.order(&world.fixture.ak.clone()).await;
    assert_eq!(world.pushes(&target).len(), 1);
    let store = crate::db::ConnectionId::new(world.fixture.store_id.clone());
    assert!(world
        .fixture
        .state
        .db
        .lock()
        .delete_app(&store, &app)
        .unwrap()
        .is_some());
    assert!(!world.target_exists(&app, &target));
    assert!(world.pushes(&target).is_empty());
    assert_eq!(world.queue(&world.event(1, &order, "order.paid")).pushes, 0);
}

/// A Live Activity's state counts the store's app orders still waiting,
/// and carries the payment URI only to a target that asked for it.
#[tokio::test]
async fn a_live_activity_counts_the_waiting_sales_and_shows_the_code_only_when_asked() {
    let world = world().await;
    let other = world.another_app().await;
    let with_code = world
        .register(
            &other,
            live_activity("code", "https://relay.example/a", true),
        )
        .await;
    let without = world
        .register(
            &other,
            live_activity("plain", "https://relay.example/b", false),
        )
        .await;
    let first = world.order(&world.fixture.ak.clone()).await;
    let second = world.order(&world.fixture.ak.clone()).await;
    let state = |target: &PushTargetId, n: usize| -> Value {
        serde_json::from_str(&world.pushes(target)[n].payload).unwrap()
    };
    // The second sale was made while the first still waited.
    assert_eq!(state(&with_code, 1)["phase"], "waiting");
    assert_eq!(state(&with_code, 1)["waitingCount"], 2);
    assert_eq!(state(&with_code, 1)["sale"], "#1042");
    let uri = state(&with_code, 1)["paymentURI"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        uri.starts_with("monero:") && uri.ends_with("?tx_amount=24.5"),
        "{uri}"
    );
    assert!(state(&without, 1).get("paymentURI").is_none());

    world.queue(&world.event(1, &first, "order.paid"));
    let paid = state(&with_code, 2);
    assert_eq!(
        (&paid["phase"], &paid["waitingCount"]),
        (&json!("paid"), &json!(1))
    );
    assert!(paid.get("paymentURI").is_none(), "{paid}");
    world.queue(&world.event(2, &second, "order.expired"));
    assert_eq!(state(&without, 3)["waitingCount"], 0);
}

// -- The worker, in process --------------------------------------------------

/// A push found due after its TTL ended is given up on, with no request.
#[tokio::test]
async fn a_push_past_its_ttl_is_given_up_without_a_request() {
    let world = world().await;
    let (stub, origin) = Stub::start(201).await;
    let ak = world.fixture.ak.clone();
    let device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("{origin}/up/1"), &device, None),
        )
        .await;
    let order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.paid"));
    let expires_at = world.pushes(&target)[0].expires_at;
    world.clock.set(expires_at);

    assert_eq!(world.tick().await, 1);
    assert_eq!(stub.count(), 0);
    let push = &world.pushes(&target)[0];
    assert_eq!(push.gave_up_at, Some(expires_at));
    assert_eq!(push.last_error.as_deref(), Some(EXPIRED_UNSENT));
    assert_eq!(push.attempt_count, 0);
    assert_eq!(world.tick().await, 0);
}

/// A push whose row can't be read (a name no sale event has, written by
/// hand) is given up on, saying so, and the ones beside it are sent.
#[tokio::test]
async fn an_unreadable_push_is_given_up_and_the_others_are_sent() {
    let world = world().await;
    let (stub, origin) = Stub::start(201).await;
    let ak = world.fixture.ak.clone();
    let device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("{origin}/up/1"), &device, None),
        )
        .await;
    let (first, second) = (world.order(&ak).await, world.order(&ak).await);
    world.queue(&world.event(1, &first, "order.paid"));
    world.queue(&world.event(2, &second, "order.paid"));
    world
        .fixture
        .state
        .db
        .lock()
        .conn_for_test()
        .execute(
            "UPDATE push_deliveries SET event_type = 'sale.refunded' WHERE event_id = 'evt_1'",
            [],
        )
        .unwrap();

    assert_eq!(world.tick().await, 2);
    assert_eq!(stub.count(), 1);
    let pushes = world.pushes(&target);
    assert_eq!(pushes[0].gave_up_at, Some(world.clock.now()));
    let error = pushes[0].last_error.as_deref().unwrap();
    assert!(
        error.starts_with("unreadable: ") && error.contains("sale.refunded"),
        "{error}"
    );
    assert_eq!(pushes[1].delivered_at, Some(world.clock.now()));
}

/// `push.max_attempts` lowered below what a push has made already gives it
/// up, with no request.
#[tokio::test]
async fn a_push_with_no_attempts_left_is_given_up_without_a_request() {
    let world = world().await;
    let (stub, origin) = Stub::start(500).await;
    let ak = world.fixture.ak.clone();
    let device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("{origin}/up/1"), &device, None),
        )
        .await;
    let order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.paid"));
    world.tick().await;
    world.clock.advance(5);
    world.tick().await;
    assert_eq!(world.pushes(&target)[0].attempt_count, 2);
    assert_eq!(stub.count(), 2);

    world.clock.advance(20);
    let lowered = PushConfig {
        max_attempts: 2,
        ..PushConfig::default()
    };
    assert_eq!(world.tick_with(&lowered).await, 1);
    assert_eq!(stub.count(), 2);
    let push = &world.pushes(&target)[0];
    assert_eq!(push.gave_up_at, Some(world.clock.now()));
    assert_eq!(push.last_error.as_deref(), Some(NO_ATTEMPTS_LEFT));
}

/// Pushes delivered, given up on or superseded are deleted once older
/// than `push.keep_days`, each by its own time; younger ones, and ones
/// still waiting, stay.
#[tokio::test]
async fn old_pushes_are_pruned_after_push_keep_days() {
    let world = world().await;
    let (stub, origin) = Stub::start(201).await;
    let ak = world.fixture.ak.clone();
    let device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("{origin}/up/1"), &device, None),
        )
        .await;
    let mut orders = Vec::new();
    for _ in 0..4 {
        orders.push(world.order(&ak).await);
    }
    let start = world.clock.now();
    // Delivered (evt_1, evt_3) and superseded (evt_2) at `start`.
    world.queue(&world.event(1, &orders[0], "order.paid"));
    world.queue(&world.event(2, &orders[1], "order.unconfirmed"));
    world.queue(&world.event(3, &orders[1], "order.confirming"));
    assert_eq!(world.tick().await, 2);
    // Given up on an hour later (evt_4: refused as too large).
    world.clock.advance(3_600);
    stub.answer(413, None);
    world.queue(&world.event(4, &orders[2], "order.paid"));
    assert_eq!(world.tick().await, 1);
    // Still waiting (evt_5).
    world.queue(&world.event(5, &orders[3], "order.paid"));
    let pushes = world.pushes(&target);
    assert_eq!(
        pushes
            .iter()
            .map(|p| (p.delivered_at, p.superseded_at, p.gave_up_at))
            .collect::<Vec<_>>(),
        vec![
            (Some(start), None, None),
            (None, Some(start), None),
            (Some(start), None, None),
            (None, None, Some(start + 3_600)),
            (None, None, None),
        ]
    );

    let keep = PushConfig::default().keep_secs;
    async fn left_after_pruning_at(world: &World, target: &PushTargetId, at: i64) -> Vec<String> {
        super::delivery::prune_deliveries(&world.fixture.state.db, &PushConfig::default(), at)
            .await
            .unwrap();
        world
            .pushes(target)
            .into_iter()
            .map(|p| p.event_id)
            .collect()
    }
    assert_eq!(
        left_after_pruning_at(&world, &target, start + keep).await,
        ["evt_1", "evt_2", "evt_3", "evt_4", "evt_5"]
    );
    assert_eq!(
        left_after_pruning_at(&world, &target, start + keep + 1).await,
        ["evt_4", "evt_5"]
    );
    assert_eq!(
        left_after_pruning_at(&world, &target, start + 3_600 + keep + 1).await,
        ["evt_5"],
        "a push still waiting is never pruned"
    );
}

// -- Over HTTP ---------------------------------------------------------------

/// The whole way: the engine marks an app's order paid, the subscriber
/// reads the event, the worker pushes it to a UnifiedPush endpoint as
/// RFC 8291 ciphertext, `Urgency: high`, a 32-character `Topic`, and a
/// VAPID `Authorization` that verifies against this Monokulo's key for the
/// endpoint's origin; the device decrypts it to the stream's sale with
/// `api_version`. The target's last success is recorded.
#[tokio::test]
async fn a_paid_sale_reaches_a_push_service_encrypted_and_signed_over_http() {
    let world = world().await;
    let (stub, origin) = Stub::start(201).await;
    let ak = world.fixture.ak.clone();
    let device = ::push::encrypt::ReceiverKeys::generate();
    let endpoint = format!("{origin}/up/upAbcDef");
    let target = world
        .register(
            &ak,
            encrypted("unifiedpush", &endpoint, &device, Some(&["sale.paid"])),
        )
        .await;
    let order = world.order(&ak).await;
    let app = world.app_id(&ak).await;

    let state = world.fixture.state.clone();
    let subscriber = tokio::spawn(crate::webhooks::subscriber::run_subscriber(
        state.db.clone(),
        state.engine.client.clone(),
        state.webhooks.clone(),
        state.push.clone(),
        state.apps.clone(),
        crate::webhooks::Addresses::new(state.settings.clone(), state.tor.clone()),
    ));
    world.engine.mark_order_paid(order.as_str()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while world.pushes(&target).is_empty() {
            state.push.woken().await;
        }
    })
    .await
    .expect("the paid event is queued and wakes the worker");
    subscriber.abort();
    world.clock.set(crate::now_unix());
    assert_eq!(world.tick().await, 1);

    let received = stub.received.lock();
    assert_eq!(received.len(), 1);
    let push = &received[0];
    assert_eq!(push.path, "/up/upAbcDef");
    let header = |name: &str| push.headers[name].to_str().unwrap().to_owned();
    assert_eq!(header("content-encoding"), "aes128gcm");
    assert_eq!(header("urgency"), "high");
    assert_eq!(header("topic"), policy_topic(&target, &order));
    assert_eq!(header("topic").len(), 32);
    let ttl: u32 = header("ttl").parse().unwrap();
    assert!(ttl > 1_700 && ttl <= 1_800, "{ttl}");
    let authorization = ::push::vapid::Authorization::parse(&header("authorization")).unwrap();
    ::push::vapid::verify(
        &authorization,
        &state.push.vapid_public_key(),
        &::push::Origin::parse(&origin).unwrap(),
        crate::now_unix(),
    )
    .expect("signed by this Monokulo for the push service's origin");

    let plaintext = ::push::encrypt::decrypt(&device, &push.body).unwrap();
    let sale: Value = serde_json::from_slice(&plaintext).unwrap();
    assert_eq!(sale["api_version"], crate::webhooks::body::API_VERSION);
    assert_eq!(sale["event"], "sale.paid");
    assert_eq!(sale["order_id"], order.as_str());
    assert_eq!(sale["merchant_order_id"], "#1042");
    assert_eq!(
        sale["_links"]["mk:order"]["href"],
        format!("/api/v1/orders/{order}")
    );
    drop(received);

    let delivered = &world.pushes(&target)[0];
    assert_eq!(
        (delivered.delivered_at, delivered.last_status_code),
        (Some(world.clock.now()), Some(201))
    );
    let target = state
        .db
        .lock()
        .get_push_target(&app, &target)
        .unwrap()
        .unwrap();
    assert_eq!(
        (target.last_ok_at, target.failures),
        (Some(world.clock.now()), 0)
    );
}

/// A `429` with `Retry-After: 7` tries again 7 seconds on and counts
/// nothing against the target (the push service is limiting this sender);
/// a `410` then deletes the target and its pushes at once. No log line,
/// and no recorded error, holds the endpoint's URL.
#[tokio::test]
async fn a_rate_limit_waits_as_told_and_a_gone_subscription_deletes_the_target_over_http() {
    let (lines, _guard) = Lines::capture();
    let world = world().await;
    let (stub, origin) = Stub::start(429).await;
    stub.answer(429, Some("7"));
    let ak = world.fixture.ak.clone();
    let app = world.app_id(&ak).await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let secret = "upSecretCapabilityToken";
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("{origin}/up/{secret}"), &device, None),
        )
        .await;
    let order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.paid"));

    let start = world.clock.now();
    assert_eq!(world.tick().await, 1);
    let push = &world.pushes(&target)[0];
    assert_eq!(
        (
            push.attempt_count,
            push.next_attempt_at,
            push.last_status_code
        ),
        (1, Some(start + 7), Some(429))
    );
    let stored = |id: &PushTargetId| {
        world
            .fixture
            .state
            .db
            .lock()
            .get_push_target(&app, id)
            .unwrap()
    };
    assert_eq!(stored(&target).unwrap().failures, 0, "a 429 counts nothing");
    world.clock.advance(6);
    assert_eq!(world.tick().await, 0, "not before Retry-After");

    stub.answer(410, None);
    world.clock.advance(1);
    assert_eq!(world.tick().await, 1);
    assert!(
        stored(&target).is_none(),
        "a gone subscription deletes the target"
    );
    assert!(world.pushes(&target).is_empty());
    assert_eq!(stub.count(), 2);

    let logged = lines.text();
    assert!(logged.contains("push target deleted"), "{logged}");
    assert!(!logged.contains(secret), "an endpoint reached a log line");
}

/// Failing answers back off 5, 20, 80 and 320 seconds, each counting
/// against the target; the fifth attempt is the last, and the target's
/// 20th failure in a row deletes it. No log line, and no recorded error,
/// holds the endpoint's URL.
#[tokio::test]
async fn twenty_failures_in_a_row_delete_the_target_over_http() {
    let (lines, _guard) = Lines::capture();
    let world = world().await;
    let (stub, origin) = Stub::start(503).await;
    let ak = world.fixture.ak.clone();
    let app = world.app_id(&ak).await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let secret = "upAnotherSecretToken";
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("{origin}/up/{secret}"), &device, None),
        )
        .await;
    for seq in 1..=4 {
        let order = world.order(&ak).await;
        world.queue(&world.event(seq, &order, "order.paid"));
    }
    let failures = || {
        world
            .fixture
            .state
            .db
            .lock()
            .get_push_target(&app, &target)
            .unwrap()
            .map(|target| target.failures)
    };

    let mut at = world.clock.now();
    for (n, wait) in [(1u32, 5), (2, 20), (3, 80), (4, 320)] {
        assert_eq!(world.tick().await, 4);
        assert_eq!(failures(), Some(4 * n));
        for push in world.pushes(&target) {
            assert_eq!(push.attempt_count, n);
            assert_eq!(push.next_attempt_at, Some(at + wait));
            assert_eq!(push.last_status_code, Some(503));
            let error = push.last_error.unwrap();
            assert!(!error.contains(secret), "{error}");
        }
        at += wait;
        world.clock.set(at);
    }
    assert_eq!(world.tick().await, 4);
    assert_eq!(
        failures(),
        None,
        "the 20th failure in a row deletes the target"
    );
    assert!(world.pushes(&target).is_empty());
    assert_eq!(stub.count(), 20);

    let logged = lines.text();
    assert!(
        logged.contains("failed too many times in a row"),
        "{logged}"
    );
    assert!(!logged.contains(secret), "an endpoint reached a log line");
}

/// A `live-activity` target is pushed plain JSON, the Live Activity's
/// state, with the payment URI only for a target registered with
/// `include_payment_uri`.
#[tokio::test]
async fn a_live_activity_target_gets_plain_json_over_http() {
    let world = world().await;
    let (stub, origin) = Stub::start(201).await;
    let other = world.another_app().await;
    world
        .register(
            &other,
            live_activity("code", &format!("{origin}/la/code"), true),
        )
        .await;
    world
        .register(
            &other,
            live_activity("plain", &format!("{origin}/la/plain"), false),
        )
        .await;
    world.order(&world.fixture.ak.clone()).await;
    assert_eq!(world.tick().await, 2);

    let received = stub.received.lock();
    let by_path = |path: &str| -> (&HeaderMap, Value) {
        let push = received.iter().find(|p| p.path == path).unwrap();
        (&push.headers, serde_json::from_slice(&push.body).unwrap())
    };
    for path in ["/la/code", "/la/plain"] {
        let (headers, state) = by_path(path);
        assert_eq!(headers["content-type"], "application/json");
        assert!(headers.get("content-encoding").is_none());
        assert_eq!(headers["urgency"], "normal");
        assert_eq!(state["phase"], "waiting");
        assert_eq!(state["amount"], "24.50");
        assert!(state.get("order_id").is_none());
    }
    assert!(by_path("/la/code").1["paymentURI"]
        .as_str()
        .unwrap()
        .starts_with("monero:"));
    assert!(by_path("/la/plain").1.get("paymentURI").is_none());
}

// -- Edge and failure cases --------------------------------------------------

impl World {
    /// An order made with `key` under `idempotency_key`, as a till retrying
    /// after a lost answer sends it.
    async fn order_keyed(&self, key: &str, idempotency_key: &str) -> OrderId {
        let made = self
            .call(
                "POST",
                "/api/v1/orders",
                key,
                Some(json!({
                    "amount": "24.50", "currency": "XMR", "merchant_order_id": "#1042",
                    "idempotency_key": idempotency_key,
                })),
            )
            .await;
        OrderId::new(made["order_id"].as_str().unwrap())
    }

    fn target(&self, app: &AppId, target: &PushTargetId) -> Option<crate::db::PushTarget> {
        self.fixture
            .state
            .db
            .lock()
            .get_push_target(app, target)
            .unwrap()
    }

    /// `copies` more of the target's one queued push, each its own event,
    /// all due with it: a store with many sales at once.
    fn copy_push(&self, target: &PushTargetId, copies: u32) {
        let copied = self
            .fixture
            .state
            .db
            .lock()
            .conn_for_test()
            .execute(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2)
                 INSERT INTO push_deliveries
                     (push_target_id, event_seq, event_id, event_type, order_id, topic, urgency,
                      payload, expires_at_utc, created_at_utc, next_attempt_at_utc)
                 SELECT d.push_target_id, d.event_seq, d.event_id || '_' || n.i, d.event_type,
                        d.order_id, d.topic, d.urgency, d.payload, d.expires_at_utc,
                        d.created_at_utc, d.next_attempt_at_utc
                 FROM push_deliveries d, n WHERE d.push_target_id = ?1",
                rusqlite::params![target, copies],
            )
            .unwrap();
        assert_eq!(copied, copies as usize);
    }
}

/// A till retrying an order after a lost answer (the same idempotency
/// key) gets the first order back, and the store's other apps are pushed
/// its `sale.waiting` once. An order the store's own server makes pushes
/// no one.
#[tokio::test]
async fn a_retried_order_is_pushed_as_waiting_once_and_a_shops_order_not_at_all() {
    let world = world().await;
    let other = world.another_app().await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(
            &other,
            encrypted("t", "https://push.example/t", &device, None),
        )
        .await;
    let ak = world.fixture.ak.clone();
    let first = world.order_keyed(&ak, "till-1-sale-77").await;
    let again = world.order_keyed(&ak, "till-1-sale-77").await;
    assert_eq!(first, again);
    let pushes = world.pushes(&target);
    assert_eq!(pushes.len(), 1, "{pushes:?}");
    assert_eq!(pushes[0].order_id, first);

    world.order(&world.fixture.sk.clone()).await;
    assert_eq!(world.pushes(&target).len(), 1);
}

/// A newer push about an order supersedes only its own target's unsent
/// one: each target keeps the latest about the order.
#[tokio::test]
async fn a_newer_push_supersedes_only_its_own_targets_unsent_push() {
    let world = world().await;
    let ak = world.fixture.ak.clone();
    let device = ::push::encrypt::ReceiverKeys::generate();
    let a = world
        .register(&ak, encrypted("a", "https://push.example/a", &device, None))
        .await;
    let b = world
        .register(&ak, encrypted("b", "https://push.example/b", &device, None))
        .await;
    let order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.unconfirmed"));
    world.queue(&world.event(2, &order, "order.confirming"));
    for target in [&a, &b] {
        let pushes = world.pushes(target);
        let state: Vec<(&str, bool, bool)> = pushes
            .iter()
            .map(|p| {
                (
                    p.event_type.as_str(),
                    p.superseded_at.is_some(),
                    p.next_attempt_at.is_some(),
                )
            })
            .collect();
        assert_eq!(
            state,
            [("sale.seen", true, false), ("sale.confirming", false, true)],
            "{target}"
        );
    }
}

/// A target registered again as another kind of target, or no longer
/// showing the payment code, or for fewer events, isn't sent what was
/// queued for it as it was: an `encrypted` payload (the whole sale, the
/// order's id and address among it) would go out in the clear to a Live
/// Activity's relay, and a payment URI to a device that turned "show code"
/// off. Those pushes are given up on, saying why, and nothing is sent.
#[tokio::test]
async fn a_target_registered_again_for_other_pushes_is_not_sent_the_old_ones() {
    let world = world().await;
    let other = world.another_app().await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let was_encrypted = world
        .register(
            &other,
            encrypted("phone", "https://push.example/phone", &device, None),
        )
        .await;
    let showed_code = world
        .register(
            &other,
            live_activity("island", "https://relay.example/island", true),
        )
        .await;
    let wanted_waiting = world
        .register(
            &other,
            encrypted("tablet", "https://push.example/tablet", &device, None),
        )
        .await;
    world.order(&world.fixture.ak.clone()).await;
    for target in [&was_encrypted, &showed_code, &wanted_waiting] {
        assert_eq!(world.pushes(target).len(), 1, "{target}");
    }
    let showing: Value = serde_json::from_str(&world.pushes(&showed_code)[0].payload).unwrap();
    assert!(showing.get("paymentURI").is_some(), "{showing}");

    let now_live = world
        .register(
            &other,
            live_activity("phone", "https://relay.example/phone", false),
        )
        .await;
    let now_hidden = world
        .register(
            &other,
            live_activity("island", "https://relay.example/island", false),
        )
        .await;
    let now_paid_only = world
        .register(
            &other,
            encrypted(
                "tablet",
                "https://push.example/tablet",
                &device,
                Some(&["sale.paid"]),
            ),
        )
        .await;
    assert_eq!(
        [&now_live, &now_hidden, &now_paid_only],
        [&was_encrypted, &showed_code, &wanted_waiting],
        "registered again in place"
    );
    for target in [&was_encrypted, &showed_code, &wanted_waiting] {
        let push = &world.pushes(target)[0];
        assert_eq!(
            (push.next_attempt_at, push.gave_up_at.is_some()),
            (None, true),
            "{target}: {push:?}"
        );
        assert_eq!(
            push.last_error.as_deref(),
            Some(crate::db::REREGISTERED),
            "{target}"
        );
    }
    assert_eq!(world.tick().await, 0);
}

/// A subscription the app moved (a new endpoint and keys, as a
/// UnifiedPush distributor hands out) under the same `client_id` still
/// gets the push queued before, at its new endpoint, encrypted to its new
/// keys: the payload is kept plain and encrypted at each attempt.
#[tokio::test]
async fn a_moved_subscription_gets_its_waiting_push_at_the_new_endpoint_over_http() {
    let world = world().await;
    let (old_stub, old_origin) = Stub::start(201).await;
    let (new_stub, new_origin) = Stub::start(201).await;
    let ak = world.fixture.ak.clone();
    let old_device = ::push::encrypt::ReceiverKeys::generate();
    let new_device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(
            &ak,
            encrypted("up", &format!("{old_origin}/up/1"), &old_device, None),
        )
        .await;
    let order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.paid"));
    let moved = world
        .register(
            &ak,
            encrypted("up", &format!("{new_origin}/up/2"), &new_device, None),
        )
        .await;
    assert_eq!(moved, target);

    assert_eq!(world.tick().await, 1);
    assert_eq!(old_stub.count(), 0);
    let received = new_stub.received.lock();
    assert_eq!(received.len(), 1);
    assert!(::push::encrypt::decrypt(&old_device, &received[0].body).is_err());
    let sale: Value =
        serde_json::from_slice(&::push::encrypt::decrypt(&new_device, &received[0].body).unwrap())
            .unwrap();
    assert_eq!(sale["event"], "sale.paid");
}

/// Each attempt is encrypted afresh (RFC 8291: a new salt and key every
/// time; both decrypt to the same sale) and asks for what is left of the
/// TTL. A failure starts the target's run of failures; the next success
/// ends it. A newer push about the order leaves the delivered one as it
/// was.
#[tokio::test]
async fn each_attempt_is_encrypted_afresh_with_what_is_left_of_the_ttl_over_http() {
    let world = world().await;
    let (stub, origin) = Stub::start(503).await;
    let ak = world.fixture.ak.clone();
    let app = world.app_id(&ak).await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("{origin}/up/1"), &device, None),
        )
        .await;
    let order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.paid"));
    let start = world.clock.now();

    assert_eq!(world.tick().await, 1);
    assert_eq!(world.target(&app, &target).unwrap().failures, 1);
    stub.answer(201, None);
    world.clock.advance(5);
    assert_eq!(world.tick().await, 1);

    let received = stub.received.lock();
    assert_eq!(received.len(), 2);
    let ttls: Vec<&str> = received
        .iter()
        .map(|p| p.headers["ttl"].to_str().unwrap())
        .collect();
    assert_eq!(ttls, ["1800", "1795"]);
    assert_ne!(received[0].body, received[1].body);
    // The salt and the sender's key, the header's first 16 and last 65
    // bytes (RFC 8188 §2.1), both new.
    assert_ne!(received[0].body[..16], received[1].body[..16]);
    assert_ne!(received[0].body[21..86], received[1].body[21..86]);
    let plain: Vec<Vec<u8>> = received
        .iter()
        .map(|p| ::push::encrypt::decrypt(&device, &p.body).unwrap())
        .collect();
    assert_eq!(plain[0], plain[1]);
    drop(received);

    let stored = world.target(&app, &target).unwrap();
    assert_eq!((stored.failures, stored.last_ok_at), (0, Some(start + 5)));
    world.queue(&world.event(2, &order, "order.paid"));
    let pushes = world.pushes(&target);
    assert_eq!(
        (pushes[0].delivered_at, pushes[0].superseded_at),
        (Some(start + 5), None)
    );
}

/// A `404` says the subscription is gone as a `410` does: the target is
/// deleted with its pushes, the ones in flight beside it in the same batch
/// included, and the batch's pushes to other targets are sent.
#[tokio::test]
async fn a_404_deletes_the_target_and_its_pushes_in_the_same_batch_over_http() {
    let world = world().await;
    let (gone, gone_origin) = Stub::start(404).await;
    let (fine, fine_origin) = Stub::start(201).await;
    let ak = world.fixture.ak.clone();
    let app = world.app_id(&ak).await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let dead = world
        .register(
            &ak,
            encrypted("dead", &format!("{gone_origin}/up/1"), &device, None),
        )
        .await;
    let alive = world
        .register(
            &ak,
            encrypted("alive", &format!("{fine_origin}/up/2"), &device, None),
        )
        .await;
    for seq in 1..=3 {
        let order = world.order(&ak).await;
        world.queue(&world.event(seq, &order, "order.paid"));
    }

    assert_eq!(world.tick().await, 6);
    assert!(world.target(&app, &dead).is_none());
    assert!(world.pushes(&dead).is_empty());
    assert!(gone.count() >= 1);
    assert_eq!(fine.count(), 3);
    assert!(world
        .pushes(&alive)
        .iter()
        .all(|p| p.delivered_at.is_some()));
    assert_eq!(world.tick().await, 0);
}

/// A `413` gives the push up at once and counts nothing against the
/// target: the payload, not the device, was the trouble. The target's
/// next push is sent.
#[tokio::test]
async fn a_413_gives_the_push_up_without_counting_against_the_target_over_http() {
    let world = world().await;
    let (stub, origin) = Stub::start(413).await;
    let ak = world.fixture.ak.clone();
    let app = world.app_id(&ak).await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("{origin}/up/1"), &device, None),
        )
        .await;
    let first = world.order(&ak).await;
    world.queue(&world.event(1, &first, "order.paid"));
    assert_eq!(world.tick().await, 1);
    let push = &world.pushes(&target)[0];
    assert_eq!(
        (
            push.attempt_count,
            push.last_status_code,
            push.next_attempt_at,
            push.gave_up_at
        ),
        (1, Some(413), None, Some(world.clock.now()))
    );
    assert_eq!(world.target(&app, &target).unwrap().failures, 0);

    stub.answer(201, None);
    let second = world.order(&ak).await;
    world.queue(&world.event(2, &second, "order.paid"));
    assert_eq!(world.tick().await, 1);
    assert!(world.pushes(&target)[1].delivered_at.is_some());
}

/// A wait the push's TTL wouldn't outlast gives it up at the attempt,
/// rather than sending it stale: a `503` asking for an hour, and a `429`
/// asking for exactly what is left (the retry would fall at the TTL's
/// end). The `503` counts against the target; the `429` doesn't.
#[tokio::test]
async fn a_wait_past_the_ttl_gives_the_push_up_at_once_over_http() {
    let world = world().await;
    let (stub, origin) = Stub::start(503).await;
    stub.answer(503, Some("3600"));
    let ak = world.fixture.ak.clone();
    let app = world.app_id(&ak).await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("{origin}/up/1"), &device, None),
        )
        .await;
    let first = world.order(&ak).await;
    world.queue(&world.event(1, &first, "order.paid"));
    assert_eq!(world.tick().await, 1);

    stub.answer(429, Some("1800"));
    let second = world.order(&ak).await;
    world.queue(&world.event(2, &second, "order.paid"));
    assert_eq!(world.tick().await, 1);

    let now = world.clock.now();
    let pushes = world.pushes(&target);
    let outcomes: Vec<_> = pushes
        .iter()
        .map(|p| {
            (
                p.attempt_count,
                p.last_status_code,
                p.next_attempt_at,
                p.gave_up_at,
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            (1, Some(503), None, Some(now)),
            (1, Some(429), None, Some(now))
        ]
    );
    assert_eq!(world.target(&app, &target).unwrap().failures, 1);
}

/// Any other refusal (`400`, or `401` for a token the push service won't
/// take) is tried again on the schedule and counts against the target.
#[tokio::test]
async fn a_refused_push_is_tried_again_on_the_schedule_over_http() {
    let world = world().await;
    let (stub, origin) = Stub::start(400).await;
    let ak = world.fixture.ak.clone();
    let app = world.app_id(&ak).await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("{origin}/up/1"), &device, None),
        )
        .await;
    let order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.paid"));
    let start = world.clock.now();
    assert_eq!(world.tick().await, 1);
    stub.answer(401, None);
    world.clock.advance(5);
    assert_eq!(world.tick().await, 1);

    let push = &world.pushes(&target)[0];
    assert_eq!(
        (
            push.attempt_count,
            push.last_status_code,
            push.next_attempt_at
        ),
        (2, Some(401), Some(start + 5 + 20))
    );
    assert_eq!(world.target(&app, &target).unwrap().failures, 2);
}

/// With private addresses off (`webhooks.allow_private_urls`, as in
/// production), an endpoint on one is refused before any request, and the
/// refusal counts like any failure of the target's. The refusal names no
/// URL.
#[tokio::test]
async fn an_endpoint_on_a_private_address_is_refused_without_a_request() {
    let world = world().await;
    let ak = world.fixture.ak.clone();
    let app = world.app_id(&ak).await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let secret = "upPrivateCapability";
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("https://10.0.0.1/up/{secret}"), &device, None),
        )
        .await;
    let order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.paid"));
    let start = world.clock.now();
    let sent = run_delivery_tick(
        &world.fixture.state.db,
        &world.client,
        &PushConfig::default(),
        Reach::Public,
        &world.clock,
    )
    .await
    .unwrap();
    assert_eq!(sent, 1);
    let push = &world.pushes(&target)[0];
    let error = push.last_error.as_deref().unwrap();
    assert!(error.contains("private or loopback"), "{error}");
    assert!(!error.contains(secret), "{error}");
    assert_eq!(
        (
            push.attempt_count,
            push.last_status_code,
            push.next_attempt_at
        ),
        (1, None, Some(start + 5))
    );
    assert_eq!(world.target(&app, &target).unwrap().failures, 1);
}

/// A push service that can't be connected to is tried again on the
/// schedule; the recorded error and the log lines say why without the
/// endpoint's URL.
#[tokio::test]
async fn a_push_service_that_cant_be_reached_is_retried_without_its_url_over_http() {
    let (lines, _guard) = Lines::capture();
    let world = world().await;
    // A port nothing listens on: bound, then let go.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let ak = world.fixture.ak.clone();
    let device = ::push::encrypt::ReceiverKeys::generate();
    let secret = "upUnreachableCapability";
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("http://{closed}/up/{secret}"), &device, None),
        )
        .await;
    let order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.paid"));
    let start = world.clock.now();
    assert_eq!(world.tick().await, 1);

    let push = &world.pushes(&target)[0];
    let error = push.last_error.as_deref().unwrap();
    assert!(error.starts_with("could not connect"), "{error}");
    assert!(!error.contains(secret), "{error}");
    assert_eq!(push.next_attempt_at, Some(start + 5));
    let logged = lines.text();
    assert!(logged.contains("push failed; will retry"), "{logged}");
    assert!(!logged.contains(secret), "an endpoint reached a log line");
}

/// A push service that takes a push and never answers is given up on at
/// `push.delivery_timeout_ms` (here its least, 100 ms) and tried again on
/// the schedule, counted against the target.
#[tokio::test]
async fn a_push_service_that_never_answers_times_out_over_http() {
    let world = world().await;
    let (stub, origin) = Stub::start(201).await;
    stub.hold(std::time::Duration::from_secs(30));
    let ak = world.fixture.ak.clone();
    let app = world.app_id(&ak).await;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("{origin}/up/1"), &device, None),
        )
        .await;
    let order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.paid"));
    let start = world.clock.now();
    let config = PushConfig {
        delivery_timeout: std::time::Duration::from_millis(100),
        ..PushConfig::default()
    };
    let began = std::time::Instant::now();
    assert_eq!(world.tick_with(&config).await, 1);
    assert!(began.elapsed() < std::time::Duration::from_secs(5));

    let push = &world.pushes(&target)[0];
    let error = push.last_error.as_deref().unwrap();
    assert!(error.starts_with("timed out"), "{error}");
    assert_eq!(
        (push.attempt_count, push.next_attempt_at, push.delivered_at),
        (1, Some(start + 5), None)
    );
    assert_eq!(world.target(&app, &target).unwrap().failures, 1);
    assert_eq!(stub.count(), 1);
}

/// A batch is 50 pushes, 16 of them in flight at once, so a slow push
/// service holds a batch up for a few of its answers, not all of them;
/// the rest go in the next batch.
#[tokio::test]
async fn a_batch_sends_sixteen_pushes_at_once_and_fifty_in_all_over_http() {
    let world = world().await;
    let (stub, origin) = Stub::start(201).await;
    let hold = std::time::Duration::from_millis(100);
    stub.hold(hold);
    let ak = world.fixture.ak.clone();
    let device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("{origin}/up/1"), &device, None),
        )
        .await;
    let order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.paid"));
    world.copy_push(&target, 59);

    let began = std::time::Instant::now();
    assert_eq!(world.tick().await, super::delivery::DELIVERY_BATCH as usize);
    let took = began.elapsed();
    assert_eq!(stub.most_in_flight.load(Ordering::SeqCst), 16);
    // One at a time would take 50 holds; 16 at a time, four. Half of 50
    // leaves room for a loaded machine.
    assert!(took < hold * 25, "{took:?}");
    assert_eq!(world.tick().await, 10);
    assert_eq!(stub.count(), 60);
    assert!(world
        .pushes(&target)
        .iter()
        .all(|p| p.delivered_at.is_some()));
}

/// Each push service gets a token for its own origin, carrying this
/// Monokulo's web address as `sub`, signed with its key: it verifies
/// there, and nowhere else (another origin, another key, after it
/// expires); a token another key signed doesn't pass for this one's.
#[tokio::test]
async fn each_push_service_gets_a_token_for_its_own_origin_only_over_http() {
    let world = world().await;
    let (one, one_origin) = Stub::start(201).await;
    let (two, two_origin) = Stub::start(201).await;
    let contact = super::delivery::contact(&world.fixture.state.settings);
    assert!(contact.is_some(), "the fixture has an https web address");
    let client = PushClient::build(world.fixture.state.push.vapid(), contact.clone()).unwrap();
    let ak = world.fixture.ak.clone();
    let device = ::push::encrypt::ReceiverKeys::generate();
    for (name, origin) in [("one", &one_origin), ("two", &two_origin)] {
        world
            .register(
                &ak,
                encrypted(name, &format!("{origin}/up/{name}"), &device, None),
            )
            .await;
    }
    let order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.paid"));
    let sent = run_delivery_tick(
        &world.fixture.state.db,
        &client,
        &PushConfig::default(),
        Reach::AnyAddress,
        &world.clock,
    )
    .await
    .unwrap();
    assert_eq!(sent, 2);

    let key = world.fixture.state.push.vapid_public_key();
    let origin = |text: &str| ::push::Origin::parse(text).unwrap();
    let now = world.clock.now();
    let header = |stub: &Stub| {
        let received = stub.received.lock();
        assert_eq!(received.len(), 1);
        received[0].headers["authorization"]
            .to_str()
            .unwrap()
            .to_owned()
    };
    let (one_header, two_header) = (header(&one), header(&two));
    assert_ne!(one_header, two_header);
    for (header, own, other) in [
        (&one_header, &one_origin, &two_origin),
        (&two_header, &two_origin, &one_origin),
    ] {
        let authorization = ::push::vapid::Authorization::parse(header).unwrap();
        let claims = ::push::vapid::verify(&authorization, &key, &origin(own), now).unwrap();
        assert_eq!(claims.sub, contact);
        assert!(matches!(
            ::push::vapid::verify(&authorization, &key, &origin(other), now),
            Err(::push::vapid::VapidError::Audience)
        ));
        let stranger = ::push::VapidKey::generate().public();
        assert!(matches!(
            ::push::vapid::verify(&authorization, &stranger, &origin(own), now),
            Err(::push::vapid::VapidError::KeyMismatch)
        ));
        let expiry = i64::try_from(::push::vapid::EXPIRY.as_secs()).unwrap();
        assert!(matches!(
            ::push::vapid::verify(&authorization, &key, &origin(own), now + expiry),
            Err(::push::vapid::VapidError::Expired)
        ));
    }

    // Another key's token, passed off under this Monokulo's key.
    let forger = ::push::VapidKey::generate();
    let forged = forger
        .authorization(&origin(&one_origin), None, now)
        .unwrap();
    let forged = forged
        .header_value()
        .to_str()
        .unwrap()
        .replace(&forger.public().to_string(), &key.to_string());
    let forged = ::push::vapid::Authorization::parse(&forged).unwrap();
    assert!(matches!(
        ::push::vapid::verify(&forged, &key, &origin(&one_origin), now),
        Err(::push::vapid::VapidError::Jwt(_))
    ));
}

/// A push superseded while its attempt is in flight keeps that attempt
/// (it was made) but, failing, isn't tried again: the newer one is sent
/// instead, alone.
#[tokio::test]
async fn a_push_superseded_while_it_is_sent_is_not_tried_again_over_http() {
    let world = world().await;
    let (stub, origin) = Stub::start(503).await;
    stub.hold(std::time::Duration::from_millis(300));
    let ak = world.fixture.ak.clone();
    let device = ::push::encrypt::ReceiverKeys::generate();
    let target = world
        .register(
            &ak,
            encrypted("t", &format!("{origin}/up/1"), &device, None),
        )
        .await;
    let order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.unconfirmed"));

    let (sent, ()) = tokio::join!(world.tick(), async {
        while stub.in_flight.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        world.queue(&world.event(2, &order, "order.confirming"));
    });
    assert_eq!(sent, 1);
    let pushes = world.pushes(&target);
    let old = &pushes[0];
    assert_eq!(
        (
            old.attempt_count,
            old.last_status_code,
            old.next_attempt_at,
            old.gave_up_at,
            old.superseded_at.is_some()
        ),
        (1, Some(503), None, None, true)
    );
    assert_eq!(pushes[1].next_attempt_at, Some(world.clock.now()));

    stub.hold(std::time::Duration::ZERO);
    stub.answer(201, None);
    assert_eq!(world.tick().await, 1);
    assert_eq!(stub.count(), 2);
    let plain = ::push::encrypt::decrypt(&device, &stub.received.lock()[1].body).unwrap();
    let sale: Value = serde_json::from_slice(&plain).unwrap();
    assert_eq!(sale["event"], "sale.confirming");
}

// Through the push relay, to stand-ins for APNs and FCM: every test there
// goes over real HTTP.
#[cfg(feature = "relay")]
mod relay_tests;
