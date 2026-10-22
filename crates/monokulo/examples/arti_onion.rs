//! Spike: run monokulo's onion service with Arti embedded in the process,
//! no tor daemon.
//!
//! cargo run -p monokulo --features arti --example arti_onion -- <state dir> <onion listener>
//!
//! The state directory keeps the service's identity key, so the onion
//! address stays the same across runs. Every stream a visitor opens to port
//! 80 is forwarded to monokulo's onion listener (`abuse.onion_listener`)
//! with the PROXY v1 header tor's `HiddenServiceExportCircuitID haproxy`
//! sends, so the listener and the per-circuit limits work unchanged. Arti
//! gives no circuit number of its own; each accepted rendezvous circuit gets
//! the next one from a counter.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use arti_client::config::TorClientConfigBuilder;
use arti_client::TorClient;
use futures_util::StreamExt;
use safelog::DisplayRedacted;
use tokio::io::AsyncWriteExt;
use tor_cell::relaycell::msg::{Connected, End, EndReason};
use tor_hsservice::config::OnionServiceConfigBuilder;
use tor_hsservice::{HsNickname, StreamRequest};
use tor_proto::stream::IncomingStreamRequest;

static NEXT_CIRCUIT: AtomicU32 = AtomicU32::new(1);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().ok_or("state dir")?);
    let listener: SocketAddr = args.next().ok_or("onion listener")?.parse()?;

    let started = std::time::Instant::now();
    let config =
        TorClientConfigBuilder::from_directories(dir.join("state"), dir.join("cache")).build()?;
    let client = TorClient::create_bootstrapped(config).await?;
    eprintln!("bootstrapped in {:?}", started.elapsed());

    let service = OnionServiceConfigBuilder::default()
        .nickname(HsNickname::new("monokulo".into())?)
        .build()?;
    let (running, rend_requests) = client
        .launch_onion_service(service)?
        .ok_or("onion service disabled")?;
    let address = running.onion_address().ok_or("no onion address")?;
    eprintln!("onion address: {}", address.display_unredacted());

    let mut rend_requests = Box::pin(rend_requests);
    while let Some(rend) = rend_requests.next().await {
        let circuit = NEXT_CIRCUIT.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            let Ok(mut streams) = rend.accept().await else {
                return;
            };
            while let Some(stream) = streams.next().await {
                tokio::spawn(forward(stream, circuit, listener));
            }
        });
    }
    drop(running);
    Ok(())
}

async fn forward(request: StreamRequest, circuit: u32, listener: SocketAddr) {
    let port = match request.request() {
        IncomingStreamRequest::Begin(begin) => begin.port(),
        _ => 0,
    };
    if port != 80 {
        let _ = request.reject(End::new_with_reason(EndReason::DONE)).await;
        return;
    }
    let Ok(mut upstream) = tokio::net::TcpStream::connect(listener).await else {
        let _ = request
            .reject(End::new_with_reason(EndReason::CONNECTREFUSED))
            .await;
        return;
    };
    // The header tor writes (see `abuse::proxy_protocol`).
    let header = format!(
        "PROXY TCP6 fc00:dead:beef:4dad::{:x}:{:x} ::1 {} {port}\r\n",
        circuit >> 16,
        circuit & 0xffff,
        circuit & 0xffff,
    );
    if upstream.write_all(header.as_bytes()).await.is_err() {
        return;
    }
    let Ok(mut visitor) = request.accept(Connected::new_empty()).await else {
        return;
    };
    let _ = tokio::io::copy_bidirectional(&mut visitor, &mut upstream).await;
}
