//! Chain agreement across a network's nodes (docs/chain_agreement.md).
//!
//! Each round pins one node and believes it. When more than one node is
//! configured, this asks every node for the blocks we recorded, so that a
//! node serving a made-up chain is outvoted rather than believed:
//!
//! - [`verdict`] and [`assess`] turn the nodes' answers into an [`Outcome`];
//! - [`AgreementState::next`] is the state machine: given an [`Input`], it
//!   returns the next state and the [`Effects`] to apply, and nothing else
//!   changes the state, the settlement ceiling or which nodes are excluded;
//! - [`step`] runs a check when one is due and applies what it decided.
//!
//! The pure parts are tested exhaustively below; `work::tests` drives the
//! whole thing against fake nodes.

use std::time::Duration;

use crate::daemon_fallback::FallbackDaemonClient;
use crate::store::db::Class;
use crate::store::Db;

/// How many recorded blocks a check looks at, from the highest down: an
/// honest fork is never this deep, so a chain no node agrees with anywhere
/// in it has left the majority's.
pub const WINDOW: u64 = 6;
/// A check runs when the recorded chain has grown, or after this long.
pub const RECHECK_SECS: i64 = 30;
/// How long no second node may answer before the ceiling is lifted rather
/// than every payment held for as long as fallbacks are down.
pub const UNVERIFIED_GRACE_SECS: i64 = 10 * 60;
/// One node's time to answer one question.
const NODE_DEADLINE: Duration = Duration::from_secs(5);
/// One whole check's time.
const CHECK_DEADLINE: Duration = Duration::from_secs(12);

/// One node's answer about one recorded block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Vote {
    /// It has the block we recorded at that height.
    Agree,
    /// It has another block there (its id).
    Disagree(String),
    /// No answer, or it doesn't have that height yet.
    Abstain,
}

/// What the votes about one block say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// At least two nodes have our block, and fewer have any other.
    Agreed,
    /// At least two nodes have the same other block, more than have ours.
    Outvoted,
    /// A disagreement neither side wins.
    Contested,
    /// Fewer than two nodes said anything.
    Unverified,
}

/// The verdict on one block. "At least two" means at least one node besides
/// the one that gave us the block: a node agreeing with itself proves
/// nothing.
pub fn verdict(votes: &[Vote]) -> Verdict {
    let agree = votes.iter().filter(|v| **v == Vote::Agree).count();
    let disagree = votes
        .iter()
        .filter(|v| matches!(v, Vote::Disagree(_)))
        .count();
    if agree + disagree < 2 {
        return Verdict::Unverified;
    }
    if agree >= 2 && agree > disagree {
        return Verdict::Agreed;
    }
    if largest_other(votes) >= 2 && largest_other(votes) > agree {
        return Verdict::Outvoted;
    }
    Verdict::Contested
}

/// The most nodes agreeing on one block other than ours.
fn largest_other(votes: &[Vote]) -> usize {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for vote in votes {
        if let Vote::Disagree(hash) = vote {
            match counts.iter_mut().find(|(h, _)| h == hash) {
                Some((_, n)) => *n += 1,
                None => counts.push((hash, 1)),
            }
        }
    }
    counts.into_iter().map(|(_, n)| n).max().unwrap_or(0)
}

fn voting(votes: &[Vote], wanted: impl Fn(&Vote) -> bool) -> Vec<usize> {
    votes
        .iter()
        .enumerate()
        .filter(|(_, v)| wanted(v))
        .map(|(idx, _)| idx)
        .collect()
}

/// What one check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every recorded block up to `height` is on the majority's chain.
    /// `off_chain`: the nodes not on the majority's chain, each with the
    /// lowest height it was shown off at: another block there, or outvoted
    /// on a block above it. `on_chain`: the nodes shown to be on it (they
    /// have that block, and lost nothing above). A node that said nothing
    /// is neither.
    Agreed {
        height: u64,
        off_chain: Vec<(usize, u64)>,
        on_chain: Vec<usize>,
    },
    /// No recorded block in the window is on the majority's chain, and at
    /// the deepest the majority has another: our chain left theirs at
    /// least that far back. `minority`: the nodes that have ours.
    Outvoted { height: u64, minority: Vec<usize> },
    /// Nodes disagree and no side wins.
    Contested { height: u64 },
    /// No block got two opinions.
    Unverified,
}

/// The outcome of asking about recorded blocks from the highest down,
/// `tried` in that order, stopping at the first [`Verdict::Agreed`].
pub fn assess(tried: &[(u64, Vec<Vote>)]) -> Outcome {
    let Some((deepest, votes)) = tried.last() else {
        return Outcome::Unverified;
    };
    match verdict(votes) {
        Verdict::Agreed => {
            // Ours lost above (an outvoted block, then an agreed one below
            // it): the nodes that have our losing blocks are off the
            // majority's chain too. For an honest fork that only pins a
            // majority node until the other catches up.
            let mut off_chain: Vec<(usize, u64)> =
                voting(votes, |v| matches!(v, Vote::Disagree(_)))
                    .into_iter()
                    .map(|idx| (idx, *deepest))
                    .collect();
            for (height, above) in &tried[..tried.len() - 1] {
                if verdict(above) == Verdict::Outvoted {
                    for idx in voting(above, |v| *v == Vote::Agree) {
                        match off_chain.iter_mut().find(|(node, _)| *node == idx) {
                            Some((_, at)) => *at = (*at).min(*height),
                            None => off_chain.push((idx, *height)),
                        }
                    }
                }
            }
            off_chain.sort_unstable();
            let on_chain = voting(votes, |v| *v == Vote::Agree)
                .into_iter()
                .filter(|idx| !off_chain.iter().any(|(node, _)| node == idx))
                .collect();
            Outcome::Agreed {
                height: *deepest,
                off_chain,
                on_chain,
            }
        }
        Verdict::Outvoted => Outcome::Outvoted {
            height: *deepest,
            minority: voting(votes, |v| *v == Vote::Agree),
        },
        Verdict::Contested | Verdict::Unverified => {
            match tried
                .iter()
                .find(|(_, votes)| verdict(votes) != Verdict::Unverified)
            {
                Some((height, _)) => Outcome::Contested { height: *height },
                None => Outcome::Unverified,
            }
        }
    }
}

/// Why settlement is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    Contested,
    /// The nodes with our chain are outvoted, and excluded.
    Outvoted,
    /// No second node answers (yet).
    Unverified,
}

/// One network's agreement. Each state carries what it needs, so a
/// ceiling without an agreed height can't be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgreementState {
    /// One node: nothing to compare with. No ceiling.
    Single,
    /// More than one node, no outcome yet. The stored ceiling stays.
    Checking,
    /// The majority has our chain up to `height`: the ceiling.
    Agreed { height: u64, since: i64 },
    /// Settlement held at the last agreed height (or entirely, without one).
    Holding {
        hold: Hold,
        since: i64,
        last_agreed: Option<u64>,
    },
    /// No second node answered for [`UNVERIFIED_GRACE_SECS`]: the ceiling is
    /// lifted, trusting the pinned node as with one node.
    Degraded { since: i64 },
}

/// What a transition is given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    /// The network has one node (or none).
    OneNode,
    Checked(Outcome),
}

/// What to do to the stored ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CeilingWrite {
    Keep,
    Set(u64),
    /// Keep it; if there is none, hold every settlement (ceiling 0).
    KeepOrHoldAll,
    /// No ceiling.
    Clear,
}

/// What a transition does to the stored ceiling. (Which nodes are left out
/// is [`next_excluded`].)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effects {
    pub ceiling: CeilingWrite,
}

/// The nodes left out of pinning, each with the lowest height it was shown
/// off the majority's chain at.
pub type Excluded = std::collections::BTreeMap<usize, u64>;

/// Who is left out after `input`. A node is let back in only on evidence:
/// it has the majority's block at or above the height it was caught at.
/// Saying nothing proves nothing (a node serving a shorter made-up chain
/// has nothing to say about the real tip), and agreeing below the fork
/// proves nothing either (every chain shares the blocks before it).
pub fn next_excluded(excluded: &Excluded, input: &Input) -> Excluded {
    let mut next = excluded.clone();
    let mut catch = |node: usize, at: u64| {
        next.entry(node)
            .and_modify(|lowest| *lowest = (*lowest).min(at))
            .or_insert(at);
    };
    match input {
        Input::OneNode => return Excluded::new(),
        Input::Checked(Outcome::Agreed {
            height,
            off_chain,
            on_chain,
        }) => {
            for &(node, at) in off_chain {
                catch(node, at);
            }
            next.retain(|node, caught| !(on_chain.contains(node) && *height >= *caught));
        }
        Input::Checked(Outcome::Outvoted { height, minority }) => {
            for &node in minority {
                catch(node, *height);
            }
        }
        Input::Checked(Outcome::Contested { .. } | Outcome::Unverified) => {}
    }
    next
}

impl AgreementState {
    /// The next state and what to do on the way. Pure, and total over
    /// every state and input.
    pub fn next(&self, input: &Input, now: i64) -> (AgreementState, Effects) {
        use AgreementState::*;
        let held = |hold: Hold| {
            let since = match self {
                Holding {
                    hold: was, since, ..
                } if *was == hold => *since,
                _ => now,
            };
            Holding {
                hold,
                since,
                last_agreed: self.last_agreed(),
            }
        };
        match input {
            Input::OneNode => (
                Single,
                Effects {
                    ceiling: CeilingWrite::Clear,
                },
            ),
            Input::Checked(Outcome::Agreed { height, .. }) => (
                Agreed {
                    height: *height,
                    since: match self {
                        Agreed { since, .. } => *since,
                        _ => now,
                    },
                },
                Effects {
                    ceiling: CeilingWrite::Set(*height),
                },
            ),
            Input::Checked(Outcome::Outvoted { .. }) => (
                held(Hold::Outvoted),
                Effects {
                    ceiling: CeilingWrite::KeepOrHoldAll,
                },
            ),
            Input::Checked(Outcome::Contested { .. }) => (
                held(Hold::Contested),
                Effects {
                    ceiling: CeilingWrite::KeepOrHoldAll,
                },
            ),
            Input::Checked(Outcome::Unverified) => match self {
                Degraded { since } => (
                    Degraded { since: *since },
                    Effects {
                        ceiling: CeilingWrite::Clear,
                    },
                ),
                Holding {
                    hold: Hold::Unverified,
                    since,
                    ..
                } if now - since >= UNVERIFIED_GRACE_SECS => (
                    Degraded { since: now },
                    Effects {
                        ceiling: CeilingWrite::Clear,
                    },
                ),
                _ => (
                    held(Hold::Unverified),
                    Effects {
                        ceiling: CeilingWrite::KeepOrHoldAll,
                    },
                ),
            },
        }
    }

    fn last_agreed(&self) -> Option<u64> {
        match self {
            AgreementState::Agreed { height, .. } => Some(*height),
            AgreementState::Holding { last_agreed, .. } => *last_agreed,
            _ => None,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            AgreementState::Single => "single",
            AgreementState::Checking => "checking",
            AgreementState::Agreed { .. } => "agreed",
            AgreementState::Holding { .. } => "holding",
            AgreementState::Degraded { .. } => "degraded",
        }
    }

    /// For `/status`.
    fn report(&self) -> (&'static str, Option<&'static str>, Option<i64>) {
        match self {
            AgreementState::Single | AgreementState::Checking => (self.name(), None, None),
            AgreementState::Agreed { since, .. } | AgreementState::Degraded { since } => {
                (self.name(), None, Some(*since))
            }
            AgreementState::Holding { hold, since, .. } => (
                self.name(),
                Some(match hold {
                    Hold::Contested => "contested",
                    Hold::Outvoted => "outvoted",
                    Hold::Unverified => "unverified",
                }),
                Some(*since),
            ),
        }
    }
}

/// One network's agreement as it stands, kept across rounds and shared with
/// `/status`.
#[derive(Debug)]
pub struct ChainAgreement {
    inner: parking_lot::Mutex<Tracked>,
}

#[derive(Debug)]
struct Tracked {
    state: AgreementState,
    /// When the last check ran, and the recorded chain's height then.
    last_check: Option<(i64, Option<u64>)>,
    /// Each node's vote on the deciding block, with its label.
    votes: Vec<(String, Vote)>,
    decided_at: Option<u64>,
    ceiling: Option<u64>,
    /// Who is left out (`next_excluded`), for the node list `labels`: a
    /// changed list is a new client, which excludes nobody.
    excluded: Excluded,
    labels: Vec<String>,
}

impl Default for ChainAgreement {
    fn default() -> Self {
        ChainAgreement {
            inner: parking_lot::Mutex::new(Tracked {
                state: AgreementState::Checking,
                last_check: None,
                votes: Vec::new(),
                decided_at: None,
                ceiling: None,
                excluded: Excluded::new(),
                labels: Vec::new(),
            }),
        }
    }
}

impl ChainAgreement {
    pub fn state(&self) -> AgreementState {
        self.inner.lock().state.clone()
    }

    /// For `/status`: `None` with one node.
    pub fn report(&self, nodes: &FallbackDaemonClient) -> Option<shared::agreement::Agreement> {
        let tracked = self.inner.lock();
        if tracked.state == AgreementState::Single {
            return None;
        }
        let (state, hold, since) = tracked.state.report();
        Some(shared::agreement::Agreement {
            state: state.to_string(),
            hold: hold.map(str::to_string),
            since,
            ceiling: tracked.ceiling,
            checked_at: tracked.last_check.map(|(at, _)| at),
            height: tracked.decided_at,
            nodes: tracked
                .votes
                .iter()
                .enumerate()
                .map(|(idx, (label, vote))| shared::agreement::NodeVote {
                    node: label.clone(),
                    vote: match vote {
                        Vote::Agree => "agrees",
                        Vote::Disagree(_) => "disagrees",
                        Vote::Abstain => "no answer",
                    }
                    .to_string(),
                    excluded: nodes.is_excluded(idx),
                })
                .collect(),
        })
    }
}

/// Where a check starts: the highest recorded block that at least two
/// nodes can speak to (the second-highest node tip), or the recorded top if
/// lower. A chain served ahead of every other node is compared where the
/// others are, so it can't pass as merely unverified. `None`: fewer than two
/// nodes answered.
pub fn start_height(high_water: u64, tips: &[Option<u64>]) -> Option<u64> {
    let mut known: Vec<u64> = tips.iter().flatten().copied().collect();
    known.sort_unstable_by(|a, b| b.cmp(a));
    known.get(1).map(|second| high_water.min(*second))
}

/// Asks every node for its tip, then about the recorded blocks from
/// [`start_height`] down (at most [`WINDOW`]), stopping at the first the
/// majority agrees on. `None` while nothing is recorded. Each attempt is
/// returned with its votes: none when nothing can be compared.
async fn check(
    db: &Db,
    network: monero::Network,
    nodes: &FallbackDaemonClient,
) -> Result<Option<(u64, Vec<(u64, Vec<Vote>)>)>, crate::scanner::ScannerError> {
    let Some(high_water) = db
        .run(Class::Scanner, move |s| s.max_scanned_height(network))
        .await?
    else {
        return Ok(None);
    };
    let tips = futures_util::future::join_all(nodes.nodes().iter().map(|node| async move {
        tokio::time::timeout(NODE_DEADLINE, node.client.get_height())
            .await
            .ok()
            .and_then(Result::ok)
    }))
    .await;
    let Some(start) = start_height(high_water, &tips) else {
        return Ok(Some((high_water, Vec::new())));
    };
    let rows = db
        .run(Class::Scanner, move |s| {
            s.scanned_blocks_between(network, start.saturating_sub(WINDOW - 1), start)
        })
        .await?;
    let mut tried = Vec::new();
    for (height, ours) in rows.into_iter().rev() {
        let votes = futures_util::future::join_all(nodes.nodes().iter().map(|node| {
            let ours = &ours;
            async move {
                match tokio::time::timeout(NODE_DEADLINE, node.client.get_block_hash(height)).await
                {
                    Ok(Ok(hash)) if hash == *ours => Vote::Agree,
                    Ok(Ok(hash)) => Vote::Disagree(hash),
                    _ => Vote::Abstain,
                }
            }
        }))
        .await;
        let agreed = verdict(&votes) == Verdict::Agreed;
        tried.push((height, votes));
        if agreed {
            break;
        }
    }
    Ok(Some((high_water, tried)))
}

/// Runs a check if one is due on a network with more than one node, and
/// applies what the state machine decides. A check that can't finish
/// changes nothing.
pub(crate) async fn step(
    agreement: &ChainAgreement,
    db: &Db,
    network: monero::Network,
    nodes: &FallbackDaemonClient,
    now: i64,
) {
    if nodes.nodes().len() <= 1 {
        if agreement.state() != AgreementState::Single {
            apply(agreement, db, network, nodes, &Input::OneNode, now, None).await;
        }
        return;
    }
    let high_water = db
        .run(Class::Scanner, move |s| s.max_scanned_height(network))
        .await
        .ok()
        .flatten();
    // Every round while settlement is held, nothing is decided yet, or the
    // agreed height is below the recorded top (a node catching up): a hold
    // ends, and the ceiling rises, as soon as the nodes agree.
    let due = {
        let tracked = agreement.inner.lock();
        match (&tracked.state, tracked.last_check) {
            (_, None) => true,
            (AgreementState::Agreed { height, .. }, Some((at, checked))) => {
                Some(*height) < high_water || checked != high_water || now - at >= RECHECK_SECS
            }
            _ => true,
        }
    };
    if !due {
        return;
    }
    let checked = match tokio::time::timeout(CHECK_DEADLINE, check(db, network, nodes)).await {
        Ok(Ok(checked)) => checked,
        Ok(Err(error)) => {
            tracing::warn!(network = ?network, error = %error, "checking that the nodes agree failed (retried)");
            return;
        }
        Err(_) => return,
    };
    // Nothing recorded yet is nothing compared: unverified, which holds
    // settlement until a check can compare (the round about to record the
    // first blocks mustn't settle on them unchecked).
    let (high_water, tried) = match checked {
        Some((high_water, tried)) => (Some(high_water), tried),
        None => (None, Vec::new()),
    };
    let outcome = assess(&tried);
    let decided = tried.last().map(|(height, votes)| {
        let labels = nodes.nodes().iter().map(|n| n.label.clone());
        (*height, labels.zip(votes.iter().cloned()).collect())
    });
    if apply(
        agreement,
        db,
        network,
        nodes,
        &Input::Checked(outcome),
        now,
        decided,
    )
    .await
    {
        agreement.inner.lock().last_check = Some((now, high_water));
    }
}

/// Applies one transition: the ceiling first (a failed write changes
/// nothing, and the check is retried), then the exclusion, then the state.
async fn apply(
    agreement: &ChainAgreement,
    db: &Db,
    network: monero::Network,
    nodes: &FallbackDaemonClient,
    input: &Input,
    now: i64,
    decided: Option<(u64, Vec<(String, Vote)>)>,
) -> bool {
    let current = agreement.state();
    let (next, effects) = current.next(input, now);
    let write = effects.ceiling;
    let ceiling = db
        .run(Class::Scanner, move |s| {
            s.write_settlement_ceiling(network, write, now)?;
            s.settlement_ceiling(network)
        })
        .await;
    let ceiling = match ceiling {
        Ok(ceiling) => ceiling,
        Err(error) => {
            tracing::warn!(network = ?network, error = %error, "recording the nodes' agreement failed (retried)");
            return false;
        }
    };
    let labels: Vec<String> = nodes.nodes().iter().map(|n| n.label.clone()).collect();
    let excluded = {
        let tracked = agreement.inner.lock();
        let before = if tracked.labels == labels {
            tracked.excluded.clone()
        } else {
            Excluded::new()
        };
        next_excluded(&before, input)
    };
    // Never every node: then the exclusion stays as it was.
    let excluded = if nodes.set_excluded(&excluded.keys().copied().collect::<Vec<_>>()) {
        Some(excluded)
    } else {
        None
    };
    if std::mem::discriminant(&next) != std::mem::discriminant(&current)
        || matches!((&next, &current), (AgreementState::Holding { hold: a, .. }, AgreementState::Holding { hold: b, .. }) if a != b)
    {
        log_transition(network, &next, ceiling);
    }
    let mut tracked = agreement.inner.lock();
    tracked.state = next;
    tracked.ceiling = ceiling;
    if let Some(excluded) = excluded {
        tracked.excluded = excluded;
        tracked.labels = labels;
    }
    if let Some((height, votes)) = decided {
        tracked.decided_at = Some(height);
        tracked.votes = votes;
    }
    true
}

fn log_transition(network: monero::Network, state: &AgreementState, ceiling: Option<u64>) {
    match state {
        AgreementState::Single | AgreementState::Checking => {}
        AgreementState::Agreed { height, .. } => {
            tracing::info!(network = ?network, height, "the nodes agree on the recorded chain");
        }
        AgreementState::Holding { hold, .. } => {
            tracing::warn!(network = ?network, hold = ?hold, ceiling = ?ceiling, "the nodes don't agree on the recorded chain: no order settles above the last agreed block");
        }
        AgreementState::Degraded { .. } => {
            tracing::warn!(network = ?network, "no other node has answered for 10 minutes: trusting the pinned node alone, as with one node");
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use Vote::*;

    fn d(hash: &str) -> Vote {
        Disagree(hash.to_string())
    }

    #[test]
    fn a_verdict_needs_two_opinions_and_a_winner() {
        let cases: &[(&[Vote], Verdict)] = &[
            (&[], Verdict::Unverified),
            (&[Agree], Verdict::Unverified),
            (&[Agree, Abstain], Verdict::Unverified),
            (&[d("y"), Abstain, Abstain], Verdict::Unverified),
            (&[Agree, Agree], Verdict::Agreed),
            (&[Agree, d("y")], Verdict::Contested),
            (&[Agree, Agree, d("y")], Verdict::Agreed),
            (&[Agree, d("y"), d("y")], Verdict::Outvoted),
            (&[Agree, d("y"), d("z")], Verdict::Contested),
            (&[Agree, Agree, d("y"), d("y")], Verdict::Contested),
            (&[Agree, Agree, d("y"), d("y"), d("y")], Verdict::Outvoted),
            (&[Agree, Agree, d("y"), d("z"), Abstain], Verdict::Contested),
            (&[Agree, Agree, Agree, d("y"), d("y")], Verdict::Agreed),
            (&[Abstain, d("y"), d("y")], Verdict::Outvoted),
            (&[Abstain, Agree, Agree, Abstain, Abstain], Verdict::Agreed),
        ];
        for (votes, expected) in cases {
            assert_eq!(verdict(votes), *expected, "{votes:?}");
        }
    }

    /// Every mix of votes from one to five nodes: the verdict's rules hold.
    #[test]
    fn every_vote_mix_for_up_to_five_nodes_keeps_the_rules() {
        let choices = [Agree, d("y"), d("z"), Abstain];
        for n in 1..=5u32 {
            for code in 0..4usize.pow(n) {
                let votes: Vec<Vote> = (0..n)
                    .map(|i| choices[(code / 4usize.pow(i)) % 4].clone())
                    .collect();
                let agree = votes.iter().filter(|v| **v == Agree).count();
                let disagree = votes.iter().filter(|v| matches!(v, Disagree(_))).count();
                match verdict(&votes) {
                    Verdict::Agreed => assert!(agree >= 2 && agree > disagree, "{votes:?}"),
                    Verdict::Outvoted => {
                        assert!(
                            largest_other(&votes) >= 2 && largest_other(&votes) > agree,
                            "{votes:?}"
                        )
                    }
                    Verdict::Contested => {
                        assert!(disagree > 0 && agree + disagree >= 2, "{votes:?}")
                    }
                    Verdict::Unverified => assert!(agree + disagree < 2, "{votes:?}"),
                }
                // One node never decides alone.
                assert!(n > 1 || verdict(&votes) == Verdict::Unverified, "{votes:?}");
            }
        }
    }

    #[test]
    fn a_check_starts_where_at_least_two_nodes_have_blocks() {
        assert_eq!(
            start_height(100, &[Some(100), Some(100), Some(99)]),
            Some(100)
        );
        assert_eq!(
            start_height(100, &[Some(100), Some(99)]),
            Some(99),
            "one behind"
        );
        assert_eq!(
            start_height(98, &[Some(100), Some(100)]),
            Some(98),
            "our record is lower"
        );
        // A chain served far ahead of the others is compared where they are.
        assert_eq!(
            start_height(140, &[Some(140), Some(100), Some(101)]),
            Some(101)
        );
        assert_eq!(
            start_height(100, &[Some(100), None]),
            None,
            "nobody to compare with"
        );
        assert_eq!(start_height(100, &[None, None, Some(7)]), None);
    }

    #[test]
    fn a_check_s_outcome_comes_from_the_heights_it_tried() {
        // A fresh honest fork: our top block lost, the one below agreed. The
        // node with our lost block is left out until it catches up.
        assert_eq!(
            assess(&[
                (10, vec![Agree, d("y"), d("y")]),
                (9, vec![Agree, Agree, Agree])
            ]),
            Outcome::Agreed {
                height: 9,
                off_chain: vec![(0, 10)],
                on_chain: vec![1, 2]
            }
        );
        // A node a block behind abstains at the top.
        assert_eq!(
            assess(&[(10, vec![Agree, Abstain]), (9, vec![Agree, Agree])]),
            Outcome::Agreed {
                height: 9,
                off_chain: vec![],
                on_chain: vec![0, 1]
            }
        );
        // A node with nothing to say there (an outvoted node whose made-up
        // chain is shorter) is neither on nor off: it isn't let back in.
        assert_eq!(
            assess(&[(16, vec![Abstain, Agree, Agree])]),
            Outcome::Agreed {
                height: 16,
                off_chain: vec![],
                on_chain: vec![1, 2]
            }
        );
        // A dissenter at the agreed height is named.
        assert_eq!(
            assess(&[(10, vec![Agree, Agree, d("y")])]),
            Outcome::Agreed {
                height: 10,
                off_chain: vec![(2, 10)],
                on_chain: vec![0, 1]
            }
        );
        // Outvoted all the way down: the nodes with our chain are named.
        let outvoted: Vec<(u64, Vec<Vote>)> = (5..=10)
            .rev()
            .map(|h| (h, vec![Agree, d("y"), d("y")]))
            .collect();
        assert_eq!(
            assess(&outvoted),
            Outcome::Outvoted {
                height: 5,
                minority: vec![0]
            }
        );
        // Two nodes split: contested, at the highest disputed block.
        let split: Vec<(u64, Vec<Vote>)> =
            (5..=10).rev().map(|h| (h, vec![Agree, d("y")])).collect();
        assert_eq!(assess(&split), Outcome::Contested { height: 10 });
        // Outvoted at the top, nobody answering deeper: contested.
        assert_eq!(
            assess(&[
                (10, vec![Agree, d("y"), d("y")]),
                (9, vec![Agree, Abstain, Abstain])
            ]),
            Outcome::Contested { height: 10 }
        );
        // Nobody else answers.
        assert_eq!(
            assess(&[(10, vec![Agree, Abstain]), (9, vec![Agree, Abstain])]),
            Outcome::Unverified
        );
        assert_eq!(assess(&[]), Outcome::Unverified);
    }

    fn effects(ceiling: CeilingWrite) -> Effects {
        Effects { ceiling }
    }

    #[test]
    fn a_node_is_let_back_in_only_on_evidence_from_where_it_was_caught() {
        let agreed = |height, off_chain: Vec<(usize, u64)>, on_chain: Vec<usize>| {
            Input::Checked(Outcome::Agreed {
                height,
                off_chain,
                on_chain,
            })
        };
        // Caught at 3 (outvoted on its made-up block there).
        let excluded = next_excluded(&Excluded::new(), &agreed(2, vec![(0, 3)], vec![1, 2]));
        assert_eq!(excluded, Excluded::from([(0, 3)]));
        // Agreeing below where it was caught (blocks every chain shares)
        // proves nothing.
        let excluded = next_excluded(&excluded, &agreed(2, vec![], vec![0, 1, 2]));
        assert_eq!(excluded, Excluded::from([(0, 3)]));
        // Saying nothing proves nothing.
        let excluded = next_excluded(&excluded, &agreed(16, vec![], vec![1, 2]));
        assert_eq!(excluded, Excluded::from([(0, 3)]));
        // Caught lower still: the lowest is kept.
        let excluded = next_excluded(
            &excluded,
            &Input::Checked(Outcome::Outvoted {
                height: 2,
                minority: vec![0],
            }),
        );
        assert_eq!(excluded, Excluded::from([(0, 2)]));
        // Neither a dispute nor silence changes anything.
        for input in [
            Input::Checked(Outcome::Contested { height: 20 }),
            Input::Checked(Outcome::Unverified),
        ] {
            assert_eq!(next_excluded(&excluded, &input), excluded);
        }
        // It has the majority's block at or above where it was caught: back in.
        let excluded = next_excluded(&excluded, &agreed(17, vec![], vec![0, 1, 2]));
        assert_eq!(excluded, Excluded::new());
        // One node: nobody excluded.
        let caught = Excluded::from([(1, 5)]);
        assert_eq!(next_excluded(&caught, &Input::OneNode), Excluded::new());
    }

    #[test]
    fn a_node_caught_above_the_agreed_block_is_named_with_that_height() {
        assert_eq!(
            assess(&[
                (12, vec![Agree, d("y"), d("y")]),
                (11, vec![Agree, d("z"), d("z")]),
                (10, vec![Agree, Agree, d("w")]),
            ]),
            Outcome::Agreed {
                height: 10,
                off_chain: vec![(0, 11), (2, 10)],
                on_chain: vec![1],
            }
        );
    }

    #[test]
    fn every_state_goes_where_each_input_says() {
        use AgreementState::*;
        let agreed = Input::Checked(Outcome::Agreed {
            height: 100,
            off_chain: vec![(2, 100)],
            on_chain: vec![0, 1],
        });
        let outvoted = Input::Checked(Outcome::Outvoted {
            height: 95,
            minority: vec![0],
        });
        let contested = Input::Checked(Outcome::Contested { height: 100 });
        let unverified = Input::Checked(Outcome::Unverified);
        let states = [
            Single,
            Checking,
            Agreed {
                height: 90,
                since: 10,
            },
            Holding {
                hold: Hold::Contested,
                since: 10,
                last_agreed: Some(90),
            },
            Holding {
                hold: Hold::Outvoted,
                since: 10,
                last_agreed: None,
            },
            Holding {
                hold: Hold::Unverified,
                since: 10,
                last_agreed: Some(90),
            },
            Degraded { since: 10 },
        ];
        let now = 1_000;
        for state in &states {
            let last = state.last_agreed();
            // One node: no ceiling, from anywhere.
            assert_eq!(
                state.next(&Input::OneNode, now),
                (Single, effects(CeilingWrite::Clear)),
                "{state:?}"
            );
            // Agreement sets the ceiling.
            let (next, fx) = state.next(&agreed, now);
            assert!(matches!(next, Agreed { height: 100, .. }), "{state:?}");
            assert_eq!(fx, effects(CeilingWrite::Set(100)));
            // Outvoted holds at the last agreed height.
            let (next, fx) = state.next(&outvoted, now);
            assert!(
                matches!(next, Holding { hold: Hold::Outvoted, last_agreed, .. } if last_agreed == last),
                "{state:?}"
            );
            assert_eq!(fx, effects(CeilingWrite::KeepOrHoldAll));
            // Contested holds.
            let (next, fx) = state.next(&contested, now);
            assert!(
                matches!(
                    next,
                    Holding {
                        hold: Hold::Contested,
                        ..
                    }
                ),
                "{state:?}"
            );
            assert_eq!(fx, effects(CeilingWrite::KeepOrHoldAll));
            // Unverified holds, unless already degraded or past its grace.
            let (next, fx) = state.next(&unverified, now);
            match state {
                Degraded { .. } => {
                    assert_eq!(next, *state);
                    assert_eq!(fx, effects(CeilingWrite::Clear));
                }
                Holding {
                    hold: Hold::Unverified,
                    ..
                } => {
                    assert_eq!(next, Degraded { since: now }, "990 s past its start");
                    assert_eq!(fx, effects(CeilingWrite::Clear));
                }
                _ => {
                    assert!(
                        matches!(next, Holding { hold: Hold::Unverified, since, .. } if since == now)
                    );
                    assert_eq!(fx, effects(CeilingWrite::KeepOrHoldAll));
                }
            }
        }
    }

    #[test]
    fn an_unverified_hold_lasts_its_grace_then_degrades_until_agreement() {
        use AgreementState::*;
        let unverified = Input::Checked(Outcome::Unverified);
        let (state, _) = Agreed {
            height: 50,
            since: 0,
        }
        .next(&unverified, 100);
        assert_eq!(
            state,
            Holding {
                hold: Hold::Unverified,
                since: 100,
                last_agreed: Some(50)
            }
        );
        let (state, fx) = state.next(&unverified, 100 + UNVERIFIED_GRACE_SECS - 1);
        assert_eq!(
            state,
            Holding {
                hold: Hold::Unverified,
                since: 100,
                last_agreed: Some(50)
            },
            "keeps its start"
        );
        assert_eq!(fx.ceiling, CeilingWrite::KeepOrHoldAll);
        let (state, fx) = state.next(&unverified, 100 + UNVERIFIED_GRACE_SECS);
        assert_eq!(
            state,
            Degraded {
                since: 100 + UNVERIFIED_GRACE_SECS
            }
        );
        assert_eq!(fx.ceiling, CeilingWrite::Clear);
        // A node that disagrees while degraded holds everything again.
        let (held, fx) = state.next(&Input::Checked(Outcome::Contested { height: 60 }), 2_000);
        assert_eq!(
            held,
            Holding {
                hold: Hold::Contested,
                since: 2_000,
                last_agreed: None
            }
        );
        assert_eq!(fx.ceiling, CeilingWrite::KeepOrHoldAll);
        let (state, _) = held.next(
            &Input::Checked(Outcome::Agreed {
                height: 61,
                off_chain: vec![],
                on_chain: vec![],
            }),
            2_100,
        );
        assert_eq!(
            state,
            Agreed {
                height: 61,
                since: 2_100
            }
        );
    }

    #[test]
    fn a_contested_or_outvoted_hold_never_degrades_and_keeps_its_start() {
        use AgreementState::*;
        let contested = Input::Checked(Outcome::Contested { height: 10 });
        let (state, _) = Checking.next(&contested, 0);
        let (state, fx) = state.next(&contested, 10 * UNVERIFIED_GRACE_SECS);
        assert_eq!(
            state,
            Holding {
                hold: Hold::Contested,
                since: 0,
                last_agreed: None
            }
        );
        assert_eq!(fx.ceiling, CeilingWrite::KeepOrHoldAll);
        let (state, _) = state.next(
            &Input::Checked(Outcome::Outvoted {
                height: 5,
                minority: vec![1],
            }),
            50,
        );
        assert_eq!(
            state,
            Holding {
                hold: Hold::Outvoted,
                since: 50,
                last_agreed: None
            },
            "a new reason, a new start"
        );
    }
}
