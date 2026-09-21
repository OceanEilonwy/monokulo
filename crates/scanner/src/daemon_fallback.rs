//! `FallbackDaemonClient`: a `MoneroDaemonClient` that wraps an ordered list of other
//! `MoneroDaemonClient`s (typically one real `RpcDaemonClient` per configured
//! `[monero_node.<network>]` primary node plus its `fallbacks`) and fails over
//! between them, so a single flaky or down public node doesn't stop payment
//! detection on that network. See `docs/DESIGN.md` §7.1 and `config::MoneroNodeConfig::fallbacks`.
//!
//! ## Failover policy
//!
//! Every call starts at whichever node index last succeeded (`current`), not always
//! at index 0 - a live deployment whose primary node has gone down and stayed down
//! should not re-try it (and pay its connection-timeout cost) on every single scan
//! tick forever; it should settle onto whichever node is actually answering. If that
//! node fails, the next node in order is tried, wrapping around, until either one
//! succeeds (which becomes the new `current`) or every node has been tried once, in
//! which case the last error is returned. There is no separate health-check
//! loop or background probing - the next real call is the health check, which keeps
//! this simple and means it never reports a node "up" based on stale information.
//!
//! A single down node therefore costs at most one failed request's worth of latency
//! per call while it stays down (not a cascading retry storm), and recovery is
//! automatic: the moment the current node starts failing, the very next call moves
//! on, and a previously-failed node earlier in priority order is naturally retried
//! again once the chain of calls wraps back around to it.

use std::sync::atomic::{AtomicUsize, Ordering};

use monero::Transaction;

use crate::daemon::{DaemonError, KeyImageStatus, MoneroDaemonClient, TxLocation};

/// One entry in a [`FallbackDaemonClient`]'s ordered node list - the real client plus
/// a human-readable label (e.g. `"host:port"`) purely for the `eprintln!` diagnostics
/// on failover, so an operator's logs say *which* configured node just went down or
/// recovered rather than an opaque index.
pub struct FallbackNode {
    pub label: String,
    pub client: std::sync::Arc<dyn MoneroDaemonClient>,
}

pub struct FallbackDaemonClient {
    nodes: Vec<FallbackNode>,
    /// Index into `nodes` of whichever node most recently answered successfully -
    /// where the *next* call starts trying from. Relaxed ordering is enough: this is
    /// an optimization (skip nodes already known-bad) rather than a correctness
    /// requirement, since every call still tries every node in order if needed.
    current: AtomicUsize,
}

impl FallbackDaemonClient {
    /// `nodes` must be non-empty (enforced by every real construction path: a
    /// `[monero_node.<network>]` table always has at least its primary node, even
    /// with zero `fallbacks`) - see `note_all_failed`'s doc comment for what happens
    /// if this invariant is ever violated anyway.
    pub fn new(nodes: Vec<FallbackNode>) -> Self {
        Self { nodes, current: AtomicUsize::new(0) }
    }

    /// Every configured node, in priority order - for a caller that wants to
    /// report on (or query) each one individually rather than through this
    /// type's own failover behavior, e.g. an operator-facing status page
    /// showing every node's live height, not just whichever one currently
    /// happens to answer first.
    pub fn nodes(&self) -> &[FallbackNode] {
        &self.nodes
    }

    /// Index into [`Self::nodes`] of whichever node this client would try
    /// first on its *next* call right now - see the module doc comment's
    /// own "Failover policy" section. A snapshot, not a guarantee: another
    /// concurrent call can change it the instant after this returns, same
    /// as any other use of `current` in this type.
    pub fn current_index(&self) -> usize {
        self.current.load(Ordering::Relaxed)
    }

    fn note_success(&self, idx: usize) {
        let previous = self.current.swap(idx, Ordering::Relaxed);
        if previous != idx {
            eprintln!(
                "monero daemon fallback: now using node {idx} ({}) after node {previous} ({})",
                self.nodes[idx].label, self.nodes[previous].label
            );
        }
    }

    fn note_failure(&self, idx: usize, error: &DaemonError) {
        eprintln!("monero daemon fallback: node {idx} ({}) failed, trying next: {error}", self.nodes[idx].label);
    }

    /// Only reachable if `nodes` was constructed empty, which every real call site
    /// avoids (see `new`'s doc comment) - kept as a clear error rather than a panic
    /// or an infinite loop, since a `MoneroDaemonClient` with no nodes configured at
    /// all is a real (if avoidable) misconfiguration, not a logic bug worth crashing
    /// the process over.
    fn note_all_failed() -> DaemonError {
        DaemonError::Request("no Monero daemon nodes configured".to_string())
    }
}

#[async_trait::async_trait]
impl MoneroDaemonClient for FallbackDaemonClient {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        let start = self.current.load(Ordering::Relaxed);
        let mut last_err = None;
        for offset in 0..self.nodes.len() {
            let idx = (start + offset) % self.nodes.len();
            match self.nodes[idx].client.get_height().await {
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

    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        let start = self.current.load(Ordering::Relaxed);
        let mut last_err = None;
        for offset in 0..self.nodes.len() {
            let idx = (start + offset) % self.nodes.len();
            match self.nodes[idx].client.get_block_hash(height).await {
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

    async fn get_block_timestamp(&self, height: u64) -> Result<u64, DaemonError> {
        let start = self.current.load(Ordering::Relaxed);
        let mut last_err = None;
        for offset in 0..self.nodes.len() {
            let idx = (start + offset) % self.nodes.len();
            match self.nodes[idx].client.get_block_timestamp(height).await {
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

    async fn get_block_transactions(&self, height: u64) -> Result<Vec<Transaction>, DaemonError> {
        let start = self.current.load(Ordering::Relaxed);
        let mut last_err = None;
        for offset in 0..self.nodes.len() {
            let idx = (start + offset) % self.nodes.len();
            match self.nodes[idx].client.get_block_transactions(height).await {
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

    async fn get_blocks_range(&self, start_height: u64, count: u64) -> Result<Vec<Vec<Transaction>>, DaemonError> {
        let start = self.current.load(Ordering::Relaxed);
        let mut last_err = None;
        for offset in 0..self.nodes.len() {
            let idx = (start + offset) % self.nodes.len();
            match self.nodes[idx].client.get_blocks_range(start_height, count).await {
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

    async fn get_mempool_transactions(&self) -> Result<Vec<Transaction>, DaemonError> {
        let start = self.current.load(Ordering::Relaxed);
        let mut last_err = None;
        for offset in 0..self.nodes.len() {
            let idx = (start + offset) % self.nodes.len();
            match self.nodes[idx].client.get_mempool_transactions().await {
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

    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        let start = self.current.load(Ordering::Relaxed);
        let mut last_err = None;
        for offset in 0..self.nodes.len() {
            let idx = (start + offset) % self.nodes.len();
            match self.nodes[idx].client.locate_transaction(txid).await {
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

    async fn get_transaction(&self, txid: &str) -> Result<Transaction, DaemonError> {
        let start = self.current.load(Ordering::Relaxed);
        let mut last_err = None;
        for offset in 0..self.nodes.len() {
            let idx = (start + offset) % self.nodes.len();
            match self.nodes[idx].client.get_transaction(txid).await {
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

    async fn is_key_image_spent(&self, key_images: &[String]) -> Result<Vec<KeyImageStatus>, DaemonError> {
        let start = self.current.load(Ordering::Relaxed);
        let mut last_err = None;
        for offset in 0..self.nodes.len() {
            let idx = (start + offset) % self.nodes.len();
            match self.nodes[idx].client.is_key_image_spent(key_images).await {
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

    /// Polls *every* configured node - not just the currently "sticky" `current`
    /// one every other method here uses - since this is the one call in the whole
    /// client where a single wrong or malicious node's answer has a real,
    /// permanent consequence: `SpentInBlockchain` is what a real payment gets
    /// voided on (`scanner::void_if_double_spend_proven`), and unlike a block hash
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
    /// next time; a false one permanently voids real money), and the disagreement
    /// itself is logged, since it means one of the configured nodes is either
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
        let mut responses: Vec<Vec<KeyImageStatus>> = Vec::new();
        for node in &self.nodes {
            match node.client.is_key_image_spent(key_images).await {
                Ok(statuses) if statuses.len() == key_images.len() => responses.push(statuses),
                Ok(wrong_length) => eprintln!(
                    "monero daemon fallback: node {} returned {} key-image statuses for {} key images - excluded \
                     from corroboration",
                    node.label,
                    wrong_length.len(),
                    key_images.len()
                ),
                Err(e) => eprintln!(
                    "monero daemon fallback: node {} unreachable during key-image corroboration, excluded from \
                     the vote: {e}",
                    node.label
                ),
            }
        }
        if responses.is_empty() {
            return Err(DaemonError::Request("no nodes reachable to check key image status".to_string()));
        }

        let mut result = Vec::with_capacity(key_images.len());
        for i in 0..key_images.len() {
            let votes: Vec<KeyImageStatus> = responses.iter().map(|r| r[i]).collect();
            let status = if votes.len() < 2 {
                votes[0]
            } else if votes.iter().all(|v| *v == KeyImageStatus::SpentInBlockchain) {
                KeyImageStatus::SpentInBlockchain
            } else if votes.contains(&KeyImageStatus::SpentInBlockchain) {
                eprintln!(
                    "monero daemon fallback: nodes disagree on whether key image {} is spent in the blockchain - \
                     refusing to affirm a double-spend on a disagreement, but one of the configured nodes is \
                     wrong (or lying) about it and is worth investigating",
                    key_images.get(i).map(String::as_str).unwrap_or("?")
                );
                KeyImageStatus::Unspent
            } else {
                votes[0]
            };
            result.push(status);
        }
        Ok(result)
    }
}

#[cfg(test)]
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
            Self { healthy: AtomicBool::new(healthy), calls: AtomicUsize::new(0) }
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

        async fn get_block_timestamp(&self, _height: u64) -> Result<u64, DaemonError> {
            unimplemented!("not exercised by these tests")
        }

        async fn get_block_transactions(&self, _height: u64) -> Result<Vec<Transaction>, DaemonError> {
            unimplemented!("not exercised by these tests")
        }

        async fn get_mempool_transactions(&self) -> Result<Vec<Transaction>, DaemonError> {
            unimplemented!("not exercised by these tests")
        }

        async fn locate_transaction(&self, _txid: &str) -> Result<TxLocation, DaemonError> {
            unimplemented!("not exercised by these tests")
        }
        async fn get_transaction(&self, _txid: &str) -> Result<Transaction, DaemonError> {
            unimplemented!("not exercised by these tests")
        }

        async fn is_key_image_spent(&self, _key_images: &[String]) -> Result<Vec<KeyImageStatus>, DaemonError> {
            unimplemented!("not exercised by these tests")
        }
    }

    fn node(label: &str, client: Arc<FlakyDaemonClient>) -> (FallbackNode, Arc<FlakyDaemonClient>) {
        (FallbackNode { label: label.to_string(), client: client.clone() }, client)
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
        assert_eq!(primary.call_count(), 1, "the down primary should not be retried once a fallback is sticky");
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
    }

    impl KeyImageDaemonClient {
        fn answering(statuses: Vec<KeyImageStatus>) -> Self {
            Self { statuses, unreachable: false }
        }

        fn unreachable() -> Self {
            Self { statuses: vec![], unreachable: true }
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
        async fn get_block_timestamp(&self, _height: u64) -> Result<u64, DaemonError> {
            unimplemented!("not exercised by these tests")
        }
        async fn get_block_transactions(&self, _height: u64) -> Result<Vec<Transaction>, DaemonError> {
            unimplemented!("not exercised by these tests")
        }
        async fn get_mempool_transactions(&self) -> Result<Vec<Transaction>, DaemonError> {
            unimplemented!("not exercised by these tests")
        }
        async fn locate_transaction(&self, _txid: &str) -> Result<TxLocation, DaemonError> {
            unimplemented!("not exercised by these tests")
        }
        async fn get_transaction(&self, _txid: &str) -> Result<Transaction, DaemonError> {
            unimplemented!("not exercised by these tests")
        }
        async fn is_key_image_spent(&self, key_images: &[String]) -> Result<Vec<KeyImageStatus>, DaemonError> {
            if self.unreachable {
                return Err(DaemonError::Request("key image daemon is unreachable".to_string()));
            }
            assert_eq!(key_images.len(), self.statuses.len(), "test misconfigured: statuses must match key_images length");
            Ok(self.statuses.clone())
        }
    }

    fn ki_node(label: &str, client: KeyImageDaemonClient) -> FallbackNode {
        FallbackNode { label: label.to_string(), client: Arc::new(client) }
    }

    #[tokio::test]
    async fn unanimous_spent_in_blockchain_across_every_node_is_affirmed() {
        let client = FallbackDaemonClient::new(vec![
            ki_node("a", KeyImageDaemonClient::answering(vec![KeyImageStatus::SpentInBlockchain])),
            ki_node("b", KeyImageDaemonClient::answering(vec![KeyImageStatus::SpentInBlockchain])),
        ]);
        let result = client.is_key_image_spent_corroborated(&["ki1".to_string()]).await.unwrap();
        assert_eq!(result, vec![KeyImageStatus::SpentInBlockchain]);
    }

    #[tokio::test]
    async fn disagreement_between_nodes_refuses_to_affirm_a_double_spend() {
        // Exactly the scenario a single lying/wrong node used to cause a wrongful,
        // permanent void for: one node claims spent, another (equally reachable and
        // equally configured) says otherwise. Corroboration must not just trust
        // whichever one happens to be asked.
        let client = FallbackDaemonClient::new(vec![
            ki_node("a", KeyImageDaemonClient::answering(vec![KeyImageStatus::SpentInBlockchain])),
            ki_node("b", KeyImageDaemonClient::answering(vec![KeyImageStatus::Unspent])),
        ]);
        let result = client.is_key_image_spent_corroborated(&["ki1".to_string()]).await.unwrap();
        assert_eq!(result, vec![KeyImageStatus::Unspent], "a disagreement must never affirm SpentInBlockchain");
    }

    #[tokio::test]
    async fn a_single_configured_node_is_trusted_as_is_with_nothing_to_corroborate_against() {
        let client =
            FallbackDaemonClient::new(vec![ki_node("only", KeyImageDaemonClient::answering(vec![KeyImageStatus::SpentInBlockchain]))]);
        let result = client.is_key_image_spent_corroborated(&["ki1".to_string()]).await.unwrap();
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
            ki_node("b", KeyImageDaemonClient::answering(vec![KeyImageStatus::SpentInBlockchain])),
        ]);
        let result = client.is_key_image_spent_corroborated(&["ki1".to_string()]).await.unwrap();
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
        let err = client.is_key_image_spent_corroborated(&["ki1".to_string()]).await.unwrap_err();
        assert!(matches!(err, DaemonError::Request(_)));
    }

    #[tokio::test]
    async fn unanimous_agreement_on_unspent_is_reported_as_is() {
        let client = FallbackDaemonClient::new(vec![
            ki_node("a", KeyImageDaemonClient::answering(vec![KeyImageStatus::Unspent])),
            ki_node("b", KeyImageDaemonClient::answering(vec![KeyImageStatus::Unspent])),
        ]);
        let result = client.is_key_image_spent_corroborated(&["ki1".to_string()]).await.unwrap();
        assert_eq!(result, vec![KeyImageStatus::Unspent]);
    }

    #[tokio::test]
    async fn each_key_image_is_corroborated_independently_of_the_others() {
        // Two key images in one call: nodes agree on the first, disagree on the
        // second - the verdict for one must not leak into the other.
        let client = FallbackDaemonClient::new(vec![
            ki_node(
                "a",
                KeyImageDaemonClient::answering(vec![KeyImageStatus::SpentInBlockchain, KeyImageStatus::SpentInBlockchain]),
            ),
            ki_node("b", KeyImageDaemonClient::answering(vec![KeyImageStatus::SpentInBlockchain, KeyImageStatus::Unspent])),
        ]);
        let result = client.is_key_image_spent_corroborated(&["ki1".to_string(), "ki2".to_string()]).await.unwrap();
        assert_eq!(result, vec![KeyImageStatus::SpentInBlockchain, KeyImageStatus::Unspent]);
    }
}
