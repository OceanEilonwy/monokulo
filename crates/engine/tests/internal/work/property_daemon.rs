//! Fail individual daemon operations without changing the scripted chain.
use crate::daemon::{
    fake::FakeDaemonClient, BlockOutline, ChainBlock, ChainHeader, ChainTip, DaemonError,
    FetchedTx, KeyImageStatus, MoneroDaemonClient, TxLocation,
};
use crate::exploration_rpc::Rpc;
use std::sync::atomic::{AtomicU16, Ordering};

pub(crate) struct ScriptedDaemon {
    inner: FakeDaemonClient,
    failures: AtomicU16,
}

// This scanner-only mask also has one response-shape fault, after its nine
// RPC bits. It is not a difficulty-header transport fault.
const TRUNCATED_SPENT: u16 = 1 << 9;

impl ScriptedDaemon {
    pub(crate) fn new() -> Self {
        Self {
            inner: FakeDaemonClient::new(),
            failures: AtomicU16::new(0),
        }
    }
    pub(crate) fn fail_calls(&self, mask: u16) {
        self.failures.store(mask, Ordering::Relaxed);
    }
    pub(crate) fn calls_healthy(&self) -> bool {
        self.failures.load(Ordering::Relaxed) == 0
    }
    fn check(&self, op: Rpc) -> Result<(), DaemonError> {
        if self.failures.load(Ordering::Relaxed) & op.bit() != 0 {
            Err(DaemonError::Request(format!(
                "generated failure of daemon operation {op:?}"
            )))
        } else {
            Ok(())
        }
    }
}
impl std::ops::Deref for ScriptedDaemon {
    type Target = FakeDaemonClient;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[async_trait::async_trait]
impl MoneroDaemonClient for ScriptedDaemon {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.check(Rpc::Tip)?;
        self.inner.get_height().await
    }
    async fn get_tip(&self) -> Result<ChainTip, DaemonError> {
        self.check(Rpc::Tip)?;
        self.inner.get_tip().await
    }
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        self.check(Rpc::Hash)?;
        self.inner.get_block_hash(height).await
    }
    async fn get_chain_blocks(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainBlock>, DaemonError> {
        self.check(Rpc::Blocks)?;
        self.inner.get_chain_blocks(start_height, count).await
    }
    async fn get_chain_headers(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainHeader>, DaemonError> {
        self.check(Rpc::Headers)?;
        self.inner.get_chain_headers(start_height, count).await
    }
    async fn get_block_outline(
        &self,
        height: u64,
        tx_count: Option<u64>,
    ) -> Result<BlockOutline, DaemonError> {
        self.check(Rpc::Outline)?;
        self.inner.get_block_outline(height, tx_count).await
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.check(Rpc::Pool)?;
        self.inner.get_mempool_txids().await
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<FetchedTx>, DaemonError> {
        self.check(Rpc::Transactions)?;
        self.inner.get_transactions_with_ids(txids).await
    }
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        self.check(Rpc::Location)?;
        self.inner.locate_transaction(txid).await
    }
    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        if self.failures.load(Ordering::Relaxed) & TRUNCATED_SPENT != 0 {
            return Ok(Vec::new());
        }
        self.check(Rpc::Spent)?;
        self.inner.is_key_image_spent(key_images).await
    }
}
