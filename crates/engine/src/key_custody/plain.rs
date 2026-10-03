use parking_lot::RwLock;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{compiler_fence, Ordering};
use std::sync::Arc;

use monero::cryptonote::onetime_key::SubKeyChecker;
use monero::{Address, PrivateKey, PublicKey, ViewPair};
use zeroize::Zeroize as _;

use super::outputs::{owned_outputs, pays};
use super::{
    KeyCustody, KeyCustodyError, MatchedOutput, Network, ScanIndices, ScanInput, SubaddressIndex,
    TxMatches, WalletHandle, WalletMaterial,
};

/// Ceiling on `major_range.len() * minor_range.len()` for one scan. Table
/// construction is one scalar multiplication per candidate index (see
/// [`SubKeyChecker::new`]), so an unbounded range isn't slow, it's a hang: a caller
/// that accidentally passed `0..u32::MAX` would wedge the scanner thread for the
/// rest of the process's life with no error and no progress, and every payment for
/// every tenant would stop being detected. Refusing loudly is the only safe
/// behaviour. 1M entries is already far past anything legitimate - the trait docs
/// ask callers to keep ranges tight around *active* orders - while still leaving
/// enormous headroom over the `0..next_minor_index` ranges the scanner actually
/// issues.
const MAX_SCAN_TABLE_ENTRIES: u64 = 1_000_000;

/// A table of derived spend keys worth keeping between scans: building it
/// costs one scalar multiplication per index, which is what makes rebuilding
/// it for every transaction wasteful.
#[derive(Default)]
struct KeyTable {
    /// The set the table covers whole; `None` while empty or being built.
    /// Compared as a set, not by `ScanIndices::generation` (a 64-bit hash:
    /// two windows colliding would keep a table for the wrong one, and
    /// payments to the new indices would be missed with no error).
    covers: Option<ScanIndices>,
    indices: std::collections::BTreeSet<u32>,
    table: HashMap<PublicKey, SubaddressIndex>,
    building: Option<ScanIndices>,
    pending_indices: Vec<u32>,
}

impl KeyTable {
    /// Makes the table cover exactly `indices`, deriving only indices it
    /// didn't have and dropping ones no longer wanted (task 7.3: a store's
    /// scan window changes by an order or two at a time).
    fn update_batch_to(&mut self, view_pair: &ViewPair, indices: &ScanIndices) -> u64 {
        if self.covers.as_ref() == Some(indices) {
            return 0;
        }
        if self.building.as_ref() != Some(indices) {
            let wanted: std::collections::BTreeSet<u32> =
                indices.minors().iter().copied().collect();
            self.table
                .retain(|_, index| wanted.contains(&index.minor) && index.major == 0);
            self.indices.retain(|index| wanted.contains(index));
            self.pending_indices = wanted.difference(&self.indices).copied().collect();
            self.building = Some(indices.clone());
            self.covers = None;
        }
        let mut derived = 0;
        for _ in 0..SCAN_TABLE_BUILD_BATCH {
            let Some(minor) = self.pending_indices.pop() else {
                break;
            };
            let index = SubaddressIndex { major: 0, minor };
            self.table.insert(
                monero::cryptonote::subaddress::get_spend_public_key(view_pair, index),
                index,
            );
            self.indices.insert(minor);
            derived += 1;
        }
        if self.pending_indices.is_empty() {
            self.covers = self.building.take();
        }
        derived
    }
}

const SCAN_TABLE_BUILD_BATCH: usize = 256;

#[derive(Default)]
struct RangeTable {
    range: Option<(u32, u32, u32, u32)>,
    next_major: u32,
    next_minor: u32,
    complete: bool,
    table: HashMap<PublicKey, SubaddressIndex>,
}

impl RangeTable {
    fn update_batch(
        &mut self,
        view_pair: &ViewPair,
        major: &Range<u32>,
        minor: &Range<u32>,
    ) -> usize {
        let range = (major.start, major.end, minor.start, minor.end);
        if self.range != Some(range) {
            self.range = Some(range);
            self.table.clear();
            self.next_major = major.start;
            self.next_minor = minor.start;
            self.complete = major.is_empty() || minor.is_empty();
        }
        let mut derived = 0;
        for _ in 0..SCAN_TABLE_BUILD_BATCH {
            if self.complete {
                break;
            }
            let index = SubaddressIndex {
                major: self.next_major,
                minor: self.next_minor,
            };
            self.table.insert(
                monero::cryptonote::subaddress::get_spend_public_key(view_pair, index),
                index,
            );
            derived += 1;
            if self.next_minor + 1 < minor.end {
                self.next_minor += 1;
            } else {
                self.next_minor = minor.start;
                if self.next_major + 1 < major.end {
                    self.next_major += 1;
                } else {
                    self.complete = true;
                }
            }
        }
        derived
    }
}

struct WalletEntry {
    view_pair: ViewPair,
    /// The table for this wallet's live scan window, updated incrementally.
    /// An async mutex: the table is moved into a blocking scan and back, so
    /// scans of one wallet run one at a time (different wallets in parallel)
    /// and no table is ever copied.
    live: Arc<tokio::sync::Mutex<KeyTable>>,
    lookup: Arc<tokio::sync::Mutex<RangeTable>>,
    /// Derivations done so far: how much elliptic-curve work this wallet's
    /// scan tables have cost, and how tests check nothing is rebuilt.
    derivations: std::sync::atomic::AtomicU64,
}

impl WalletEntry {
    fn new(view_pair: ViewPair) -> Self {
        Self {
            view_pair,
            live: Arc::new(tokio::sync::Mutex::new(KeyTable::default())),
            lookup: Arc::new(tokio::sync::Mutex::new(RangeTable::default())),
            derivations: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

/// Scans are elliptic-curve work. They run on the blocking pool (task 7.2),
/// never on the async workers that serve requests, and at most one per CPU
/// the engine may use at a time: its size, and the slots.
static SCAN_SLOTS: std::sync::OnceLock<(usize, Arc<tokio::sync::Semaphore>)> =
    std::sync::OnceLock::new();

/// Sets how many scans may run at once, and returns the size in effect.
///
/// One per CPU the engine may use (`server.cpus`,
/// `threads::ThreadPlan::scan_slots`). `Engine::start` calls it before any
/// scan; the first size stays for the process, which holds one engine.
pub fn size_scan_slots(slots: usize) -> usize {
    SCAN_SLOTS
        .get_or_init(|| {
            let slots = slots.max(1);
            (slots, Arc::new(tokio::sync::Semaphore::new(slots)))
        })
        .0
}

/// The slots, one per CPU the process may use unless the engine said
/// otherwise first (an engine-less test or tool scanning directly).
fn scan_slots() -> Arc<tokio::sync::Semaphore> {
    let cores = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
    size_scan_slots(cores);
    SCAN_SLOTS.get().map_or_else(
        || Arc::new(tokio::sync::Semaphore::new(cores)),
        |(_, slots)| Arc::clone(slots),
    )
}

/// Best-effort scrub of the one *long-lived* copy of a tenant's view key: the one
/// this registry holds for the life of the process, and the one still sitting in
/// freed heap after a tenant offboards via `remove_wallet`.
///
/// Deliberately scoped, because `monero::PrivateKey` is `Copy` and so is
/// `ViewPair` - every `SubKeyChecker`/`KeyGenerator` call the crate makes takes its
/// own transient copy that nothing here can track or clear. Chasing those would
/// require the upstream crate to zeroize, not us. What this *does* buy is that
/// offboarding a tenant doesn't leave their view key readable in freed memory for
/// the remaining uptime of a long-running process, which is the difference between
/// "compromise the box now" and "compromise the box any time later".
///
/// The volatile write is the load-bearing part: a plain store into a value
/// about to be dropped is exactly the kind of dead store LLVM is free to
/// delete, and a `compiler_fence` only orders memory operations, it doesn't
/// keep one. This is `zeroize`'s own technique (`ptr::write_volatile` then a
/// fence); `zeroize` itself can't be used directly because `PrivateKey`
/// exposes no mutable view of its scalar's bytes, but it is `Copy`, so the
/// whole value is written at once.
impl Drop for WalletEntry {
    #[expect(
        unsafe_code,
        clippy::volatile_composites,
        reason = "the view-key scrub: a volatile write is the only store LLVM may not \
                  delete, and `PrivateKey` offers no bytes for `zeroize`; the write may \
                  be split, which is fine, since every byte only has to end up zero"
    )]
    fn drop(&mut self) {
        if let Ok(zero) = PrivateKey::from_slice(&[0u8; 32]) {
            // SAFETY: `self.view_pair.view` is a valid, aligned, initialised
            // `PrivateKey` owned by `self` for the whole of `drop`; a
            // volatile write of another `PrivateKey` over it is the same
            // write the assignment would do, only not elidable.
            unsafe {
                std::ptr::write_volatile(&raw mut self.view_pair.view, zero);
            }
        }
        compiler_fence(Ordering::SeqCst);
    }
}

/// Reference `KeyCustody` implementation: view pairs live in this process's ordinary
/// memory behind a lock, with no encryption at rest and no isolation from the host
/// process.
///
/// See the module-level docs for why that's an acceptable default for a self-hosted,
/// single-tenant deployment and not for a multi-tenant hosted one.
#[derive(Default)]
pub struct PlainKeyCustody {
    wallets: RwLock<HashMap<WalletHandle, Arc<WalletEntry>>>,
    registration_ids: RwLock<HashMap<String, WalletHandle>>,
}

#[async_trait::async_trait]
impl KeyCustody for PlainKeyCustody {
    async fn register_wallet(
        &self,
        material: WalletMaterial,
    ) -> Result<WalletHandle, KeyCustodyError> {
        let view_pair = material.to_view_pair()?;
        let handle = WalletHandle::generate();
        self.wallets
            .write()
            .insert(handle, Arc::new(WalletEntry::new(view_pair)));
        Ok(handle)
    }

    async fn remove_wallet(&self, handle: WalletHandle) -> Result<(), KeyCustodyError> {
        // Keep lock order the same as idempotent registration.
        self.registration_ids
            .write()
            .retain(|_, registered| *registered != handle);
        self.wallets
            .write()
            .remove(&handle)
            .map(|_| ())
            .ok_or(KeyCustodyError::UnknownWallet)
    }

    async fn seal(&self, material: &WalletMaterial) -> Result<Vec<u8>, KeyCustodyError> {
        // "No encryption" is the entire point of this backend - sealing is a no-op
        // serialization, not a security boundary. A stolen database file is exactly
        // as sensitive as this wallet's key material, which is the expected
        // tradeoff for a self-hosted, single-tenant deployment.
        //
        // The returned `Vec` necessarily carries the view key (that's what the
        // caller persists), but the fixed-size staging buffer `to_raw_bytes` hands
        // back is a second copy with no reason to outlive this call - scrub it
        // rather than leaving it in the freed stack frame.
        let mut raw = material.to_raw_bytes();
        let sealed = raw.to_vec();
        raw.zeroize();
        Ok(sealed)
    }

    async fn unseal_and_register(&self, sealed: &[u8]) -> Result<WalletHandle, KeyCustodyError> {
        let material = WalletMaterial::from_raw_bytes(sealed)?;
        self.register_wallet(material).await
    }

    async fn unseal_and_register_idempotent(
        &self,
        sealed: &[u8],
        registration_id: &str,
    ) -> Result<WalletHandle, KeyCustodyError> {
        if registration_id.is_empty() || registration_id.len() > 128 {
            return Err(KeyCustodyError::InvalidKeyMaterial(
                "invalid registration id".into(),
            ));
        }
        let material = WalletMaterial::from_raw_bytes(sealed)?;
        let view_pair = material.to_view_pair()?;
        let mut registrations = self.registration_ids.write();
        let mut wallets = self.wallets.write();
        if let Some(&handle) = registrations.get(registration_id) {
            if let Some(existing) = wallets.get(&handle) {
                if existing.view_pair != view_pair {
                    return Err(KeyCustodyError::InvalidKeyMaterial(
                        "registration id belongs to another wallet".into(),
                    ));
                }
                return Ok(handle);
            }
            registrations.remove(registration_id);
        }
        let handle = WalletHandle::generate();
        wallets.insert(handle, Arc::new(WalletEntry::new(view_pair)));
        registrations.insert(registration_id.to_owned(), handle);
        Ok(handle)
    }

    async fn derive_subaddress(
        &self,
        handle: WalletHandle,
        index: SubaddressIndex,
        network: Network,
    ) -> Result<Address, KeyCustodyError> {
        let wallets = self.wallets.read();
        let entry = wallets.get(&handle).ok_or(KeyCustodyError::UnknownWallet)?;
        if index.is_zero() {
            // 0/0 is the account's *standard* address, not a subaddress - its keys
            // are the unmodified root pair `(v*G, S)`. `get_subaddress` returns
            // exactly those keys but still stamps the result with the *subaddress*
            // network byte, and that combination is not merely cosmetic: it is
            // unpayable. A sender seeing a subaddress-tagged address derives the tx
            // pubkey as `R = r*D` and the shared secret from `8*r*C`, while this
            // wallet looks for `8*v*R`. Those agree only when `C = v*D`, which holds
            // for every genuine subaddress and never for the root pair (there
            // `C = v*G` but `v*D = v*S`). So funds sent to that string would be
            // permanently undetectable by the very wallet it names - see
            // `admin::create_tenant`, which reports this address to the merchant as
            // their `primary_address`. Every non-zero index goes through the
            // subaddress path unchanged.
            return Ok(Address::standard(
                network,
                entry.view_pair.spend,
                PublicKey::from_private_key(&entry.view_pair.view),
            ));
        }
        Ok(monero::cryptonote::subaddress::get_subaddress(
            &entry.view_pair,
            index,
            Some(network),
        ))
    }

    async fn scan_tx_outputs(
        &self,
        handle: WalletHandle,
        tx: &ScanInput,
        major_range: Range<u32>,
        minor_range: Range<u32>,
    ) -> Result<Vec<MatchedOutput>, KeyCustodyError> {
        let table_entries = (major_range.end.saturating_sub(major_range.start) as u64)
            .saturating_mul(minor_range.end.saturating_sub(minor_range.start) as u64);
        if table_entries > MAX_SCAN_TABLE_ENTRIES {
            return Err(KeyCustodyError::ScanFailed(format!(
                "requested subaddress range {}x{} exceeds the {MAX_SCAN_TABLE_ENTRIES}-entry scan limit",
                major_range.end.saturating_sub(major_range.start),
                minor_range.end.saturating_sub(minor_range.start),
            )));
        }
        let entry = self.entry(handle)?;
        // Keep one independent lookup range per wallet. Each completed CPU
        // batch stays in the cache even if the caller is cancelled while the
        // blocking worker runs; a retry resumes instead of starting at zero.
        let mut lookup = Arc::clone(&entry.lookup).lock_owned().await;
        while lookup.range
            != Some((
                major_range.start,
                major_range.end,
                minor_range.start,
                minor_range.end,
            ))
            || !lookup.complete
        {
            let view_pair = entry.view_pair;
            let major = major_range.clone();
            let minor = minor_range.clone();
            let permit = scan_slots().acquire_owned().await;
            let (returned, derived) = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let derived = lookup.update_batch(&view_pair, &major, &minor);
                (lookup, derived)
            })
            .await
            .map_err(|e| {
                KeyCustodyError::ScanFailed(format!("building the scan table failed: {e}"))
            })?;
            lookup = returned;
            entry
                .derivations
                .fetch_add(derived as u64, Ordering::Relaxed);
            if !lookup.complete {
                tokio::task::yield_now().await;
            }
        }
        let view_pair = entry.view_pair;
        let tx = tx.clone();
        let permit = scan_slots().acquire_owned().await;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if !pays(&view_pair, &lookup.table, &tx) {
                return Ok(Vec::new());
            }
            let keys = std::mem::take(&mut lookup.table);
            let cached_range = lookup.range.take();
            lookup.complete = false;
            let checker = SubKeyChecker {
                table: keys,
                keys: &view_pair,
            };
            let found = owned_outputs(&checker, &tx);
            lookup.table = checker.table;
            lookup.range = cached_range;
            lookup.complete = true;
            Ok(found)
        })
        .await
        .map_err(|e| KeyCustodyError::ScanFailed(format!("scan task failed: {e}")))?
    }

    async fn scan_txs_for_indices(
        &self,
        handle: WalletHandle,
        txs: &[ScanInput],
        indices: &ScanIndices,
    ) -> Result<Vec<TxMatches>, KeyCustodyError> {
        if indices.len() as u64 > MAX_SCAN_TABLE_ENTRIES {
            return Err(KeyCustodyError::ScanFailed(format!(
                "{} indices exceed the {MAX_SCAN_TABLE_ENTRIES}-entry scan limit",
                indices.len()
            )));
        }
        let entry = self.entry(handle)?;
        // The owned guard travels with each blocking task. Dropping this
        // future cannot discard work already running: that task finishes one
        // bounded batch and leaves the table, including partial progress, in
        // the wallet's cache before releasing the guard.
        let mut live = Arc::clone(&entry.live).lock_owned().await;
        while live.covers.as_ref() != Some(indices) {
            let view_pair = entry.view_pair;
            let wanted = indices.clone();
            let permit = scan_slots().acquire_owned().await;
            let (returned, derived) = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let derived = live.update_batch_to(&view_pair, &wanted);
                (live, derived)
            })
            .await
            .map_err(|e| {
                KeyCustodyError::ScanFailed(format!("updating the scan table failed: {e}"))
            })?;
            live = returned;
            entry.derivations.fetch_add(derived, Ordering::Relaxed);
            if live.covers.as_ref() != Some(indices) {
                tokio::task::yield_now().await;
            }
        }
        let txs = txs.to_vec();
        let view_pair = entry.view_pair;
        let permit = scan_slots().acquire_owned().await;
        // The whole batch in one blocking task: the hop to the blocking pool
        // and back costs more than finding that a transaction pays nothing.
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut found = Vec::new();
            for (tx, input) in txs.iter().enumerate() {
                if !pays(&view_pair, &live.table, input) {
                    continue;
                }
                // If `owned_outputs` panics, the next call must rebuild, not
                // treat an emptied table as a completed generation.
                let keys = std::mem::take(&mut live.table);
                let cached_indices = std::mem::take(&mut live.indices);
                let covers = live.covers.take();
                let checker = SubKeyChecker {
                    table: keys,
                    keys: &view_pair,
                };
                let outputs = owned_outputs(&checker, input);
                live.table = checker.table;
                live.indices = cached_indices;
                live.covers = covers;
                if !outputs.is_empty() {
                    found.push(TxMatches { tx, outputs });
                }
            }
            Ok(found)
        })
        .await
        .map_err(|e| KeyCustodyError::ScanFailed(format!("scan task failed: {e}")))?
    }
}

impl PlainKeyCustody {
    /// How many wallets are registered (for tests that check nothing is
    /// left behind).
    pub fn wallet_count(&self) -> usize {
        self.wallets.read().len()
    }

    fn entry(&self, handle: WalletHandle) -> Result<Arc<WalletEntry>, KeyCustodyError> {
        self.wallets
            .read()
            .get(&handle)
            .cloned()
            .ok_or(KeyCustodyError::UnknownWallet)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use monero::consensus::encode::deserialize;
    use monero::Transaction;

    fn random_scalar_bytes(seed: u8) -> [u8; 32] {
        // Not cryptographically random - deterministic per-test fixture data only.
        let mut bytes = [seed; 32];
        bytes[31] &= 0x0f; // keep well under the group order so it's a valid scalar
        bytes
    }

    #[tokio::test]
    async fn register_then_derive_subaddress_roundtrip() {
        let view_key = PrivateKey::from_slice(&random_scalar_bytes(1)).unwrap();
        let spend_key = PrivateKey::from_slice(&random_scalar_bytes(2)).unwrap();
        let spend_pubkey = PublicKey::from_private_key(&spend_key);

        let custody = PlainKeyCustody::default();
        let handle = custody
            .register_wallet(WalletMaterial::new(
                view_key.to_bytes(),
                spend_pubkey.to_bytes(),
            ))
            .await
            .unwrap();

        let primary = custody
            .derive_subaddress(handle, SubaddressIndex::default(), Network::Mainnet)
            .await
            .unwrap();

        // 0/0 is the account's primary keyset, so the derived keys should equal the
        // root view pair directly - though `get_subaddress` still tags the address
        // as `AddressType::SubAddress` rather than `Standard` even at index zero,
        // which is a quirk of the underlying primitive, not something we need to
        // paper over here.
        assert_eq!(primary.public_spend, spend_pubkey);
        assert_eq!(primary.public_view, PublicKey::from_private_key(&view_key));

        custody.remove_wallet(handle).await.unwrap();
        let err = custody
            .derive_subaddress(handle, SubaddressIndex::default(), Network::Mainnet)
            .await
            .unwrap_err();
        assert!(matches!(err, KeyCustodyError::UnknownWallet));
    }

    #[tokio::test]
    async fn scan_tx_outputs_finds_output_paid_to_subaddress() {
        // Fixture transaction and key pair lifted byte-for-byte from monero-rs's own
        // `code_coverage_owned_tx_out` test in blockdata/transaction.rs, which is
        // known to contain one output paying subaddress 0/1 for this exact wallet -
        // it exercises the real RingCT amount-decryption path, not just key
        // matching. We only ever hand this boundary the *private view key* plus the
        // *public* spend key (derived here from the fixture's private spend key,
        // the same way a real caller would derive it from a tenant-submitted watch
        // key), matching the watch-only shape `KeyCustody` is built around.
        let raw_tx = hex::decode(include_str!("../../tests/fixtures/subaddress_tx.hex"))
            .expect("fixture is valid hex");
        let tx: Transaction = deserialize(&raw_tx).expect("fixture is a valid monero tx");

        let view_key = PrivateKey::from_slice(
            &hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07")
                .unwrap(),
        )
        .unwrap();
        let secret_spend = PrivateKey::from_slice(
            &hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907")
                .unwrap(),
        )
        .unwrap();
        let spend_pubkey = PublicKey::from_private_key(&secret_spend);

        let custody = PlainKeyCustody::default();
        let handle = custody
            .register_wallet(WalletMaterial::new(
                view_key.to_bytes(),
                spend_pubkey.to_bytes(),
            ))
            .await
            .unwrap();

        let matches = custody
            .scan_tx_outputs(handle, &ScanInput::of(&tx), 0..2, 0..3)
            .await
            .unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].output_index, 1);
        assert_eq!(
            matches[0].subaddress_index,
            SubaddressIndex { major: 0, minor: 1 }
        );
        assert!(matches[0].amount_piconero.unwrap() > 0);
    }

    #[tokio::test]
    async fn a_batch_scan_says_which_of_its_transactions_pay_the_wallet() {
        let raw_tx = hex::decode(include_str!("../../tests/fixtures/subaddress_tx.hex")).unwrap();
        let tx: Transaction = deserialize(&raw_tx).unwrap();
        // The same outputs in the other order: each key was made for the
        // other position, so neither belongs to the wallet.
        let mut unrelated = tx.clone();
        unrelated.prefix.outputs.reverse();
        let view_key = PrivateKey::from_slice(
            &hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07")
                .unwrap(),
        )
        .unwrap();
        let secret_spend = PrivateKey::from_slice(
            &hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907")
                .unwrap(),
        )
        .unwrap();
        let custody = PlainKeyCustody::default();
        let handle = custody
            .register_wallet(WalletMaterial::new(
                view_key.to_bytes(),
                PublicKey::from_private_key(&secret_spend).to_bytes(),
            ))
            .await
            .unwrap();
        let window = ScanIndices::new([1, 5]);
        let batch = [
            ScanInput::of(&unrelated),
            ScanInput::of(&tx),
            ScanInput::of(&Transaction::default()),
            ScanInput::of(&unrelated),
            ScanInput::of(&tx),
        ];

        let found = custody
            .scan_txs_for_indices(handle, &batch, &window)
            .await
            .unwrap();

        assert_eq!(found.iter().map(|m| m.tx).collect::<Vec<_>>(), [1, 4]);
        assert_eq!(
            derivations(&custody, handle),
            2,
            "one table for the batch, not one per transaction"
        );
        for matches in &found {
            let alone = custody
                .scan_tx_outputs(handle, &batch[matches.tx], 0..1, 0..6)
                .await
                .unwrap();
            assert_eq!(matches.outputs, alone, "as when scanned on its own");
            assert_eq!(matches.outputs.len(), 1);
            assert_eq!(matches.outputs[0].output_index, 1);
            assert!(matches.outputs[0].amount_piconero.unwrap() > 0);
        }

        // A window the payment isn't in, and an empty batch: nothing.
        let elsewhere = ScanIndices::new([5]);
        assert!(custody
            .scan_txs_for_indices(handle, &batch, &elsewhere)
            .await
            .unwrap()
            .is_empty());
        assert!(custody
            .scan_txs_for_indices(handle, &[], &window)
            .await
            .unwrap()
            .is_empty());
        assert!(matches!(
            custody
                .scan_txs_for_indices(WalletHandle::generate(), &batch, &window)
                .await,
            Err(KeyCustodyError::UnknownWallet)
        ));
    }

    #[tokio::test]
    async fn the_live_table_is_updated_incrementally_and_never_rebuilt_for_the_same_set() {
        let raw_tx = hex::decode(include_str!("../../tests/fixtures/subaddress_tx.hex")).unwrap();
        let tx: Transaction = deserialize(&raw_tx).unwrap();
        let view_key = PrivateKey::from_slice(
            &hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07")
                .unwrap(),
        )
        .unwrap();
        let secret_spend = PrivateKey::from_slice(
            &hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907")
                .unwrap(),
        )
        .unwrap();
        let spend_pubkey = PublicKey::from_private_key(&secret_spend);

        let custody = PlainKeyCustody::default();
        let handle = custody
            .register_wallet(WalletMaterial::new(
                view_key.to_bytes(),
                spend_pubkey.to_bytes(),
            ))
            .await
            .unwrap();

        // The fixture pays minor index 1. Three scans with the same window: the
        // three keys are derived once.
        let window = ScanIndices::new([1, 5, 9]);
        for _ in 0..3 {
            let matches = custody
                .scan_txs_for_indices(handle, &[ScanInput::of(&tx)], &window)
                .await
                .unwrap();
            assert_eq!(matches.len(), 1);
        }
        assert_eq!(derivations(&custody, handle), 3, "same set, no rebuild");

        // One order opens and one closes: one new derivation, not a rebuild.
        let window = ScanIndices::new([1, 9, 12]);
        custody
            .scan_txs_for_indices(handle, &[ScanInput::of(&tx)], &window)
            .await
            .unwrap();
        assert_eq!(derivations(&custody, handle), 4);

        // An index no longer in the window no longer matches.
        let without = ScanIndices::new([9, 12]);
        assert!(custody
            .scan_txs_for_indices(handle, &[ScanInput::of(&tx)], &without)
            .await
            .unwrap()
            .is_empty());

        // A one-off range scan (payment lookup) builds its own table and leaves
        // the live one alone: scanning the live window again derives nothing.
        let before = derivations(&custody, handle);
        assert_eq!(
            custody
                .scan_tx_outputs(handle, &ScanInput::of(&tx), 0..1, 0..3)
                .await
                .unwrap()
                .len(),
            1
        );
        let after_lookup = derivations(&custody, handle);
        assert_eq!(after_lookup - before, 3, "the lookup's own table");
        custody
            .scan_txs_for_indices(handle, &[ScanInput::of(&tx)], &without)
            .await
            .unwrap();
        assert_eq!(
            derivations(&custody, handle),
            after_lookup,
            "the live table survived the lookup"
        );
    }

    #[test]
    fn a_large_lookup_range_builds_in_reusable_bounded_batches() {
        let material = WalletMaterial::new(
            random_scalar_bytes(71),
            PublicKey::from_private_key(&PrivateKey::from_slice(&random_scalar_bytes(72)).unwrap())
                .to_bytes(),
        );
        let pair = material.to_view_pair().unwrap();
        let mut table = RangeTable::default();
        let major = 0..1;
        let minor = 0..(SCAN_TABLE_BUILD_BATCH as u32 + 3);
        assert_eq!(
            table.update_batch(&pair, &major, &minor),
            SCAN_TABLE_BUILD_BATCH
        );
        assert!(!table.complete);
        assert_eq!(table.update_batch(&pair, &major, &minor), 3);
        assert!(table.complete);
        assert_eq!(table.update_batch(&pair, &major, &minor), 0);
        assert_eq!(table.table.len(), SCAN_TABLE_BUILD_BATCH + 3);
    }

    #[test]
    fn cancelling_a_scan_preserves_the_next_payment_match() {
        // One blocking worker lets us stop a scan at an await deterministically,
        // without sleeps, deadlines, or depending on how fast the CPU runs.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let tx: Transaction = deserialize(
                &hex::decode(include_str!("../../tests/fixtures/subaddress_tx.hex")).unwrap(),
            )
            .unwrap();
            let view =
                hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07")
                    .unwrap()
                    .try_into()
                    .unwrap();
            let spend = PrivateKey::from_slice(
                &hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907")
                    .unwrap(),
            )
            .unwrap();
            let custody = PlainKeyCustody::default();
            let handle = custody
                .register_wallet(WalletMaterial::new(
                    view,
                    PublicKey::from_private_key(&spend).to_bytes(),
                ))
                .await
                .unwrap();
            let window = ScanIndices::new([1]);
            assert_eq!(
                custody
                    .scan_txs_for_indices(handle, &[ScanInput::of(&tx)], &window)
                    .await
                    .unwrap()
                    .len(),
                1
            );

            // Exercise cancellation with an unchanged cache and during a window
            // update. In either case the very next scan must find the payment.
            for window in [window, ScanIndices::new([1, 5])] {
                let (release, wait) = std::sync::mpsc::channel::<()>();
                let (started, ready) = tokio::sync::oneshot::channel();
                let blocker = tokio::task::spawn_blocking(move || {
                    started.send(()).unwrap();
                    let _ = wait.recv();
                });
                ready.await.unwrap();
                let txs = [ScanInput::of(&tx)];
                let mut scan = custody.scan_txs_for_indices(handle, &txs, &window);
                std::future::poll_fn(|cx| {
                    assert!(scan.as_mut().poll(cx).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
                drop(scan);
                drop(release);
                blocker.await.unwrap();
                assert_eq!(
                    custody
                        .scan_txs_for_indices(handle, &[ScanInput::of(&tx)], &window)
                        .await
                        .unwrap()
                        .len(),
                    1,
                    "the first scan after cancellation must still find the payment",
                );
            }
        });
    }

    #[tokio::test]
    async fn scans_of_different_wallets_run_in_parallel_off_the_async_workers() {
        let raw_tx = hex::decode(include_str!("../../tests/fixtures/subaddress_tx.hex")).unwrap();
        let tx: Transaction = deserialize(&raw_tx).unwrap();
        let custody = Arc::new(PlainKeyCustody::default());
        let mut handles = vec![];
        for seed in 10..18u8 {
            let view = PrivateKey::from_slice(&random_scalar_bytes(seed)).unwrap();
            let spend = PublicKey::from_private_key(
                &PrivateKey::from_slice(&random_scalar_bytes(seed + 40)).unwrap(),
            );
            handles.push(
                custody
                    .register_wallet(WalletMaterial::new(view.to_bytes(), spend.to_bytes()))
                    .await
                    .unwrap(),
            );
        }
        // All at once on a current-thread runtime: if scans ran on the async
        // worker they would still complete, but this checks nothing deadlocks
        // when many wallets scan concurrently through the shared slots.
        let window = ScanIndices::range(0..200);
        let scans = handles.iter().map(|h| {
            let custody = Arc::clone(&custody);
            let tx = tx.clone();
            let window = window.clone();
            let h = *h;
            async move {
                custody
                    .scan_txs_for_indices(h, &[ScanInput::of(&tx)], &window)
                    .await
            }
        });
        for result in futures_util::future::join_all(scans).await {
            assert!(
                result.unwrap().is_empty(),
                "none of these wallets is paid by the fixture"
            );
        }
    }

    #[tokio::test]
    async fn seal_then_unseal_survives_a_simulated_restart() {
        let view_key = PrivateKey::from_slice(&random_scalar_bytes(3)).unwrap();
        let spend_key = PrivateKey::from_slice(&random_scalar_bytes(4)).unwrap();
        let spend_pubkey = PublicKey::from_private_key(&spend_key);
        let material = WalletMaterial::new(view_key.to_bytes(), spend_pubkey.to_bytes());

        // "Before restart": register, derive an address, seal what we'd persist.
        let custody_before = PlainKeyCustody::default();
        let handle_before = custody_before
            .register_wallet(material.clone())
            .await
            .unwrap();
        let address_before = custody_before
            .derive_subaddress(
                handle_before,
                SubaddressIndex { major: 0, minor: 7 },
                Network::Mainnet,
            )
            .await
            .unwrap();
        let sealed = custody_before.seal(&material).await.unwrap();

        // "After restart": a brand new backend instance, nothing in memory except
        // what came out of storage. The old handle is meaningless here - only the
        // sealed bytes and a fresh handle from unsealing them matter.
        let custody_after = PlainKeyCustody::default();
        let handle_after = custody_after.unseal_and_register(&sealed).await.unwrap();
        assert_ne!(handle_before, handle_after);

        let address_after = custody_after
            .derive_subaddress(
                handle_after,
                SubaddressIndex { major: 0, minor: 7 },
                Network::Mainnet,
            )
            .await
            .unwrap();
        assert_eq!(address_before, address_after);
    }

    #[tokio::test]
    async fn index_zero_derives_a_standard_address_not_an_unpayable_subaddress() {
        // Regression test for the worst shape of bug this boundary can produce: an
        // address the service hands out that no payment to it could ever be
        // detected at. `get_subaddress` at 0/0 returns the correct root keys under a
        // *subaddress* network byte, which makes a sender derive the shared secret
        // as `8*r*C` against `C = v*G` while this wallet looks for `8*v*r*D` against
        // `D = S` - they never agree, so the funds land somewhere this wallet cannot
        // see. `admin::create_tenant` reports exactly this address to a merchant as
        // their `primary_address`.
        let view_key = PrivateKey::from_slice(&random_scalar_bytes(5)).unwrap();
        let spend_key = PrivateKey::from_slice(&random_scalar_bytes(6)).unwrap();
        let spend_pubkey = PublicKey::from_private_key(&spend_key);

        let custody = PlainKeyCustody::default();
        let handle = custody
            .register_wallet(WalletMaterial::new(
                view_key.to_bytes(),
                spend_pubkey.to_bytes(),
            ))
            .await
            .unwrap();

        let primary = custody
            .derive_subaddress(handle, SubaddressIndex::default(), Network::Mainnet)
            .await
            .unwrap();
        assert_eq!(
            primary.addr_type,
            monero::AddressType::Standard,
            "0/0 must encode as a standard address; a subaddress-tagged root key pair is unpayable"
        );
        assert_eq!(
            primary,
            Address::standard(
                Network::Mainnet,
                spend_pubkey,
                PublicKey::from_private_key(&view_key),
            )
        );
        // Mainnet standard addresses start with '4'; subaddresses start with '8'.
        // Asserting the rendered form too, since that string is what a merchant
        // actually copies out of the admin API.
        assert!(primary.to_string().starts_with('4'), "got {primary}");

        // Every non-zero index must still take the real subaddress path.
        let first_order_address = custody
            .derive_subaddress(
                handle,
                SubaddressIndex { major: 0, minor: 1 },
                Network::Mainnet,
            )
            .await
            .unwrap();
        assert_eq!(
            first_order_address.addr_type,
            monero::AddressType::SubAddress
        );
        assert_ne!(first_order_address.public_spend, spend_pubkey);
    }

    #[tokio::test]
    async fn an_index_at_the_top_of_the_u32_range_derives_without_overflowing() {
        // Subaddress derivation hashes the index rather than doing arithmetic on it,
        // so `u32::MAX` is not a special case - but nothing else proves that, and a
        // panic here would take down whichever request or scan tick hit it.
        let view_key = PrivateKey::from_slice(&random_scalar_bytes(7)).unwrap();
        let spend_key = PrivateKey::from_slice(&random_scalar_bytes(8)).unwrap();
        let custody = PlainKeyCustody::default();
        let handle = custody
            .register_wallet(WalletMaterial::new(
                view_key.to_bytes(),
                PublicKey::from_private_key(&spend_key).to_bytes(),
            ))
            .await
            .unwrap();

        let extreme = custody
            .derive_subaddress(
                handle,
                SubaddressIndex {
                    major: u32::MAX,
                    minor: u32::MAX,
                },
                Network::Mainnet,
            )
            .await
            .unwrap();
        assert_eq!(extreme.addr_type, monero::AddressType::SubAddress);
        // Distinct from its neighbours - i.e. the index really is participating in
        // the derivation rather than saturating to something shared.
        let neighbour = custody
            .derive_subaddress(
                handle,
                SubaddressIndex {
                    major: u32::MAX,
                    minor: u32::MAX - 1,
                },
                Network::Mainnet,
            )
            .await
            .unwrap();
        assert_ne!(extreme, neighbour);
    }

    #[tokio::test]
    async fn an_absurdly_wide_scan_range_is_refused_rather_than_hanging_forever() {
        // Table construction is one scalar multiplication per candidate index, so a
        // range this size isn't slow, it never finishes - and it would take the
        // whole scanner loop (every tenant, every network) with it, silently.
        let raw_tx = hex::decode(include_str!("../../tests/fixtures/subaddress_tx.hex")).unwrap();
        let tx: Transaction = deserialize(&raw_tx).unwrap();
        let view_key = PrivateKey::from_slice(&random_scalar_bytes(9)).unwrap();
        let spend_key = PrivateKey::from_slice(&random_scalar_bytes(10)).unwrap();
        let custody = PlainKeyCustody::default();
        let handle = custody
            .register_wallet(WalletMaterial::new(
                view_key.to_bytes(),
                PublicKey::from_private_key(&spend_key).to_bytes(),
            ))
            .await
            .unwrap();

        let err = custody
            .scan_tx_outputs(handle, &ScanInput::of(&tx), 0..1, 0..u32::MAX)
            .await
            .unwrap_err();
        assert!(matches!(err, KeyCustodyError::ScanFailed(_)), "got {err:?}");

        // A realistic range is of course still accepted.
        custody
            .scan_tx_outputs(handle, &ScanInput::of(&tx), 0..1, 0..64)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn removing_a_wallet_scrubs_its_view_key_rather_than_leaving_it_in_freed_memory() {
        // The registry holds the only long-lived copy of a tenant's view key.
        // Offboarding must not leave it readable for the remaining uptime of the
        // process; `WalletEntry`'s `Drop` is what guarantees that, and this pins the
        // observable half of it (that removal actually drops the entry, and that a
        // scrubbed entry can never answer a later scan).
        let view_key = PrivateKey::from_slice(&random_scalar_bytes(11)).unwrap();
        let spend_key = PrivateKey::from_slice(&random_scalar_bytes(12)).unwrap();
        let custody = PlainKeyCustody::default();
        let handle = custody
            .register_wallet(WalletMaterial::new(
                view_key.to_bytes(),
                PublicKey::from_private_key(&spend_key).to_bytes(),
            ))
            .await
            .unwrap();

        custody.remove_wallet(handle).await.unwrap();
        assert!(custody.wallets.read().is_empty());
        assert!(matches!(
            custody.remove_wallet(handle).await.unwrap_err(),
            KeyCustodyError::UnknownWallet
        ));

        let raw_tx = hex::decode(include_str!("../../tests/fixtures/subaddress_tx.hex")).unwrap();
        let tx: Transaction = deserialize(&raw_tx).unwrap();
        assert!(matches!(
            custody
                .scan_tx_outputs(handle, &ScanInput::of(&tx), 0..1, 0..2)
                .await
                .unwrap_err(),
            KeyCustodyError::UnknownWallet
        ));
    }

    #[tokio::test]
    async fn registering_the_same_material_twice_yields_independent_handles_that_both_work() {
        // Reachable in production: `main::register_all_tenants` registers every
        // tenant at boot, and `http::resolve_wallet_handle` can register the same
        // tenant again if two requests race the cache. Both handles must derive
        // identical addresses, and removing one must not disturb the other - a
        // shared or recycled handle space would break exactly that.
        let view_key = PrivateKey::from_slice(&random_scalar_bytes(13)).unwrap();
        let spend_key = PrivateKey::from_slice(&random_scalar_bytes(14)).unwrap();
        let material = WalletMaterial::new(
            view_key.to_bytes(),
            PublicKey::from_private_key(&spend_key).to_bytes(),
        );

        let custody = PlainKeyCustody::default();
        let sealed = custody.seal(&material).await.unwrap();
        let first = custody.unseal_and_register(&sealed).await.unwrap();
        let second = custody.unseal_and_register(&sealed).await.unwrap();
        assert_ne!(first, second, "each registration must get its own handle");

        let index = SubaddressIndex { major: 0, minor: 3 };
        let from_first = custody
            .derive_subaddress(first, index, Network::Mainnet)
            .await
            .unwrap();
        let from_second = custody
            .derive_subaddress(second, index, Network::Mainnet)
            .await
            .unwrap();
        assert_eq!(from_first, from_second);

        custody.remove_wallet(first).await.unwrap();
        assert_eq!(
            custody
                .derive_subaddress(second, index, Network::Mainnet)
                .await
                .unwrap(),
            from_second,
            "removing one registration must not invalidate an independent one"
        );
    }

    #[tokio::test]
    async fn seal_rejects_truncated_or_overlong_material_instead_of_silently_padding() {
        // `from_raw_bytes` is the only path back from at-rest bytes to a live key.
        // A short read that silently zero-padded (or a long one that silently
        // truncated) would register a wallet with the *wrong* view key - it would
        // scan cleanly and simply never match a real payment.
        for bad_len in [0usize, 31, 32, 63, 65, 128] {
            let custody = PlainKeyCustody::default();
            let err = custody
                .unseal_and_register(&vec![7u8; bad_len])
                .await
                .unwrap_err();
            assert!(
                matches!(err, KeyCustodyError::InvalidKeyMaterial(_)),
                "{bad_len} bytes should be rejected, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn seal_round_trips_every_byte_of_both_keys_including_high_bytes() {
        // Guards the `[..32]`/`[32..]` split in `to_raw_bytes`/`from_raw_bytes`
        // against an off-by-one that would swap or clip key bytes - the kind of
        // corruption that survives a restart and then silently sees no payments.
        let mut view_bytes = [0u8; 32];
        for (i, b) in view_bytes.iter_mut().enumerate() {
            *b = i as u8;
        }
        view_bytes[31] &= 0x0f; // keep it a canonical scalar
        let spend_bytes =
            PublicKey::from_private_key(&PrivateKey::from_slice(&random_scalar_bytes(15)).unwrap())
                .to_bytes();

        let material = WalletMaterial::new(view_bytes, spend_bytes);
        let sealed = PlainKeyCustody::default().seal(&material).await.unwrap();
        assert_eq!(sealed.len(), 64);
        assert_eq!(&sealed[..32], &view_bytes[..]);
        assert_eq!(&sealed[32..], &spend_bytes[..]);

        let restored = WalletMaterial::from_raw_bytes(&sealed).unwrap();
        let original_pair = material.to_view_pair().unwrap();
        let restored_pair = restored.to_view_pair().unwrap();
        assert_eq!(original_pair.view.to_bytes(), restored_pair.view.to_bytes());
        assert_eq!(original_pair.spend, restored_pair.spend);
    }

    #[tokio::test]
    async fn concurrent_registrations_and_removals_never_cross_wires_between_wallets() {
        // Two tenants' wallets registered and scanned from many tasks at once: the
        // guarantee that matters is that a handle only ever resolves to *its own*
        // key material, never a neighbour's, no matter the interleaving. A
        // misattributed payment is the single worst outcome this module can produce.
        use std::sync::Arc;

        let custody = Arc::new(PlainKeyCustody::default());
        let mut expected = Vec::new();
        for seed in 20u8..24 {
            let view_key = PrivateKey::from_slice(&random_scalar_bytes(seed)).unwrap();
            let spend_key = PrivateKey::from_slice(&random_scalar_bytes(seed + 40)).unwrap();
            let spend_pubkey = PublicKey::from_private_key(&spend_key);
            let handle = custody
                .register_wallet(WalletMaterial::new(
                    view_key.to_bytes(),
                    spend_pubkey.to_bytes(),
                ))
                .await
                .unwrap();
            let index = SubaddressIndex { major: 0, minor: 1 };
            let address = custody
                .derive_subaddress(handle, index, Network::Mainnet)
                .await
                .unwrap();
            expected.push((handle, address));
        }

        let mut tasks = Vec::new();
        for (handle, address) in expected.clone() {
            for _ in 0..8 {
                let custody = Arc::clone(&custody);
                tasks.push(tokio::spawn(async move {
                    let index = SubaddressIndex { major: 0, minor: 1 };
                    assert_eq!(
                        custody
                            .derive_subaddress(handle, index, Network::Mainnet)
                            .await
                            .unwrap(),
                        address,
                        "a handle resolved to another wallet's key material"
                    );
                }));
            }
        }
        // Churn the map concurrently with those reads, so the reads are genuinely
        // racing writer acquisitions of the registry lock rather than a quiet map.
        for seed in 60u8..68 {
            let custody = Arc::clone(&custody);
            tasks.push(tokio::spawn(async move {
                let view_key = PrivateKey::from_slice(&random_scalar_bytes(seed)).unwrap();
                let spend_key = PrivateKey::from_slice(&random_scalar_bytes(seed + 20)).unwrap();
                let handle = custody
                    .register_wallet(WalletMaterial::new(
                        view_key.to_bytes(),
                        PublicKey::from_private_key(&spend_key).to_bytes(),
                    ))
                    .await
                    .unwrap();
                custody.remove_wallet(handle).await.unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        // Every original wallet is still intact and still its own.
        for (handle, address) in expected {
            let index = SubaddressIndex { major: 0, minor: 1 };
            assert_eq!(
                custody
                    .derive_subaddress(handle, index, Network::Mainnet)
                    .await
                    .unwrap(),
                address
            );
        }
    }

    fn derivations(custody: &PlainKeyCustody, handle: WalletHandle) -> u64 {
        custody
            .wallets
            .read()
            .get(&handle)
            .unwrap()
            .derivations
            .load(Ordering::Relaxed)
    }

    /// The engine sizes the scan slots once, from its CPUs; a later size
    /// changes nothing. (The slots are shared by every test in this binary,
    /// some scanning right now, so only their size is checked here.)
    #[test]
    fn the_scan_slots_keep_the_first_size_they_are_given() {
        let size = size_scan_slots(3);
        assert!(size >= 1);
        assert_eq!(size_scan_slots(size + 5), size, "the first size stays");
    }
}
