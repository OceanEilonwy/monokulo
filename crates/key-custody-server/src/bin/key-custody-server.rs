//! Standalone binary: runs a `key_custody_server::server::KeyCustodyServer`
//! wrapping a fresh, empty `PlainKeyCustody`, bound to a Unix socket path given
//! on argv - the genuinely-separate-process side of WBS 2.1.2's split. Lives in
//! its own crate (`key-custody-server`, separate from `key-custody-service`'s
//! `client.rs`/`protocol.rs`) as of WBS 2.1.3 - see `server.rs`'s own module doc
//! comment for why that split was necessary, not just tidier.
//!
//! Deliberately as thin as `mock-woocommerce/src/main.rs`'s own CLI wrapper:
//! all the real logic (listening, dispatch, framing) lives in the library
//! (this crate's `server.rs`, plus `key-custody-service`'s `protocol.rs`),
//! unit-testable the normal way and reusable in-process by this crate's own
//! tests without paying for a second OS process every time - see
//! `tests/socket_key_custody.rs`'s doc comment for which test actually does
//! exercise this compiled binary as a real child process, and why only that
//! one needs to.
//!
//! No config file, no `--help`, no daemonizing: a single required argument
//! (the socket path) is the entire interface this needs at this stage. A real
//! deployment (WBS 2.1.3 and beyond) will run this under whatever process
//! supervisor manages the rest of the split (systemd unit, container
//! entrypoint, etc.) - that supervisor is what should own restart policy,
//! logging destination, and stale-socket cleanup between crashes, not this
//! binary guessing at them. `engine`'s own `main.rs`, dialing this
//! process as a *client* via `key_custody_service::client::SocketKeyCustody`,
//! makes a different, complementary choice for the ordinary "both processes are
//! starting up around the same time" race - see `main.rs`'s
//! `connect_socket_key_custody` doc comment for why a bounded retry loop belongs
//! there without that changing anything about this binary's own hands-off
//! stance on restart policy.

use std::process::ExitCode;

use engine::key_custody::PlainKeyCustody;
use key_custody_server::server::KeyCustodyServer;

#[tokio::main]
async fn main() -> ExitCode {
    let telemetry = telemetry::init("key-custody-server", "KEY_CUSTODY");
    let socket_path = match std::env::args().nth(1) {
        Some(path) => path,
        None => {
            eprintln!("usage: key-custody-server <unix-socket-path>");
            return ExitCode::FAILURE;
        }
    };

    let server = KeyCustodyServer::new(PlainKeyCustody::default());
    tracing::info!(socket = %socket_path, "key-custody-server listening");
    if let Err(e) = server.listen(&socket_path).await {
        tracing::error!(socket = %socket_path, error = %e, "fatal error");
        telemetry.flush(std::time::Duration::from_secs(5)).await;
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
