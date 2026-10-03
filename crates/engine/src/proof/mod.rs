//! Proof-of-work checking (docs/proof_of_work.md): following the heaviest
//! chain whose every block's proof of work the engine checked itself, from
//! an anchor its nodes agreed on.
//!
//! Each round, every configured node is asked for its tip. A node whose tip
//! is on the proven chain is fine. One whose chain goes on past it has its
//! blocks fetched and checked, from where its chain leaves the proven one:
//! a branch that becomes heavier than the proven chain replaces it (an
//! extension of the tip is heavier at its first block), so the proven chain
//! is always the heaviest valid chain any node has shown. A node that
//! serves a block breaking a rule is caught; one whose whole chain is
//! lighter, or that left the proven chain further back than can be
//! followed, is off it. Either is excluded from scanning
//! (`FallbackDaemonClient::set_excluded`) until its tip is on the proven
//! chain again.
//!
//! Orders settle only on blocks that are both recorded by the scan and
//! proven ([`crate::store::Store::proof_ceiling`]).

pub mod anchor;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::join_all;
use shared::proof::{AnchorStatus, Hashing, NodeProof, NodeVerdict, ProofState, ProofStatus};

use crate::daemon::{DaemonError, MoneroDaemonClient};
use crate::daemon_fallback::FallbackDaemonClient;
use crate::pow::hasher::Hasher;
use crate::pow::{self, ProvenBlock, Rejection, Verdict, Window, DIFFICULTY_BLOCKS};
use crate::store::db::{Class, Db};
use crate::store::proof::ProvenWrite;
use crate::store::{Store, StoreError};

/// How checking is paced and bounded. [`ProofTuning::DEFAULT`] in
/// production; tests pass their own, explicitly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProofTuning {
    /// Blocks below the nodes' tips an anchor is taken at: deeper than any
    /// reorg Monero has seen (18 blocks, in 2025). Also how far back the
    /// proven chain is kept, so the deepest reorg it can follow.
    pub anchor_depth: u64,
    /// Blocks of an anchor's window whose proof of work is checked.
    pub anchor_samples: usize,
    /// Most blocks checked in one round, across nodes: a catch-up after
    /// downtime is spread over rounds, each committed as it goes.
    pub blocks_per_round: u64,
    /// Blocks fetched at once from one node.
    pub fetch_concurrency: usize,
    /// How long one call to a node may take.
    pub call_timeout: Duration,
    /// Time between rounds when nothing is left to do and no node announces
    /// a block (never less than the scan's poll interval).
    pub poll: Duration,
    /// The least difficulty a block of an anchor's window may claim, per
    /// network: a made-up window costs at least this much work a block.
    /// Well below the real difficulty, which can fall.
    pub min_anchor_difficulty_mainnet: u128,
    pub min_anchor_difficulty_stagenet: u128,
    pub min_anchor_difficulty_testnet: u128,
}

impl ProofTuning {
    pub const DEFAULT: ProofTuning = ProofTuning {
        anchor_depth: 720,
        anchor_samples: 64,
        blocks_per_round: 256,
        fetch_concurrency: 8,
        call_timeout: Duration::from_secs(20),
        poll: Duration::from_secs(5),
        // Mainnet's difficulty was about 750 G in October 2026.
        min_anchor_difficulty_mainnet: 100_000_000_000,
        // Stagenet's was about 3.7 M.
        min_anchor_difficulty_stagenet: 100_000,
        min_anchor_difficulty_testnet: 100,
    };

    pub fn min_anchor_difficulty(&self, network: monero::Network) -> u128 {
        match network {
            monero::Network::Mainnet => self.min_anchor_difficulty_mainnet,
            monero::Network::Stagenet => self.min_anchor_difficulty_stagenet,
            monero::Network::Testnet => self.min_anchor_difficulty_testnet,
        }
    }

    /// Refuses a tuning checking can't run with.
    pub fn validate(&self) -> Result<(), String> {
        if self.anchor_depth == 0 {
            return Err("anchor_depth must be at least 1".to_string());
        }
        if self.anchor_samples == 0 {
            return Err("anchor_samples must be at least 1 (the anchor itself)".to_string());
        }
        if self.blocks_per_round == 0 || self.fetch_concurrency == 0 {
            return Err("blocks_per_round and fetch_concurrency must be at least 1".to_string());
        }
        if self.call_timeout.is_zero() || self.poll.is_zero() {
            return Err("call_timeout and poll must be more than zero".to_string());
        }
        if self.min_anchor_difficulty_mainnet == 0
            || self.min_anchor_difficulty_stagenet == 0
            || self.min_anchor_difficulty_testnet == 0
        {
            return Err("a network's anchor difficulty floor must be at least 1".to_string());
        }
        Ok(())
    }

    /// `call` within [`Self::call_timeout`], its error as text.
    async fn bounded<T>(
        &self,
        call: impl std::future::Future<Output = Result<T, DaemonError>>,
    ) -> Result<T, String> {
        match tokio::time::timeout(self.call_timeout, call).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err(format!("no answer within {:?}", self.call_timeout)),
        }
    }
}

/// One configured node, by its label.
pub struct NodeRef<'a> {
    pub label: &'a str,
    pub client: &'a dyn MoneroDaemonClient,
}

/// What looking at one node found, and whether checking it has more to do.
struct Finding {
    verdict: NodeVerdict,
    detail: Option<String>,
    height: Option<u64>,
    /// Blocks left to check (the round's budget ran out, or the chain moved).
    more: bool,
}

impl Finding {
    fn new(verdict: NodeVerdict, height: Option<u64>) -> Self {
        Self {
            verdict,
            detail: None,
            height,
            more: false,
        }
    }

    fn because(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    fn more(mut self) -> Self {
        self.more = true;
        self
    }
}

/// Why a round couldn't do its work: the database, or RandomX itself. The
/// round is retried; nothing is held against any node.
#[derive(Debug, thiserror::Error)]
pub enum ProofError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Hash(#[from] crate::pow::hasher::HashError),
    #[error("{0}")]
    Missing(String),
}

/// What a round did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RoundReport {
    /// Work is left: the next round starts at once.
    pub backlogged: bool,
}

/// One network's checking, kept across rounds. Everything that matters is
/// in the database; losing this costs a key build and the exclusions,
/// which the next round finds again.
pub struct Follower {
    network: monero::Network,
    tuning: ProofTuning,
    hasher: Option<Hasher>,
    /// Nodes caught or found off the proven chain, by label, with what was
    /// found: excluded until their tip is on the proven chain again.
    off_chain: HashMap<String, (NodeVerdict, String)>,
    status: Option<ProofStatus>,
    blocks_checked: u64,
}

impl Follower {
    /// A follower with `tuning`, which must be valid ([`ProofTuning::validate`]).
    pub fn new(network: monero::Network, tuning: ProofTuning) -> Result<Self, String> {
        tuning.validate()?;
        Ok(Self {
            network,
            tuning,
            hasher: None,
            off_chain: HashMap::new(),
            status: None,
            blocks_checked: 0,
        })
    }

    /// For `/status`: `None` while checking is off.
    pub fn status(&self) -> Option<ProofStatus> {
        self.status.clone()
    }

    pub fn tuning(&self) -> &ProofTuning {
        &self.tuning
    }

    async fn db<T: Send + 'static>(
        &self,
        db: &Db,
        f: impl FnOnce(&Store, monero::Network) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, ProofError> {
        let network = self.network;
        Ok(db.run(Class::Scanner, move |s| f(s, network)).await?)
    }

    /// One round: off, it forgets everything; on, it anchors if it must and
    /// follows every node's chain as far as the round allows.
    pub async fn round(
        &mut self,
        db: &Db,
        client: &FallbackDaemonClient,
        enabled: bool,
        now: i64,
    ) -> RoundReport {
        if !enabled {
            self.turn_off(db, client).await;
            return RoundReport::default();
        }
        match self.checked_round(db, client, now).await {
            Ok(report) => report,
            Err(error) => {
                shared::throttled!(
                    format!("proof-round:{:?}", self.network),
                    warn,
                    network = ?self.network,
                    error = %error,
                    "proof-of-work check failed (retried next round)"
                );
                let status = self.status.get_or_insert_with(Default::default);
                status.summary = format!("The last check failed: {error}. It is retried.");
                status.checked_at = Some(now);
                RoundReport::default()
            }
        }
    }

    async fn turn_off(&mut self, db: &Db, client: &FallbackDaemonClient) {
        let was_on = self.status.take().is_some() || self.hasher.is_some();
        self.hasher = None;
        if !self.off_chain.is_empty() {
            self.off_chain.clear();
            client.set_excluded(&[]);
        }
        // Idempotent and cheap: deletes nothing once done. Done every round,
        // so a failure is retried.
        if let Err(error) = self.db(db, |s, network| s.disable_proof(network)).await {
            shared::throttled!(
                format!("proof-off:{:?}", self.network),
                warn,
                network = ?self.network,
                error = %error,
                "turning proof-of-work checking off failed (retried)"
            );
        } else if was_on {
            tracing::info!(network = ?self.network, "proof-of-work checking turned off: orders settle on the node's word");
        }
    }

    async fn checked_round(
        &mut self,
        db: &Db,
        client: &FallbackDaemonClient,
        now: i64,
    ) -> Result<RoundReport, ProofError> {
        self.db(db, move |s, network| s.enable_proof(network, now))
            .await?;
        let hasher = match &self.hasher {
            Some(hasher) => hasher.clone(),
            None => {
                let name = format!("randomx {}", shared::network::network_str(self.network));
                let hasher = Hasher::start(&name)?;
                self.hasher = Some(hasher.clone());
                hasher
            }
        };
        let nodes: Vec<NodeRef<'_>> = client
            .nodes()
            .iter()
            .map(|node| NodeRef {
                label: &node.label,
                client: node.client.as_ref(),
            })
            .collect();
        let state = self.db(db, |s, network| s.proof_network(network)).await?;
        let anchor = match state.and_then(|state| state.anchor) {
            Some(anchor) => anchor,
            None => match anchor::take(&nodes, self.network, &self.tuning, &hasher).await {
                Ok(new) => {
                    let top = new.window.last().map(|b| b.height).unwrap_or_default();
                    let (agreed, total) = (new.agreed, new.nodes);
                    self.db(db, move |s, network| s.write_anchor(network, &new, now))
                        .await?;
                    tracing::info!(
                        network = ?self.network,
                        height = top,
                        agreed,
                        nodes = total,
                        "proof-of-work checking anchored"
                    );
                    self.db(db, |s, network| s.proof_network(network))
                        .await?
                        .and_then(|state| state.anchor)
                        .ok_or_else(|| ProofError::Missing("the anchor just written".to_string()))?
                }
                Err(problem) => {
                    shared::throttled!(
                        format!("proof-anchor:{:?}", self.network),
                        info,
                        network = ?self.network,
                        problem = %problem,
                        "proof-of-work checking can't anchor yet"
                    );
                    self.status = Some(ProofStatus {
                        state: ProofState::Anchoring,
                        summary: format!(
                            "Waiting for an anchor: {problem}. No order on this network settles until there is one."
                        ),
                        ceiling: Some(0),
                        hashing: Some(hashing(&hasher)),
                        blocks_checked: self.blocks_checked,
                        checked_at: Some(now),
                        ..Default::default()
                    });
                    return Ok(RoundReport::default());
                }
            },
        };

        let tips = join_all(
            nodes
                .iter()
                .map(|node| self.tuning.bounded(node.client.get_tip())),
        )
        .await;
        // The node claiming the highest tip first: a catch-up comes from
        // the furthest chain, and a liar claiming far ahead is caught at
        // its first bad block.
        let mut order: Vec<usize> = (0..nodes.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(tips[i].as_ref().map_or(0, |t| t.height)));
        let mut budget = self.tuning.blocks_per_round;
        let mut findings: Vec<Option<Finding>> = (0..nodes.len()).map(|_| None).collect();
        let mut backlogged = false;
        for i in order {
            let finding = match &tips[i] {
                Err(error) => Finding::new(NodeVerdict::Unreachable, None).because(error.clone()),
                Ok(tip) => {
                    self.look_at(
                        db,
                        &nodes[i],
                        tip.height,
                        tip.hash.clone(),
                        &hasher,
                        &mut budget,
                        now,
                    )
                    .await?
                }
            };
            backlogged |= finding.more;
            findings[i] = Some(finding);
        }
        let findings: Vec<Finding> = findings
            .into_iter()
            .map(|f| f.unwrap_or_else(|| Finding::new(NodeVerdict::Unknown, None)))
            .collect();
        self.exclude(client, &nodes, &findings);

        let tip = self
            .db(db, |s, network| s.proven_tip(network))
            .await?
            .ok_or_else(|| ProofError::Missing("a proven chain after anchoring".to_string()))?;
        let keep_from = tip
            .height
            .saturating_sub(self.tuning.anchor_depth + DIFFICULTY_BLOCKS as u64 - 1);
        self.db(db, move |s, network| s.prune_proven(network, keep_from))
            .await?;
        let ceiling = self.db(db, |s, network| s.proof_ceiling(network)).await?;
        self.status = Some(self.describe(&anchor, &tip, ceiling, &nodes, findings, &hasher, now));
        Ok(RoundReport { backlogged })
    }

    /// Looks at one node whose tip is `height` (`hash`, if it said), and
    /// checks its chain past the proven one, within `budget` blocks.
    #[allow(clippy::too_many_arguments)] // one node's look, with the round's shared handles
    async fn look_at(
        &mut self,
        db: &Db,
        node: &NodeRef<'_>,
        height: u64,
        hash: Option<String>,
        hasher: &Hasher,
        budget: &mut u64,
        now: i64,
    ) -> Result<Finding, ProofError> {
        let tuning = self.tuning.clone();
        let ours = self
            .db(db, |s, network| s.proven_tip(network))
            .await?
            .ok_or_else(|| ProofError::Missing("a proven chain".to_string()))?;
        let floor = self
            .db(db, |s, network| s.proven_floor(network))
            .await?
            .ok_or_else(|| ProofError::Missing("a proven chain".to_string()))?;
        // The lowest block with a whole window below it: the deepest a
        // branch can leave the proven chain and still be checked.
        let deepest = floor.height + DIFFICULTY_BLOCKS as u64 - 1;
        let unreachable =
            |error: String| Finding::new(NodeVerdict::Unreachable, Some(height)).because(error);
        let node_hash = |at: u64| tuning.bounded(node.client.get_block_hash(at));

        let top = height.min(ours.height);
        let top_hash = match (&hash, top == height) {
            (Some(hash), true) => hash.clone(),
            _ => match node_hash(top).await {
                Ok(hash) => hash,
                Err(error) => return Ok(unreachable(error)),
            },
        };
        let ours_at = |at: u64| self.db(db, move |s, network| s.proven_block(network, at));
        let agrees_at_top = ours_at(top)
            .await?
            .is_some_and(|b| hex::encode(b.id) == top_hash.to_ascii_lowercase());
        let parent_height = if agrees_at_top {
            if height <= ours.height {
                return Ok(Finding::new(NodeVerdict::OnChain, Some(height)));
            }
            ours.height
        } else {
            if top < deepest {
                return Ok(Finding::new(NodeVerdict::Diverged, Some(height))
                    .because(format!("its chain isn't the proven one at block {top}, below the deepest followable block {deepest}")));
            }
            let network = self.network;
            let agrees = |at: u64, theirs: String| {
                let db = db.clone();
                async move {
                    let mine = db
                        .run(Class::Scanner, move |s| s.proven_block(network, at))
                        .await?;
                    Ok::<bool, ProofError>(
                        mine.is_some_and(|b| hex::encode(b.id) == theirs.to_ascii_lowercase()),
                    )
                }
            };
            let at_deepest = match node_hash(deepest).await {
                Ok(hash) => hash,
                Err(error) => return Ok(unreachable(error)),
            };
            if !agrees(deepest, at_deepest).await? {
                return Ok(Finding::new(NodeVerdict::Diverged, Some(height)).because(format!(
                    "its chain left the proven one before block {deepest}, further back than can be followed"
                )));
            }
            // `lo` agrees, `hi` doesn't.
            let (mut lo, mut hi) = (deepest, top);
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                let theirs = match node_hash(mid).await {
                    Ok(hash) => hash,
                    Err(error) => return Ok(unreachable(error)),
                };
                if agrees(mid, theirs).await? {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo
        };
        self.check_branch(db, node, parent_height, height, ours, hasher, budget, now)
            .await
    }

    /// Checks `node`'s blocks after `parent_height` (where its chain leaves
    /// the proven one, or the proven tip) up to its tip `height`, replacing
    /// the proven chain above `parent_height` as soon as the branch is
    /// heavier than `ours`, the proven tip.
    #[allow(clippy::too_many_arguments)] // one branch's check, with the round's shared handles
    async fn check_branch(
        &mut self,
        db: &Db,
        node: &NodeRef<'_>,
        parent_height: u64,
        height: u64,
        ours: ProvenBlock,
        hasher: &Hasher,
        budget: &mut u64,
        now: i64,
    ) -> Result<Finding, ProofError> {
        let window_blocks = self
            .db(db, move |s, network| {
                s.proven_blocks_ending_at(network, parent_height, DIFFICULTY_BLOCKS as u64)
            })
            .await?;
        let mut window = Window::new(window_blocks);
        if !window.is_full() {
            return Err(ProofError::Missing(format!(
                "the {DIFFICULTY_BLOCKS} proven blocks up to {parent_height}"
            )));
        }
        let Some(mut parent) = window.tip().cloned() else {
            return Err(ProofError::Missing(format!("proven block {parent_height}")));
        };
        let mut heaviest = ours.cumulative_difficulty;
        let mut branch: Vec<ProvenBlock> = Vec::new();
        let now_secs = u64::try_from(now).unwrap_or(0);
        let mut next = parent_height + 1;
        while next <= height {
            if *budget == 0 {
                return Ok(Finding::new(NodeVerdict::Ahead, Some(height))
                    .because(format!(
                        "checked up to block {}; the rest next round",
                        next - 1
                    ))
                    .more());
            }
            // At most 64 at once: every block's RandomX key is then at least
            // 65 blocks back, so known before the batch.
            let count = (height - next + 1)
                .min(*budget)
                .min(pow::SEEDHASH_EPOCH_LAG);
            let heights: Vec<u64> = (next..next + count).collect();
            let mut blobs = Vec::with_capacity(heights.len());
            for chunk in heights.chunks(self.tuning.fetch_concurrency) {
                let fetched = join_all(
                    chunk
                        .iter()
                        .map(|&h| self.tuning.bounded(node.client.get_block_blob(h))),
                )
                .await;
                for blob in fetched {
                    match blob {
                        Ok(blob) => blobs.push(blob),
                        Err(error) => {
                            return Ok(
                                Finding::new(NodeVerdict::Unreachable, Some(height)).because(error)
                            );
                        }
                    }
                }
            }
            let mut candidates = Vec::with_capacity(blobs.len());
            for (&h, blob) in heights.iter().zip(&blobs) {
                match pow::decode(h, blob) {
                    Ok(candidate) => candidates.push(candidate),
                    Err(rejection) => return Ok(self.rejected(node, height, &rejection)),
                }
            }
            // Hash the batch, a request per key.
            let mut keyed: Vec<([u8; 32], Vec<usize>)> = Vec::new();
            for (i, candidate) in candidates.iter().enumerate() {
                let key_height = pow::seed_height(candidate.height);
                let key = match branch.iter().find(|b| b.height == key_height) {
                    Some(block) => block.id,
                    None => self
                        .db(db, move |s, network| s.proof_seed(network, key_height))
                        .await?
                        .ok_or_else(|| {
                            ProofError::Missing(format!("the RandomX key at block {key_height}"))
                        })?,
                };
                match keyed.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, members)) => members.push(i),
                    None => keyed.push((key, vec![i])),
                }
            }
            let mut hashes = vec![[0u8; 32]; candidates.len()];
            for (key, members) in keyed {
                let inputs = members
                    .iter()
                    .map(|&i| candidates[i].pow_input.clone())
                    .collect();
                for (i, hash) in members.iter().zip(hasher.hash(key, inputs).await?) {
                    hashes[*i] = hash;
                }
            }
            for (candidate, hash) in candidates.iter().zip(&hashes) {
                let checked = pow::check_header(&window, candidate, now_secs)
                    .and_then(|difficulty| pow::accept(&window, candidate, difficulty, hash));
                match checked {
                    Ok(block) => {
                        window.push(block.clone());
                        branch.push(block);
                        self.blocks_checked += 1;
                        *budget = budget.saturating_sub(1);
                    }
                    Err(rejection) => {
                        // What was checked of this branch still counts if it
                        // is heavier.
                        self.commit_if_heavier(db, &mut parent, &mut branch, &mut heaviest)
                            .await?;
                        return Ok(self.rejected(node, height, &rejection));
                    }
                }
            }
            if self
                .commit_if_heavier(db, &mut parent, &mut branch, &mut heaviest)
                .await?
                == Some(ProvenWrite::Stale)
            {
                return Ok(Finding::new(NodeVerdict::Unknown, Some(height))
                    .because("the proven chain changed while its blocks were checked")
                    .more());
            }
            next += count;
        }
        if branch.is_empty() {
            Ok(Finding::new(NodeVerdict::OnChain, Some(height)))
        } else {
            Ok(
                Finding::new(NodeVerdict::Lighter, Some(height)).because(format!(
                    "its chain leaves the proven one after block {} and has less work",
                    parent.height
                )),
            )
        }
    }

    /// Makes `branch` (checked blocks after `parent`) the proven chain's
    /// top if it is heavier than `heaviest`, the proven tip's work. Then
    /// the branch's last block is the parent of what follows.
    async fn commit_if_heavier(
        &self,
        db: &Db,
        parent: &mut ProvenBlock,
        branch: &mut Vec<ProvenBlock>,
        heaviest: &mut u128,
    ) -> Result<Option<ProvenWrite>, ProofError> {
        let Some(last) = branch.last().cloned() else {
            return Ok(None);
        };
        if last.cumulative_difficulty <= *heaviest {
            return Ok(None);
        }
        let (from, blocks) = (parent.clone(), std::mem::take(branch));
        // Blocks above the parent are replaced: a switch, not an extension.
        let switched = self
            .db(db, |s, network| s.proven_tip(network))
            .await?
            .is_some_and(|tip| tip.height > from.height);
        let written = self
            .db(db, move |s, network| {
                s.write_proven(network, &from, &blocks)
            })
            .await?;
        if written == ProvenWrite::Written {
            if switched {
                tracing::warn!(
                    network = ?self.network,
                    fork = parent.height,
                    height = last.height,
                    "proven chain switched to a heavier branch"
                );
            }
            *parent = last.clone();
            *heaviest = last.cumulative_difficulty;
        }
        Ok(Some(written))
    }

    fn rejected(&self, node: &NodeRef<'_>, height: u64, rejection: &Rejection) -> Finding {
        match rejection.verdict() {
            Verdict::Invalid => {
                shared::throttled!(
                    format!("proof-caught:{}", node.label),
                    warn,
                    network = ?self.network,
                    node = %node.label,
                    rejection = %rejection,
                    "a node served a block that breaks the proof-of-work rules"
                );
                Finding::new(NodeVerdict::Caught, Some(height)).because(rejection.to_string())
            }
            Verdict::NotYet => {
                Finding::new(NodeVerdict::Ahead, Some(height)).because(rejection.to_string())
            }
            Verdict::Moved => Finding::new(NodeVerdict::Unknown, Some(height))
                .because("its chain moved while its blocks were checked")
                .more(),
        }
    }

    /// Excludes from scanning every node caught or found off the proven
    /// chain, until it is found on it again.
    fn exclude(
        &mut self,
        client: &FallbackDaemonClient,
        nodes: &[NodeRef<'_>],
        findings: &[Finding],
    ) {
        for (node, finding) in nodes.iter().zip(findings) {
            match finding.verdict {
                NodeVerdict::OnChain | NodeVerdict::Ahead => {
                    self.off_chain.remove(node.label);
                }
                NodeVerdict::Caught | NodeVerdict::Lighter | NodeVerdict::Diverged => {
                    self.off_chain.insert(
                        node.label.to_string(),
                        (finding.verdict, finding.detail.clone().unwrap_or_default()),
                    );
                }
                NodeVerdict::Unknown | NodeVerdict::Unreachable => {}
            }
        }
        // Labels of nodes no longer configured are forgotten.
        self.off_chain
            .retain(|label, _| nodes.iter().any(|node| node.label == label));
        let indices: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| self.off_chain.contains_key(node.label))
            .map(|(i, _)| i)
            .collect();
        if !client.set_excluded(&indices) {
            shared::throttled!(
                format!("proof-exclude-all:{:?}", self.network),
                warn,
                network = ?self.network,
                "every node is off the proven chain; none is excluded, and nothing new settles"
            );
            client.set_excluded(&[]);
        }
    }

    #[allow(clippy::too_many_arguments)] // the round's facts, gathered once
    fn describe(
        &self,
        anchor: &crate::store::proof::Anchor,
        tip: &ProvenBlock,
        ceiling: Option<u64>,
        nodes: &[NodeRef<'_>],
        findings: Vec<Finding>,
        hasher: &Hasher,
        now: i64,
    ) -> ProofStatus {
        let reachable: Vec<&Finding> = findings
            .iter()
            .filter(|f| !matches!(f.verdict, NodeVerdict::Unreachable | NodeVerdict::Unknown))
            .collect();
        let none_on_chain = !reachable.is_empty()
            && reachable.iter().all(|f| {
                matches!(
                    f.verdict,
                    NodeVerdict::Caught | NodeVerdict::Diverged | NodeVerdict::Lighter
                )
            });
        let diverged = reachable.iter().any(|f| f.verdict == NodeVerdict::Diverged);
        let (state, summary) = if none_on_chain && diverged {
            (
                ProofState::Held,
                format!(
                    "Every node's chain left the proven one more than {} blocks back, further than can be followed. Nothing new settles. If the network really reorganised that deep, check the nodes, then take a new anchor.",
                    self.tuning.anchor_depth
                ),
            )
        } else if none_on_chain {
            (
                ProofState::Held,
                "No node serves the proven chain or a valid heavier one. Nothing new settles until one does.".to_string(),
            )
        } else {
            let excluded = self.off_chain.len();
            let mut summary = format!(
                "Proven up to block {}; orders settle on blocks up to {}.",
                tip.height,
                ceiling.unwrap_or(0)
            );
            if excluded > 0 {
                summary.push_str(&format!(
                    " {excluded} node{} left out of scanning for serving another chain.",
                    if excluded == 1 { "" } else { "s" }
                ));
            }
            (ProofState::Following, summary)
        };
        ProofStatus {
            state,
            summary,
            anchor: Some(AnchorStatus {
                height: anchor.height,
                hash: anchor.hash.clone(),
                agreed: anchor.agreed,
                nodes: anchor.nodes,
                anchored_at: anchor.anchored_at,
            }),
            proven_height: Some(tip.height),
            proven_hash: Some(hex::encode(tip.id)),
            ceiling,
            nodes: nodes
                .iter()
                .zip(findings)
                .map(|(node, finding)| NodeProof {
                    node: node.label.to_string(),
                    height: finding.height,
                    verdict: finding.verdict,
                    detail: finding.detail.or_else(|| {
                        self.off_chain
                            .get(node.label)
                            .map(|(_, detail)| detail.clone())
                    }),
                    excluded: self.off_chain.contains_key(node.label),
                })
                .collect(),
            blocks_checked: self.blocks_checked,
            hashing: Some(hashing(hasher)),
            checked_at: Some(now),
        }
    }
}

fn hashing(hasher: &Hasher) -> Hashing {
    let stats = hasher.stats();
    Hashing {
        jit: stats.jit,
        mean_hash_ms: stats.mean_hash_ms,
        mean_key_build_ms: stats.mean_key_build_ms,
        keys_held: stats.keys_held,
    }
}

/// Runs proof-of-work checking for `network` for the life of its node
/// setting: a round whenever its node announces a block or the poll
/// interval passes, at once while work is left. Each round reads whether
/// checking is on, so the setting applies from the next round; off, the
/// RandomX memory is freed.
pub async fn run_loop(
    network: monero::Network,
    db: Db,
    daemons: crate::engine_settings::Daemons,
    settings: Arc<crate::engine_settings::EngineSettings>,
    status: crate::scanner_status::ScannerStatusMap,
    wakes: Arc<crate::node_events::NodeWakes>,
    tuning: ProofTuning,
) {
    let mut follower = match Follower::new(network, tuning) {
        Ok(follower) => follower,
        Err(error) => {
            tracing::error!(network = ?network, error = %error, "proof-of-work checking can't run with this tuning");
            return;
        }
    };
    loop {
        let scan = settings.scan.load();
        let poll = follower.tuning().poll.max(scan.poll_interval);
        let Some(client) = daemons.get(network) else {
            tokio::time::sleep(poll).await;
            continue;
        };
        let enabled = scan.checks_proof_of_work(network);
        let report = follower
            .round(&db, &client, enabled, crate::now_unix())
            .await;
        // Not for a network being stopped: its status row was removed.
        if let Some(entry) = status.write().get_mut(&network) {
            entry.proof = follower.status();
        }
        if report.backlogged {
            tokio::task::yield_now().await;
        } else {
            wakes.proof_or(poll).await;
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
