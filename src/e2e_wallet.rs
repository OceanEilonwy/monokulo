//! Pure-Rust construction, signing, and broadcast of a real Monero transaction -
//! everything needed to *send* a test payment with no external wallet process.
//! Built on `monero-wallet` (the actively-maintained successor to
//! "monero-serai": a real CLSAG + Bulletproofs+ implementation over
//! `curve25519-dalek`, used in production by the Serai project) talking directly
//! to the daemon's plain RPC via `monero-daemon-rpc`.
//!
//! Two callers share this module: `tests/e2e_stagenet.rs` (via `tests/support/mod.rs`,
//! a thin re-export) and `mock-woocommerce`'s own real stagenet connect-flow test
//! (WBS 1.4.5, `mock-woocommerce/tests/e2e_stagenet_connect_flow.rs`), which needs
//! it as a genuine library dependency since it lives in a different crate
//! entirely and cannot reach a `tests/`-local module. Gated behind the `e2e`
//! Cargo feature - see the dependency comments in `Cargo.toml` - so a normal
//! build never links this in.
//!
//! The RPC transport (`ReqwestTransport` below) is this crate's own `reqwest`
//! client wired up to `monero-daemon-rpc`'s single-method `HttpTransport` trait,
//! rather than the `monero-simple-request-rpc` crate an earlier version used -
//! that pulled in a second, entirely unused TLS stack
//! (`simple-request`/`hyper-rustls`/`ring`) just to reach a plain `http://` node,
//! and its conflicting rustls "ring" vs. this crate's own reqwest "aws-lc-rs"
//! provider feature was the actual reason a `CryptoProvider` had to be installed
//! manually. Implementing the one method directly removes both problems.
//!
//! The only thing borrowed from moneropay_core's own (separately tested)
//! `RpcDaemonClient` is `locate_transaction` - mapping a known txid to the block
//! height it landed in. Everything downstream (scanning that block into a
//! spendable `WalletOutput`, selecting real decoys, building and signing the
//! transaction, and broadcasting it) goes through monero-wallet, which already
//! implements it correctly - there is no reason to duplicate the scanning math
//! moneropay's own `key_custody` module does for a completely different
//! key-management library's types.
//!
//! RNG note: every place this module needs randomness (decoy selection, the
//! transaction's `outgoing_view_key` seed, CLSAG signing nonces via
//! `SignableTransaction::sign`) uses `rand_core::OsRng` - the standard
//! `getrandom`-backed CSPRNG, not a seeded/deterministic RNG. `monero-wallet`
//! itself pins `rand_core` 0.6 (one major version behind this crate's own
//! `rand = "0.10"`), which is why this file depends on `rand_core` directly rather
//! than reusing `rand`'s re-export - see the dependency comment in Cargo.toml.

use std::future::Future;
use std::time::Duration;

use monero_daemon_rpc::{prelude::*, HttpTransport, MoneroDaemon};
use monero_wallet::{
    address::{MoneroAddress, Network},
    ed25519::{CompressedPoint, Point, Scalar},
    ringct::RctType,
    send::{Change, SendError, SignableTransaction},
    transaction::Transaction,
    OutputWithDecoys, Scanner, ViewPair, WalletOutput,
};
use rand_core::{OsRng, RngCore};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::daemon::{MoneroDaemonClient, TxLocation};

/// The ring size required for the `ClsagBulletproofPlus` RCT type this module
/// always signs with - the standard type on every live Monero network today.
const RING_LEN: u8 = 16;

/// Monero requires this many confirmations on any output before it's spendable -
/// `CRYPTONOTE_DEFAULT_TX_SPENDABLE_AGE`, a real consensus rule no amount of
/// output-splitting can route around.
const SPENDABLE_AGE: u64 = 10;

/// `monero-daemon-rpc`'s `HttpTransport` over this crate's own `reqwest::Client`,
/// so callers need only one HTTP/TLS stack, not two - see the module doc above.
#[derive(Clone)]
struct ReqwestTransport {
    client: reqwest::Client,
    base_url: String,
}

impl HttpTransport for ReqwestTransport {
    fn post(
        &self,
        route: &str,
        body: Vec<u8>,
        response_size_limit: Option<usize>,
    ) -> impl Send + Future<Output = Result<Vec<u8>, InterfaceError>> {
        let request = self
            .client
            .post(format!("{}/{route}", self.base_url))
            .header("content-type", "application/json")
            .body(body);
        async move {
            let response = request
                .send()
                .await
                .map_err(|e| InterfaceError::InterfaceError(format!("request failed: {e}")))?;
            // Best-effort cap via the advertised length, not a streamed hard limit -
            // this trusts "a real public node", not an adversarial one, and
            // `monero-daemon-rpc`'s own `rpc_call_core` already truncates the
            // buffer it's handed regardless; this just avoids buffering an enormous
            // body first when the server is honest about its size upfront.
            if let (Some(limit), Some(len)) = (response_size_limit, response.content_length()) {
                if len > limit as u64 {
                    return Err(InterfaceError::InterfaceError(format!(
                        "response claimed {len} bytes, exceeding the {limit}-byte limit for {route}"
                    )));
                }
            }
            let bytes = response
                .bytes()
                .await
                .map_err(|e| InterfaceError::InterfaceError(format!("{e}")))?;
            Ok(bytes.to_vec())
        }
    }
}

/// See `StagenetSpendWallet::balance`.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct WalletBalance {
    pub spendable_piconero: u64,
    pub spendable_outputs: usize,
    pub pending_piconero: u64,
    pub pending_outputs: usize,
}

pub struct StagenetSpendWallet {
    view_pair: ViewPair,
    spend_key: Zeroizing<Scalar>,
    address: MoneroAddress,
    rpc: MoneroDaemon<ReqwestTransport>,
}

#[derive(Debug, thiserror::Error)]
pub enum SpendWalletError {
    #[error("cannot reach the stagenet node at {url}: {source}")]
    DaemonUnreachable { url: String, source: InterfaceError },
    #[error(
        "insufficient funds: this test payment needs {needed} piconero, but only {available} \
         piconero of spendable output(s) were found across the wallet's known_txids (an output \
         needs 10 confirmations, ~20 min on stagenet, before it's spendable - if a prior run's \
         change is still that young, this is expected; wait and retry).\n\
         Otherwise, fund the wallet from the stagenet faucet:\n\
         1. open https://stagenet-faucet.xmr-tw.org/\n\
         2. send to: {address}\n\
         Then add the faucet's txid to `customer.known_txids` in e2e/stagenet-wallets.json."
    )]
    InsufficientFunds {
        needed: u64,
        available: u64,
        address: String,
    },
    #[error("failed to build/sign the transaction: {0}")]
    Send(#[from] SendError),
    #[error("failed to broadcast the transaction: {0}")]
    Broadcast(#[source] PublishTransactionError),
    #[error("daemon RPC call failed: {0}")]
    Rpc(String),
}

fn hex32(hex_str: &str) -> [u8; 32] {
    let bytes = hex::decode(hex_str).expect("invalid hex in stagenet-wallets.json key material");
    bytes
        .try_into()
        .expect("key material must be exactly 32 bytes")
}

fn scalar_from_hex(hex_str: &str) -> Zeroizing<Scalar> {
    Zeroizing::new(
        Scalar::read(&mut &hex32(hex_str)[..])
            .expect("stagenet-wallets.json private key isn't a canonical ed25519 scalar"),
    )
}

/// Recomputes the key image an output would have if spent by `spend_key`, using
/// the exact same derivation `SignableTransaction::sign` uses internally - so we
/// can ask the daemon whether this specific output is already spent *before*
/// attempting to build a transaction with it, rather than only finding out from a
/// failed broadcast.
fn key_image(spend_key: &Scalar, output: &WalletOutput) -> CompressedPoint {
    // `input_key_dalek` is the actual one-time private key for this specific
    // output (spend key + its key offset) - as secret as the wallet's spend key
    // itself, so it's `Zeroizing`-wrapped the same way `SignableTransaction::sign`
    // wraps the equivalent value internally, rather than left as a bare `Scalar`
    // that lingers unscrubbed in memory after this function returns.
    let spend_key_dalek: Zeroizing<curve25519_dalek::Scalar> = Zeroizing::new((*spend_key).into());
    let key_offset_dalek: curve25519_dalek::Scalar = output.key_offset().into();
    let input_key_dalek: Zeroizing<curve25519_dalek::Scalar> =
        Zeroizing::new(*spend_key_dalek + key_offset_dalek);
    // Mirrors the `WrongPrivateKey` guard `SignableTransaction::sign` runs before
    // using the equivalent value (`send/mod.rs`'s `input_key * G == input.key()`) -
    // cheap, and turns a key mismatch into a clear panic right here instead of a
    // wrong-but-plausible key image that gets reported "unspent" and only fails,
    // confusingly, much later after paying for decoy selection.
    debug_assert_eq!(
        Point::from(&*input_key_dalek * curve25519_dalek::constants::ED25519_BASEPOINT_TABLE),
        output.key(),
        "computed one-time private key doesn't match this output's public key - spend key mismatch"
    );
    let hashed_point: Point = Point::biased_hash(output.key().compress().to_bytes());
    let hashed_point_dalek: curve25519_dalek::EdwardsPoint = hashed_point.into();
    let key_image_point: curve25519_dalek::EdwardsPoint = *input_key_dalek * hashed_point_dalek;
    Point::from(key_image_point).compress()
}

impl StagenetSpendWallet {
    /// Connects to `node_url` and derives keys from the given hex-encoded private
    /// spend/view keys, asserting the derived legacy address matches
    /// `expected_address` - a self-check against a transcription error in
    /// `stagenet-wallets.json`, since a wrong key here would otherwise fail silently
    /// distant from its actual cause (e.g. as "no spendable outputs found").
    pub async fn connect(
        node_url: &str,
        accept_invalid_certs: bool,
        private_spend_key_hex: &str,
        private_view_key_hex: &str,
        expected_address: &str,
    ) -> Result<Self, SpendWalletError> {
        let spend_key = scalar_from_hex(private_spend_key_hex);
        let view_key = scalar_from_hex(private_view_key_hex);
        let spend_key_dalek: Zeroizing<curve25519_dalek::Scalar> =
            Zeroizing::new((*spend_key).into());
        let public_spend =
            Point::from(&*spend_key_dalek * curve25519_dalek::constants::ED25519_BASEPOINT_TABLE);
        let view_pair = ViewPair::new(public_spend, view_key)
            .expect("torsioned spend key in stagenet-wallets.json");
        let address = view_pair.legacy_address(Network::Stagenet);
        assert_eq!(
            address.to_string(),
            expected_address,
            "derived address doesn't match stagenet-wallets.json's expected address - \
             private_spend_key/private_view_key don't match that address"
        );

        // Real decoy selection in `send`/`send_to` below is genuinely slow against a
        // public node: `get_output_distribution` alone returns a ~1MB payload
        // (observed 5-14s), and Monero's gamma-distribution age sampling (tuned for
        // mainnet's output density) needs more resample rounds - each its own
        // `get_outs` round trip - to find enough *unlocked* decoys on stagenet's
        // comparatively sparse RingCT output set. An empirically observed full run
        // took ~250s; 600s leaves margin without masking a genuinely dead node.
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(accept_invalid_certs)
            .timeout(Duration::from_secs(600))
            .build()
            .expect("failed to build reqwest client");
        let transport = ReqwestTransport {
            client,
            base_url: node_url.trim_end_matches('/').to_string(),
        };
        let rpc = MoneroDaemon::new(transport).await.map_err(|source| {
            SpendWalletError::DaemonUnreachable {
                url: node_url.to_string(),
                source,
            }
        })?;

        Ok(Self {
            view_pair,
            spend_key,
            address,
            rpc,
        })
    }

    pub fn address(&self) -> String {
        self.address.to_string()
    }

    /// Locates every txid in `known_txids` (skipping any not yet confirmed - e.g. a
    /// change output from a run still in the mempool) and scans each *distinct*
    /// block they land in, returning every output found belonging to this wallet
    /// alongside the height it landed at (needed to check the spendable-age rule
    /// below).
    ///
    /// Scanning by block height, deduplicated, rather than once per txid, matters
    /// for correctness and not just efficiency: `Scanner::scan` returns every
    /// matching output in the whole block, not just the ones belonging to the txid
    /// that was looked up. Two `known_txids` landing in the same block (the two
    /// original faucet payouts easily could; a busy run's own txs, less so) would
    /// otherwise scan that block twice and push every one of its outputs twice -
    /// `SignableTransaction::new` rejects a duplicate input key outright
    /// (`SendError::InvalidInputs`), so this would surface as a hard, confusing
    /// failure rather than silently double-spending anything.
    async fn known_outputs(
        &self,
        legacy_locate: &dyn MoneroDaemonClient,
        known_txids: &[String],
    ) -> Result<Vec<(u64, WalletOutput)>, SpendWalletError> {
        let mut heights = std::collections::BTreeSet::new();
        for txid in known_txids {
            let location = legacy_locate
                .locate_transaction(txid)
                .await
                .map_err(|e| SpendWalletError::Rpc(e.to_string()))?;
            if let TxLocation::InBlock(height) = location {
                heights.insert(height);
            }
        }
        let mut scanner = Scanner::new(self.view_pair.clone());
        let mut outputs = vec![];
        for height in heights {
            let block = self
                .rpc
                .block_by_number(height as usize)
                .await
                .map_err(|e| SpendWalletError::Rpc(e.to_string()))?;
            let scannable = self
                .rpc
                .expand_to_scannable_block(block)
                .await
                .map_err(|e| SpendWalletError::Rpc(e.to_string()))?;
            let found = scanner
                .scan(scannable)
                .map_err(|e| SpendWalletError::Rpc(e.to_string()))?
                .not_additionally_locked();
            outputs.extend(found.into_iter().map(|o| (height, o)));
        }
        Ok(outputs)
    }

    /// Splits `candidates` into (old enough to spend, still too young) by the
    /// `SPENDABLE_AGE` rule - a *separate* rule from the `additional_timelock`
    /// field `not_additionally_locked` already strips in `known_outputs` above,
    /// and one a real chain won't let us skip regardless of what the transaction
    /// itself claims.
    fn partition_by_age(
        candidates: Vec<(u64, WalletOutput)>,
        latest_height: u64,
    ) -> (Vec<WalletOutput>, Vec<WalletOutput>) {
        type AgedOutputs = Vec<(u64, WalletOutput)>;
        let (old_enough, too_young): (AgedOutputs, AgedOutputs) = candidates
            .into_iter()
            .partition(|(height, _)| latest_height.saturating_sub(*height) >= SPENDABLE_AGE);
        (
            old_enough.into_iter().map(|(_, o)| o).collect(),
            too_young.into_iter().map(|(_, o)| o).collect(),
        )
    }

    /// Filters `old_enough` down to outputs not yet spent, per the daemon's
    /// key-image index - what lets a later run (or a later `send`/`make_change`
    /// call) correctly notice an earlier one's change output as real, spendable
    /// balance without needing separate tracking.
    async fn filter_unspent(
        &self,
        legacy_locate: &dyn MoneroDaemonClient,
        old_enough: Vec<WalletOutput>,
    ) -> Result<Vec<WalletOutput>, SpendWalletError> {
        if old_enough.is_empty() {
            return Ok(old_enough);
        }
        let key_images: Vec<String> = old_enough
            .iter()
            .map(|o| hex::encode(key_image(&self.spend_key, o).to_bytes()))
            .collect();
        let statuses = legacy_locate
            .is_key_image_spent(&key_images)
            .await
            .map_err(|e| SpendWalletError::Rpc(e.to_string()))?;
        Ok(old_enough
            .into_iter()
            .zip(statuses)
            .filter(|(_, status)| *status == crate::daemon::KeyImageStatus::Unspent)
            .map(|(o, _)| o)
            .collect())
    }

    async fn spendable_now(
        &self,
        legacy_locate: &dyn MoneroDaemonClient,
        latest_height: u64,
        candidates: Vec<(u64, WalletOutput)>,
    ) -> Result<Vec<WalletOutput>, SpendWalletError> {
        let (old_enough, _too_young) = Self::partition_by_age(candidates, latest_height);
        self.filter_unspent(legacy_locate, old_enough).await
    }

    /// A UI-friendly balance summary: what's actually spendable right now
    /// (`send`/`make_change` will succeed up to this much), versus what's been
    /// received but is still within the `SPENDABLE_AGE` confirmation window.
    pub async fn balance(
        &self,
        legacy_locate: &dyn MoneroDaemonClient,
        known_txids: &[String],
    ) -> Result<WalletBalance, SpendWalletError> {
        let latest_height = self
            .rpc
            .latest_block_number()
            .await
            .map_err(|e| SpendWalletError::Rpc(e.to_string()))? as u64;
        let candidates = self.known_outputs(legacy_locate, known_txids).await?;
        let (old_enough, too_young) = Self::partition_by_age(candidates, latest_height);
        let pending_piconero = too_young.iter().map(|o| o.commitment().amount).sum();
        let pending_outputs = too_young.len();
        let spendable = self.filter_unspent(legacy_locate, old_enough).await?;
        Ok(WalletBalance {
            spendable_piconero: spendable.iter().map(|o| o.commitment().amount).sum(),
            spendable_outputs: spendable.len(),
            pending_piconero,
            pending_outputs,
        })
    }

    /// Builds, signs, and broadcasts a real transaction sending `amount` piconero to
    /// `to`, spending from `known_txids`' outputs (see `known_outputs`/`spendable_now`
    /// above), with change returned to this same wallet. Returns the new
    /// transaction's hash on success.
    pub async fn send(
        &self,
        legacy_locate: &dyn MoneroDaemonClient,
        known_txids: &[String],
        to: &str,
        amount: u64,
    ) -> Result<[u8; 32], SpendWalletError> {
        let to =
            MoneroAddress::from_str(Network::Stagenet, to).expect("invalid destination address");
        self.send_to(legacy_locate, known_txids, vec![(to, amount)])
            .await
    }

    /// Splits every currently spendable output into `splits` equal-sized new
    /// outputs, all paid back to this same wallet in one transaction - so a later
    /// `send`/`make_change` call has several independent outputs to choose from
    /// instead of one. Not used by WBS 1.4.5's own test, kept for parity with the
    /// dev-tooling this module was ported from and future e2e-tooling callers.
    #[allow(dead_code)]
    pub async fn make_change(
        &self,
        legacy_locate: &dyn MoneroDaemonClient,
        known_txids: &[String],
        splits: u32,
    ) -> Result<[u8; 32], SpendWalletError> {
        let balance = self.balance(legacy_locate, known_txids).await?;
        let per_output = balance.spendable_piconero / (splits as u64 + 1);
        if per_output == 0 {
            return Err(SpendWalletError::InsufficientFunds {
                needed: splits as u64,
                available: balance.spendable_piconero,
                address: self.address(),
            });
        }
        let destinations: Vec<(MoneroAddress, u64)> =
            std::iter::repeat_n((self.address, per_output), splits as usize).collect();
        self.send_to(legacy_locate, known_txids, destinations).await
    }

    /// Shared build/sign/broadcast core for `send` and `make_change`: selects
    /// spendable outputs, builds a transaction paying every `(address, amount)` in
    /// `destinations`, and broadcasts it. `destinations` may repeat the same
    /// address (as `make_change` does) - each still becomes an independent
    /// one-time output on-chain.
    async fn send_to(
        &self,
        legacy_locate: &dyn MoneroDaemonClient,
        known_txids: &[String],
        destinations: Vec<(MoneroAddress, u64)>,
    ) -> Result<[u8; 32], SpendWalletError> {
        let total_amount: u64 = destinations.iter().map(|(_, amount)| amount).sum();

        let latest_height = self
            .rpc
            .latest_block_number()
            .await
            .map_err(|e| SpendWalletError::Rpc(e.to_string()))? as u64;
        let candidates = self.known_outputs(legacy_locate, known_txids).await?;
        let mut spendable = self
            .spendable_now(legacy_locate, latest_height, candidates)
            .await?;
        // Largest-first: `OutputWithDecoys::new` triggers a real, comparatively slow
        // `get_output_distribution` RPC call (a ~1MB payload against a public node)
        // per output converted - most payments here are covered by a single
        // existing output, so selecting greedily and stopping as soon as a build
        // succeeds (see the retry loop below) keeps the common case to one such call
        // instead of paying that cost for every known output regardless of need.
        spendable.sort_unstable_by_key(|o| std::cmp::Reverse(o.commitment().amount));

        // One block of lag margin for decoy selection, not the tip itself: a
        // node's `latest_block_number()` and the `get_outs` calls `OutputWithDecoys`
        // makes against decoy indices near the tip can land on different backends of
        // a pooled public endpoint - `src/scanner.rs`'s own seed-height margin exists
        // for exactly this "genuine replication lag" reason. Without it, an
        // occasional decoy request errors as "being requested from blocks this node
        // doesn't have". The spendable-age check above deliberately keeps using the
        // real, un-shifted `latest_height` - a stricter (not laxer) age requirement.
        let decoy_block_number = (latest_height.saturating_sub(1)) as usize;
        // A real bound, not `u64::MAX`: `ProvidesFeeRates::fee_rate`'s `max_per_weight`
        // exists specifically so a malicious or misbehaving node can't hand back an
        // absurd rate and have it silently accepted (see the doc on
        // `ProvidesUnvalidatedFeeRates::fee_rate`: "may be manipulated to unsafe
        // levels and MUST be sanity checked") - passing `u64::MAX` opts out of that
        // check entirely. Real stagenet fee rates observed here are ~20_000
        // piconero/weight-unit; 1_000_000 is generous headroom for genuine fee-market
        // spikes while still rejecting anything absurd.
        const MAX_FEE_PER_WEIGHT: u64 = 1_000_000;
        let fee_rate = self
            .rpc
            .fee_rate(
                monero_wallet::interface::FeePriority::Unimportant,
                MAX_FEE_PER_WEIGHT,
            )
            .await
            .map_err(|e| SpendWalletError::Rpc(e.to_string()))?;

        let mut inputs = Vec::new();
        let mut last_necessary_fee: Option<u64> = None;
        let mut remaining = spendable.into_iter();
        let signable = loop {
            let Some(output) = remaining.next() else {
                // Ran out of spendable outputs without ever building successfully.
                // `needed` includes the last attempt's fee estimate when we have one,
                // so e.g. "needed 335000000, available 335000000" (a reader's first
                // guess: "but that's exactly enough!") doesn't happen - the fee on top
                // is exactly why it wasn't.
                return Err(SpendWalletError::InsufficientFunds {
                    needed: total_amount + last_necessary_fee.unwrap_or(0),
                    available: inputs
                        .iter()
                        .map(|i: &OutputWithDecoys| i.commitment().amount)
                        .sum(),
                    address: self.address(),
                });
            };
            inputs.push(
                OutputWithDecoys::new(&mut OsRng, &self.rpc, RING_LEN, decoy_block_number, output)
                    .await
                    .map_err(|e| SpendWalletError::Rpc(e.to_string()))?,
            );

            let mut outgoing_view_key = Zeroizing::new([0u8; 32]);
            OsRng.fill_bytes(outgoing_view_key.as_mut());
            match SignableTransaction::new(
                RctType::ClsagBulletproofPlus,
                outgoing_view_key,
                inputs.clone(),
                destinations.clone(),
                Change::new(self.view_pair.clone(), None),
                vec![],
                fee_rate,
            ) {
                Ok(signable) => break signable,
                Err(SendError::NotEnoughFunds { necessary_fee, .. }) => {
                    last_necessary_fee = necessary_fee;
                    continue;
                }
                Err(SendError::NoInputs) => continue,
                Err(e) => return Err(e.into()),
            }
        };

        let tx: Transaction = signable.sign(&mut OsRng, &self.spend_key)?;
        let hash = tx.hash();
        self.rpc
            .publish_transaction(&tx)
            .await
            .map_err(SpendWalletError::Broadcast)?;
        Ok(hash)
    }
}

/// Name of a top-level wallet fixture entry in `e2e/stagenet-wallets.json` -
/// `"customer"` or `"merchant"`.
pub type WalletFixtureKey = &'static str;
pub const CUSTOMER: WalletFixtureKey = "customer";
pub const MERCHANT: WalletFixtureKey = "merchant";

/// A spendable wallet fixture read from `e2e/stagenet-wallets.json` - either
/// `customer` (an ordinary wallet, faucet-funded, that plays "the person paying
/// an invoice") or `merchant` (the moneropay tenant's own wallet - watch-only
/// *inside* moneropay, but this file additionally has its private spend key so
/// a caller here can independently verify/inspect it). Never given to moneropay
/// itself in the customer's case; see `e2e/README.md`.
pub struct WalletFixture {
    pub address: String,
    pub private_spend_key_hex: String,
    pub private_view_key_hex: String,
    pub known_txids: Vec<String>,
}

/// Reads `wallets_json_path` and extracts the fixture named `key`
/// (`CUSTOMER`/`MERCHANT`) - centralized here so callers (`tests/e2e_stagenet.rs`,
/// `mock-woocommerce`'s own real stagenet test) don't duplicate this parsing.
pub fn load_wallet_fixture(
    wallets_json_path: &str,
    key: WalletFixtureKey,
) -> Result<WalletFixture, String> {
    let contents = std::fs::read_to_string(wallets_json_path)
        .map_err(|e| format!("failed to read {wallets_json_path}: {e}"))?;
    let json: Value = serde_json::from_str(&contents)
        .map_err(|e| format!("failed to parse {wallets_json_path}: {e}"))?;
    let field = |name: &str| -> Result<String, String> {
        json[key][name]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("{key}.{name} missing in {wallets_json_path}"))
    };
    let known_txids = json[key]["known_txids"]
        .as_array()
        .ok_or_else(|| format!("{key}.known_txids missing in {wallets_json_path}"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| "known_txids entries must be strings".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WalletFixture {
        address: field("address")?,
        private_spend_key_hex: field("private_spend_key")?,
        private_view_key_hex: field("private_view_key")?,
        known_txids,
    })
}

/// Appends `tx_hash` to `<key>.known_txids` in `wallets_json_path` and writes the
/// file back, so a later run automatically finds this run's change output once
/// it's confirmed and unlocked - see the comment on `known_txids` in
/// `e2e/stagenet-wallets.json` and `StagenetSpendWallet::spendable_now` above.
///
/// Re-reads the file fresh (rather than reusing an earlier in-memory copy) and
/// writes via a temp-file-plus-rename rather than a direct truncating write, so
/// two overlapping runs can't clobber each other's appended txid, and a crash or
/// Ctrl-C mid-write can't leave this credentials-bearing file half-written.
pub fn record_known_txid(
    wallets_json_path: &str,
    key: WalletFixtureKey,
    tx_hash: &str,
) -> Result<(), String> {
    let mut wallets_json: Value = serde_json::from_str(
        &std::fs::read_to_string(wallets_json_path)
            .map_err(|e| format!("failed to read {wallets_json_path}: {e}"))?,
    )
    .map_err(|e| format!("failed to parse {wallets_json_path}: {e}"))?;
    let known = wallets_json[key]["known_txids"]
        .as_array_mut()
        .ok_or_else(|| format!("{key}.known_txids must be an array"))?;
    if !known.iter().any(|v| v.as_str() == Some(tx_hash)) {
        known.push(Value::String(tx_hash.to_string()));
    }
    let tmp_path = format!("{wallets_json_path}.tmp");
    std::fs::write(
        &tmp_path,
        serde_json::to_string_pretty(&wallets_json).unwrap() + "\n",
    )
    .map_err(|e| format!("failed to write {tmp_path}: {e}"))?;
    std::fs::rename(&tmp_path, wallets_json_path)
        .map_err(|e| format!("failed to move {tmp_path} into place over {wallets_json_path}: {e}"))
}
