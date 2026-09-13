//! Unix-socket server for `KeyCustody` (WBS 2.1.2).
//!
//! This is the "separate OS process" half of the split described in
//! `src/key_custody/mod.rs`'s module docs: a [`KeyCustodyServer`] holds a real
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

use std::io;
use std::path::Path;
use std::sync::Arc;

use moneropay_core::key_custody::{
    KeyCustody, Network, PlainKeyCustody, SubaddressIndex, WalletHandle, WalletMaterial,
};
use monero::Transaction;
use tokio::net::{UnixListener, UnixStream};

use crate::protocol::{read_frame, write_frame, KeyCustodyRequest, KeyCustodyResponse};
use crate::{
    AddressWire, KeyCustodyErrorWire, MatchedOutputWire, SealedMaterialWire, WalletHandleWire,
    WireConversionError,
};

/// Wraps a real `PlainKeyCustody` behind a Unix socket. Owns nothing about
/// *where* that socket lives - `listen` takes the path each time it's called,
/// so the same server type works identically whether it's bound once by a
/// standalone binary or bound to a fresh throwaway path inside a test.
pub struct KeyCustodyServer {
    custody: Arc<PlainKeyCustody>,
}

impl KeyCustodyServer {
    pub fn new(custody: PlainKeyCustody) -> Self {
        KeyCustodyServer { custody: Arc::new(custody) }
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
        let listener = UnixListener::bind(socket_path)?;
        loop {
            let (stream, _addr) = listener.accept().await?;
            let custody = Arc::clone(&self.custody);
            tokio::spawn(async move {
                handle_connection(stream, custody).await;
            });
        }
    }
}

/// Serve requests on one already-accepted connection until it closes or a
/// framing/protocol error makes it unsafe to keep reading from - see
/// `protocol.rs`'s `read_frame` doc comment for exactly which conditions count
/// as "closes" (`Ok(None)`) versus "unsafe to keep reading from" (`Err`).
/// Neither case panics; both simply drop `stream`, which closes the socket.
async fn handle_connection(mut stream: UnixStream, custody: Arc<PlainKeyCustody>) {
    loop {
        let request: KeyCustodyRequest = match read_frame(&mut stream).await {
            Ok(Some(request)) => request,
            Ok(None) => return, // peer closed cleanly between requests
            Err(e) => {
                eprintln!("key-custody-service: closing connection after a framing error: {e}");
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
                eprintln!(
                    "key-custody-service: closing connection after a malformed request: {e}"
                );
                return;
            }
        };

        if let Err(e) = write_frame(&mut stream, &response).await {
            eprintln!("key-custody-service: closing connection after a write error: {e}");
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
            let result = custody.remove_wallet(handle).await.map_err(KeyCustodyErrorWire::from);
            KeyCustodyResponse::RemoveWallet(result)
        }
        KeyCustodyRequest::Seal(req) => {
            let material = WalletMaterial::try_from(&req.material)?;
            let result = custody
                .seal(&material)
                .await
                .map(|bytes| SealedMaterialWire::from(bytes.as_slice()))
                .map_err(KeyCustodyErrorWire::from);
            KeyCustodyResponse::Seal(result)
        }
        KeyCustodyRequest::UnsealAndRegister(req) => {
            let sealed = req.sealed.to_bytes()?;
            let result = custody
                .unseal_and_register(&sealed)
                .await
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
            let tx = Transaction::try_from(&req.tx)?;
            let major_range = std::ops::Range::<u32>::from(req.major_range);
            let minor_range = std::ops::Range::<u32>::from(req.minor_range);
            let result = custody
                .scan_tx_outputs(handle, &tx, major_range, minor_range)
                .await
                .map(|matches| matches.into_iter().map(MatchedOutputWire::from).collect())
                .map_err(KeyCustodyErrorWire::from);
            KeyCustodyResponse::ScanTxOutputs(result)
        }
    })
}
