//! What the Bell's tests share: a Bell built from settings as the process
//! builds it, called in-process; stand-ins for Square and for a push
//! service on loopback; webhooks signed as Square signs them; tills with
//! their keys; and a capture of the log.

#![allow(dead_code, reason = "each test file uses its own part of this")]

use std::cell::RefCell;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, PoisonError};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit as _, Mac as _};
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use monokular_bell::Bell;
use monokular_bell::settings;
use push::encrypt::ReceiverKeys;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tower::ServiceExt as _;

/// Where the Bell says it is, in every test.
pub const PUBLIC_URI: &str = "https://bell.example.org";
/// What Square signs its webhooks for, at [`PUBLIC_URI`].
pub const WEBHOOK_URL: &str = "https://bell.example.org/v1/square/webhook";
pub const APPLICATION_ID: &str = "sq0idp-MonokularTest";
/// The webhook subscription's signature key (Square's documented one).
pub const WEBHOOK_KEY: &str = "Ibxx_5AKakO-3qeNVR61Dw";
/// The Bell's VAPID private key, as the environment gives it.
pub const VAPID_HEX: &str = "4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d";

/// The merchants the stand-in Square knows: an access token each, and
/// their locations.
pub const MERCHANT: &str = "MLEFBHHSJGVHD";
pub const TOKEN: &str = "EAAAl-merchant-one-access-token-0123456789";
pub const LOCATION: &str = "L8GWD5R9QB376";
pub const OTHER_LOCATION: &str = "LQH2Z0WDD3MZ8";
pub const OTHER_MERCHANT: &str = "6SSW7HV8K2ST5";
pub const OTHER_TOKEN: &str = "EAAAl-merchant-two-access-token-9876543210";
pub const OTHER_MERCHANTS_LOCATION: &str = "S8GWD5R9QB376";

/// A request a stand-in received.
#[derive(Debug)]
pub struct Received {
    pub method: Method,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Received {
    pub fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .map(|value| value.to_str().unwrap())
            .unwrap_or_else(|| panic!("no {name} header: {:?}", self.headers))
    }
}

/// What a stand-in answers.
#[derive(Clone)]
pub struct Answer {
    pub status: StatusCode,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl Answer {
    pub fn status(code: u16) -> Self {
        Answer {
            status: StatusCode::from_u16(code).unwrap(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn json(mut self, body: Value) -> Self {
        self.body = body.to_string().into_bytes();
        self.headers
            .push(("content-type", "application/json".to_owned()));
        self
    }

    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_owned()));
        self
    }
}

type Answers = Arc<Mutex<Box<dyn FnMut(&Received) -> Answer + Send>>>;

/// A stand-in on loopback, over HTTP/1.1, answering each request as
/// `answer` says and handing over what it received.
pub struct Stub {
    pub address: SocketAddr,
    received: tokio::sync::Mutex<mpsc::UnboundedReceiver<Received>>,
    answers: Answers,
}

impl Stub {
    pub async fn start(answer: impl FnMut(&Received) -> Answer + Send + 'static) -> Arc<Stub> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sent, received) = mpsc::unbounded_channel();
        let answers: Answers = Arc::new(Mutex::new(Box::new(answer)));
        let serving = answers.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (sent, answers) = (sent.clone(), serving.clone());
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                        let (sent, answers) = (sent.clone(), answers.clone());
                        async move {
                            let (parts, body) = request.into_parts();
                            let body = body.collect().await.unwrap().to_bytes().to_vec();
                            let received = Received {
                                method: parts.method,
                                path: parts.uri.path().to_owned(),
                                headers: parts.headers,
                                body,
                            };
                            let answer =
                                (answers.lock().unwrap_or_else(PoisonError::into_inner))(&received);
                            let _ = sent.send(received);
                            let mut response = hyper::Response::new(Full::new(
                                axum::body::Bytes::from(answer.body),
                            ));
                            *response.status_mut() = answer.status;
                            for (name, value) in answer.headers {
                                response.headers_mut().insert(
                                    HeaderName::from_static(name),
                                    HeaderValue::from_str(&value).unwrap(),
                                );
                            }
                            Ok::<_, Infallible>(response)
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Arc::new(Stub {
            address,
            received: tokio::sync::Mutex::new(received),
            answers,
        })
    }

    /// A push service taking every push (`201`, as RFC 8030 has it).
    pub async fn push_service() -> Arc<Stub> {
        Stub::start(|_| Answer::status(201)).await
    }

    /// Square, knowing two merchants by their tokens: [`MERCHANT`] with
    /// [`LOCATION`] and [`OTHER_LOCATION`], [`OTHER_MERCHANT`] with
    /// [`OTHER_MERCHANTS_LOCATION`], the first of each its main one (asked
    /// for as `main`). Any other token is refused (`401`), as is a
    /// location of another merchant (`404`), as Square does.
    pub async fn square() -> Arc<Stub> {
        Stub::start(|received| {
            let merchants: HashMap<&str, (&str, &[&str])> = HashMap::from([
                (TOKEN, (MERCHANT, &[LOCATION, OTHER_LOCATION][..])),
                (
                    OTHER_TOKEN,
                    (OTHER_MERCHANT, &[OTHER_MERCHANTS_LOCATION][..]),
                ),
            ]);
            let errors = |code: u16, category: &str, detail: &str| {
                Answer::status(code).json(
                    json!({"errors": [{"category": category, "code": category, "detail": detail}]}),
                )
            };
            let token = received
                .headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "));
            let Some((merchant, locations)) = token.and_then(|token| merchants.get(token)) else {
                return errors(
                    401,
                    "AUTHENTICATION_ERROR",
                    "This request could not be authorized.",
                );
            };
            let Some(location) = received.path.strip_prefix("/v2/locations/") else {
                return errors(404, "NOT_FOUND", "No such route.");
            };
            // Square's alias for the merchant's main location, answered
            // with that location's own ID.
            let location = if location == "main" {
                locations[0]
            } else {
                location
            };
            if !locations.contains(&location) {
                return errors(404, "NOT_FOUND", "Location not found.");
            }
            Answer::status(200).json(json!({"location": {
                "id": location,
                "name": "Front Counter",
                "merchant_id": merchant,
                "status": "ACTIVE",
                "currency": "USD",
            }}))
        })
        .await
    }

    /// Answers from now on as `answer` says.
    pub fn answer(&self, answer: impl FnMut(&Received) -> Answer + Send + 'static) {
        *self.answers.lock().unwrap_or_else(PoisonError::into_inner) = Box::new(answer);
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// The next request it received.
    pub async fn next(&self) -> Received {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            self.received.lock().await.recv().await.unwrap()
        })
        .await
        .expect("a request reached the stand-in")
    }

    /// Everything it received so far.
    pub async fn drain(&self) -> Vec<Received> {
        let mut received = self.received.lock().await;
        let mut all = Vec::new();
        while let Ok(request) = received.try_recv() {
            all.push(request);
        }
        all
    }
}

/// An `http` address on loopback that nothing listens on.
pub fn unreachable_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}

/// An `http` address on loopback that takes connections and never answers
/// (the kernel accepts them into the listener's backlog): a Square or a
/// push service slower than the Bell waits. Silent while the listener
/// lives.
pub fn silent_url() -> (std::net::TcpListener, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    (listener, url)
}

/// Seconds since the Unix epoch, as the Bell's clock reads them.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// The public key of [`VAPID_HEX`]: the key every ding is signed with.
pub fn vapid_public_key() -> push::VapidPublicKey {
    let mut private = [0u8; 32];
    hex::decode_to_slice(VAPID_HEX, &mut private).unwrap();
    push::VapidKey::from_bytes(&private).unwrap().public()
}

/// A completed `payment.created` at [`MERCHANT`]'s [`LOCATION`]: the
/// webhook that dings.
pub fn completed_payment() -> String {
    payment_webhook("payment.created", "COMPLETED", MERCHANT, LOCATION)
}

/// A Bell built as the process builds it, from options file `values` over
/// these: at [`PUBLIC_URI`], for [`APPLICATION_ID`], Square at
/// `square_url`, private push endpoints allowed (the stand-ins are on
/// loopback), its database in a directory of its own.
pub struct TestBell {
    pub bell: Bell,
    pub router: axum::Router,
    pub store_path: PathBuf,
}

impl TestBell {
    pub async fn start(square_url: &str, values: &[(&str, &str)]) -> TestBell {
        let dir = std::env::temp_dir().join(format!(
            "monokular-bell-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let store_path = dir.join("bell.db");
        let mut stored: HashMap<String, String> = [
            ("bell.public_uri", PUBLIC_URI),
            ("bell.store_path", store_path.to_str().unwrap()),
            ("bell.square.application_id", APPLICATION_ID),
            ("bell.square.api_url", square_url),
            ("bell.allow_private_endpoints", "true"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        for (key, value) in values {
            stored.insert((*key).to_owned(), (*value).to_owned());
        }
        // A store of another Bell's, when `values` names one.
        let store_path = PathBuf::from(&stored["bell.store_path"]);
        let env = live_settings::Env::fixed([
            ("MONOKULAR_BELL_SQUARE_WEBHOOK_KEY", WEBHOOK_KEY),
            ("MONOKULAR_BELL_VAPID_KEY", VAPID_HEX),
        ]);
        let config = settings::load(stored, env).await.unwrap();
        let bell = Bell::start(&config).await.unwrap();
        TestBell {
            router: bell.router(),
            bell,
            store_path,
        }
    }

    /// A Bell whose Square can't be reached: for the tests that never ask.
    pub async fn without_square(values: &[(&str, &str)]) -> TestBell {
        TestBell::start(&unreachable_url(), values).await
    }

    /// `request` from `peer`, as a connection from there brings it.
    pub async fn call_from(&self, peer: &str, mut request: Request<Body>) -> Answered {
        let peer: SocketAddr = peer.parse().unwrap();
        request.extensions_mut().insert(ConnectInfo(peer));
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Answered {
            status,
            headers,
            body: body.to_vec(),
        }
    }

    pub async fn call(&self, request: Request<Body>) -> Answered {
        self.call_from("192.0.2.1:50000", request).await
    }

    /// Registers `till` for `location` with `token`, and hands back the
    /// answer.
    pub async fn register(&self, token: &str, location: &str, till: &Till) -> Answered {
        self.call(registration(token, location, &till.subscription()))
            .await
    }

    /// Registers `till`, which must be accepted, and hands back its id.
    pub async fn registered(&self, token: &str, location: &str, till: &Till) -> String {
        let answered = self.register(token, location, till).await;
        assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.text());
        answered.json()["registration_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// `body` as Square sends it, signed for [`WEBHOOK_URL`]; then waits
    /// for the dings it started.
    pub async fn webhook(&self, body: &str) -> Answered {
        let answered = self
            .call(webhook_request(body, Some(&sign(WEBHOOK_URL, body))))
            .await;
        self.bell.dings_sent().await;
        answered
    }

    /// The database's rows of `registrations`, as `(id, merchant_id,
    /// location_id, last_ok_at, failures)`.
    pub fn rows(&self) -> Vec<(String, String, String, Option<i64>, i64)> {
        let connection = rusqlite::Connection::open(&self.store_path).unwrap();
        let mut statement = connection
            .prepare(
                "SELECT id, merchant_id, location_id, last_ok_at, failures
                 FROM registrations ORDER BY created_at, id",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// Every byte the database keeps, its write-ahead log included.
    pub fn stored_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for suffix in ["", "-wal", "-shm"] {
            let mut path = self.store_path.clone().into_os_string();
            path.push(suffix);
            if let Ok(file) = std::fs::read(&path) {
                bytes.extend(file);
            }
        }
        bytes
    }
}

/// A till: its push subscription's keys, and its endpoint at a push
/// service.
pub struct Till {
    pub keys: ReceiverKeys,
    pub endpoint: String,
}

impl Till {
    /// A till whose endpoint is `/push/{name}` at `service`.
    pub fn at(service: &Stub, name: &str) -> Till {
        Till {
            keys: ReceiverKeys::generate(),
            endpoint: format!("{}/push/{name}", service.url()),
        }
    }

    pub fn subscription(&self) -> Value {
        let keys = self.keys.subscription_keys();
        json!({
            "endpoint": self.endpoint,
            "keys": {"p256dh": keys.p256dh.to_base64url(), "auth": keys.auth.to_base64url()},
        })
    }
}

/// A Bell's answer.
pub struct Answered {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Answered {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", self.text()))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The problem's slug, from its `type`.
    pub fn problem(&self) -> String {
        assert_eq!(
            self.headers[header::CONTENT_TYPE],
            "application/problem+json",
            "{}",
            self.text()
        );
        let problem = self.json();
        assert_eq!(problem["status"], self.status.as_u16());
        problem["type"]
            .as_str()
            .unwrap()
            .strip_prefix("https://github.com/OceanEilonwy/monokular/blob/main/docs/bell.md#")
            .unwrap()
            .to_owned()
    }
}

pub fn registration(token: &str, location: &str, subscription: &Value) -> Request<Body> {
    json_request(
        Method::POST,
        "/v1/register",
        json!({
            "square_access_token": token,
            "location_id": location,
            "subscription": subscription,
        }),
    )
}

pub fn json_request(method: Method, path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

pub fn unregistration(id: &str) -> Request<Body> {
    Request::delete(format!("/v1/register/{id}"))
        .body(Body::empty())
        .unwrap()
}

/// Square's signature of `body` sent to `url`: base64 of HMAC-SHA256 over
/// the URL and the body, keyed with [`WEBHOOK_KEY`].
pub fn sign(url: &str, body: &str) -> String {
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(WEBHOOK_KEY.as_bytes()).unwrap();
    mac.update(url.as_bytes());
    mac.update(body.as_bytes());
    STANDARD.encode(mac.finalize().into_bytes())
}

pub fn webhook_request(body: &str, signature: Option<&str>) -> Request<Body> {
    let mut request =
        Request::post("/v1/square/webhook").header(header::CONTENT_TYPE, "application/json");
    if let Some(signature) = signature {
        request = request.header("x-square-hmacsha256-signature", signature);
    }
    request.body(Body::from(body.to_owned())).unwrap()
}

/// What a card payment is to Square's webhooks: amounts, the card and the
/// buyer, which the Bell must keep none of.
pub const CARD_LAST_4: &str = "4242";
pub const FINGERPRINT: &str = "sq-1-fingerprint-of-the-buyers-card-Hn3aD9";
pub const BUYER_EMAIL: &str = "buyer.of.coffee@example.com";
pub const PAYMENT_ID: &str = "KkAkhdMsgzn59SM8A89WgKwekxLZY";
pub const AMOUNT: u64 = 92_517;

/// A `payment.created` or `payment.updated` webhook of a payment of
/// `merchant`'s at `location`, as Square sends it: each call another
/// payment, as the Bell rings each payment once.
pub fn payment_webhook(kind: &str, status: &str, merchant: &str, location: &str) -> String {
    static PAYMENTS: AtomicU64 = AtomicU64::new(0);
    let n = PAYMENTS.fetch_add(1, Ordering::Relaxed);
    payment_webhook_of(
        &format!("PmtAkhdMsgzn59SM8A89W{n:08}"),
        kind,
        status,
        merchant,
        location,
    )
}

/// A `payment.created` or `payment.updated` webhook of payment
/// `payment_id`, `merchant`'s at `location`, as Square sends it.
pub fn payment_webhook_of(
    payment_id: &str,
    kind: &str,
    status: &str,
    merchant: &str,
    location: &str,
) -> String {
    json!({
        "merchant_id": merchant,
        "type": kind,
        "event_id": "13b867cf-db3d-4b1c-90b6-2f32a9d78124",
        "created_at": "2026-10-27T10:00:00.000Z",
        "data": {
            "type": "payment",
            "id": payment_id,
            "object": {"payment": {
                "id": payment_id,
                "created_at": "2026-10-27T10:00:00.000Z",
                "amount_money": {"amount": AMOUNT, "currency": "USD"},
                "total_money": {"amount": AMOUNT, "currency": "USD"},
                "approved_money": {"amount": AMOUNT, "currency": "USD"},
                "card_details": {
                    "status": "CAPTURED",
                    "card": {
                        "card_brand": "VISA",
                        "last_4": CARD_LAST_4,
                        "exp_month": 11,
                        "exp_year": 2029,
                        "fingerprint": FINGERPRINT,
                        "bin": "424242",
                    },
                    "entry_method": "KEYED",
                },
                "buyer_email_address": BUYER_EMAIL,
                "location_id": location,
                "order_id": "uZhPq6R6rkK3ZT9lYBJ8NawqHTMZY",
                "receipt_number": "Kkxx",
                "status": status,
                "source_type": "CARD",
                "version": 3,
            }},
        },
    })
    .to_string()
}

/// Every log line this thread writes, at every level, until the guard
/// drops. One global subscriber sends each line to the buffer of the
/// thread that wrote it (tests run on a current-thread runtime, so the
/// Bell's lines are the test's thread's).
pub fn capture_log() -> (CaptureGuard, Captured) {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(|| ThreadSink)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
    let captured = Captured::default();
    SINK.with(|sink| *sink.borrow_mut() = Some(captured.clone()));
    (CaptureGuard(()), captured)
}

#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

thread_local! {
    static SINK: RefCell<Option<Captured>> = const { RefCell::new(None) };
}

struct ThreadSink;

impl std::io::Write for ThreadSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        SINK.with(|sink| {
            if let Some(captured) = sink.borrow().as_ref() {
                captured
                    .0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct CaptureGuard(());

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        SINK.with(|sink| sink.borrow_mut().take());
    }
}
