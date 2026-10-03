//! The engine page's state machine: an engine event in, the next state and
//! what to animate out (`docs/engine_visualizer.md`).
//!
//! [`step`] is the page's only logic. It does no I/O, reads no clock (an
//! event's own time is the only time it knows) and knows nothing of how the
//! state is drawn: [`State`] is what the page shows, [`Effect`]s are the
//! movements that lead to it, and a [`Mark`] is the event as a sentence for
//! the timeline and the events table. Everything that draws, live, replayed
//! or scrubbed, and with or without JavaScript, is drawn from these.
//!
//! The engine's snapshots ([`Event::Snapshot`]) are the truth; between them
//! the state follows the events. A snapshot overrides whatever the events
//! left, so a missed or misread event costs at most a few seconds of
//! inaccuracy.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;
use shared::activity::{
    Database, Event, Group, Node, PoolPath, Recorded, Snapshot, Tier, TierOutcome, Transition,
    UnitProgress, Wait, Wake, Work,
};
use shared::order_status::OrderStatus;

use crate::views::scaling::thousands;

/// Rounds and sleeps kept for the recent-rounds ribbon.
pub const RIBBON: usize = 90;
/// Pool transactions kept to show.
pub const POOL_TXS: usize = Snapshot::POOL_TXS;
/// Order status changes kept to show.
pub const LAST_TRANSITIONS: usize = 3;
/// Tokens flown for one event at most: a block holding forty payments
/// flies three, not forty.
pub const MAX_TOKENS: usize = 3;
/// How far back "a minute" reaches, in the events' own milliseconds.
const MINUTE_MS: i64 = 60_000;

/// Everything the engine page shows about one network at one moment.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct State {
    /// The time of the last event applied (the engine's Unix milliseconds).
    pub at_ms: i64,
    pub chain: Chain,
    /// Groups of stores by the block their scan has reached, highest first.
    pub groups: Vec<StoreGroup>,
    /// Catch-up groups the engine didn't list (beyond the first few).
    pub more_groups: u64,
    /// The round in progress, or the last one.
    pub round: Option<Round>,
    /// Recent rounds and the sleeps between them, oldest first.
    pub ribbon: VecDeque<RibbonEntry>,
    /// Whether the last reorg check found the recorded chain agreeing.
    pub chain_agrees: Option<bool>,
    pub reorg: Option<Reorg>,
    pub pool: Pool,
    /// The node's whole pool, what the next block is mined from: known
    /// only while someone watches the page.
    pub node_pool: Option<NodePool>,
    pub orders: Orders,
    pub upkeep: Upkeep,
    pub database: Database,
    pub webhooks: Webhooks,
    pub nodes: Vec<Node>,
    /// The last call made to the node, as far as the events tell.
    pub last_call: Option<Call>,
    /// When durable writes happened in the last minute.
    pub saves: VecDeque<i64>,
    /// Durable writes by what was written, since the page's history began.
    pub saved: Saved,
    /// What the next round's `woken_by` is: set by a sleep, taken by the
    /// round that follows it.
    #[serde(skip)]
    pending_wake: Option<Wake>,
    #[serde(skip)]
    next_group_id: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Chain {
    /// The node's tip as the last round read it.
    pub tip: Option<u64>,
    /// The highest block recorded.
    pub high_water: Option<u64>,
    /// Blocks fetched and held for scanning.
    pub cached: BTreeSet<u64>,
    pub cache_bytes: u64,
    pub cache_budget_bytes: u64,
    /// Blocks with a scan saved partway: transactions done of the total.
    pub checkpoints: BTreeMap<u64, (u64, u64)>,
    /// Blocks the node no longer agrees with: diverged, or being replaced
    /// by a reorganisation.
    pub replaced: BTreeSet<u64>,
    pub scanning: Option<Scanning>,
}

/// A block scan in progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Scanning {
    pub height: u64,
    pub group: Group,
    pub stores: u64,
    pub done_txs: u64,
    pub total_txs: u64,
    pub header_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct StoreGroup {
    /// Stable while the group moves, so the page can animate it.
    pub id: u64,
    pub cursor: u64,
    pub stores: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Round {
    pub number: u64,
    pub budget_ms: u64,
    pub tip: Option<u64>,
    /// What ended the sleep before it; `None` when it followed a round with
    /// work left over (or the page has no sleep before it).
    pub woken_by: Option<Wake>,
    /// Drawn to the scale of its whole budget, with each tier's share:
    /// stores were catching up, or a reorg was open, as it started.
    pub to_budget: bool,
    pub units: Vec<UnitBar>,
    /// How each tier ended, once it has.
    pub ended: [Option<TierOutcome>; 5],
    /// How far into the round its units have got.
    pub elapsed_ms: u64,
    pub finished: Option<Finished>,
}

/// A span of a round: one of a tier's units, or work done for a tier
/// outside its units (`work`). A round's spans are back to back, so they
/// add up to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct UnitBar {
    pub tier: Tier,
    /// 1 or 2 for a unit; 0 for other work.
    pub pass: u8,
    pub start_ms: u64,
    pub ms: u64,
    pub progress: UnitProgress,
    pub work: Option<Work>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Finished {
    pub ms: u64,
    pub backlogged: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RibbonEntry {
    Round {
        number: u64,
        ms: u64,
        /// Time spent by each tier, in [`Tier::ALL`] order.
        tiers_ms: [u64; 5],
        backlogged: bool,
    },
    Sleep {
        ms: u64,
        woken_by: Wake,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReorgStep {
    /// Found; nothing queued yet.
    Found,
    /// Payments at or above the fork are being queued.
    Collecting,
    /// Queued payments are being re-examined.
    Processing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Reorg {
    pub fork: u64,
    pub step: ReorgStep,
    /// Payments left to re-examine, as of the last snapshot.
    pub candidates: Option<u64>,
    pub examined: u64,
    pub changed: u64,
    pub voided: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Pool {
    /// Whether the scanner looks at the pool: only while an order could be
    /// paid from it, or a payment waits for a block.
    pub watched: bool,
    /// Transactions the scanner remembers from the pool.
    pub size: u64,
    /// The newest few, shortened, oldest first.
    pub txs: VecDeque<PoolTx>,
    /// When the fast path scanned new transactions, in the last minute.
    pub fast_passes: VecDeque<i64>,
    /// Payments found in the pool since the page's history began.
    pub found: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct NodePool {
    pub txs: u64,
    /// Their size in bytes, when the node said.
    pub bytes: Option<u64>,
    /// The block weight a miner can fill without a smaller reward.
    pub penalty_free: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PoolTx {
    pub txid: String,
    pub matched: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Orders {
    /// Orders waiting to be recomputed because a payment changed.
    pub pending: u64,
    /// Orders due a recompute by time or height.
    pub due: u64,
    /// The last status changes, newest first.
    pub last: VecDeque<Transition>,
    /// Unconfirmed payments the last vanished-payment check looked at.
    pub looked: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Upkeep {
    /// The round upkeep last ran in.
    pub round: Option<u64>,
    /// Old block hashes it removed then.
    pub pruned: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Webhooks {
    pub due: u64,
    /// Deliveries in each 10 s of the five minutes before the last
    /// snapshot, oldest first.
    pub sent: Vec<u32>,
}

/// A call made to the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Call {
    BlockHash { height: u64 },
    Blocks { from: u64, count: u64 },
    Pool,
    Transactions,
}

/// Durable writes since the page's history began, by what was written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Saved {
    pub cursors: u64,
    pub checkpoints: u64,
    pub reorg: u64,
    pub payments: u64,
    pub recomputes: u64,
}

/// A place on the page a movement starts or ends at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Anchor {
    Node,
    /// A block.
    Cell(u64),
    /// A group of stores, by [`StoreGroup::id`].
    Group(u64),
    Pool,
    Reorg,
    Orders,
    Webhooks,
    Database,
    Upkeep,
}

/// What flies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Token {
    Payment,
    Envelope,
    Stores,
}

/// A movement leading to the next state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    /// A call goes from the node to where its answer is used.
    Packet { to: Anchor, call: Call },
    Fly {
        from: Anchor,
        to: Anchor,
        token: Token,
    },
    /// Something was written to disk here.
    Save { at: Anchor },
    /// Something happened here: a block committed, a fast pass, upkeep, the
    /// node announcing a block.
    Flash { at: Anchor },
    /// Reorg detection compared this block with the node's.
    Probe { height: u64 },
    /// New blocks appeared at the node.
    NewBlocks { from: u64, to: u64 },
    /// Recorded blocks were deleted by a rewind.
    Drop { from: u64, to: u64 },
}

/// An event as a sentence: a line on the timeline, a row in the events
/// table. Key marks are the circles.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Mark {
    pub seq: u64,
    pub at_ms: i64,
    pub round: u64,
    pub tier: Option<Tier>,
    pub key: bool,
    pub text: String,
}

/// What one step produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub effects: Vec<Effect>,
    pub mark: Option<Mark>,
}

/// Applies `recorded` to `state`: the next state, and what led to it.
pub fn step(state: &mut State, recorded: &Recorded) -> Output {
    let mut out = Step {
        effects: Vec::new(),
        mark: None,
    };
    state.at_ms = state.at_ms.max(recorded.at_ms);
    match &recorded.event {
        Event::Snapshot(snapshot) => state.apply_snapshot(snapshot),
        Event::RoundStarted {
            round,
            budget_ms,
            tip,
        } => state.round_started(&mut out, *round, *budget_ms, *tip),
        Event::Unit {
            tier,
            pass,
            start_ms,
            ms,
            progress,
        } => state.unit(
            &mut out,
            UnitBar {
                tier: *tier,
                pass: *pass,
                start_ms: *start_ms,
                ms: *ms,
                progress: *progress,
                work: None,
            },
        ),
        Event::Work {
            tier,
            start_ms,
            ms,
            what,
        } => state.unit(
            &mut out,
            UnitBar {
                tier: *tier,
                pass: 0,
                start_ms: *start_ms,
                ms: *ms,
                progress: UnitProgress::Advanced,
                work: Some(*what),
            },
        ),
        Event::TierEnded { tier, outcome } => {
            if let Some(round) = &mut state.round {
                round.ended[tier.index()] = Some(*outcome);
            }
        }
        Event::RoundFinished {
            round,
            ms,
            backlogged,
        } => state.round_finished(*round, *ms, *backlogged),
        Event::Slept { ms, woken_by } => {
            state.push_ribbon(RibbonEntry::Sleep {
                ms: *ms,
                woken_by: *woken_by,
            });
            state.pending_wake = Some(*woken_by);
        }
        Event::ChainChecked { agrees, looked_up } => {
            state.chain_agrees = Some(*agrees);
            if let Some(height) = state.chain.high_water {
                // Asked of the node only when the tip's own hash, which
                // came with the round's tip request, wasn't enough.
                if *looked_up {
                    state.last_call = Some(Call::BlockHash { height });
                    out.effects.push(Effect::Packet {
                        to: Anchor::Cell(height),
                        call: Call::BlockHash { height },
                    });
                }
                out.effects.push(Effect::Probe { height });
            }
        }
        Event::ReorgFound { fork } => state.reorg_found(&mut out, *fork),
        Event::ReorgCollected => {
            if let Some(reorg) = &mut state.reorg {
                reorg.step = ReorgStep::Collecting;
            }
            state.save(&mut out, Anchor::Reorg, |saved| &mut saved.reorg);
            out.mark(
                Tier::Chain,
                false,
                "Payments at or above the fork were queued to be re-examined.".to_owned(),
            );
        }
        Event::ReorgProcessed {
            examined,
            changed,
            voided,
        } => state.reorg_processed(&mut out, *examined, *changed, *voided),
        Event::ReorgRewound { fork } => state.rewound(&mut out, *fork),
        Event::Seeded { height } => {
            state.chain.high_water = Some(*height);
            state.chain.tip = Some(state.chain.tip.map_or(*height, |tip| tip.max(*height)));
            state.save(&mut out, Anchor::Cell(*height), |saved| &mut saved.cursors);
            out.mark(
                Tier::Blocks,
                true,
                format!(
                    "Started scanning this network at block {}.",
                    thousands(*height)
                ),
            );
        }
        Event::Fetched {
            from,
            count,
            bytes: _,
            ahead: _,
        } => {
            state
                .chain
                .cached
                .extend(*from..from.saturating_add(*count));
            let call = Call::Blocks {
                from: *from,
                count: *count,
            };
            state.last_call = Some(call);
            out.effects.push(Effect::Packet {
                to: Anchor::Cell(from.saturating_add(count / 2)),
                call,
            });
        }
        Event::BlockScanStarted {
            height,
            group,
            stores,
            txs,
            header_only,
        } => {
            state.chain.scanning = Some(Scanning {
                height: *height,
                group: *group,
                stores: *stores,
                done_txs: 0,
                total_txs: *txs,
                header_only: *header_only,
            });
        }
        Event::BlockProgress {
            height,
            done_txs,
            total_txs,
        } => match &mut state.chain.scanning {
            Some(scanning) if scanning.height == *height => {
                scanning.done_txs = *done_txs;
                scanning.total_txs = *total_txs;
            }
            Some(_) | None => {}
        },
        Event::Checkpointed {
            height,
            stores,
            done_txs,
            total_txs,
        } => {
            state.chain.scanning = None;
            state
                .chain
                .checkpoints
                .insert(*height, (*done_txs, *total_txs));
            state.save(&mut out, Anchor::Cell(*height), |saved| {
                &mut saved.checkpoints
            });
            out.mark(
                Tier::Blocks,
                true,
                format!(
                    "Out of time partway through block {}: {} of {} transactions scanned for {}, saved.",
                    thousands(*height),
                    thousands(*done_txs),
                    thousands(*total_txs),
                    stores_phrase(*stores)
                ),
            );
        }
        Event::Committed {
            height,
            group,
            stores,
            matches,
            idle_moved,
            header_only,
        } => state.committed(
            &mut out,
            Commit {
                height: *height,
                group: *group,
                stores: *stores,
                matches: *matches,
                idle_moved: *idle_moved,
                header_only: *header_only,
            },
        ),
        Event::Diverged { height } => {
            state.chain.replaced.insert(*height);
            out.effects.push(Effect::Probe { height: *height });
            out.mark(
                Tier::Blocks,
                true,
                format!(
                    "Block {} from the node doesn't extend the recorded chain: new blocks wait for the reorganisation to be reconciled.",
                    thousands(*height)
                ),
            );
        }
        Event::IdleAdvanced { from, to, stores } => {
            state.move_stores(&mut out, *from, *to, *stores);
            out.mark(
                Tier::Blocks,
                false,
                format!(
                    "{} with nothing that could have been paid moved straight from block {} to {}.",
                    capitalised(&stores_phrase(*stores)),
                    thousands(*from),
                    thousands(*to)
                ),
            );
        }
        Event::NodePool {
            txs,
            bytes,
            penalty_free,
        } => {
            state.node_pool = Some(NodePool {
                txs: *txs,
                bytes: *bytes,
                penalty_free: *penalty_free,
            });
        }
        Event::PoolScanned {
            path,
            pool,
            scanned,
        } => {
            state.pool.size = *pool;
            state.pool.watched = true;
            // The scanner read the node's pool: its count is the latest,
            // and its size moves with it, at the same size per transaction.
            if let Some(node_pool) = &mut state.node_pool {
                node_pool.bytes = node_pool.bytes.map(|bytes| {
                    bytes
                        .saturating_mul(*pool)
                        .checked_div(node_pool.txs)
                        .unwrap_or(bytes)
                });
                node_pool.txs = *pool;
            }
            match path {
                PoolPath::Fast => {
                    state.pool.fast_passes.push_back(recorded.at_ms);
                    out.effects.push(Effect::Flash { at: Anchor::Pool });
                    if *scanned > 0 {
                        out.mark(
                            Tier::Mempool,
                            false,
                            format!(
                                "The fast path scanned {} new in the pool.",
                                plural(*scanned, "transaction")
                            ),
                        );
                    }
                }
                PoolPath::Round => {
                    state.last_call = Some(Call::Pool);
                    out.effects.push(Effect::Packet {
                        to: Anchor::Pool,
                        call: Call::Pool,
                    });
                }
            }
        }
        Event::TxMatched { path, txid } => {
            match state.pool.txs.iter_mut().find(|tx| tx.txid == *txid) {
                Some(tx) => tx.matched = true,
                None => state.push_pool_tx(txid.clone(), true),
            }
            state.pool.found += 1;
            out.effects.push(Effect::Fly {
                from: Anchor::Pool,
                to: Anchor::Orders,
                token: Token::Payment,
            });
            state.save(&mut out, Anchor::Orders, |saved| &mut saved.payments);
            out.mark(
                Tier::Mempool,
                true,
                match path {
                    PoolPath::Fast => format!("Transaction {txid} in the pool pays an order: recorded at once by the fast path."),
                    PoolPath::Round => format!("Transaction {txid} in the pool pays an order: found by the round's rotation."),
                },
            );
        }
        Event::Recomputed {
            orders,
            transitions,
        } => state.recomputed(&mut out, *orders, transitions),
        Event::Vanished { looked } => state.orders.looked = *looked,
        Event::Upkeep { pruned } => {
            state.upkeep = Upkeep {
                round: state.round.as_ref().map(|round| round.number),
                pruned: *pruned,
            };
            out.effects.push(Effect::Flash { at: Anchor::Upkeep });
            if *pruned > 0 {
                out.mark(
                    Tier::Upkeep,
                    false,
                    format!("Pruned {} old block hashes.", thousands(*pruned)),
                );
            }
        }
    }
    state.forget_before(recorded.at_ms - MINUTE_MS);
    Output {
        effects: out.effects,
        mark: out.mark.map(|(tier, key, text)| Mark {
            seq: recorded.seq,
            at_ms: recorded.at_ms,
            round: state.round.as_ref().map_or(0, |round| round.number),
            tier,
            key,
            text,
        }),
    }
}

/// The output being built by one step.
struct Step {
    effects: Vec<Effect>,
    mark: Option<(Option<Tier>, bool, String)>,
}

impl Step {
    fn mark(&mut self, tier: Tier, key: bool, text: String) {
        self.mark = Some((Some(tier), key, text));
    }
}

/// A commit, as the event says it.
#[derive(Clone, Copy)]
struct Commit {
    height: u64,
    group: Group,
    stores: u64,
    matches: u64,
    idle_moved: u64,
    header_only: bool,
}

impl State {
    /// The snapshot's facts replace the events' account of them. Groups at
    /// a cursor the page already knows keep their identity.
    fn apply_snapshot(&mut self, snapshot: &Snapshot) {
        self.chain.tip = snapshot.tip.or(self.chain.tip);
        self.chain.high_water = snapshot.high_water;
        self.chain.cached = snapshot.cached.iter().copied().collect();
        self.chain.cache_bytes = snapshot.cache_bytes;
        self.chain.cache_budget_bytes = snapshot.cache_budget_bytes;
        let known = std::mem::take(&mut self.chain.checkpoints);
        self.chain.checkpoints = snapshot
            .checkpoints
            .iter()
            .map(|height| (*height, known.get(height).copied().unwrap_or((0, 0))))
            .collect();
        let previous = std::mem::take(&mut self.groups);
        self.groups = snapshot
            .groups
            .iter()
            .map(|group| StoreGroup {
                id: previous
                    .iter()
                    .find(|known| known.cursor == group.cursor)
                    .map_or_else(|| self.new_group_id(), |known| known.id),
                cursor: group.cursor,
                stores: group.stores,
            })
            .collect();
        self.more_groups = snapshot.more_groups;
        self.reorg = snapshot.reorg.map(|job| {
            let known = self.reorg.filter(|known| known.fork == job.fork);
            Reorg {
                fork: job.fork,
                step: match job.phase {
                    shared::activity::ReorgPhase::Collect => ReorgStep::Collecting,
                    shared::activity::ReorgPhase::Process => ReorgStep::Processing,
                },
                candidates: Some(job.candidates),
                examined: known.map_or(0, |known| known.examined),
                changed: known.map_or(0, |known| known.changed),
                voided: known.map_or(0, |known| known.voided),
            }
        });
        if self.reorg.is_none() {
            self.chain.replaced.clear();
        }
        let matched: BTreeSet<String> = self
            .pool
            .txs
            .iter()
            .filter(|tx| tx.matched)
            .map(|tx| tx.txid.clone())
            .collect();
        self.pool.watched = snapshot.pool.watched;
        self.pool.size = snapshot.pool.size;
        self.pool.txs = snapshot
            .pool
            .txids
            .iter()
            .map(|txid| PoolTx {
                matched: matched.contains(txid),
                txid: txid.clone(),
            })
            .collect();
        self.orders.pending = snapshot.recomputes_pending;
        self.orders.due = snapshot.orders_due;
        self.webhooks = Webhooks {
            due: snapshot.webhooks.due,
            sent: snapshot.webhooks.sent.clone(),
        };
        self.database = snapshot.database;
        self.nodes.clone_from(&snapshot.nodes);
    }

    fn round_started(&mut self, out: &mut Step, number: u64, budget_ms: u64, tip: Option<u64>) {
        if let (Some(known), Some(tip)) = (self.chain.tip, tip) {
            if tip > known {
                out.effects.push(Effect::NewBlocks {
                    from: known + 1,
                    to: tip,
                });
                out.effects.push(Effect::Flash { at: Anchor::Node });
                out.mark(
                    Tier::Chain,
                    false,
                    if tip == known + 1 {
                        format!("The node has a new block: {}.", thousands(tip))
                    } else {
                        format!(
                            "The node has {} new blocks, up to {}.",
                            thousands(tip - known),
                            thousands(tip)
                        )
                    },
                );
            }
        }
        self.chain.tip = tip.or(self.chain.tip);
        let to_budget = self.reorg.is_some() || self.catching_up() > 0;
        self.round = Some(Round {
            number,
            budget_ms,
            tip,
            woken_by: self.pending_wake.take(),
            to_budget,
            units: Vec::new(),
            ended: [None; 5],
            elapsed_ms: 0,
            finished: None,
        });
    }

    fn unit(&mut self, out: &mut Step, unit: UnitBar) {
        let Some(round) = &mut self.round else {
            return;
        };
        round.elapsed_ms = round.elapsed_ms.max(unit.start_ms.saturating_add(unit.ms));
        round.units.push(unit);
        match unit.progress {
            UnitProgress::Failed => out.mark(
                unit.tier,
                true,
                format!(
                    "The {} tier failed and stopped for this round; it is retried next round.",
                    unit.tier
                ),
            ),
            UnitProgress::Blocked(Wait::NodeFailed) => out.mark(
                unit.tier,
                true,
                format!(
                    "The {} tier stopped for this round: the node failed.",
                    unit.tier
                ),
            ),
            UnitProgress::Blocked(_) | UnitProgress::Advanced | UnitProgress::Idle => {}
        }
    }

    fn round_finished(&mut self, number: u64, ms: u64, backlogged: bool) {
        self.chain.scanning = None;
        let Some(round) = self.round.as_mut().filter(|round| round.number == number) else {
            return;
        };
        round.elapsed_ms = round.elapsed_ms.max(ms);
        round.finished = Some(Finished { ms, backlogged });
        for outcome in &mut round.ended {
            outcome.get_or_insert(TierOutcome::Backlogged);
        }
        let mut tiers_ms = [0; 5];
        for unit in &round.units {
            tiers_ms[unit.tier.index()] += unit.ms;
        }
        let entry = RibbonEntry::Round {
            number,
            ms,
            tiers_ms,
            backlogged,
        };
        self.push_ribbon(entry);
    }

    fn reorg_found(&mut self, out: &mut Step, fork: u64) {
        let deeper = self.reorg.is_some_and(|reorg| fork < reorg.fork);
        let reorg = self.reorg.get_or_insert(Reorg {
            fork,
            step: ReorgStep::Found,
            candidates: None,
            examined: 0,
            changed: 0,
            voided: 0,
        });
        reorg.fork = reorg.fork.min(fork);
        if let Some(high_water) = self.chain.high_water {
            self.chain.replaced.extend(fork..=high_water);
        }
        out.effects.push(Effect::Probe { height: fork });
        self.save(out, Anchor::Reorg, |saved| &mut saved.reorg);
        out.mark(
            Tier::Chain,
            true,
            if deeper {
                format!(
                    "The reorganisation goes deeper: it is reconciled from block {} now.",
                    thousands(fork)
                )
            } else {
                format!(
                    "The node's chain differs from block {} on: a reorganisation. Payments from there are re-examined, and new blocks wait.",
                    thousands(fork)
                )
            },
        );
    }

    fn reorg_processed(&mut self, out: &mut Step, examined: u64, changed: u64, voided: u64) {
        if let Some(reorg) = &mut self.reorg {
            reorg.step = ReorgStep::Processing;
            reorg.examined += examined;
            reorg.changed += changed;
            reorg.voided += voided;
            reorg.candidates = reorg.candidates.map(|left| left.saturating_sub(examined));
        }
        self.last_call = Some(Call::Transactions);
        out.effects.push(Effect::Packet {
            to: Anchor::Reorg,
            call: Call::Transactions,
        });
        self.save(out, Anchor::Reorg, |saved| &mut saved.reorg);
        out.mark(
            Tier::Chain,
            voided > 0,
            format!(
                "Re-examined {} against the node's chain: {} changed, {} voided as double-spent.",
                plural(examined, "payment"),
                thousands(changed),
                thousands(voided)
            ),
        );
    }

    fn rewound(&mut self, out: &mut Step, fork: u64) {
        let ancestor = fork.saturating_sub(1);
        let deleted_to = self.chain.high_water.filter(|high| *high >= fork);
        if let Some(to) = deleted_to {
            out.effects.push(Effect::Drop { from: fork, to });
        }
        self.chain.high_water = self.chain.high_water.map(|high| high.min(ancestor));
        self.chain.cached.retain(|height| *height < fork);
        self.chain.checkpoints.retain(|height, _| *height < fork);
        self.chain.replaced.clear();
        self.chain.scanning = None;
        let above: Vec<StoreGroup> = self
            .groups
            .iter()
            .filter(|group| group.cursor > ancestor)
            .copied()
            .collect();
        for group in above {
            self.groups.retain(|kept| kept.id != group.id);
            self.add_stores(ancestor, group.stores, Some(group.id));
        }
        self.reorg = None;
        self.save(out, Anchor::Reorg, |saved| &mut saved.reorg);
        out.mark(
            Tier::Chain,
            true,
            match deleted_to {
                Some(to) if to > fork => format!(
                    "Rewound: blocks {} to {} deleted, stores moved back to block {}. The replacement blocks are scanned next.",
                    thousands(fork),
                    thousands(to),
                    thousands(ancestor)
                ),
                Some(_) | None => format!(
                    "Rewound: block {} deleted, stores moved back to block {}. The replacement block is scanned next.",
                    thousands(fork),
                    thousands(ancestor)
                ),
            },
        );
    }

    fn committed(&mut self, out: &mut Step, commit: Commit) {
        let Commit {
            height,
            group,
            stores,
            matches,
            idle_moved,
            header_only,
        } = commit;
        let parent = height.saturating_sub(1);
        self.chain.scanning = None;
        self.chain.checkpoints.remove(&height);
        self.chain.replaced.remove(&height);
        let frontier = group == Group::Frontier;
        if frontier {
            self.chain.cached.remove(&height);
            self.chain.high_water = Some(
                self.chain
                    .high_water
                    .map_or(height, |high| high.max(height)),
            );
        }
        let high_water = self.chain.high_water.unwrap_or(height);
        let was_frontier = self.groups.iter().any(|g| g.cursor == high_water);
        // The scanned stores move to the block; the idle ones with them on
        // the frontier, straight to the high-water mark when catching up.
        // The smaller move goes first, so the larger one is what empties
        // the group, and it keeps its identity where most of it went.
        let idle_to = if frontier { height } else { high_water };
        let mut moves = if idle_to == height {
            vec![(height, stores.saturating_add(idle_moved))]
        } else {
            vec![(height, stores), (idle_to, idle_moved)]
        };
        moves.sort_by_key(|(_, moving)| *moving);
        for (to, moving) in moves {
            self.move_stores(out, parent, to, moving);
        }
        // As the engine's cache does: a block at or below every group's
        // cursor serves nobody any more.
        if let Some(lowest) = self.groups.iter().map(|g| g.cursor).min() {
            self.chain.cached.retain(|cached| *cached > lowest);
        }
        out.effects.push(Effect::Flash {
            at: Anchor::Cell(height),
        });
        self.save(out, Anchor::Cell(height), |saved| &mut saved.cursors);
        for _ in 0..usize::try_from(matches)
            .unwrap_or(usize::MAX)
            .min(MAX_TOKENS)
        {
            out.effects.push(Effect::Fly {
                from: Anchor::Cell(height),
                to: Anchor::Orders,
                token: Token::Payment,
            });
        }
        let found = if matches > 0 {
            format!(", {} found in it", plural(matches, "payment"))
        } else {
            String::new()
        };
        let joined = !frontier && height == high_water && was_frontier;
        let text = if header_only {
            format!(
                "Block {} recorded from its header: no store had anything to look for in it.",
                thousands(height)
            )
        } else if frontier {
            format!(
                "Block {} scanned for {} and committed{found}.",
                thousands(height),
                stores_phrase(stores)
            )
        } else if joined {
            format!(
                "Block {} scanned for {} catching up{found}: they caught up and joined the frontier.",
                thousands(height),
                stores_phrase(stores)
            )
        } else {
            format!(
                "Block {} scanned for {} catching up{found}.",
                thousands(height),
                stores_phrase(stores)
            )
        };
        out.mark(Tier::Blocks, matches > 0 || joined, text);
    }

    fn recomputed(&mut self, out: &mut Step, orders: u64, transitions: &[Transition]) {
        self.orders.pending = self.orders.pending.saturating_sub(orders);
        for transition in transitions.iter().rev() {
            self.orders.last.push_front(*transition);
        }
        self.orders.last.truncate(LAST_TRANSITIONS);
        let queued = u64::try_from(transitions.len()).unwrap_or(u64::MAX);
        self.webhooks.due = self.webhooks.due.saturating_add(queued);
        self.save(out, Anchor::Orders, |saved| &mut saved.recomputes);
        for _ in transitions.iter().take(MAX_TOKENS) {
            out.effects.push(Effect::Fly {
                from: Anchor::Orders,
                to: Anchor::Webhooks,
                token: Token::Envelope,
            });
        }
        let settled = transitions
            .iter()
            .any(|t| matches!(t.to, OrderStatus::Paid | OrderStatus::Overpaid));
        match transitions {
            [] => {}
            [only] => out.mark(
                Tier::Settlement,
                settled,
                format!(
                    "An order went from {} to {}; its webhook is queued.",
                    only.from.as_str(),
                    only.to.as_str()
                ),
            ),
            [first, ..] => out.mark(
                Tier::Settlement,
                settled,
                format!(
                    "{} orders changed status (one from {} to {}); their webhooks are queued.",
                    thousands(queued),
                    first.from.as_str(),
                    first.to.as_str()
                ),
            ),
        }
    }

    /// Moves `stores` from the group at `from` to the group at `to`: as
    /// many as the group at `from` has, if the page knows it; else the
    /// group at `to` simply gains them.
    fn move_stores(&mut self, out: &mut Step, from: u64, to: u64, stores: u64) {
        if stores == 0 || from == to {
            return;
        }
        let source = self.take_stores(from, stores);
        let target = self.add_stores(
            to,
            stores,
            source.filter(|(_, emptied)| *emptied).map(|(id, _)| id),
        );
        if let Some((id, _)) = source {
            if id != target {
                out.effects.push(Effect::Fly {
                    from: Anchor::Group(id),
                    to: Anchor::Group(target),
                    token: Token::Stores,
                });
            }
        }
    }

    /// Takes up to `stores` from the group at `cursor`: its id, and whether
    /// that emptied it (it is then gone).
    fn take_stores(&mut self, cursor: u64, stores: u64) -> Option<(u64, bool)> {
        let index = self.groups.iter().position(|g| g.cursor == cursor)?;
        let group = &mut self.groups[index];
        group.stores = group.stores.saturating_sub(stores);
        let id = group.id;
        let emptied = group.stores == 0;
        if emptied {
            self.groups.remove(index);
        }
        Some((id, emptied))
    }

    /// Adds `stores` to the group at `cursor`, making it (with `id`, if
    /// given and free) if there is none. Returns its id.
    fn add_stores(&mut self, cursor: u64, stores: u64, id: Option<u64>) -> u64 {
        if let Some(group) = self.groups.iter_mut().find(|g| g.cursor == cursor) {
            group.stores = group.stores.saturating_add(stores);
            return group.id;
        }
        let id = id
            .filter(|id| !self.groups.iter().any(|g| g.id == *id))
            .unwrap_or_else(|| self.new_group_id());
        let at = self
            .groups
            .iter()
            .position(|g| g.cursor < cursor)
            .unwrap_or(self.groups.len());
        self.groups.insert(at, StoreGroup { id, cursor, stores });
        id
    }

    fn new_group_id(&mut self) -> u64 {
        self.next_group_id += 1;
        self.next_group_id
    }

    /// How many stores are behind the high-water mark.
    pub fn catching_up(&self) -> u64 {
        let Some(high_water) = self.chain.high_water else {
            return 0;
        };
        self.groups
            .iter()
            .filter(|group| group.cursor < high_water)
            .map(|group| group.stores)
            .sum()
    }

    fn push_pool_tx(&mut self, txid: String, matched: bool) {
        self.pool.txs.push_back(PoolTx { txid, matched });
        while self.pool.txs.len() > POOL_TXS {
            self.pool.txs.pop_front();
        }
    }

    fn push_ribbon(&mut self, entry: RibbonEntry) {
        self.ribbon.push_back(entry);
        while self.ribbon.len() > RIBBON {
            self.ribbon.pop_front();
        }
    }

    /// Records a durable write at `at`, counted under `kind`.
    fn save(&mut self, out: &mut Step, at: Anchor, kind: impl FnOnce(&mut Saved) -> &mut u64) {
        *kind(&mut self.saved) += 1;
        self.saves.push_back(self.at_ms);
        out.effects.push(Effect::Save { at });
    }

    /// Lets go of the per-minute figures from before `since`.
    fn forget_before(&mut self, since: i64) {
        while self.saves.front().is_some_and(|at| *at < since) {
            self.saves.pop_front();
        }
        while self.pool.fast_passes.front().is_some_and(|at| *at < since) {
            self.pool.fast_passes.pop_front();
        }
    }
}

/// "1 store", "3 stores".
pub fn stores_phrase(stores: u64) -> String {
    plural(stores, "store")
}

/// `n` and `noun`, plural unless one: "1 payment", "1,204 payments".
pub fn plural(n: u64, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{} {noun}s", thousands(n))
    }
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
