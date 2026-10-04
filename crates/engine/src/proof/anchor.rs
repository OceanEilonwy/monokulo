//! Taking an anchor (`docs/proof_of_work.md`).
//!
//! The anchor is the one block, with the 735 before it, that the engine
//! takes on its nodes' word, deep enough below their tips that no reorg
//! reaches it. Every block after it is checked.
//!
//! What a block's id commits to (its timestamp, its parent) a node can't
//! change, but a block's difficulty isn't in it: it is computed from the
//! blocks before, back to genesis. So the window's claimed difficulties are
//! the one thing taken on trust, and they are hedged three ways:
//!
//! - a majority of the configured nodes must give the same window, row for
//!   row (one node: its word);
//! - every claimed difficulty must be at least the network's floor
//!   ([`ProofTuning::min_difficulty`]), and the difficulty the
//!   window gives the block after the anchor must be the one the nodes
//!   claim for it;
//! - a random sample of the window's blocks must have a proof of work that
//!   meets the difficulty claimed for it, so a made-up window costs real
//!   work at least the floor, block for block.

use std::collections::{BTreeMap, BTreeSet};

use futures_util::future::join_all;

use super::{NodeRef, ProofTuning};
use crate::daemon::DifficultyHeader;
use crate::pow::hasher::Hasher;
use crate::pow::{self, ProvenBlock, DIFFICULTY_BLOCKS};
use crate::store::proof::NewAnchor;

/// Why no anchor could be taken yet. Every one is retried next round.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnchorProblem {
    #[error("{answered} of {total} nodes answered; {needed} must agree")]
    TooFewAnswered {
        answered: usize,
        needed: usize,
        total: usize,
    },
    #[error("the nodes' chain (height {height}) is too short to anchor {depth} blocks below its tip after RandomX began at {fork}")]
    ChainTooShort { height: u64, depth: u64, fork: u64 },
    #[error("at most {largest} of {total} nodes gave the same blocks; {needed} must agree")]
    NoMajority {
        largest: usize,
        needed: usize,
        total: usize,
    },
    #[error("the blocks the nodes agree on break a rule: {0}")]
    Inconsistent(String),
    #[error("block {height} of the agreed window failed its check: {reason}")]
    SampleFailed { height: u64, reason: String },
    #[error("{0}")]
    Failed(String),
}

/// The window as proven blocks, and the `RandomX` keys below it.
type CheckedWindow = (Vec<ProvenBlock>, Vec<(u64, [u8; 32])>);

/// What one node said the window is.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Answer {
    /// The window's headers and the block after it.
    rows: Vec<DifficultyHeader>,
    /// `RandomX` key blocks below the window: (height, id).
    keys: Vec<(u64, String)>,
}

/// Takes an anchor from `nodes` (every configured node of `network`).
pub async fn take(
    nodes: &[NodeRef<'_>],
    network: monero::Network,
    tuning: &ProofTuning,
    hasher: &Hasher,
    now: i64,
) -> Result<NewAnchor, AnchorProblem> {
    let total = nodes.len();
    let needed = total / 2 + 1;
    let heights = join_all(
        nodes
            .iter()
            .map(|node| tuning.bounded(node.client.get_height())),
    )
    .await;
    let mut heights: Vec<u64> = heights.into_iter().filter_map(Result::ok).collect();
    if heights.len() < needed {
        return Err(AnchorProblem::TooFewAnswered {
            answered: heights.len(),
            needed,
            total,
        });
    }
    // The highest block at least `needed` nodes have: a node far ahead (or
    // claiming to be) can't choose the anchor.
    heights.sort_unstable_by(|a, b| b.cmp(a));
    let base = heights[needed - 1];
    let fork = pow::randomx_fork_height(network);
    let window_len = DIFFICULTY_BLOCKS as u64;
    let anchor = base.saturating_sub(tuning.anchor_depth);
    let start = (anchor + 1).saturating_sub(window_len);
    if anchor < window_len || start < fork {
        return Err(AnchorProblem::ChainTooShort {
            height: base,
            depth: tuning.anchor_depth,
            fork,
        });
    }
    let key_heights: BTreeSet<u64> = (start..=anchor + 1)
        .map(pow::seed_height)
        .filter(|h| *h < start)
        .collect();

    let answers = join_all(
        nodes
            .iter()
            .map(|node| ask(node, start, anchor + 1, &key_heights, tuning)),
    )
    .await;
    // Identical answers, grouped; the largest group wins if it is a
    // majority of the configured nodes.
    let mut groups: Vec<(Answer, Vec<usize>)> = Vec::new();
    for (index, answer) in answers.into_iter().enumerate() {
        let Ok(answer) = answer else { continue };
        match groups.iter_mut().find(|(seen, _)| *seen == answer) {
            Some((_, members)) => members.push(index),
            None => groups.push((answer, vec![index])),
        }
    }
    let Some((answer, members)) = groups.into_iter().max_by_key(|(_, m)| m.len()) else {
        return Err(AnchorProblem::TooFewAnswered {
            answered: 0,
            needed,
            total,
        });
    };
    if members.len() < needed {
        return Err(AnchorProblem::NoMajority {
            largest: members.len(),
            needed,
            total,
        });
    }

    let (window, seeds) = check_answer(&answer, start, anchor, tuning.min_difficulty(network))?;
    check_dates(&window, tuning.anchor_depth, now)?;
    sample(nodes, &members, &answer, &window, &seeds, tuning, hasher).await?;
    Ok(NewAnchor {
        agreed: u32::try_from(members.len()).unwrap_or(u32::MAX),
        nodes: u32::try_from(total).unwrap_or(u32::MAX),
        window,
        seeds,
    })
}

/// One node's window `start..=last` and key ids.
async fn ask(
    node: &NodeRef<'_>,
    start: u64,
    last: u64,
    key_heights: &BTreeSet<u64>,
    tuning: &ProofTuning,
) -> Result<Answer, String> {
    let mut rows: Vec<DifficultyHeader> = Vec::new();
    let mut next = start;
    while next <= last {
        let got = tuning
            .bounded(node.client.get_difficulty_headers(next, last - next + 1))
            .await?;
        if got.is_empty() {
            return Err(format!("no headers from {next}"));
        }
        next += got.len() as u64;
        rows.extend(got);
    }
    rows.truncate((last - start + 1) as usize);
    let mut keys = Vec::new();
    for &height in key_heights {
        keys.push((
            height,
            tuning.bounded(node.client.get_block_hash(height)).await?,
        ));
    }
    Ok(Answer { rows, keys })
}

fn id_of(hex_id: &str) -> Result<[u8; 32], AnchorProblem> {
    hex::decode(hex_id)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| AnchorProblem::Inconsistent(format!("{hex_id:?} isn't a block id")))
}

/// The window's dates against the clock (`now`, unix seconds): the anchor
/// between a quarter and four times `depth` blocks' time old, and the window
/// spanning between a quarter and four times its blocks' time.
fn check_dates(window: &[ProvenBlock], depth: u64, now: i64) -> Result<(), AnchorProblem> {
    let target = pow::DIFFICULTY_TARGET_SECS as u64;
    let (Some(first), Some(last)) = (window.first(), window.last()) else {
        return Err(AnchorProblem::Inconsistent("an empty window".to_owned()));
    };
    let age = u64::try_from(now)
        .unwrap_or(0)
        .saturating_sub(last.timestamp);
    let expected_age = depth * target;
    if age < expected_age / 4 || age > expected_age * 4 {
        return Err(AnchorProblem::Inconsistent(format!(
            "the anchor is dated {age} s ago, where about {expected_age} s is expected"
        )));
    }
    let span = last.timestamp.saturating_sub(first.timestamp);
    let expected_span = (window.len() as u64 - 1) * target;
    if span < expected_span / 4 || span > expected_span * 4 {
        return Err(AnchorProblem::Inconsistent(format!(
            "the window spans {span} s, where about {expected_span} s is expected"
        )));
    }
    Ok(())
}

/// The agreed window, checked: consecutive, chained, every difficulty
/// consistent with the cumulative ones and at least `floor`, and the
/// difficulty it gives the block after the anchor the one claimed for it.
/// Returns the window as proven blocks and the keys below it.
fn check_answer(
    answer: &Answer,
    start: u64,
    anchor: u64,
    floor: u128,
) -> Result<CheckedWindow, AnchorProblem> {
    let rows = &answer.rows;
    let expected = (anchor + 2 - start) as usize;
    if rows.len() != expected {
        return Err(AnchorProblem::Inconsistent(format!(
            "{} headers where {expected} were asked for",
            rows.len()
        )));
    }
    for (i, row) in rows.iter().enumerate() {
        if row.height != start + i as u64 {
            return Err(AnchorProblem::Inconsistent(format!(
                "header {} where {} was asked for",
                row.height,
                start + i as u64
            )));
        }
        if row.difficulty < floor {
            return Err(AnchorProblem::Inconsistent(format!(
                "block {} claims difficulty {}, below this network's floor of {floor}",
                row.height, row.difficulty
            )));
        }
        if i > 0 {
            let before = &rows[i - 1];
            if row.prev_hash != before.hash {
                return Err(AnchorProblem::Inconsistent(format!(
                    "block {} doesn't follow block {}",
                    row.height, before.height
                )));
            }
            if before.cumulative_difficulty.checked_add(row.difficulty)
                != Some(row.cumulative_difficulty)
            {
                return Err(AnchorProblem::Inconsistent(format!(
                    "block {}'s cumulative difficulty doesn't add up",
                    row.height
                )));
            }
        }
    }
    let window = rows[..rows.len() - 1]
        .iter()
        .map(|row| {
            Ok(ProvenBlock {
                height: row.height,
                id: id_of(&row.hash)?,
                timestamp: row.timestamp,
                cumulative_difficulty: row.cumulative_difficulty,
            })
        })
        .collect::<Result<Vec<_>, AnchorProblem>>()?;
    let after = &rows[rows.len() - 1];
    let computed = pow::Window::new(window.iter().cloned()).next_difficulty();
    if computed != after.difficulty {
        return Err(AnchorProblem::Inconsistent(format!(
            "the window gives block {} difficulty {computed}, the nodes claim {}",
            after.height, after.difficulty
        )));
    }
    let seeds = answer
        .keys
        .iter()
        .map(|(height, hash)| Ok((*height, id_of(hash)?)))
        .collect::<Result<Vec<_>, AnchorProblem>>()?;
    Ok((window, seeds))
}

/// Checks the proof of work of a random sample of the window's blocks
/// (always the anchor itself) against the difficulty claimed for each,
/// fetched from the agreeing nodes in turn.
#[cfg_attr(
    not(test),
    expect(
        clippy::cfg_not_test,
        reason = "production samples must remain unpredictable"
    )
)]
async fn sample(
    nodes: &[NodeRef<'_>],
    members: &[usize],
    answer: &Answer,
    window: &[ProvenBlock],
    seeds: &[(u64, [u8; 32])],
    tuning: &ProofTuning,
    hasher: &Hasher,
) -> Result<(), AnchorProblem> {
    let last = window.len() - 1;
    // Production sampling remains unpredictable. Tests use a replayable seed.
    #[cfg(not(test))]
    let mut rng = {
        use rand::SeedableRng as _;
        rand::rngs::StdRng::from_rng(&mut rand::rng())
    };
    #[cfg(test)]
    let seed = std::env::var("PROPTEST_RNG_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(47);
    #[cfg(test)]
    let mut rng = {
        use rand::SeedableRng as _;
        rand::rngs::StdRng::seed_from_u64(seed)
    };
    let mut picked: BTreeSet<usize> = rand::seq::index::sample(
        &mut rng,
        last,
        tuning.anchor_samples.saturating_sub(1).min(last),
    )
    .into_iter()
    .collect();
    picked.insert(last);
    #[cfg(test)]
    {
        fn record_sample(seed: u64, heights: &[u64]) {
            // Direct stderr writes survive libtest capture in nextest failure output.
            use std::io::Write as _;
            let heights = heights
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let _ = writeln!(
                std::io::stderr().lock(),
                "anchor sample seed={seed}, heights=[{heights}]"
            );
        }
        record_sample(
            seed,
            &picked.iter().map(|&i| window[i].height).collect::<Vec<_>>(),
        );
    }
    let start = window[0].height;
    let key_of = |height: u64| -> Option<[u8; 32]> {
        let at = pow::seed_height(height);
        if at >= start {
            window.get((at - start) as usize).map(|b| b.id)
        } else {
            seeds.iter().find(|(h, _)| *h == at).map(|(_, id)| *id)
        }
    };
    let picked: Vec<usize> = picked.into_iter().collect();
    let mut by_key: BTreeMap<[u8; 32], Vec<(usize, Vec<u8>)>> = BTreeMap::new();
    for chunk in picked.chunks(tuning.fetch_concurrency.max(1)) {
        let blobs = join_all(chunk.iter().enumerate().map(async |(i, &index)| {
            let mut failure = None;
            // All members agreed on this window. One peer timing out on a
            // sampled blob must not prevent a healthy majority anchoring.
            for offset in 0..members.len() {
                let node = &nodes[members[(i + offset) % members.len()]];
                match tuning
                    .bounded(node.client.get_block_blob(window[index].height))
                    .await
                {
                    Ok(blob) => match pow::decode(window[index].height, &blob) {
                        Ok(candidate) if candidate.id == window[index].id => return Ok(candidate),
                        Ok(_) => failure = Some("sample blob is not the agreed block".to_owned()),
                        Err(error) => failure = Some(format!("invalid sample blob: {error}")),
                    },
                    Err(error) => failure = Some(error),
                }
            }
            Err(failure.unwrap_or_else(|| "no node supplied a sampled block".to_owned()))
        }))
        .await;
        for (&index, blob) in chunk.iter().zip(blobs) {
            let height = window[index].height;
            let candidate =
                blob.map_err(|reason| AnchorProblem::SampleFailed { height, reason })?;
            let key = key_of(height).ok_or_else(|| {
                AnchorProblem::Failed(format!("no RandomX key for block {height}"))
            })?;
            by_key
                .entry(key)
                .or_default()
                .push((index, candidate.pow_input));
        }
    }
    for (key, blocks) in by_key {
        let (indices, inputs): (Vec<usize>, Vec<Vec<u8>>) = blocks.into_iter().unzip();
        let pow_hashes = hasher
            .hash(key, inputs)
            .await
            .map_err(|e| AnchorProblem::Failed(e.to_string()))?;
        for (index, hash) in indices.into_iter().zip(pow_hashes) {
            let difficulty = answer.rows[index].difficulty;
            if !pow::check_hash(&hash, difficulty) {
                return Err(AnchorProblem::SampleFailed {
                    height: window[index].height,
                    reason: format!(
                        "its proof of work doesn't meet the difficulty {difficulty} the nodes claim"
                    ),
                });
            }
        }
    }
    Ok(())
}
