//! Proof-of-work checking (`docs/proof_of_work.md)`: following the heaviest
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
use std::fmt::Write as _;
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
    /// reorg Monero has seen (18 blocks, in 2025). Also the deepest reorg
    /// the proven chain can follow.
    pub anchor_depth: u64,
    /// Blocks the proven chain's ids are kept for (at least what
    /// `anchor_depth` needs): a payment settles only on a block found here,
    /// so one older than this, never settled, waits for an operator.
    pub keep_blocks: u64,
    /// How long a node caught serving a block that breaks a rule stays
    /// excluded, even once it serves the proven chain again: a liar
    /// mustn't get back in by answering the proof loop honestly for a
    /// round.
    pub caught_for: Duration,
    /// The wait after an anchor couldn't be taken, doubling with each
    /// failure in a row up to ten times it: a fresh random sample each
    /// round would let a window with a few bad blocks through by retries.
    pub anchor_retry: Duration,
    /// Blocks of an anchor's window whose proof of work is checked.
    pub anchor_samples: usize,
    /// Most blocks checked for one node in one round: a catch-up after
    /// downtime is spread over rounds, each committed as it goes. A branch
    /// leaving the proven chain below its tip gets as many as it needs to
    /// outweigh it.
    pub blocks_per_round: u64,
    /// Blocks fetched at once from one node.
    pub fetch_concurrency: usize,
    /// How long one call to a node may take.
    pub call_timeout: Duration,
    /// Time between rounds when nothing is left to do and no node announces
    /// a block (never less than the scan's poll interval).
    pub poll: Duration,
    /// The least difficulty any block may have, per network: claimed by an
    /// anchor's window, or computed for a block after it. A made-up chain
    /// then costs at least this much work a block, however its timestamps
    /// were bent. Well below the real difficulty, which can fall.
    pub min_difficulty_mainnet: u128,
    pub min_difficulty_stagenet: u128,
    pub min_difficulty_testnet: u128,
}

impl ProofTuning {
    pub const DEFAULT: Self = Self {
        anchor_depth: 720,
        // 30 days.
        keep_blocks: 21_600,
        caught_for: Duration::from_hours(1),
        anchor_retry: Duration::from_secs(60),
        anchor_samples: 64,
        blocks_per_round: 256,
        fetch_concurrency: 8,
        call_timeout: Duration::from_secs(20),
        poll: Duration::from_secs(5),
        // Mainnet's difficulty was about 750 G in October 2026.
        min_difficulty_mainnet: 100_000_000_000,
        // Stagenet's was about 3.7 M.
        min_difficulty_stagenet: 100_000,
        min_difficulty_testnet: 100,
    };

    pub fn min_difficulty(&self, network: monero::Network) -> u128 {
        match network {
            monero::Network::Mainnet => self.min_difficulty_mainnet,
            monero::Network::Stagenet => self.min_difficulty_stagenet,
            monero::Network::Testnet => self.min_difficulty_testnet,
        }
    }

    /// Proven blocks kept below the tip: the larger of what a reorg
    /// `anchor_depth` deep needs (its window too) and `keep_blocks`.
    fn kept(&self) -> u64 {
        (self.anchor_depth + DIFFICULTY_BLOCKS as u64 - 1).max(self.keep_blocks)
    }

    /// Refuses a tuning checking can't run with.
    pub fn validate(&self) -> Result<(), String> {
        if self.anchor_depth == 0 {
            return Err("anchor_depth must be at least 1".to_owned());
        }
        if self.anchor_samples == 0 {
            return Err("anchor_samples must be at least 1 (the anchor itself)".to_owned());
        }
        if self.blocks_per_round == 0 || self.fetch_concurrency == 0 {
            return Err("blocks_per_round and fetch_concurrency must be at least 1".to_owned());
        }
        if self.call_timeout.is_zero() || self.poll.is_zero() || self.anchor_retry.is_zero() {
            return Err("call_timeout, poll and anchor_retry must be more than zero".to_owned());
        }
        if self.min_difficulty_mainnet == 0
            || self.min_difficulty_stagenet == 0
            || self.min_difficulty_testnet == 0
        {
            return Err("a network's difficulty floor must be at least 1".to_owned());
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
#[derive(Clone)]
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

/// Why a round couldn't do its work: the database, or `RandomX` itself. The
/// round is retried; nothing is held against any node.
#[derive(Debug, thiserror::Error)]
pub enum ProofError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Hash(#[from] pow::hasher::HashError),
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
    /// Nodes caught or found off the proven chain, by label: excluded until
    /// their tip is on the proven chain again (a caught one, not before
    /// [`ProofTuning::caught_for`]).
    off_chain: HashMap<String, OffChain>,
    /// What each node's tip was last found to be, by label, when it was
    /// lighter or caught: not checked again until its tip changes.
    settled: HashMap<String, (String, Finding)>,
    status: Option<ProofStatus>,
    blocks_checked: u64,
    /// No anchor is tried before this, after a failure; and how many
    /// failed in a row.
    next_anchor: Option<tokio::time::Instant>,
    anchor_failures: u32,
    /// Checking is known to be off in the database (forgotten on the way).
    known_off: bool,
}

/// A node excluded from scanning, and since when.
struct OffChain {
    verdict: NodeVerdict,
    detail: String,
    since: tokio::time::Instant,
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
            settled: HashMap::new(),
            status: None,
            blocks_checked: 0,
            next_anchor: None,
            anchor_failures: 0,
            known_off: false,
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
        self.settled.clear();
        self.next_anchor = None;
        self.anchor_failures = 0;
        if !self.off_chain.is_empty() {
            self.off_chain.clear();
            client.set_excluded(&[]);
        }
        // Once (at start, or when turned off), and again until it works.
        if self.known_off {
            return;
        }
        if let Err(error) = self.db(db, Store::disable_proof).await {
            shared::throttled!(
                format!("proof-off:{:?}", self.network),
                warn,
                network = ?self.network,
                error = %error,
                "turning proof-of-work checking off failed (retried)"
            );
        } else {
            self.known_off = true;
            if was_on {
                tracing::info!(network = ?self.network, "proof-of-work checking turned off: orders settle on the node's word");
            }
        }
    }

    async fn checked_round(
        &mut self,
        db: &Db,
        client: &FallbackDaemonClient,
        now: i64,
    ) -> Result<RoundReport, ProofError> {
        self.known_off = false;
        let mut state = self.db(db, Store::proof_network).await?;
        if state.is_none() {
            state = self
                .db(db, move |s, network| {
                    s.enable_proof(network, now)?;
                    s.proof_network(network)
                })
                .await?;
            tracing::info!(network = ?self.network, "proof-of-work checking turned on: orders settle on proven blocks only");
        }
        let hasher = if let Some(hasher) = &self.hasher {
            hasher.clone()
        } else {
            let name = format!(
                "engine-randomx-{}",
                shared::network::network_str(self.network)
            );
            let hasher = Hasher::start(&name)?;
            self.hasher = Some(hasher.clone());
            hasher
        };
        let nodes: Vec<NodeRef<'_>> = client
            .nodes()
            .iter()
            .map(|node| NodeRef {
                label: &node.label,
                client: node.client.as_ref(),
            })
            .collect();
        let anchor = match state.and_then(|state| state.anchor) {
            Some(anchor) => anchor,
            None if self
                .next_anchor
                .is_some_and(|at| tokio::time::Instant::now() < at) =>
            {
                return Ok(RoundReport::default());
            }
            None => match anchor::take(&nodes, self.network, &self.tuning, &hasher, now).await {
                Ok(new) => {
                    self.next_anchor = None;
                    self.anchor_failures = 0;
                    let top = new.window.last().map_or_default(|b| b.height);
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
                    self.db(db, Store::proof_network)
                        .await?
                        .and_then(|state| state.anchor)
                        .ok_or_else(|| ProofError::Missing("the anchor just written".to_owned()))?
                }
                Err(problem) => {
                    self.anchor_failures = self.anchor_failures.saturating_add(1);
                    let wait = self
                        .tuning
                        .anchor_retry
                        .saturating_mul(1 << self.anchor_failures.saturating_sub(1).min(10))
                        .min(self.tuning.anchor_retry.saturating_mul(10));
                    self.next_anchor = Some(tokio::time::Instant::now() + wait);
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
        let mut findings: Vec<Option<Finding>> = std::iter::repeat_n(None, nodes.len()).collect();
        let mut backlogged = false;
        for i in order {
            // Each node its own: one can't use up another's.
            let mut budget = self.tuning.blocks_per_round;
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
            .db(db, Store::proven_tip)
            .await?
            .ok_or_else(|| ProofError::Missing("a proven chain after anchoring".to_owned()))?;
        let keep_from = tip.height.saturating_sub(self.tuning.kept());
        self.db(db, move |s, network| s.prune_proven(network, keep_from))
            .await?;
        let ceiling = self.db(db, Store::proof_ceiling).await?;
        self.status = Some(self.describe(
            client, &anchor, &tip, ceiling, &nodes, findings, &hasher, now,
        ));
        Ok(RoundReport { backlogged })
    }

    /// Looks at one node whose tip is `height` (`hash`, if it said), and
    /// checks its chain past the proven one, within `budget` blocks.
    #[expect(
        clippy::too_many_arguments,
        reason = "one node's look, with the round's shared handles"
    )]
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
        // Its tip was found lighter or caught and hasn't changed: nothing
        // new to check.
        if let (Some(hash), Some((seen, finding))) = (&hash, self.settled.get(node.label)) {
            if seen == hash {
                return Ok(finding.clone());
            }
        }
        let finding = self
            .look_afresh(db, node, height, hash.clone(), &tuning, hasher, budget, now)
            .await?;
        match (&hash, finding.verdict) {
            (Some(hash), NodeVerdict::Lighter | NodeVerdict::Caught) => {
                self.settled
                    .insert(node.label.to_owned(), (hash.clone(), finding.clone()));
            }
            _ => {
                self.settled.remove(node.label);
            }
        }
        Ok(finding)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one node's look, with the round's shared handles"
    )]
    async fn look_afresh(
        &mut self,
        db: &Db,
        node: &NodeRef<'_>,
        height: u64,
        hash_at_height: Option<String>,
        tuning: &ProofTuning,
        hasher: &Hasher,
        budget: &mut u64,
        now: i64,
    ) -> Result<Finding, ProofError> {
        let ours = self
            .db(db, Store::proven_tip)
            .await?
            .ok_or_else(|| ProofError::Missing("a proven chain".to_owned()))?;
        let floor = self
            .db(db, Store::proven_floor)
            .await?
            .ok_or_else(|| ProofError::Missing("a proven chain".to_owned()))?;
        // The lowest block with a whole window below it: the deepest a
        // branch can leave the proven chain and still be checked.
        let deepest = floor.height + DIFFICULTY_BLOCKS as u64 - 1;
        let unreachable =
            |error: String| Finding::new(NodeVerdict::Unreachable, Some(height)).because(error);
        let node_hash = |at: u64| tuning.bounded(node.client.get_block_hash(at));

        if height < floor.height {
            // Syncing, or long down: below everything kept, so nothing to
            // compare. Not held against it.
            return Ok(
                Finding::new(NodeVerdict::Unknown, Some(height)).because(format!(
                    "too far behind (block {height}) to compare with the proven chain"
                )),
            );
        }
        let top = height.min(ours.height);
        let top_hash = match (&hash_at_height, top == height) {
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
            if lo >= height {
                // It said its tip isn't the proven block there, then that
                // the proven chain runs up to it.
                return Ok(self.caught(
                    node,
                    height,
                    format!("its answers about block {height} contradict each other"),
                ));
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
    ///
    /// Each batch is checked cheapest first: blocks the proven chain
    /// already has are passed over unhashed (a node's word on where its
    /// chain leaves ours costs it nothing to bend); the rest have every rule
    /// but the hash checked in order, then are hashed, then their time is
    /// checked against the clock (a bad proof is caught whatever its time).
    #[expect(
        clippy::too_many_arguments,
        reason = "one branch's check, with the round's shared handles"
    )]
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
        // A branch below the proven tip gets what it needs to outweigh it.
        let competing = parent_height < ours.height;
        if competing {
            *budget = (*budget).max(ours.height - parent_height + pow::SEEDHASH_EPOCH_LAG);
        }
        let floor = self.tuning.min_difficulty(self.network);
        let mut heaviest = ours.cumulative_difficulty;
        let mut branch: Vec<ProvenBlock> = Vec::new();
        let now_secs = u64::try_from(now).unwrap_or(0);
        let mut next = parent_height + 1;
        while next <= height {
            if *budget == 0 {
                if competing && !branch.is_empty() {
                    return Ok(Finding::new(NodeVerdict::Lighter, Some(height)).because(format!(
                        "its chain leaves the proven one after block {} and, {} blocks on, still has less work",
                        parent.height,
                        branch.len()
                    )));
                }
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
            next += count;
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

            // Blocks the proven chain has: passed over.
            let mut fresh = candidates.as_slice();
            while let Some(candidate) = fresh.first().filter(|_| branch.is_empty()) {
                let at = candidate.height;
                let mine = self
                    .db(db, move |s, network| s.proven_block(network, at))
                    .await?;
                match mine {
                    Some(mine) if mine.id == candidate.id => {
                        window.push(mine.clone());
                        parent = mine;
                        fresh = &fresh[1..];
                    }
                    _ => break,
                }
            }

            // Every rule but the hash, in order, against the window as it
            // would be: stop at the first broken one.
            let mut pending: Vec<(usize, u128, ProvenBlock)> = Vec::new();
            let mut broken: Option<Rejection> = None;
            let mut ahead = window.clone();
            for (i, candidate) in fresh.iter().enumerate() {
                let difficulty = match pow::check_header(&ahead, candidate) {
                    Ok(difficulty) if difficulty < floor => {
                        broken = Some(Rejection::BelowFloor {
                            height: candidate.height,
                            difficulty,
                            floor,
                        });
                        break;
                    }
                    Ok(difficulty) => difficulty,
                    Err(rejection) => {
                        broken = Some(rejection);
                        break;
                    }
                };
                let block = ProvenBlock {
                    height: candidate.height,
                    id: candidate.id,
                    timestamp: candidate.timestamp,
                    cumulative_difficulty: ahead
                        .tip()
                        .map_or(0, |tip| tip.cumulative_difficulty)
                        .saturating_add(difficulty),
                };
                ahead.push(block.clone());
                pending.push((i, difficulty, block));
            }

            // Then the hashes, a request per key.
            let mut keyed: Vec<([u8; 32], Vec<usize>)> = Vec::new();
            for (at, (i, _, _)) in pending.iter().enumerate() {
                let key_height = pow::seed_height(fresh[*i].height);
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
                    Some((_, members)) => members.push(at),
                    None => keyed.push((key, vec![at])),
                }
            }
            let mut pow_hashes = vec![[0u8; 32]; pending.len()];
            for (key, members) in keyed {
                let inputs = members
                    .iter()
                    .map(|&at| fresh[pending[at].0].pow_input.clone())
                    .collect();
                for (at, hash) in members.iter().zip(hasher.hash(key, inputs).await?) {
                    pow_hashes[*at] = hash;
                }
            }
            for ((i, difficulty, block), hash) in pending.into_iter().zip(&pow_hashes) {
                let candidate = &fresh[i];
                let checked = pow::accept(&window, candidate, difficulty, hash)
                    .and_then(|_| pow::check_time(candidate, now_secs));
                if let Err(rejection) = checked {
                    broken = Some(rejection);
                    break;
                }
                window.push(block.clone());
                branch.push(block);
                self.blocks_checked += 1;
                *budget = budget.saturating_sub(1);
            }
            // What was checked of this branch counts if it is heavier.
            let written = self
                .commit_if_heavier(db, &mut parent, &mut branch, &mut heaviest)
                .await?;
            if let Some(rejection) = broken {
                return Ok(self.rejected(node, height, &rejection));
            }
            if written == Some(ProvenWrite::Stale) {
                return Ok(Finding::new(NodeVerdict::Unknown, Some(height))
                    .because("the proven chain changed while its blocks were checked")
                    .more());
            }
        }
        if branch.is_empty() {
            Ok(Finding::new(NodeVerdict::OnChain, Some(height)))
        } else {
            Ok(
                Finding::new(NodeVerdict::Lighter, Some(height)).because(format!(
                    "its chain leaves the proven one after block {} and has no more work",
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
            .db(db, Store::proven_tip)
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
            Verdict::Invalid => self.caught(node, height, rejection.to_string()),
            Verdict::NotYet => {
                Finding::new(NodeVerdict::Ahead, Some(height)).because(rejection.to_string())
            }
            // Looked at again next round, not at once: a node can say this
            // every time.
            Verdict::Moved => Finding::new(NodeVerdict::Unknown, Some(height))
                .because("its chain moved while its blocks were checked"),
        }
    }

    fn caught(&self, node: &NodeRef<'_>, height: u64, why: String) -> Finding {
        shared::throttled!(
            format!("proof-caught:{}", node.label),
            warn,
            network = ?self.network,
            node = %node.label,
            why = %why,
            "a node served a block that breaks the proof-of-work rules"
        );
        Finding::new(NodeVerdict::Caught, Some(height)).because(why)
    }

    /// Excludes from scanning every node caught or found off the proven
    /// chain, until it is found on it again (a caught one, not before
    /// [`ProofTuning::caught_for`]). If that would be every node, only the
    /// caught ones are left out; if they are every node, none is.
    fn exclude(
        &mut self,
        client: &FallbackDaemonClient,
        nodes: &[NodeRef<'_>],
        findings: &[Finding],
    ) {
        let now = tokio::time::Instant::now();
        for (node, finding) in nodes.iter().zip(findings) {
            match finding.verdict {
                NodeVerdict::OnChain => {
                    let served_its_time = self.off_chain.get(node.label).is_none_or(|off| {
                        off.verdict != NodeVerdict::Caught
                            || now.duration_since(off.since) >= self.tuning.caught_for
                    });
                    if served_its_time {
                        self.off_chain.remove(node.label);
                    }
                }
                NodeVerdict::Caught | NodeVerdict::Lighter | NodeVerdict::Diverged => {
                    let since = match self.off_chain.get(node.label) {
                        // Caught again: its time starts again.
                        Some(off) if finding.verdict != NodeVerdict::Caught => off.since,
                        _ => now,
                    };
                    self.off_chain.insert(
                        node.label.to_owned(),
                        OffChain {
                            verdict: finding.verdict,
                            detail: finding.detail.clone().unwrap_or_default(),
                            since,
                        },
                    );
                }
                NodeVerdict::Ahead | NodeVerdict::Unknown | NodeVerdict::Unreachable => {}
            }
        }
        // Labels of nodes no longer configured are forgotten.
        self.off_chain
            .retain(|label, _| nodes.iter().any(|node| node.label == label));
        let indices = |caught_only: bool| -> Vec<usize> {
            nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| {
                    self.off_chain
                        .get(node.label)
                        .is_some_and(|off| !caught_only || off.verdict == NodeVerdict::Caught)
                })
                .map(|(i, _)| i)
                .collect()
        };
        if !client.set_excluded(&indices(false)) && !client.set_excluded(&indices(true)) {
            client.set_excluded(&[]);
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the round's facts, gathered once"
    )]
    fn describe(
        &self,
        client: &FallbackDaemonClient,
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
                "No node serves the proven chain or a valid heavier one. Nothing new settles until one does.".to_owned(),
            )
        } else {
            let excluded = (0..nodes.len()).filter(|&i| client.is_excluded(i)).count();
            let mut summary = format!(
                "Proven up to block {}; orders settle on blocks up to {}.",
                tip.height,
                ceiling.unwrap_or(0)
            );
            if excluded > 0 {
                let _ = write!(
                    summary,
                    " {excluded} node{} left out of scanning for serving another chain.",
                    if excluded == 1 { "" } else { "s" }
                );
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
                .enumerate()
                .map(|(i, (node, finding))| NodeProof {
                    node: node.label.to_owned(),
                    height: finding.height,
                    verdict: finding.verdict,
                    detail: finding
                        .detail
                        .or_else(|| self.off_chain.get(node.label).map(|off| off.detail.clone())),
                    excluded: client.is_excluded(i),
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
/// setting.
///
/// A round runs whenever its node announces a block or the poll interval
/// passes, at once while work is left. Each round reads whether checking is
/// on, so the setting applies from the next round; off, the `RandomX`
/// memory is freed.
#[expect(
    clippy::infinite_loop,
    reason = "a supervised loop: `shared::supervise` restarts one that returns"
)]
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
            // At once, but never a busy loop.
            tokio::time::sleep(Duration::from_millis(100)).await;
        } else {
            wakes.proof_or(poll).await;
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/verification/proof/tests.rs"]
pub(crate) mod tests;
