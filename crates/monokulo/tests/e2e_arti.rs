//! A real end-to-end check of Tor inside monokulo (`monokulo::tor`), on the
//! live Tor network. Ignored by default (like `e2e_tor.rs`): run it with
//!
//! ```sh
//! cargo nextest run -p monokulo --test e2e_arti --run-ignored only --no-capture
//! ```
//!
//! Needs `tor` on `PATH` (only as the visitor's client and to host a pretend
//! onion shop) and outbound network access. Publishing two fresh onion
//! services and reaching them takes a minute or two.
//!
//! What it does:
//!
//! 1. Starts a pretend onion shop: a local HTTP server serving
//!    `/.well-known/monokulo-verify`, published as an onion service by a
//!    real `tor`, which also gives the test a `SocksPort` to visit through.
//! 2. Starts Tor inside monokulo the way saving `tor.mode = inside` does
//!    (`TorReloadable`), in front of monokulo's real router.
//! 3. Reaches monokulo's onion address through the `SocksPort` and checks
//!    the page came as a visitor on Tor sees it (`Referrer-Policy:
//!    no-referrer`, no `Onion-Location`).
//! 4. Fetches the shop's verification file over Tor inside monokulo
//!    (`WellKnownOverTor`), as verifying an onion shop does.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use live_settings::Reloadable as _;
use monokulo::embed_domains::WellKnownFetch as _;
use monokulo::http::{build_router, AppState};
use monokulo::settings::{TorConfig, TorMode};
use monokulo::tor::{Tor, TorReloadable, TorStatus, WellKnownOverTor};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_socks::tcp::Socks5Stream;

const TOKEN_LINE: &str = "monokulo-verify=e2e-arti-token";
const TIMEOUT: Duration = Duration::from_secs(420);

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn private_dir(path: &std::path::Path) {
    std::fs::create_dir_all(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

/// The pretend shop's web server: the verification file, and nothing else.
async fn serve_shop(port: u16) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut request = vec![0; 2048];
                let n = stream.read(&mut request).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..n]);
                let (status, body) = if request.starts_with("GET /.well-known/monokulo-verify ") {
                    ("200 OK", format!("{TOKEN_LINE}\n"))
                } else {
                    ("404 Not Found", String::new())
                };
                let _ = stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });
}

/// Retries `attempt` every 5 seconds until it answers or [`TIMEOUT`] runs out.
async fn until<T, F, Fut>(what: &str, mut attempt: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match attempt().await {
            Ok(value) => return value,
            Err(e) if Instant::now() < deadline => {
                println!("{what}: {e}, retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(e) => panic!("{what}: {e} (gave up after {TIMEOUT:?})"),
        }
    }
}

#[tokio::test]
#[ignore = "needs tor and the live Tor network"]
async fn live_tor_inside_monokulo_serves_its_onion_address_and_checks_onion_shops() {
    let run = std::env::temp_dir().join(format!("monokulo-e2e-arti-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&run);
    let (shop_dir, client_data) = (
        run.join("shop-hs"),
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("e2e-tor-data"),
    );
    private_dir(&shop_dir);
    private_dir(&client_data);

    // 1. The pretend shop, published by a real tor that is also our client.
    let shop_port = free_port();
    serve_shop(shop_port).await;
    let socks = free_port();
    std::fs::write(
        run.join("torrc"),
        format!(
            "DataDirectory {}\nSocksPort 127.0.0.1:{socks}\nHiddenServiceDir {}\nHiddenServicePort 80 127.0.0.1:{shop_port}\n",
            client_data.display(),
            shop_dir.display()
        ),
    )
    .unwrap();
    let _client = tokio::process::Command::new("tor")
        .arg("-f")
        .arg(run.join("torrc"))
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("could not start `tor` - is it installed and on PATH?");

    // 2. Tor inside monokulo, as saving tor.mode = inside starts it.
    let tor = Tor::default();
    tor.router_ready(build_router(AppState {
        tor: tor.clone(),
        ..AppState::for_tests()
    }));
    let reloadable = TorReloadable::new(tor.clone());
    let config = TorConfig {
        mode: TorMode::Inside,
        state_dir: run.join("monokulo-tor"),
        ..TorConfig::defaults()
    };
    let (change, _) = reloadable
        .prepare(&config, &TorConfig::defaults())
        .await
        .expect("preparing Tor inside monokulo");
    reloadable.install(change).await;
    let onion = until("Tor inside monokulo's address", || {
        let tor = tor.clone();
        async move {
            match tor.status() {
                TorStatus::Failed { error, .. } => panic!("Tor inside monokulo failed: {error}"),
                status => status
                    .address()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{status:?}")),
            }
        }
    })
    .await;
    println!("monokulo is at {onion}");
    let written =
        std::fs::read_to_string(config.state_dir.join(monokulo::tor::HOSTNAME_FILE)).unwrap();
    assert_eq!(written.trim(), onion);

    // 3. A visitor on Tor reaches it, and gets what a visitor on Tor gets.
    let response = until("reaching monokulo over Tor", || {
        let onion = onion.clone();
        async move {
            let mut stream = tokio::time::timeout(
                Duration::from_secs(90),
                Socks5Stream::connect(("127.0.0.1", socks), (onion.as_str(), 80)),
            )
            .await
            .map_err(|_| "SOCKS connect timed out".to_string())?
            .map_err(|e| e.to_string())?;
            stream
                .write_all(
                    format!("GET /login HTTP/1.1\r\nHost: {onion}\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .map_err(|e| e.to_string())?;
            let mut response = String::new();
            stream
                .read_to_string(&mut response)
                .await
                .map_err(|e| e.to_string())?;
            Ok(response)
        }
    })
    .await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let lower = response.to_lowercase();
    assert!(
        lower.contains("referrer-policy: no-referrer\r\n"),
        "{response}"
    );
    assert!(!lower.contains("onion-location"), "{response}");

    // 4. The shop's file, fetched over Tor inside monokulo.
    let shop = std::fs::read_to_string(shop_dir.join("hostname"))
        .expect("the shop's tor wrote its hostname")
        .trim()
        .to_string();
    let fetch = WellKnownOverTor(tor.clone());
    let text = until("fetching the shop's file over Tor", || {
        let (fetch, shop) = (&fetch, shop.clone());
        async move { fetch.fetch(&shop).await }
    })
    .await;
    assert!(
        text.is_some_and(|text| text.lines().any(|line| line.trim() == TOKEN_LINE)),
        "the shop's file holds the token"
    );

    reloadable.install(monokulo::tor::TorChange::Stop).await;
    assert_eq!(tor.status(), TorStatus::Off);
    let _ = std::fs::remove_dir_all(&run);
}
