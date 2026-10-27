//! What the relay's tests share: a relay built from settings as a process
//! builds it, called in-process; stand-ins for APNs, FCM and FCM's token
//! endpoint on loopback; pushes written the way a sender writes them; and
//! a capture of the log.
//!
//! `fixtures/` holds throwaway keys made for these tests and trusted by
//! nothing: `apns-test.p8`, a P-256 key in the form of Apple's
//! `AuthKey_<id>.p8` (`openssl genpkey -algorithm EC -pkeyopt
//! ec_paramgen_curve:P-256`), and `fcm-test-key.pem`, the 2048-bit RSA
//! key (`openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048`) of
//! the service account [`fcm_account`] makes, in the form Google hands
//! out, its `token_uri` a stand-in's. (Made in code, not kept as a file:
//! a file that looks like a real service account is refused by the
//! repository's secret scanning.)

#![allow(dead_code, reason = "each test file uses its own part of this")]

use std::cell::RefCell;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Once, PoisonError};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use push::{Message, Origin, Topic, Ttl, Urgency, VapidKey};
use push_relay::settings::{self, Host};
use push_relay::Relay;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tower::ServiceExt as _;

pub const APNS_KEY: &str = include_str!("../fixtures/apns-test.p8");
/// A service account getting its tokens at `token_uri`.
pub fn fcm_account(token_uri: &str) -> String {
    json!({
        "type": "service_account",
        "project_id": "monokular-test",
        "private_key_id": "0123456789abcdef0123456789abcdef01234567",
        "private_key": include_str!("../fixtures/fcm-test-key.pem"),
        "client_email": "relay@monokular-test.iam.gserviceaccount.com",
        "client_id": "100000000000000000000",
        "token_uri": token_uri,
    })
    .to_string()
}

/// Where the relay says it is, in the tests that call it in-process.
pub const PUBLIC_URI: &str = "https://relay.example.org";

/// The app every test registers for.
pub const APP: &str = "monokular";
pub const BUNDLE: &str = "com.example.monokular";
pub const PROJECT: &str = "monokular-test";

/// An APNs device token, as an app sends it.
pub fn apns_token() -> String {
    "0f".repeat(32)
}

pub const FCM_TOKEN: &str = "cX9:APA91b-device_token";

/// A request a stand-in received.
#[derive(Debug)]
pub struct Received {
    pub method: Method,
    pub path: String,
    pub version: axum::http::Version,
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

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
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

/// A stand-in on loopback, over HTTP/1.1 or HTTP/2 with prior knowledge
/// (as the relay reaches an `http` APNs), answering each request as
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
                                version: parts.version,
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
                    let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
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

    /// A stand-in that takes everything: `200`, and for FCM's token
    /// endpoint an access token good for an hour.
    pub async fn accepting() -> Arc<Stub> {
        Stub::start(|received| {
            if received.path == "/token" {
                Answer::status(200).json(json!({
                    "access_token": "ya29.test-access-token",
                    "expires_in": 3600,
                    "token_type": "Bearer",
                }))
            } else {
                Answer::status(200).json(json!({"name": "projects/x/messages/1"}))
            }
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

/// A relay built as a process builds it, from options file `values` over
/// these: APNs and FCM at `apns` and `fcm` (FCM's token endpoint too),
/// the app [`APP`] on both, its database in a directory of its own.
pub struct TestRelay {
    pub relay: Relay,
    pub router: axum::Router,
    pub store_path: PathBuf,
}

impl TestRelay {
    pub async fn start(apns: &Stub, fcm: &Stub, values: &[(&str, &str)]) -> TestRelay {
        Self::at(PUBLIC_URI, apns, fcm, values).await
    }

    pub async fn at(
        public_uri: &str,
        apns: &Stub,
        fcm: &Stub,
        values: &[(&str, &str)],
    ) -> TestRelay {
        let dir = std::env::temp_dir().join(format!(
            "push-relay-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let store_path = dir.join("push-relay.db");
        let apps = json!({APP: {"apns_topic": BUNDLE, "fcm_project_id": PROJECT}}).to_string();
        let (apns_url, fcm_url) = (apns.url(), fcm.url());
        let mut stored: HashMap<String, String> = [
            ("server.public_uri", public_uri),
            ("store.path", store_path.to_str().unwrap()),
            ("handles.apps", apps.as_str()),
            ("apns.endpoint", apns_url.as_str()),
            ("apns.team_id", "TEAM123456"),
            ("apns.key_id", "KEY1234567"),
            ("fcm.endpoint", fcm_url.as_str()),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        for (key, value) in values {
            stored.insert((*key).to_owned(), (*value).to_owned());
        }
        let env = live_settings::Env::fixed([
            ("MONOKULO_RELAY_APNS_KEY", APNS_KEY.to_owned()),
            (
                "MONOKULO_RELAY_FCM_CREDENTIALS",
                fcm_account(&format!("{}/token", fcm.url())),
            ),
        ]);
        let config = settings::load(stored, env, Host::Standalone).await.unwrap();
        let relay = Relay::start(&config).await.unwrap();
        TestRelay {
            router: relay.router(),
            relay,
            store_path,
        }
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

    /// Registers `token` on `platform` for `sender`, and hands back the
    /// endpoint.
    pub async fn register(&self, platform: &str, token: &str, sender: &VapidKey) -> String {
        let answered = self.call(registration(platform, token, sender)).await;
        assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.text());
        answered.json()["endpoint"].as_str().unwrap().to_owned()
    }

    /// `push` to `endpoint` (one of this relay's).
    pub async fn push(&self, endpoint: &str, push: PushBuilder) -> Answered {
        self.call(push.request(endpoint)).await
    }

    /// Sets the handle's expiry to `expires_at`, as if that much time had
    /// passed.
    pub fn expire(&self, endpoint: &str, expires_at: i64) {
        let id = endpoint.rsplit('/').next().unwrap();
        let connection = rusqlite::Connection::open(&self.store_path).unwrap();
        connection
            .execute(
                "UPDATE handles SET expires_at = ?2 WHERE id = ?1",
                rusqlite::params![id, expires_at],
            )
            .unwrap();
    }

    /// How many handles the database holds.
    pub fn handles(&self) -> i64 {
        let connection = rusqlite::Connection::open(&self.store_path).unwrap();
        connection
            .query_row("SELECT COUNT(*) FROM handles", [], |row| row.get(0))
            .unwrap()
    }
}

/// A relay's answer.
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
            .strip_prefix("https://docs.monokulo.com/push-relay/#")
            .unwrap()
            .to_owned()
    }
}

pub fn registration(platform: &str, token: &str, sender: &VapidKey) -> Request<Body> {
    json_request(
        "/v1/register",
        json!({
            "platform": platform,
            "token": token,
            "app": APP,
            "vapid_public_key": sender.public().to_base64url(),
        }),
    )
}

pub fn json_request(path: &str, body: Value) -> Request<Body> {
    Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// A push, written as a sender writes it (RFC 8030, RFC 8291, RFC 8292).
pub struct PushBuilder {
    pub key: Arc<VapidKey>,
    pub audience: String,
    pub message: Message,
    pub urgency: Urgency,
    pub ttl: Option<Ttl>,
    pub topic: Option<Topic>,
}

impl PushBuilder {
    /// `message` signed by `key` for the relay at [`PUBLIC_URI`], high
    /// urgency, a TTL of 30 minutes, topic `sale-1042`.
    pub fn new(key: &Arc<VapidKey>, message: Message) -> Self {
        PushBuilder {
            key: key.clone(),
            audience: PUBLIC_URI.to_owned(),
            message,
            urgency: Urgency::High,
            ttl: Some(Ttl::seconds(1800)),
            topic: Some(Topic::parse("sale-1042").unwrap()),
        }
    }

    pub fn request(&self, endpoint: &str) -> Request<Body> {
        let path = endpoint
            .strip_prefix(PUBLIC_URI)
            .unwrap_or_else(|| panic!("{endpoint} is under {PUBLIC_URI}"));
        let authorization = self
            .key
            .authorization(
                &Origin::parse(&self.audience).unwrap(),
                None,
                service_runtime::time::now_unix(),
            )
            .unwrap();
        let mut request = Request::post(path)
            .header(header::AUTHORIZATION, authorization.header_value())
            .header("urgency", self.urgency.header_value());
        if let Some(ttl) = self.ttl {
            request = request.header("ttl", ttl.as_secs());
        }
        if let Some(topic) = &self.topic {
            request = request.header("topic", topic.as_str());
        }
        request = match &self.message {
            Message::Encrypted(_) => request
                .header(header::CONTENT_ENCODING, "aes128gcm")
                .header(header::CONTENT_TYPE, "application/octet-stream"),
            Message::PlainJson(_) => request.header(header::CONTENT_TYPE, "application/json"),
        };
        request
            .body(Body::from(self.message.body().to_vec()))
            .unwrap()
    }
}

/// A device's keys and what a sender would encrypt to them.
pub fn encrypted(keys: &push::encrypt::ReceiverKeys, plaintext: &[u8]) -> Message {
    Message::Encrypted(push::encrypt(&keys.subscription_keys(), plaintext).unwrap())
}

pub fn plain(value: Value) -> Message {
    Message::PlainJson(push::PlainJson::new(&value).unwrap())
}

/// Every log line this thread writes, at every level, until the guard
/// drops. One global subscriber sends each line to the buffer of the
/// thread that wrote it (tests run on a current-thread runtime, so the
/// relay's lines are the test's thread's).
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
