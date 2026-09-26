//! A real end-to-end check of monokulo's Tor support, against a real `tor`
//! process and the live Tor network. Ignored by default (like the stagenet
//! tests): run it with
//!
//! ```sh
//! cargo test -p monokulo --test e2e_tor -- --ignored --nocapture
//! ```
//!
//! Needs `tor` >= 0.4.8 with the proof-of-work module on `PATH`
//! (`tor --list-modules` shows `pow: yes`) and outbound network access.
//! Publishing a fresh onion service and reaching it takes a minute or two;
//! each wait below has a generous timeout and fails loudly when it runs out.
//!
//! What it does:
//!
//! 1. Starts monokulo in-process (with a real test engine behind it, and one
//!    store with one order), serving its onion listener on loopback.
//! 2. Starts a real `tor` with a temporary `DataDirectory` and a v3 onion
//!    service in front of that listener, using exactly the service lines of
//!    `deploy/tor/torrc.snippet` (only the directory and the target port
//!    are rewritten), plus a `SocksPort` and a `ControlPort` for the test.
//! 3. Checks through the control port that tor accepted the proof-of-work,
//!    circuit-export, intro-DoS and stream settings.
//! 4. Acts as several visitors through the `SocksPort`, each with its own
//!    SOCKS username/password, so tor's default `IsolateSOCKSAuth` puts each
//!    on its own circuit, and checks that monokulo sees one distinct circuit
//!    identity per visitor, challenges and then blocks only the abusive one,
//!    and caps open live-update streams per (circuit, store).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use monokulo::abuse::proxy_protocol::{OnionListener, OnionPeer};
use monokulo::abuse::{AbuseConfig, AbuseProtection, ClientIdentity};
use monokulo::db::Db;
use monokulo::engine_client::{CreateTenantRequest, EngineClient};
use monokulo::http::{build_router, AppState};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_socks::tcp::Socks5Stream;

const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
const ENCRYPTION_KEY: [u8; 32] = [7u8; 32];

/// Small limits so the test needs only a handful of requests over Tor.
const SOFT: u32 = 5;
const HARD: u32 = 12;
const STREAM_CAP: usize = 3;
const BITS: u32 = 8;

const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(300);
const REACHABLE_TIMEOUT: Duration = Duration::from_secs(420);
/// How long one visitor keeps retrying a SOCKS connect (see `Visitor::connect`).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(300);

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct Tor {
    child: tokio::process::Child,
    /// This run's files (onion service keys, torrc, log); removed afterwards.
    dir: PathBuf,
    /// tor's `DataDirectory`, kept between runs (see [`data_dir`]).
    data: PathBuf,
    socks: SocketAddr,
    control: SocketAddr,
}

impl Drop for Tor {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// tor's `DataDirectory`, kept under cargo's target directory between runs,
/// the way a real tor client keeps it. With a warm cache of the network
/// consensus and relay descriptors, tor bootstraps in seconds. From empty it
/// has to download them all first, which on the live network can take longer
/// than the bootstrap timeout. The onion service itself (keys, address) is
/// still new every run, in [`Tor::dir`].
fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("e2e-tor-data")
}

/// `deploy/tor/torrc.snippet`, with the directory and target port for this
/// run, plus what the test itself needs.
fn torrc(dir: &Path, data: &Path, onion_port: u16, socks: u16, control: u16) -> String {
    let snippet = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/tor/torrc.snippet"))
        .expect("deploy/tor/torrc.snippet must exist");
    assert!(snippet.contains("HiddenServiceDir /var/lib/tor/monokulo/"));
    assert!(snippet.contains("HiddenServicePort 80 127.0.0.1:8082"));
    let service = snippet
        .replace("HiddenServiceDir /var/lib/tor/monokulo/", &format!("HiddenServiceDir {}", dir.join("hs").display()))
        .replace("HiddenServicePort 80 127.0.0.1:8082", &format!("HiddenServicePort 80 127.0.0.1:{onion_port}"));
    format!(
        "DataDirectory {data}\nSocksPort 127.0.0.1:{socks}\nControlPort 127.0.0.1:{control}\nCookieAuthentication 1\n\
         Log notice file {log}\n{service}\n",
        data = data.display(),
        log = dir.join("tor.log").display(),
    )
}

async fn start_tor(onion_port: u16) -> Tor {
    let dir = std::env::temp_dir().join(format!("monokulo-e2e-tor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data = data_dir();
    for path in [dir.join("hs"), data.clone()] {
        std::fs::create_dir_all(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let (socks, control) = (free_port(), free_port());
    std::fs::write(dir.join("torrc"), torrc(&dir, &data, onion_port, socks, control)).unwrap();
    let child = tokio::process::Command::new("tor")
        .arg("-f")
        .arg(dir.join("torrc"))
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("could not start `tor` - is it installed and on PATH?");
    Tor { child, dir, data, socks: ([127, 0, 0, 1], socks).into(), control: ([127, 0, 0, 1], control).into() }
}

/// One control-port session, authenticated with the cookie.
struct Control(BufReader<TcpStream>);

impl Control {
    async fn connect(tor: &Tor) -> Control {
        let deadline = Instant::now() + Duration::from_secs(60);
        let stream = loop {
            match TcpStream::connect(tor.control).await {
                Ok(stream) => break stream,
                Err(e) if Instant::now() < deadline => {
                    let _ = e;
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Err(e) => panic!("tor's control port never opened: {e} (log: {})", tor.dir.join("tor.log").display()),
            }
        };
        let mut control = Control(BufReader::new(stream));
        let cookie = std::fs::read(tor.data.join("control_auth_cookie")).expect("control_auth_cookie");
        let reply = control.command(&format!("AUTHENTICATE {}", hex::encode(cookie))).await;
        assert!(reply.starts_with("250"), "control port refused authentication: {reply}");
        control
    }

    /// Sends one command and returns every reply line up to the final
    /// `250 ` (or error) line.
    async fn command(&mut self, command: &str) -> String {
        self.0.get_mut().write_all(format!("{command}\r\n").as_bytes()).await.unwrap();
        let mut reply = String::new();
        loop {
            let mut line = String::new();
            if self.0.read_line(&mut line).await.unwrap() == 0 {
                panic!("control port closed while answering {command:?}: {reply}");
            }
            reply.push_str(&line);
            if line.len() >= 4 && line.as_bytes()[3] == b' ' {
                return reply;
            }
        }
    }
}

/// One simulated visitor: its own SOCKS credentials, so its own circuit.
struct Visitor {
    name: &'static str,
    socks: SocketAddr,
    onion: String,
}

struct Reply {
    status: u16,
    headers: String,
    body: String,
}

impl Visitor {
    /// Connects through tor on this visitor's own circuit. Building a new
    /// circuit to an onion service on the live network sometimes takes
    /// longer than one attempt allows, so a failed SOCKS connect is retried
    /// until `CONNECT_TIMEOUT` runs out. A failed attempt never reaches
    /// monokulo, so retrying can't change the counts this test checks.
    async fn connect(&self) -> std::io::Result<TcpStream> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            let attempt = tokio::time::timeout(
                Duration::from_secs(90),
                Socks5Stream::connect_with_password(self.socks, (self.onion.as_str(), 80), self.name, "password"),
            )
            .await;
            let error = match attempt {
                Ok(Ok(stream)) => return Ok(stream.into_inner()),
                Ok(Err(e)) => e.to_string(),
                Err(_) => "SOCKS connect timed out".to_string(),
            };
            if Instant::now() >= deadline {
                return Err(std::io::Error::other(format!("{}: {error} (gave up after {CONNECT_TIMEOUT:?})", self.name)));
            }
            println!("{}: {error}, retrying", self.name);
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    async fn get(&self, path: &str, extra_headers: &str) -> std::io::Result<Reply> {
        let mut stream = self.connect().await?;
        let request = format!("GET {path} HTTP/1.1\r\nHost: {}\r\n{extra_headers}Connection: close\r\n\r\n", self.onion);
        stream.write_all(request.as_bytes()).await?;
        let mut raw = Vec::new();
        tokio::time::timeout(Duration::from_secs(90), stream.read_to_end(&mut raw))
            .await
            .map_err(|_| std::io::Error::other("response timed out"))??;
        let text = String::from_utf8_lossy(&raw).to_string();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head.split(' ').nth(1).and_then(|code| code.parse().ok()).ok_or_else(|| std::io::Error::other(format!("no HTTP status in {head:?}")))?;
        Ok(Reply { status, headers: head.to_ascii_lowercase(), body: body.to_string() })
    }

    /// Opens a live-update stream and returns its status line's code,
    /// keeping the connection open (returned) when it was accepted.
    async fn open_stream(&self, path: &str) -> (u16, Option<TcpStream>) {
        let mut stream = self.connect().await.expect("SOCKS connect");
        let request = format!("GET {path} HTTP/1.1\r\nHost: {}\r\nAccept: text/event-stream\r\n\r\n", self.onion);
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        tokio::time::timeout(Duration::from_secs(90), reader.read_line(&mut status_line)).await.expect("stream answer timed out").unwrap();
        let status: u16 = status_line.split(' ').nth(1).and_then(|code| code.parse().ok()).expect("status line");
        (status, (status == 200).then(|| reader.into_inner()))
    }
}

struct Store {
    pk: String,
    order_id: String,
}

async fn start_monokulo(engine_addr: SocketAddr) -> (AppState, SocketAddr, Store) {
    let engine_client = EngineClient::new(format!("http://{engine_addr}"));
    let tenant = engine_client
        .create_tenant(CreateTenantRequest {
            view_key_hex: TEST_VIEW_KEY_HEX.to_string(),
            spend_pubkey_hex: TEST_SPEND_PUBKEY_HEX.to_string(),
            network: Some("mainnet".to_string()),
            confirmations_required: None,
            order_expiry_seconds: Some(3600),
        })
        .await
        .expect("creating the test tenant");
    let order = engine_client.create_order(&tenant.secret_token, 1_000_000, None, None).await.expect("creating the test order");
    let db = Db::open_in_memory().unwrap();
    db.create_user("u1", "tor@example.com", "x", false, 0).unwrap();
    db.create_store_connection(
        "store-1",
        "u1",
        "custom",
        "https://shop.example",
        &tenant.public_key,
        &monokulo::crypto::encrypt(&ENCRYPTION_KEY, &tenant.secret_token),
        &format!("http://{engine_addr}"),
        0,
        "XMR",
    )
    .unwrap();
    let config = AbuseConfig { soft_per_min: SOFT, hard_per_min: HARD, stream_cap: STREAM_CAP, challenge_bits: BITS, ..Default::default() };
    let state = AppState {
        db: db.into_shared(),
        engine_client,
        encryption_key: ENCRYPTION_KEY,
        status_cache: monokulo::http::status_page::new_status_cache(),
        exchange_rate: Arc::new(monokulo::exchange_rate_config::ExchangeRateProviders::xmr_only()),
        abuse: Arc::new(AbuseProtection::new(config)),
        dns: Arc::new(monokulo::embed_domains::UnavailableDns("no DNS in tests".to_string())),
    };
    let listener = OnionListener::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.bound_address();
    let router = build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router.into_make_service_with_connect_info::<OnionPeer>()).await.unwrap();
    });
    (state, addr, Store { pk: tenant.public_key, order_id: order.order_id })
}

fn circuits(state: &AppState) -> Vec<u32> {
    state
        .abuse
        .limiter
        .clients()
        .into_iter()
        .map(|client| match client {
            ClientIdentity::Circuit(id) => id,
            other => panic!("every client over Tor must be a circuit, got {other:?}"),
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a real tor and the live Tor network; see the module doc comment"]
async fn tor_visitors_are_told_apart_by_circuit_and_only_the_abusive_one_is_slowed() {
    let engine = scanner_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
    let (state, onion_listener, store) = start_monokulo(engine.addr).await;
    let tor = start_tor(onion_listener.port()).await;
    println!("tor data and log in {}", tor.dir.display());

    // -- bootstrap ---------------------------------------------------------
    let mut control = Control::connect(&tor).await;
    let deadline = Instant::now() + BOOTSTRAP_TIMEOUT;
    loop {
        let reply = control.command("GETINFO status/bootstrap-phase").await;
        if reply.contains("PROGRESS=100") {
            break;
        }
        assert!(Instant::now() < deadline, "tor did not finish bootstrapping within {BOOTSTRAP_TIMEOUT:?}: {reply}");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("tor bootstrapped");

    // -- tor accepted the settings from deploy/tor/torrc.snippet ------------
    let options = control.command("GETCONF HiddenServiceOptions").await;
    for expected in [
        "HiddenServiceExportCircuitID=haproxy",
        "HiddenServicePoWDefensesEnabled=1",
        "HiddenServiceEnableIntroDoSDefense=1",
        "HiddenServiceMaxStreams=64",
        "HiddenServiceMaxStreamsCloseCircuit=1",
    ] {
        assert!(options.contains(expected), "tor did not accept {expected}: {options}");
    }
    let modules = std::process::Command::new("tor").arg("--list-modules").output().unwrap();
    assert!(String::from_utf8_lossy(&modules.stdout).contains("pow: yes"), "this tor lacks the proof-of-work module");

    let hostname = std::fs::read_to_string(tor.dir.join("hs/hostname")).expect("the onion service has a hostname").trim().to_string();
    println!("onion service: {hostname}");
    let visitor = |name: &'static str| Visitor { name, socks: tor.socks, onion: hostname.clone() };
    let (a, b, c, d) = (visitor("visitor-a"), visitor("visitor-b"), visitor("visitor-c"), visitor("visitor-d"));
    let status_path = format!("/pay/{}/orders/{}/status", store.pk, store.order_id);
    let events_path = format!("/pay/{}/orders/{}/events", store.pk, store.order_id);

    // -- wait until the descriptor is published and reachable ---------------
    let deadline = Instant::now() + REACHABLE_TIMEOUT;
    let first = loop {
        match a.get(&status_path, "").await {
            Ok(reply) => break reply,
            Err(e) => {
                assert!(Instant::now() < deadline, "the onion service was not reachable within {REACHABLE_TIMEOUT:?}: {e}");
                println!("not reachable yet ({e}), retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };
    assert_eq!(first.status, 200, "{}", first.body);
    println!("reachable over Tor");

    // -- one circuit per visitor --------------------------------------------
    assert_eq!(b.get(&status_path, "").await.unwrap().status, 200);
    let seen = circuits(&state);
    assert_eq!(seen.len(), 2, "two visitors, two circuits: {seen:?}");
    assert_ne!(seen[0], seen[1]);

    // -- visitor A past the soft limit is challenged; B is not ---------------
    // Normally the 6th request; a few spare in case a minute boundary
    // passes mid-test (the count is a rolling minute).
    let mut challenged = None;
    for _ in 0..SOFT + 3 {
        let reply = a.get(&status_path, "").await.unwrap();
        if reply.status == 429 {
            challenged = Some(reply);
            break;
        }
        assert_eq!(reply.status, 200);
    }
    let challenged = challenged.expect("visitor A is challenged past the soft limit");
    assert!(challenged.headers.contains("monokulo-challenge:"), "{}", challenged.headers);
    let body: serde_json::Value = serde_json::from_str(&challenged.body).unwrap();
    let challenge = body["challenge"]["challenge"].as_str().unwrap().to_string();
    assert_eq!(b.get(&status_path, "").await.unwrap().status, 200, "the other visitor is unaffected");

    // A solves it (as monokulo-client.js would) and gets through.
    let proof = format!("{challenge}.{}", monokulo::abuse::challenge::solve(&challenge, BITS));
    let solved = a.get(&status_path, &format!("Monokulo-Proof: {proof}\r\n")).await.unwrap();
    assert_eq!(solved.status, 200, "the proof is accepted: {}", solved.body);

    // -- past the hard limit only A gets 429 + Retry-After -------------------
    let mut blocked = None;
    for _ in 0..HARD + 3 {
        let reply = a.get(&status_path, "").await.unwrap();
        if reply.status == 429 {
            blocked = Some(reply);
            break;
        }
    }
    let blocked = blocked.expect("visitor A reaches the hard limit");
    assert!(blocked.headers.contains("retry-after:"), "{}", blocked.headers);
    assert!(!blocked.headers.contains("monokulo-challenge:"), "nothing to solve past the hard limit");
    assert_eq!(b.get(&status_path, "").await.unwrap().status, 200, "the other visitor is still unaffected");

    // -- open live-update streams are capped per (circuit, store) -----------
    let mut held = Vec::new();
    for n in 0..STREAM_CAP {
        let (status, stream) = c.open_stream(&events_path).await;
        assert_eq!(status, 200, "stream {n} of visitor C");
        held.push(stream);
    }
    assert_eq!(c.open_stream(&events_path).await.0, 429, "one stream too many for visitor C's circuit");
    assert_eq!(d.open_stream(&events_path).await.0, 200, "another circuit has its own allowance");
    drop(held);

    assert_eq!(circuits(&state).len(), 4, "four visitors, four circuits");
    println!("PASS");
}
