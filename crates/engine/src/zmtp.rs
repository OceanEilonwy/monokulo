//! The subscribing end of ZMQ's wire protocol, ZMTP 3.0
//! (rfc.zeromq.org/spec/23), as much of it as listening to monerod's
//! `--zmq-pub` takes: a SUB socket with the NULL security mechanism, over
//! TCP or a Unix socket, connected to one publisher.
//!
//! monerod's publisher is libzmq 4.x. This side speaks 3.0 rather than 3.1
//! (rfc.zeromq.org/spec/37) so that subscriptions go as messages starting
//! with a `0x01` byte, the form every libzmq 4 PUB and XPUB accepts; a
//! peer speaking a later minor version downgrades to the one advertised.
//!
//! Everything read comes from the network, so every length is checked
//! before it is used and nothing is allocated ahead of the bytes that fill
//! it. What is received only ever wakes a scan loop (`node_events`), so a
//! peer that breaks the protocol just ends the connection.

use std::io;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

/// The largest frame accepted. Only monerod's `json-minimal-*` topics are
/// subscribed to, and libzmq's PUB filters by subscription before sending,
/// so the large `json-full-*` messages never arrive. The biggest minimal
/// ones are a long reorg's `chain_main` (67 bytes a block id) and a burst of
/// `txpool_add` (about 100 bytes a transaction): 64 MiB holds a million
/// block ids or over half a million transactions, far past any reorg or
/// burst a node sees, while bounding what a broken or hostile peer can make
/// the engine hold. A frame is read as its bytes arrive, never allocated
/// from the size it declares.
pub(crate) const MAX_FRAME: u64 = 64 << 20;

/// How long a publisher has to finish the handshake once connected to. A
/// listener that accepts and then says nothing would otherwise leave the
/// subscriber waiting forever, never retrying.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// A frame's flags (spec 23, "Framing"): more frames of the same message
/// follow, the size is 8 bytes rather than 1, and the frame is a command.
const MORE: u8 = 0x01;
const LONG: u8 = 0x02;
const COMMAND: u8 = 0x04;

/// Something both ends can read and write: a TCP or Unix socket.
pub(crate) trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}

/// A connection to a publisher, subscribed and past the handshake.
pub(crate) struct Subscriber<S> {
    stream: S,
}

/// Connects to `endpoint` (`tcp://host:port` or `ipc:///path`, as given to
/// monerod's `--zmq-pub`) and subscribes to every message starting with one
/// of `topics`.
pub(crate) async fn connect(
    endpoint: &str,
    topics: &[&str],
) -> io::Result<Subscriber<Box<dyn Transport>>> {
    let stream: Box<dyn Transport> = if let Some(address) = endpoint.strip_prefix("tcp://") {
        let stream = tokio::net::TcpStream::connect(address).await?;
        // Subscriptions are a few small writes, each worth sending at once.
        stream.set_nodelay(true)?;
        Box::new(stream)
    } else if let Some(path) = endpoint.strip_prefix("ipc://") {
        ipc(path).await?
    } else {
        return Err(invalid(format!("{endpoint} is not tcp:// or ipc://")));
    };
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, Subscriber::start(stream, topics)).await {
        Ok(started) => started,
        Err(_elapsed) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the publisher did not finish the ZMTP handshake",
        )),
    }
}

#[cfg(unix)]
async fn ipc(path: &str) -> io::Result<Box<dyn Transport>> {
    Ok(Box::new(tokio::net::UnixStream::connect(path).await?))
}

#[cfg(not(unix))]
async fn ipc(_path: &str) -> io::Result<Box<dyn Transport>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "ipc:// needs Unix sockets, which this platform lacks",
    ))
}

impl<S: AsyncRead + AsyncWrite + Unpin> Subscriber<S> {
    /// The handshake over an open `stream`, then the subscriptions: our
    /// greeting, theirs, our READY, theirs. Each step waits for the one
    /// before it, so nothing reaches libzmq before it can take it.
    async fn start(mut stream: S, topics: &[&str]) -> io::Result<Self> {
        stream.write_all(&greeting()).await?;
        let mut theirs = [0; 64];
        stream.read_exact(&mut theirs).await?;
        check_greeting(&theirs)?;
        stream.write_all(&ready()).await?;
        let mut subscriber = Self { stream };
        let (name, data) = subscriber.command().await?;
        match name.as_slice() {
            b"READY" => check_ready(&data)?,
            b"ERROR" => return Err(refused(&data)),
            _ => return Err(invalid("the publisher's first command is not READY")),
        }
        for topic in topics {
            subscriber.stream.write_all(&subscribe(topic)).await?;
        }
        subscriber.stream.flush().await?;
        Ok(subscriber)
    }

    /// The first frame of the next message. monerod sends each as one
    /// frame, `<topic>:<json>`; the frames after the first of a longer
    /// message are read past without being kept. Commands between messages
    /// are answered (PING) or ignored, as the spec has a peer do with ones
    /// it does not use. An error, or the end of the stream, ends the
    /// connection.
    pub(crate) async fn recv(&mut self) -> io::Result<Vec<u8>> {
        loop {
            let (flags, size) = self.header().await?;
            if flags & COMMAND != 0 {
                let body = self.body(size).await?;
                let (name, data) = command(&body)?;
                match name {
                    b"PING" => self.pong(data).await?,
                    b"ERROR" => return Err(refused(data)),
                    _ => {}
                }
                continue;
            }
            let first = self.body(size).await?;
            let mut more = flags & MORE != 0;
            while more {
                let (flags, size) = self.header().await?;
                if flags & COMMAND != 0 {
                    return Err(invalid("a command inside a multi-frame message"));
                }
                let skipped =
                    tokio::io::copy(&mut (&mut self.stream).take(size), &mut tokio::io::sink())
                        .await?;
                if skipped != size {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                more = flags & MORE != 0;
            }
            return Ok(first);
        }
    }

    /// A frame's flags and size, checked against what this side accepts.
    async fn header(&mut self) -> io::Result<(u8, u64)> {
        let flags = self.stream.read_u8().await?;
        if flags & !(MORE | LONG | COMMAND) != 0 {
            return Err(invalid(format!(
                "frame flags {flags:#04x} use reserved bits"
            )));
        }
        if flags & COMMAND != 0 && flags & MORE != 0 {
            return Err(invalid("a command frame with more frames after it"));
        }
        let size = if flags & LONG == 0 {
            u64::from(self.stream.read_u8().await?)
        } else {
            self.stream.read_u64().await?
        };
        if size > MAX_FRAME {
            return Err(invalid(format!(
                "a frame of {size} bytes, over the {MAX_FRAME} accepted"
            )));
        }
        Ok((flags, size))
    }

    /// `size` bytes, read as they arrive: a size alone never allocates.
    async fn body(&mut self, size: u64) -> io::Result<Vec<u8>> {
        let mut body = Vec::new();
        (&mut self.stream).take(size).read_to_end(&mut body).await?;
        if body.len() as u64 != size {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(body)
    }

    /// The next frame, which must be a command: its name and data.
    async fn command(&mut self) -> io::Result<(Vec<u8>, Vec<u8>)> {
        let (flags, size) = self.header().await?;
        if flags & COMMAND == 0 {
            return Err(invalid("a message before the handshake finished"));
        }
        let body = self.body(size).await?;
        let (name, data) = command(&body)?;
        Ok((name.to_vec(), data.to_vec()))
    }

    /// ZMTP 3.1's heartbeat, which a libzmq 4 publisher with a heartbeat
    /// interval sends whatever version it was told: unanswered, it would
    /// drop a subscriber that otherwise never writes.
    async fn pong(&mut self, ping: &[u8]) -> io::Result<()> {
        // A PING is a 2-byte TTL then up to 16 bytes of context, which the
        // PONG returns.
        let context = ping.get(2..).unwrap_or_default();
        if context.len() > 16 {
            return Err(invalid("a PING with over 16 bytes of context"));
        }
        let mut body = vec![4];
        body.extend_from_slice(b"PONG");
        body.extend_from_slice(context);
        self.stream.write_all(&frame(COMMAND, &body)).await?;
        self.stream.flush().await
    }
}

/// This side's greeting (spec 23, "Greeting"): the signature, version 3.0,
/// the NULL mechanism padded to 20 bytes, not the server, then filler.
fn greeting() -> [u8; 64] {
    let mut greeting = [0; 64];
    greeting[0] = 0xFF;
    greeting[9] = 0x7F;
    greeting[10] = 3;
    greeting[11] = 0;
    greeting[12..16].copy_from_slice(b"NULL");
    greeting
}

/// The publisher's greeting: ZMTP 3 or later (a later one downgrades to
/// ours), with the NULL mechanism this side speaks. The signature's padding
/// is not read, as libzmq does not read it either; only the low bit of the
/// tenth byte marks a ZMTP 2 or later peer.
fn check_greeting(greeting: &[u8; 64]) -> io::Result<()> {
    if greeting[0] != 0xFF || greeting[9] & 0x01 == 0 {
        return Err(invalid("the publisher's greeting is not ZMTP's"));
    }
    if greeting[10] < 3 {
        return Err(invalid(format!(
            "the publisher speaks ZMTP {}, not 3",
            greeting[10]
        )));
    }
    let mechanism = &greeting[12..32];
    if mechanism[..4] != *b"NULL" || mechanism[4..].iter().any(|&b| b != 0) {
        return Err(invalid(format!(
            "the publisher wants the {} security mechanism, not NULL",
            String::from_utf8_lossy(mechanism).trim_end_matches('\0')
        )));
    }
    Ok(())
}

/// One frame, with a short size when it fits in a byte.
fn frame(flags: u8, body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(body.len() + 9);
    if let Ok(size) = u8::try_from(body.len()) {
        frame.push(flags);
        frame.push(size);
    } else {
        frame.push(flags | LONG);
        frame.extend_from_slice(&(body.len() as u64).to_be_bytes());
    }
    frame.extend_from_slice(body);
    frame
}

/// This side's READY command: its one property says it is a SUB socket,
/// which a PUB or XPUB peer checks before accepting it.
fn ready() -> Vec<u8> {
    let mut body = vec![5];
    body.extend_from_slice(b"READY");
    body.push(11);
    body.extend_from_slice(b"Socket-Type");
    body.extend_from_slice(&3u32.to_be_bytes());
    body.extend_from_slice(b"SUB");
    frame(COMMAND, &body)
}

/// A ZMTP 3.0 subscription: a one-frame message of `0x01` then the topic.
fn subscribe(topic: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(topic.len() + 1);
    body.push(1);
    body.extend_from_slice(topic.as_bytes());
    frame(0, &body)
}

/// A command's name and data: a one-byte name length, the name, the rest.
fn command(body: &[u8]) -> io::Result<(&[u8], &[u8])> {
    let (&length, rest) = body
        .split_first()
        .ok_or_else(|| invalid("an empty command"))?;
    let length = usize::from(length);
    if length == 0 || rest.len() < length {
        return Err(invalid("a command whose name overruns it"));
    }
    Ok(rest.split_at(length))
}

/// The publisher's READY: its properties must name a socket a SUB can
/// listen to, as libzmq checks of its own peers.
fn check_ready(mut properties: &[u8]) -> io::Result<()> {
    let mut socket_type = None;
    while let Some((&length, rest)) = properties.split_first() {
        let length = usize::from(length);
        let truncated = || invalid("the publisher's READY has a truncated property");
        let (name, rest) = rest.split_at_checked(length).ok_or_else(truncated)?;
        let (size, rest) = rest.split_at_checked(4).ok_or_else(truncated)?;
        let size = u32::from_be_bytes([size[0], size[1], size[2], size[3]]) as usize;
        let (value, rest) = rest.split_at_checked(size).ok_or_else(truncated)?;
        if length == 0 {
            return Err(invalid("the publisher's READY has a property with no name"));
        }
        // Property names are case-insensitive (spec 23, "Command Metadata").
        if name.eq_ignore_ascii_case(b"Socket-Type") {
            socket_type = Some(value);
        }
        properties = rest;
    }
    match socket_type {
        Some(b"PUB" | b"XPUB") => Ok(()),
        Some(other) => Err(invalid(format!(
            "the peer is a {} socket, not a publisher",
            String::from_utf8_lossy(other)
        ))),
        None => Err(invalid("the publisher's READY has no Socket-Type")),
    }
}

/// An ERROR command's reason: a one-byte length, then the text.
fn refused(data: &[u8]) -> io::Error {
    let reason = data
        .split_first()
        .and_then(|(&length, rest)| rest.get(..usize::from(length)))
        .unwrap_or_default();
    io::Error::new(
        io::ErrorKind::ConnectionRefused,
        format!(
            "the publisher refused the connection: {}",
            String::from_utf8_lossy(reason)
        ),
    )
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/verification/notifications/zmtp.rs"]
mod tests;
