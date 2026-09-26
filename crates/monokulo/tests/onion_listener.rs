//! The onion listener with synthetic PROXY headers, over real sockets (runs
//! by default; the real-tor version is `e2e_tor.rs`, `#[ignore]`d).
//!
//! Proves: each circuit named in a PROXY header is its own client with its
//! own budget; a connection without a valid header is dropped; and the
//! ordinary listener does not honour a PROXY header at all.

use std::sync::Arc;

use monokulo::abuse::proxy_protocol::{OnionListener, OnionPeer};
use monokulo::abuse::{AbuseConfig, AbuseProtection};
use monokulo::db::Db;
use monokulo::engine_client::EngineClient;
use monokulo::http::{build_router, AppState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A soft limit of `soft_per_min` (past it the JSON status route answers
/// `429` with a challenge) and a much higher hard limit.
fn state(soft_per_min: u32) -> AppState {
    AppState {
        db: Db::open_in_memory().unwrap().into_shared(),
        // Never reached: every request below is for an unknown store.
        engine_client: EngineClient::new("http://127.0.0.1:1"),
        encryption_key: [7u8; 32],
        status_cache: monokulo::http::status_page::new_status_cache(),
        exchange_rate: Arc::new(monokulo::exchange_rate_config::ExchangeRateProviders::xmr_only()),
        abuse: Arc::new(AbuseProtection::new(AbuseConfig { soft_per_min, ..Default::default() })),
        dns: Arc::new(monokulo::embed_domains::UnavailableDns("no DNS in tests".to_string())),
    }
}

/// Sends `prefix` then one HTTP request, returns the status line's code
/// (or `None` if the server closed without answering).
async fn request(addr: std::net::SocketAddr, prefix: &str) -> Option<u16> {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!("{prefix}GET /pay/pk_unknown/orders/o1/status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).await;
    let text = String::from_utf8_lossy(&response);
    text.split(' ').nth(1).and_then(|code| code.parse().ok())
}

fn circuit(id: u32) -> String {
    format!("PROXY TCP6 fc00:dead:beef:4dad::{:x}:{:x} ::1 65535 42\r\n", id >> 16, id & 0xffff)
}

#[tokio::test]
async fn each_circuit_is_its_own_client_and_headerless_connections_are_dropped() {
    let router = build_router(state(2));
    let listener = OnionListener::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.bound_address();
    tokio::spawn(async move {
        axum::serve(listener, router.into_make_service_with_connect_info::<OnionPeer>()).await.unwrap();
    });

    // Circuit 1 spends its budget of 2 (unknown store: 404), then gets 429...
    assert_eq!(request(addr, &circuit(1)).await, Some(404));
    assert_eq!(request(addr, &circuit(1)).await, Some(404));
    assert_eq!(request(addr, &circuit(1)).await, Some(429));
    // ...while circuit 2 is unaffected.
    assert_eq!(request(addr, &circuit(0x0002_0001)).await, Some(404));

    // No header, or a malformed one: closed without an answer.
    assert_eq!(request(addr, "").await, None);
    assert_eq!(request(addr, "PROXY UNKNOWN\r\n").await, None);
}

#[tokio::test]
async fn the_ordinary_listener_never_honours_a_proxy_header() {
    let router = build_router(state(1));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
    });

    // A client can't pick its own identity: the header is not HTTP, so the
    // request is refused outright rather than attributed to a circuit.
    let answer = request(addr, &circuit(9)).await;
    assert!(matches!(answer, None | Some(400)), "got {answer:?}");
    // And the peer address is still what's counted: budget 1, then 429.
    assert_eq!(request(addr, "").await, Some(404));
    assert_eq!(request(addr, "").await, Some(429));
}
