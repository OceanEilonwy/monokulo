//! The Arti side of Tor inside monokulo: joining the Tor network, the onion
//! service, and handing each visitor's stream to the router.

use std::pin::pin;
use std::str::FromStr;
use std::time::Duration;

use arti_client::config::TorClientConfigBuilder;
use arti_client::TorClient;
use futures_util::StreamExt;
use safelog::DisplayRedacted;
use tor_cell::relaycell::msg::{Connected, End, EndReason};
use tor_hsservice::config::{OnionServiceConfigBuilder, TokenBucketConfig};
use tor_hsservice::status::State;
use tor_hsservice::{HsNickname, StreamRequest};
use tor_keymgr::config::CTorServiceKeystoreConfigBuilder;
use tor_keymgr::KeystoreId;
use tor_proto::stream::IncomingStreamRequest;

use super::{circuit_peer, Tor, TorConnections, TorListener, TorStatus, Wanted, NICKNAME};
use crate::abuse::proxy_protocol::OnionPeer;

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
            since: crate::now_unix(),
        });
        tokio::select! {
            () = tokio::time::sleep(RETRY) => {}
            _ = stopped.changed() => return,
        }
        tor.set_status(TorStatus::Starting {
            percent: 0,
            doing: "Starting again".to_string(),
        });
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

async fn serve_service(tor: &Tor, wanted: &Wanted, router: axum::Router) -> Result<(), String> {
    let nickname = HsNickname::new(NICKNAME.to_string()).map_err(|e| e.to_string())?;
    let mut config = TorClientConfigBuilder::from_directories(
        wanted.state_dir.join("state"),
        wanted.state_dir.join("cache"),
    );
    if let Some(dir) = &wanted.hidden_service_dir {
        // The external tor's key, read where it is: Arti finds the
        // service's key there before making one of its own.
        let mut ctor = CTorServiceKeystoreConfigBuilder::default();
        ctor.id(KeystoreId::from_str("ctor").map_err(|e| e.to_string())?)
            .path(dir.clone())
            .nickname(nickname.clone());
        config.storage().keystore().ctor_service(ctor);
    }
    let config = config
        .build()
        .map_err(|e| format!("Tor's settings were refused: {e}"))?;
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
                tor.set_status(TorStatus::Starting {
                    percent: (status.as_frac() * 100.0).clamp(0.0, 100.0) as u8,
                    doing: status.to_string(),
                });
            }
        }))
    };
    client
        .bootstrap()
        .await
        .map_err(|e| format!("Tor couldn't join the network: {e}"))?;
    drop(progress);

    let service = OnionServiceConfigBuilder::default()
        .nickname(nickname)
        .rate_limit_at_intro(Some(TokenBucketConfig::new(INTRO_RATE, INTRO_BURST)))
        .max_concurrent_streams_per_circuit(MAX_STREAMS_PER_CIRCUIT)
        .build()
        .map_err(|e| format!("The onion service's settings were refused: {e}"))?;
    let (running, rend_requests) = client
        .launch_onion_service(service)
        .map_err(|e| format!("The onion service couldn't start: {e}"))?
        .ok_or("The onion service is turned off in Tor's settings.")?;
    let address = running
        .onion_address()
        .ok_or("The onion service has no address.")?
        .display_unredacted()
        .to_string();
    tor.set_status(TorStatus::Starting {
        percent: 100,
        doing: "Publishing the onion address".to_string(),
    });
    tracing::info!(tor.address = %address, "tor: onion service started, publishing its address");

    let _status = {
        let tor = tor.clone();
        let address = address.clone();
        let mut events = running.status_events();
        AbortOnDrop(tokio::spawn(async move {
            while let Some(status) = events.next().await {
                let problem = status
                    .current_problem()
                    .map_or_else(|| "Tor is retrying".to_string(), |p| format!("{p:?}"));
                let next = match status.state() {
                    State::Running | State::DegradedReachable => TorStatus::Running {
                        address: address.clone(),
                        since: match tor.status() {
                            TorStatus::Running { since, .. } => since,
                            _ => crate::now_unix(),
                        },
                    },
                    State::Bootstrapping => TorStatus::Starting {
                        percent: 100,
                        doing: "Publishing the onion address".to_string(),
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

/// A task that ends when this is dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
