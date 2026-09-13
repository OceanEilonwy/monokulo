//! `SocketKeyCustody`: a `KeyCustody` implementation that forwards every call
//! over a Unix socket to a `key_custody_server::server::KeyCustodyServer` (WBS
//! 2.1.2) - that server type moved to the separate `key-custody-server` crate as
//! of WBS 2.1.3 (see `shared::key_custody`'s module doc comment for why), so it
//! is no longer a same-crate doc-link from here.
//!
//! This is the caller-facing half of the split: everything that already
//! depends on `shared::key_custody::KeyCustody` (re-exported unchanged as
//! `moneropay_core::key_custody::KeyCustody` - the HTTP API, the chain scanner,
//! tenant bootstrap at startup) can hold a `SocketKeyCustody` exactly where it
//! would otherwise hold a `PlainKeyCustody`, with no other code change - that's
//! the whole point of drawing the boundary as a trait in the first place.
//! `moneropay-core`'s own `main.rs` is wired to actually select this type behind
//! a config flag as of WBS 2.1.3 - see `src/config.rs`'s `KeyCustodyConfig` and
//! `main.rs`'s `build_key_custody`.
//!
//! **Connection lifetime and concurrency.** `SocketKeyCustody` opens one
//! persistent connection at `connect` time and reuses it for every call,
//! rather than dialing a fresh connection per request - a real deployment's
//! engine process will make many `KeyCustody` calls per second (one per
//! scanned mempool transaction, at minimum), and paying a fresh `connect(2)`
//! plus the OS's per-connection bookkeeping for each one would be pure
//! overhead for a socket that's going to stay up for the life of the process
//! anyway. That one connection is protected by a `tokio::sync::Mutex`, so
//! concurrent callers serialize onto it one call at a time rather than a
//! request-ID/correlation scheme letting several calls be in flight over the
//! wire simultaneously. Deliberately the simpler of the two options the WBS
//! calls out: a length-prefixed request/response pair has no way to tell two
//! *interleaved* responses apart without adding a correlation id to every DTO
//! in `lib.rs`, which would be real, permanent wire-format complexity to buy
//! back concurrency this workload doesn't obviously need yet - every
//! `KeyCustody` call is already bounded by the scalar-multiplication costs
//! `src/key_custody/plain.rs` documents, and a future TEE-backed backend is
//! unlikely to parallelize arbitrarily within one enclave either. If this
//! socket becomes a real per-call latency bottleneck once wired into the
//! engine (2.1.3), the fix is either a small connection pool (several
//! `SocketKeyCustody`-shaped connections, each still mutex-serialized) or a
//! real correlation-id scheme - not a change to this decision made lightly
//! now.
//!
//! **What "clean error" means here.** Every failure mode this module can hit -
//! the socket doesn't exist yet, the server process crashed or was never
//! started, the server closed the connection, a response didn't arrive within
//! `call_timeout`, or a response arrived but wasn't valid JSON for the
//! envelope shape expected - surfaces as `Err(KeyCustodyError::
//! BackendUnavailable(..))`, never a panic and never an indefinite hang. Once
//! any of those happens the connection is presumed unsafe to keep using (see
//! `call`'s doc comment for why) and every subsequent call on the same
//! `SocketKeyCustody` fails immediately without touching the socket again;
//! there is no automatic reconnect in this step. That's a deliberate,
//! documented limitation, not an oversight - transparently reconnecting mid-
//! stream would mean silently losing whatever in-flight semantics a caller was
//! relying on (e.g. "this call either reached the real backend or it didn't"),
//! and building that well is its own piece of work `2.1.3`'s real engine
//! integration is better placed to drive the requirements for than this step
//! guessing at them.

use std::path::Path;
use std::time::Duration;

use monero::{Address, Transaction};
use shared::key_custody::{
    KeyCustody, KeyCustodyError, MatchedOutput, Network, SubaddressIndex, WalletHandle,
    WalletMaterial,
};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use crate::protocol::{read_frame, write_frame, KeyCustodyRequest, KeyCustodyResponse};
use crate::{
    DeriveSubaddressRequest, MatchedOutputWire, NetworkWire, RangeWire, RegisterWalletRequest,
    RemoveWalletRequest, ScanTxOutputsRequest, SealRequest, SealedMaterialWire,
    SubaddressIndexWire, TransactionWire, UnsealAndRegisterRequest, WalletHandleWire,
    WalletMaterialWire,
};

/// How long one `call` waits for a response before giving up. Without a bound
/// here, a wedged or malicious `key-custody-service` process that accepts a
/// connection and then never answers (or answers a valid length prefix and
/// then withholds the body) would hang the calling `.await` forever - and
/// because the connection this type holds is shared, mutex-serialized state
/// (see the module doc comment), one such call would silently stall *every*
/// other concurrent caller of the same `SocketKeyCustody` too, forever, with
/// no error and no log line. Thirty seconds is generous for what should always
/// be a same-host, in-memory-speed round trip - long enough that ordinary GC/
/// scheduling jitter never trips it, short enough that a genuinely wedged
/// backend surfaces as a clear, bounded `BackendUnavailable` instead of an
/// unbounded stall. `connect_with_timeout` exists for a caller (or a test)
/// that wants a different bound.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// `KeyCustody` implementation that forwards every call to a real
/// `PlainKeyCustody` running behind a `crate::server::KeyCustodyServer` on the
/// other end of a Unix socket. See the module doc comment for the framing,
/// concurrency, and error-handling decisions behind this type.
pub struct SocketKeyCustody {
    /// `None` once any call has failed for a transport-level reason - see
    /// `call`'s doc comment. `Some` holds the one persistent connection this
    /// client reuses across every method.
    conn: Mutex<Option<UnixStream>>,
    call_timeout: Duration,
}

impl SocketKeyCustody {
    /// Connect to `socket_path` with the default call timeout. Fails cleanly
    /// (no panic) if nothing is listening there yet, matching the WBS's own
    /// acceptance test for "the socket is unreachable at connect time."
    pub async fn connect(socket_path: impl AsRef<Path>) -> Result<Self, KeyCustodyError> {
        Self::connect_with_timeout(socket_path, DEFAULT_CALL_TIMEOUT).await
    }

    /// As `connect`, with an explicit per-call timeout instead of
    /// `DEFAULT_CALL_TIMEOUT` - mainly for tests that want to prove the
    /// "doesn't hang forever" behaviour without actually waiting 30 seconds.
    pub async fn connect_with_timeout(
        socket_path: impl AsRef<Path>,
        call_timeout: Duration,
    ) -> Result<Self, KeyCustodyError> {
        let path = socket_path.as_ref();
        let stream = UnixStream::connect(path).await.map_err(|e| {
            KeyCustodyError::BackendUnavailable(format!(
                "connecting to key-custody-service at {}: {e}",
                path.display()
            ))
        })?;
        Ok(SocketKeyCustody { conn: Mutex::new(Some(stream)), call_timeout })
    }

    /// Send one request and wait for its matching response, holding the
    /// connection mutex for the round trip.
    ///
    /// **Why a failed call poisons the connection rather than trying to keep
    /// using it.** Once a `write_frame`/`read_frame` pair doesn't complete
    /// cleanly - an I/O error, a timeout, a peer that closed mid-response, or
    /// a payload that didn't decode - there is no way to know how many bytes
    /// of a request or response the peer actually saw. A timeout in
    /// particular is the sharpest version of this: the peer may still be
    /// about to write a late response for the call that just timed out, and
    /// if this client tried another call on the same stream afterwards, that
    /// stale response's bytes would land at the start of the *next* call's
    /// expected reply and be misread as if they belonged to it - silent
    /// framing corruption, not a clean error. Closing the connection (setting
    /// `conn` to `None`) the moment any of this happens turns that whole class
    /// of bug into a simple, loud "this `SocketKeyCustody` is dead, make a new
    /// one" - exactly the "clean error, not a hang, not silent corruption"
    /// bar the WBS's own acceptance tests hold this module to.
    async fn call(&self, request: KeyCustodyRequest) -> Result<KeyCustodyResponse, KeyCustodyError> {
        let mut guard = self.conn.lock().await;
        let stream = guard.as_mut().ok_or_else(|| {
            KeyCustodyError::BackendUnavailable(
                "key-custody-service connection already failed on a previous call".to_string(),
            )
        })?;

        let outcome = tokio::time::timeout(self.call_timeout, async {
            write_frame(stream, &request).await?;
            read_frame(stream).await
        })
        .await;

        match outcome {
            Ok(Ok(Some(response))) => Ok(response),
            Ok(Ok(None)) => {
                *guard = None;
                Err(KeyCustodyError::BackendUnavailable(
                    "key-custody-service closed the connection".to_string(),
                ))
            }
            Ok(Err(e)) => {
                *guard = None;
                Err(KeyCustodyError::BackendUnavailable(format!(
                    "key-custody-service connection failed: {e}"
                )))
            }
            Err(_elapsed) => {
                *guard = None;
                Err(KeyCustodyError::BackendUnavailable(format!(
                    "key-custody-service did not respond within {:?}",
                    self.call_timeout
                )))
            }
        }
    }
}

/// A response envelope arrived, but not the variant the request that produced
/// it should have gotten back - only reachable if this client and the server
/// it's talking to disagree about the protocol (a version skew, or a server
/// that isn't actually `crate::server::dispatch` at all). Still a clean error,
/// never a panic, exactly like every other failure mode in this module.
fn mismatched_response(expected: &str, got: &KeyCustodyResponse) -> KeyCustodyError {
    KeyCustodyError::BackendUnavailable(format!(
        "key-custody-service sent a {got:?} response, expected {expected}"
    ))
}

#[async_trait::async_trait]
impl KeyCustody for SocketKeyCustody {
    async fn register_wallet(&self, material: WalletMaterial) -> Result<WalletHandle, KeyCustodyError> {
        let request = KeyCustodyRequest::RegisterWallet(RegisterWalletRequest {
            material: WalletMaterialWire::from(&material),
        });
        match self.call(request).await? {
            KeyCustodyResponse::RegisterWallet(Ok(handle)) => WalletHandle::try_from(handle)
                .map_err(|e| {
                    KeyCustodyError::BackendUnavailable(format!(
                        "key-custody-service returned a malformed handle: {e}"
                    ))
                }),
            KeyCustodyResponse::RegisterWallet(Err(e)) => Err(e.into()),
            other => Err(mismatched_response("RegisterWallet", &other)),
        }
    }

    async fn remove_wallet(&self, handle: WalletHandle) -> Result<(), KeyCustodyError> {
        let request = KeyCustodyRequest::RemoveWallet(RemoveWalletRequest {
            handle: WalletHandleWire::from(handle),
        });
        match self.call(request).await? {
            KeyCustodyResponse::RemoveWallet(Ok(())) => Ok(()),
            KeyCustodyResponse::RemoveWallet(Err(e)) => Err(e.into()),
            other => Err(mismatched_response("RemoveWallet", &other)),
        }
    }

    async fn seal(&self, material: &WalletMaterial) -> Result<Vec<u8>, KeyCustodyError> {
        let request = KeyCustodyRequest::Seal(SealRequest { material: WalletMaterialWire::from(material) });
        match self.call(request).await? {
            KeyCustodyResponse::Seal(Ok(sealed)) => sealed.to_bytes().map_err(|e| {
                KeyCustodyError::BackendUnavailable(format!(
                    "key-custody-service returned malformed sealed bytes: {e}"
                ))
            }),
            KeyCustodyResponse::Seal(Err(e)) => Err(e.into()),
            other => Err(mismatched_response("Seal", &other)),
        }
    }

    async fn unseal_and_register(&self, sealed: &[u8]) -> Result<WalletHandle, KeyCustodyError> {
        let request = KeyCustodyRequest::UnsealAndRegister(UnsealAndRegisterRequest {
            sealed: SealedMaterialWire::from(sealed),
        });
        match self.call(request).await? {
            KeyCustodyResponse::UnsealAndRegister(Ok(handle)) => WalletHandle::try_from(handle)
                .map_err(|e| {
                    KeyCustodyError::BackendUnavailable(format!(
                        "key-custody-service returned a malformed handle: {e}"
                    ))
                }),
            KeyCustodyResponse::UnsealAndRegister(Err(e)) => Err(e.into()),
            other => Err(mismatched_response("UnsealAndRegister", &other)),
        }
    }

    async fn derive_subaddress(
        &self,
        handle: WalletHandle,
        index: SubaddressIndex,
        network: Network,
    ) -> Result<Address, KeyCustodyError> {
        let request = KeyCustodyRequest::DeriveSubaddress(DeriveSubaddressRequest {
            handle: WalletHandleWire::from(handle),
            index: SubaddressIndexWire::from(index),
            network: NetworkWire::from(network),
        });
        match self.call(request).await? {
            KeyCustodyResponse::DeriveSubaddress(Ok(address)) => {
                Address::try_from(&address).map_err(|e| {
                    KeyCustodyError::BackendUnavailable(format!(
                        "key-custody-service returned a malformed address: {e}"
                    ))
                })
            }
            KeyCustodyResponse::DeriveSubaddress(Err(e)) => Err(e.into()),
            other => Err(mismatched_response("DeriveSubaddress", &other)),
        }
    }

    async fn scan_tx_outputs(
        &self,
        handle: WalletHandle,
        tx: &Transaction,
        major_range: std::ops::Range<u32>,
        minor_range: std::ops::Range<u32>,
    ) -> Result<Vec<MatchedOutput>, KeyCustodyError> {
        let request = KeyCustodyRequest::ScanTxOutputs(ScanTxOutputsRequest {
            handle: WalletHandleWire::from(handle),
            tx: TransactionWire::from(tx),
            major_range: RangeWire::from(major_range),
            minor_range: RangeWire::from(minor_range),
        });
        match self.call(request).await? {
            KeyCustodyResponse::ScanTxOutputs(Ok(matches)) => matches
                .into_iter()
                .map(|m: MatchedOutputWire| {
                    MatchedOutput::try_from(m).map_err(|e| {
                        KeyCustodyError::BackendUnavailable(format!(
                            "key-custody-service returned a malformed matched output: {e}"
                        ))
                    })
                })
                .collect(),
            KeyCustodyResponse::ScanTxOutputs(Err(e)) => Err(e.into()),
            other => Err(mismatched_response("ScanTxOutputs", &other)),
        }
    }
}
