//! The ZMTP subscriber against the spec's bytes, malformed publishers and
//! arbitrary input. Each side of a connection here is a `tokio::io::duplex`
//! end; the test plays the publisher byte by byte.
use super::*;
use crate::property_support::{config, runtime};
use proptest::prelude::*;
use tokio::io::DuplexStream;

const POOL: &str = "json-minimal-txpool_add";
const CHAIN: &str = "json-minimal-chain_main";

/// A publisher's greeting and READY, written out from spec 23 rather than
/// built by the code under test.
fn publisher_handshake(socket_type: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0x7F, 3, 1];
    bytes.extend_from_slice(b"NULL");
    bytes.extend_from_slice(&[0; 16]);
    bytes.push(1); // as-server, which NULL ignores
    bytes.extend_from_slice(&[0; 31]);
    let mut ready = b"\x05READY\x0bSocket-Type".to_vec();
    ready.extend_from_slice(&(socket_type.len() as u32).to_be_bytes());
    ready.extend_from_slice(socket_type);
    bytes.extend_from_slice(&[0x04, ready.len() as u8]);
    bytes.extend_from_slice(&ready);
    bytes
}

/// Starts a subscriber to both topics over a duplex, the publisher having
/// written `from_publisher` (and then kept the connection open).
async fn start(from_publisher: &[u8]) -> (io::Result<Subscriber<DuplexStream>>, DuplexStream) {
    let (ours, mut theirs) = tokio::io::duplex(1 << 20);
    theirs.write_all(from_publisher).await.unwrap();
    (Subscriber::start(ours, &[POOL, CHAIN]).await, theirs)
}

/// A subscriber past the handshake, and the publisher's end after reading
/// everything the subscriber wrote during it.
async fn subscribed() -> (Subscriber<DuplexStream>, DuplexStream) {
    let (subscriber, mut theirs) = start(&publisher_handshake(b"PUB")).await;
    let mut sent = vec![0; 64 + 27 + 2 * 26];
    theirs.read_exact(&mut sent).await.unwrap();
    (subscriber.unwrap(), theirs)
}

fn error_of(result: io::Result<impl Sized>) -> io::Error {
    match result {
        Ok(_) => panic!("accepted"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn what_the_subscriber_sends_is_zmtp_3_0_byte_for_byte() {
    let (subscriber, mut theirs) = start(&publisher_handshake(b"XPUB")).await;
    subscriber.unwrap();
    let mut expected = vec![0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0x7F, 3, 0];
    expected.extend_from_slice(b"NULL");
    expected.extend_from_slice(&[0; 16 + 1 + 31]);
    expected.extend_from_slice(b"\x04\x19\x05READY\x0bSocket-Type\x00\x00\x00\x03SUB");
    for topic in [POOL, CHAIN] {
        expected.extend_from_slice(&[0x00, 24, 0x01]);
        expected.extend_from_slice(topic.as_bytes());
    }
    let mut sent = vec![0; expected.len()];
    theirs.read_exact(&mut sent).await.unwrap();
    assert_eq!(sent, expected);
}

#[tokio::test]
async fn a_greeting_from_anything_but_a_null_zmtp_3_peer_is_refused() {
    let good = publisher_handshake(b"PUB");
    let mut http = b"HTTP/1.1 400 Bad Request\r\n".to_vec();
    http.resize(64, b' ');
    let mut zmtp_2 = good.clone();
    zmtp_2[10] = 1;
    let mut curve = good.clone();
    curve[12..17].copy_from_slice(b"CURVE");
    let mut longer = good.clone();
    longer[16] = b'X';
    for (what, greeting) in [
        ("http", http),
        ("zmtp 2", zmtp_2),
        ("curve", curve),
        ("NULLX", longer),
    ] {
        let error = error_of(start(&greeting).await.0);
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{what}: {error}");
    }
    // A later version of ZMTP 3 is fine: it downgrades to ours.
    let mut later = good;
    later[11] = 9;
    start(&later).await.0.unwrap();
}

#[tokio::test]
async fn a_peer_that_is_not_a_publisher_is_refused() {
    let greeting = &publisher_handshake(b"PUB")[..64];
    let with = |ready: &[u8]| {
        let mut bytes = greeting.to_vec();
        bytes.extend_from_slice(ready);
        bytes
    };
    for (what, bytes, says) in [
        ("req", publisher_handshake(b"REQ"), "a REQ socket"),
        (
            "no socket type",
            with(b"\x04\x06\x05READY"),
            "no Socket-Type",
        ),
        ("truncated", with(b"\x04\x0b\x05READY\x04Iden"), "truncated"),
        (
            "unnamed",
            with(b"\x04\x0b\x05READY\x00\x00\x00\x00\x00"),
            "no name",
        ),
        ("message first", with(b"\x00\x01x"), "a message"),
        ("not ready", with(b"\x04\x05\x04PING"), "not READY"),
        ("no name", with(b"\x04\x01\x00"), "overruns"),
        ("empty command", with(b"\x04\x00"), "empty command"),
        (
            "error",
            with(b"\x04\x0d\x05ERROR\x06banned"),
            "refused the connection: banned",
        ),
    ] {
        let error = error_of(start(&bytes).await.0);
        assert!(error.to_string().contains(says), "{what}: {error}");
    }
    // Other properties, in any case, are passed over.
    let mut ready =
        b"\x05READY\x08Identity\x00\x00\x00\x02ab\x0bsocket-type\x00\x00\x00\x03PUB".to_vec();
    ready.insert(0, ready.len() as u8);
    ready.insert(0, 0x04);
    start(&with(&ready)).await.0.unwrap();
}

#[tokio::test]
async fn messages_yield_their_first_frame_whatever_the_framing() {
    let (mut subscriber, mut theirs) = subscribed().await;
    let long = "x".repeat(300);
    let mut bytes = vec![0x00, 4];
    bytes.extend_from_slice(b"pool");
    // A long size for a short frame is allowed.
    bytes.extend_from_slice(&[0x02, 0, 0, 0, 0, 0, 0, 0, 5]);
    bytes.extend_from_slice(b"chain");
    // A frame over 255 bytes, which needs the long size.
    bytes.extend_from_slice(&[0x02, 0, 0, 0, 0, 0, 0, 1, 44]);
    bytes.extend_from_slice(long.as_bytes());
    // Three frames: the later two are read past.
    bytes.extend_from_slice(b"\x01\x05first\x03\x00\x00\x00\x00\x00\x00\x00\x01y\x00\x01z");
    // A command between messages is passed over.
    bytes.extend_from_slice(b"\x04\x09\x08SUBSCRIB\x00\x04last");
    bytes.extend_from_slice(b"\x00\x00");
    theirs.write_all(&bytes).await.unwrap();
    for expected in [
        b"pool".as_slice(),
        b"chain",
        long.as_bytes(),
        b"first",
        b"last",
        b"",
    ] {
        assert_eq!(subscriber.recv().await.unwrap(), expected);
    }
    drop(theirs);
    let error = error_of(subscriber.recv().await);
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
}

#[tokio::test]
async fn a_ping_is_answered_with_its_context() {
    let (mut subscriber, mut theirs) = subscribed().await;
    theirs
        .write_all(b"\x04\x0a\x04PING\x00\x64ctx\x00\x05after")
        .await
        .unwrap();
    assert_eq!(subscriber.recv().await.unwrap(), b"after");
    let mut pong = [0; 10];
    theirs.read_exact(&mut pong).await.unwrap();
    assert_eq!(&pong, b"\x04\x08\x04PONGctx");
    theirs
        .write_all(&[[0x04, 24, 4].as_slice(), b"PING", &[0; 19]].concat())
        .await
        .unwrap();
    let error = error_of(subscriber.recv().await);
    assert!(error.to_string().contains("over 16 bytes"), "{error}");
}

#[tokio::test]
async fn malformed_frames_end_the_connection() {
    let over = (MAX_FRAME + 1).to_be_bytes();
    for (what, bytes, says) in [
        ("reserved", b"\x08\x00".to_vec(), "reserved bits"),
        (
            "command with more",
            b"\x05\x00".to_vec(),
            "more frames after",
        ),
        // Refused from the header alone: nothing that big is waited for.
        ("oversize", [[0x02].as_slice(), &over].concat(), "over the"),
        (
            "huge",
            [[0x02].as_slice(), &u64::MAX.to_be_bytes()].concat(),
            "over the",
        ),
        (
            "command inside a message",
            b"\x01\x01a\x04\x05\x04PING".to_vec(),
            "inside a multi-frame",
        ),
        ("error", b"\x04\x07\x05ERROR\x00".to_vec(), "refused"),
    ] {
        let (mut subscriber, mut theirs) = subscribed().await;
        theirs.write_all(&bytes).await.unwrap();
        let error = error_of(subscriber.recv().await);
        assert!(error.to_string().contains(says), "{what}: {error}");
    }
    for (what, bytes) in [
        ("truncated body", b"\x00\x05abc".to_vec()),
        ("truncated later frame", b"\x01\x01a\x00\x05abc".to_vec()),
        ("truncated size", b"\x02\x00\x00".to_vec()),
    ] {
        let (mut subscriber, mut theirs) = subscribed().await;
        theirs.write_all(&bytes).await.unwrap();
        drop(theirs);
        let error = error_of(subscriber.recv().await);
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof, "{what}");
    }
}

#[tokio::test]
async fn a_publisher_that_never_answers_times_out() {
    tokio::time::pause();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("tcp://{}", listener.local_addr().unwrap());
    let error = error_of(connect(&endpoint, &[POOL]).await);
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    drop(listener);
}

#[tokio::test]
async fn only_tcp_and_ipc_endpoints_are_connected_to() {
    let error = error_of(connect("udp://127.0.0.1:1", &[POOL]).await);
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

/// The test publisher, over each transport: subscriptions reach it, and
/// what it sends for them comes back.
#[tokio::test]
async fn subscribes_over_tcp_and_unix_sockets() {
    let directory = tempfile_directory();
    let mut endpoints = vec!["tcp://127.0.0.1:0".to_owned()];
    if cfg!(unix) {
        endpoints.push(format!("ipc://{}/zmq.sock", directory.display()));
    }
    for endpoint in endpoints {
        let publisher = crate::node_events::test_publisher::Publisher::bind(&endpoint).await;
        let mut subscriber = connect(publisher.endpoint(), &[CHAIN]).await.unwrap();
        // Sent until the subscription has reached the publisher.
        let received = loop {
            publisher.send(&format!("{POOL}:not subscribed"));
            publisher.send(&format!("{CHAIN}:{{}}"));
            let wait = tokio::time::sleep(Duration::from_millis(10));
            tokio::select! {
                received = subscriber.recv() => break received.unwrap(),
                () = wait => {}
            }
        };
        assert_eq!(received, format!("{CHAIN}:{{}}").as_bytes(), "{endpoint}");
        drop(publisher);
        let error = error_of(subscriber.recv().await);
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof, "{endpoint}");
    }
    let _ = std::fs::remove_dir_all(directory);
}

/// A fresh directory under `/tmp` on Unix: macOS's own temporary directory
/// (`/var/folders/...`) is long enough to push a socket path in it past the
/// 104 bytes `sun_path` holds.
fn tempfile_directory() -> std::path::PathBuf {
    let parent = if cfg!(unix) {
        std::path::PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    let directory = parent.join(format!("engine-zmtp-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

/// A message as a publisher could frame it: its frames, each with a short
/// or long size, and commands the subscriber passes over before it.
fn framed(frames: &[(Vec<u8>, bool)], ping_first: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if ping_first {
        bytes.extend_from_slice(b"\x04\x09\x04PING\x00\x01ab");
    }
    for (at, (body, long)) in frames.iter().enumerate() {
        let more = u8::from(at + 1 < frames.len());
        if *long || body.len() > 255 {
            bytes.push(more | 0x02);
            bytes.extend_from_slice(&(body.len() as u64).to_be_bytes());
        } else {
            bytes.extend_from_slice(&[more, body.len() as u8]);
        }
        bytes.extend_from_slice(body);
    }
    bytes
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/zmtp_properties.txt"
        ),
    )
}

proptest! {
    #![proptest_config(persisted_config(config()))]

    /// Whatever a publisher sends after the handshake, the subscriber
    /// neither panics nor hangs once the stream ends, and never yields a
    /// frame longer than what was sent.
    #[test]
    fn arbitrary_input_ends_in_frames_or_an_error(input in prop::collection::vec(any::<u8>(), 0..2048)) {
        runtime().block_on(async {
            let (mut subscriber, mut theirs) = subscribed().await;
            theirs.write_all(&input).await.unwrap();
            drop(theirs);
            let mut yielded = 0;
            while let Ok(frame) = subscriber.recv().await {
                yielded += frame.len() + 2;
                assert!(yielded <= input.len());
            }
        });
    }

    /// Messages of any framing come back as their first frames, in order.
    #[test]
    fn messages_round_trip_as_their_first_frame(
        messages in prop::collection::vec(
            (prop::collection::vec((prop::collection::vec(any::<u8>(), 0..300), any::<bool>()), 1..4), any::<bool>()),
            0..16,
        ),
    ) {
        runtime().block_on(async {
            let (mut subscriber, mut theirs) = subscribed().await;
            let bytes: Vec<u8> = messages
                .iter()
                .flat_map(|(frames, ping)| framed(frames, *ping))
                .collect();
            theirs.write_all(&bytes).await.unwrap();
            for (frames, _) in &messages {
                assert_eq!(subscriber.recv().await.unwrap(), frames[0].0);
            }
        });
    }
}
