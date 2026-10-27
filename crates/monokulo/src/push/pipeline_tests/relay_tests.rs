//! Pushes through the push relay, the whole chain at once: a relay (crate
//! `push-relay`, as monokulo embeds it) served on this machine and pointed
//! at stand-ins for APNs and FCM; a sale of this Monokulo's pushed by its
//! worker to targets whose endpoints are the relay's handles; and what
//! reaches APNs or FCM read as the device would read it. Every test here
//! goes over real HTTP, as the relay is reached: a push is the one thing
//! that has to land.
//!
//! The relay's own tests (`crates/push-relay/tests`) cover its routes and
//! translation in detail; these check that the two ends agree: the target
//! an app registers is a relay handle, the push is signed for the relay's
//! origin with this Monokulo's key, the ciphertext decrypts on the device
//! and the Live Activity's state is what iOS applies.

use std::collections::HashMap;

use axum::http::HeaderMap;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use live_settings::Env;
use push_relay::settings::{self, Host};
use push_relay::Relay;
use serde_json::{json, Value};

use super::*;

/// The app every handle is registered for, and what the relay knows of it.
const APP: &str = "monokular";
const BUNDLE: &str = "com.example.monokular";
const PROJECT: &str = "monokular-test";

/// A throwaway P-256 key in the form of Apple's `.p8` and a 2048-bit RSA
/// key for a service account, made for the relay's tests and trusted by
/// nothing (`crates/push-relay/tests/support/mod.rs` says how).
const APNS_KEY: &str = include_str!("../../../../push-relay/tests/fixtures/apns-test.p8");
const FCM_PRIVATE_KEY: &str =
    include_str!("../../../../push-relay/tests/fixtures/fcm-test-key.pem");

/// An APNs device token, as an app sends it.
fn apns_token() -> String {
    "0f".repeat(32)
}

const FCM_TOKEN: &str = "cX9:APA91b-device_token";

/// A request a stand-in provider received.
struct ProviderRequest {
    path: String,
    headers: HeaderMap,
    body: Value,
}

impl ProviderRequest {
    fn header(&self, name: &str) -> &str {
        self.headers[name].to_str().unwrap()
    }
}

/// A stand-in for APNs or FCM on this machine (HTTP/2 with prior
/// knowledge, as the relay reaches an `http` APNs, or HTTP/1.1), taking
/// every push with `200` and handing out an access token at `/token`.
#[derive(Clone, Default)]
struct Provider {
    received: Arc<Mutex<Vec<ProviderRequest>>>,
}

impl Provider {
    /// Starts one: it and its origin (`http://127.0.0.1:…`).
    async fn start() -> (Provider, String) {
        async fn take(
            axum::extract::State(provider): axum::extract::State<Provider>,
            uri: axum::http::Uri,
            headers: HeaderMap,
            body: axum::body::Bytes,
        ) -> axum::Json<Value> {
            if uri.path() == "/token" {
                return axum::Json(json!({
                    "access_token": "ya29.test-access-token",
                    "expires_in": 3600,
                    "token_type": "Bearer",
                }));
            }
            provider.received.lock().push(ProviderRequest {
                path: uri.path().to_owned(),
                headers,
                body: serde_json::from_slice(&body).unwrap(),
            });
            axum::Json(json!({}))
        }
        let provider = Provider::default();
        let app = axum::Router::new()
            .fallback(take)
            .with_state(provider.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (provider, format!("http://{addr}"))
    }

    /// The one push it received.
    fn only(&self) -> ProviderRequest {
        let mut received = self.received.lock();
        assert_eq!(received.len(), 1, "one push reaches the provider");
        received.pop().unwrap()
    }
}

/// A relay on this machine, built from its settings as monokulo builds the
/// embedded one, serving until dropped.
struct ServedRelay {
    origin: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl ServedRelay {
    async fn start(apns: &str, fcm: &str) -> ServedRelay {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let store_path = std::env::temp_dir()
            .join(format!(
                "monokulo-relay-test-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ))
            .join("push-relay.db");
        let apps = json!({APP: {"apns_topic": BUNDLE, "fcm_project_id": PROJECT}}).to_string();
        let stored: HashMap<String, String> = [
            ("server.public_uri", origin.as_str()),
            ("store.path", store_path.to_str().unwrap()),
            ("handles.apps", apps.as_str()),
            ("apns.endpoint", apns),
            ("apns.team_id", "TEAM123456"),
            ("apns.key_id", "KEY1234567"),
            ("fcm.endpoint", fcm),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        let account = json!({
            "type": "service_account",
            "project_id": PROJECT,
            "private_key_id": "0123456789abcdef0123456789abcdef01234567",
            "private_key": FCM_PRIVATE_KEY,
            "client_email": "relay@monokular-test.iam.gserviceaccount.com",
            "client_id": "100000000000000000000",
            "token_uri": format!("{fcm}/token"),
        });
        let env = Env::fixed([
            ("MONOKULO_RELAY_HANDLE_KEY", "0123456789abcdef".repeat(4)),
            ("MONOKULO_RELAY_APNS_KEY", APNS_KEY.to_owned()),
            ("MONOKULO_RELAY_FCM_CREDENTIALS", account.to_string()),
        ]);
        let config = settings::load(stored, env, Host::Embedded).await.unwrap();
        let relay = Relay::start(&config).await.unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            relay
                .serve(listener, async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        ServedRelay {
            origin,
            stop: Some(stop),
        }
    }

    /// Registers `token` on `platform` for the sender with `vapid_public_key`,
    /// as an app does: the endpoint it then hands that sender.
    async fn register(&self, platform: &str, token: &str, vapid_public_key: &str) -> String {
        let answer = reqwest::Client::new()
            .post(format!("{}/v1/register", self.origin))
            .json(&json!({
                "platform": platform,
                "token": token,
                "app": APP,
                "vapid_public_key": vapid_public_key,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(answer.status(), StatusCode::CREATED);
        let registered: Value = answer.json().await.unwrap();
        let endpoint = registered["endpoint"].as_str().unwrap().to_owned();
        assert!(
            endpoint.starts_with(&format!("{}/v1/push/ph_", self.origin)),
            "{endpoint}"
        );
        endpoint
    }
}

impl Drop for ServedRelay {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Everything the tests here start: a world, a served relay and the two
/// stand-in providers behind it.
struct Chain {
    world: World,
    relay: ServedRelay,
    apns: Provider,
    fcm: Provider,
}

async fn chain() -> Chain {
    let world = world().await;
    let (apns, apns_origin) = Provider::start().await;
    let (fcm, fcm_origin) = Provider::start().await;
    let relay = ServedRelay::start(&apns_origin, &fcm_origin).await;
    Chain {
        world,
        relay,
        apns,
        fcm,
    }
}

impl Chain {
    /// This Monokulo's VAPID public key, as `GET /api/v1/push-targets`
    /// hands it to an app.
    fn vapid_public_key(&self) -> String {
        self.world
            .fixture
            .state
            .push
            .vapid_public_key()
            .to_base64url()
    }
}

/// Decrypts the ciphertext a provider's JSON carries as `aes128gcm`.
fn decrypted(device: &::push::encrypt::ReceiverKeys, carried: &Value) -> Value {
    let ciphertext = URL_SAFE_NO_PAD.decode(carried.as_str().unwrap()).unwrap();
    serde_json::from_slice(&::push::encrypt::decrypt(device, &ciphertext).unwrap()).unwrap()
}

/// An iPhone's notification: the app registers its APNs token with the
/// relay, hands the relay's endpoint to Monokulo as an `encrypted` target,
/// and the paid sale, queued by the subscriber from the engine's event and
/// sent by the worker, reaches APNs as an alert carrying ciphertext only
/// the device decrypts, collapsed by the sale's topic.
#[tokio::test]
async fn a_paid_sale_reaches_apns_through_the_relay_and_decrypts_on_the_device_over_http() {
    let chain = chain().await;
    let world = &chain.world;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let endpoint = chain
        .relay
        .register("apns", &apns_token(), &chain.vapid_public_key())
        .await;
    let ak = world.fixture.ak.clone();
    let target = world
        .register(
            &ak,
            encrypted("iphone", &endpoint, &device, Some(&["sale.paid"])),
        )
        .await;
    let order = world.order(&ak).await;

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

    let received = chain.apns.only();
    assert_eq!(received.path, format!("/3/device/{}", apns_token()));
    assert_eq!(received.header("apns-push-type"), "alert");
    assert_eq!(received.header("apns-topic"), BUNDLE);
    assert_eq!(received.header("apns-priority"), "10");
    assert_eq!(received.header("apns-collapse-id"), policy_topic(&order));
    let expiration: i64 = received.header("apns-expiration").parse().unwrap();
    let now = crate::now_unix();
    assert!(
        (now + 1_700..=now + 1_800).contains(&expiration),
        "{expiration}"
    );
    assert_eq!(
        received.body["aps"],
        json!({"alert": {"title": "New sale"}, "mutable-content": 1})
    );
    let sale = decrypted(&device, &received.body["aes128gcm"]);
    assert_eq!(sale["event"], "sale.paid");
    assert_eq!(sale["order_id"], order.as_str());
    assert_eq!(sale["merchant_order_id"], "#1042");
    assert_eq!(sale["api_version"], crate::webhooks::body::API_VERSION);
    assert!(chain.fcm.received.lock().is_empty());

    let delivered = &world.pushes(&target)[0];
    assert_eq!(
        (delivered.delivered_at, delivered.last_status_code),
        (Some(world.clock.now()), Some(201))
    );
}

/// An Android phone with Google services: the same chain through the
/// relay's FCM side, as a data message with the sale's priority, whose
/// `aes128gcm` the app decrypts; the relay's one access-token exchange
/// goes to the service account's `token_uri`.
#[tokio::test]
async fn a_paid_sale_reaches_fcm_through_the_relay_as_a_data_message_over_http() {
    let chain = chain().await;
    let world = &chain.world;
    let device = ::push::encrypt::ReceiverKeys::generate();
    let endpoint = chain
        .relay
        .register("fcm", FCM_TOKEN, &chain.vapid_public_key())
        .await;
    let ak = world.fixture.ak.clone();
    let target = world
        .register(
            &ak,
            encrypted("android", &endpoint, &device, Some(&["sale.paid"])),
        )
        .await;
    let order = world.order(&ak).await;
    world.queue(&world.event(1, &order, "order.paid"));
    assert_eq!(world.tick().await, 1);

    let received = chain.fcm.only();
    assert_eq!(
        received.path,
        format!("/v1/projects/{PROJECT}/messages:send")
    );
    assert_eq!(
        received.header("authorization"),
        "Bearer ya29.test-access-token"
    );
    let message = &received.body["message"];
    assert_eq!(message["token"], FCM_TOKEN);
    assert_eq!(message["android"]["priority"], "HIGH");
    assert_eq!(message["android"]["collapse_key"], policy_topic(&order));
    let sale = decrypted(&device, &message["data"]["aes128gcm"]);
    assert_eq!(sale["event"], "sale.paid");
    assert_eq!(sale["order_id"], order.as_str());
    assert!(chain.apns.received.lock().is_empty());
    assert_eq!(world.pushes(&target)[0].last_status_code, Some(201));
}

/// An iPhone's Live Activity: its push token registered on the relay's
/// `apns-liveactivity` platform and handed to Monokulo as a `live-activity`
/// target showing the code. Another app's new sale reaches APNs as an
/// update whose `content-state` is the sale's state, the payment URI in it
/// as asked and the order's id nowhere; the sale paid, the update carries
/// the alert, no URI, and the activity is never ended by the relay.
#[tokio::test]
async fn a_live_activity_is_updated_through_the_relay_as_the_sale_moves_over_http() {
    let chain = chain().await;
    let world = &chain.world;
    let endpoint = chain
        .relay
        .register(
            "apns-liveactivity",
            &"ab".repeat(32),
            &chain.vapid_public_key(),
        )
        .await;
    let other = world.another_app().await;
    world
        .register(&other, live_activity("island", &endpoint, true))
        .await;
    let order = world.order(&world.fixture.ak.clone()).await;
    assert_eq!(world.tick().await, 1);

    let waiting = chain.apns.only();
    assert_eq!(waiting.header("apns-push-type"), "liveactivity");
    assert_eq!(
        waiting.header("apns-topic"),
        format!("{BUNDLE}.push-type.liveactivity")
    );
    assert_eq!(waiting.header("apns-priority"), "5");
    let aps = &waiting.body["aps"];
    assert_eq!(aps["event"], "update");
    assert!(aps.get("alert").is_none(), "{aps}");
    let state = &aps["content-state"];
    assert_eq!(state["phase"], "waiting");
    assert_eq!(state["amount"], "24.50");
    assert_eq!(state["currency"], "XMR");
    assert_eq!(state["sale"], "#1042");
    assert_eq!(state["waitingCount"], 1);
    assert!(state["paymentURI"].as_str().unwrap().starts_with("monero:"));
    assert!(state.get("order_id").is_none());
    assert_eq!(aps["stale-date"], state["expiresAt"]);

    world.queue(&world.event(1, &order, "order.paid"));
    assert_eq!(world.tick().await, 1);
    let paid = chain.apns.only();
    assert_eq!(paid.header("apns-priority"), "10");
    let aps = &paid.body["aps"];
    assert_eq!(aps["event"], "update");
    assert_eq!(aps["alert"], json!({"title": "Paid · 24.50 XMR"}));
    assert_eq!(aps["content-state"]["phase"], "paid");
    assert_eq!(aps["content-state"]["waitingCount"], 0);
    assert!(aps["content-state"].get("paymentURI").is_none());
}
