//! `Sender::send` against a push service on loopback: the one place a push
//! has to land for real, so the request on the wire (headers, signature,
//! body) and the answers read from a real response are checked end to end.
//! What `send` does with each answer is tested in-process in `send.rs`;
//! these say the same holds over a socket.
//!
//! An endpoint is https only, so the stub speaks TLS with a certificate
//! for `push.test` signed by a test CA (`fixtures/`, made with the openssl
//! CLI, valid until 2126; the CA's own key was thrown away). The client
//! trusts only that CA and resolves `push.test` to the stub.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use http::{HeaderMap, HeaderValue, Method, StatusCode};
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use push::encrypt::{decrypt, ReceiverKeys};
use push::wire::PushRequest;
use push::{
    encrypt, vapid, Contact, Endpoint, Message, Options, PlainJson, SendError, Sender, Sent, Topic,
    Ttl, Urgency, VapidKey,
};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::rustls::pki_types::pem::PemObject as _;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::{crypto, ServerConfig};
use tokio_rustls::TlsAcceptor;

const CA: &[u8] = include_bytes!("fixtures/ca.pem");
const CERT: &[u8] = include_bytes!("fixtures/push.test.pem");
const KEY: &[u8] = include_bytes!("fixtures/push.test.key.pem");

/// 2027-01-15T08:00:00Z, the time every push here is signed at.
const NOW: i64 = 1_800_000_000;

/// What the push service received.
struct Received {
    method: Method,
    path: String,
    headers: HeaderMap,
    body: Vec<u8>,
}

/// An answer the push service gives, in turn.
#[derive(Clone, Copy)]
struct Answer {
    status: StatusCode,
    retry_after: Option<&'static str>,
}

impl Answer {
    const fn status(code: u16) -> Self {
        let Ok(status) = StatusCode::from_u16(code) else {
            panic!("not a status code")
        };
        Answer {
            status,
            retry_after: None,
        }
    }

    const fn retry_after(self, value: &'static str) -> Self {
        Answer {
            retry_after: Some(value),
            ..self
        }
    }
}

/// A push service at `https://push.test:<port>`, answering each push with
/// the next of `answers` (`201` once they run out) and handing over what
/// it received.
struct PushService {
    address: SocketAddr,
    received: mpsc::UnboundedReceiver<Received>,
}

impl PushService {
    async fn start(answers: impl IntoIterator<Item = Answer>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server_config()));
        let answers = Arc::new(Mutex::new(answers.into_iter().collect::<VecDeque<_>>()));
        let (sent, received) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let (acceptor, answers, sent) = (acceptor.clone(), answers.clone(), sent.clone());
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let service = hyper::service::service_fn(move |request| {
                        let (answers, sent) = (answers.clone(), sent.clone());
                        async move { answer(request, &answers, &sent).await }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tls), service)
                        .await;
                });
            }
        });
        PushService { address, received }
    }

    fn endpoint(&self, path: &str) -> Endpoint {
        Endpoint::parse(&format!("https://push.test:{}{path}", self.address.port())).unwrap()
    }

    /// A client that trusts only the test CA and finds `push.test` here,
    /// and never follows a redirect, as a sender's client shouldn't.
    fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .tls_certs_only([reqwest::Certificate::from_pem(CA).unwrap()])
            .resolve("push.test", self.address)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap()
    }

    async fn next(&mut self) -> Received {
        self.received.recv().await.unwrap()
    }
}

fn server_config() -> ServerConfig {
    let certs = CertificateDer::pem_slice_iter(CERT)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(KEY).unwrap();
    ServerConfig::builder_with_provider(Arc::new(crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap()
}

async fn answer(
    request: hyper::Request<Incoming>,
    answers: &Mutex<VecDeque<Answer>>,
    sent: &mpsc::UnboundedSender<Received>,
) -> Result<hyper::Response<Full<&'static [u8]>>, hyper::Error> {
    let (parts, body) = request.into_parts();
    let body = body.collect().await?.to_bytes().to_vec();
    let _ = sent.send(Received {
        method: parts.method,
        path: parts.uri.path().to_owned(),
        headers: parts.headers,
        body,
    });
    let next = answers
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .pop_front()
        .unwrap_or(Answer::status(201));
    let mut response = hyper::Response::new(Full::new(&b""[..]));
    *response.status_mut() = next.status;
    if let Some(value) = next.retry_after {
        response
            .headers_mut()
            .insert(http::header::RETRY_AFTER, HeaderValue::from_static(value));
    }
    if next.status.is_redirection() {
        response.headers_mut().insert(
            http::header::LOCATION,
            HeaderValue::from_static("https://push.test/elsewhere"),
        );
    }
    Ok(response)
}

fn options() -> Options {
    Options {
        urgency: Urgency::High,
        ttl: Ttl::seconds(1800),
        topic: Some(Topic::parse("0123456789abcdef0123456789abcdef").unwrap()),
    }
}

/// An encrypted push arrives as RFC 8030 has it: `POST` to the endpoint's
/// path, its options as headers, a VAPID signature for the push service's
/// own origin carrying the sender's contact, and a body only the device
/// can read.
#[tokio::test]
async fn an_encrypted_push_lands_signed_for_the_push_service_and_decrypts_over_http() {
    let mut service = PushService::start([]).await;
    let key = Arc::new(VapidKey::generate());
    let contact = Contact::parse("mailto:ops@example.net").unwrap();
    let sender = Sender::new(service.client(), key.clone(), Some(contact.clone()));
    let device = ReceiverKeys::generate();
    let endpoint = service.endpoint("/wpush/v2/secret-token");
    let message = Message::Encrypted(
        encrypt(&device.subscription_keys(), br#"{"sale":{"phase":"paid"}}"#).unwrap(),
    );

    let sent = sender
        .send(&endpoint, &message, &options(), NOW)
        .await
        .unwrap();
    assert_eq!(
        sent,
        Sent {
            status: StatusCode::CREATED
        }
    );

    let received = service.next().await;
    assert_eq!(received.method, Method::POST);
    assert_eq!(received.path, "/wpush/v2/secret-token");
    assert_eq!(received.headers["ttl"], "1800");
    assert_eq!(received.headers["urgency"], "high");
    assert_eq!(
        received.headers["topic"],
        "0123456789abcdef0123456789abcdef"
    );
    assert_eq!(received.headers["content-encoding"], "aes128gcm");
    assert_eq!(
        received.headers["content-length"],
        received.body.len().to_string().as_str()
    );

    let request = PushRequest::parse(&received.headers, &received.body).unwrap();
    assert_eq!(request.options, options());
    let claims = vapid::verify(
        &request.authorization,
        &key.public(),
        &endpoint.origin(),
        NOW,
    )
    .unwrap();
    assert_eq!(
        claims.aud.as_str(),
        format!("https://push.test:{}", service.address.port())
    );
    assert_eq!(claims.sub, Some(contact));
    let Message::Encrypted(body) = request.message else {
        panic!("not an encrypted push");
    };
    assert_eq!(
        decrypt(&device, body.as_bytes()).unwrap(),
        br#"{"sale":{"phase":"paid"}}"#
    );
}

/// A Live Activity's state goes as JSON a relay reads, with no content
/// coding, and a TTL of zero is sent as zero.
#[tokio::test]
async fn a_plain_json_push_lands_as_json_over_http() {
    let mut service = PushService::start([]).await;
    let sender = Sender::new(service.client(), Arc::new(VapidKey::generate()), None);
    let state = serde_json::json!({"phase": "seen", "amount": "12.50"});
    let message = Message::PlainJson(PlainJson::new(&state).unwrap());
    let options = Options {
        urgency: Urgency::Low,
        ttl: Ttl::ZERO,
        topic: None,
    };

    sender
        .send(&service.endpoint("/v1/push/ph_1"), &message, &options, NOW)
        .await
        .unwrap();

    let received = service.next().await;
    assert_eq!(received.headers["content-type"], "application/json");
    assert!(!received.headers.contains_key("content-encoding"));
    assert!(!received.headers.contains_key("topic"));
    assert_eq!(received.headers["ttl"], "0");
    let request = PushRequest::parse(&received.headers, &received.body).unwrap();
    assert_eq!(request.options, options);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(request.message.body()).unwrap(),
        state
    );
}

/// Each answer a push service gives, read from a real response: 2xx is
/// sent, 404 and 410 say the subscription is gone, 413 too large, 429
/// and 5xx come with the wait Retry-After gives, anything else (a
/// redirect the client doesn't follow among them) is a refusal. The pushes
/// to one push service carry one token between them.
#[tokio::test]
async fn each_push_service_answer_is_sorted_over_http() {
    let answers = [
        Answer::status(201),
        Answer::status(200),
        Answer::status(404),
        Answer::status(410),
        Answer::status(413),
        Answer::status(429).retry_after("7"),
        Answer::status(429),
        Answer::status(500),
        Answer::status(503).retry_after("Fri, 15 Jan 2027 08:01:00 GMT"),
        Answer::status(400),
        Answer::status(401),
        Answer::status(307),
    ];
    let mut service = PushService::start(answers).await;
    let sender = Sender::new(service.client(), Arc::new(VapidKey::generate()), None);
    let device = ReceiverKeys::generate();
    let endpoint = service.endpoint("/wpush/abc");

    let mut outcomes = Vec::new();
    for _ in answers {
        let message = Message::Encrypted(encrypt(&device.subscription_keys(), b"{}").unwrap());
        outcomes.push(sender.send(&endpoint, &message, &options(), NOW).await);
    }

    let mut outcomes = outcomes.into_iter();
    let mut next = || outcomes.next().unwrap();
    assert_eq!(next().unwrap().status, StatusCode::CREATED);
    assert_eq!(next().unwrap().status, StatusCode::OK);
    assert!(matches!(next(), Err(SendError::Gone)));
    assert!(matches!(next(), Err(SendError::Gone)));
    assert!(matches!(next(), Err(SendError::TooLarge)));
    assert!(matches!(
        next(),
        Err(SendError::RateLimited { retry_after: Some(wait) }) if wait == Duration::from_secs(7)
    ));
    assert!(matches!(
        next(),
        Err(SendError::RateLimited { retry_after: None })
    ));
    assert!(matches!(
        next(),
        Err(SendError::Server {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            retry_after: None
        })
    ));
    assert!(matches!(
        next(),
        Err(SendError::Server { status: StatusCode::SERVICE_UNAVAILABLE, retry_after: Some(wait) })
            if wait == Duration::from_secs(60)
    ));
    assert!(matches!(
        next(),
        Err(SendError::Refused {
            status: StatusCode::BAD_REQUEST
        })
    ));
    assert!(matches!(
        next(),
        Err(SendError::Refused {
            status: StatusCode::UNAUTHORIZED
        })
    ));
    assert!(matches!(
        next(),
        Err(SendError::Refused {
            status: StatusCode::TEMPORARY_REDIRECT
        })
    ));

    let mut tokens = Vec::new();
    for _ in answers {
        tokens.push(service.next().await.headers["authorization"].clone());
    }
    assert!(tokens.windows(2).all(|pair| pair[0] == pair[1]));
    // The redirect wasn't followed: nothing more arrived.
    assert!(service.received.try_recv().is_err());
}

/// Pushes sent at once, as the delivery worker sends them, all land, each
/// decrypting to its own plaintext.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pushes_sent_at_once_all_land_over_http() {
    let mut service = PushService::start([]).await;
    let sender = Arc::new(Sender::new(
        service.client(),
        Arc::new(VapidKey::generate()),
        None,
    ));
    let device = Arc::new(ReceiverKeys::generate());
    let endpoint = service.endpoint("/wpush/abc");

    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..16 {
        let (sender, device, endpoint) = (sender.clone(), device.clone(), endpoint.clone());
        tasks.spawn(async move {
            let plaintext = format!("{{\"n\":{n}}}");
            let message = Message::Encrypted(
                encrypt(&device.subscription_keys(), plaintext.as_bytes()).unwrap(),
            );
            sender.send(&endpoint, &message, &options(), NOW).await
        });
    }
    while let Some(sent) = tasks.join_next().await {
        assert_eq!(sent.unwrap().unwrap().status, StatusCode::CREATED);
    }

    let mut plaintexts = Vec::new();
    for _ in 0..16 {
        let received = service.next().await;
        plaintexts.push(String::from_utf8(decrypt(&device, &received.body).unwrap()).unwrap());
    }
    plaintexts.sort();
    let mut expected: Vec<String> = (0..16).map(|n| format!("{{\"n\":{n}}}")).collect();
    expected.sort();
    assert_eq!(plaintexts, expected);
}

/// A push service that hangs up is a request error, and the error never
/// carries the endpoint's path: that is the subscription's capability,
/// and errors end up in logs.
#[tokio::test]
async fn an_unreachable_push_service_is_a_request_error_without_the_endpoint_over_http() {
    // Something listens and drops every connection at once: as fast to
    // fail on every system as a refused one (see service_runtime's
    // `unreachable`, which this crate can't depend on).
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((connection, _)) = listener.accept().await {
            drop(connection);
        }
    });
    let client = reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_pem(CA).unwrap()])
        .resolve("push.test", address)
        .build()
        .unwrap();
    let sender = Sender::new(client, Arc::new(VapidKey::generate()), None);
    let endpoint = Endpoint::parse(&format!(
        "https://push.test:{}/wpush/secret-token",
        address.port()
    ))
    .unwrap();
    let message = Message::PlainJson(PlainJson::new(&()).unwrap());

    let error = sender
        .send(&endpoint, &message, &options(), NOW)
        .await
        .unwrap_err();
    assert!(matches!(error, SendError::Request(_)), "{error:?}");
    for printed in [error.to_string(), format!("{error:?}")] {
        assert!(!printed.contains("secret-token"), "{printed}");
    }
}
