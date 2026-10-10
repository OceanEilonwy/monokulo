//! The engine page's words and figures for one [`State`]: everything a
//! person reads on the page is written here, in Rust. The page's script
//! and its no-JavaScript view both draw from a [`Presented`]; neither
//! formats a number or writes a sentence of its own.

use serde::Serialize;
use shared::activity::{Group, Tier, TierOutcome, Tuning, Wait, Wake, Work};

use super::machine::{
    plural, stores_phrase, Call, CommitHow, Mark, MarkKind, NodePool, ReorgStep, RibbonEntry, Span,
    State,
};
use crate::views::scaling::thousands;
use shared::activity::PoolPath;

/// A round shorter than this is drawn to this scale: a round of a few
/// milliseconds still reads as short, but its parts apart (a part under a
/// millisecond is drawn 3px wide).
const MIN_SCALE_MS: u64 = 10;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Presented {
    pub summary: Vec<Figure>,
    pub chain: ChainView,
    pub round: Option<RoundView>,
    pub ribbon: Vec<RibbonMark>,
    pub side: Side,
}

/// One figure of the summary row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Figure {
    pub label: &'static str,
    pub value: String,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChainView {
    pub tip: Option<u64>,
    pub high_water: Option<u64>,
    /// The lowest block a group of stores has reached.
    pub lowest: Option<u64>,
    /// Where the reorg window (blocks checked again every round) starts.
    pub window_from: Option<u64>,
    pub cached: Vec<u64>,
    /// Blocks with a scan saved partway, and how far (0 to 1).
    pub checkpoints: Vec<(u64, f64)>,
    pub replaced: Vec<u64>,
    /// The block being scanned, and how far (0 to 1).
    pub scanning: Option<(u64, f64)>,
    pub groups: Vec<GroupView>,
    /// The block still to come, filled by the node's pool.
    pub next_block: Option<NextBlock>,
    pub cache: String,
    pub nodes: Vec<NodeView>,
    /// The last call to the node in use, as monerod names it; `None`
    /// before the page has seen one.
    pub call: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GroupView {
    pub id: u64,
    pub cursor: u64,
    pub frontier: bool,
    /// Being scanned for now.
    pub busy: bool,
    /// Waiting (for a reorg to be reconciled).
    pub waiting: bool,
    pub label: String,
    pub title: String,
}

/// The next block, as the node's pool would fill it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NextBlock {
    /// How full, of the size a miner can fill at full reward (0 to 1),
    /// when the node said how big its pool is.
    pub fill: Option<f64>,
    /// More than a block takes at full reward.
    pub over: bool,
    /// The pool's transactions, short: "23", "1.2k".
    pub count: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NodeView {
    pub label: String,
    /// The node the engine is using now: the last call goes under it.
    pub active: bool,
    pub chip: &'static str,
    /// `ok` for the active node, empty for a fallback, `warn` cooling down.
    pub tone: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RoundView {
    pub number: u64,
    pub title: String,
    pub state: String,
    /// The length the lanes are drawn to.
    pub scale_ms: u64,
    /// How far the round has got: the sum of its lanes' times.
    pub elapsed_ms: u64,
    pub elapsed: String,
    pub lanes: Vec<Lane>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Lane {
    pub tier: Tier,
    pub name: &'static str,
    pub share: String,
    /// The tier's time in the round, everything it waited on included.
    pub ms: u64,
    pub time: String,
    pub bars: Vec<Bar>,
    /// The tier's reserved share, from where it started: drawn while the
    /// round is drawn to its budget.
    pub reserved: Option<(u64, u64)>,
    pub outcome: Option<Chip>,
}

/// A segment of a lane: a run of the tier's units, drawn solid, with the
/// tier's work outside its units drawn outlined over only the time it
/// took (docs/engine_visualizer_decisions.md D39).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Bar {
    pub start_ms: u64,
    /// The time it took: what its label and the round's total add up.
    pub ms: u64,
    /// How long it is drawn, from its start to its last piece's end: the
    /// time other tiers ran between its pieces included.
    pub span_ms: u64,
    /// What it is drawn as, left to right.
    pub shapes: Vec<Shape>,
    /// Only work for the tier outside its units (the round's tip request,
    /// say).
    pub work: bool,
    pub title: String,
    /// Aggregated operations, available in the explicit timing details.
    pub details: Vec<String>,
    /// Its time, written after it; `None` when the next segment of the
    /// lane is too close, which then carries this one's time too. The
    /// labels add up to the round.
    pub label: Option<String>,
    /// The segment that finished last: the round's end marker is just
    /// past its right edge.
    pub last: bool,
}

/// One drawn piece of a segment, in the round's milliseconds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Shape {
    /// The tier's units: solid. `leftover` ran in pass 2, on time left
    /// over (striped).
    Fill {
        from_ms: u64,
        to_ms: u64,
        leftover: bool,
    },
    /// The tier's work outside its units: an outline the solid part's
    /// height. `joined`: it starts where the shape before it ends.
    /// `band_from`: split on a wide track, but joined from there on a
    /// narrow one.
    Work {
        from_ms: u64,
        to_ms: u64,
        joined: bool,
        band_from: Option<u64>,
    },
    /// A 1px line at mid height across the time other tiers ran between
    /// a unit and the tier's later work; `band` as for `Work`.
    Thread {
        from_ms: u64,
        to_ms: u64,
        band: bool,
    },
}

/// A gap between a segment's pieces narrower than this share of the drawn
/// scale is covered by the outline after it (joined); a wider one is
/// crossed by a thread.
const JOIN_SHARE: f64 = 0.025;
/// On a narrow track the same share is far fewer pixels: gaps under this
/// are joined there too (the page's container query, `.band`).
const JOIN_SHARE_NARROW: f64 = 0.05;

/// Segments closer than this, as a share of the drawn length, share one
/// label.
const LABEL_GAP: f64 = 0.08;

/// One of a tier's spans, as a segment is built from them.
struct Piece {
    start_ms: u64,
    ms: u64,
    solid: bool,
    leftover: bool,
}

/// What `pieces` (one segment's, in time order) are drawn as at
/// `scale_ms`: each unit solid, each run of work outside units outlined
/// over the time it took, the gap before it joined or crossed by a thread
/// by its share of the scale. Joined outlines in a row are one box.
fn shapes(pieces: &[Piece], scale_ms: u64) -> Vec<Shape> {
    let mut shapes: Vec<Shape> = Vec::with_capacity(pieces.len() + 2);
    let mut end: Option<u64> = None;
    for piece in pieces {
        let to = piece.start_ms + piece.ms;
        let gap = end.map(|end| piece.start_ms.saturating_sub(end));
        let share = |gap: u64| gap as f64 / scale_ms.max(1) as f64;
        match (piece.solid, gap) {
            (true, gap) => {
                // A unit is never after a gap in one segment: a unit after
                // a gap starts the next segment.
                match shapes.last_mut() {
                    Some(Shape::Fill {
                        to_ms, leftover, ..
                    }) if gap == Some(0) && *leftover == piece.leftover => *to_ms = to,
                    _ => shapes.push(Shape::Fill {
                        from_ms: piece.start_ms,
                        to_ms: to,
                        leftover: piece.leftover,
                    }),
                }
            }
            (false, None) => shapes.push(Shape::Work {
                from_ms: piece.start_ms,
                to_ms: to,
                joined: false,
                band_from: None,
            }),
            (false, Some(gap)) => {
                let before = end.unwrap_or(piece.start_ms);
                if share(gap) < JOIN_SHARE {
                    match shapes.last_mut() {
                        // Joined to an outline: the same box, on to here.
                        Some(Shape::Work { to_ms, .. }) => *to_ms = to,
                        _ => shapes.push(Shape::Work {
                            from_ms: before,
                            to_ms: to,
                            joined: true,
                            band_from: None,
                        }),
                    }
                } else {
                    let band = share(gap) < JOIN_SHARE_NARROW;
                    shapes.push(Shape::Thread {
                        from_ms: before,
                        to_ms: piece.start_ms,
                        band,
                    });
                    shapes.push(Shape::Work {
                        from_ms: piece.start_ms,
                        to_ms: to,
                        joined: false,
                        band_from: band.then_some(before),
                    });
                }
            }
        }
        end = Some(end.map_or(to, |end| end.max(to)));
    }
    shapes
}

/// A time as a lane writes it: "<1ms" for a part that took under a
/// millisecond (times are whole milliseconds), never "0ms".
pub fn lane_time(ms: u64) -> String {
    if ms == 0 {
        "<1ms".to_owned()
    } else {
        milliseconds(ms)
    }
}

/// `tier`'s segments in `round`. A segment is a run of the tier's units
/// with the tier's later work outside its units (however long after);
/// a unit after a gap starts the next one. A segment that took no time is
/// left out where the lane has one that took some. Each is labelled with
/// its time.
fn segments(round: &super::machine::Round, tier: Tier, scale_ms: u64) -> Vec<Bar> {
    type OperationTotals = std::collections::BTreeMap<&'static str, (u64, u64)>;
    let mut built: Vec<(Vec<Piece>, OperationTotals)> = Vec::new();
    for unit in round.units.iter().filter(|unit| unit.tier == tier) {
        let operation = match unit.span {
            Span::Work {
                what: Work::TipRequest,
            } => "Node tip requests",
            Span::Work {
                what: Work::CacheCarry,
            } => "Fetched block cache updates",
            Span::Unit { pass: 2, .. } => "Work using remaining budget",
            Span::Unit { .. } => match tier {
                Tier::Chain => "Chain checks",
                Tier::Blocks => "Block scan work",
                Tier::Mempool => "Mempool scan work",
                Tier::Settlement => "Order settlement work",
                Tier::Upkeep => "Housekeeping work",
            },
        };
        let piece = Piece {
            start_ms: unit.start_ms,
            ms: unit.ms,
            solid: !matches!(unit.span, Span::Work { what: _ }),
            leftover: unit.leftover(),
        };
        let attaches = built.last().is_some_and(|(pieces, _)| {
            let end = pieces.iter().map(|p| p.start_ms + p.ms).max().unwrap_or(0);
            !piece.solid || piece.start_ms == end
        });
        if attaches {
            if let Some((pieces, parts)) = built.last_mut() {
                pieces.push(piece);
                let entry = parts.entry(operation).or_default();
                entry.0 += 1;
                entry.1 += unit.ms;
            }
        } else {
            built.push((
                vec![piece],
                std::collections::BTreeMap::from([(operation, (1, unit.ms))]),
            ));
        }
    }
    let took = |pieces: &[Piece]| pieces.iter().map(|p| p.ms).sum::<u64>();
    if built.iter().any(|(pieces, _)| took(pieces) > 0) {
        built.retain(|(pieces, _)| took(pieces) > 0);
    } else {
        built.truncate(1);
    }
    let close = (scale_ms as f64 * LABEL_GAP) as u64;
    let next_starts: Vec<Option<u64>> = (0..built.len())
        .map(|i| {
            built
                .get(i + 1)
                .and_then(|(pieces, _)| pieces.first().map(|p| p.start_ms))
        })
        .collect();
    let mut carried = 0;
    built
        .into_iter()
        .zip(next_starts)
        .map(|((pieces, parts), next_start)| {
            let start_ms = pieces.first().map_or(0, |p| p.start_ms);
            let end_ms = pieces
                .iter()
                .map(|p| p.start_ms + p.ms)
                .max()
                .unwrap_or(start_ms);
            let ms = took(&pieces);
            let count: u64 = parts.values().map(|(count, _)| count).sum();
            let mut bar = Bar {
                start_ms,
                ms,
                span_ms: end_ms - start_ms,
                shapes: shapes(&pieces, scale_ms),
                work: pieces.iter().all(|p| !p.solid),
                title: format!(
                    "{}: {} total · {} operation{}",
                    tier_name(tier),
                    lane_time(ms).replace("ms", " ms"),
                    count,
                    if count == 1 { "" } else { "s" }
                ),
                details: parts
                    .into_iter()
                    .map(|(operation, (count, ms))| {
                        format!(
                            "{operation}: {count}, {} total",
                            lane_time(ms).replace("ms", " ms")
                        )
                    })
                    .collect(),
                label: None,
                last: false,
            };
            carried += bar.ms;
            if next_start.is_none_or(|next| next.saturating_sub(end_ms) >= close) {
                bar.label = Some(lane_time(carried));
                carried = 0;
            }
            bar
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Chip {
    /// `ok`, `hi`, `warn` or `err`: the theme's chip tones.
    pub tone: &'static str,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RibbonMark {
    Round {
        number: u64,
        /// Height in pixels, by the round's length on a log scale.
        height: u8,
        /// Each tier's part of the round, in tier order.
        parts: Vec<(Tier, f64)>,
        title: String,
    },
    Sleep {
        /// Cut short by a new block.
        woken: bool,
        title: String,
    },
}

/// The one-line summaries on the right, and their details.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Side {
    pub reorg: Panel,
    pub mempool: Panel,
    pub orders: Panel,
    pub upkeep: Panel,
    pub database: Panel,
    pub restart: Panel,
    /// Steps of an open reorg job: found, collect, process (rewind is the
    /// job ending).
    pub reorg_step: Option<&'static str>,
    pub pool_txs: Vec<(String, bool)>,
    /// The last status changes, newest first: `(from, to)`.
    pub transitions: Vec<(&'static str, &'static str)>,
    /// Queued jobs per database class, and the capacity.
    pub queues: [u64; 2],
    pub queue_capacity: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Panel {
    pub summary: String,
    /// Draws attention: open by itself, with an alert edge.
    pub alert: bool,
    pub rows: Vec<(String, String)>,
}

/// A mark as the page shows it: a line on the timeline, a row in the
/// events table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MarkView {
    pub seq: u64,
    pub at_ms: i64,
    pub round: u64,
    pub tier: Tier,
    pub key: bool,
    pub text: String,
}

pub fn mark_view(mark: &Mark) -> MarkView {
    MarkView {
        seq: mark.seq,
        at_ms: mark.at_ms,
        round: mark.round,
        tier: mark.tier,
        key: mark.key,
        text: mark_text(&mark.what),
    }
}

/// What happened, as a sentence.
pub fn mark_text(what: &MarkKind) -> String {
    match what {
        MarkKind::NewBlocks { tip, count: 1 } => {
            format!("The node has a new block: {}.", thousands(*tip))
        }
        MarkKind::NewBlocks { tip, count } => format!(
            "The node has {} new blocks, up to {}.",
            thousands(*count),
            thousands(*tip)
        ),
        MarkKind::ReorgFound { fork, deeper: true } => format!(
            "The reorganisation goes deeper: it is reconciled from block {} now.",
            thousands(*fork)
        ),
        MarkKind::ReorgFound { fork, deeper: false } => format!(
            "The node's chain differs from block {} on: a reorganisation. Payments from there are re-examined, and new blocks wait.",
            thousands(*fork)
        ),
        MarkKind::ReorgCollected => {
            "Payments at or above the fork were queued to be re-examined.".to_owned()
        }
        MarkKind::ReorgProcessed {
            examined,
            changed,
            voided,
        } => format!(
            "Re-examined {} against the node's chain: {} changed, {} voided as double-spent.",
            plural(*examined, "payment"),
            thousands(*changed),
            thousands(*voided)
        ),
        MarkKind::Rewound {
            fork,
            deleted_to: Some(to),
            ancestor,
        } if to > fork => format!(
            "Rewound: blocks {} to {} deleted, stores moved back to block {}. The replacement blocks are scanned next.",
            thousands(*fork),
            thousands(*to),
            thousands(*ancestor)
        ),
        MarkKind::Rewound {
            fork,
            deleted_to: _,
            ancestor,
        } => format!(
            "Rewound: block {} deleted, stores moved back to block {}. The replacement block is scanned next.",
            thousands(*fork),
            thousands(*ancestor)
        ),
        MarkKind::Seeded { height } => format!(
            "Started scanning this network at block {}.",
            thousands(*height)
        ),
        MarkKind::Checkpointed {
            height,
            stores,
            done_txs,
            total_txs,
        } => format!(
            "Out of time partway through block {}: {} of {} transactions scanned for {}, saved.",
            thousands(*height),
            thousands(*done_txs),
            thousands(*total_txs),
            stores_phrase(*stores)
        ),
        MarkKind::Committed {
            height,
            stores,
            found,
            how,
        } => {
            let found = if *found > 0 {
                format!(", {} found in it", plural(*found, "payment"))
            } else {
                String::new()
            };
            let (height, stores) = (thousands(*height), stores_phrase(*stores));
            match how {
                CommitHow::HeaderOnly => format!(
                    "Block {height} recorded from its header: no store had anything to look for in it."
                ),
                CommitHow::Frontier => {
                    format!("Block {height} scanned for {stores} and committed{found}.")
                }
                CommitHow::Joined => format!(
                    "Block {height} scanned for {stores} catching up{found}: they caught up and joined the frontier."
                ),
                CommitHow::CatchingUp => {
                    format!("Block {height} scanned for {stores} catching up{found}.")
                }
            }
        }
        MarkKind::Diverged { height } => format!(
            "Block {} from the node doesn't extend the recorded chain: new blocks wait for the reorganisation to be reconciled.",
            thousands(*height)
        ),
        MarkKind::IdleAdvanced { from, to, stores } => format!(
            "{} with nothing that could have been paid moved straight from block {} to {}.",
            capitalised(&stores_phrase(*stores)),
            thousands(*from),
            thousands(*to)
        ),
        MarkKind::FastScanned { transactions } => format!(
            "The fast path scanned {} new in the pool.",
            plural(*transactions, "transaction")
        ),
        MarkKind::TxMatched {
            path: PoolPath::Fast,
            txid,
        } => format!(
            "Transaction {txid} in the pool pays an order: recorded at once by the fast path."
        ),
        MarkKind::TxMatched {
            path: PoolPath::Round,
            txid,
        } => format!(
            "Transaction {txid} in the pool pays an order: found by the round's rotation."
        ),
        MarkKind::Recomputed {
            orders: 1,
            from,
            to,
        } => format!(
            "An order went from {} to {}; its order event is written.",
            from.as_str(),
            to.as_str()
        ),
        MarkKind::Recomputed { orders, from, to } => format!(
            "{} orders changed status (one from {} to {}); their order events are written.",
            thousands(*orders),
            from.as_str(),
            to.as_str()
        ),
        MarkKind::Pruned { hashes } => format!("Pruned {} old block hashes.", thousands(*hashes)),
        MarkKind::TierFailed { tier } => format!(
            "The {tier} tier failed and stopped for this round; it is retried next round."
        ),
        MarkKind::NodeFailed { tier } => {
            format!("The {tier} tier stopped for this round: the node failed.")
        }
    }
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// `state` in words and figures, its rounds to `tuning`'s scale.
pub fn present(state: &State, tuning: &Tuning) -> Presented {
    Presented {
        summary: summary(state),
        chain: chain(state, tuning),
        round: present_round(state, tuning),
        ribbon: state.ribbon.iter().map(ribbon_mark).collect(),
        side: side(state),
    }
}

fn summary(state: &State) -> Vec<Figure> {
    let chain = &state.chain;
    let lowest = state.groups.iter().map(|g| g.cursor).min();
    let behind = match (chain.tip, lowest.or(chain.high_water)) {
        (Some(tip), Some(low)) => Some(tip.saturating_sub(low)),
        _ => None,
    };
    let catching_up = state.catching_up();
    let stores: u64 = state.groups.iter().map(|g| g.stores).sum();
    let last_round = state.ribbon.iter().rev().find_map(|entry| match entry {
        RibbonEntry::Round { ms, backlogged, .. } => Some((*ms, *backlogged)),
        RibbonEntry::Sleep { .. } => None,
    });
    vec![
        Figure {
            label: "Node tip",
            value: chain.tip.map_or_else(dash, thousands),
            note: state.nodes.iter().find(|node| node.active).map_or_else(
                || "no node reported yet".to_owned(),
                |node| node.label.clone(),
            ),
        },
        Figure {
            label: "Scanned to",
            value: chain.high_water.map_or_else(dash, thousands),
            note: "high-water mark".to_owned(),
        },
        Figure {
            label: "Behind",
            value: behind.map_or_else(dash, thousands),
            note: match behind {
                Some(0) => "caught up".to_owned(),
                Some(_) if catching_up > 0 => format!("{} catching up", stores_phrase(catching_up)),
                Some(_) => "new blocks to scan".to_owned(),
                None => "not known yet".to_owned(),
            },
        },
        Figure {
            label: "Stores",
            value: thousands(stores),
            note: if catching_up > 0 {
                format!("{} catching up", thousands(catching_up))
            } else {
                "all at the high-water mark".to_owned()
            },
        },
        Figure {
            label: "Last round",
            value: last_round.map_or_else(dash, |(ms, _)| seconds(ms)),
            note: match last_round {
                Some((_, true)) => "work was left".to_owned(),
                Some((_, false)) | None => "of a 10s budget".to_owned(),
            },
        },
        Figure {
            label: "Chain",
            value: if state.reorg.is_some() {
                "Reorg".to_owned()
            } else if !chain.replaced.is_empty() {
                "Diverged".to_owned()
            } else {
                match state.chain_agrees {
                    Some(true) => "Agrees".to_owned(),
                    Some(false) => "Differs".to_owned(),
                    None => dash(),
                }
            },
            note: match state.reorg {
                Some(reorg) => format!("from block {}", thousands(reorg.fork)),
                None if !chain.replaced.is_empty() => "waiting for reconciliation".to_owned(),
                None => "with the node".to_owned(),
            },
        },
    ]
}

fn chain(state: &State, tuning: &Tuning) -> ChainView {
    let chain = &state.chain;
    let high_water = chain.high_water;
    ChainView {
        tip: chain.tip,
        high_water,
        lowest: state.groups.iter().map(|g| g.cursor).min(),
        window_from: high_water
            .map(|high| high.saturating_sub(tuning.reorg_check_depth.saturating_sub(1))),
        cached: chain.cached.iter().copied().collect(),
        checkpoints: chain
            .checkpoints
            .iter()
            .map(|(height, (done, total))| (*height, fraction(*done, *total)))
            .collect(),
        replaced: chain.replaced.iter().copied().collect(),
        scanning: chain
            .scanning
            .map(|s| (s.height, fraction(s.done_txs, s.total_txs))),
        groups: state
            .groups
            .iter()
            .map(|group| {
                let frontier = Some(group.cursor) == high_water;
                let busy = chain.scanning.is_some_and(|s| {
                    s.height == group.cursor + 1 && (s.group == Group::Frontier) == frontier
                });
                let pages = if group.stores > tuning.group_page {
                    format!(
                        " ({} pages)",
                        group.stores.div_ceil(tuning.group_page.max(1))
                    )
                } else {
                    String::new()
                };
                GroupView {
                    id: group.id,
                    cursor: group.cursor,
                    frontier,
                    busy,
                    waiting: state.reorg.is_some(),
                    label: format!(
                        "{}, {}{pages}",
                        if frontier { "Frontier" } else { "Catching up" },
                        stores_phrase(group.stores)
                    ),
                    title: format!(
                        "{} whose scan has reached block {}",
                        stores_phrase(group.stores),
                        thousands(group.cursor)
                    ),
                }
            })
            .collect(),
        next_block: state.node_pool.map(next_block),
        cache: format!(
            "cache {} of {}",
            megabytes(chain.cache_bytes),
            megabytes(chain.cache_budget_bytes)
        ),
        nodes: state
            .nodes
            .iter()
            .map(|node| NodeView {
                label: node.label.clone(),
                active: node.active,
                chip: if node.active {
                    "active"
                } else if node.cooling_down {
                    "cooling down"
                } else {
                    "fallback"
                },
                tone: if node.active {
                    "ok"
                } else if node.cooling_down {
                    "warn"
                } else {
                    ""
                },
            })
            .collect(),
        call: state.last_call.map(|call| match call {
            Call::BlockHash { height } => format!("on_get_block_hash {}", thousands(height)),
            Call::Blocks { from, count } => {
                format!(
                    "get_blocks.bin {} from {}",
                    thousands(count),
                    thousands(from)
                )
            }
            Call::Pool => "get_transaction_pool_hashes".to_owned(),
            Call::Transactions => "get_transactions".to_owned(),
        }),
    }
}

fn next_block(pool: NodePool) -> NextBlock {
    let fill = pool
        .bytes
        .zip(pool.penalty_free)
        .map(|(bytes, free)| bytes as f64 / free.max(1) as f64);
    let over = fill.is_some_and(|fill| fill > 1.0);
    let transactions = plural(pool.txs, "transaction");
    NextBlock {
        fill: fill.map(|fill| rounded(fill.min(1.0))),
        over,
        count: compact(pool.txs),
        title: match pool.bytes.zip(pool.penalty_free) {
            Some((bytes, free)) => format!(
                "The next block: {transactions} waiting in the node's pool, {} of the {} a miner can fill at full reward ({:.0} %){}",
                kilobytes(bytes),
                kilobytes(free),
                fill.unwrap_or(0.0) * 100.0,
                if over {
                    ": more than one block takes without a smaller reward."
                } else {
                    "."
                }
            ),
            None => format!(
                "The next block: {transactions} waiting in the node's pool (the node didn't say their size)."
            ),
        },
    }
}

/// The round in progress, or the last one, as the round card draws it.
pub fn present_round(state: &State, tuning: &Tuning) -> Option<RoundView> {
    state
        .round
        .as_ref()
        .map(|round| round_view(state, round, tuning))
}

fn round_view(state: &State, round: &super::machine::Round, tuning: &Tuning) -> RoundView {
    // The round's spans are back to back from its start, so the lanes'
    // times add up to how far it has got.
    let elapsed_ms: u64 = round.units.iter().map(|unit| unit.ms).sum();
    let scale_ms = if round.to_budget {
        round.budget_ms.max(elapsed_ms)
    } else {
        (elapsed_ms.saturating_mul(115) / 100).max(MIN_SCALE_MS)
    };
    let state_line = match round.finished {
        Some(finished) if finished.backlogged => {
            "Ended with work left: the next round starts at once.".to_owned()
        }
        Some(_) => "Ended. Sleeping until the poll interval is up or the node announces a block."
            .to_owned(),
        None => match round.woken_by {
            Some(Wake::Interval) => "Running, after the poll interval.".to_owned(),
            Some(Wake::NewBlock) => "Running, woken by the node announcing a block.".to_owned(),
            None if state.ribbon.is_empty() => "Running.".to_owned(),
            None => "Running, started at once: work was left.".to_owned(),
        },
    };
    let mut lanes: Vec<Lane> = Tier::ALL
        .iter()
        .map(|tier| {
            let share = tuning.shares[tier.index()];
            let lane_ms = round
                .units
                .iter()
                .filter(|unit| unit.tier == *tier)
                .map(|unit| unit.ms)
                .sum();
            let bars = segments(round, *tier, scale_ms);
            // The reserved share is drawn from where the tier's first
            // segment starts (its work before its units with it), so the
            // outline goes round its segments rather than across one.
            let started = round
                .units
                .iter()
                .any(|unit| {
                    unit.tier == *tier
                        && matches!(
                            unit.span,
                            Span::Unit {
                                pass: 1,
                                progress: _
                            }
                        )
                })
                .then(|| bars.first().map(|bar| bar.start_ms))
                .flatten();
            Lane {
                tier: *tier,
                name: tier_name(*tier),
                share: format!("{share} %"),
                ms: lane_ms,
                time: lane_time(lane_ms),
                bars,
                reserved: if round.to_budget {
                    started.map(|start| (start, round.budget_ms * u64::from(share) / 100))
                } else {
                    None
                },
                outcome: round.ended[tier.index()].map(outcome_chip),
            }
        })
        .collect();
    // The end marker goes on the segment that finished last (the latest
    // to start, of those ending last).
    if let Some(bar) = lanes
        .iter_mut()
        .flat_map(|lane| lane.bars.iter_mut())
        .max_by_key(|bar| (bar.start_ms + bar.span_ms, bar.start_ms))
    {
        bar.last = true;
    }
    RoundView {
        number: round.number,
        title: format!("Round {}", thousands(round.number)),
        state: state_line,
        scale_ms,
        elapsed_ms,
        elapsed: lane_time(elapsed_ms),
        lanes,
    }
}

/// A tier's ending as a chip.
pub fn outcome_chip(outcome: TierOutcome) -> Chip {
    match outcome {
        TierOutcome::Idle => Chip {
            tone: "ok",
            text: "Idle".to_owned(),
        },
        TierOutcome::Backlogged => Chip {
            tone: "hi",
            text: "Backlogged: out of time with work left".to_owned(),
        },
        TierOutcome::Blocked(wait) => Chip {
            tone: "warn",
            text: format!("Waiting: {}", wait_words(wait)),
        },
        TierOutcome::Failed => Chip {
            tone: "err",
            text: "Failed: retried next round".to_owned(),
        },
    }
}

fn wait_words(wait: Wait) -> &'static str {
    match wait {
        Wait::ChainHeightUnknown => "the chain height is unknown",
        Wait::ReorgBeingReconciled => "a reorganisation is being reconciled",
        Wait::RewoundThisRound => "rewound this round",
        Wait::NodeFailed => "the node failed",
        Wait::NodeCannotServeTip => "the node can't serve its tip yet",
        Wait::MempoolUnreadable => "the pool couldn't be read",
        Wait::ReorgCandidatesRetrying => "payments to re-examine are waiting to retry",
        Wait::ChainDiverged => "the node's chain differs from the recorded one",
    }
}

fn ribbon_mark(entry: &RibbonEntry) -> RibbonMark {
    match *entry {
        RibbonEntry::Round {
            number,
            ms,
            tiers_ms,
            backlogged,
        } => {
            let total: u64 = tiers_ms.iter().sum();
            RibbonMark::Round {
                number,
                height: ribbon_height(ms),
                parts: Tier::ALL
                    .iter()
                    .filter(|tier| tiers_ms[tier.index()] > 0)
                    .map(|tier| (*tier, fraction(tiers_ms[tier.index()], total)))
                    .collect(),
                title: format!(
                    "Round {}: {}{}",
                    thousands(number),
                    seconds(ms),
                    if backlogged { ", work was left" } else { "" }
                ),
            }
        }
        RibbonEntry::Sleep { ms, woken_by } => RibbonMark::Sleep {
            woken: woken_by == Wake::NewBlock,
            title: match woken_by {
                Wake::NewBlock => format!("Slept {}, cut short by a new block", seconds(ms)),
                Wake::Interval => format!("Slept {}, the poll interval", seconds(ms)),
            },
        },
    }
}

/// 4 to 28 pixels: 4 for a millisecond, 28 for ten seconds or more.
fn ribbon_height(ms: u64) -> u8 {
    let scaled = (ms.max(1) as f64).log10() / 4.0;
    let pixels = 4.0 + 24.0 * scaled.clamp(0.0, 1.0);
    // In range by the clamp.
    pixels.round() as u8
}

fn side(state: &State) -> Side {
    let reorg = state.reorg;
    let orders = &state.orders;
    let waiting = orders.pending + orders.due;
    let fast_passes = state.pool.fast_passes.count();
    let queued: u64 = state.database.queued.iter().sum();
    let saves = state.saves.count();
    Side {
        reorg: Panel {
            summary: match reorg {
                Some(reorg) => format!(
                    "Reorg from {}: {}",
                    thousands(reorg.fork),
                    match reorg.step {
                        ReorgStep::Found => "found",
                        ReorgStep::Collecting => "collecting payments",
                        ReorgStep::Processing => "re-examining payments",
                    }
                ),
                None if !state.chain.replaced.is_empty() => {
                    "The node's chain differs: waiting to reconcile".to_owned()
                }
                None => match (state.chain_agrees, state.chain.high_water) {
                    (Some(true), Some(high)) => {
                        format!("Agrees with the node at {}", thousands(high))
                    }
                    _ => "No reorganisation".to_owned(),
                },
            },
            alert: reorg.is_some() || !state.chain.replaced.is_empty(),
            rows: vec![
                (
                    "Fork at".to_owned(),
                    reorg.map_or_else(dash, |r| thousands(r.fork)),
                ),
                (
                    "Payments left to re-examine".to_owned(),
                    reorg
                        .and_then(|r| r.candidates)
                        .map_or_else(dash, thousands),
                ),
                (
                    "Re-examined, changed, voided".to_owned(),
                    reorg.map_or_else(dash, |r| {
                        format!(
                            "{}, {}, {}",
                            thousands(r.examined),
                            thousands(r.changed),
                            thousands(r.voided)
                        )
                    }),
                ),
            ],
        },
        mempool: Panel {
            summary: {
                let found = if state.pool.found > 0 {
                    format!(", {} found", plural(state.pool.found, "payment"))
                } else {
                    String::new()
                };
                match (state.node_pool, state.pool.watched) {
                    (Some(node), true) => {
                        format!("{} in the node's pool{found}", thousands(node.txs))
                    }
                    (Some(node), false) => {
                        format!("{} in the node's pool, not scanned", thousands(node.txs))
                    }
                    (None, true) => format!(
                        "{} scanned in the pool{found}",
                        thousands(state.pool.remembered)
                    ),
                    (None, false) => "Not scanned: no order waits to be paid".to_owned(),
                }
            },
            alert: false,
            rows: vec![
                (
                    "In the node's pool".to_owned(),
                    state
                        .node_pool
                        .map_or_else(dash, |node| thousands(node.txs)),
                ),
                (
                    "Of the next block's full-reward size".to_owned(),
                    state
                        .node_pool
                        .and_then(|node| node.bytes.zip(node.penalty_free))
                        .map(|(bytes, free)| {
                            format!("{:.0} %", bytes as f64 / free.max(1) as f64 * 100.0)
                        })
                        .unwrap_or_else(dash),
                ),
                (
                    "Scanned by the engine".to_owned(),
                    if state.pool.watched {
                        "yes".to_owned()
                    } else {
                        "no: no order waits to be paid".to_owned()
                    },
                ),
                (
                    "Transactions scanned and remembered".to_owned(),
                    thousands(state.pool.remembered),
                ),
                (
                    "Fast passes with something new, last minute".to_owned(),
                    thousands(fast_passes),
                ),
                ("Payments found".to_owned(), thousands(state.pool.found)),
            ],
        },
        orders: Panel {
            summary: if reorg.is_some() && waiting > 0 {
                "paid held while the reorg is open".to_owned()
            } else if waiting > 0 {
                format!("{} to recompute", thousands(waiting))
            } else {
                "nothing to recompute".to_owned()
            },
            alert: false,
            rows: vec![
                ("A payment changed".to_owned(), thousands(orders.pending)),
                ("Due by time or height".to_owned(), thousands(orders.due)),
                (
                    "Unconfirmed payments checked last".to_owned(),
                    thousands(orders.looked),
                ),
            ],
        },
        upkeep: Panel {
            summary: state.upkeep.round.map_or_else(
                || "every round, in the background".to_owned(),
                |round| format!("ran in round {}", thousands(round)),
            ),
            alert: false,
            rows: vec![(
                "Old block hashes pruned last time".to_owned(),
                thousands(state.upkeep.pruned),
            )],
        },
        database: Panel {
            summary: format!(
                "{} queued, {} jobs done",
                thousands(queued),
                thousands(state.database.completed)
            ),
            alert: false,
            rows: vec![
                (
                    "Scanner queue".to_owned(),
                    queue(state.database.queued[0], state.database.capacity),
                ),
                (
                    "Admin queue".to_owned(),
                    queue(state.database.queued[1], state.database.capacity),
                ),
                (
                    "Longest wait for a turn".to_owned(),
                    micros(state.database.max_queue_wait_us),
                ),
                ("Longest job".to_owned(), micros(state.database.max_run_us)),
            ],
        },
        restart: Panel {
            summary: format!(
                "{} a minute; {} only in memory",
                plural(saves, "save"),
                plural(
                    u64::try_from(state.chain.cached.len()).unwrap_or(u64::MAX),
                    "block"
                )
            ),
            alert: false,
            rows: vec![
                (
                    "Store cursors saved".to_owned(),
                    thousands(state.saved.cursors),
                ),
                (
                    "Block checkpoints saved".to_owned(),
                    thousands(state.saved.checkpoints),
                ),
                (
                    "Reorg job steps saved".to_owned(),
                    thousands(state.saved.reorg),
                ),
                (
                    "Payments recorded".to_owned(),
                    thousands(state.saved.payments),
                ),
                (
                    "Recomputes saved".to_owned(),
                    thousands(state.saved.recomputes),
                ),
                (
                    "In memory: block cache".to_owned(),
                    megabytes(state.chain.cache_bytes),
                ),
                (
                    "In memory: pool transactions".to_owned(),
                    thousands(state.pool.remembered),
                ),
            ],
        },
        reorg_step: reorg.map(|r| match r.step {
            ReorgStep::Found => "found",
            ReorgStep::Collecting => "collect",
            ReorgStep::Processing => "process",
        }),
        pool_txs: state
            .pool
            .txs
            .iter()
            .map(|tx| (tx.txid.clone(), tx.matched))
            .collect(),
        transitions: orders
            .last
            .iter()
            .map(|t| (t.from.as_str(), t.to.as_str()))
            .collect(),
        queues: state.database.queued,
        queue_capacity: state.database.capacity,
    }
}

pub fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Chain => "Chain",
        Tier::Blocks => "Blocks",
        Tier::Mempool => "Mempool",
        Tier::Settlement => "Settlement",
        Tier::Upkeep => "Upkeep",
    }
}

/// "0.42s" under a second, "7.8s" over.
pub fn seconds(ms: u64) -> String {
    if ms < 1000 {
        format!("{:.2}s", ms as f64 / 1000.0)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// "4ms", "1,204ms": the round card's times, whole milliseconds so its
/// parts visibly add up to it.
pub fn milliseconds(ms: u64) -> String {
    format!("{}ms", thousands(ms))
}

/// Decimal kilobytes, as block sizes are usually given: "41 kB".
fn kilobytes(bytes: u64) -> String {
    format!("{} kB", thousands(bytes.div_ceil(1000)))
}

/// A count in at most four characters: "23", "1.2k", "12k", "1.2M".
fn compact(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{:.1}k", n as f64 / 1000.0),
        10_000..1_000_000 => format!("{}k", n / 1000),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

fn micros(us: u64) -> String {
    if us < 1000 {
        format!("{us}µs")
    } else {
        format!("{:.1}ms", us as f64 / 1000.0)
    }
}

fn queue(queued: u64, capacity: u64) -> String {
    format!("{} of {}", thousands(queued), thousands(capacity))
}

/// `part` of `whole`, 0 to 1, to three places: enough to draw, and short
/// in every frame the page is sent.
fn fraction(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        rounded((part as f64 / whole as f64).clamp(0.0, 1.0))
    }
}

fn rounded(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn dash() -> String {
    "–".to_owned()
}

#[cfg(test)]
mod tests;
