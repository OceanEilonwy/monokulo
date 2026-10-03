//! The boundary itself: the [`KeyCustody`] trait, the handle callers hold and
//! the types that cross it.

use std::ops::Range;

use monero::blockdata::transaction::TransactionPrefix;
#[cfg(test)]
use monero::consensus::encode::serialize;
pub use monero::cryptonote::subaddress::Index as SubaddressIndex;
use monero::util::ringct::RctSigBase;
pub use monero::Network;
use monero::{Address, PrivateKey, PublicKey, Transaction, ViewPair};
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Opaque reference to a registered wallet's key material.
///
/// Carries no key data itself — it's safe to log, store in SQLite next to a
/// `tenant_id`, and pass across the async/sync boundary freely. Going from a
/// `WalletHandle` back to actual scalars is only possible from inside a
/// `KeyCustody` implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WalletHandle(Uuid);

impl WalletHandle {
    /// Mints a new, random handle (so deliberately no `Default`). Public:
    /// every backend mints its own.
    pub fn generate() -> Self {
        WalletHandle(Uuid::new_v4())
    }
}

/// The watch-only key material for one tenant's wallet: a private view key and a
/// public spend key. Deliberately not a private spend key — nothing in this system
/// is ever able to construct or sign a transaction, only observe them. Losing a
/// `WalletMaterial` compromises a tenant's payment privacy; it never compromises
/// their funds.
///
/// The `Debug` impl redacts the view key on purpose — this type is likely to end up
/// in a request struct at some point, and derived `Debug` would happily print the
/// private key into a log line.
#[derive(Clone, ZeroizeOnDrop)]
pub struct WalletMaterial {
    view_key: [u8; 32],
    #[zeroize(skip)] // public by definition, nothing to protect
    spend_pubkey: [u8; 32],
}

impl WalletMaterial {
    pub fn new(view_key: [u8; 32], spend_pubkey: [u8; 32]) -> Self {
        WalletMaterial {
            view_key,
            spend_pubkey,
        }
    }

    /// Convenience constructor for the admin API, where a tenant pastes both keys as
    /// hex strings copied out of their wallet software.
    pub fn from_hex(view_key_hex: &str, spend_pubkey_hex: &str) -> Result<Self, KeyCustodyError> {
        let view_key = decode_32(view_key_hex, "view key")?;
        let spend_pubkey = decode_32(spend_pubkey_hex, "spend public key")?;
        Ok(WalletMaterial::new(view_key, spend_pubkey))
    }

    /// Only for use inside a `KeyCustody` implementation, to turn sealed/at-rest
    /// material into the crypto types needed to actually scan or derive addresses.
    /// Not `pub(crate)` because real backends (a Nitro or SEV-SNP implementation)
    /// will live in their own crates — but callers outside a `KeyCustody`
    /// implementation should never need this.
    pub fn to_view_pair(&self) -> Result<ViewPair, KeyCustodyError> {
        let view = PrivateKey::from_slice(&self.view_key)
            .map_err(|e| KeyCustodyError::InvalidKeyMaterial(format!("view key: {e}")))?;
        let spend = PublicKey::from_slice(&self.spend_pubkey)
            .map_err(|e| KeyCustodyError::InvalidKeyMaterial(format!("spend public key: {e}")))?;
        Ok(ViewPair { view, spend })
    }

    /// Raw `view_key || spend_pubkey` bytes, for a `KeyCustody` implementation's
    /// `seal` to work with. Same access-scope convention as `to_view_pair`: not
    /// `pub(crate)` since real backends live in their own crates, but nothing
    /// outside a `KeyCustody` implementation should call this.
    pub fn to_raw_bytes(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        out[..32].copy_from_slice(&self.view_key);
        out[32..].copy_from_slice(&self.spend_pubkey);
        out
    }

    /// Inverse of `to_raw_bytes`, for a `KeyCustody` implementation's
    /// `unseal_and_register` to work with.
    pub fn from_raw_bytes(bytes: &[u8]) -> Result<Self, KeyCustodyError> {
        if bytes.len() != 64 {
            return Err(KeyCustodyError::InvalidKeyMaterial(format!(
                "expected 64 bytes (view key || spend pubkey), got {}",
                bytes.len()
            )));
        }
        let mut view_key = [0u8; 32];
        let mut spend_pubkey = [0u8; 32];
        view_key.copy_from_slice(&bytes[..32]);
        spend_pubkey.copy_from_slice(&bytes[32..]);
        Ok(WalletMaterial::new(view_key, spend_pubkey))
    }
}

impl std::fmt::Debug for WalletMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletMaterial")
            .field("view_key", &"<redacted>")
            .field("spend_pubkey", &hex::encode(self.spend_pubkey))
            .finish()
    }
}

fn decode_32(hex_str: &str, what: &'static str) -> Result<[u8; 32], KeyCustodyError> {
    let mut bytes = hex::decode(hex_str)
        .map_err(|e| KeyCustodyError::InvalidKeyMaterial(format!("{what}: {e}")))?;
    if bytes.len() != 32 {
        bytes.zeroize();
        return Err(KeyCustodyError::InvalidKeyMaterial(format!(
            "{what}: expected 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    bytes.zeroize();
    Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum KeyCustodyError {
    #[error("no wallet registered for this handle")]
    UnknownWallet,
    #[error("invalid key material: {0}")]
    InvalidKeyMaterial(String),
    #[error("key custody backend unavailable: {0}")]
    BackendUnavailable(String),
    #[error("scan failed: {0}")]
    ScanFailed(String),
}

/// One transaction output found to belong to a wallet during a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchedOutput {
    /// Position of this output within the transaction (`vout` index).
    pub output_index: usize,
    /// Which subaddress (account/index pair) the output was paid to.
    pub subaddress_index: SubaddressIndex,
    /// Amount in piconero. `None` if the output's amount couldn't be decrypted:
    /// the sender encrypted something other than the amount the output
    /// commits to. Such an output is reported, not an error, so a scan can't
    /// be made to fail by sending a wallet one.
    pub amount_piconero: Option<u64>,
}

/// The outputs of one transaction in a scanned batch that belong to a wallet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxMatches {
    /// Position of the transaction in the batch.
    pub tx: usize,
    /// Its outputs that belong to the wallet; never empty.
    pub outputs: Vec<MatchedOutput>,
}

/// What a scan reads of a transaction: its keys, its outputs and its
/// encrypted amounts. The inputs, ring signatures and range proofs are most
/// of a transaction's bytes and a scan never looks at them, so they are left
/// out of what is handed to a worker thread.
///
/// Made once per transaction and cheap to clone, however many wallets the
/// transaction is scanned for. Like the transaction it comes from, it is
/// public chain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanInput(std::sync::Arc<ScanParts>);

#[derive(Debug, PartialEq, Eq)]
struct ScanParts {
    prefix: TransactionPrefix,
    rct: Option<RctSigBase>,
}

impl ScanInput {
    pub fn of(tx: &Transaction) -> Self {
        ScanInput(std::sync::Arc::new(ScanParts {
            prefix: TransactionPrefix {
                version: tx.prefix.version.clone(),
                unlock_time: tx.prefix.unlock_time.clone(),
                inputs: Vec::new(),
                outputs: tx.prefix.outputs.clone(),
                extra: tx.prefix.extra.clone(),
            },
            rct: tx.rct_signatures.sig.as_ref().map(|rct| RctSigBase {
                rct_type: rct.rct_type,
                txn_fee: rct.txn_fee,
                // One per input, and the inputs are left out.
                pseudo_outs: Vec::new(),
                ecdh_info: rct.ecdh_info.clone(),
                out_pk: rct.out_pk.clone(),
            }),
        }))
    }

    /// The transaction's prefix, without its inputs: the transaction keys
    /// (in `extra`) and the outputs.
    pub fn prefix(&self) -> &TransactionPrefix {
        &self.0.prefix
    }

    /// The encrypted amounts and commitments of the outputs, if the
    /// transaction has them.
    pub fn rct(&self) -> Option<&RctSigBase> {
        self.0.rct.as_ref()
    }
}

/// The boundary between "the rest of the payment service" and "wherever tenant view
/// keys actually live." See the module docs for the threat model this exists to
/// narrow.
///
/// A note on the `major_range`/`minor_range` parameters on `scan_tx_outputs`
/// (`derive_subaddress` takes a single `index` instead, not a range - see its own
/// signature below): building a lookup table for a range
/// costs one scalar multiplication per candidate index, because that's how the
/// underlying primitive works — it derives every spend key in the range up front,
/// then checks outputs against the resulting table. `PlainKeyCustody` amortizes
/// this by caching the table per wallet and rebuilding only when the requested range
/// actually changes, so a tenant with a stable set of pending orders pays that cost
/// once, not once per transaction scanned. That said, callers should still keep
/// ranges as tight as possible around currently-*active* (non-terminal) orders — a
/// cache only helps once it's warm, and the *first* scan after a range widens still
/// pays the full construction cost, so an ever-growing `0..next_free_index` (rather
/// than recycling minor indices from completed orders) would mean that cost keeps
/// growing indefinitely instead of staying flat. Any other `KeyCustody`
/// implementation — a future TEE-backed one included — should assume the same
/// applies and cache accordingly; this trait doesn't hide the cost model, on
/// purpose, since it's a real constraint of Monero's stealth-address design.
#[async_trait::async_trait]
pub trait KeyCustody: Send + Sync {
    /// Take ownership of freshly-submitted watch-only key material (e.g. a tenant
    /// pasting keys into the onboarding form) and return an opaque handle to it.
    /// `material` is consumed; implementations must not retain it in any form the
    /// caller can reach afterwards.
    async fn register_wallet(
        &self,
        material: WalletMaterial,
    ) -> Result<WalletHandle, KeyCustodyError>;

    /// Forget a wallet's key material entirely (tenant offboarding).
    async fn remove_wallet(&self, handle: WalletHandle) -> Result<(), KeyCustodyError>;

    /// Produce the bytes a caller should persist for this wallet — e.g. in a
    /// `tenants` table row — so it can be re-registered after a process restart,
    /// when `PlainKeyCustody`'s in-memory registry (and any handle it issued) is
    /// gone. This is deliberately a *separate* concern from the rest of this trait:
    /// everything else here is about protecting keys while they're in use; this
    /// pair (`seal`/`unseal_and_register`) is about protecting them at rest.
    /// `PlainKeyCustody` seals to plain bytes, matching its "no encryption" stance
    /// everywhere else. A TEE-backed implementation should seal to something only
    /// it (or an enclave with the same measurement) can unseal, so a stolen
    /// database file is not sufficient to recover a tenant's view key even though
    /// this same backend's job during scanning is to keep it in the clear.
    async fn seal(&self, material: &WalletMaterial) -> Result<Vec<u8>, KeyCustodyError>;

    /// Inverse of `seal`, run once at startup per stored wallet. Combines unsealing
    /// with registration in one call — sealed bytes should never come back out as a
    /// `WalletMaterial` a caller could hold onto; the only thing that leaves this
    /// call is a fresh `WalletHandle`.
    async fn unseal_and_register(&self, sealed: &[u8]) -> Result<WalletHandle, KeyCustodyError>;

    /// Retryable registration for a stored tenant. Reusing `registration_id`
    /// returns the same handle if the server completed an earlier attempt but
    /// its response was lost. Backends without remote state may use the default.
    async fn unseal_and_register_idempotent(
        &self,
        sealed: &[u8],
        registration_id: &str,
    ) -> Result<WalletHandle, KeyCustodyError> {
        let _ = registration_id;
        self.unseal_and_register(sealed).await
    }

    /// Derive the receiving address for one subaddress index. Requires the private
    /// view key internally, which is exactly why it lives behind this boundary
    /// rather than in the order-creation code path.
    async fn derive_subaddress(
        &self,
        handle: WalletHandle,
        index: SubaddressIndex,
        network: Network,
    ) -> Result<Address, KeyCustodyError>;

    /// Check every output of `tx` against the given index ranges, returning the
    /// ones that belong to this wallet. `tx` is public blockchain data (from the
    /// mempool or a confirmed block) — nothing about the transaction itself is
    /// sensitive, only the key used to check it.
    async fn scan_tx_outputs(
        &self,
        handle: WalletHandle,
        tx: &ScanInput,
        major_range: Range<u32>,
        minor_range: Range<u32>,
    ) -> Result<Vec<MatchedOutput>, KeyCustodyError>;

    /// Check every output of every transaction in `txs` against a set of
    /// minor indices (account 0), not necessarily contiguous: the indices of
    /// one store's orders that are open or recently closed
    /// (admin_settings_v2.md task 7.3). Returns the transactions that pay
    /// the wallet, in order, each with its position in `txs`.
    ///
    /// A whole batch in one call, because a call has a cost of its own (a
    /// hop to a worker thread, or a round trip to another process) that is
    /// not worth paying per transaction: nearly every transaction pays the
    /// wallet nothing. The call fails or succeeds as a whole.
    ///
    /// The set's `generation` changes whenever its contents do, so an
    /// implementation can keep a table per wallet and update it only when
    /// the set changes.
    ///
    /// The default scans the transactions one by one, covering the set with
    /// one contiguous range (`min..=max`), which is correct but builds a
    /// bigger table than needed; backends that can do better override it.
    /// A `Range<u32>` can't name `u32::MAX`, so the default never matches
    /// that one index; no store reaches it (indices are claimed from 0, one
    /// per order).
    async fn scan_txs_for_indices(
        &self,
        handle: WalletHandle,
        txs: &[ScanInput],
        indices: &ScanIndices,
    ) -> Result<Vec<TxMatches>, KeyCustodyError> {
        let Some((low, high)) = indices.bounds() else {
            return Ok(Vec::new());
        };
        let mut found = Vec::new();
        for (tx, input) in txs.iter().enumerate() {
            let outputs = self
                .scan_tx_outputs(handle, input, 0..1, low..high.saturating_add(1))
                .await?;
            if !outputs.is_empty() {
                found.push(TxMatches { tx, outputs });
            }
        }
        Ok(found)
    }
    /// Checks whether the backend still holds the wallets registered with it
    /// and returns its "state epoch", which goes up each time the backend is
    /// found to have lost them (a key-custody sidecar restarted with empty
    /// memory). Every handle issued before the change is then useless, and
    /// the caller must register all its wallets again from their sealed
    /// material (admin_settings_v2.md task 5.8). A backend that can't lose
    /// its wallets independently of this process keeps the default: always 0.
    async fn check_state(&self) -> Result<u64, KeyCustodyError> {
        Ok(0)
    }

    /// Why this backend can't serve right now, if it can't, though it holds
    /// its wallets (`check_state` is about losing them): for the backend's
    /// health. A backend that is always ready keeps the default.
    fn unavailable(&self) -> Option<String> {
        None
    }

    // -- Per-store backends (admin_settings_v2.md part 5) -----------------
    //
    // An engine can hold several backends at once, each store's keys in the
    // backend its row names. These take the backend by name. A single
    // backend ignores the name (it is the only one there is); a router over
    // several (`engine::key_custody::CustodyRouter`) uses it to pick one,
    // and routes every other call by the handle, which it remembers.

    /// `register_wallet`, in the backend called `backend`.
    async fn register_wallet_in(
        &self,
        backend: &str,
        material: WalletMaterial,
    ) -> Result<WalletHandle, KeyCustodyError> {
        let _ = backend;
        self.register_wallet(material).await
    }

    /// `unseal_and_register`, in the backend called `backend`.
    async fn unseal_and_register_in(
        &self,
        backend: &str,
        sealed: &[u8],
    ) -> Result<WalletHandle, KeyCustodyError> {
        let _ = backend;
        self.unseal_and_register(sealed).await
    }

    async fn unseal_and_register_in_idempotent(
        &self,
        backend: &str,
        sealed: &[u8],
        registration_id: &str,
    ) -> Result<WalletHandle, KeyCustodyError> {
        let _ = backend;
        self.unseal_and_register_idempotent(sealed, registration_id)
            .await
    }

    /// `seal`, by the backend called `backend`.
    async fn seal_in(
        &self,
        backend: &str,
        material: &WalletMaterial,
    ) -> Result<Vec<u8>, KeyCustodyError> {
        let _ = backend;
        self.seal(material).await
    }

    /// Whether `handle` still refers to a wallet in a backend that is enabled
    /// and hasn't lost its wallets. A caller holding handles drops the ones
    /// that aren't, so they get registered again (or, for a disabled
    /// backend, the store is left unserved).
    fn handle_is_live(&self, handle: WalletHandle) -> bool {
        let _ = handle;
        true
    }

    /// The backends enabled right now, by name; empty when this isn't a
    /// router (a single backend, whatever it is called).
    fn enabled_backends(&self) -> Vec<String> {
        Vec::new()
    }

    /// Each enabled backend's health, by name: `None` if it answers, else
    /// why not. Empty when this isn't a router.
    async fn backend_health(&self) -> Vec<(String, Option<String>)> {
        Vec::new()
    }

    // -- Keys sent encrypted to the backend (`crate::transport`) ----------

    /// Whether this backend takes a merchant's keys in the clear
    /// (`register_wallet`). One that doesn't takes them only through
    /// `register_envelope`.
    fn takes_raw_keys(&self) -> bool {
        true
    }

    /// Issues a single-use challenge for `action` (and, for a move, `store`)
    /// and returns the bundle a merchant's client checks and encrypts their
    /// keys against. Only backends that take encrypted keys have one.
    async fn key_bundle(
        &self,
        action: crate::transport::Action,
        store: Option<&str>,
    ) -> Result<crate::transport::Bundle, KeyCustodyError> {
        let _ = (action, store);
        Err(KeyCustodyError::InvalidKeyMaterial(
            "this key custody backend takes keys as they are, not encrypted".into(),
        ))
    }

    /// Registers keys a merchant sent encrypted to this backend under a
    /// challenge it issued for `action` and `store`. Returns the handle and
    /// the sealed bytes to store, as `seal` would give for the same keys: the
    /// keys themselves never leave the backend.
    async fn register_envelope(
        &self,
        envelope: &crate::transport::Envelope,
        action: crate::transport::Action,
        store: Option<&str>,
    ) -> Result<(WalletHandle, Vec<u8>), KeyCustodyError> {
        let _ = (envelope, action, store);
        Err(KeyCustodyError::InvalidKeyMaterial(
            "this key custody backend takes keys as they are, not encrypted".into(),
        ))
    }

    /// `takes_raw_keys`, of the backend called `backend`.
    fn takes_raw_keys_in(&self, backend: &str) -> bool {
        let _ = backend;
        self.takes_raw_keys()
    }

    /// `key_bundle`, from the backend called `backend`.
    async fn key_bundle_in(
        &self,
        backend: &str,
        action: crate::transport::Action,
        store: Option<&str>,
    ) -> Result<crate::transport::Bundle, KeyCustodyError> {
        let _ = backend;
        self.key_bundle(action, store).await
    }

    /// `register_envelope`, in the backend called `backend`.
    async fn register_envelope_in(
        &self,
        backend: &str,
        envelope: &crate::transport::Envelope,
        action: crate::transport::Action,
        store: Option<&str>,
    ) -> Result<(WalletHandle, Vec<u8>), KeyCustodyError> {
        let _ = backend;
        self.register_envelope(envelope, action, store).await
    }
}

/// A set of minor subaddress indices (account 0) to scan for, sorted and
/// without duplicates, with a generation that identifies its contents. Cheap
/// to clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanIndices {
    minors: std::sync::Arc<Vec<u32>>,
    generation: u64,
}

impl ScanIndices {
    pub fn new(minors: impl IntoIterator<Item = u32>) -> Self {
        let mut minors: Vec<u32> = minors.into_iter().collect();
        minors.sort_unstable();
        minors.dedup();
        // FNV-1a over the indices: equal sets always get equal generations.
        let mut generation: u64 = 0xcbf29ce484222325;
        for minor in &minors {
            for byte in minor.to_le_bytes() {
                generation ^= u64::from(byte);
                generation = generation.wrapping_mul(0x100000001b3);
            }
        }
        ScanIndices {
            minors: std::sync::Arc::new(minors),
            generation,
        }
    }

    /// Every index in `range`.
    pub fn range(range: Range<u32>) -> Self {
        ScanIndices::new(range)
    }

    pub fn minors(&self) -> &[u32] {
        &self.minors
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn len(&self) -> usize {
        self.minors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.minors.is_empty()
    }

    /// Lowest and highest index, if any.
    pub fn bounds(&self) -> Option<(u32, u32)> {
        Some((*self.minors.first()?, *self.minors.last()?))
    }
}

/// Whether `material` is the wallet `address` belongs to, on `network`: the
/// public spend key must match, and so must the public view key derived
/// from the private view key.
///
/// Compares keys, not strings, so any valid spelling of the address works.
/// `Err` if the address doesn't parse or the keys are malformed.
pub fn wallet_matches_address(
    material: &WalletMaterial,
    address: &str,
    network: Network,
) -> Result<bool, String> {
    let address: monero::Address = address
        .parse()
        .map_err(|e| format!("{address:?} is not a Monero address: {e}"))?;
    let pair = material.to_view_pair().map_err(|e| e.to_string())?;
    Ok(address.network == network
        && address.public_spend == pair.spend
        && address.public_view == monero::PublicKey::from_private_key(&pair.view))
}

/// Removes `handle` from `custody`, best effort, and logs a failure.
///
/// Until the backend restarts, a removal that failed leaves a copy of a
/// store's view key live in it. `UnknownWallet` means the handle is already
/// gone, which is what was wanted, so it is not logged. `store_id` is the
/// store the handle belonged to, when the caller knows it; `context` says
/// what was being done, for the log line.
pub async fn remove_wallet_logged(
    custody: &dyn KeyCustody,
    handle: WalletHandle,
    store_id: Option<&str>,
    context: &str,
) {
    let Err(e) = custody.remove_wallet(handle).await else {
        return;
    };
    if matches!(e, KeyCustodyError::UnknownWallet) {
        return;
    }
    if let Some(store_id) = store_id {
        tracing::warn!(
            store.id = %store_id,
            error = %e,
            "{context}: removing a store's keys from key custody failed, so a copy stays there until the backend restarts"
        );
    } else {
        tracing::warn!(
            wallet.handle = ?handle,
            error = %e,
            "{context}: removing a store's keys from key custody failed, so a copy stays there until the backend restarts"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_tx() -> Transaction {
        let raw = hex::decode(include_str!(
            "../../engine/tests/fixtures/subaddress_tx.hex"
        ))
        .unwrap();
        monero::consensus::encode::deserialize(&raw).unwrap()
    }

    #[test]
    fn a_scan_input_keeps_what_a_scan_reads_and_leaves_out_the_rest() {
        let tx = fixture_tx();
        let input = ScanInput::of(&tx);

        assert!(input.prefix().inputs.is_empty());
        assert_eq!(input.prefix().outputs, tx.prefix.outputs);
        assert_eq!(input.prefix().extra, tx.prefix.extra);
        let rct = tx.rct_signatures.sig.as_ref().unwrap();
        let kept = input.rct().unwrap();
        assert_eq!(kept.rct_type, rct.rct_type);
        assert_eq!(kept.ecdh_info, rct.ecdh_info);
        assert_eq!(kept.out_pk, rct.out_pk);
        assert!(
            serialize(input.prefix()).len() * 4 < serialize(&tx).len(),
            "{} bytes of a {}-byte transaction",
            serialize(input.prefix()).len(),
            serialize(&tx).len()
        );
    }
}
