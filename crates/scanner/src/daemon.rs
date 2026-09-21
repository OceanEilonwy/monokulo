//! `MoneroDaemonClient`: the trait wrapping calls the chain scanner needs against
//! `monerod`, so reorg/double-spend logic (`src/scanner.rs`) can be tested against a
//! deterministic scripted fake instead of a live node. See `docs/DESIGN.md` §7.1.
//!
//! The real implementation (talking to `monerod` over `reqwest` + `rustls`) is not
//! part of this pass - only the trait, its types, and the test double are
//! implemented here, since the reorg/double-spend *logic* behind this boundary is
//! the part worth getting right and testing thoroughly before wiring up the RPC
//! plumbing on the other side of it.

use monero::Transaction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyImageStatus {
    Unspent,
    SpentInBlockchain,
    SpentInPool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxLocation {
    InBlock(u64),
    InPool,
    NotFound,
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("daemon request failed: {0}")]
    Request(String),
}

#[async_trait::async_trait]
pub trait MoneroDaemonClient: Send + Sync {
    async fn get_height(&self) -> Result<u64, DaemonError>;
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError>;
    async fn get_block_transactions(&self, height: u64) -> Result<Vec<Transaction>, DaemonError>;

    /// Fetches transactions for a contiguous range of up to `count` blocks
    /// starting at `start_height`, in as few daemon round trips as the
    /// implementor can manage. The result is in ascending height order
    /// starting at `start_height` (entry `i` is the transactions of block
    /// `start_height + i`) but MAY be shorter than `count` - a node that
    /// doesn't honor a batch-size hint, or a range that runs past what the
    /// node currently has, are both real possibilities a caller must handle
    /// by advancing by the returned length, not by `count`. Only an empty
    /// result for a genuinely available range signals a real problem.
    ///
    /// The default implementation is the always-correct fallback every test
    /// double gets for free, with no override required: one
    /// [`Self::get_block_transactions`] call per height, in order - no
    /// batching, but nothing new to get wrong either. `RpcDaemonClient`
    /// overrides this with monerod's own `get_blocks.bin`, a single real
    /// HTTP round trip per chunk instead of one per block - see its own doc
    /// comment. Added for `scanner::rescan_order`, which can walk tens of
    /// thousands of blocks in one job; the live scanner's own per-tick walk
    /// stays on `get_block_transactions` (it only ever advances by a handful
    /// of blocks a tick, where the fixed overhead of a second RPC call to
    /// resolve `get_block_hash` per height already dominates any batching
    /// win).
    async fn get_blocks_range(&self, start_height: u64, count: u64) -> Result<Vec<Vec<Transaction>>, DaemonError> {
        let mut out = Vec::new();
        for height in start_height..start_height.saturating_add(count) {
            out.push(self.get_block_transactions(height).await?);
        }
        Ok(out)
    }

    async fn get_mempool_transactions(&self) -> Result<Vec<Transaction>, DaemonError>;
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError>;
    async fn is_key_image_spent(&self, key_images: &[String]) -> Result<Vec<KeyImageStatus>, DaemonError>;

    /// A block's own declared timestamp (unix seconds) - the one primitive
    /// `find_height_at_or_before` below needs and no other caller in this
    /// codebase has ever needed before it (`docs/order_rescan_wbs.md`
    /// Phase 0). Required, not defaulted: fetching it is inherently
    /// backend-specific (a real RPC call for `RpcDaemonClient`, scripted
    /// state for any test double).
    async fn get_block_timestamp(&self, height: u64) -> Result<u64, DaemonError>;

    /// Finds the highest block height whose own timestamp is `<=`
    /// `target_timestamp` - "at or before." A default method, built purely
    /// from `get_height`/`get_block_timestamp` above (the same "one real
    /// method, every implementor gets this for free" shape
    /// `is_key_image_spent_corroborated` below already uses) - binary
    /// search over `[0, tip]`, `O(log n)` calls to `get_block_timestamp`.
    ///
    /// Monero block timestamps are **not** strictly monotonic (consensus
    /// only bounds drift via a median-time-past rule, it doesn't forbid a
    /// later block declaring an earlier timestamp than its immediate
    /// predecessor within that tolerance) - this is a best-effort search
    /// against an assumed-roughly-monotonic sequence, not an exact
    /// guarantee. A caller that needs a safety margin around that
    /// imprecision (`docs/order_rescan_wbs.md` Phase 1.1's own rescan
    /// primitive does) applies it on top of this result, not inside it -
    /// this method's only job is "the closest reasonable answer," not "a
    /// provably exact one."
    async fn find_height_at_or_before(&self, target_timestamp: u64) -> Result<u64, DaemonError> {
        let tip = self.get_height().await?;
        // Common case first (a rescan triggered "now" wants something close
        // to the tip): if the tip itself is already at or before the
        // target, it's the answer - no search needed.
        if self.get_block_timestamp(tip).await? <= target_timestamp {
            return Ok(tip);
        }
        let (mut lo, mut hi) = (0u64, tip);
        while lo < hi {
            // Upper-mid bias: this loop searches for the *rightmost* height
            // whose timestamp still satisfies `<= target_timestamp` - the
            // standard shape for that (lower-mid would loop forever
            // whenever `lo`/`hi` become adjacent and the predicate holds at
            // `hi`).
            let mid = lo + (hi - lo + 1) / 2;
            if self.get_block_timestamp(mid).await? <= target_timestamp {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        Ok(lo)
    }

    /// Like `is_key_image_spent`, but for a client that knows about more than one
    /// node (see `daemon_fallback::FallbackDaemonClient::is_key_image_spent_corroborated`)
    /// - corroborates the answer across all of them before ever affirming
    /// `SpentInBlockchain`, since a false positive here permanently voids a real
    /// payment (`docs/DESIGN.md` §7.7). The default here, inherited by every
    /// single-node client (`RpcDaemonClient`, every test double), simply delegates:
    /// with exactly one source of truth to consult, there is nothing to corroborate
    /// against, so behavior is unchanged from plain `is_key_image_spent`.
    async fn is_key_image_spent_corroborated(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.is_key_image_spent(key_images).await
    }
}

/// Test double for `MoneroDaemonClient`, scripted via a small timeline API. Lets
/// reorg/double-spend scenarios be constructed deterministically, without a live or
/// regtest `monerod` - see `docs/TESTING.md` §3 for why this matters (reorgs are
/// rare in production, so bugs here are exactly the kind that go unnoticed for a
/// long time otherwise).
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Clone)]
    struct FakeBlock {
        hash: String,
        txs: Vec<Transaction>,
        timestamp: u64,
    }

    /// A fixed, arbitrary epoch and block spacing for `FakeBlock`'s own
    /// default timestamp formula (`FAKE_GENESIS_TIMESTAMP + height *
    /// FAKE_BLOCK_TIME_SECS`) - deterministic and monotonic by
    /// construction, so any test that never calls `set_block_timestamp`
    /// still gets *some* well-defined, increasing value per height for
    /// free. Neither number needs to match anything real; `set_block_timestamp`
    /// exists specifically for a test that wants to script something else
    /// (including deliberately non-monotonic timestamps, to exercise
    /// `find_height_at_or_before`'s own tolerance for that).
    const FAKE_GENESIS_TIMESTAMP: u64 = 1_700_000_000;
    const FAKE_BLOCK_TIME_SECS: u64 = 120;

    fn default_fake_timestamp(height: u64) -> u64 {
        FAKE_GENESIS_TIMESTAMP + height * FAKE_BLOCK_TIME_SECS
    }

    #[derive(Default)]
    struct State {
        blocks: HashMap<u64, FakeBlock>,
        height: u64,
        /// When set, `get_height` reports this instead of `height` - models a
        /// public endpoint's height-reporting backend being temporarily ahead of
        /// whatever backend actually serves block data, an inconsistency observed
        /// live against a real public node (see `run_scan_tick`'s bootstrap
        /// branch). Deliberately independent of `height`, which `push_block` still
        /// uses for its own auto-increment - the two heights need to be able to
        /// disagree for the scenario this exists to model.
        height_override: Option<u64>,
        mempool: Vec<Transaction>,
        /// txid -> location, explicitly tracked rather than derived from `blocks`
        /// so a test can put a tx "in the pool" without it ever having been mined,
        /// or mark it fully gone (NotFound) after a reorg.
        tx_locations: HashMap<String, TxLocation>,
        key_image_status: HashMap<String, KeyImageStatus>,
    }

    #[derive(Default)]
    pub struct FakeDaemonClient {
        state: Mutex<State>,
        /// Defaults to `false` (see `#[derive(Default)]`) - `new()` immediately sets
        /// it `true`, so every existing test that never touches this stays online as
        /// before. Lets a test simulate this node going unreachable (for exercising
        /// `daemon_fallback::FallbackDaemonClient` failover) without a second,
        /// differently-implemented test double - see `set_online`.
        online: std::sync::atomic::AtomicBool,
    }

    fn txid_of(tx: &Transaction) -> String {
        // Must match production's txid computation exactly (see
        // `scanner::tx_id_hex`) - a test double using a different identity scheme
        // than the real scanner would silently make every `locate_transaction`
        // lookup miss, since the scanner records payments keyed by the real hash.
        use monero::cryptonote::hash::Hashable;
        hex::encode(tx.hash().to_bytes())
    }

    impl FakeDaemonClient {
        pub fn new() -> Self {
            let client = Self::default();
            client.online.store(true, std::sync::atomic::Ordering::Relaxed);
            client
        }

        /// Simulates this node going unreachable (`online = false`) or coming back
        /// (`online = true`). While offline, every `MoneroDaemonClient` method
        /// returns `Err` instead of consulting the scripted chain state, which is
        /// otherwise left completely untouched - flipping back online resumes
        /// exactly where the scripted chain was left, nothing lost or reset.
        pub fn set_online(&self, online: bool) {
            self.online.store(online, std::sync::atomic::Ordering::Relaxed);
        }

        fn require_online(&self) -> Result<(), DaemonError> {
            if self.online.load(std::sync::atomic::Ordering::Relaxed) {
                Ok(())
            } else {
                Err(DaemonError::Request("fake daemon is offline".to_string()))
            }
        }

        /// Mines a new block at the next height, containing `txs`. Each tx is
        /// recorded as `InBlock(height)`, matching what a real node would report.
        pub fn push_block(&self, hash: &str, txs: Vec<Transaction>) -> u64 {
            let mut state = self.state.lock().unwrap();
            let height = state.height + 1;
            for tx in &txs {
                state.tx_locations.insert(txid_of(tx), TxLocation::InBlock(height));
            }
            state.blocks.insert(height, FakeBlock { hash: hash.to_string(), txs, timestamp: default_fake_timestamp(height) });
            state.height = height;
            state.height_override = None; // a real block now backs this height - any prior lag is resolved
            height
        }

        /// Places a block at an explicit height, rather than at "one past the last
        /// one pushed". The only way to script a chain that includes height 0:
        /// `push_block` starts counting at 1, so the genesis-divergence branch of
        /// reorg handling (no common ancestor to re-anchor to) is otherwise
        /// unreachable from a test. Also useful for building a chain around a gap.
        pub fn seed_block_at(&self, height: u64, hash: &str, txs: Vec<Transaction>) {
            let mut state = self.state.lock().unwrap();
            for tx in &txs {
                state.tx_locations.insert(txid_of(tx), TxLocation::InBlock(height));
            }
            state.blocks.insert(height, FakeBlock { hash: hash.to_string(), txs, timestamp: default_fake_timestamp(height) });
            state.height = state.height.max(height);
        }

        /// Overrides a block's timestamp after the fact (the block must
        /// already exist - `push_block`/`seed_block_at` it first). Only
        /// tests exercising `find_height_at_or_before` need this; every
        /// other existing test gets a deterministic, monotonic default for
        /// free and never needs to call it.
        pub fn set_block_timestamp(&self, height: u64, timestamp: u64) {
            let mut state = self.state.lock().unwrap();
            if let Some(block) = state.blocks.get_mut(&height) {
                block.timestamp = timestamp;
            }
        }

        /// Makes `get_height` report one past the last real block, without that
        /// block actually existing - models a public endpoint's height-reporting
        /// backend being briefly ahead of its block-serving backend. See
        /// `height_override`'s doc comment.
        pub fn advance_height_without_a_block(&self) {
            let mut state = self.state.lock().unwrap();
            state.height_override = Some(state.height + 1);
        }

        /// Makes `get_height` report an arbitrary height, however far from the
        /// blocks this fake can actually serve. The generalisation of
        /// `advance_height_without_a_block`, for the case where the discrepancy is
        /// not a one-block lag but a daemon simply asserting something untrue - see
        /// `docs/DESIGN.md` §7.7 on what the scanner does and does not take on
        /// trust from its configured node.
        pub fn report_height(&self, height: u64) {
            self.state.lock().unwrap().height_override = Some(height);
        }

        pub fn set_mempool(&self, txs: Vec<Transaction>) {
            let mut state = self.state.lock().unwrap();
            for tx in &txs {
                state.tx_locations.entry(txid_of(tx)).or_insert(TxLocation::InPool);
            }
            state.mempool = txs;
        }

        /// Simulates a reorg: replaces every block from `from_height` to the
        /// current tip with `new_blocks` (each `(hash, txs)`), re-tagging every
        /// previously-known tx that isn't in one of the new blocks as `NotFound`
        /// (the caller then decides, via `set_key_image_status`, whether that's a
        /// "still propagating" or "proven double-spend" situation).
        pub fn reorg_from(&self, from_height: u64, new_blocks: Vec<(&str, Vec<Transaction>)>) {
            let mut state = self.state.lock().unwrap();
            let old_txids: Vec<String> = state
                .blocks
                .iter()
                .filter(|(h, _)| **h >= from_height)
                .flat_map(|(_, b)| b.txs.iter().map(txid_of))
                .collect();
            state.blocks.retain(|h, _| *h < from_height);

            let mut height = from_height - 1;
            let mut new_txids = Vec::new();
            for (hash, txs) in new_blocks {
                height += 1;
                for tx in &txs {
                    let id = txid_of(tx);
                    state.tx_locations.insert(id.clone(), TxLocation::InBlock(height));
                    new_txids.push(id);
                }
                state.blocks.insert(height, FakeBlock { hash: hash.to_string(), txs, timestamp: default_fake_timestamp(height) });
            }
            state.height = height;

            for txid in old_txids {
                if !new_txids.contains(&txid) {
                    state.tx_locations.insert(txid, TxLocation::NotFound);
                }
            }
        }

        pub fn set_key_image_status(&self, key_image_hex: &str, status: KeyImageStatus) {
            self.state.lock().unwrap().key_image_status.insert(key_image_hex.to_string(), status);
        }

        pub fn drop_from_mempool(&self, tx: &Transaction) {
            let mut state = self.state.lock().unwrap();
            state.mempool.retain(|t| txid_of(t) != txid_of(tx));
            state.tx_locations.insert(txid_of(tx), TxLocation::NotFound);
        }
    }

    #[async_trait::async_trait]
    impl MoneroDaemonClient for FakeDaemonClient {
        async fn get_height(&self) -> Result<u64, DaemonError> {
            self.require_online()?;
            let state = self.state.lock().unwrap();
            Ok(state.height_override.unwrap_or(state.height))
        }

        async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
            self.require_online()?;
            self.state
                .lock()
                .unwrap()
                .blocks
                .get(&height)
                .map(|b| b.hash.clone())
                .ok_or_else(|| DaemonError::Request(format!("no block at height {height}")))
        }

        async fn get_block_transactions(&self, height: u64) -> Result<Vec<Transaction>, DaemonError> {
            self.require_online()?;
            Ok(self
                .state
                .lock()
                .unwrap()
                .blocks
                .get(&height)
                .map(|b| b.txs.clone())
                .unwrap_or_default())
        }

        async fn get_mempool_transactions(&self) -> Result<Vec<Transaction>, DaemonError> {
            self.require_online()?;
            Ok(self.state.lock().unwrap().mempool.clone())
        }

        async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
            self.require_online()?;
            Ok(self
                .state
                .lock()
                .unwrap()
                .tx_locations
                .get(txid)
                .copied()
                .unwrap_or(TxLocation::NotFound))
        }

        async fn is_key_image_spent(&self, key_images: &[String]) -> Result<Vec<KeyImageStatus>, DaemonError> {
            self.require_online()?;
            let state = self.state.lock().unwrap();
            Ok(key_images
                .iter()
                .map(|ki| state.key_image_status.get(ki).copied().unwrap_or(KeyImageStatus::Unspent))
                .collect())
        }

        async fn get_block_timestamp(&self, height: u64) -> Result<u64, DaemonError> {
            self.require_online()?;
            self.state
                .lock()
                .unwrap()
                .blocks
                .get(&height)
                .map(|b| b.timestamp)
                .ok_or_else(|| DaemonError::Request(format!("no block at height {height}")))
        }
    }

    pub fn txid_hex(tx: &Transaction) -> String {
        txid_of(tx)
    }
}

/// Real tests for `find_height_at_or_before` - a default trait method (see
/// its own doc comment on `MoneroDaemonClient`), tested here directly
/// against `fake::FakeDaemonClient` rather than a live node - the same
/// hermetic, scripted-chain approach `src/scanner.rs`'s own reorg/double-
/// spend tests already use, and the primary coverage this method gets
/// (`daemon_rpc.rs`'s own `#[ignore]`d live-node tests are the secondary,
/// "does this actually work against real monerod" proof, not the main
/// suite).
#[cfg(test)]
mod tests {
    use super::fake::FakeDaemonClient;
    use super::*;

    #[tokio::test]
    async fn finds_the_block_whose_timestamp_exactly_matches_the_target() {
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]); // height 1, default timestamp
        daemon.push_block("h2", vec![]); // height 2, default timestamp
        daemon.push_block("h3", vec![]); // height 3, default timestamp
        let h2_ts = daemon.get_block_timestamp(2).await.unwrap();
        assert_eq!(daemon.find_height_at_or_before(h2_ts).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn finds_the_nearest_earlier_block_when_the_target_falls_between_two() {
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        daemon.push_block("h3", vec![]);
        let h2_ts = daemon.get_block_timestamp(2).await.unwrap();
        // One second after block 2's own timestamp, still well before block
        // 3's (blocks are `FAKE_BLOCK_TIME_SECS` = 120s apart by default) -
        // "at or before" must land on 2, not round up to 3.
        assert_eq!(daemon.find_height_at_or_before(h2_ts + 1).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn a_target_at_or_after_the_tips_own_timestamp_returns_the_tip() {
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        let tip_ts = daemon.get_block_timestamp(2).await.unwrap();
        assert_eq!(daemon.find_height_at_or_before(tip_ts).await.unwrap(), 2);
        // Comfortably in the future - exercises the early-return path
        // ("the tip itself is already at or before the target") explicitly,
        // not just the coincidence of picking exactly the tip's own
        // timestamp.
        assert_eq!(daemon.find_height_at_or_before(tip_ts + 1_000_000).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn a_target_before_every_block_returns_genesis() {
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        let genesis_ts = daemon.get_block_timestamp(0).await;
        // Height 0 was never pushed by this test (`push_block` starts
        // counting at 1) - a target before every real block must still
        // resolve cleanly to height 0, not error just because nothing was
        // ever explicitly seeded there.
        assert!(genesis_ts.is_err(), "sanity check: this test never seeded height 0 itself");
        assert_eq!(daemon.find_height_at_or_before(0).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_single_block_chain_returns_that_block_for_any_target_at_or_after_it() {
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        let ts = daemon.get_block_timestamp(1).await.unwrap();
        assert_eq!(daemon.find_height_at_or_before(ts).await.unwrap(), 1);
        assert_eq!(daemon.find_height_at_or_before(ts + 999).await.unwrap(), 1);
        // A target genuinely before this one real block's own timestamp -
        // "at or before" has nothing real to point to, same as the
        // multi-block `a_target_before_every_block_returns_genesis` case -
        // 0 (genesis) is the honest answer, not the one real block that
        // happens to exist here.
        assert_eq!(daemon.find_height_at_or_before(0).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn tolerates_non_monotonic_timestamps_without_panicking_or_erroring() {
        // Block timestamps are not strictly monotonic on the real chain
        // (see this method's own doc comment) - script exactly that here:
        // height 2 declares an *earlier* timestamp than height 1, the one
        // shape a naive "assume strictly increasing" search could loop
        // forever or panic on. The method's own contract is "best-effort,
        // not exact" - this proves it degrades to *some* real answer
        // instead of hanging or crashing, not a specific "correct" height
        // (there isn't a single unambiguous one once monotonicity breaks).
        let daemon = FakeDaemonClient::new();
        daemon.push_block("h1", vec![]);
        daemon.push_block("h2", vec![]);
        daemon.push_block("h3", vec![]);
        let h1_ts = daemon.get_block_timestamp(1).await.unwrap();
        daemon.set_block_timestamp(2, h1_ts - 10);
        let result = daemon.find_height_at_or_before(h1_ts).await;
        assert!(result.is_ok(), "must degrade to a real answer, not error, on non-monotonic input: {result:?}");
        let height = result.unwrap();
        assert!(height <= 3, "must still return a real height within the scripted chain, got {height}");
    }
}
