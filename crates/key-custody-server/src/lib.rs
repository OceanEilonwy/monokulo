//! `key-custody-server`: the out-of-process half of WBS 2.1's socket-based
//! `KeyCustody` split - a `KeyCustodyServer` wrapping a real
//! `scanner::key_custody::PlainKeyCustody` and answering every call over a
//! Unix socket, plus the standalone `key-custody-server` binary
//! (`src/bin/key-custody-server.rs`) that runs it as its own OS process.
//!
//! Split out of `key-custody-service` (which originally held this, `client.rs`,
//! and `protocol.rs` together, per WBS 2.1.2's "extend, don't fork" framing) as
//! of WBS 2.1.3, once `scanner`'s own `main.rs` needed to depend on
//! `key-custody-service` for `SocketKeyCustody` - this crate's `server.rs` needs a
//! real `PlainKeyCustody`, which only exists in `scanner`, so keeping it in
//! the same crate `main.rs` depends on would have made `scanner` depend on
//! a crate that depends on `scanner`, a real Cargo dependency cycle. See
//! `server.rs`'s own module doc comment, and `shared/src/key_custody.rs`'s, for
//! the full account.
//!
//! This crate depends on both `scanner` (for `PlainKeyCustody`) and
//! `key-custody-service` (for the protocol/DTO types `server.rs` decodes requests
//! into and encodes responses from) - nothing depends on this crate in turn, so
//! the workspace's package graph stays a DAG.

pub mod server;
