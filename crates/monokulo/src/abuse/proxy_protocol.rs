//! The onion listener: a second, loopback-only listener for tor's onion
//! service, which prefixes every connection with a PROXY protocol v1 header
//! naming the Tor circuit it arrived on (`HiddenServiceExportCircuitID
//! haproxy` in `torrc`; see `deploy/tor/` and `docs/TOR.md`).
//!
//! From the tor manual: the header looks like
//! `PROXY TCP6 fc00:dead:beef:4dad::ffff:ffff ::1 65535 42\r\n`, and the
//! circuit's global identifier is the last 32 bits of the first (source)
//! address. Everything else in the header can be ignored.
//!
//! Only this listener reads a PROXY header, and it *requires* one: a
//! connection without a valid header is closed. The ordinary listener never
//! parses one - anyone could send `PROXY ...` to it, and hyper simply refuses
//! it as a malformed HTTP request - so a client can't pick its own identity.
//! The onion listener must be bound to loopback so only the local tor can
//! reach it (`validate_onion_listener`).

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use super::identity::{group_address, ClientIdentity};

/// A PROXY v1 header is at most 107 bytes including its `\r\n`.
const MAX_HEADER_LEN: usize = 107;

/// How long a connection on the onion listener gets to send its header.
/// Tor sends it immediately; anything slower is not tor.
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);

/// What a connection on the onion listener is, as request handlers see it
/// (`ConnectInfo<OnionPeer>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnionPeer {
    /// The source address from the PROXY header (for tor, the circuit
    /// encoded as `fc00:dead:beef:4dad::<id>`).
    pub source: IpAddr,
}

impl OnionPeer {
    /// The circuit, when the source address is one tor made up for it
    /// (inside `fc00::/16`); otherwise the source address itself, so a
    /// PROXY sender other than tor (e.g. haproxy on the same machine) still
    /// gives each real client its own identity.
    pub fn identity(&self) -> ClientIdentity {
        match self.source {
            IpAddr::V6(v6) if v6.segments()[0] == 0xfc00 => {
                let s = v6.segments();
                ClientIdentity::Circuit(((s[6] as u32) << 16) | s[7] as u32)
            }
            other => ClientIdentity::Address(group_address(other)),
        }
    }
}

/// Parses one PROXY protocol v1 header line (without its `\r\n`) into the
/// source address it names. `PROXY UNKNOWN` (allowed by the spec for a
/// proxy that doesn't know the client) has no source and is refused, as is
/// anything malformed.
pub fn parse_v1_header(line: &str) -> Result<IpAddr, String> {
    let mut parts = line.split(' ');
    if parts.next() != Some("PROXY") {
        return Err("not a PROXY protocol header".to_string());
    }
    let family = parts.next().ok_or("missing protocol family")?;
    let fields: Vec<&str> = parts.collect();
    let [source, destination, source_port, destination_port] = fields[..] else {
        return Err(format!("expected 4 fields after {family}, got {}", fields.len()));
    };
    let source: IpAddr = source.parse().map_err(|_| format!("bad source address {source:?}"))?;
    let destination: IpAddr = destination.parse().map_err(|_| format!("bad destination address {destination:?}"))?;
    let family_matches = match family {
        "TCP4" => source.is_ipv4() && destination.is_ipv4(),
        "TCP6" => source.is_ipv6() && destination.is_ipv6(),
        other => return Err(format!("unsupported protocol family {other:?}")),
    };
    if !family_matches {
        return Err(format!("addresses don't match {family}"));
    }
    for port in [source_port, destination_port] {
        port.parse::<u16>().map_err(|_| format!("bad port {port:?}"))?;
    }
    Ok(source)
}

/// Reads exactly one PROXY v1 header off `stream`, byte by byte so nothing
/// after it (the HTTP request) is consumed.
async fn read_header(stream: &mut TcpStream) -> Result<IpAddr, String> {
    let mut line = Vec::with_capacity(MAX_HEADER_LEN);
    loop {
        let byte = stream.read_u8().await.map_err(|e| format!("connection ended before a PROXY header: {e}"))?;
        line.push(byte);
        if line.ends_with(b"\r\n") {
            let text = std::str::from_utf8(&line[..line.len() - 2]).map_err(|_| "PROXY header is not text".to_string())?;
            return parse_v1_header(text);
        }
        if line.len() >= MAX_HEADER_LEN {
            return Err("PROXY header too long".to_string());
        }
    }
}

/// The onion listener only makes sense on loopback: it trusts whatever
/// PROXY header arrives, so only the local tor may connect.
pub fn validate_onion_listener(value: &str) -> Result<Option<SocketAddr>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let address: SocketAddr = value.parse().map_err(|_| format!("{value:?} is not an address:port, e.g. 127.0.0.1:8082"))?;
    if !address.ip().is_loopback() {
        return Err(format!(
            "{value} is not a loopback address: the onion listener believes the PROXY header tor sends, so only \
             the local tor may reach it (use e.g. 127.0.0.1:8082)"
        ));
    }
    Ok(Some(address))
}

/// An `axum::serve` listener that accepts TCP connections, reads each one's
/// PROXY header on its own task (so one slow connection can't hold up the
/// others), and hands over only the connections whose header was valid.
pub struct OnionListener {
    local_addr: SocketAddr,
    ready: mpsc::Receiver<(TcpStream, OnionPeer)>,
}

impl OnionListener {
    pub async fn bind(address: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(address).await?;
        let local_addr = listener.local_addr()?;
        let (sender, ready) = mpsc::channel(256);
        tokio::spawn(async move {
            loop {
                let (mut stream, peer) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(e) => {
                        eprintln!("onion listener: accept failed: {e}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let connection_sender = sender.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(HEADER_TIMEOUT, read_header(&mut stream)).await {
                        Ok(Ok(source)) => {
                            let _ = connection_sender.send((stream, OnionPeer { source })).await;
                        }
                        Ok(Err(e)) => eprintln!("onion listener: refused a connection from {peer}: {e}"),
                        Err(_) => eprintln!("onion listener: refused a connection from {peer}: no PROXY header within {HEADER_TIMEOUT:?}"),
                    }
                });
                if sender.is_closed() {
                    return;
                }
            }
        });
        Ok(OnionListener { local_addr, ready })
    }
}

impl Listener for OnionListener {
    type Io = TcpStream;
    type Addr = OnionPeer;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(connection) => connection,
            // The acceptor task only ends if its listener is gone; nothing
            // more will ever arrive.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(OnionPeer { source: self.local_addr.ip() })
    }
}

impl Connected<IncomingStream<'_, OnionListener>> for OnionPeer {
    fn connect_info(stream: IncomingStream<'_, OnionListener>) -> Self {
        *stream.remote_addr()
    }
}

impl OnionListener {
    /// The address actually bound (useful when binding port 0 in tests).
    pub fn bound_address(&self) -> SocketAddr {
        self.local_addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tor_headers_parse_and_carry_the_circuit_in_the_source_address() {
        let source = parse_v1_header("PROXY TCP6 fc00:dead:beef:4dad::ffff:ffff ::1 65535 42").unwrap();
        assert_eq!(OnionPeer { source }.identity(), ClientIdentity::Circuit(0xffff_ffff));
        let source = parse_v1_header("PROXY TCP6 fc00:dead:beef:4dad::aabb:ccdd ::1 65535 42").unwrap();
        assert_eq!(OnionPeer { source }.identity(), ClientIdentity::Circuit(0xaabb_ccdd));
        let source = parse_v1_header("PROXY TCP6 fc00:dead:beef:4dad::0:29 ::1 65535 42").unwrap();
        assert_eq!(OnionPeer { source }.identity(), ClientIdentity::Circuit(41));
        // A non-tor PROXY sender: its real client address is the identity.
        let source = parse_v1_header("PROXY TCP4 198.51.100.7 127.0.0.1 51000 8082").unwrap();
        assert_eq!(OnionPeer { source }.identity(), ClientIdentity::Address("198.51.100.7".parse().unwrap()));
    }

    #[test]
    fn malformed_or_incomplete_headers_are_refused() {
        for bad in [
            "",
            "GET / HTTP/1.1",
            "PROXY UNKNOWN",
            "PROXY TCP4 1.2.3.4 5.6.7.8 1",
            "PROXY TCP4 1.2.3.4 5.6.7.8 1 2 3",
            "PROXY TCP4 ::1 ::1 1 2",
            "PROXY TCP6 1.2.3.4 ::1 1 2",
            "PROXY TCP4 1.2.3.4 5.6.7.8 70000 2",
            "PROXY TCP4 1.2.3.4 5.6.7.8 x 2",
            "PROXY TCP5 1.2.3.4 5.6.7.8 1 2",
            "proxy TCP4 1.2.3.4 5.6.7.8 1 2",
        ] {
            assert!(parse_v1_header(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn the_onion_listener_must_be_loopback() {
        assert_eq!(validate_onion_listener("").unwrap(), None);
        assert_eq!(validate_onion_listener("127.0.0.1:8082").unwrap(), Some("127.0.0.1:8082".parse().unwrap()));
        assert!(validate_onion_listener("[::1]:8082").unwrap().is_some());
        assert!(validate_onion_listener("0.0.0.0:8082").is_err());
        assert!(validate_onion_listener("192.168.1.2:8082").is_err());
        assert!(validate_onion_listener("localhost:8082").is_err());
    }
}
