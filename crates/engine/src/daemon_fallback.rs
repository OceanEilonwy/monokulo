//! `FallbackDaemonClient`: a `MoneroDaemonClient` that wraps an ordered list of other
//! `MoneroDaemonClient`s (typically one real `RpcDaemonClient` per configured
//! `[monero_node.<network>]` primary node plus its `fallbacks`) and fails over
//! between them, so a single flaky or down public node doesn't stop payment
//! detection on that network. See `docs/DESIGN.md` §7.1 and `config::MoneroNodeConfig::fallbacks`.
//!
//! ## Failover policy
//!
//! Every call tries the nodes in their configured order, skipping the ones
//! in cooldown after a failure (5 s after the first failure in a row,
//! doubling up to 5 minutes): a down primary is not paid for on every scan
//! tick, and once its cooldown ends it is tried again ahead of the
//! fallbacks, so a deployment never settles on a fallback for good - one
//! that answers but is thousands of blocks behind would otherwise hide new
//! confirmations until the operator noticed. If a node fails, the next in
//! order is tried, until one succeeds or every node has been tried once, in
//! which case the last error is returned. There is no separate health-check
//! loop or background probing - the next real call is the health check,
//! which keeps this simple and means it never reports a node "up" based on
//! stale information.
//!
//! A down node therefore costs at most one failed request's worth of
//! latency per call, and then nothing until its cooldown ends.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::daemon::{ChainBlock, ChainHeader, ChainTip, FetchedTx, PoolAnswer};
use parking_lot::Mutex;
use tokio::time::Instant;

use crate::daemon::{DaemonError, KeyImageStatus, MoneroDaemonClient, TxLocation};

/// One entry in a [`FallbackDaemonClient`]'s ordered node list - the real client plus
/// a human-readable label (e.g. `"host:port"`) purely for the log events
/// on failover, so an operator's logs say *which* configured node just went down or
/// recovered rather than an opaque index.
pub struct FallbackNode {
    pub label: String,
    pub client: std::sync::Arc<dyn MoneroDaemonClient>,
}

/// After a node fails it is skipped for a while (task 7.6), so a dead node
/// doesn't cost a full request timeout on every call: 5s after the first
/// failure in a row, doubling up to 5 minutes. A node in cooldown is still
/// tried when every node is in cooldown, so recovery is never blocked.
const FIRST_COOLDOWN: Duration = Duration::from_secs(5);
const MAX_COOLDOWN: Duration = Duration::from_secs(5 * 60);

/// Longest one call may take across all the nodes it tries, so a call with
/// every node dead fails in bounded time rather than one full request
/// timeout per node.
pub const CALL_DEADLINE: Duration = Duration::from_secs(30);
/// Longest one node gets within a call: the client's own request timeout
/// (`daemon_rpc::REQUEST_TIMEOUT`). A node is never cut off short of what
/// its own client would give it; the shrinking deadline limits how many
/// nodes a call gets to try instead.
const MAX_ATTEMPT: Duration = crate::daemon_rpc::REQUEST_TIMEOUT;
/// Added to a node's own timeout where a layer above waits on it, so the
/// node's own error (naming the node and the timeout) arrives first.
pub const DEADLINE_MARGIN: Duration = Duration::from_secs(1);

#[derive(Default)]
struct NodeHealth {
    failures_in_a_row: u32,
    cooldown_until: Option<Instant>,
}

pub struct FallbackDaemonClient {
    nodes: Vec<FallbackNode>,
    health: Vec<Mutex<NodeHealth>>,
    /// Index into `nodes` of whichever node most recently answered
    /// successfully, for the status page and the failover log. Relaxed
    /// ordering is enough: it is reported, not relied on.
    current: AtomicUsize,
}

impl FallbackDaemonClient {
    /// `nodes` must be non-empty (enforced by every real construction path: a
    /// `[monero_node.<network>]` table always has at least its primary node, even
    /// with zero `fallbacks`) - see `note_all_failed`'s doc comment for what happens
    /// if this invariant is ever violated anyway.
    pub fn new(nodes: Vec<FallbackNode>) -> Self {
        let health = nodes
            .iter()
            .map(|_| Mutex::new(NodeHealth::default()))
            .collect();
        Self {
            nodes,
            health,
            current: AtomicUsize::new(0),
        }
    }

    /// Every configured node, in priority order - for a caller that wants to
    /// report on (or query) each one individually rather than through this
    /// type's own failover behavior, e.g. an operator-facing status page
    /// showing every node's live height, not just whichever one currently
    /// happens to answer first.
    pub fn nodes(&self) -> &[FallbackNode] {
        &self.nodes
    }

    /// Index into [`Self::nodes`] of whichever node last answered a call.
    /// A snapshot, not a guarantee: another concurrent call can change it
    /// the instant after this returns.
    pub fn current_index(&self) -> usize {
        self.current.load(Ordering::Relaxed)
    }

    /// Whether node `idx` is in its post-failure cooldown right now.
    pub fn in_cooldown(&self, idx: usize) -> bool {
        self.health.get(idx).is_some_and(|h| {
            h.lock()
                .cooldown_until
                .is_some_and(|until| Instant::now() < until)
        })
    }

    /// The order to try nodes in for one call: the configured order, nodes
    /// out of cooldown first, then (so that recovery is never blocked) the
    /// ones in cooldown. The primary is first whenever it is not cooling
    /// down, so a fallback is never kept for good.
    fn attempt_order(&self) -> Vec<usize> {
        let (ready, cooling): (Vec<usize>, Vec<usize>) =
            (0..self.nodes.len()).partition(|&idx| !self.in_cooldown(idx));
        ready.into_iter().chain(cooling).collect()
    }

    fn note_success(&self, idx: usize) {
        *self.health[idx].lock() = NodeHealth::default();
        let previous = self.current.swap(idx, Ordering::Relaxed);
        if previous != idx {
            tracing::warn!(
                node = %self.nodes[idx].label,
                previous_node = %self.nodes[previous].label,
                "monero daemon fallback: now using node {idx} after node {previous}"
            );
        }
    }

    fn note_failure(&self, idx: usize, error: &DaemonError) {
        let cooldown = {
            let mut health = self.health[idx].lock();
            health.failures_in_a_row = health.failures_in_a_row.saturating_add(1);
            let cooldown = FIRST_COOLDOWN
                .saturating_mul(1 << health.failures_in_a_row.saturating_sub(1).min(16))
                .min(MAX_COOLDOWN);
            health.cooldown_until = Some(Instant::now() + cooldown);
            cooldown
        };
        shared::throttled!(
            format!("node-failed:{}", self.nodes[idx].label),
            warn,
            node = %self.nodes[idx].label,
            skipped_for = ?cooldown,
            error = %error,
            "monero daemon fallback: node {idx} failed, skipping it and trying the next"
        );
    }

    /// Only reachable if `nodes` was constructed empty, which every real call site
    /// avoids (see `new`'s doc comment) - kept as a clear error rather than a panic
    /// or an infinite loop, since a `MoneroDaemonClient` with no nodes configured at
    /// all is a real (if avoidable) misconfiguration, not a logic bug worth crashing
    /// the process over.
    fn note_all_failed() -> DaemonError {
        DaemonError::Request("no Monero daemon nodes configured".to_string())
    }

    /// A handle that sends every call to one node, for the length of one scan
    /// tick (task 7.6). Different nodes can be at different heights or on
    /// different forks, and one tick mixing their answers (a height from one,
    /// blocks from another) can reach wrong conclusions. The pinned node is
    /// the first configured one out of cooldown. If it fails,
    /// the call fails (the tick ends and retries next time, when another node
    /// is picked) and the failure counts towards its cooldown here.
    pub fn pin(&self) -> PinnedDaemon<'_> {
        let idx = self.attempt_order().first().copied().unwrap_or(0);
        PinnedDaemon { inner: self, idx }
    }

    async fn failover<'a, T, F, Fut>(&'a self, call: F) -> Result<T, DaemonError>
    where
        F: Fn(&'a dyn MoneroDaemonClient) -> Fut,
        Fut: std::future::Future<Output = Result<T, DaemonError>> + 'a,
    {
        self.failover_within(CALL_DEADLINE, |_| MAX_ATTEMPT, call)
            .await
    }

    /// [`Self::failover`] with a deadline of `total` for the whole call and
    /// `per_node(node)` for each attempt: a block request is given what each
    /// node's own link needs (docs/engine_scaling.md section 2).
    async fn failover_within<'a, T, F, Fut>(
        &'a self,
        total: Duration,
        per_node: impl Fn(&dyn MoneroDaemonClient) -> Duration,
        call: F,
    ) -> Result<T, DaemonError>
    where
        F: Fn(&'a dyn MoneroDaemonClient) -> Fut,
        Fut: std::future::Future<Output = Result<T, DaemonError>> + 'a,
    {
        let deadline = Instant::now() + total;
        let mut last_err = None;
        let order = self.attempt_order();
        for (tried, &idx) in order.iter().enumerate() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                last_err = Some(DaemonError::TimedOut(format!(
                    "no node answered within {total:?}"
                )));
                break;
            }
            // A node gets what its own client would give it, within what's
            // left of the call: one that hangs costs the nodes after it
            // their turn in this call, never the call its deadline.
            let _ = tried;
            let node = self.nodes[idx].client.as_ref();
            let this_attempt = remaining.min(per_node(node));
            let outcome = match tokio::time::timeout(this_attempt, call(node)).await {
                Ok(outcome) => outcome,
                Err(_) => Err(DaemonError::TimedOut(format!(
                    "no answer within {this_attempt:?}"
                ))),
            };
            match outcome {
                Ok(v) => {
                    self.note_success(idx);
                    return Ok(v);
                }
                Err(e) => {
                    self.note_failure(idx, &e);
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(Self::note_all_failed))
    }
}

/// One scan tick's view of a [`FallbackDaemonClient`]: every call goes to the
/// same node. See [`FallbackDaemonClient::pin`].
pub struct PinnedDaemon<'a> {
    inner: &'a FallbackDaemonClient,
    idx: usize,
}

impl PinnedDaemon<'_> {
    #[cfg(test)]
    fn node_index(&self) -> usize {
        self.idx
    }

    async fn one<'b, T, F, Fut>(&'b self, call: F) -> Result<T, DaemonError>
    where
        F: FnOnce(&'b dyn MoneroDaemonClient) -> Fut,
        Fut: std::future::Future<Output = Result<T, DaemonError>> + 'b,
    {
        self.one_within(CALL_DEADLINE, call).await
    }

    /// [`Self::one`] with a deadline of `deadline`.
    async fn one_within<'b, T, F, Fut>(
        &'b self,
        deadline: Duration,
        call: F,
    ) -> Result<T, DaemonError>
    where
        F: FnOnce(&'b dyn MoneroDaemonClient) -> Fut,
        Fut: std::future::Future<Output = Result<T, DaemonError>> + 'b,
    {
        let Some(node) = self.inner.nodes.get(self.idx) else {
            return Err(FallbackDaemonClient::note_all_failed());
        };
        let outcome = match tokio::time::timeout(deadline, call(node.client.as_ref())).await {
            Ok(outcome) => outcome,
            Err(_) => Err(DaemonError::TimedOut(format!(
                "no answer within the call's {deadline:?} deadline"
            ))),
        };
        match &outcome {
            Ok(_) => self.inner.note_success(self.idx),
            Err(e) => self.inner.note_failure(self.idx, e),
        }
        outcome
    }
}

/// About how large a block's outline of `tx_count` transactions is: each id
/// comes three times over in the answer.
fn outline_bytes(tx_count: Option<u64>) -> u64 {
    tx_count.unwrap_or(0).saturating_mul(256)
}

#[async_trait::async_trait]
impl MoneroDaemonClient for PinnedDaemon<'_> {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.one(|c| c.get_height()).await
    }
    async fn get_tip(&self) -> Result<ChainTip, DaemonError> {
        self.one(|c| c.get_tip()).await
    }
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        self.one(|c| c.get_block_hash(height)).await
    }
    async fn get_chain_blocks(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainBlock>, DaemonError> {
        let deadline = self.chain_blocks_timeout(count);
        self.one_within(deadline, |c| c.get_chain_blocks(start_height, count))
            .await
    }

    fn link(&self) -> Option<crate::link::LinkSnapshot> {
        self.inner.nodes.get(self.idx)?.client.link()
    }

    fn transfer_rate(&self) -> Option<f64> {
        self.inner.nodes.get(self.idx)?.client.transfer_rate()
    }

    /// The pinned node's own timeout, with a moment over it so the node's
    /// own error (which names it) wins.
    fn chain_blocks_timeout(&self, count: u64) -> Duration {
        self.inner
            .nodes
            .get(self.idx)
            .map_or(CALL_DEADLINE, |node| {
                (node.client.chain_blocks_timeout(count) + DEADLINE_MARGIN).max(CALL_DEADLINE)
            })
    }
    async fn get_chain_headers(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainHeader>, DaemonError> {
        self.one(|c| c.get_chain_headers(start_height, count)).await
    }

    /// The pinned node's own, as [`Self::chain_blocks_timeout`].
    fn transfer_timeout(&self, bytes: u64) -> Duration {
        self.inner
            .nodes
            .get(self.idx)
            .map_or(CALL_DEADLINE, |node| {
                (node.client.transfer_timeout(bytes) + DEADLINE_MARGIN).max(CALL_DEADLINE)
            })
    }

    async fn get_block_outline(
        &self,
        height: u64,
        tx_count: Option<u64>,
    ) -> Result<crate::daemon::BlockOutline, DaemonError> {
        let deadline = self.transfer_timeout(outline_bytes(tx_count));
        self.one_within(deadline, |c| c.get_block_outline(height, tx_count))
            .await
    }
    /// The pinned node's two answers. It is judged by the tip's.
    async fn get_tip_and_mempool(&self) -> (Result<ChainTip, DaemonError>, PoolAnswer) {
        let both = self
            .one(|c| async move {
                let (tip, pool) = c.get_tip_and_mempool().await;
                tip.map(|tip| (tip, pool))
            })
            .await;
        match both {
            Ok((tip, pool)) => (Ok(tip), pool),
            Err(error) => {
                let pool = Err(DaemonError::Request(error.to_string()));
                (Err(error), pool)
            }
        }
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.one(|c| c.get_mempool_txids()).await
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<FetchedTx>, DaemonError> {
        self.one(|c| c.get_transactions_with_ids(txids)).await
    }
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        self.one(|c| c.locate_transaction(txid)).await
    }
    async fn locate_transactions(
        &self,
        txids: &[String],
    ) -> Result<HashMap<String, TxLocation>, DaemonError> {
        self.one(|c| c.locate_transactions(txids)).await
    }
    /// Deliberately every node, not just the pinned one: see
    /// `FallbackDaemonClient::locate_transaction_corroborated`.
    async fn locate_transaction_corroborated(
        &self,
        txid: &str,
    ) -> Result<Option<TxLocation>, DaemonError> {
        self.inner.locate_transaction_corroborated(txid).await
    }
    async fn find_transaction(
        &self,
        txid: &str,
    ) -> Result<Option<(FetchedTx, TxLocation)>, DaemonError> {
        self.one(|c| c.find_transaction(txid)).await
    }
    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.one(|c| c.is_key_image_spent(key_images)).await
    }
    /// Deliberately every node, not just the pinned one: see
    /// `FallbackDaemonClient::is_key_image_spent_corroborated`.
    async fn is_key_image_spent_corroborated(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.inner.is_key_image_spent_corroborated(key_images).await
    }
}

#[async_trait::async_trait]
impl MoneroDaemonClient for FallbackDaemonClient {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.failover(|c| c.get_height()).await
    }

    async fn get_tip(&self) -> Result<ChainTip, DaemonError> {
        self.failover(|c| c.get_tip()).await
    }

    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        self.failover(|c| c.get_block_hash(height)).await
    }

    /// One node answers for the whole range: a block's contents and id never
    /// come from two nodes.
    async fn get_chain_blocks(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainBlock>, DaemonError> {
        self.failover_within(
            self.chain_blocks_timeout(count),
            |node| node.chain_blocks_timeout(count) + DEADLINE_MARGIN,
            |c| c.get_chain_blocks(start_height, count),
        )
        .await
    }

    /// The node a call would try first.
    fn link(&self) -> Option<crate::link::LinkSnapshot> {
        let first = *self.attempt_order().first()?;
        self.nodes[first].client.link()
    }

    fn transfer_rate(&self) -> Option<f64> {
        let first = *self.attempt_order().first()?;
        self.nodes[first].client.transfer_rate()
    }

    /// Room for the first two nodes in order to try, each with what its own
    /// link needs: a primary that times out leaves its fallback a turn.
    fn chain_blocks_timeout(&self, count: u64) -> Duration {
        let total: Duration = self
            .attempt_order()
            .into_iter()
            .take(2)
            .map(|idx| self.nodes[idx].client.chain_blocks_timeout(count) + DEADLINE_MARGIN)
            .sum();
        total.max(CALL_DEADLINE)
    }

    async fn get_chain_headers(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainHeader>, DaemonError> {
        self.failover(|c| c.get_chain_headers(start_height, count))
            .await
    }

    /// Room for the first two nodes, as [`Self::chain_blocks_timeout`].
    fn transfer_timeout(&self, bytes: u64) -> Duration {
        let total: Duration = self
            .attempt_order()
            .into_iter()
            .take(2)
            .map(|idx| self.nodes[idx].client.transfer_timeout(bytes) + DEADLINE_MARGIN)
            .sum();
        total.max(CALL_DEADLINE)
    }

    /// From the first node that answers, each given what its link needs
    /// for an outline of `tx_count` transactions.
    async fn get_block_outline(
        &self,
        height: u64,
        tx_count: Option<u64>,
    ) -> Result<crate::daemon::BlockOutline, DaemonError> {
        let bytes = outline_bytes(tx_count);
        self.failover_within(
            self.transfer_timeout(bytes),
            |node| node.transfer_timeout(bytes) + DEADLINE_MARGIN,
            |c| c.get_block_outline(height, tx_count),
        )
        .await
    }

    /// One node answers both where it can: the first that gives its tip.
    /// A pool that node couldn't give (or every node's tip failing) is
    /// asked for on its own, from whichever node answers.
    async fn get_tip_and_mempool(&self) -> (Result<ChainTip, DaemonError>, PoolAnswer) {
        let both = self
            .failover(|c| async move {
                let (tip, pool) = c.get_tip_and_mempool().await;
                tip.map(|tip| (tip, pool))
            })
            .await;
        match both {
            Ok((tip, Ok(pool))) => (Ok(tip), Ok(pool)),
            Ok((tip, Err(_))) => (Ok(tip), self.get_mempool_txids().await),
            Err(error) => (Err(error), self.get_mempool_txids().await),
        }
    }

    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.failover(|c| c.get_mempool_txids()).await
    }

    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<FetchedTx>, DaemonError> {
        self.failover(|c| c.get_transactions_with_ids(txids)).await
    }

    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        self.failover(|c| c.locate_transaction(txid)).await
    }

    async fn locate_transactions(
        &self,
        txids: &[String],
    ) -> Result<HashMap<String, TxLocation>, DaemonError> {
        self.failover(|c| c.locate_transactions(txids)).await
    }

    async fn find_transaction(
        &self,
        txid: &str,
    ) -> Result<Option<(FetchedTx, TxLocation)>, DaemonError> {
        self.failover(|c| c.find_transaction(txid)).await
    }

    /// Every node at once, like `is_key_image_spent_corroborated`, and for
    /// the same reason: `NotFound` is what a real payment gets voided on. A
    /// node that places the transaction wins over any number that don't
    /// (a block over the pool): a node can lag, be on a losing fork, or
    /// lie, but a transaction it can show is one that exists. `NotFound`
    /// only when every node that answered says so; an error when none did.
    /// With one node there is no second opinion (`None`), as for any
    /// single-node client.
    async fn locate_transaction_corroborated(
        &self,
        txid: &str,
    ) -> Result<Option<TxLocation>, DaemonError> {
        if self.nodes.len() < 2 {
            return Ok(None);
        }
        let answers = futures_util::future::join_all(
            self.nodes
                .iter()
                .map(|node| node.client.locate_transaction(txid)),
        )
        .await;
        let mut best: Option<TxLocation> = None;
        for (node, answer) in self.nodes.iter().zip(answers) {
            match answer {
                Ok(location) => {
                    let rank = |location: &TxLocation| match location {
                        TxLocation::InBlock(_) => 2,
                        TxLocation::InPool => 1,
                        TxLocation::NotFound => 0,
                    };
                    if best.is_none_or(|current| rank(&location) > rank(&current)) {
                        best = Some(location);
                    }
                }
                Err(e) => tracing::warn!(
                    node = %node.label,
                    error = %e,
                    "monero daemon fallback: node unreachable while locating a transaction, excluded from \
                     corroboration"
                ),
            }
        }
        best.map(Some).ok_or_else(|| {
            DaemonError::Request("no nodes reachable to locate a transaction".to_string())
        })
    }

    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.failover(|c| c.is_key_image_spent(key_images)).await
    }

    /// Polls *every* configured node - not just the currently "sticky" `current`
    /// one every other method here uses - since this is the one call in the whole
    /// client where a single wrong or malicious node's answer has a real,
    /// permanent consequence: `SpentInBlockchain` is what a real payment gets
    /// voided on (`engine::void_if_double_spend_proven`), and unlike a block hash
    /// (checked against the scanner's own recorded history) or a locate-transaction
    /// result (only ever trusted for its *presence*, never its absence), nothing
    /// else in the system can cross-check this answer at all - see
    /// `docs/DESIGN.md` §7.7's "Fallback nodes widen this trust boundary".
    ///
    /// Affirms `SpentInBlockchain` only when every node that answered agrees on it.
    /// A single node's answer (because it is the only one configured, or every
    /// other one failed to respond this time) is trusted as-is - there is nothing
    /// to corroborate it against, exactly like a bare single-node deployment
    /// always has been. Genuine disagreement among two or more nodes is not
    /// resolved by majority vote: refusing to affirm a double-spend is the safe
    /// direction to be wrong in (a missed double-spend is merely re-checked again
    /// next time; a false one can retract a real payment and notify the merchant).
    /// Disagreement is
    /// returned as `Disputed`, so revalidation cannot mistake it for affirmative
    /// unspent evidence. It is also logged, since one configured node is either
    /// lying or badly wrong about something checkable - worth an operator's
    /// attention regardless of which way this particular call resolves.
    ///
    /// No performance concern in practice: `is_key_image_spent` (corroborated or
    /// not) is only ever called from the rare "a payment's transaction has gone
    /// missing" path, never from the hot per-tick scanning loop - polling every
    /// node here costs nothing that matters.
    async fn is_key_image_spent_corroborated(
        &self,
        key_images: &[String],
    ) -> std::result::Result<Vec<KeyImageStatus>, DaemonError> {
        // Every node at once: the answer waits for the slowest node, not
        // for each in turn.
        let answers = futures_util::future::join_all(
            self.nodes
                .iter()
                .map(|node| node.client.is_key_image_spent(key_images)),
        )
        .await;
        let mut responses: Vec<Vec<KeyImageStatus>> = Vec::new();
        for (node, answer) in self.nodes.iter().zip(answers) {
            match answer {
                Ok(statuses) if statuses.len() == key_images.len() => responses.push(statuses),
                Ok(wrong_length) => tracing::warn!(
                    node = %node.label,
                    statuses = wrong_length.len(),
                    key_images = key_images.len(),
                    "monero daemon fallback: node returned the wrong number of key-image statuses - excluded from \
                     corroboration"
                ),
                Err(e) => tracing::warn!(
                    node = %node.label,
                    error = %e,
                    "monero daemon fallback: node unreachable during key-image corroboration, excluded from the vote"
                ),
            }
        }
        if responses.is_empty() {
            return Err(DaemonError::Request(
                "no nodes reachable to check key image status".to_string(),
            ));
        }

        let mut result = Vec::with_capacity(key_images.len());
        for i in 0..key_images.len() {
            let votes: Vec<KeyImageStatus> = responses.iter().map(|r| r[i]).collect();
            let status = if votes.iter().all(|vote| *vote == votes[0]) {
                votes[0]
            } else {
                tracing::warn!(
                    key_image = key_images.get(i).map(String::as_str).unwrap_or("?"),
                    "monero daemon fallback: nodes disagree on a key image - treating the status as disputed until \
                     they agree"
                );
                KeyImageStatus::Disputed
            };
            result.push(status);
        }
        Ok(result)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    /// A `MoneroDaemonClient` whose every method can be toggled between succeeding
    /// (with a fixed, arbitrary-but-distinguishable value) and failing, plus a call
    /// counter - enough to prove failover order, stickiness, and recovery without
    /// needing `daemon::fake::FakeDaemonClient`'s much heavier scripted-chain API
    /// (which has no notion of a node being "down" at all).
    struct FlakyDaemonClient {
        healthy: AtomicBool,
        calls: AtomicUsize,
    }

    impl FlakyDaemonClient {
        fn new(healthy: bool) -> Self {
            Self {
                healthy: AtomicBool::new(healthy),
                calls: AtomicUsize::new(0),
            }
        }

        fn set_healthy(&self, healthy: bool) {
            self.healthy.store(healthy, Ordering::Relaxed);
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    #[async_trait::async_trait]
    impl MoneroDaemonClient for FlakyDaemonClient {
        async fn get_height(&self) -> Result<u64, DaemonError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.healthy.load(Ordering::Relaxed) {
                Ok(1)
            } else {
                Err(DaemonError::Request("flaky node is down".to_string()))
            }
        }

        async fn get_block_hash(&self, _height: u64) -> Result<String, DaemonError> {
            unimplemented!("not exercised by these tests")
        }

        async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
            unimplemented!("not exercised by these tests")
        }

        async fn get_chain_blocks(
            &self,
            _start_height: u64,
            _count: u64,
        ) -> Result<Vec<ChainBlock>, DaemonError> {
            unimplemented!("not exercised by these tests")
        }

        async fn get_transactions_with_ids(
            &self,
            _txids: &[String],
        ) -> Result<Vec<FetchedTx>, DaemonError> {
            unimplemented!("not exercised by these tests")
        }

        async fn locate_transaction(&self, _txid: &str) -> Result<TxLocation, DaemonError> {
            unimplemented!("not exercised by these tests")
        }

        async fn is_key_image_spent(
            &self,
            _key_images: &[String],
        ) -> Result<Vec<KeyImageStatus>, DaemonError> {
            unimplemented!("not exercised by these tests")
        }
    }

    fn node(label: &str, client: Arc<FlakyDaemonClient>) -> (FallbackNode, Arc<FlakyDaemonClient>) {
        (
            FallbackNode {
                label: label.to_string(),
                client: client.clone(),
            },
            client,
        )
    }

    #[tokio::test]
    async fn a_healthy_primary_is_always_used_and_the_fallback_is_never_called() {
        let (primary_node, primary) = node("primary", Arc::new(FlakyDaemonClient::new(true)));
        let (fallback_node, fallback) = node("fallback", Arc::new(FlakyDaemonClient::new(true)));
        let client = FallbackDaemonClient::new(vec![primary_node, fallback_node]);

        for _ in 0..3 {
            assert_eq!(client.get_height().await.unwrap(), 1);
        }
        assert_eq!(primary.call_count(), 3);
        assert_eq!(fallback.call_count(), 0);
    }

    #[tokio::test]
    async fn a_down_primary_fails_over_to_the_fallback_within_the_same_call() {
        let (primary_node, primary) = node("primary", Arc::new(FlakyDaemonClient::new(false)));
        let (fallback_node, fallback) = node("fallback", Arc::new(FlakyDaemonClient::new(true)));
        let client = FallbackDaemonClient::new(vec![primary_node, fallback_node]);

        let result = client.get_height().await;
        assert_eq!(result.unwrap(), 1);
        assert_eq!(primary.call_count(), 1);
        assert_eq!(fallback.call_count(), 1);
    }

    #[tokio::test]
    async fn once_failed_over_the_client_becomes_sticky_to_the_working_node() {
        let (primary_node, primary) = node("primary", Arc::new(FlakyDaemonClient::new(false)));
        let (fallback_node, fallback) = node("fallback", Arc::new(FlakyDaemonClient::new(true)));
        let client = FallbackDaemonClient::new(vec![primary_node, fallback_node]);

        client.get_height().await.unwrap();
        assert_eq!(primary.call_count(), 1);
        assert_eq!(fallback.call_count(), 1);

        // A second call must not re-try the still-down primary first - it should go
        // straight to the fallback that already proved healthy.
        client.get_height().await.unwrap();
        assert_eq!(
            primary.call_count(),
            1,
            "the down primary should not be retried once a fallback is sticky"
        );
        assert_eq!(fallback.call_count(), 2);
    }

    #[tokio::test]
    async fn recovery_of_an_earlier_node_is_picked_back_up_once_the_current_one_fails() {
        let (primary_node, primary) = node("primary", Arc::new(FlakyDaemonClient::new(false)));
        let (fallback_node, fallback) = node("fallback", Arc::new(FlakyDaemonClient::new(true)));
        let client = FallbackDaemonClient::new(vec![primary_node, fallback_node]);

        client.get_height().await.unwrap();

        // The primary comes back; the fallback then goes down. The client should
        // wrap back around to the now-healthy primary rather than erroring out.
        primary.set_healthy(true);
        fallback.set_healthy(false);
        let result = client.get_height().await;
        assert_eq!(result.unwrap(), 1);
        assert_eq!(primary.call_count(), 2);
    }

    #[tokio::test]
    async fn every_node_failing_returns_the_last_node_s_error_rather_than_panicking() {
        let (primary_node, _primary) = node("primary", Arc::new(FlakyDaemonClient::new(false)));
        let (fallback_node, _fallback) = node("fallback", Arc::new(FlakyDaemonClient::new(false)));
        let client = FallbackDaemonClient::new(vec![primary_node, fallback_node]);

        let err = client.get_height().await.unwrap_err();
        assert!(matches!(err, DaemonError::Request(_)));
    }

    #[tokio::test]
    async fn a_single_configured_node_with_no_fallbacks_behaves_like_a_bare_client() {
        let (only_node, only) = node("only", Arc::new(FlakyDaemonClient::new(true)));
        let client = FallbackDaemonClient::new(vec![only_node]);

        assert_eq!(client.get_height().await.unwrap(), 1);
        assert_eq!(only.call_count(), 1);
    }

    /// A `MoneroDaemonClient` whose `is_key_image_spent` returns a fixed, configured
    /// answer (or errors, if built via `unreachable()`) - for
    /// `is_key_image_spent_corroborated`'s policy tests, which need several nodes
    /// with independently controllable key-image answers rather than
    /// `FlakyDaemonClient`'s simpler healthy/unhealthy toggle.
    struct KeyImageDaemonClient {
        statuses: Vec<KeyImageStatus>,
        unreachable: bool,
        /// Where it places any transaction (`locate_transaction`).
        location: TxLocation,
    }

    impl KeyImageDaemonClient {
        fn answering(statuses: Vec<KeyImageStatus>) -> Self {
            Self {
                statuses,
                unreachable: false,
                location: TxLocation::NotFound,
            }
        }

        fn unreachable() -> Self {
            Self {
                statuses: vec![],
                unreachable: true,
                location: TxLocation::NotFound,
            }
        }

        fn placing(location: TxLocation) -> Self {
            Self {
                statuses: vec![],
                unreachable: false,
                location,
            }
        }
    }

    #[async_trait::async_trait]
    impl MoneroDaemonClient for KeyImageDaemonClient {
        async fn get_height(&self) -> Result<u64, DaemonError> {
            unimplemented!("not exercised by these tests")
        }
        async fn get_block_hash(&self, _height: u64) -> Result<String, DaemonError> {
            unimplemented!("not exercised by these tests")
        }
        async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
            unimplemented!("not exercised by these tests")
        }
        async fn get_chain_blocks(
            &self,
            _start_height: u64,
            _count: u64,
        ) -> Result<Vec<ChainBlock>, DaemonError> {
            unimplemented!("not exercised by these tests")
        }

        async fn get_transactions_with_ids(
            &self,
            _txids: &[String],
        ) -> Result<Vec<FetchedTx>, DaemonError> {
            unimplemented!("not exercised by these tests")
        }

        async fn locate_transaction(&self, _txid: &str) -> Result<TxLocation, DaemonError> {
            if self.unreachable {
                return Err(DaemonError::Request(
                    "key image daemon is unreachable".to_string(),
                ));
            }
            Ok(self.location)
        }
        async fn is_key_image_spent(
            &self,
            key_images: &[String],
        ) -> Result<Vec<KeyImageStatus>, DaemonError> {
            if self.unreachable {
                return Err(DaemonError::Request(
                    "key image daemon is unreachable".to_string(),
                ));
            }
            assert_eq!(
                key_images.len(),
                self.statuses.len(),
                "test misconfigured: statuses must match key_images length"
            );
            Ok(self.statuses.clone())
        }
    }

    fn ki_node(label: &str, client: KeyImageDaemonClient) -> FallbackNode {
        FallbackNode {
            label: label.to_string(),
            client: Arc::new(client),
        }
    }

    #[tokio::test]
    async fn a_transaction_is_nowhere_only_when_every_reachable_node_says_so() {
        // One node's "not found" voids a real payment on its own otherwise:
        // its own inputs are spent on every node (by it), so nothing else
        // corroborates the absence.
        let one_places_it = FallbackDaemonClient::new(vec![
            ki_node("a", KeyImageDaemonClient::placing(TxLocation::NotFound)),
            ki_node("b", KeyImageDaemonClient::placing(TxLocation::InBlock(7))),
            ki_node("c", KeyImageDaemonClient::placing(TxLocation::InPool)),
        ]);
        assert_eq!(
            one_places_it
                .locate_transaction_corroborated("tx")
                .await
                .unwrap(),
            Some(TxLocation::InBlock(7)),
            "a block over the pool, over nowhere"
        );
        let all_nowhere = FallbackDaemonClient::new(vec![
            ki_node("a", KeyImageDaemonClient::placing(TxLocation::NotFound)),
            ki_node("b", KeyImageDaemonClient::unreachable()),
            ki_node("c", KeyImageDaemonClient::placing(TxLocation::NotFound)),
        ]);
        assert_eq!(
            all_nowhere
                .locate_transaction_corroborated("tx")
                .await
                .unwrap(),
            Some(TxLocation::NotFound),
            "an unreachable node has no say"
        );
        let none_reachable = FallbackDaemonClient::new(vec![
            ki_node("a", KeyImageDaemonClient::unreachable()),
            ki_node("b", KeyImageDaemonClient::unreachable()),
        ]);
        assert!(none_reachable
            .locate_transaction_corroborated("tx")
            .await
            .is_err());
        let single = FallbackDaemonClient::new(vec![ki_node(
            "only",
            KeyImageDaemonClient::placing(TxLocation::NotFound),
        )]);
        assert_eq!(
            single.locate_transaction_corroborated("tx").await.unwrap(),
            None,
            "one node has no second opinion to give"
        );
        // The pinned handle asks every node too.
        let pinned = one_places_it.pin();
        assert_eq!(
            pinned.locate_transaction_corroborated("tx").await.unwrap(),
            Some(TxLocation::InBlock(7))
        );
    }

    #[tokio::test]
    async fn unanimous_spent_in_blockchain_across_every_node_is_affirmed() {
        let client = FallbackDaemonClient::new(vec![
            ki_node(
                "a",
                KeyImageDaemonClient::answering(vec![KeyImageStatus::SpentInBlockchain]),
            ),
            ki_node(
                "b",
                KeyImageDaemonClient::answering(vec![KeyImageStatus::SpentInBlockchain]),
            ),
        ]);
        let result = client
            .is_key_image_spent_corroborated(&["ki1".to_string()])
            .await
            .unwrap();
        assert_eq!(result, vec![KeyImageStatus::SpentInBlockchain]);
    }

    #[tokio::test]
    async fn disagreement_between_nodes_refuses_to_affirm_a_double_spend() {
        // Exactly the scenario a single lying/wrong node used to cause a wrongful,
        // permanent void for: one node claims spent, another (equally reachable and
        // equally configured) says otherwise. Corroboration must not just trust
        // whichever one happens to be asked.
        let client = FallbackDaemonClient::new(vec![
            ki_node(
                "a",
                KeyImageDaemonClient::answering(vec![KeyImageStatus::SpentInBlockchain]),
            ),
            ki_node(
                "b",
                KeyImageDaemonClient::answering(vec![KeyImageStatus::Unspent]),
            ),
        ]);
        let result = client
            .is_key_image_spent_corroborated(&["ki1".to_string()])
            .await
            .unwrap();
        assert_eq!(
            result,
            vec![KeyImageStatus::Disputed],
            "a disagreement must remain inconclusive"
        );
    }

    #[tokio::test]
    async fn pool_and_unspent_disagreement_is_also_inconclusive() {
        let client = FallbackDaemonClient::new(vec![
            ki_node(
                "a",
                KeyImageDaemonClient::answering(vec![KeyImageStatus::Unspent]),
            ),
            ki_node(
                "b",
                KeyImageDaemonClient::answering(vec![KeyImageStatus::SpentInPool]),
            ),
        ]);
        assert_eq!(
            client
                .is_key_image_spent_corroborated(&["ki1".to_string()])
                .await
                .unwrap(),
            vec![KeyImageStatus::Disputed]
        );
    }

    #[tokio::test]
    async fn a_single_configured_node_is_trusted_as_is_with_nothing_to_corroborate_against() {
        let client = FallbackDaemonClient::new(vec![ki_node(
            "only",
            KeyImageDaemonClient::answering(vec![KeyImageStatus::SpentInBlockchain]),
        )]);
        let result = client
            .is_key_image_spent_corroborated(&["ki1".to_string()])
            .await
            .unwrap();
        assert_eq!(
            result,
            vec![KeyImageStatus::SpentInBlockchain],
            "with only one node configured, corroboration is impossible - behavior must match plain is_key_image_spent"
        );
    }

    #[tokio::test]
    async fn only_one_node_reachable_this_call_is_also_trusted_as_is() {
        let client = FallbackDaemonClient::new(vec![
            ki_node("a", KeyImageDaemonClient::unreachable()),
            ki_node(
                "b",
                KeyImageDaemonClient::answering(vec![KeyImageStatus::SpentInBlockchain]),
            ),
        ]);
        let result = client
            .is_key_image_spent_corroborated(&["ki1".to_string()])
            .await
            .unwrap();
        assert_eq!(
            result,
            vec![KeyImageStatus::SpentInBlockchain],
            "one unreachable node must not block corroboration when a second node did answer"
        );
    }

    #[tokio::test]
    async fn every_node_unreachable_is_an_error_not_a_silent_unspent() {
        let client = FallbackDaemonClient::new(vec![
            ki_node("a", KeyImageDaemonClient::unreachable()),
            ki_node("b", KeyImageDaemonClient::unreachable()),
        ]);
        let err = client
            .is_key_image_spent_corroborated(&["ki1".to_string()])
            .await
            .unwrap_err();
        assert!(matches!(err, DaemonError::Request(_)));
    }

    #[tokio::test]
    async fn unanimous_agreement_on_unspent_is_reported_as_is() {
        let client = FallbackDaemonClient::new(vec![
            ki_node(
                "a",
                KeyImageDaemonClient::answering(vec![KeyImageStatus::Unspent]),
            ),
            ki_node(
                "b",
                KeyImageDaemonClient::answering(vec![KeyImageStatus::Unspent]),
            ),
        ]);
        let result = client
            .is_key_image_spent_corroborated(&["ki1".to_string()])
            .await
            .unwrap();
        assert_eq!(result, vec![KeyImageStatus::Unspent]);
    }

    #[tokio::test]
    async fn each_key_image_is_corroborated_independently_of_the_others() {
        // Two key images in one call: nodes agree on the first, disagree on the
        // second - the verdict for one must not leak into the other.
        let client = FallbackDaemonClient::new(vec![
            ki_node(
                "a",
                KeyImageDaemonClient::answering(vec![
                    KeyImageStatus::SpentInBlockchain,
                    KeyImageStatus::SpentInBlockchain,
                ]),
            ),
            ki_node(
                "b",
                KeyImageDaemonClient::answering(vec![
                    KeyImageStatus::SpentInBlockchain,
                    KeyImageStatus::Unspent,
                ]),
            ),
        ]);
        let result = client
            .is_key_image_spent_corroborated(&["ki1".to_string(), "ki2".to_string()])
            .await
            .unwrap();
        assert_eq!(
            result,
            vec![KeyImageStatus::SpentInBlockchain, KeyImageStatus::Disputed]
        );
    }

    // -- Cooldown, call deadline and per-tick pinning (task 7.6) --------------

    /// A node that never answers, the way a node behind a black-holed
    /// connection behaves.
    struct HangingDaemonClient;

    #[async_trait::async_trait]
    impl MoneroDaemonClient for HangingDaemonClient {
        async fn get_height(&self) -> Result<u64, DaemonError> {
            std::future::pending().await
        }
        async fn get_block_hash(&self, _height: u64) -> Result<String, DaemonError> {
            std::future::pending().await
        }
        async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
            std::future::pending().await
        }
        async fn get_chain_blocks(
            &self,
            _start_height: u64,
            _count: u64,
        ) -> Result<Vec<ChainBlock>, DaemonError> {
            std::future::pending().await
        }

        async fn get_transactions_with_ids(
            &self,
            _txids: &[String],
        ) -> Result<Vec<FetchedTx>, DaemonError> {
            std::future::pending().await
        }

        async fn locate_transaction(&self, _txid: &str) -> Result<TxLocation, DaemonError> {
            std::future::pending().await
        }
        async fn is_key_image_spent(
            &self,
            _key_images: &[String],
        ) -> Result<Vec<KeyImageStatus>, DaemonError> {
            std::future::pending().await
        }
    }

    /// A node whose block requests take `delay` and whose link
    /// measurements allow `timeout` for them; its block is named `label`.
    struct SlowLinkClient {
        label: &'static str,
        delay: Duration,
        timeout: Duration,
    }

    #[async_trait::async_trait]
    impl MoneroDaemonClient for SlowLinkClient {
        fn chain_blocks_timeout(&self, _count: u64) -> Duration {
            self.timeout
        }
        async fn get_height(&self) -> Result<u64, DaemonError> {
            std::future::pending().await
        }
        async fn get_block_hash(&self, _height: u64) -> Result<String, DaemonError> {
            std::future::pending().await
        }
        async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
            std::future::pending().await
        }
        async fn get_chain_blocks(
            &self,
            start_height: u64,
            _count: u64,
        ) -> Result<Vec<ChainBlock>, DaemonError> {
            tokio::time::sleep(self.delay).await;
            Ok(vec![ChainBlock {
                height: start_height,
                hash: self.label.to_string(),
                prev_hash: String::new(),
                timestamp: 0,
                txs: Vec::new(),
                txids: Vec::new(),
                wire_bytes: 0,
            }])
        }
        async fn get_transactions_with_ids(
            &self,
            _txids: &[String],
        ) -> Result<Vec<FetchedTx>, DaemonError> {
            std::future::pending().await
        }
        async fn locate_transaction(&self, _txid: &str) -> Result<TxLocation, DaemonError> {
            std::future::pending().await
        }
        async fn is_key_image_spent(
            &self,
            _key_images: &[String],
        ) -> Result<Vec<KeyImageStatus>, DaemonError> {
            std::future::pending().await
        }
    }

    fn slow_link(label: &'static str, delay_secs: u64, timeout_secs: u64) -> FallbackNode {
        FallbackNode {
            label: label.into(),
            client: Arc::new(SlowLinkClient {
                label,
                delay: Duration::from_secs(delay_secs),
                timeout: Duration::from_secs(timeout_secs),
            }),
        }
    }

    /// A block request gets what each node's link needs
    /// (docs/engine_scaling.md section 2): a slow primary that will deliver
    /// within its own timeout isn't cut off at the fixed 15 s, and a primary
    /// that hangs still leaves its fallback a turn.
    #[tokio::test(start_paused = true)]
    async fn a_block_request_gives_each_node_the_time_its_link_needs() {
        let client = FallbackDaemonClient::new(vec![
            slow_link("primary", 30, 40),
            slow_link("fallback", 0, 15),
        ]);
        let started = Instant::now();
        let blocks = client.get_chain_blocks(7, 1).await.unwrap();
        assert_eq!(
            blocks[0].hash, "primary",
            "past 15 s but within its own timeout"
        );
        assert_eq!(started.elapsed(), Duration::from_secs(30));

        let client = FallbackDaemonClient::new(vec![
            slow_link("primary", 3600, 40),
            slow_link("fallback", 5, 15),
        ]);
        assert_eq!(
            client.chain_blocks_timeout(1),
            Duration::from_secs(41 + 16),
            "room for both nodes' own timeouts"
        );
        let started = Instant::now();
        let blocks = client.get_chain_blocks(7, 1).await.unwrap();
        assert_eq!(blocks[0].hash, "fallback");
        assert_eq!(started.elapsed(), Duration::from_secs(41 + 5));
        assert_eq!(client.current_index(), 1, "the fallback answered");

        // Pinned to one node, a block request gets that node's own timeout.
        let client = FallbackDaemonClient::new(vec![slow_link("only", 50, 55)]);
        let pinned = client.pin();
        assert_eq!(pinned.get_chain_blocks(7, 1).await.unwrap()[0].hash, "only");
        assert_eq!(pinned.chain_blocks_timeout(1), Duration::from_secs(56));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_node_cools_down_for_longer_each_time_and_recovers() {
        let (a_node, a) = node("a", Arc::new(FlakyDaemonClient::new(false)));
        let (b_node, b) = node("b", Arc::new(FlakyDaemonClient::new(false)));
        let client = FallbackDaemonClient::new(vec![a_node, b_node]);

        assert!(client.get_height().await.is_err());
        assert!(client.in_cooldown(0) && client.in_cooldown(1));
        tokio::time::advance(FIRST_COOLDOWN + Duration::from_millis(1)).await;
        assert!(!client.in_cooldown(0), "the first cooldown is short");

        // A second failure in a row doubles it.
        assert!(client.get_height().await.is_err());
        tokio::time::advance(FIRST_COOLDOWN + Duration::from_millis(1)).await;
        assert!(
            client.in_cooldown(0),
            "still cooling down after a second failure"
        );
        tokio::time::advance(FIRST_COOLDOWN).await;
        assert!(!client.in_cooldown(0));

        // Once it answers, it's healthy again.
        a.set_healthy(true);
        b.set_healthy(true);
        assert_eq!(client.get_height().await.unwrap(), 1);
        assert!(!client.in_cooldown(0));
        let _ = b;
    }

    #[tokio::test(start_paused = true)]
    async fn a_node_in_cooldown_is_skipped_and_the_primary_is_back_first_after_it() {
        let (a_node, a) = node("a", Arc::new(FlakyDaemonClient::new(false)));
        let (b_node, b) = node("b", Arc::new(FlakyDaemonClient::new(false)));
        let (c_node, c) = node("c", Arc::new(FlakyDaemonClient::new(true)));
        let client = FallbackDaemonClient::new(vec![a_node, b_node, c_node]);
        // The primary and the first fallback die: the call moves on to c,
        // and both cool down.
        assert_eq!(client.get_height().await.unwrap(), 1);
        assert_eq!(client.current_index(), 2);
        assert_eq!((a.call_count(), b.call_count(), c.call_count()), (1, 1, 1));
        // While they cool down neither is tried: the ready node comes first
        // and answers.
        assert_eq!(client.get_height().await.unwrap(), 1);
        assert_eq!((a.call_count(), b.call_count(), c.call_count()), (1, 1, 2));
        // Once its cooldown ends the primary is tried first again, even
        // though c kept answering: a fallback is never kept for good (it
        // could be answering from far behind the chain).
        a.set_healthy(true);
        tokio::time::advance(FIRST_COOLDOWN + Duration::from_millis(1)).await;
        assert_eq!(client.get_height().await.unwrap(), 1);
        assert_eq!(client.current_index(), 0);
        assert_eq!((a.call_count(), b.call_count(), c.call_count()), (2, 1, 2));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hanging_node_gets_only_its_share_of_the_call_deadline() {
        let hanging = FallbackNode {
            label: "hanging".into(),
            client: Arc::new(HangingDaemonClient),
        };
        let (ok_node, ok) = node("ok", Arc::new(FlakyDaemonClient::new(true)));
        let client = FallbackDaemonClient::new(vec![hanging, ok_node]);

        let started = Instant::now();
        assert_eq!(
            client.get_height().await.unwrap(),
            1,
            "the second node still got its turn"
        );
        assert!(
            started.elapsed() <= MAX_ATTEMPT + Duration::from_millis(10),
            "the hanging node got its client's own timeout, no more: took {:?}",
            started.elapsed()
        );
        assert_eq!(ok.call_count(), 1);

        // With every node hanging, the call fails within the deadline.
        let all_hanging = FallbackDaemonClient::new(vec![
            FallbackNode {
                label: "h1".into(),
                client: Arc::new(HangingDaemonClient),
            },
            FallbackNode {
                label: "h2".into(),
                client: Arc::new(HangingDaemonClient),
            },
            FallbackNode {
                label: "h3".into(),
                client: Arc::new(HangingDaemonClient),
            },
        ]);
        let started = Instant::now();
        assert!(all_hanging.get_height().await.is_err());
        assert!(
            started.elapsed() <= CALL_DEADLINE + MAX_ATTEMPT,
            "took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_pinned_tick_uses_one_node_and_a_failure_moves_the_next_tick_on() {
        let (a_node, a) = node("a", Arc::new(FlakyDaemonClient::new(true)));
        let (b_node, b) = node("b", Arc::new(FlakyDaemonClient::new(true)));
        let client = FallbackDaemonClient::new(vec![a_node, b_node]);

        let pinned = client.pin();
        assert_eq!(pinned.node_index(), 0);
        for _ in 0..3 {
            pinned.get_height().await.unwrap();
        }
        assert_eq!(
            (a.call_count(), b.call_count()),
            (3, 0),
            "every call of the tick went to one node"
        );

        a.set_healthy(false);
        assert!(
            pinned.get_height().await.is_err(),
            "no failover inside a pinned tick"
        );
        assert_eq!(b.call_count(), 0);

        let next_tick = client.pin();
        assert_eq!(
            next_tick.node_index(),
            1,
            "the failed node is cooling down, so the next tick picks another"
        );
        next_tick.get_height().await.unwrap();
        assert_eq!(b.call_count(), 1);
    }
}
