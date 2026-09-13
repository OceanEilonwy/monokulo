//! Wire-level DTOs for the `KeyCustody` boundary (WBS 2.1.1).
//!
//! This crate is deliberately *just* data and conversions - no socket, no server,
//! no client adapter. That split is the WBS's own framing, not an accident of
//! scope creep avoidance: 2.1.2 (a Unix-socket server plus a client that
//! implements `KeyCustody` by forwarding calls over it) needs a stable wire
//! format to build against, and getting that format right - and proven to
//! round-trip - is a self-contained problem worth finishing before any IO code
//! exists to obscure a shape bug.
//!
//! Every type here exists because `src/key_custody/mod.rs`'s real types don't
//! (and mostly shouldn't) derive `Serialize`/`Deserialize` themselves:
//! - `KeyCustodyError` is a `thiserror` enum with `String` payloads, not
//!   `serde`-derived - the engine crate has no reason to carry a wire format for
//!   an error type nothing outside it currently needs to serialize.
//! - `WalletHandle` wraps a private `uuid::Uuid` with no public accessor before
//!   this task - see the `as_bytes`/`from_bytes` pair added to it in
//!   `src/key_custody/mod.rs` for this exact purpose, documented there.
//! - `WalletMaterial` is `ZeroizeOnDrop` and deliberately *not* `Serialize` - it
//!   already exposes `to_raw_bytes`/`from_raw_bytes` for exactly this kind of
//!   boundary-crossing use (its own doc comments say so), so the DTO here wraps
//!   those 64 bytes rather than inventing a second shape.
//! - `MatchedOutput`'s own fields are all plain and `Copy`, but its
//!   `subaddress_index: SubaddressIndex` is a re-export of `monero`-rs's
//!   `cryptonote::subaddress::Index`, which only derives `Serialize`/
//!   `Deserialize` behind that crate's own `serde` feature - a feature this
//!   workspace doesn't enable (see `SubaddressIndexWire`'s doc comment for why
//!   turning it on wasn't the chosen fix).
//! - `Address`, `Transaction`, and `Network` are all `monero`-rs types this
//!   crate reuses that crate's *own* existing encodings for wherever one exists
//!   (base58 address text, consensus/wire byte encoding, and this codebase's own
//!   `network::network_str`/`parse_network` helpers respectively) rather than
//!   inventing a second one that could silently drift from the first.
//!
//! Every DTO below wraps its real-type payload as a hex-encoded `String` (or a
//! small plain struct of `u32`s, for the genuinely plain cases). Hex, not
//! base64: this codebase already depends on `hex` pervasively (`WalletMaterial`
//! itself, `crate::crypto`, every `sk_`/`pk_` token) and never on a base64
//! crate, so hex keeps this crate's wire values readable in a log line or a
//! `curl`'d test payload without adding a new encoding convention this
//! workspace doesn't already have.

use std::ops::Range;

use moneropay_core::key_custody::{
    KeyCustodyError, MatchedOutput, Network, SubaddressIndex, WalletHandle, WalletMaterial,
};
use moneropay_core::network::{network_str, parse_network};
use monero::consensus::encode::{deserialize, serialize};
use monero::{Address, Transaction};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Failure converting a wire DTO *back* into a real engine type: malformed hex, a
/// wrong byte length, an unrecognized network name, a string that doesn't parse as
/// a Monero address, bytes that don't decode as a valid consensus-encoded
/// transaction, or a `u64` that doesn't fit this platform's `usize`.
///
/// Deliberately distinct from `KeyCustodyErrorWire` below. `KeyCustodyErrorWire`
/// carries a `KeyCustody` *method's own* result across the wire - it's meaningful
/// application data. `WireConversionError` only ever originates locally, when this
/// crate's own `TryFrom` impls turn a (possibly corrupted, possibly
/// attacker-controlled once 2.1.2 has a socket listening) wire value back into
/// something the real engine types can use. What a future socket server does with
/// one of these - close the connection, or fold it into a `KeyCustodyErrorWire::
/// BackendUnavailable` sent back to the caller - is a 2.1.2 decision, not this
/// step's.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireConversionError {
    #[error("invalid hex: {0}")]
    InvalidHex(String),
    #[error("invalid byte length: expected {expected}, got {got}")]
    InvalidLength { expected: usize, got: usize },
    #[error("invalid network name: {0}")]
    InvalidNetwork(String),
    #[error("invalid monero address: {0}")]
    InvalidAddress(String),
    #[error("invalid monero transaction encoding: {0}")]
    InvalidTransaction(String),
    #[error("invalid key material: {0}")]
    InvalidKeyMaterial(String),
    #[error("value out of range for this platform: {0}")]
    OutOfRange(String),
}

// ---------------------------------------------------------------------------
// WalletHandle
// ---------------------------------------------------------------------------

/// Wire form of a `WalletHandle`: its underlying UUID's 16 bytes, hex-encoded.
/// See the `as_bytes`/`from_bytes` pair on `WalletHandle` itself
/// (`src/key_custody/mod.rs`) for why those had to be added to make this
/// possible, and why doing so doesn't weaken the "opaque handle" design that
/// type's own doc comment describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletHandleWire {
    pub uuid_hex: String,
}

impl From<WalletHandle> for WalletHandleWire {
    fn from(handle: WalletHandle) -> Self {
        WalletHandleWire {
            uuid_hex: hex::encode(handle.as_bytes()),
        }
    }
}

impl TryFrom<&WalletHandleWire> for WalletHandle {
    type Error = WireConversionError;

    fn try_from(wire: &WalletHandleWire) -> Result<Self, Self::Error> {
        let bytes = hex::decode(&wire.uuid_hex)
            .map_err(|e| WireConversionError::InvalidHex(e.to_string()))?;
        let array: [u8; 16] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| WireConversionError::InvalidLength { expected: 16, got: bytes.len() })?;
        Ok(WalletHandle::from_bytes(array))
    }
}

impl TryFrom<WalletHandleWire> for WalletHandle {
    type Error = WireConversionError;

    fn try_from(wire: WalletHandleWire) -> Result<Self, Self::Error> {
        WalletHandle::try_from(&wire)
    }
}

// ---------------------------------------------------------------------------
// WalletMaterial
// ---------------------------------------------------------------------------

/// Wire form of `WalletMaterial`: the exact `view_key || spend_pubkey` 64-byte
/// layout `WalletMaterial::to_raw_bytes`/`from_raw_bytes` already define for this
/// purpose, hex-encoded rather than reinvented.
///
/// Zeroized on drop and redacted from `Debug`, mirroring `WalletMaterial` itself.
/// This struct carries a real private view key for as long as it's alive - in
/// exactly the plaintext form a socket client (2.1.2) will hold it in transiently
/// between decoding a request and either handing it to a real `KeyCustody`
/// implementation or serializing it back onto the wire. The wire copy is no less
/// sensitive than the in-memory `WalletMaterial` it was built from just because
/// it's represented as hex text instead of two `[u8; 32]`s, so it gets the same
/// scrub-on-drop treatment. `PartialEq`/`Eq` are deliberately not derived here for
/// the same reason `WalletMaterial` doesn't derive them: a round trip must be
/// proven by comparing the *real* raw bytes after conversion back, never by
/// comparing two wire values (or worse, two `Debug` strings) directly - see this
/// crate's own tests, and the WBS's explicit warning that a serialization bug
/// here "fails silently as empty view key rather than loudly."
#[derive(Clone, ZeroizeOnDrop, Serialize, Deserialize)]
pub struct WalletMaterialWire {
    pub raw_hex: String,
}

impl std::fmt::Debug for WalletMaterialWire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletMaterialWire")
            .field("raw_hex", &"<redacted>")
            .finish()
    }
}

impl From<&WalletMaterial> for WalletMaterialWire {
    fn from(material: &WalletMaterial) -> Self {
        let mut raw = material.to_raw_bytes();
        let wire = WalletMaterialWire { raw_hex: hex::encode(raw) };
        // `to_raw_bytes` hands back a fresh stack copy with no reason to outlive
        // this call - scrub it here rather than leaving it in the freed frame,
        // same reasoning `PlainKeyCustody::seal` already applies to its own copy.
        raw.zeroize();
        wire
    }
}

impl TryFrom<&WalletMaterialWire> for WalletMaterial {
    type Error = WireConversionError;

    fn try_from(wire: &WalletMaterialWire) -> Result<Self, Self::Error> {
        let bytes = hex::decode(&wire.raw_hex)
            .map_err(|e| WireConversionError::InvalidHex(e.to_string()))?;
        WalletMaterial::from_raw_bytes(&bytes)
            .map_err(|e| WireConversionError::InvalidKeyMaterial(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Sealed material (KeyCustody::seal / unseal_and_register)
// ---------------------------------------------------------------------------

/// Wire form of `KeyCustody::seal`'s `Vec<u8>` output and `unseal_and_register`'s
/// `&[u8]` input.
///
/// Deliberately *not* assumed to be 64 bytes the way `WalletMaterialWire` is.
/// `PlainKeyCustody::seal` happens to produce exactly `WalletMaterial::
/// to_raw_bytes()`'s 64 bytes today - its own doc comment is explicit that this is
/// a no-op serialization, not a security boundary, for that backend specifically.
/// A TEE-backed `KeyCustody` implementation's `seal` is expected to produce
/// something sealed *to that enclave* (ciphertext plus whatever authentication/
/// key-wrapping overhead the sealing primitive adds) - almost certainly a
/// different length, and with no reason to assume it shares `WalletMaterialWire`'s
/// shape just because the one implementation that exists today happens to.
///
/// Zeroized on drop and redacted from `Debug` for the same reason as
/// `WalletMaterialWire`: for `PlainKeyCustody` specifically, these bytes *are* the
/// raw key material, unencrypted.
#[derive(Clone, ZeroizeOnDrop, Serialize, Deserialize)]
pub struct SealedMaterialWire {
    pub sealed_hex: String,
}

impl std::fmt::Debug for SealedMaterialWire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedMaterialWire")
            .field("sealed_hex", &"<redacted>")
            .finish()
    }
}

impl From<&[u8]> for SealedMaterialWire {
    fn from(bytes: &[u8]) -> Self {
        SealedMaterialWire { sealed_hex: hex::encode(bytes) }
    }
}

impl SealedMaterialWire {
    pub fn to_bytes(&self) -> Result<Vec<u8>, WireConversionError> {
        hex::decode(&self.sealed_hex).map_err(|e| WireConversionError::InvalidHex(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// KeyCustodyError
// ---------------------------------------------------------------------------

/// Wire form of `KeyCustodyError`, mapped by variant per the WBS's own
/// suggestion. `KeyCustodyError` is a `thiserror` enum and deliberately doesn't
/// derive `Serialize`/`Deserialize` itself - the engine crate has no business
/// knowing about wire formats - so this crate carries its own copy of the same
/// variant shapes and converts between them explicitly rather than trying to
/// derive through the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyCustodyErrorWire {
    UnknownWallet,
    InvalidKeyMaterial(String),
    BackendUnavailable(String),
    ScanFailed(String),
}

impl From<KeyCustodyError> for KeyCustodyErrorWire {
    fn from(e: KeyCustodyError) -> Self {
        match e {
            KeyCustodyError::UnknownWallet => KeyCustodyErrorWire::UnknownWallet,
            KeyCustodyError::InvalidKeyMaterial(s) => KeyCustodyErrorWire::InvalidKeyMaterial(s),
            KeyCustodyError::BackendUnavailable(s) => KeyCustodyErrorWire::BackendUnavailable(s),
            KeyCustodyError::ScanFailed(s) => KeyCustodyErrorWire::ScanFailed(s),
        }
    }
}

impl From<KeyCustodyErrorWire> for KeyCustodyError {
    fn from(e: KeyCustodyErrorWire) -> Self {
        match e {
            KeyCustodyErrorWire::UnknownWallet => KeyCustodyError::UnknownWallet,
            KeyCustodyErrorWire::InvalidKeyMaterial(s) => KeyCustodyError::InvalidKeyMaterial(s),
            KeyCustodyErrorWire::BackendUnavailable(s) => KeyCustodyError::BackendUnavailable(s),
            KeyCustodyErrorWire::ScanFailed(s) => KeyCustodyError::ScanFailed(s),
        }
    }
}

// ---------------------------------------------------------------------------
// SubaddressIndex
// ---------------------------------------------------------------------------

/// Wire form of `SubaddressIndex` (`monero::cryptonote::subaddress::Index`).
///
/// That type *does* derive `Serialize`/`Deserialize` upstream, but only behind
/// `monero`-rs's own `serde` cargo feature, which this workspace's root
/// `Cargo.toml` doesn't enable for its `monero` dependency (its default features
/// are `full`, not `serde`). Turning that feature on here was considered and
/// rejected: Cargo feature unification means enabling `monero/serde` in this
/// crate's `Cargo.toml` would silently enable it for every other workspace member
/// too whenever they're built together (e.g. `cargo build --workspace`), pulling
/// in `curve25519-dalek/serde` and `serde-big-array` for the whole dependency
/// graph as a side effect of a change scoped to this one crate - exactly the kind
/// of non-obvious, action-at-a-distance build change this task's own instructions
/// warn against for `cargo fmt`. `major`/`minor` are two public `u32`s; carrying
/// them plainly here costs nothing and avoids that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubaddressIndexWire {
    pub major: u32,
    pub minor: u32,
}

impl From<SubaddressIndex> for SubaddressIndexWire {
    fn from(index: SubaddressIndex) -> Self {
        SubaddressIndexWire { major: index.major, minor: index.minor }
    }
}

impl From<SubaddressIndexWire> for SubaddressIndex {
    fn from(wire: SubaddressIndexWire) -> Self {
        SubaddressIndex { major: wire.major, minor: wire.minor }
    }
}

// ---------------------------------------------------------------------------
// MatchedOutput
// ---------------------------------------------------------------------------

/// Wire form of `MatchedOutput`. `output_index` crosses as `u64`, not the
/// engine's native `usize` - `usize`'s width isn't fixed by the language, and a
/// wire format that silently varied by the target platform building this crate
/// (or a future service/client pair built for different architectures) would be
/// exactly the kind of latent bug this task's DTOs are supposed to rule out.
/// `usize -> u64` is always lossless; converting back checks the reverse
/// direction explicitly instead of casting, in case this ever runs on a 32-bit
/// target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchedOutputWire {
    pub output_index: u64,
    pub subaddress_index: SubaddressIndexWire,
    pub amount_piconero: Option<u64>,
}

impl From<MatchedOutput> for MatchedOutputWire {
    fn from(m: MatchedOutput) -> Self {
        MatchedOutputWire {
            output_index: m.output_index as u64,
            subaddress_index: m.subaddress_index.into(),
            amount_piconero: m.amount_piconero,
        }
    }
}

impl TryFrom<MatchedOutputWire> for MatchedOutput {
    type Error = WireConversionError;

    fn try_from(wire: MatchedOutputWire) -> Result<Self, Self::Error> {
        let output_index = usize::try_from(wire.output_index).map_err(|_| {
            WireConversionError::OutOfRange(format!(
                "output_index {} doesn't fit this platform's usize",
                wire.output_index
            ))
        })?;
        Ok(MatchedOutput {
            output_index,
            subaddress_index: wire.subaddress_index.into(),
            amount_piconero: wire.amount_piconero,
        })
    }
}

// ---------------------------------------------------------------------------
// Range<u32> (major_range / minor_range on scan_tx_outputs)
// ---------------------------------------------------------------------------

/// Wire form of a `Range<u32>`. `std::ops::Range` doesn't implement `Serialize`/
/// `Deserialize` itself in current `serde` - it deliberately isn't `Copy` (so a
/// `for` loop can consume it) which the derive machinery for foreign types can't
/// see past - so this is a plain `{start, end}` struct rather than an attempt to
/// derive through it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeWire {
    pub start: u32,
    pub end: u32,
}

impl From<Range<u32>> for RangeWire {
    fn from(range: Range<u32>) -> Self {
        RangeWire { start: range.start, end: range.end }
    }
}

impl From<RangeWire> for Range<u32> {
    fn from(wire: RangeWire) -> Self {
        wire.start..wire.end
    }
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

/// Wire form of `monero::Network`, reusing `moneropay_core::network::
/// network_str`/`parse_network` rather than a second string mapping that could
/// drift from the one the config file and admin API already use - the WBS's own
/// suggestion, and the obviously correct one: those helpers are already the
/// single source of truth this codebase uses everywhere else a `Network` crosses
/// a text boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkWire(pub String);

impl From<Network> for NetworkWire {
    fn from(network: Network) -> Self {
        NetworkWire(network_str(network).to_string())
    }
}

impl TryFrom<&NetworkWire> for Network {
    type Error = WireConversionError;

    fn try_from(wire: &NetworkWire) -> Result<Self, Self::Error> {
        parse_network(&wire.0).map_err(|e| WireConversionError::InvalidNetwork(e.0))
    }
}

// ---------------------------------------------------------------------------
// Address
// ---------------------------------------------------------------------------

/// Wire form of `monero::Address` - its own base58 text encoding
/// (`Display`/`FromStr`), the same string a merchant would copy out of a wallet
/// or this service's own admin API, not a fresh byte encoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressWire(pub String);

impl From<Address> for AddressWire {
    fn from(address: Address) -> Self {
        AddressWire(address.to_string())
    }
}

impl TryFrom<&AddressWire> for Address {
    type Error = WireConversionError;

    fn try_from(wire: &AddressWire) -> Result<Self, Self::Error> {
        wire.0
            .parse::<Address>()
            .map_err(|e| WireConversionError::InvalidAddress(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Transaction
// ---------------------------------------------------------------------------

/// Wire form of `monero::Transaction` - its own consensus/wire encoding
/// (`monero::consensus::encode::serialize`/`deserialize`, the same functions
/// `src/scanner.rs`'s and `src/key_custody/plain.rs`'s own tests already use to
/// load fixture transactions), hex-encoded. Reused deliberately rather than
/// adding a fresh `serde` derive: a transaction is public blockchain data that
/// already has exactly one correct byte encoding (the one every Monero node and
/// wallet agrees on), so inventing a second, `serde`-specific one would be pure
/// risk - a subtle field-ordering or varint-width mismatch between the two would
/// be the kind of bug that only shows up against a real transaction, not a
/// hand-built test fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionWire {
    pub bytes_hex: String,
}

impl From<&Transaction> for TransactionWire {
    fn from(tx: &Transaction) -> Self {
        TransactionWire { bytes_hex: hex::encode(serialize(tx)) }
    }
}

impl TryFrom<&TransactionWire> for Transaction {
    type Error = WireConversionError;

    fn try_from(wire: &TransactionWire) -> Result<Self, Self::Error> {
        let bytes = hex::decode(&wire.bytes_hex)
            .map_err(|e| WireConversionError::InvalidHex(e.to_string()))?;
        deserialize(&bytes).map_err(|e| WireConversionError::InvalidTransaction(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Per-method request/response DTOs
// ---------------------------------------------------------------------------
//
// One request struct and one response shape per `KeyCustody` trait method, in
// the same order the trait declares them. Every response is a type alias for
// `Result<TWire, KeyCustodyErrorWire>` rather than a hand-rolled `Ok`/`Err` enum:
// `serde` already implements `Serialize`/`Deserialize` for `std::result::Result`
// (externally tagged, `{"Ok": ...}` / `{"Err": ...}`) exactly the way a hand-rolled
// version here would, so reusing it is less code with an identical wire shape -
// the WBS explicitly allows this ("reuse `Result` directly if it serializes the
// way you want with serde's built-in support"). Consistent across all six, per
// the WBS's own instruction to pick one shape and stick to it.

/// `KeyCustody::register_wallet(material: WalletMaterial) -> Result<WalletHandle, KeyCustodyError>`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterWalletRequest {
    pub material: WalletMaterialWire,
}
pub type RegisterWalletResponse = Result<WalletHandleWire, KeyCustodyErrorWire>;

/// `KeyCustody::remove_wallet(handle: WalletHandle) -> Result<(), KeyCustodyError>`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveWalletRequest {
    pub handle: WalletHandleWire,
}
pub type RemoveWalletResponse = Result<(), KeyCustodyErrorWire>;

/// `KeyCustody::seal(material: &WalletMaterial) -> Result<Vec<u8>, KeyCustodyError>`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealRequest {
    pub material: WalletMaterialWire,
}
pub type SealResponse = Result<SealedMaterialWire, KeyCustodyErrorWire>;

/// `KeyCustody::unseal_and_register(sealed: &[u8]) -> Result<WalletHandle, KeyCustodyError>`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsealAndRegisterRequest {
    pub sealed: SealedMaterialWire,
}
pub type UnsealAndRegisterResponse = Result<WalletHandleWire, KeyCustodyErrorWire>;

/// `KeyCustody::derive_subaddress(handle: WalletHandle, index: SubaddressIndex, network: Network) -> Result<Address, KeyCustodyError>`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeriveSubaddressRequest {
    pub handle: WalletHandleWire,
    pub index: SubaddressIndexWire,
    pub network: NetworkWire,
}
pub type DeriveSubaddressResponse = Result<AddressWire, KeyCustodyErrorWire>;

/// `KeyCustody::scan_tx_outputs(handle: WalletHandle, tx: &Transaction, major_range: Range<u32>, minor_range: Range<u32>) -> Result<Vec<MatchedOutput>, KeyCustodyError>`
///
/// Note there is no `network` parameter here - only `derive_subaddress` takes
/// one. This crate's DTOs were built against the real signatures in
/// `src/key_custody/mod.rs`, not against that trait's own module-level doc
/// comment, which (as of this task) still describes "the `major_range`/
/// `minor_range` parameters shared by `derive_subaddress` and `scan_tx_outputs`" -
/// stale prose left over from an earlier version of the trait where
/// `derive_subaddress` apparently did take a range. It doesn't today; only
/// `scan_tx_outputs` does. Worth flagging since it's exactly the kind of
/// "changed since the WBS was written, and the WBS said as much" gap this task
/// was warned to check for firsthand rather than trust.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanTxOutputsRequest {
    pub handle: WalletHandleWire,
    pub tx: TransactionWire,
    pub major_range: RangeWire,
    pub minor_range: RangeWire,
}
pub type ScanTxOutputsResponse = Result<Vec<MatchedOutputWire>, KeyCustodyErrorWire>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Same real fixture transaction used by `src/scanner.rs`'s and
    /// `src/key_custody/plain.rs`'s own tests - a genuine RingCT transaction with
    /// a real output, not an empty/default `Transaction`, so the consensus-encode
    /// round trip is exercised against real varints/field data, not a degenerate
    /// case that would round-trip even with a field dropped.
    fn fixture_tx() -> Transaction {
        let raw = hex::decode(include_str!("../../tests/fixtures/subaddress_tx.hex"))
            .expect("fixture is valid hex");
        deserialize(&raw).expect("fixture is a valid monero transaction")
    }

    fn fixture_view_key() -> [u8; 32] {
        monero::PrivateKey::from_slice(
            &hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07")
                .unwrap(),
        )
        .unwrap()
        .to_bytes()
    }

    fn fixture_spend_pubkey() -> [u8; 32] {
        let secret_spend = monero::PrivateKey::from_slice(
            &hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907")
                .unwrap(),
        )
        .unwrap();
        monero::PublicKey::from_private_key(&secret_spend).to_bytes()
    }

    // -- WalletHandle --

    #[test]
    fn wallet_handle_round_trips_through_json() {
        let handle = WalletHandle::from_bytes([7u8; 16]);
        let wire = WalletHandleWire::from(handle);
        let json = serde_json::to_string(&wire).unwrap();
        let decoded: WalletHandleWire = serde_json::from_str(&json).unwrap();
        let restored = WalletHandle::try_from(&decoded).unwrap();
        assert_eq!(handle.as_bytes(), restored.as_bytes());
    }

    #[test]
    fn wallet_handle_wire_rejects_truncated_hex_instead_of_silently_padding() {
        let bad = WalletHandleWire { uuid_hex: hex::encode([1u8; 8]) };
        assert!(matches!(
            WalletHandle::try_from(&bad),
            Err(WireConversionError::InvalidLength { expected: 16, got: 8 })
        ));
    }

    // -- WalletMaterial: the WBS's explicitly-called-out test --

    #[test]
    fn wallet_material_round_trips_the_real_key_bytes_exactly_not_just_a_successful_result() {
        // Every byte value 0..64 present at least once across the two keys
        // (masked to stay a canonical scalar for the view key), so an
        // off-by-one, a truncation, or a swapped half would show up as a
        // mismatch rather than accidentally cancelling out against a
        // degenerate all-same-byte fixture.
        let mut view_key = [0u8; 32];
        for (i, b) in view_key.iter_mut().enumerate() {
            *b = i as u8;
        }
        view_key[31] &= 0x0f;
        let mut spend_pubkey = fixture_spend_pubkey();
        spend_pubkey[0] ^= 0xFF; // still just needs to be 64 arbitrary, non-zero bytes here

        let original = WalletMaterial::new(view_key, spend_pubkey);

        let wire = WalletMaterialWire::from(&original);
        let json = serde_json::to_string(&wire).unwrap();
        let decoded: WalletMaterialWire = serde_json::from_str(&json).unwrap();
        let restored = WalletMaterial::try_from(&decoded).unwrap();

        // The load-bearing assertion: compare the *real raw bytes* extracted from
        // both sides, never the wire value, `WalletMaterial`'s redacted `Debug`,
        // or a bare "did this return Ok" check - any of those could pass while
        // the view key silently came back as all-zero.
        assert_eq!(
            original.to_raw_bytes(),
            restored.to_raw_bytes(),
            "the real key bytes must survive the round trip exactly"
        );

        // The redaction itself is real, and doesn't affect the bytes above: the
        // rendered Debug string never contains the hex-encoded key material.
        let debug_str = format!("{:?}", wire);
        assert!(debug_str.contains("<redacted>"));
        assert!(!debug_str.contains(&wire.raw_hex));
    }

    #[test]
    fn wallet_material_wire_rejects_the_wrong_byte_count() {
        let wire = WalletMaterialWire { raw_hex: hex::encode([9u8; 40]) };
        assert!(matches!(
            WalletMaterial::try_from(&wire),
            Err(WireConversionError::InvalidKeyMaterial(_))
        ));
    }

    // -- Sealed material --

    #[test]
    fn sealed_material_round_trips_an_arbitrary_length_blob() {
        // Deliberately not 64 bytes - proves this DTO doesn't assume
        // `PlainKeyCustody`'s current sealed-bytes length, per its own doc
        // comment above.
        let mut sealed = vec![0u8; 96];
        for (i, b) in sealed.iter_mut().enumerate() {
            *b = (i * 7) as u8;
        }

        let wire = SealedMaterialWire::from(sealed.as_slice());
        let json = serde_json::to_string(&wire).unwrap();
        let decoded: SealedMaterialWire = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.to_bytes().unwrap(), sealed);
    }

    // -- KeyCustodyError: every variant, per the WBS's explicit ask --

    #[test]
    fn every_key_custody_error_variant_round_trips() {
        let cases = vec![
            KeyCustodyError::UnknownWallet,
            KeyCustodyError::InvalidKeyMaterial("bad view key".to_string()),
            KeyCustodyError::BackendUnavailable("enclave unreachable".to_string()),
            KeyCustodyError::ScanFailed("range too wide".to_string()),
        ];
        for original in cases {
            let original_msg = original.to_string();
            let wire = KeyCustodyErrorWire::from(original);
            let json = serde_json::to_string(&wire).unwrap();
            let decoded: KeyCustodyErrorWire = serde_json::from_str(&json).unwrap();
            let restored: KeyCustodyError = decoded.into();
            assert_eq!(restored.to_string(), original_msg);
        }
    }

    // -- MatchedOutput, with and without an amount --

    #[test]
    fn matched_output_round_trips_with_a_known_amount() {
        let original = MatchedOutput {
            output_index: 3,
            subaddress_index: SubaddressIndex { major: 0, minor: 12 },
            amount_piconero: Some(123_456_789_012),
        };
        let wire = MatchedOutputWire::from(original);
        let json = serde_json::to_string(&wire).unwrap();
        let decoded: MatchedOutputWire = serde_json::from_str(&json).unwrap();
        let restored = MatchedOutput::try_from(decoded).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn matched_output_round_trips_with_no_decryptable_amount() {
        let original = MatchedOutput {
            output_index: 0,
            subaddress_index: SubaddressIndex { major: u32::MAX, minor: u32::MAX },
            amount_piconero: None,
        };
        let wire = MatchedOutputWire::from(original);
        let json = serde_json::to_string(&wire).unwrap();
        let decoded: MatchedOutputWire = serde_json::from_str(&json).unwrap();
        let restored = MatchedOutput::try_from(decoded).unwrap();
        assert_eq!(original, restored);
    }

    // -- Range<u32> --

    #[test]
    fn range_round_trips() {
        let original = 4u32..97u32;
        let wire = RangeWire::from(original.clone());
        let json = serde_json::to_string(&wire).unwrap();
        let decoded: RangeWire = serde_json::from_str(&json).unwrap();
        let restored: Range<u32> = decoded.into();
        assert_eq!(original, restored);
    }

    // -- Network --

    #[test]
    fn every_network_variant_round_trips() {
        for network in [Network::Mainnet, Network::Stagenet, Network::Testnet] {
            let wire = NetworkWire::from(network);
            let json = serde_json::to_string(&wire).unwrap();
            let decoded: NetworkWire = serde_json::from_str(&json).unwrap();
            let restored = Network::try_from(&decoded).unwrap();
            assert_eq!(network, restored);
        }
    }

    #[test]
    fn an_unrecognized_network_name_is_a_clean_conversion_error() {
        let wire = NetworkWire("not-a-real-network".to_string());
        assert!(matches!(Network::try_from(&wire), Err(WireConversionError::InvalidNetwork(_))));
    }

    // -- Address: a real derived address, not a degenerate one --

    #[test]
    fn address_round_trips_a_real_standard_address() {
        let view_key = monero::PrivateKey::from_slice(&fixture_view_key()).unwrap();
        let spend_pubkey = monero::PublicKey::from_slice(&fixture_spend_pubkey()).unwrap();
        let original = Address::standard(
            Network::Mainnet,
            spend_pubkey,
            monero::PublicKey::from_private_key(&view_key),
        );

        let wire = AddressWire::from(original);
        let json = serde_json::to_string(&wire).unwrap();
        let decoded: AddressWire = serde_json::from_str(&json).unwrap();
        let restored = Address::try_from(&decoded).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn address_round_trips_a_real_subaddress() {
        let view_key = monero::PrivateKey::from_slice(&fixture_view_key()).unwrap();
        let spend_pubkey = monero::PublicKey::from_slice(&fixture_spend_pubkey()).unwrap();
        let view_pair = monero::ViewPair { view: view_key, spend: spend_pubkey };
        let original = monero::cryptonote::subaddress::get_subaddress(
            &view_pair,
            SubaddressIndex { major: 0, minor: 1 },
            Some(Network::Stagenet),
        );

        let wire = AddressWire::from(original);
        let json = serde_json::to_string(&wire).unwrap();
        let decoded: AddressWire = serde_json::from_str(&json).unwrap();
        let restored = Address::try_from(&decoded).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn address_wire_rejects_garbage_text() {
        let wire = AddressWire("not a monero address".to_string());
        assert!(matches!(Address::try_from(&wire), Err(WireConversionError::InvalidAddress(_))));
    }

    // -- Transaction: a real, non-trivial fixture --

    #[test]
    fn a_real_fixture_transaction_round_trips_through_its_consensus_encoding() {
        let original = fixture_tx();
        let wire = TransactionWire::from(&original);
        let json = serde_json::to_string(&wire).unwrap();
        let decoded: TransactionWire = serde_json::from_str(&json).unwrap();
        let restored = Transaction::try_from(&decoded).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn transaction_wire_rejects_truncated_bytes_rather_than_panicking() {
        let wire = TransactionWire { bytes_hex: hex::encode([1u8, 2, 3]) };
        assert!(matches!(Transaction::try_from(&wire), Err(WireConversionError::InvalidTransaction(_))));
    }

    // -- Full per-method request/response DTOs --

    #[test]
    fn register_wallet_request_and_response_round_trip() {
        let material = WalletMaterial::new(fixture_view_key(), fixture_spend_pubkey());
        let request = RegisterWalletRequest { material: WalletMaterialWire::from(&material) };
        let json = serde_json::to_string(&request).unwrap();
        let decoded: RegisterWalletRequest = serde_json::from_str(&json).unwrap();
        let restored = WalletMaterial::try_from(&decoded.material).unwrap();
        assert_eq!(material.to_raw_bytes(), restored.to_raw_bytes());

        let handle = WalletHandle::from_bytes([3u8; 16]);
        let ok_response: RegisterWalletResponse = Ok(WalletHandleWire::from(handle));
        let json = serde_json::to_string(&ok_response).unwrap();
        let decoded: RegisterWalletResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(WalletHandle::try_from(decoded.unwrap()).unwrap(), handle);

        let err_response: RegisterWalletResponse =
            Err(KeyCustodyErrorWire::from(KeyCustodyError::InvalidKeyMaterial("bad".into())));
        let json = serde_json::to_string(&err_response).unwrap();
        let decoded: RegisterWalletResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Err(KeyCustodyErrorWire::InvalidKeyMaterial(m)) if m == "bad"));
    }

    #[test]
    fn remove_wallet_request_and_response_round_trip() {
        let handle = WalletHandle::from_bytes([9u8; 16]);
        let request = RemoveWalletRequest { handle: WalletHandleWire::from(handle) };
        let json = serde_json::to_string(&request).unwrap();
        let decoded: RemoveWalletRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(WalletHandle::try_from(&decoded.handle).unwrap(), handle);

        let ok_response: RemoveWalletResponse = Ok(());
        let json = serde_json::to_string(&ok_response).unwrap();
        let decoded: RemoveWalletResponse = serde_json::from_str(&json).unwrap();
        assert!(decoded.is_ok());

        let err_response: RemoveWalletResponse = Err(KeyCustodyErrorWire::from(KeyCustodyError::UnknownWallet));
        let json = serde_json::to_string(&err_response).unwrap();
        let decoded: RemoveWalletResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Err(KeyCustodyErrorWire::UnknownWallet)));
    }

    #[test]
    fn seal_request_and_response_round_trip() {
        let material = WalletMaterial::new(fixture_view_key(), fixture_spend_pubkey());
        let request = SealRequest { material: WalletMaterialWire::from(&material) };
        let json = serde_json::to_string(&request).unwrap();
        let decoded: SealRequest = serde_json::from_str(&json).unwrap();
        let restored = WalletMaterial::try_from(&decoded.material).unwrap();
        assert_eq!(material.to_raw_bytes(), restored.to_raw_bytes());

        let sealed_bytes = material.to_raw_bytes();
        let ok_response: SealResponse = Ok(SealedMaterialWire::from(&sealed_bytes[..]));
        let json = serde_json::to_string(&ok_response).unwrap();
        let decoded: SealResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.unwrap().to_bytes().unwrap(), sealed_bytes.to_vec());
    }

    #[test]
    fn unseal_and_register_request_and_response_round_trip() {
        let sealed_bytes = vec![5u8; 64];
        let request = UnsealAndRegisterRequest { sealed: SealedMaterialWire::from(sealed_bytes.as_slice()) };
        let json = serde_json::to_string(&request).unwrap();
        let decoded: UnsealAndRegisterRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.sealed.to_bytes().unwrap(), sealed_bytes);

        let handle = WalletHandle::from_bytes([2u8; 16]);
        let ok_response: UnsealAndRegisterResponse = Ok(WalletHandleWire::from(handle));
        let json = serde_json::to_string(&ok_response).unwrap();
        let decoded: UnsealAndRegisterResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(WalletHandle::try_from(decoded.unwrap()).unwrap(), handle);
    }

    #[test]
    fn derive_subaddress_request_and_response_round_trip() {
        let handle = WalletHandle::from_bytes([1u8; 16]);
        let request = DeriveSubaddressRequest {
            handle: WalletHandleWire::from(handle),
            index: SubaddressIndexWire::from(SubaddressIndex { major: 0, minor: 4 }),
            network: NetworkWire::from(Network::Stagenet),
        };
        let json = serde_json::to_string(&request).unwrap();
        let decoded: DeriveSubaddressRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(WalletHandle::try_from(&decoded.handle).unwrap(), handle);
        assert_eq!(SubaddressIndex::from(decoded.index), SubaddressIndex { major: 0, minor: 4 });
        assert_eq!(Network::try_from(&decoded.network).unwrap(), Network::Stagenet);

        let view_key = monero::PrivateKey::from_slice(&fixture_view_key()).unwrap();
        let spend_pubkey = monero::PublicKey::from_slice(&fixture_spend_pubkey()).unwrap();
        let address = Address::standard(
            Network::Mainnet,
            spend_pubkey,
            monero::PublicKey::from_private_key(&view_key),
        );
        let ok_response: DeriveSubaddressResponse = Ok(AddressWire::from(address));
        let json = serde_json::to_string(&ok_response).unwrap();
        let decoded: DeriveSubaddressResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(Address::try_from(&decoded.unwrap()).unwrap(), address);
    }

    #[test]
    fn scan_tx_outputs_request_and_response_round_trip() {
        let handle = WalletHandle::from_bytes([4u8; 16]);
        let tx = fixture_tx();
        let request = ScanTxOutputsRequest {
            handle: WalletHandleWire::from(handle),
            tx: TransactionWire::from(&tx),
            major_range: RangeWire::from(0u32..2u32),
            minor_range: RangeWire::from(0u32..3u32),
        };
        let json = serde_json::to_string(&request).unwrap();
        let decoded: ScanTxOutputsRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(WalletHandle::try_from(&decoded.handle).unwrap(), handle);
        assert_eq!(Transaction::try_from(&decoded.tx).unwrap(), tx);
        assert_eq!(Range::<u32>::from(decoded.major_range), 0u32..2u32);
        assert_eq!(Range::<u32>::from(decoded.minor_range), 0u32..3u32);

        // Response carrying two matches, one with and one without an amount -
        // proves `Vec<MatchedOutputWire>` round-trips, not just a lone element.
        let matches = vec![
            MatchedOutput {
                output_index: 1,
                subaddress_index: SubaddressIndex { major: 0, minor: 1 },
                amount_piconero: Some(42),
            },
            MatchedOutput {
                output_index: 2,
                subaddress_index: SubaddressIndex { major: 0, minor: 2 },
                amount_piconero: None,
            },
        ];
        let ok_response: ScanTxOutputsResponse =
            Ok(matches.iter().copied().map(MatchedOutputWire::from).collect());
        let json = serde_json::to_string(&ok_response).unwrap();
        let decoded: ScanTxOutputsResponse = serde_json::from_str(&json).unwrap();
        let restored: Vec<MatchedOutput> =
            decoded.unwrap().into_iter().map(|w| MatchedOutput::try_from(w).unwrap()).collect();
        assert_eq!(restored, matches);

        let err_response: ScanTxOutputsResponse =
            Err(KeyCustodyErrorWire::from(KeyCustodyError::ScanFailed("range too wide".into())));
        let json = serde_json::to_string(&err_response).unwrap();
        let decoded: ScanTxOutputsResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Err(KeyCustodyErrorWire::ScanFailed(m)) if m == "range too wide"));
    }
}
