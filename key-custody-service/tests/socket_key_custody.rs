//! Acceptance tests for WBS 2.1.2 ("Socket-based `KeyCustody` implementation").
//!
//! Two groups of tests live here:
//!
//! 1. **The ported `plain.rs` suite.** The WBS's own acceptance test for this
//!    step is `src/key_custody/plain.rs`'s existing 12 `#[tokio::test]`s,
//!    "identical assertions, different backend" - run against a real
//!    `SocketKeyCustody` talking over a real Unix socket to a real
//!    `KeyCustodyServer` wrapping a real `PlainKeyCustody`, instead of calling
//!    `PlainKeyCustody` directly. 10 of the 12 port verbatim: same scenario,
//!    same assertions, only the concrete `KeyCustody` value under test
//!    changes. The other two (`repeated_scans_over_same_range_reuse_the_
//!    cached_table` and `removing_a_wallet_scrubs_its_view_key_rather_than_
//!    leaving_it_in_freed_memory`) each assert on `PlainKeyCustody`'s own
//!    private internals in the original, which a socket *client* structurally
//!    cannot see - see each test's own doc comment below for exactly what was
//!    kept, what was dropped, and why.
//!
//! 2. **New tests specifically for the socket mechanism itself**, beyond what
//!    porting the existing suite exercises: a real second *process* (not just
//!    a background task in this same test binary) doing a full round trip,
//!    and the socket-specific failure modes the WBS calls out by name -
//!    unreachable at connect time, closed/crashed mid-connection, and garbage
//!    bytes on the wire.
//!
//! **Why only one test spawns a real second process.** Every ported test and
//! every failure-mode test below runs `KeyCustodyServer::listen` as a
//! `tokio::spawn`ed background task *inside this same test process* - that's
//! legitimate and deliberate: it's still a real Unix socket, a real separate
//! `PlainKeyCustody` instance, and real `serde_json`-over-the-wire framing on
//! both ends, so it genuinely exercises the protocol; it's just faster than
//! paying `std::process::Command::spawn`'s process-creation cost 20-some times
//! over. Exactly one test
//! (`a_real_child_process_running_the_compiled_server_binary_serves_a_full_
//! round_trip_over_a_real_socket`) launches the actual compiled
//! `key-custody-server` binary as a genuinely separate OS process via
//! `env!("CARGO_BIN_EXE_key-custody-server")` - that's the one test in this
//! whole file that actually proves the "compromising the main engine process
//! alone never yields the keys" claim has a real mechanism behind it, since
//! every other test's "server" still lives in the same address space as the
//! thing calling it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use key_custody_service::client::SocketKeyCustody;
use key_custody_service::protocol::{read_frame, write_frame, KeyCustodyRequest, KeyCustodyResponse};
use key_custody_service::server::KeyCustodyServer;
use key_custody_service::WalletHandleWire;
use moneropay_core::key_custody::{
    KeyCustody, KeyCustodyError, Network, PlainKeyCustody, SubaddressIndex, WalletHandle,
    WalletMaterial,
};
use monero::consensus::encode::deserialize;
use monero::{PrivateKey, PublicKey, Transaction};
use tokio::net::{UnixListener, UnixStream};

// ---------------------------------------------------------------------------
// Test harness (not itself part of the acceptance test - just plumbing)
// ---------------------------------------------------------------------------

/// A unique-per-call socket path under the OS temp dir. Uniqueness comes from
/// this process's pid plus a monotonically increasing counter (not just a
/// timestamp - two calls in the same test binary can land in the same
/// nanosecond on a fast machine) rather than pulling in a new dependency
/// (`tempfile`/`uuid`) just for test scaffolding this crate doesn't otherwise
/// need.
fn temp_socket_path(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!("kcs-test-{tag}-{}-{n}.sock", std::process::id()));
    path
}

/// Best-effort socket-file cleanup, run via `Drop` so it still fires if a test
/// panics partway through (the socket file itself is harmless clutter either
/// way - this is just tidiness, not correctness).
struct CleanupSocket(PathBuf);
impl Drop for CleanupSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Connect, retrying briefly. `KeyCustodyServer::listen` binds the socket
/// before it starts accepting, but that bind happens inside the `tokio::spawn`ed
/// task's future, which isn't guaranteed to have been polled even once by the
/// time `tokio::spawn` returns control to the caller - a single immediate
/// connect attempt would be a real, if usually-fast-enough-to-hide, race.
async fn connect_with_retry(path: &Path) -> SocketKeyCustody {
    for _ in 0..200 {
        match SocketKeyCustody::connect(path).await {
            Ok(client) => return client,
            Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
        }
    }
    panic!("key-custody-server never became reachable at {}", path.display());
}

/// A real `KeyCustodyServer` (wrapping a fresh `PlainKeyCustody`) running as a
/// background task in this test process, plus a `SocketKeyCustody` already
/// connected to it. Bundled together with the socket path's cleanup guard so
/// a test just holds one value.
struct TestServer {
    client: SocketKeyCustody,
    _cleanup: CleanupSocket,
}

async fn spawn_server_and_client(tag: &str) -> TestServer {
    let socket_path = temp_socket_path(tag);
    let server = Arc::new(KeyCustodyServer::new(PlainKeyCustody::default()));
    let listen_path = socket_path.clone();
    tokio::spawn(async move {
        if let Err(e) = server.listen(&listen_path).await {
            eprintln!("test key-custody-server on {}: {e}", listen_path.display());
        }
    });
    let client = connect_with_retry(&socket_path).await;
    TestServer { client, _cleanup: CleanupSocket(socket_path) }
}

fn random_scalar_bytes(seed: u8) -> [u8; 32] {
    // Same fixture convention `plain.rs`'s own tests use: deterministic, not
    // cryptographically random, just a distinct valid scalar per call site.
    let mut bytes = [seed; 32];
    bytes[31] &= 0x0f;
    bytes
}

fn fixture_tx() -> Transaction {
    let raw = hex::decode(include_str!("../../tests/fixtures/subaddress_tx.hex"))
        .expect("fixture is valid hex");
    deserialize(&raw).expect("fixture is a valid monero transaction")
}

/// The same fixture view/spend keys `plain.rs`'s own
/// `scan_tx_outputs_finds_output_paid_to_subaddress` test uses - known, ahead
/// of time, to have a real output in `fixture_tx()` addressed to subaddress
/// 0/1 under this exact pair.
fn fixture_view_key() -> PrivateKey {
    PrivateKey::from_slice(
        &hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07").unwrap(),
    )
    .unwrap()
}
fn fixture_spend_pubkey() -> PublicKey {
    let secret_spend = PrivateKey::from_slice(
        &hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907").unwrap(),
    )
    .unwrap();
    PublicKey::from_private_key(&secret_spend)
}

// ---------------------------------------------------------------------------
// 1. The ported `plain.rs` suite (12 tests, matching that file's own names)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn register_then_derive_subaddress_roundtrip() {
    let view_key = PrivateKey::from_slice(&random_scalar_bytes(1)).unwrap();
    let spend_key = PrivateKey::from_slice(&random_scalar_bytes(2)).unwrap();
    let spend_pubkey = PublicKey::from_private_key(&spend_key);

    let ts = spawn_server_and_client("roundtrip").await;
    let handle = ts
        .client
        .register_wallet(WalletMaterial::new(view_key.to_bytes(), spend_pubkey.to_bytes()))
        .await
        .unwrap();

    let primary = ts
        .client
        .derive_subaddress(handle, SubaddressIndex::default(), Network::Mainnet)
        .await
        .unwrap();
    assert_eq!(primary.public_spend, spend_pubkey);
    assert_eq!(primary.public_view, PublicKey::from_private_key(&view_key));

    ts.client.remove_wallet(handle).await.unwrap();
    let err = ts
        .client
        .derive_subaddress(handle, SubaddressIndex::default(), Network::Mainnet)
        .await
        .unwrap_err();
    assert!(matches!(err, KeyCustodyError::UnknownWallet));
}

#[tokio::test]
async fn scan_tx_outputs_finds_output_paid_to_subaddress() {
    let tx = fixture_tx();
    let ts = spawn_server_and_client("scan-finds").await;
    let handle = ts
        .client
        .register_wallet(WalletMaterial::new(
            fixture_view_key().to_bytes(),
            fixture_spend_pubkey().to_bytes(),
        ))
        .await
        .unwrap();

    let matches = ts.client.scan_tx_outputs(handle, &tx, 0..2, 0..3).await.unwrap();

    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].output_index, 1);
    assert_eq!(matches[0].subaddress_index, SubaddressIndex { major: 0, minor: 1 });
    assert!(matches[0].amount_piconero.unwrap() > 0);
}

/// Ported *partially*, on purpose - see the WBS 2.1.2 brief's own framing of
/// this exact test. The original asserts `rebuild_count(&custody, handle) ==
/// 1` after three same-range scans and `== 2` after a range change, reading
/// `PlainKeyCustody.wallets.read().unwrap().get(&handle).unwrap().
/// rebuild_count` directly - a `#[cfg(test)]`-only `AtomicU64` field on the
/// private `WalletEntry` struct.
///
/// That assertion cannot be ported here, for two independent reasons, not
/// just one:
/// 1. It isn't observable through the `KeyCustody` trait at all -
///    `scan_tx_outputs` returns the identical, correct result whether or not
///    the lookup table was actually rebuilt, exactly as `plain.rs`'s own
///    module doc comment says. A socket client only ever sees trait-level
///    behaviour, by construction - there is no wire message that could carry
///    "did the cache rebuild" without this crate inventing one purely to let
///    a test peek behind the boundary the whole point of this step is to
///    prove holds.
/// 2. Even a test harness with a live, same-process `Arc<PlainKeyCustody>`
///    (which `TestServer` here deliberately does *not* expose, precisely
///    because of this) could not reach it: `wallets` is a private field of
///    `PlainKeyCustody`, visible only within `src/key_custody/plain.rs`'s own
///    module tree - not from `key_custody_service`, and not even from
///    `src/key_custody/mod.rs`, its own parent module, under Rust's privacy
///    rules. And `rebuild_count` itself is `#[cfg(test)]`-gated on
///    `WalletEntry`, so it isn't even *compiled into* `WalletEntry` when
///    `moneropay-core` is built as an ordinary path dependency the way this
///    crate builds it - "same process" is necessary but nowhere near
///    sufficient for "same field access."
///
/// So: only the black-box behaviour ports - three scans over the same range,
/// then a fourth over a genuinely wider range, all returning correct results
/// (the actual caching mechanism, if it were broken, would surface as *wrong*
/// results here, e.g. stale matches from a table that should have been
/// rebuilt but wasn't - so this isn't a no-op test, it just doesn't measure
/// the caching-efficiency claim specifically). The caching-efficiency
/// assertion itself is dropped, not silently - this comment is the trace of
/// that decision.
#[tokio::test]
async fn repeated_scans_over_same_range_reuse_the_cached_table() {
    let tx = fixture_tx();
    let ts = spawn_server_and_client("cache-range").await;
    let handle = ts
        .client
        .register_wallet(WalletMaterial::new(
            fixture_view_key().to_bytes(),
            fixture_spend_pubkey().to_bytes(),
        ))
        .await
        .unwrap();

    for _ in 0..3 {
        let matches = ts.client.scan_tx_outputs(handle, &tx, 0..2, 0..3).await.unwrap();
        assert_eq!(matches.len(), 1, "same-range scan should still find the real output");
    }

    let widened = ts.client.scan_tx_outputs(handle, &tx, 0..2, 0..4).await.unwrap();
    assert_eq!(widened.len(), 1, "a genuinely wider range must still find the same real output");
}

#[tokio::test]
async fn seal_then_unseal_survives_a_simulated_restart() {
    let view_key = PrivateKey::from_slice(&random_scalar_bytes(3)).unwrap();
    let spend_key = PrivateKey::from_slice(&random_scalar_bytes(4)).unwrap();
    let spend_pubkey = PublicKey::from_private_key(&spend_key);
    let material = WalletMaterial::new(view_key.to_bytes(), spend_pubkey.to_bytes());

    // "Before restart": one server/client pair, register, derive, seal.
    let before = spawn_server_and_client("restart-before").await;
    let handle_before = before.client.register_wallet(material.clone()).await.unwrap();
    let address_before = before
        .client
        .derive_subaddress(handle_before, SubaddressIndex { major: 0, minor: 7 }, Network::Mainnet)
        .await
        .unwrap();
    let sealed = before.client.seal(&material).await.unwrap();

    // "After restart": a genuinely independent server/client pair - a fresh
    // `PlainKeyCustody` behind a fresh socket, nothing shared with `before`
    // except the sealed bytes just produced, mirroring a real process restart
    // where the old in-memory registry (and the old handle) is simply gone.
    let after = spawn_server_and_client("restart-after").await;
    let handle_after = after.client.unseal_and_register(&sealed).await.unwrap();
    assert_ne!(handle_before, handle_after);

    let address_after = after
        .client
        .derive_subaddress(handle_after, SubaddressIndex { major: 0, minor: 7 }, Network::Mainnet)
        .await
        .unwrap();
    assert_eq!(address_before, address_after);
}

#[tokio::test]
async fn index_zero_derives_a_standard_address_not_an_unpayable_subaddress() {
    let view_key = PrivateKey::from_slice(&random_scalar_bytes(5)).unwrap();
    let spend_key = PrivateKey::from_slice(&random_scalar_bytes(6)).unwrap();
    let spend_pubkey = PublicKey::from_private_key(&spend_key);

    let ts = spawn_server_and_client("index-zero").await;
    let handle = ts
        .client
        .register_wallet(WalletMaterial::new(view_key.to_bytes(), spend_pubkey.to_bytes()))
        .await
        .unwrap();

    let primary = ts
        .client
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
        monero::Address::standard(Network::Mainnet, spend_pubkey, PublicKey::from_private_key(&view_key))
    );
    assert!(primary.to_string().starts_with('4'), "got {primary}");

    let first_order_address = ts
        .client
        .derive_subaddress(handle, SubaddressIndex { major: 0, minor: 1 }, Network::Mainnet)
        .await
        .unwrap();
    assert_eq!(first_order_address.addr_type, monero::AddressType::SubAddress);
    assert_ne!(first_order_address.public_spend, spend_pubkey);
}

#[tokio::test]
async fn an_index_at_the_top_of_the_u32_range_derives_without_overflowing() {
    let view_key = PrivateKey::from_slice(&random_scalar_bytes(7)).unwrap();
    let spend_key = PrivateKey::from_slice(&random_scalar_bytes(8)).unwrap();
    let ts = spawn_server_and_client("u32-top").await;
    let handle = ts
        .client
        .register_wallet(WalletMaterial::new(
            view_key.to_bytes(),
            PublicKey::from_private_key(&spend_key).to_bytes(),
        ))
        .await
        .unwrap();

    let extreme = ts
        .client
        .derive_subaddress(handle, SubaddressIndex { major: u32::MAX, minor: u32::MAX }, Network::Mainnet)
        .await
        .unwrap();
    assert_eq!(extreme.addr_type, monero::AddressType::SubAddress);

    let neighbour = ts
        .client
        .derive_subaddress(
            handle,
            SubaddressIndex { major: u32::MAX, minor: u32::MAX - 1 },
            Network::Mainnet,
        )
        .await
        .unwrap();
    assert_ne!(extreme, neighbour);
}

#[tokio::test]
async fn an_absurdly_wide_scan_range_is_refused_rather_than_hanging_forever() {
    let tx = fixture_tx();
    let view_key = PrivateKey::from_slice(&random_scalar_bytes(9)).unwrap();
    let spend_key = PrivateKey::from_slice(&random_scalar_bytes(10)).unwrap();
    let ts = spawn_server_and_client("wide-scan").await;
    let handle = ts
        .client
        .register_wallet(WalletMaterial::new(
            view_key.to_bytes(),
            PublicKey::from_private_key(&spend_key).to_bytes(),
        ))
        .await
        .unwrap();

    let err = ts.client.scan_tx_outputs(handle, &tx, 0..1, 0..u32::MAX).await.unwrap_err();
    assert!(matches!(err, KeyCustodyError::ScanFailed(_)), "got {err:?}");

    // A realistic range is still accepted - proves the refusal above is about
    // the range's size, not a general scan-tx-outputs breakage.
    ts.client.scan_tx_outputs(handle, &tx, 0..1, 0..64).await.unwrap();
}

/// Ported *partially* - see this test's counterpart doc comment above
/// (`repeated_scans_over_same_range_reuse_the_cached_table`) for the general
/// shape of the argument; the specifics here are different.
///
/// The original test makes three assertions after removing a wallet:
/// 1. `custody.wallets.read().unwrap().is_empty()` - direct private-field
///    access.
/// 2. A second `remove_wallet` on the same (now-removed) handle returns
///    `UnknownWallet`.
/// 3. A `scan_tx_outputs` against the removed handle returns `UnknownWallet`.
///
/// (2) and (3) are pure `KeyCustody`-trait black-box behaviour - nothing
/// about them requires seeing inside `PlainKeyCustody` - so both port
/// verbatim below. (1) does not, for the same reason
/// `rebuild_count` above doesn't: `wallets` is a private field of
/// `PlainKeyCustody`, visible only within `src/key_custody/plain.rs`'s own
/// module - not reachable from this crate no matter how the test harness
/// here is structured (there is no accessor exposing it, and adding one
/// purely to satisfy this one assertion would mean widening
/// `PlainKeyCustody`'s public surface for a test-only need the trait itself
/// was never meant to expose). Dropped, with this comment standing in its
/// place rather than a silent gap.
#[tokio::test]
async fn removing_a_wallet_scrubs_its_view_key_rather_than_leaving_it_in_freed_memory() {
    let view_key = PrivateKey::from_slice(&random_scalar_bytes(11)).unwrap();
    let spend_key = PrivateKey::from_slice(&random_scalar_bytes(12)).unwrap();
    let ts = spawn_server_and_client("remove-scrub").await;
    let handle = ts
        .client
        .register_wallet(WalletMaterial::new(
            view_key.to_bytes(),
            PublicKey::from_private_key(&spend_key).to_bytes(),
        ))
        .await
        .unwrap();

    ts.client.remove_wallet(handle).await.unwrap();
    assert!(matches!(ts.client.remove_wallet(handle).await.unwrap_err(), KeyCustodyError::UnknownWallet));

    let tx = fixture_tx();
    assert!(matches!(
        ts.client.scan_tx_outputs(handle, &tx, 0..1, 0..2).await.unwrap_err(),
        KeyCustodyError::UnknownWallet
    ));
}

#[tokio::test]
async fn registering_the_same_material_twice_yields_independent_handles_that_both_work() {
    let view_key = PrivateKey::from_slice(&random_scalar_bytes(13)).unwrap();
    let spend_key = PrivateKey::from_slice(&random_scalar_bytes(14)).unwrap();
    let material =
        WalletMaterial::new(view_key.to_bytes(), PublicKey::from_private_key(&spend_key).to_bytes());

    let ts = spawn_server_and_client("dup-register").await;
    let sealed = ts.client.seal(&material).await.unwrap();
    let first = ts.client.unseal_and_register(&sealed).await.unwrap();
    let second = ts.client.unseal_and_register(&sealed).await.unwrap();
    assert_ne!(first, second, "each registration must get its own handle");

    let index = SubaddressIndex { major: 0, minor: 3 };
    let from_first = ts.client.derive_subaddress(first, index, Network::Mainnet).await.unwrap();
    let from_second = ts.client.derive_subaddress(second, index, Network::Mainnet).await.unwrap();
    assert_eq!(from_first, from_second);

    ts.client.remove_wallet(first).await.unwrap();
    assert_eq!(
        ts.client.derive_subaddress(second, index, Network::Mainnet).await.unwrap(),
        from_second,
        "removing one registration must not invalidate an independent one"
    );
}

#[tokio::test]
async fn seal_rejects_truncated_or_overlong_material_instead_of_silently_padding() {
    for bad_len in [0usize, 31, 32, 63, 65, 128] {
        let ts = spawn_server_and_client(&format!("bad-len-{bad_len}")).await;
        let err = ts.client.unseal_and_register(&vec![7u8; bad_len]).await.unwrap_err();
        assert!(
            matches!(err, KeyCustodyError::InvalidKeyMaterial(_)),
            "{bad_len} bytes should be rejected, got {err:?}"
        );
    }
}

#[tokio::test]
async fn seal_round_trips_every_byte_of_both_keys_including_high_bytes() {
    let mut view_bytes = [0u8; 32];
    for (i, b) in view_bytes.iter_mut().enumerate() {
        *b = i as u8;
    }
    view_bytes[31] &= 0x0f;
    let spend_bytes =
        PublicKey::from_private_key(&PrivateKey::from_slice(&random_scalar_bytes(15)).unwrap()).to_bytes();

    let material = WalletMaterial::new(view_bytes, spend_bytes);
    let ts = spawn_server_and_client("seal-bytes").await;
    let sealed = ts.client.seal(&material).await.unwrap();
    assert_eq!(sealed.len(), 64);
    assert_eq!(&sealed[..32], &view_bytes[..]);
    assert_eq!(&sealed[32..], &spend_bytes[..]);

    // Round-trip through the socket a second time (unseal_and_register,
    // rather than reaching for `WalletMaterial::from_raw_bytes` directly the
    // way `plain.rs`'s own version of this test does) - `to_view_pair` isn't
    // reachable outside a `KeyCustody` implementation (see its own doc
    // comment in `src/key_custody/mod.rs`), so the trait-level equivalent of
    // "does the restored key material behave identically" is deriving the
    // same address from both a fresh registration of the pre-seal material
    // and a registration recovered via `unseal_and_register`.
    let handle_direct = ts.client.register_wallet(material.clone()).await.unwrap();
    let handle_restored = ts.client.unseal_and_register(&sealed).await.unwrap();
    let index = SubaddressIndex { major: 0, minor: 2 };
    let address_direct = ts.client.derive_subaddress(handle_direct, index, Network::Mainnet).await.unwrap();
    let address_restored =
        ts.client.derive_subaddress(handle_restored, index, Network::Mainnet).await.unwrap();
    assert_eq!(address_direct, address_restored);
}

#[tokio::test]
async fn concurrent_registrations_and_removals_never_cross_wires_between_wallets() {
    let ts = spawn_server_and_client("concurrent").await;
    let client = Arc::new(ts.client);
    let mut expected = Vec::new();
    for seed in 20u8..24 {
        let view_key = PrivateKey::from_slice(&random_scalar_bytes(seed)).unwrap();
        let spend_key = PrivateKey::from_slice(&random_scalar_bytes(seed + 40)).unwrap();
        let spend_pubkey = PublicKey::from_private_key(&spend_key);
        let handle = client
            .register_wallet(WalletMaterial::new(view_key.to_bytes(), spend_pubkey.to_bytes()))
            .await
            .unwrap();
        let index = SubaddressIndex { major: 0, minor: 1 };
        let address = client.derive_subaddress(handle, index, Network::Mainnet).await.unwrap();
        expected.push((handle, address));
    }

    let mut tasks = Vec::new();
    for (handle, address) in expected.clone() {
        for _ in 0..8 {
            let client = Arc::clone(&client);
            tasks.push(tokio::spawn(async move {
                let index = SubaddressIndex { major: 0, minor: 1 };
                assert_eq!(
                    client.derive_subaddress(handle, index, Network::Mainnet).await.unwrap(),
                    address,
                    "a handle resolved to another wallet's key material"
                );
            }));
        }
    }
    // Churn the registry concurrently with those reads - all serialized onto
    // the one shared connection by `SocketKeyCustody`'s own mutex (see
    // `client.rs`'s module doc comment), but still genuinely concurrent from
    // the caller's point of view, and still exercising the server's own
    // per-connection-task, shared-`Arc<PlainKeyCustody>` handling.
    for seed in 60u8..68 {
        let client = Arc::clone(&client);
        tasks.push(tokio::spawn(async move {
            let view_key = PrivateKey::from_slice(&random_scalar_bytes(seed)).unwrap();
            let spend_key = PrivateKey::from_slice(&random_scalar_bytes(seed + 20)).unwrap();
            let handle = client
                .register_wallet(WalletMaterial::new(
                    view_key.to_bytes(),
                    PublicKey::from_private_key(&spend_key).to_bytes(),
                ))
                .await
                .unwrap();
            client.remove_wallet(handle).await.unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }

    for (handle, address) in expected {
        let index = SubaddressIndex { major: 0, minor: 1 };
        assert_eq!(client.derive_subaddress(handle, index, Network::Mainnet).await.unwrap(), address);
    }
}

// ---------------------------------------------------------------------------
// 2. New tests for the socket mechanism itself
// ---------------------------------------------------------------------------

/// The one test in this file that runs the *compiled binary* as a genuinely
/// separate OS process, rather than a background task sharing this test
/// process's address space - see this file's own top doc comment for why that
/// distinction matters and why every other test here doesn't bother with it.
#[tokio::test]
async fn a_real_child_process_running_the_compiled_server_binary_serves_a_full_round_trip_over_a_real_socket()
{
    let socket_path = temp_socket_path("subprocess");
    let _cleanup = CleanupSocket(socket_path.clone());

    let child = std::process::Command::new(env!("CARGO_BIN_EXE_key-custody-server"))
        .arg(&socket_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to launch the compiled key-custody-server binary");

    // Kill the child even if an assertion below panics - a leaked
    // `key-custody-server` process holding a stale socket open would make the
    // *next* run of this test flaky for an unrelated reason (a stale socket
    // file at the same path, on a machine where pid+counter happened to
    // repeat - astronomically unlikely, but the guard is nearly free).
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut guard = KillOnDrop(child);

    let client = connect_with_retry(&socket_path).await;

    // The real acceptance scenario: register -> derive -> scan -> remove,
    // using the same known-good fixture wallet `plain.rs`'s own scan test
    // uses, so the scan step finds a genuine match rather than trivially
    // returning nothing.
    let handle = client
        .register_wallet(WalletMaterial::new(
            fixture_view_key().to_bytes(),
            fixture_spend_pubkey().to_bytes(),
        ))
        .await
        .expect("register_wallet over the real child process");

    let address = client
        .derive_subaddress(handle, SubaddressIndex { major: 0, minor: 1 }, Network::Stagenet)
        .await
        .expect("derive_subaddress over the real child process");
    assert_eq!(address.addr_type, monero::AddressType::SubAddress);

    let tx = fixture_tx();
    let matches = client
        .scan_tx_outputs(handle, &tx, 0..2, 0..3)
        .await
        .expect("scan_tx_outputs over the real child process");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].subaddress_index, SubaddressIndex { major: 0, minor: 1 });

    client.remove_wallet(handle).await.expect("remove_wallet over the real child process");
    let err = client
        .derive_subaddress(handle, SubaddressIndex::default(), Network::Mainnet)
        .await
        .unwrap_err();
    assert!(matches!(err, KeyCustodyError::UnknownWallet));

    // Explicit, even though `KillOnDrop` would also do it on scope exit -
    // makes the intent visible at the point that matters, right after the
    // scenario this test exists to prove is done.
    let _ = guard.0.kill();
}

#[tokio::test]
async fn connecting_to_a_socket_that_nothing_is_listening_on_is_a_clean_error_not_a_panic_or_hang() {
    let socket_path = temp_socket_path("never-bound");
    // Deliberately never bound by anything - this path has never existed.
    match SocketKeyCustody::connect(&socket_path).await {
        Err(KeyCustodyError::BackendUnavailable(_)) => {}
        Ok(_) => panic!("connecting to a socket nothing is listening on unexpectedly succeeded"),
        Err(other) => panic!("expected BackendUnavailable, got a different KeyCustodyError: {other}"),
    }
}

#[tokio::test]
async fn a_clean_error_not_a_hang_when_the_server_closes_the_connection_mid_session() {
    let socket_path = temp_socket_path("mid-session-close");
    let _cleanup = CleanupSocket(socket_path.clone());
    let listener = UnixListener::bind(&socket_path).unwrap();

    tokio::spawn(async move {
        // Answer exactly one request correctly, then let `stream` drop -
        // simulating the server process being killed (or crashing) in the
        // gap between two calls a client makes on what it still believes is
        // one live connection, which is exactly the scenario a real deployed
        // key-custody-service restarting mid-shift would look like from the
        // engine's point of view.
        if let Ok((mut stream, _addr)) = listener.accept().await {
            let request: Result<Option<KeyCustodyRequest>, _> = read_frame(&mut stream).await;
            if let Ok(Some(KeyCustodyRequest::RegisterWallet(_))) = request {
                let handle = WalletHandle::from_bytes([9u8; 16]);
                let response = KeyCustodyResponse::RegisterWallet(Ok(WalletHandleWire::from(handle)));
                let _ = write_frame(&mut stream, &response).await;
            }
            // `stream` dropped here: the connection closes.
        }
    });

    let client = connect_with_retry(&socket_path).await;
    let material = WalletMaterial::new(random_scalar_bytes(200), random_scalar_bytes(201));
    let handle = client.register_wallet(material).await.expect("first call should succeed normally");
    assert_eq!(handle, WalletHandle::from_bytes([9u8; 16]));

    // Second call on the same, now-closed connection.
    let err = client.remove_wallet(handle).await.unwrap_err();
    assert!(matches!(err, KeyCustodyError::BackendUnavailable(_)), "expected a clean error, got {err}");
}

#[tokio::test]
async fn client_returns_a_clean_error_when_the_peer_sends_garbage_instead_of_a_valid_response() {
    use tokio::io::AsyncWriteExt;

    let socket_path = temp_socket_path("garbage-to-client");
    let _cleanup = CleanupSocket(socket_path.clone());
    let listener = UnixListener::bind(&socket_path).unwrap();

    tokio::spawn(async move {
        if let Ok((mut stream, _addr)) = listener.accept().await {
            // A well-formed length prefix (3 bytes) followed by bytes that
            // aren't valid JSON at all, let alone a `KeyCustodyResponse`.
            // Deliberately doesn't bother reading the client's actual
            // request first - a genuinely malicious or badly-broken peer
            // has no obligation to play along with the protocol either.
            let _ = stream.write_all(&3u32.to_be_bytes()).await;
            let _ = stream.write_all(b"???").await;
        }
    });

    let client = connect_with_retry(&socket_path).await;
    let material = WalletMaterial::new(random_scalar_bytes(210), random_scalar_bytes(211));
    let err = client.register_wallet(material).await.unwrap_err();
    assert!(matches!(err, KeyCustodyError::BackendUnavailable(_)), "expected a clean error, got {err}");
}

#[tokio::test]
async fn garbage_bytes_from_a_raw_connection_are_rejected_cleanly_without_taking_down_the_server() {
    use tokio::io::AsyncWriteExt;

    let socket_path = temp_socket_path("garbage-to-server");
    let _cleanup = CleanupSocket(socket_path.clone());
    let server = Arc::new(KeyCustodyServer::new(PlainKeyCustody::default()));
    {
        let server = Arc::clone(&server);
        let listen_path = socket_path.clone();
        tokio::spawn(async move {
            let _ = server.listen(&listen_path).await;
        });
    }
    // Wait for the listener to actually be bound before dialing raw
    // connections at it (there's no `SocketKeyCustody::connect` retry helper
    // to lean on here, since these connections are deliberately not going
    // through that type).
    for _ in 0..200 {
        if UnixStream::connect(&socket_path).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Case 1: a length prefix that claims a frame far larger than
    // `MAX_FRAME_BYTES` - proves the server refuses up front rather than
    // trying to allocate (or block waiting to read) an attacker-chosen
    // amount of memory.
    {
        let mut raw = UnixStream::connect(&socket_path).await.unwrap();
        raw.write_all(&[0xFF, 0xFF, 0xFF, 0xFF]).await.unwrap();
        // The server should close its end after rejecting this - reading
        // from our side should observe that (EOF or a reset), not hang.
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 1];
            raw.read(&mut buf).await
        })
        .await;
        assert!(result.is_ok(), "server did not close the connection within 5s after an oversized frame");
    }

    // Case 2: a valid, small length prefix followed by bytes that aren't
    // valid JSON at all.
    {
        let mut raw = UnixStream::connect(&socket_path).await.unwrap();
        raw.write_all(&3u32.to_be_bytes()).await.unwrap();
        raw.write_all(b"!!!").await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 1];
            raw.read(&mut buf).await
        })
        .await;
        assert!(result.is_ok(), "server did not close the connection within 5s after a malformed payload");
    }

    // The server itself must still be alive and functioning normally for a
    // well-behaved client after both of the above - proving the garbage
    // connections were isolated per-connection failures, not something that
    // wedged or crashed the whole server.
    let client = connect_with_retry(&socket_path).await;
    let handle = client
        .register_wallet(WalletMaterial::new(random_scalar_bytes(220), random_scalar_bytes(221)))
        .await
        .expect("server should still serve a well-behaved client after receiving garbage");
    client.remove_wallet(handle).await.unwrap();
}
