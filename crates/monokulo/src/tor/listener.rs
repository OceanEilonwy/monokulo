//! The `axum::serve` listener Tor inside monokulo hands its streams to.

use std::io;
use std::net::{IpAddr, Ipv6Addr};

use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use crate::abuse::proxy_protocol::OnionPeer;

/// A visitor's connection, from Arti or from a test.
pub trait TorStream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> TorStream for T {}

/// How many accepted connections may wait for the server at once.
const QUEUE: usize = 256;

/// The sending side: each accepted connection, with the circuit it came on.
#[derive(Clone)]
pub struct TorConnections(mpsc::Sender<(Box<dyn TorStream>, OnionPeer)>);

/// The receiving side, served like any listener.
pub struct TorListener(mpsc::Receiver<(Box<dyn TorStream>, OnionPeer)>);

impl TorListener {
    /// A listener and the sender that feeds it.
    pub fn new() -> (TorConnections, TorListener) {
        let (sender, receiver) = mpsc::channel(QUEUE);
        (TorConnections(sender), TorListener(receiver))
    }
}

impl TorConnections {
    /// Hands `stream` to the server; `false` once the server has stopped.
    pub async fn send(&self, stream: impl TorStream, peer: OnionPeer) -> bool {
        self.0.send((Box::new(stream), peer)).await.is_ok()
    }
}

/// The identity a visitor on rendezvous circuit `circuit` has: the address
/// a C tor's `HiddenServiceExportCircuitID haproxy` would give it
/// (`fc00:dead:beef:4dad::<circuit>`), so it reads as that circuit
/// (`OnionPeer::identity`) whichever tor it came through.
pub fn circuit_peer(circuit: u32) -> OnionPeer {
    let [a, b, c, d] = circuit.to_be_bytes();
    OnionPeer {
        source: IpAddr::V6(Ipv6Addr::new(
            0xfc00,
            0xdead,
            0xbeef,
            0x4dad,
            0,
            0,
            u16::from_be_bytes([a, b]),
            u16::from_be_bytes([c, d]),
        )),
    }
}

impl Listener for TorListener {
    type Io = Box<dyn TorStream>;
    type Addr = OnionPeer;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.recv().await {
            Some(connection) => connection,
            // Every sender is gone: nothing more will ever arrive.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(OnionPeer {
            source: IpAddr::V6(Ipv6Addr::LOCALHOST),
        })
    }
}

impl Connected<IncomingStream<'_, TorListener>> for OnionPeer {
    fn connect_info(stream: IncomingStream<'_, TorListener>) -> Self {
        *stream.remote_addr()
    }
}
