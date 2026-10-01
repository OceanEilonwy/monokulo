//! Unix-socket server for `KeyCustody` (WBS 2.1.2).
//!
//! This is the "separate OS process" half of the split described in
//! `shared::key_custody`'s module docs (`shared/src/key_custody.rs` - moved there
//! from `engine`'s own `src/key_custody/mod.rs` as of WBS 2.1.3, see that
//! module's doc comment for why): a [`KeyCustodyServer`] holds a real
//! [`PlainKeyCustody`] and answers every `KeyCustody` call over a Unix socket
//! instead of in-process function calls, so that whatever process embeds this
//! server is the *only* process that ever has a tenant's view key in its own
//! heap. `bin/key-custody-server.rs` is the thin binary wrapper that actually
//! runs this as its own process; this module is also used directly (in-process,
//! as a background `tokio::spawn`ed task) by most of this crate's own tests,
//! since spinning up a second OS process for every test would be slow for no
//! extra coverage - see `tests/socket_key_custody.rs`'s own doc comment for the
//! one test that deliberately does use a real second process, to prove that
//! part of the story too.
//!
//! Still `PlainKeyCustody` underneath, not a TEE-backed implementation - per the
//! WBS, that's future work. This step proves the *mechanism* (a process
//! boundary plus a socket in between actually works end to end, including under
//! concurrent load and adversarial/garbage input), not a new custody backend.
//!
//! **This module lives in its own crate (`key-custody-server`), separate from
//! `key-custody-service`'s `client.rs`/`protocol.rs`, as of WBS 2.1.3.** It was
//! originally part of `key-custody-service` itself (WBS 2.1.2's "extend, don't
//! fork" framing), and moved out once `engine`'s own `main.rs` needed to
//! depend on `key-custody-service` for `SocketKeyCustody`: this module needs a
//! real `PlainKeyCustody`, which only exists in `engine`, so as long as it
//! lived in the same crate as `client.rs`, that crate depending on `engine`
//! while `engine` depended on it back was a real, hard Cargo dependency
//! cycle (`error: cyclic package dependency`, confirmed directly, not just
//! reasoned about). `key-custody-server` depends on both `engine` (for
//! `PlainKeyCustody`) and `key-custody-service` (for the protocol/DTO types this
//! module still needs); nothing depends on `key-custody-server` back, so the graph
//! stays a DAG. See `shared/src/key_custody.rs`'s module doc comment for the full
//! account, and this session's `work_notes.md` entry for the reasoning trail.

use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use engine::key_custody::{
    KeyCustody, Network, PlainKeyCustody, ScanInput, SubaddressIndex, WalletHandle, WalletMaterial,
};
use key_custody_service::protocol::{
    read_frame, write_frame, KeyCustodyRequest, KeyCustodyResponse,
};
use key_custody_service::{
    AddressWire, KeyCustodyErrorWire, MatchedOutputWire, SealedMaterialWire, TxMatchesWire,
    WalletHandleWire, WireConversionError,
};
use tokio::net::{UnixListener, UnixStream};
use zeroize::Zeroizing;

/// Most connections served at once. The engine keeps a small pool per
/// backend; anything beyond this is not the engine, and waits.
pub const MAX_CONNECTIONS: usize = 64;

/// Longest a connection may sit without sending a whole request before it
/// is closed. Well above the client's own per-call timeout, so a slow
/// call is never cut, but a peer that opened a connection and stalled
/// (or sent a length prefix and nothing after it) does not hold a task and
/// a buffer for good.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Wraps a real `PlainKeyCustody` behind a Unix socket. Owns nothing about
/// *where* that socket lives - `listen` takes the path each time it's called,
/// so the same server type works identically whether it's bound once by a
/// standalone binary or bound to a fresh throwaway path inside a test.
pub struct KeyCustodyServer {
    custody: Arc<PlainKeyCustody>,
}

impl KeyCustodyServer {
    pub fn new(custody: PlainKeyCustody) -> Self {
        KeyCustodyServer {
            custody: Arc::new(custody),
        }
    }

    /// Bind `socket_path` and serve connections until an unrecoverable accept
    /// error occurs (this never returns `Ok` in normal operation - the caller,
    /// whether that's `bin/key-custody-server.rs`'s `main` or a test's
    /// `tokio::spawn`, decides when to stop it). Each accepted connection is
    /// handled on its own spawned task against a shared `Arc<PlainKeyCustody>`,
    /// so one slow or wedged client never blocks another - the concurrency
    /// story this side of the socket is "however many connections show up,
    /// handled independently," matching `PlainKeyCustody`'s own existing
    /// internal locking, which was already proven safe under concurrent access
    /// by `plain.rs`'s `concurrent_registrations_and_removals_never_cross_
    /// wires_between_wallets` test.
    ///
    /// Binding fails if a file already exists at `socket_path` (the normal
    /// `bind(2)` behaviour for `AF_UNIX`) - deliberately not removed
    /// automatically here, since silently unlinking an arbitrary path this
    /// process didn't create itself is exactly the kind of surprising behind-
    /// the-scenes action this codebase avoids elsewhere. A caller that expects
    /// a stale socket file from a previous crashed run should remove it
    /// explicitly first; `bin/key-custody-server.rs` does not do this either,
    /// on the same reasoning - see its own doc comment.
    pub async fn listen(&self, socket_path: impl AsRef<Path>) -> io::Result<()> {
        self.serve(Self::bind(socket_path)?).await
    }

    /// Binds `socket_path` for [`Self::serve`], readable and writable by
    /// this user alone: `bind(2)` creates the file with the umask's
    /// permissions, which on a shared machine can let another user connect
    /// to the process that holds every store's view key. Synchronous, so a
    /// caller can hand the bound listener to a runtime (or a thread) and
    /// know the socket exists before anything tries to connect to it.
    pub fn bind(socket_path: impl AsRef<Path>) -> io::Result<std::os::unix::net::UnixListener> {
        let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(listener)
    }

    /// Serves connections on a listener from [`Self::bind`] until an
    /// unrecoverable accept error occurs (this never returns `Ok` in normal
    /// operation). A transient accept failure (file descriptors run out, a
    /// connection aborted before it was accepted) is logged and retried: it
    /// must not take down the process that holds every registered wallet.
    /// Only a peer running as the user that owns the socket is served, and
    /// at most [`MAX_CONNECTIONS`] at once.
    pub async fn serve(&self, listener: std::os::unix::net::UnixListener) -> io::Result<()> {
        let owner = listener
            .local_addr()?
            .as_pathname()
            .map(std::fs::metadata)
            .transpose()?
            .map(|metadata| metadata.uid());
        let listener = UnixListener::from_std(listener)?;
        let connections = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
        loop {
            // The semaphore is never closed, so this can't fail.
            let Ok(permit) = Arc::clone(&connections).acquire_owned().await else {
                return Ok(());
            };
            let (stream, _addr) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(e) if accept_error_is_transient(&e) => {
                    tracing::warn!(error = %e, "accept failed; retrying shortly");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                Err(e) => return Err(e),
            };
            match stream.peer_cred() {
                Ok(peer) if owner.is_none_or(|owner| owner == peer.uid()) => {}
                Ok(peer) => {
                    tracing::warn!(uid = peer.uid(), "refusing a connection from another user");
                    continue;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "refusing a connection whose peer can't be identified");
                    continue;
                }
            }
            let custody = Arc::clone(&self.custody);
            tokio::spawn(async move {
                let _permit = permit;
                handle_connection(stream, custody).await;
            });
        }
    }
}

/// Whether an `accept(2)` failure is about this one connection or the
/// moment (descriptors or buffers run out, the peer hung up first,
/// interrupted) rather than the listener itself.
fn accept_error_is_transient(e: &io::Error) -> bool {
    use io::ErrorKind::*;
    matches!(
        e.kind(),
        ConnectionAborted | ConnectionReset | Interrupted | WouldBlock | OutOfMemory
    ) || matches!(e.raw_os_error(), Some(24 | 23 | 105)) // EMFILE, ENFILE, ENOBUFS
}

/// Serve requests on one already-accepted connection until it closes or a
/// framing/protocol error makes it unsafe to keep reading from - see
/// `protocol.rs`'s `read_frame` doc comment for exactly which conditions count
/// as "closes" (`Ok(None)`) versus "unsafe to keep reading from" (`Err`).
/// Neither case panics; both simply drop `stream`, which closes the socket.
async fn handle_connection(mut stream: UnixStream, custody: Arc<PlainKeyCustody>) {
    loop {
        let request: KeyCustodyRequest =
            match tokio::time::timeout(IDLE_TIMEOUT, read_frame(&mut stream)).await {
                Ok(Ok(Some(request))) => request,
                Ok(Ok(None)) => return, // peer closed cleanly between requests
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "closing connection after a framing error");
                    return;
                }
                Err(_) => {
                    tracing::info!("closing a connection idle for {IDLE_TIMEOUT:?}");
                    return;
                }
            };

        let response = match dispatch(&custody, request).await {
            Ok(response) => response,
            Err(e) => {
                // `dispatch` only returns `Err` when a *decoded* request's own
                // fields don't convert into real types - e.g. a handle whose
                // hex isn't 16 bytes, an address string that doesn't parse, a
                // transaction that isn't valid consensus-encoded bytes. That's
                // not the same kind of thing as any of `KeyCustodyErrorWire`'s
                // four variants, all of which describe a real `KeyCustody`
                // *application* outcome (unknown wallet, bad key material, a
                // backend problem, a failed scan) for a request this server
                // could actually attempt. Forcing a decode failure into one of
                // those would misrepresent it - a caller matching on
                // `KeyCustodyErrorWire::UnknownWallet` to decide whether to
                // retry with a fresh registration, say, has no reason to
                // expect it might also mean "your bytes were corrupted in
                // transit." `lib.rs`'s own `WireConversionError` doc comment
                // left this exact decision to 2.1.2: close the connection,
                // the same way a raw framing error does, rather than invent a
                // fifth `KeyCustodyErrorWire` variant for a failure mode that
                // isn't part of the `KeyCustody` trait's own contract at all.
                tracing::warn!(error = %e, "closing connection after a malformed request");
                return;
            }
        };

        if let Err(e) = write_frame(&mut stream, &response).await {
            tracing::warn!(error = %e, "closing connection after a write error");
            return;
        }
    }
}

/// Decode one request's fields into real engine types, call the matching
/// `PlainKeyCustody` method, and re-encode the result. `Err` here means the
/// request's own fields didn't decode - see `handle_connection`'s doc comment
/// for what the caller does with that. Every other outcome, including every
/// `KeyCustodyError` the backend itself returns, is folded into the returned
/// `KeyCustodyResponse`'s `Err(KeyCustodyErrorWire)` side, never this
/// function's own `Result::Err`.
pub async fn dispatch(
    custody: &PlainKeyCustody,
    request: KeyCustodyRequest,
) -> Result<KeyCustodyResponse, WireConversionError> {
    Ok(match request {
        KeyCustodyRequest::RegisterWallet(req) => {
            let material = WalletMaterial::try_from(&req.material)?;
            let result = custody
                .register_wallet(material)
                .await
                .map(WalletHandleWire::from)
                .map_err(KeyCustodyErrorWire::from);
            KeyCustodyResponse::RegisterWallet(result)
        }
        KeyCustodyRequest::RemoveWallet(req) => {
            let handle = WalletHandle::try_from(&req.handle)?;
            let result = custody
                .remove_wallet(handle)
                .await
                .map_err(KeyCustodyErrorWire::from);
            KeyCustodyResponse::RemoveWallet(result)
        }
        KeyCustodyRequest::Seal(req) => {
            let material = WalletMaterial::try_from(&req.material)?;
            // The sealed bytes are the key itself for this backend: scrubbed
            // once encoded for the wire.
            let result = custody
                .seal(&material)
                .await
                .map(|bytes| SealedMaterialWire::from(Zeroizing::new(bytes).as_slice()))
                .map_err(KeyCustodyErrorWire::from);
            KeyCustodyResponse::Seal(result)
        }
        KeyCustodyRequest::UnsealAndRegister(req) => {
            let sealed = Zeroizing::new(req.sealed.to_bytes()?);
            let result = match req.registration_id.as_deref() {
                Some(id) => custody.unseal_and_register_idempotent(&sealed, id).await,
                None => custody.unseal_and_register(&sealed).await,
            }
            .map(WalletHandleWire::from)
            .map_err(KeyCustodyErrorWire::from);
            KeyCustodyResponse::UnsealAndRegister(result)
        }
        KeyCustodyRequest::DeriveSubaddress(req) => {
            let handle = WalletHandle::try_from(&req.handle)?;
            let index = SubaddressIndex::from(req.index);
            let network = Network::try_from(&req.network)?;
            let result = custody
                .derive_subaddress(handle, index, network)
                .await
                .map(AddressWire::from)
                .map_err(KeyCustodyErrorWire::from);
            KeyCustodyResponse::DeriveSubaddress(result)
        }
        KeyCustodyRequest::ScanTxOutputs(req) => {
            let handle = WalletHandle::try_from(&req.handle)?;
            let tx = ScanInput::try_from(&req.tx)?;
            let major_range = std::ops::Range::<u32>::from(req.major_range);
            let minor_range = std::ops::Range::<u32>::from(req.minor_range);
            let result = custody
                .scan_tx_outputs(handle, &tx, major_range, minor_range)
                .await
                .map(|matches| matches.into_iter().map(MatchedOutputWire::from).collect())
                .map_err(KeyCustodyErrorWire::from);
            KeyCustodyResponse::ScanTxOutputs(result)
        }
        KeyCustodyRequest::ScanTxsForIndices(req) => {
            let handle = WalletHandle::try_from(&req.handle)?;
            let txs = req
                .txs
                .iter()
                .map(ScanInput::try_from)
                .collect::<Result<Vec<_>, _>>()?;
            let indices = engine::key_custody::ScanIndices::new(req.minors);
            let result = custody
                .scan_txs_for_indices(handle, &txs, &indices)
                .await
                .map(|matches| matches.into_iter().map(TxMatchesWire::from).collect())
                .map_err(KeyCustodyErrorWire::from);
            KeyCustodyResponse::ScanTxsForIndices(result)
        }
    })
}
