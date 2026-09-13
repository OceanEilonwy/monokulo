//! Standalone binary: runs a `key_custody_service::server::KeyCustodyServer`
//! wrapping a fresh, empty `PlainKeyCustody`, bound to a Unix socket path given
//! on argv - the genuinely-separate-process side of WBS 2.1.2's split.
//!
//! Deliberately as thin as `mock-woocommerce/src/main.rs`'s own CLI wrapper:
//! all the real logic (listening, dispatch, framing) lives in the library
//! (`server.rs`, `protocol.rs`), unit-testable the normal way and reusable
//! in-process by this crate's own tests without paying for a second OS
//! process every time - see `tests/socket_key_custody.rs`'s doc comment for
//! which test actually does exercise this compiled binary as a real child
//! process, and why only that one needs to.
//!
//! No config file, no `--help`, no daemonizing: a single required argument
//! (the socket path) is the entire interface this needs at this stage. A real
//! deployment (WBS 2.1.3 and beyond) will run this under whatever process
//! supervisor manages the rest of the split (systemd unit, container
//! entrypoint, etc.) - that supervisor is what should own restart policy,
//! logging destination, and stale-socket cleanup between crashes, not this
//! binary guessing at them.

use std::process::ExitCode;

use key_custody_service::server::KeyCustodyServer;
use moneropay_core::key_custody::PlainKeyCustody;

#[tokio::main]
async fn main() -> ExitCode {
    let socket_path = match std::env::args().nth(1) {
        Some(path) => path,
        None => {
            eprintln!("usage: key-custody-server <unix-socket-path>");
            return ExitCode::FAILURE;
        }
    };

    let server = KeyCustodyServer::new(PlainKeyCustody::default());
    println!("key-custody-server listening on {socket_path}");
    if let Err(e) = server.listen(&socket_path).await {
        eprintln!("key-custody-server: fatal error on {socket_path}: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
