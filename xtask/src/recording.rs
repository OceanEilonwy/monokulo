//! `cargo xtask record-stagenet-node`: the recording the engine's
//! `daemon_rpc_replay` test replays on every change. A proxy on a loopback
//! port forwards each request to the public stagenet node and keeps the
//! exchange, and the live test `live_stagenet_node_answers_as_recorded` is
//! pointed at it, so the recording holds exactly the calls that test makes
//! and the answers it checked. The file is rewritten only when the test
//! passes: a recording the assertions reject would fail every CI run.
//!
//! The node speaks plain HTTP/1.1, so the proxy needs nothing beyond the
//! standard library: each request is forwarded on a connection of its own,
//! closed by the node once it has answered.

use crate::support::{write_json, Exit};
use serde::Serialize;
use std::{
    io::{self, BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

pub(crate) const HELP: &str = "\
        record-stagenet-node\n\
                      Re-record crates/engine/tests/fixtures/stagenet_node_recording.json from the public\n\
                      stagenet node, through the live replay test; only needed when the node client's requests change";

/// The node the recording is made from, as the replay test's `NODE` names it.
const NODE: (&str, u16) = ("node2.monerodevs.org", 38089);
const FIXTURE: &str = "crates/engine/tests/fixtures/stagenet_node_recording.json";
const TEST: &str = "live_stagenet_node_answers_as_recorded";
/// A public node can be slow, but one that says nothing for this long has
/// dropped the request.
const NODE_TIMEOUT: Duration = Duration::from_secs(120);

/// One request and the node's answer, as the replay test reads them.
#[derive(Serialize, Clone, PartialEq, Debug)]
struct Exchange {
    path: String,
    request_hex: String,
    response_hex: String,
}

type Recorded = Arc<Mutex<Vec<Exchange>>>;

/// A request or response read off a connection: its first line, its
/// content type, and its body.
struct Message {
    first: String,
    content_type: Option<String>,
    body: Vec<u8>,
}

/// Reads one HTTP/1.1 message. `None` when the connection closed before a
/// new one began. A body is read by its `Content-Length`, or de-chunked, or
/// (a response on a connection the node closes) read to the end.
fn read_message(reader: &mut impl BufRead, to_end: bool) -> io::Result<Option<Message>> {
    let mut first = String::new();
    if reader.read_line(&mut first)? == 0 {
        return Ok(None);
    }
    let (mut length, mut chunked, mut content_type) = (None, false, None);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => {
                length = Some(value.parse::<usize>().map_err(io::Error::other)?);
            }
            "transfer-encoding" => chunked = value.eq_ignore_ascii_case("chunked"),
            "content-type" => content_type = Some(value.to_string()),
            _ => {}
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size)?;
            let size = usize::from_str_radix(size.trim().split(';').next().unwrap_or(""), 16)
                .map_err(io::Error::other)?;
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk)?;
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    } else if let Some(length) = length {
        body.resize(length, 0);
        reader.read_exact(&mut body)?;
    } else if to_end {
        reader.read_to_end(&mut body)?;
    }
    Ok(Some(Message {
        first: first.trim_end().to_string(),
        content_type,
        body,
    }))
}

/// Sends `request` to the node at `node` and returns its answer.
fn forward(node: (&str, u16), request: &Message) -> io::Result<Message> {
    let mut stream = TcpStream::connect(node)?;
    stream.set_read_timeout(Some(NODE_TIMEOUT))?;
    let (host, port) = node;
    let mut head = format!(
        "{}\r\nHost: {host}:{port}\r\nContent-Length: {}\r\nConnection: close\r\n",
        request.first,
        request.body.len()
    );
    if let Some(content_type) = &request.content_type {
        head.push_str(&format!("Content-Type: {content_type}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&request.body)?;
    read_message(&mut BufReader::new(stream), true)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the node closed without answering",
        )
    })
}

/// Answers the requests of one client connection by forwarding each to
/// `node`, recording every exchange, until the client hangs up.
fn serve(client: TcpStream, node: (&str, u16), recorded: &Recorded) -> io::Result<()> {
    let mut writer = client.try_clone()?;
    let mut reader = BufReader::new(client);
    while let Some(request) = read_message(&mut reader, false)? {
        let answer = forward(node, &request)?;
        let path = request
            .first
            .split_whitespace()
            .nth(1)
            .unwrap_or("")
            .split('?')
            .next()
            .unwrap_or("")
            .to_string();
        recorded
            .lock()
            .map_err(|_| io::Error::other("a proxy thread panicked"))?
            .push(Exchange {
                path,
                request_hex: hex::encode(&request.body),
                response_hex: hex::encode(&answer.body),
            });
        // The status line as the node gave it, under the client's HTTP version.
        let status = answer.first.split_once(' ').map_or("200 OK", |(_, s)| s);
        let mut head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\n",
            answer.body.len()
        );
        if let Some(content_type) = &answer.content_type {
            head.push_str(&format!("Content-Type: {content_type}\r\n"));
        }
        head.push_str("\r\n");
        writer.write_all(head.as_bytes())?;
        writer.write_all(&answer.body)?;
    }
    Ok(())
}

/// Starts the recording proxy in front of `node`; returns its port and
/// what it records.
fn proxy(node: (&'static str, u16)) -> io::Result<(u16, Recorded)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let recorded = Recorded::default();
    let kept = Arc::clone(&recorded);
    thread::spawn(move || {
        for client in listener.incoming().flatten() {
            let kept = Arc::clone(&kept);
            thread::spawn(move || {
                if let Err(e) = serve(client, node, &kept) {
                    eprintln!("record-stagenet-node: {e}");
                }
            });
        }
    });
    Ok((port, recorded))
}

pub(crate) fn record(root: &Path) -> io::Result<Exit> {
    let (port, recorded) = proxy(NODE)?;
    let status = Command::new("cargo")
        .args(["test", "-p", "engine", "--test", "daemon_rpc_replay", "--"])
        .args(["--ignored", "--exact", TEST, "--nocapture"])
        .env(
            "ENGINE_LIVE_STAGENET_NODE",
            format!("http://127.0.0.1:{port}"),
        )
        .current_dir(root)
        .status()?;
    if !status.success() {
        eprintln!("{TEST} failed through the proxy, so {FIXTURE} is unchanged");
        return Ok(Exit::of(status));
    }
    let exchanges = recorded
        .lock()
        .map_err(|_| io::Error::other("a proxy thread panicked"))?
        .clone();
    write_json(&root.join(FIXTURE), &exchanges)?;
    eprintln!("recorded {} exchanges into {FIXTURE}", exchanges.len());
    Ok(Exit::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node that answers every request with the same body, chunked, as
    /// an HTTP server may, and says which path it was asked for.
    fn fake_node() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut writer = stream.try_clone().unwrap();
                let request = read_message(&mut BufReader::new(stream), false)
                    .unwrap()
                    .unwrap();
                let body = format!(
                    "{}:{}",
                    request.first,
                    String::from_utf8_lossy(&request.body)
                );
                write!(
                    writer,
                    "HTTP/1.1 200 Ok\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
                    body.len()
                )
                .unwrap();
            }
        });
        port
    }

    #[test]
    fn the_proxy_answers_each_request_on_a_kept_alive_connection_and_records_it() {
        let node_port = fake_node();
        let node: (&'static str, u16) = ("127.0.0.1", node_port);
        let (port, recorded) = proxy(node).unwrap();
        let client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        for (path, body) in [("/get_height", "{}"), ("/get_blocks.bin?x=1", "\u{1}\u{2}")] {
            write!(
                writer,
                "POST {path} HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            let answer = read_message(&mut reader, false).unwrap().unwrap();
            assert_eq!(answer.first, "HTTP/1.1 200 Ok");
            assert_eq!(answer.content_type.as_deref(), Some("application/json"));
            assert_eq!(
                String::from_utf8(answer.body).unwrap(),
                format!("POST {path} HTTP/1.1:{body}")
            );
        }
        let recorded = recorded.lock().unwrap().clone();
        assert_eq!(
            recorded
                .iter()
                .map(|e| (e.path.as_str(), e.request_hex.as_str()))
                .collect::<Vec<_>>(),
            [("/get_height", "7b7d"), ("/get_blocks.bin", "0102")]
        );
        assert_eq!(
            recorded[0].response_hex,
            hex::encode("POST /get_height HTTP/1.1:{}")
        );
    }
}
