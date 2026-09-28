//! `SocketKeyCustody`: a `KeyCustody` implementation that forwards every call
//! over a Unix socket to a `key_custody_server::server::KeyCustodyServer` (WBS
//! 2.1.2) - that server type moved to the separate `key-custody-server` crate as
//! of WBS 2.1.3 (see `shared::key_custody`'s module doc comment for why), so it
//! is no longer a same-crate doc-link from here.
//!
//! This is the caller-facing half of the split: everything that already
//! depends on `shared::key_custody::KeyCustody` (re-exported unchanged as
//! `scanner::key_custody::KeyCustody` - the HTTP API, the chain scanner,
//! tenant bootstrap at startup) can hold a `SocketKeyCustody` exactly where it
//! would otherwise hold a `PlainKeyCustody`, with no other code change - that's
//! the whole point of drawing the boundary as a trait in the first place.
//! `scanner`'s own `main.rs` is wired to actually select this type behind
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
//! any of those happens that one connection is closed; the next call that
//! needs a connection opens a new one (admin_settings_v2.md task 7.5). So a
//! restarted server is reached again without a new client. A restarted
//! server has also lost every wallet registered with it: `check_state`
//! notices that through a canary wallet, so the engine can register its
//! wallets again (task 5.8).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use monero::{Address, Transaction};
use shared::key_custody::{
    KeyCustody, KeyCustodyError, MatchedOutput, Network, ScanIndices, SubaddressIndex, WalletHandle,
    WalletMaterial,
};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use crate::protocol::{read_frame, write_frame, KeyCustodyRequest, KeyCustodyResponse};
use crate::{
    DeriveSubaddressRequest, MatchedOutputWire, NetworkWire, RangeWire, RegisterWalletRequest,
    RemoveWalletRequest, ScanTxOutputsForIndicesRequest, ScanTxOutputsRequest, SealRequest, SealedMaterialWire,
    SubaddressIndexWire, TransactionWire, UnsealAndRegisterRequest, WalletHandleWire,
    WalletMaterialWire,
};

pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Connections kept to the server at most (task 7.5). One is opened at
/// connect time; more only when calls overlap, so a single-threaded caller
/// uses one connection, as before.
pub const DEFAULT_POOL_SIZE: usize = 4;

/// The canary's key material (task 5.8): a fixed, worthless wallet the
/// client registers so it can later ask whether the server still has it. If
/// not, the server lost its memory and every handle it issued is gone.
const CANARY_VIEW_KEY: [u8; 32] = [
    0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x01, 0x02, 0x03, 0x04,
    0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x01,
];

pub struct SocketKeyCustody {
    socket_path: PathBuf,
    /// Each slot is one connection, used by one call at a time; `None` until
    /// (re)opened. A connection that fails is dropped and reopened by the next
    /// call that needs it, so one bad connection never stops the others and a
    /// restarted server is reached again without a new client.
    slots: Vec<Mutex<Option<UnixStream>>>,
    call_timeout: Duration,
    /// Set once the server has shown it doesn't know the index-set request
    /// (an older server): index-set scans then use the range request.
    indices_unsupported: AtomicBool,
    canary: parking_lot::Mutex<Option<WalletHandle>>,
    epoch: AtomicU64,
    state_check: Mutex<()>,
}

impl SocketKeyCustody {
    pub async fn connect(socket_path: impl AsRef<Path>) -> Result<Self, KeyCustodyError> {
        Self::connect_with_timeout(socket_path, DEFAULT_CALL_TIMEOUT).await
    }

    pub async fn connect_with_timeout(
        socket_path: impl AsRef<Path>,
        call_timeout: Duration,
    ) -> Result<Self, KeyCustodyError> {
        Self::connect_with_pool(socket_path, call_timeout, DEFAULT_POOL_SIZE).await
    }

    pub async fn connect_with_pool(
        socket_path: impl AsRef<Path>,
        call_timeout: Duration,
        pool_size: usize,
    ) -> Result<Self, KeyCustodyError> {
        let socket_path = socket_path.as_ref().to_path_buf();
        let first = open(&socket_path).await?;
        let mut slots: Vec<Mutex<Option<UnixStream>>> = (0..pool_size.max(1)).map(|_| Mutex::new(None)).collect();
        slots[0] = Mutex::new(Some(first));
        Ok(SocketKeyCustody {
            socket_path,
            slots,
            call_timeout,
            indices_unsupported: AtomicBool::new(false),
            canary: parking_lot::Mutex::new(None),
            epoch: AtomicU64::new(0),
            state_check: Mutex::new(()),
        })
    }

    /// A client that hasn't connected yet: every call tries to connect, and
    /// fails with `BackendUnavailable` until the server is there. For an
    /// engine starting while its key-custody server is down (task 5.8): it
    /// carries on and picks the server up when it appears.
    pub fn not_connected_yet(socket_path: impl AsRef<Path>, call_timeout: Duration) -> Self {
        SocketKeyCustody {
            socket_path: socket_path.as_ref().to_path_buf(),
            slots: (0..DEFAULT_POOL_SIZE).map(|_| Mutex::new(None)).collect(),
            call_timeout,
            indices_unsupported: AtomicBool::new(false),
            canary: parking_lot::Mutex::new(None),
            epoch: AtomicU64::new(0),
            state_check: Mutex::new(()),
        }
    }

    /// Whether the server answers a request every version knows, on a fresh
    /// connection: removing a handle nobody has (answered "unknown wallet",
    /// and harmless).
    async fn answers_a_known_request(&self) -> bool {
        let request = KeyCustodyRequest::RemoveWallet(RemoveWalletRequest { handle: WalletHandleWire::from(WalletHandle::new()) });
        matches!(self.call_fresh(request).await, Ok(KeyCustodyResponse::RemoveWallet(_)))
    }

    /// One request on a connection opened for it and closed after, so a
    /// stale pooled connection can't be what fails.
    async fn call_fresh(&self, request: KeyCustodyRequest) -> Result<KeyCustodyResponse, KeyCustodyError> {
        let mut stream = open(&self.socket_path).await?;
        match tokio::time::timeout(self.call_timeout, async {
            write_frame(&mut stream, &request).await?;
            read_frame(&mut stream).await
        })
        .await
        {
            Ok(Ok(Some(response))) => Ok(response),
            Ok(Ok(None)) => Err(KeyCustodyError::BackendUnavailable("key-custody-service closed the connection".to_string())),
            Ok(Err(e)) => Err(KeyCustodyError::BackendUnavailable(format!("key-custody-service connection failed: {e}"))),
            Err(_) => Err(KeyCustodyError::BackendUnavailable(format!(
                "key-custody-service did not respond within {:?}",
                self.call_timeout
            ))),
        }
    }

    /// Sends one request and waits for its answer on a free connection:
    /// an idle open one if there is one, else a closed slot (opened now), else
    /// the first to come free. A transport failure closes that connection
    /// only.
    async fn call(&self, request: KeyCustodyRequest) -> Result<KeyCustodyResponse, KeyCustodyError> {
        let mut guard = self.acquire().await;
        if guard.is_none() {
            *guard = Some(open(&self.socket_path).await?);
        }
        let Some(stream) = guard.as_mut() else {
            return Err(KeyCustodyError::BackendUnavailable("no connection to key-custody-service".to_string()));
        };

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

    async fn acquire(&self) -> tokio::sync::MutexGuard<'_, Option<UnixStream>> {
        let mut closed = None;
        for slot in &self.slots {
            if let Ok(guard) = slot.try_lock() {
                if guard.is_some() {
                    return guard;
                }
                if closed.is_none() {
                    closed = Some(guard);
                }
            }
        }
        if let Some(guard) = closed {
            return guard;
        }
        // Every connection is busy: wait for one, spreading waiters out.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let pick = (NEXT.fetch_add(1, Ordering::Relaxed) as usize) % self.slots.len();
        self.slots[pick].lock().await
    }

    /// How many times this client has found the server to have lost its
    /// wallets. See `KeyCustody::check_state`.
    pub fn state_epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }
}

async fn open(path: &Path) -> Result<UnixStream, KeyCustodyError> {
    UnixStream::connect(path).await.map_err(|e| {
        KeyCustodyError::BackendUnavailable(format!("connecting to key-custody-service at {}: {e}", path.display()))
    })
}

fn canary_material() -> WalletMaterial {
    // The canary's spend public key is the view key's public key: any valid
    // point does, since nothing is ever paid to it.
    let view = monero::PrivateKey::from_slice(&CANARY_VIEW_KEY).map(|k| k.to_bytes()).unwrap_or([1u8; 32]);
    let spend = monero::PrivateKey::from_slice(&view)
        .map(|k| monero::PublicKey::from_private_key(&k).to_bytes())
        .unwrap_or([1u8; 32]);
    WalletMaterial::new(view, spend)
}

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
    async fn scan_tx_outputs_for_indices(
        &self,
        handle: WalletHandle,
        tx: &Transaction,
        indices: &ScanIndices,
    ) -> Result<Vec<MatchedOutput>, KeyCustodyError> {
        let covering_range = |indices: &ScanIndices| indices.bounds().map(|(low, high)| low..high.saturating_add(1));
        if self.indices_unsupported.load(Ordering::Relaxed) {
            return match covering_range(indices) {
                None => Ok(Vec::new()),
                Some(range) => self.scan_tx_outputs(handle, tx, 0..1, range).await,
            };
        }
        let request = KeyCustodyRequest::ScanTxOutputsForIndices(ScanTxOutputsForIndicesRequest {
            handle: WalletHandleWire::from(handle),
            tx: TransactionWire::from(tx),
            minors: indices.minors().to_vec(),
        });
        match self.call(request).await {
            Ok(KeyCustodyResponse::ScanTxOutputsForIndices(Ok(matches))) => matches
                .into_iter()
                .map(|m: MatchedOutputWire| {
                    MatchedOutput::try_from(m).map_err(|e| {
                        KeyCustodyError::BackendUnavailable(format!(
                            "key-custody-service returned a malformed matched output: {e}"
                        ))
                    })
                })
                .collect(),
            Ok(KeyCustodyResponse::ScanTxOutputsForIndices(Err(e))) => Err(e.into()),
            Ok(other) => Err(mismatched_response("ScanTxOutputsForIndices", &other)),
            // An older server closes the connection on a request it doesn't
            // know - but so does a server that went down or restarted since
            // this connection was opened. Try again on a fresh connection;
            // only if the server closes that one too while still answering a
            // request every version knows is it really an older server, and
            // then the range request is used from now on. Otherwise the
            // original error stands and index-set requests carry on.
            Err(KeyCustodyError::BackendUnavailable(first)) => {
                let retry = KeyCustodyRequest::ScanTxOutputsForIndices(ScanTxOutputsForIndicesRequest {
                    handle: WalletHandleWire::from(handle),
                    tx: TransactionWire::from(tx),
                    minors: indices.minors().to_vec(),
                });
                let reason = match self.call_fresh(retry).await {
                    Ok(KeyCustodyResponse::ScanTxOutputsForIndices(Ok(matches))) => {
                        return matches
                            .into_iter()
                            .map(|m: MatchedOutputWire| {
                                MatchedOutput::try_from(m).map_err(|e| {
                                    KeyCustodyError::BackendUnavailable(format!(
                                        "key-custody-service returned a malformed matched output: {e}"
                                    ))
                                })
                            })
                            .collect();
                    }
                    Ok(KeyCustodyResponse::ScanTxOutputsForIndices(Err(e))) => return Err(e.into()),
                    Ok(other) => return Err(mismatched_response("ScanTxOutputsForIndices", &other)),
                    Err(KeyCustodyError::BackendUnavailable(reason)) if self.answers_a_known_request().await => reason,
                    Err(_) => return Err(KeyCustodyError::BackendUnavailable(first)),
                };
                tracing::warn!(
                    reason = %reason,
                    "key-custody-service didn't answer an index-set scan; assuming an older server and using range \
                     scans from now on. Upgrade key-custody-server to scan only each store's open orders."
                );
                self.indices_unsupported.store(true, Ordering::Relaxed);
                match covering_range(indices) {
                    None => Ok(Vec::new()),
                    Some(range) => self.scan_tx_outputs(handle, tx, 0..1, range).await,
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Registers a canary wallet the first time, then asks for it: if the
    /// server no longer knows it, the server lost its wallets (it restarted),
    /// so the epoch goes up and a new canary is registered.
    async fn check_state(&self) -> Result<u64, KeyCustodyError> {
        let _one_at_a_time = self.state_check.lock().await;
        let existing = *self.canary.lock();
        match existing {
            None => {
                let handle = self.register_wallet(canary_material()).await?;
                *self.canary.lock() = Some(handle);
            }
            Some(handle) => match self.derive_subaddress(handle, SubaddressIndex::default(), Network::Mainnet).await {
                Ok(_) => {}
                Err(KeyCustodyError::UnknownWallet) => {
                    let epoch = self.epoch.fetch_add(1, Ordering::Relaxed) + 1;
                    // A restarted server may be a newer one: try index-set
                    // scans again.
                    self.indices_unsupported.store(false, Ordering::Relaxed);
                    tracing::warn!(epoch, "key-custody-service has lost its wallets (it restarted?)");
                    let handle = self.register_wallet(canary_material()).await?;
                    *self.canary.lock() = Some(handle);
                }
                Err(e) => return Err(e),
            },
        }
        Ok(self.epoch.load(Ordering::Relaxed))
    }

}
