//! An address for tests that need a node or an engine that can't be
//! reached.
//!
//! A port nothing listens on looks like the obvious choice, but Windows
//! retries a refused connection for about two seconds before it gives up,
//! where Linux and macOS fail at once. A test that tries one several times
//! spends its time waiting, and only on Windows. Here something does listen,
//! and hangs up on every connection the moment it arrives: the request
//! fails as fast on every system, the same way a refusal does for the
//! callers (an error sending the request).

use std::net::{SocketAddr, TcpListener};

/// The address of a new listener that drops every connection as soon as it
/// accepts it. Each call has its own port, for a test that needs several
/// nodes no two the same. It runs on a thread of its own until the process
/// ends, so a test needs no runtime to use it and can hand it to a child
/// process.
pub fn address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let address = listener.local_addr().expect("the bound address");
    std::thread::Builder::new()
        .name("unreachable".into())
        .spawn(move || {
            for connection in listener.incoming() {
                drop(connection);
            }
        })
        .expect("start the unreachable listener");
    address
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};

    /// A request there gets no answer: the connection ends (or is reset)
    /// instead. Each address is a port of its own.
    #[test]
    fn a_request_ends_without_an_answer() {
        let address = super::address();
        assert_ne!(super::address(), address);
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        // The write can already fail if the hang-up got here first.
        let _ = stream.write_all(b"GET / HTTP/1.1\r\nhost: x\r\n\r\n");
        let mut answer = Vec::new();
        match stream.read_to_end(&mut answer) {
            Ok(_) => assert!(answer.is_empty(), "no answer"),
            // A read timeout is WouldBlock on Unix and TimedOut on Windows.
            Err(e) => assert!(
                !matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ),
                "hung: {e}"
            ),
        }
    }
}
