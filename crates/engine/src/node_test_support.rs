//! Adversarial RPC behavior shared by scanner and verifier tests, without parsing.
use crate::daemon::{
    fake::FakeDaemonClient, BlockOutline, ChainBlock, ChainHeader, ChainTip, DaemonError,
    DifficultyHeader, FetchedTx, KeyImageStatus, MoneroDaemonClient, TxLocation,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub(crate) use crate::exploration_rpc::Rpc;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CallCounts {
    pub(crate) attempted: usize,
    pub(crate) completed: usize,
    pub(crate) cancelled: usize,
}
#[derive(Default)]
struct Counter {
    attempted: AtomicUsize,
    completed: AtomicUsize,
    cancelled: AtomicUsize,
}
struct Attempt<'a> {
    counter: &'a Counter,
    completed: bool,
}
impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.counter.cancelled.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Behavior {
    pub(crate) failures: u16,
    pub(crate) hangs: u16,
    pub(crate) delay_ms: u16,
    pub(crate) spent: Option<Vec<KeyImageStatus>>,
    pub(crate) location: Option<TxLocation>,
    pub(crate) inflate: u64,
    pub(crate) omit_pool: bool,
    /// 1 truncated bodies, 2 duplicate bodies, 3 reordered bodies,
    /// 4 unrelated bodies, 5 no blocks, 6 wrong block parent,
    /// 7 wrong block height, 8 mismatched outline hash, 9 wrong outline height, 10 wrong outline parent, 11 wrong outline timestamp.
    pub(crate) corrupt: u8,
    /// Preserve daemon transaction IDs while dropping only prunable signatures.
    pub(crate) pruned: bool,
    pub(crate) blob: Option<Vec<u8>>,
}

pub(crate) struct AdversarialNode {
    pub(crate) fake: FakeDaemonClient,
    pub(crate) behavior: parking_lot::Mutex<Behavior>,
    calls: [Counter; 11],
}

impl AdversarialNode {
    pub(crate) fn new() -> Self {
        Self {
            fake: FakeDaemonClient::new(),
            behavior: parking_lot::Mutex::default(),
            calls: std::array::from_fn(|_| Counter::default()),
        }
    }
    pub(crate) fn counts(&self, op: Rpc) -> CallCounts {
        let c = &self.calls[op as usize];
        CallCounts {
            attempted: c.attempted.load(Ordering::Relaxed),
            completed: c.completed.load(Ordering::Relaxed),
            cancelled: c.cancelled.load(Ordering::Relaxed),
        }
    }
    async fn call<T>(
        &self,
        op: Rpc,
        call: impl AsyncFnOnce(Behavior) -> Result<T, DaemonError>,
    ) -> Result<T, DaemonError> {
        let counter = &self.calls[op as usize];
        counter.attempted.fetch_add(1, Ordering::Relaxed);
        let mut attempt = Attempt {
            counter,
            completed: false,
        };
        let result = async {
            let b = self.behavior.lock().clone();
            if b.hangs & op.bit() != 0 {
                std::future::pending::<()>().await;
            }
            if b.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(u64::from(b.delay_ms))).await;
            }
            if b.failures & op.bit() != 0 {
                return Err(DaemonError::Request(format!("scripted RPC {op:?} failure")));
            }
            call(b).await
        }
        .await;
        attempt.completed = true;
        counter.completed.fetch_add(1, Ordering::Relaxed);
        result
    }
}

#[async_trait::async_trait]
impl MoneroDaemonClient for AdversarialNode {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.call(Rpc::Tip, async |b| {
            Ok(self.fake.get_height().await?.saturating_add(b.inflate))
        })
        .await
    }
    async fn get_tip(&self) -> Result<ChainTip, DaemonError> {
        self.call(Rpc::Tip, async |b| {
            let mut tip = self.fake.get_tip().await?;
            if b.inflate > 0 {
                tip.height = tip.height.saturating_add(b.inflate);
                tip.hash = None;
            }
            Ok(tip)
        })
        .await
    }
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        self.call(Rpc::Hash, async |_b| self.fake.get_block_hash(height).await)
            .await
    }
    async fn get_chain_blocks(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainBlock>, DaemonError> {
        self.call(Rpc::Blocks, async |b| {
            let mut blocks = self.fake.get_chain_blocks(start_height, count).await?;
            match b.corrupt {
                5 => blocks.clear(),
                6 => {
                    if let Some(first) = blocks.first_mut() {
                        "false-parent".clone_into(&mut first.prev_hash);
                    }
                }
                7 => {
                    if let Some(first) = blocks.first_mut() {
                        first.height += 1;
                    }
                }
                _ => {}
            }
            Ok(blocks)
        })
        .await
    }
    async fn get_chain_headers(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainHeader>, DaemonError> {
        self.call(Rpc::Headers, async |_b| {
            self.fake.get_chain_headers(start_height, count).await
        })
        .await
    }
    async fn get_block_outline(
        &self,
        height: u64,
        tx_count: Option<u64>,
    ) -> Result<BlockOutline, DaemonError> {
        self.call(Rpc::Outline, async |b| {
            let mut outline = self.fake.get_block_outline(height, tx_count).await?;
            match b.corrupt {
                8 => "false-outline".clone_into(&mut outline.hash),
                9 => outline.height += 1,
                10 => "false-parent".clone_into(&mut outline.prev_hash),
                11 => outline.timestamp += 1,
                _ => {}
            }
            Ok(outline)
        })
        .await
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.call(Rpc::Pool, async |b| {
            if b.omit_pool {
                Ok(vec![])
            } else {
                self.fake.get_mempool_txids().await
            }
        })
        .await
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<FetchedTx>, DaemonError> {
        self.call(Rpc::Transactions, async |b| {
            let mut txs = self.fake.get_transactions_with_ids(txids).await?;
            match b.corrupt {
                1 => {
                    txs.pop();
                }
                2 => {
                    if let Some(first) = txs.first().cloned() {
                        txs.push(first);
                    }
                }
                3 => txs.reverse(),
                4 => {
                    for tx in &mut txs {
                        "unsolicited".clone_into(&mut tx.txid);
                    }
                }
                _ => {}
            }
            if b.pruned {
                for fetched in &mut txs {
                    if let Some(base) = &fetched.tx.rct_signatures.sig {
                        let mut blob = monero::consensus::encode::serialize(&fetched.tx.prefix);
                        blob.extend(monero::consensus::encode::serialize(base));
                        fetched.tx = shared::monero_tx::decode_pruned(&blob)
                            .map_err(|e| DaemonError::Request(e.to_string()))?;
                    }
                }
            }
            Ok(txs)
        })
        .await
    }
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        self.call(Rpc::Location, async |b| {
            if let Some(location) = b.location {
                Ok(location)
            } else {
                self.fake.locate_transaction(txid).await
            }
        })
        .await
    }
    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.call(Rpc::Spent, async |b| {
            if let Some(statuses) = b.spent {
                Ok(statuses)
            } else {
                self.fake.is_key_image_spent(key_images).await
            }
        })
        .await
    }
    async fn get_difficulty_headers(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<DifficultyHeader>, DaemonError> {
        self.call(Rpc::Difficulty, async |_b| {
            self.fake.get_difficulty_headers(start_height, count).await
        })
        .await
    }
    async fn get_block_blob(&self, height: u64) -> Result<Vec<u8>, DaemonError> {
        self.call(Rpc::Blob, async |b| match b.blob {
            Some(blob) => Ok(blob),
            None => self.fake.get_block_blob(height).await,
        })
        .await
    }
}
