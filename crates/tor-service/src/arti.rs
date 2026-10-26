//! The Arti side of Tor inside monokulo: joining the Tor network, the onion
//! service, and handing each visitor's stream to the router.

use std::pin::pin;
use std::str::FromStr;
use std::time::Duration;

use arti_client::config::TorClientConfigBuilder;
use arti_client::{TorClient, TorClientConfig};
use futures_util::StreamExt;
use safelog::DisplayRedacted;
use tor_cell::relaycell::msg::{Connected, End, EndReason};
use tor_hsservice::config::{OnionServiceConfigBuilder, TokenBucketConfig};
use tor_hsservice::status::State;
use tor_hsservice::{HsNickname, OnionServiceConfig, StreamRequest};
use tor_keymgr::config::CTorServiceKeystoreConfigBuilder;
use tor_keymgr::KeystoreId;
use tor_proto::stream::IncomingStreamRequest;

use super::{
    circuit_peer, Tor, TorConnections, TorListener, TorStatus, Wanted, CTOR_NICKNAME,
    HOSTNAME_FILE, NICKNAME,
};
use abuse::proxy_protocol::OnionPeer;
use service_runtime::time::now_unix;

/// How long after an error the service starts again.
const RETRY: Duration = Duration::from_secs(30);

/// Introductions the introduction points pass on, per second and in a
/// burst: `HiddenServiceEnableIntroDoSRatePerSec` and `...BurstPerSec` in
/// deploy/tor/torrc.snippet.
const INTRO_RATE: u32 = 25;
const INTRO_BURST: u32 = 200;

/// Streams one rendezvous circuit may have open; one more closes the
/// circuit (`HiddenServiceMaxStreams 64`, `...MaxStreamsCloseCircuit 1`).
const MAX_STREAMS_PER_CIRCUIT: u32 = 64;

/// The one port the service answers: monokulo's pages are plain HTTP over
/// Tor, which encrypts and authenticates the connection itself.
const PORT: u16 = 80;

/// How long fetching an onion shop's verification file may take, finding
/// the shop on Tor included.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// The running Tor client.
pub type Client = std::sync::Arc<TorClient<tor_rtcompat::PreferredRuntime>>;

/// Runs the onion service until `stopped` changes or its sender is dropped,
/// starting it again [`RETRY`] after each error.
pub async fn run(
    tor: Tor,
    wanted: Wanted,
    router: axum::Router,
    mut stopped: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        let error = tokio::select! {
            error = run_once(&tor, &wanted, router.clone()) => error,
            _ = stopped.changed() => return,
        };
        if stopped.has_changed().is_err() {
            return;
        }
        tracing::warn!(error = %error, retry = ?RETRY, "tor: the onion service stopped, starting it again");
        tor.set_status(TorStatus::Failed {
            error,
            since: now_unix(),
        });
        tokio::select! {
            () = tokio::time::sleep(RETRY) => {}
            _ = stopped.changed() => return,
        }
        tor.set_status(TorStatus::Starting { percent: 0 });
    }
}

/// Joins the Tor network, launches the service and serves it; returns only
/// with what went wrong. Dropping it stops everything it started.
async fn run_once(tor: &Tor, wanted: &Wanted, router: axum::Router) -> String {
    match serve_service(tor, wanted, router).await {
        Ok(()) => "Tor stopped handing over visitors.".to_string(),
        Err(error) => error,
    }
}

/// Arti's settings and the onion service's for `wanted`.
pub(super) fn configs(wanted: &Wanted) -> Result<(TorClientConfig, OnionServiceConfig), String> {
    let nickname = match wanted.hidden_service_dir {
        Some(_) => CTOR_NICKNAME,
        None => NICKNAME,
    };
    let nickname = HsNickname::new(nickname.to_string()).map_err(|e| e.to_string())?;
    let mut config = TorClientConfigBuilder::from_directories(
        wanted.state_dir.join("state"),
        wanted.state_dir.join("cache"),
    );
    if let Some(dir) = &wanted.hidden_service_dir {
        // The external tor's key, read where it is, under a nickname Arti's
        // own key store has nothing for, so this key is the only one found.
        let mut ctor = CTorServiceKeystoreConfigBuilder::default();
        ctor.id(KeystoreId::from_str("ctor").map_err(|e| e.to_string())?)
            .path(dir.clone())
            .nickname(nickname.clone());
        config.storage().keystore().ctor_service(ctor);
    }
    let config = config
        .build()
        .map_err(|e| format!("Tor's settings were refused: {e}"))?;
    let service = OnionServiceConfigBuilder::default()
        .nickname(nickname)
        .rate_limit_at_intro(Some(TokenBucketConfig::new(INTRO_RATE, INTRO_BURST)))
        .max_concurrent_streams_per_circuit(MAX_STREAMS_PER_CIRCUIT)
        .build()
        .map_err(|e| format!("The onion service's settings were refused: {e}"))?;
    Ok((config, service))
}

/// The onion address the service will have with `wanted`, read from its
/// key without joining the network; `None` while there is no key yet and
/// Arti will make one. With `tor.hidden_service_dir` there must be a key,
/// so Arti never makes one in its place, and its address must be the one
/// in that folder's own `hostname` file, when it has one.
pub(super) fn address_to_serve(wanted: &Wanted) -> Result<Option<String>, String> {
    let (config, service) = configs(wanted)?;
    let address = TorClient::<tor_rtcompat::PreferredRuntime>::create_onion_service(
        &config, service,
    )
    .map(|service| {
        service
            .onion_address()
            .map(|address| address.display_unredacted().to_string())
    });
    let Some(dir) = &wanted.hidden_service_dir else {
        return address.map_err(|e| format!("The onion service's key couldn't be read: {e}"));
    };
    let unreadable = |why: String| {
        format!(
            "Tor can't read the key in {}{why}: give the folder tor's HiddenServiceDir points at, as a copy only monokulo's user can open (chmod 700).",
            dir.display()
        )
    };
    let address = match address {
        Ok(Some(address)) => address,
        Ok(None) => return Err(unreadable(String::new())),
        Err(e) => return Err(unreadable(format!(" ({e})"))),
    };
    match std::fs::read_to_string(dir.join(HOSTNAME_FILE)) {
        Ok(hostname) if !hostname.trim().eq_ignore_ascii_case(&address) => Err(format!(
            "The key in {} is the one for {address}, but its hostname file says {}: give the HiddenServiceDir of the address to serve.",
            dir.display(),
            hostname.trim()
        )),
        _ => Ok(Some(address)),
    }
}

async fn serve_service(tor: &Tor, wanted: &Wanted, router: axum::Router) -> Result<(), String> {
    let (config, service) = configs(wanted)?;
    let client = TorClient::builder()
        .config(config)
        .create_unbootstrapped_async()
        .await
        .map_err(|e| format!("Tor couldn't start: {e}"))?;

    let progress = {
        let tor = tor.clone();
        let mut events = client.bootstrap_events();
        AbortOnDrop(tokio::spawn(async move {
            while let Some(status) = events.next().await {
                tracing::debug!(%status, "tor: joining the network");
                tor.set_status(TorStatus::Starting {
                    percent: (status.as_frac() * 100.0).clamp(0.0, 100.0) as u8,
                });
            }
        }))
    };
    client
        .bootstrap()
        .await
        .map_err(|e| format!("Tor couldn't join the network: {e}"))?;
    drop(progress);
    tor.set_client(Some(client.clone()));
    let _client = ClearOnDrop(tor.clone());

    // Checked again just before launching: the folder may have changed
    // since the settings were saved.
    let expected = address_to_serve(wanted)?;
    let (running, rend_requests) = client
        .launch_onion_service(service)
        .map_err(|e| format!("The onion service couldn't start: {e}"))?
        .ok_or("The onion service is turned off in Tor's settings.")?;
    let address = running
        .onion_address()
        .ok_or("The onion service has no address.")?
        .display_unredacted()
        .to_string();
    if let Some(expected) = expected.filter(|expected| *expected != address) {
        return Err(format!(
            "Tor came up at {address}, not at {expected}, the address of its key."
        ));
    }
    tor.set_status(TorStatus::Publishing {
        address: address.clone(),
    });
    tracing::info!(tor.address = %address, "tor: onion service started, publishing its address");
    // Where C tor keeps it too, for the operator and the OpenWrt app.
    let hostname = wanted.state_dir.join(HOSTNAME_FILE);
    if let Err(e) = std::fs::write(&hostname, format!("{address}\n")) {
        tracing::warn!(path = %hostname.display(), error = %e, "tor: couldn't write the onion address to a file");
    }

    let _status = {
        let tor = tor.clone();
        let address = address.clone();
        let mut events = running.status_events();
        AbortOnDrop(tokio::spawn(async move {
            while let Some(status) = events.next().await {
                tracing::debug!(state = ?status.state(), problem = ?status.current_problem(), "tor: onion service status");
                let problem = status
                    .current_problem()
                    .map_or_else(|| "Tor is retrying".to_string(), |p| format!("{p:?}"));
                let next = match status.state() {
                    State::Running | State::DegradedReachable => TorStatus::Running {
                        address: address.clone(),
                        since: match tor.status() {
                            TorStatus::Running { since, .. } => since,
                            _ => now_unix(),
                        },
                    },
                    State::Bootstrapping => TorStatus::Publishing {
                        address: address.clone(),
                    },
                    State::Shutdown => continue,
                    _ => TorStatus::Degraded {
                        address: address.clone(),
                        problem,
                    },
                };
                if matches!(next, TorStatus::Running { .. })
                    && !matches!(tor.status(), TorStatus::Running { .. })
                {
                    tracing::info!(tor.address = %address, "tor: onion service reachable");
                }
                tor.set_status(next);
            }
        }))
    };

    let (connections, listener) = TorListener::new();
    let (_stop_serving, serving_stopped) = tokio::sync::watch::channel(false);
    let _server = AbortOnDrop(tokio::spawn(async move {
        if let Err(e) = super::serve(listener, router, serving_stopped).await {
            tracing::error!(error = %e, "tor: serving the onion service failed");
        }
    }));

    // One identity per rendezvous circuit: a counter, as a C tor's
    // circuit id would be.
    let mut circuits: u32 = 0;
    let mut rend_requests = pin!(rend_requests);
    while let Some(rend) = rend_requests.next().await {
        circuits = circuits.wrapping_add(1);
        let peer = circuit_peer(circuits);
        let connections = connections.clone();
        tokio::spawn(async move {
            let Ok(mut streams) = rend.accept().await else {
                return;
            };
            while let Some(stream) = streams.next().await {
                tokio::spawn(hand_over(stream, peer, connections.clone()));
            }
        });
    }
    drop(running);
    Ok(())
}

/// Accepts a stream to port 80 and gives it to the server; refuses any
/// other port.
async fn hand_over(request: StreamRequest, peer: OnionPeer, connections: TorConnections) {
    let port = match request.request() {
        IncomingStreamRequest::Begin(begin) => begin.port(),
        _ => 0,
    };
    if port != PORT {
        let _ = request.reject(End::new_with_reason(EndReason::DONE)).await;
        return;
    }
    match request.accept(Connected::new_empty()).await {
        Ok(stream) => {
            connections.send(stream, peer).await;
        }
        Err(e) => tracing::debug!(error = %e, "tor: a visitor's stream closed before it opened"),
    }
}

/// `path` on `domain` (an onion shop's verification file, monokulo's
/// `embed_domains::WELL_KNOWN_PATH`), fetched over Tor on a circuit of its
/// own: `Ok(None)` for any answer but a 200 (redirects aren't followed) or
/// one bigger than `max_bytes` (`embed_domains::WELL_KNOWN_MAX_BYTES`).
pub async fn fetch(
    client: Client,
    domain: &str,
    path: &str,
    max_bytes: usize,
) -> Result<Option<String>, String> {
    use http_body_util::BodyExt;
    let fetch = async {
        let mut prefs = arti_client::StreamPrefs::new();
        prefs.connect_to_onion_services(arti_client::config::BoolOrAuto::Explicit(true));
        prefs.new_isolation_group();
        let stream = client
            .connect_with_prefs((domain, 80), &prefs)
            .await
            .map_err(|e| format!("the shop couldn't be reached on Tor ({e})"))?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .map_err(|e| format!("the shop didn't answer HTTP ({e})"))?;
        let connection = AbortOnDrop(tokio::spawn(async move {
            let _ = connection.await;
        }));
        let request = axum::http::Request::get(path)
            .header(axum::http::header::HOST, domain)
            .header(axum::http::header::USER_AGENT, "monokulo")
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .map_err(|e| e.to_string())?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|e| format!("the shop didn't answer ({e})"))?;
        if response.status() != axum::http::StatusCode::OK {
            drop(connection);
            return Ok(None);
        }
        let body = http_body_util::Limited::new(response.into_body(), max_bytes)
            .collect()
            .await;
        drop(connection);
        Ok(body
            .ok()
            .map(|body| String::from_utf8_lossy(&body.to_bytes()).into_owned()))
    };
    tokio::time::timeout(FETCH_TIMEOUT, fetch)
        .await
        .map_err(|_| {
            format!(
                "the shop didn't answer within {} seconds",
                FETCH_TIMEOUT.as_secs()
            )
        })?
}

/// Forgets the Tor client when the service it came with stops.
struct ClearOnDrop(Tor);

impl Drop for ClearOnDrop {
    fn drop(&mut self) {
        self.0.set_client(None);
    }
}

/// A task that ends when this is dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
